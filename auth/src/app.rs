//! Shared state, sessions, token issuance and small HTTP helpers.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chaindeal_authn::{Claims, Issuer, JwkSet, API_AUDIENCE};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::crypto::{self, DataKeys};
use crate::db::Db;

pub const ACCESS_TTL: u64 = 300;
pub const SESSION_TTL: u64 = 12 * 3600;
pub const REFRESH_TTL: u64 = 12 * 3600;
pub const CODE_TTL: u64 = 60;
pub const AUTHREQ_TTL: u64 = 600;
pub const CHALLENGE_TTL: u64 = 300;
pub const WEB_CLIENT: &str = "chaindeal-web";
pub const SEED_CLIENT: &str = "chaindeal-seed";
/// The market simulator service (streams transactions to the chain over gRPC).
pub const SIM_CLIENT: &str = "chaindeal-sim";
pub const SESSION_COOKIE: &str = "__Host-cd_session";
pub const REFRESH_COOKIE: &str = "__Secure-cd_rt";
pub const ROLES: [&str; 3] = ["user", "operator", "admin"];

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

pub struct App {
    pub db: Db,
    pub keys: DataKeys,
    pub signer: Issuer,
    pub jwks: JwkSet,
    /// Public issuer identifier, e.g. https://chaindeal.localhost
    pub iss: String,
    pub redirect_uris: Vec<String>,
    /// Origins allowed to call token/logout from a browser (CSRF defence).
    pub origins: Vec<String>,
    pub seed_secret_hash: Option<String>,
    pub sim_secret_hash: Option<String>,
    pub limiter: Limiter,
    /// Bounds concurrent Argon2 work (each hash uses ~19 MiB).
    pub hashing: Semaphore,
    pub dummy_hash: String,
}

// ---- errors -----------------------------------------------------------------

pub struct ApiError(pub StatusCode, pub &'static str, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1, "error_description": self.2 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("{e:#}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "server_error", "internal error".into())
    }
}

pub fn bad(code: &'static str, msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, code, msg.into())
}

pub fn unauthorized(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::UNAUTHORIZED, "invalid_token", msg.into())
}

pub fn forbidden(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, "insufficient_scope", msg.into())
}

// ---- rate limiting ------------------------------------------------------------

/// Token buckets keyed by client IP / account; in-memory per pod.
#[derive(Default)]
pub struct Limiter(Mutex<HashMap<String, (f64, Instant)>>);

impl Limiter {
    /// Allows `burst` requests, refilling `per_min` per minute.
    pub fn allow(&self, key: &str, burst: f64, per_min: f64) -> bool {
        let mut m = self.0.lock().unwrap();
        if m.len() > 50_000 {
            m.retain(|_, (_, t)| t.elapsed().as_secs() < 600);
        }
        let now = Instant::now();
        let e = m.entry(key.to_string()).or_insert((burst, now));
        e.0 = (e.0 + e.1.elapsed().as_secs_f64() * per_min / 60.0).min(burst);
        e.1 = now;
        if e.0 >= 1.0 {
            e.0 -= 1.0;
            true
        } else {
            false
        }
    }
}

pub fn client_ip(h: &HeaderMap) -> String {
    h.get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

pub fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub fn session_cookie(sid: &str) -> String {
    format!("{SESSION_COOKIE}={sid}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={SESSION_TTL}")
}

pub fn refresh_cookie(rt: &str) -> String {
    format!("{REFRESH_COOKIE}={rt}; Path=/oauth; HttpOnly; Secure; SameSite=Strict; Max-Age={REFRESH_TTL}")
}

pub fn clear_cookies() -> [String; 2] {
    [
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0"),
        format!("{REFRESH_COOKIE}=; Path=/oauth; HttpOnly; Secure; SameSite=Strict; Max-Age=0"),
    ]
}

impl App {
    /// Browser-originated calls to token/logout must come from our own origin.
    pub fn origin_ok(&self, h: &HeaderMap) -> bool {
        match h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            Some(o) => self.origins.iter().any(|a| a == o),
            None => true, // non-browser clients (e.g. client_credentials) send no Origin
        }
    }

    pub async fn audit(&self, event: &str, user_id: Option<&str>, ip: &str, detail: Value) {
        let doc = json!({ "event": event, "user_id": user_id, "ip": ip, "detail": detail });
        if let Err(e) = self.db.audit(doc).await {
            tracing::warn!("audit write failed: {e:#}");
        }
    }

    pub async fn verify_password(&self, pw: String, phc: String) -> bool {
        let _permit = self.hashing.acquire().await;
        tokio::task::spawn_blocking(move || crypto::verify_password(&pw, &phc)).await.unwrap_or(false)
    }

    pub async fn hash_password(&self, pw: String) -> Result<String> {
        let _permit = self.hashing.acquire().await;
        tokio::task::spawn_blocking(move || crypto::hash_password(&pw)).await?
    }

    // ---- users ------------------------------------------------------------------

    pub async fn user(&self, id: &str) -> Result<Option<Value>> {
        self.db.get("users", id).await
    }

    pub fn email_of(&self, user: &Value) -> String {
        let id = user["id"].as_str().unwrap_or_default();
        self.keys.open(user["email_enc"].as_str().unwrap_or_default(), id).unwrap_or_default()
    }

    pub async fn wallets_of(&self, user_id: &str) -> Result<Vec<Value>> {
        self.db.by_idx("wallets", user_id, 100).await
    }

    /// Public view of a user (decrypted email, never the password hash).
    pub async fn user_view(&self, u: &Value) -> Result<Value> {
        let id = u["id"].as_str().unwrap_or_default();
        let wallets = self.wallets_of(id).await?;
        Ok(json!({
            "id": id,
            "email": self.email_of(u),
            "name": u["name"],
            "roles": u["roles"],
            "status": u["status"],
            "created_at": u["created_at"],
            "failed_logins": u["failed_logins"],
            "locked_until": u["locked_until"],
            "wallets": wallets,
        }))
    }

    pub fn roles_of(u: &Value) -> Vec<String> {
        u["roles"].as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect()).unwrap_or_default()
    }

    // ---- sessions ---------------------------------------------------------------

    /// Creates a login session; returns the cookie secret and its public id.
    pub async fn create_session(&self, user_id: &str, ip: &str, ua: &str) -> Result<(String, String)> {
        let sid = crypto::random_token();
        let key = crypto::hash_secret(&sid);
        let id = key[..16].to_string();
        let t = now();
        let doc = json!({ "id": id, "key": key, "user_id": user_id, "created_at": t, "last_used": t,
                          "ip": ip, "ua": ua.chars().take(160).collect::<String>() });
        self.db.put("sessions", &key, user_id, (t + SESSION_TTL) as f64, doc).await?;
        Ok((sid, id))
    }

    /// Resolves the session cookie to (session doc, user doc) if both are valid.
    pub async fn session_from(&self, h: &HeaderMap) -> Result<Option<(Value, Value)>> {
        let Some(sid) = cookie(h, SESSION_COOKIE) else { return Ok(None) };
        let Some(s) = self.db.get("sessions", &crypto::hash_secret(&sid)).await? else { return Ok(None) };
        let Some(u) = self.user(s["user_id"].as_str().unwrap_or_default()).await? else { return Ok(None) };
        if u["status"] != "active" {
            return Ok(None);
        }
        Ok(Some((s, u)))
    }

    pub async fn session_alive(&self, user_id: &str, session_id: &str) -> Result<bool> {
        Ok(self.db.by_idx("sessions", user_id, 200).await?.iter().any(|s| s["id"] == session_id))
    }

    /// Revokes one session and every refresh token issued under it.
    pub async fn revoke_session(&self, user_id: &str, session_id: &str) -> Result<bool> {
        let sessions = self.db.by_idx("sessions", user_id, 200).await?;
        let Some(s) = sessions.iter().find(|s| s["id"] == session_id) else { return Ok(false) };
        self.db.delete("sessions", s["key"].as_str().unwrap_or_default()).await?;
        self.db.delete_by_idx("refresh", session_id).await?;
        Ok(true)
    }

    pub async fn revoke_all_sessions(&self, user_id: &str, except: Option<&str>) -> Result<u32> {
        let mut n = 0;
        for s in self.db.by_idx("sessions", user_id, 500).await? {
            let id = s["id"].as_str().unwrap_or_default();
            if Some(id) != except && self.revoke_session(user_id, id).await? {
                n += 1;
            }
        }
        Ok(n)
    }

    // ---- tokens -------------------------------------------------------------------

    /// Scopes a user may hold, intersected with what the client asked for.
    pub fn grant_scopes(requested: &str, roles: &[String]) -> String {
        let mut allowed = vec!["openid", "profile", "deals:write"];
        let has = |r: &str| roles.iter().any(|x| x == r);
        if has("operator") || has("admin") {
            allowed.push("sim:control");
        }
        if has("admin") {
            allowed.push("users:admin");
        }
        let req: Vec<&str> = if requested.trim().is_empty() { vec!["openid", "profile", "deals:write"] } else { requested.split_whitespace().collect() };
        allowed.into_iter().filter(|s| req.contains(s)).collect::<Vec<_>>().join(" ")
    }

    pub async fn access_token(&self, u: &Value, client_id: &str, scope: &str, sid: &str) -> Result<String> {
        let id = u["id"].as_str().unwrap_or_default();
        let wallets = self.wallets_of(id).await?.iter().filter_map(|w| w["address"].as_str().map(str::to_string)).collect();
        let t = now();
        Ok(self.signer.sign(&Claims {
            iss: self.iss.clone(),
            sub: id.to_string(),
            aud: API_AUDIENCE.into(),
            exp: t + ACCESS_TTL,
            nbf: t,
            iat: t,
            jti: crypto::random_id(),
            scope: scope.to_string(),
            client_id: client_id.to_string(),
            roles: Self::roles_of(u),
            wallets,
            sid: sid.to_string(),
            name: u["name"].as_str().unwrap_or_default().to_string(),
        }))
    }

    pub fn id_token(&self, u: &Value, client_id: &str, nonce: Option<&str>, auth_time: u64) -> String {
        let t = now();
        self.signer.sign_value(
            &json!({
                "iss": self.iss, "sub": u["id"], "aud": client_id, "exp": t + ACCESS_TTL, "iat": t,
                "auth_time": auth_time, "nonce": nonce, "name": u["name"],
            }),
            "JWT",
        )
    }

    /// Issues a refresh token in the session's family and returns it.
    pub async fn issue_refresh(&self, user_id: &str, client_id: &str, scope: &str, sid: &str) -> Result<String> {
        let rt = crypto::random_token();
        let doc = json!({ "user_id": user_id, "client_id": client_id, "scope": scope, "sid": sid, "used": false, "issued_at": now() });
        self.db.put("refresh", &crypto::hash_secret(&rt), sid, (now() + REFRESH_TTL) as f64, doc).await?;
        Ok(rt)
    }

    /// Validates a bearer access token issued by this service.
    pub fn bearer(&self, h: &HeaderMap) -> Result<Claims, ApiError> {
        let token = h
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| unauthorized("missing bearer token"))?;
        chaindeal_authn::verify(token, &self.jwks, &self.iss, API_AUDIENCE, now()).map_err(|e| unauthorized(e.to_string()))
    }
}

/// NIST 800-63B style: length over complexity, plus a blocklist.
pub fn password_problem(pw: &str, email: &str) -> Option<&'static str> {
    const BLOCK: [&str; 8] = ["password", "123456", "qwerty", "chaindeal", "letmein", "welcome", "iloveyou", "admin"];
    let lower = pw.to_lowercase();
    let local = email.split('@').next().unwrap_or_default();
    if pw.chars().count() < 12 {
        Some("Password must be at least 12 characters.")
    } else if pw.len() > 256 {
        Some("Password is too long.")
    } else if BLOCK.iter().any(|b| lower.contains(b)) || (local.len() >= 3 && lower.contains(local)) {
        Some("Password is too easy to guess — avoid common words and your email.")
    } else if pw.chars().collect::<std::collections::HashSet<_>>().len() < 6 {
        Some("Password needs more variety.")
    } else {
        None
    }
}

pub fn valid_email(e: &str) -> bool {
    let e = e.trim();
    let Some((local, domain)) = e.split_once('@') else { return false };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && e.len() <= 254 && !e.contains(char::is_whitespace)
}
