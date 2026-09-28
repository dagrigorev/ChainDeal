//! OAuth 2.1 / OpenID Connect endpoints.
//!
//! Browser flow (public client `chaindeal-web`):
//!   SPA ──authorize(PKCE S256, state, nonce)──▶ hosted login ──▶ 303 redirect_uri?code&state&iss
//!   SPA ──token(code, code_verifier)──▶ { access_token (5 min, memory only), id_token }
//!                                        + refresh token in an HttpOnly cookie (never readable by JS)
//!   SPA ──token(grant_type=refresh_token)──▶ rotated refresh + new access token; reuse ⇒ family revoked
//! Service flow (confidential `chaindeal-seed`): client_credentials.

use std::sync::Arc;

use axum::extract::{Form, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::*;
use crate::crypto;
use crate::pages;

type S = State<Arc<App>>;

pub async fn discovery(State(a): S) -> Json<Value> {
    let i = &a.iss;
    Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/oauth/authorize"),
        "token_endpoint": format!("{i}/oauth/token"),
        "userinfo_endpoint": format!("{i}/oauth/userinfo"),
        "jwks_uri": format!("{i}/oauth/jwks"),
        "revocation_endpoint": format!("{i}/oauth/logout"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token", "client_credentials"],
        "code_challenge_methods_supported": ["S256"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["EdDSA"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_basic", "client_secret_post"],
        "scopes_supported": ["openid", "profile", "deals:write", "sim:control", "users:admin"],
        "authorization_response_iss_parameter_supported": true,
    }))
}

pub async fn jwks(State(a): S) -> Response {
    let mut r = Json(json!(a.jwks)).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    r
}

#[derive(Deserialize)]
pub struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    scope: Option<String>,
    state: Option<String>,
    nonce: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    prompt: Option<String>,
}

fn redirect_with(uri: &str, params: &[(&str, &str)]) -> String {
    let q: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect();
    format!("{uri}{}{}", if uri.contains('?') { '&' } else { '?' }, q.join("&"))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn html(status: StatusCode, body: String) -> Response {
    (status, Html(body)).into_response()
}

/// GET /oauth/authorize — validates the request, then either issues a code
/// straight away (existing session) or shows the sign-in page.
pub async fn authorize(State(a): S, h: HeaderMap, Query(q): Query<AuthorizeQuery>) -> Result<Response, ApiError> {
    // Never redirect anywhere until client and redirect_uri are verified.
    if q.client_id.as_deref() != Some(WEB_CLIENT) {
        return Ok(html(StatusCode::BAD_REQUEST, pages::error("Unknown application", "This sign-in link is not valid.")));
    }
    let Some(redirect_uri) = q.redirect_uri.filter(|r| a.redirect_uris.contains(r)) else {
        return Ok(html(StatusCode::BAD_REQUEST, pages::error("Invalid redirect", "The return address is not registered for this application.")));
    };
    let state = q.state.unwrap_or_default();
    let fail = |err: &str, desc: &str| -> Response {
        Redirect::to(&redirect_with(&redirect_uri, &[("error", err), ("error_description", desc), ("state", &state), ("iss", &a.iss)])).into_response()
    };
    if q.response_type.as_deref() != Some("code") {
        return Ok(fail("unsupported_response_type", "only the authorization code flow is supported"));
    }
    let challenge = q.code_challenge.unwrap_or_default();
    if q.code_challenge_method.as_deref() != Some("S256") || challenge.len() != 43 {
        return Ok(fail("invalid_request", "PKCE with S256 is required"));
    }
    if state.is_empty() || state.len() > 512 {
        return Ok(fail("invalid_request", "state is required"));
    }

    let req = json!({
        "client_id": WEB_CLIENT, "redirect_uri": redirect_uri, "scope": q.scope.unwrap_or_default(),
        "state": state, "nonce": q.nonce, "code_challenge": challenge, "csrf": crypto::random_token(),
    });

    // Single sign-on: an existing session skips the form (unless prompt=login).
    if q.prompt.as_deref() != Some("login") {
        if let Some((s, u)) = a.session_from(&h).await? {
            let url = issue_code(&a, &req, &u, s["id"].as_str().unwrap_or_default(), s["created_at"].as_u64().unwrap_or(0)).await?;
            return Ok(Redirect::to(&url).into_response());
        }
    }
    let req_id = crypto::random_token();
    a.db.put("challenges", &format!("authreq:{req_id}"), "authreq", (now() + AUTHREQ_TTL) as f64, req.clone()).await?;
    Ok(html(StatusCode::OK, pages::login(&req_id, req["csrf"].as_str().unwrap(), "ChainDeal", "", None, None)))
}

#[derive(Deserialize)]
pub struct ReqQuery {
    req: String,
}

async fn pending(a: &App, req_id: &str) -> Result<Option<Value>, ApiError> {
    Ok(a.db.get("challenges", &format!("authreq:{req_id}")).await?)
}

fn expired_page() -> Response {
    html(StatusCode::BAD_REQUEST, pages::error("Sign-in expired", "This sign-in attempt timed out. Go back to ChainDeal and try again."))
}

pub async fn resume(State(a): S, Query(q): Query<ReqQuery>) -> Result<Response, ApiError> {
    let Some(r) = pending(&a, &q.req).await? else { return Ok(expired_page()) };
    Ok(html(StatusCode::OK, pages::login(&q.req, r["csrf"].as_str().unwrap_or_default(), "ChainDeal", "", None, None)))
}

pub async fn register_page(State(a): S, Query(q): Query<ReqQuery>) -> Result<Response, ApiError> {
    let Some(r) = pending(&a, &q.req).await? else { return Ok(expired_page()) };
    Ok(html(StatusCode::OK, pages::register(&q.req, r["csrf"].as_str().unwrap_or_default(), "", "", None)))
}

/// Creates a single-use authorization code (60 s) and the redirect back to the client.
async fn issue_code(a: &App, req: &Value, u: &Value, sid: &str, auth_time: u64) -> anyhow::Result<String> {
    let code = crypto::random_token();
    let doc = json!({
        "client_id": req["client_id"], "redirect_uri": req["redirect_uri"], "scope": req["scope"],
        "nonce": req["nonce"], "code_challenge": req["code_challenge"], "user_id": u["id"],
        "sid": sid, "auth_time": auth_time,
    });
    a.db.put("codes", &crypto::hash_secret(&code), "code", (now() + CODE_TTL) as f64, doc).await?;
    let uri = req["redirect_uri"].as_str().unwrap_or_default();
    // RFC 9207: include `iss` so the client can detect mix-up attacks.
    Ok(redirect_with(uri, &[("code", &code), ("state", req["state"].as_str().unwrap_or_default()), ("iss", &a.iss)]))
}

fn signed_in(url: &str, sid: &str) -> Response {
    let mut r = Redirect::to(url).into_response(); // 303 See Other
    r.headers_mut().append(header::SET_COOKIE, HeaderValue::from_str(&session_cookie(sid)).unwrap());
    r
}

#[derive(Deserialize)]
pub struct LoginForm {
    req: String,
    csrf: String,
    email: String,
    password: String,
}

/// POST /oauth/login — credential check with lockout and uniform errors.
pub async fn login(State(a): S, h: HeaderMap, Form(f): Form<LoginForm>) -> Result<Response, ApiError> {
    let ip = client_ip(&h);
    let Some(req) = pending(&a, &f.req).await? else { return Ok(expired_page()) };
    let csrf = req["csrf"].as_str().unwrap_or_default();
    if !crypto::ct_eq(csrf, &f.csrf) {
        return Ok(html(StatusCode::FORBIDDEN, pages::error("Request blocked", "The form could not be verified. Please start again.")));
    }
    let retry = |msg: &str, status: StatusCode| html(status, pages::login(&f.req, csrf, "ChainDeal", &f.email, Some(msg), None));
    if !a.limiter.allow(&format!("login-ip:{ip}"), 10.0, 10.0) {
        return Ok(retry("Too many attempts. Wait a minute and try again.", StatusCode::TOO_MANY_REQUESTS));
    }
    let email = crypto::normalize_email(&f.email);
    let user = a.db.user_by_email_index(&a.keys.blind_index(&email)).await?;
    let t = now();

    let Some(u) = user else {
        // Same work and same message as a wrong password: no account enumeration.
        let _ = a.verify_password(f.password, a.dummy_hash.clone()).await;
        a.audit("login_failed", None, &ip, json!({ "reason": "unknown_account" })).await;
        return Ok(retry("Invalid email or password.", StatusCode::UNAUTHORIZED));
    };
    let id = u["id"].as_str().unwrap_or_default().to_string();
    if u["locked_until"].as_u64().unwrap_or(0) > t {
        a.audit("login_blocked", Some(&id), &ip, json!({ "reason": "locked" })).await;
        return Ok(retry("Too many failed attempts. This account is temporarily locked.", StatusCode::TOO_MANY_REQUESTS));
    }
    let ok = a.verify_password(f.password, u["password"].as_str().unwrap_or_default().to_string()).await;
    if !ok || u["status"] != "active" {
        let fails = u["failed_logins"].as_u64().unwrap_or(0) + 1;
        // Exponential lockout after 5 failures: 1, 2, 4 … 60 minutes.
        let locked = if fails >= 5 { t + (60u64 << (fails - 5).min(6)).min(3600) } else { 0 };
        a.db.update_user(&id, json!({ "failed_logins": fails, "locked_until": locked })).await?;
        a.audit("login_failed", Some(&id), &ip, json!({ "reason": if ok { "disabled" } else { "bad_password" }, "failures": fails })).await;
        return Ok(retry("Invalid email or password.", StatusCode::UNAUTHORIZED));
    }
    a.db.update_user(&id, json!({ "failed_logins": 0, "locked_until": 0, "last_login": t })).await?;
    let ua = h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let (sid, session_id) = a.create_session(&id, &ip, ua).await?;
    a.db.consume("challenges", &format!("authreq:{}", f.req)).await?;
    a.audit("login", Some(&id), &ip, json!({ "session": session_id })).await;
    let url = issue_code(&a, &req, &u, &session_id, t).await?;
    Ok(signed_in(&url, &sid))
}

#[derive(Deserialize)]
pub struct RegisterForm {
    req: String,
    csrf: String,
    name: String,
    email: String,
    password: String,
    password2: String,
}

/// POST /oauth/register — creates the account, signs in, continues the flow.
pub async fn register(State(a): S, h: HeaderMap, Form(f): Form<RegisterForm>) -> Result<Response, ApiError> {
    let ip = client_ip(&h);
    let Some(req) = pending(&a, &f.req).await? else { return Ok(expired_page()) };
    let csrf = req["csrf"].as_str().unwrap_or_default();
    if !crypto::ct_eq(csrf, &f.csrf) {
        return Ok(html(StatusCode::FORBIDDEN, pages::error("Request blocked", "The form could not be verified. Please start again.")));
    }
    let again = |msg: &str| html(StatusCode::BAD_REQUEST, pages::register(&f.req, csrf, &f.email, &f.name, Some(msg)));
    if !a.limiter.allow(&format!("register-ip:{ip}"), 5.0, 5.0) {
        return Ok(again("Too many sign-ups from your network. Try again shortly."));
    }
    let email = crypto::normalize_email(&f.email);
    let name = f.name.trim();
    if !valid_email(&email) {
        return Ok(again("Enter a valid email address."));
    }
    if name.is_empty() || name.chars().count() > 64 {
        return Ok(again("Display name must be 1–64 characters."));
    }
    if f.password != f.password2 {
        return Ok(again("Passwords don't match."));
    }
    if let Some(p) = password_problem(&f.password, &email) {
        return Ok(again(p));
    }
    let id = crypto::random_id();
    let t = now();
    let doc = json!({
        "id": id, "email_enc": a.keys.seal(&email, &id)?, "name": name,
        "password": a.hash_password(f.password).await?, "roles": ["user"], "status": "active",
        "created_at": t, "failed_logins": 0, "locked_until": 0,
    });
    if !a.db.insert("users", &id, &a.keys.blind_index(&email), 0.0, doc.clone()).await? {
        a.audit("register_conflict", None, &ip, json!({})).await;
        return Ok(again("An account with this email may already exist — try signing in."));
    }
    let ua = h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let (sid, session_id) = a.create_session(&id, &ip, ua).await?;
    a.db.consume("challenges", &format!("authreq:{}", f.req)).await?;
    a.audit("register", Some(&id), &ip, json!({ "session": session_id })).await;
    let url = issue_code(&a, &req, &doc, &session_id, t).await?;
    Ok(signed_in(&url, &sid))
}

#[derive(Deserialize)]
pub struct TokenForm {
    grant_type: String,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    code_verifier: Option<String>,
    scope: Option<String>,
}

fn token_response(body: Value, refresh: Option<&str>) -> Response {
    let mut r = Json(body).into_response();
    let hd = r.headers_mut();
    hd.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    hd.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    if let Some(rt) = refresh {
        hd.append(header::SET_COOKIE, HeaderValue::from_str(&refresh_cookie(rt)).unwrap());
    }
    r
}

fn invalid_grant(msg: &str) -> ApiError {
    bad("invalid_grant", msg)
}

/// POST /oauth/token
pub async fn token(State(a): S, h: HeaderMap, Form(f): Form<TokenForm>) -> Result<Response, ApiError> {
    let ip = client_ip(&h);
    if !a.origin_ok(&h) {
        return Err(ApiError(StatusCode::FORBIDDEN, "invalid_request", "origin not allowed".into()));
    }
    if !a.limiter.allow(&format!("token-ip:{ip}"), 60.0, 120.0) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "slow_down", "too many token requests".into()));
    }
    match f.grant_type.as_str() {
        "authorization_code" => {
            if f.client_id.as_deref() != Some(WEB_CLIENT) {
                return Err(bad("invalid_client", "unknown client"));
            }
            let code = f.code.ok_or_else(|| bad("invalid_request", "code is required"))?;
            // Consumed atomically: a code works exactly once.
            let c = a.db.consume("codes", &crypto::hash_secret(&code)).await?.ok_or_else(|| invalid_grant("code is invalid, expired or already used"))?;
            if c["client_id"] != WEB_CLIENT || c["redirect_uri"].as_str() != f.redirect_uri.as_deref() {
                return Err(invalid_grant("code was not issued for this client / redirect_uri"));
            }
            let verifier = f.code_verifier.unwrap_or_default();
            if !crypto::pkce_matches(&verifier, c["code_challenge"].as_str().unwrap_or_default()) {
                a.audit("pkce_failed", c["user_id"].as_str(), &ip, json!({})).await;
                return Err(invalid_grant("PKCE verification failed"));
            }
            let uid = c["user_id"].as_str().unwrap_or_default();
            let sid = c["sid"].as_str().unwrap_or_default();
            let u = a.user(uid).await?.filter(|u| u["status"] == "active").ok_or_else(|| invalid_grant("account unavailable"))?;
            if !a.session_alive(uid, sid).await? {
                return Err(invalid_grant("session ended"));
            }
            let scope = App::grant_scopes(c["scope"].as_str().unwrap_or_default(), &App::roles_of(&u));
            let access = a.access_token(&u, WEB_CLIENT, &scope, sid).await?;
            let id_token = a.id_token(&u, WEB_CLIENT, c["nonce"].as_str(), c["auth_time"].as_u64().unwrap_or(0));
            let rt = a.issue_refresh(uid, WEB_CLIENT, &scope, sid).await?;
            Ok(token_response(
                json!({ "access_token": access, "token_type": "Bearer", "expires_in": ACCESS_TTL, "scope": scope, "id_token": id_token }),
                Some(&rt),
            ))
        }
        "refresh_token" => {
            let rt = cookie(&h, REFRESH_COOKIE).ok_or_else(|| invalid_grant("no refresh token"))?;
            let fresh = crypto::random_token();
            let r = a.db.rotate_refresh(&crypto::hash_secret(&rt), &crypto::hash_secret(&fresh), (now() + REFRESH_TTL) as f64).await?;
            match r["status"].as_str() {
                Some("ok") => {}
                Some("reuse") => {
                    // A rotated-out token came back: assume theft, end the whole session.
                    let (uid, sid) = (r["user_id"].as_str().unwrap_or_default(), r["sid"].as_str().unwrap_or_default());
                    a.revoke_session(uid, sid).await?;
                    a.audit("refresh_reuse_detected", Some(uid), &ip, json!({ "session": sid })).await;
                    return Err(invalid_grant("refresh token reuse detected; session revoked"));
                }
                _ => return Err(invalid_grant("refresh token invalid or expired")),
            }
            let d = &r["doc"];
            let (uid, sid) = (d["user_id"].as_str().unwrap_or_default(), d["sid"].as_str().unwrap_or_default());
            let u = a.user(uid).await?.filter(|u| u["status"] == "active").ok_or_else(|| invalid_grant("account unavailable"))?;
            if !a.session_alive(uid, sid).await? {
                return Err(invalid_grant("session ended"));
            }
            // Re-derive scope from current roles so demotions take effect on refresh.
            let scope = App::grant_scopes(d["scope"].as_str().unwrap_or_default(), &App::roles_of(&u));
            let access = a.access_token(&u, WEB_CLIENT, &scope, sid).await?;
            Ok(token_response(json!({ "access_token": access, "token_type": "Bearer", "expires_in": ACCESS_TTL, "scope": scope }), Some(&fresh)))
        }
        "client_credentials" => {
            let (id, secret) = basic_auth(&h).or_else(|| Some((f.client_id.clone()?, f.client_secret.clone()?))).ok_or_else(|| bad("invalid_client", "client authentication required"))?;
            let ok = id == SEED_CLIENT && a.seed_secret_hash.as_deref().is_some_and(|hsh| crypto::ct_eq(hsh, &crypto::hash_secret(&secret)));
            if !ok {
                a.audit("client_auth_failed", None, &ip, json!({ "client": id })).await;
                return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_client", "client authentication failed".into()));
            }
            let allowed = ["deals:write", "tx:any"];
            let scope: Vec<&str> = f.scope.as_deref().unwrap_or("deals:write tx:any").split_whitespace().filter(|s| allowed.contains(s)).collect();
            let t = now();
            let token = a.signer.sign(&chaindeal_authn::Claims {
                iss: a.iss.clone(), sub: format!("client:{SEED_CLIENT}"), aud: chaindeal_authn::API_AUDIENCE.into(),
                exp: t + ACCESS_TTL, nbf: t, iat: t, jti: crypto::random_id(), scope: scope.join(" "),
                client_id: SEED_CLIENT.into(), roles: vec![], wallets: vec![], sid: String::new(), name: "seed service".into(),
            });
            Ok(token_response(json!({ "access_token": token, "token_type": "Bearer", "expires_in": ACCESS_TTL, "scope": scope.join(" ") }), None))
        }
        _ => Err(bad("unsupported_grant_type", "unsupported grant_type")),
    }
}

fn basic_auth(h: &HeaderMap) -> Option<(String, String)> {
    let v = h.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Basic ")?;
    let raw = String::from_utf8(STANDARD.decode(v).ok()?).ok()?;
    let (id, secret) = raw.split_once(':')?;
    Some((id.to_string(), secret.to_string()))
}

/// POST /oauth/logout — ends the browser session and its refresh tokens.
pub async fn logout(State(a): S, h: HeaderMap) -> Result<Response, ApiError> {
    if !a.origin_ok(&h) {
        return Err(ApiError(StatusCode::FORBIDDEN, "invalid_request", "origin not allowed".into()));
    }
    if let Some((s, u)) = a.session_from(&h).await? {
        let (uid, sid) = (u["id"].as_str().unwrap_or_default(), s["id"].as_str().unwrap_or_default());
        a.revoke_session(uid, sid).await?;
        a.audit("logout", Some(uid), &client_ip(&h), json!({ "session": sid })).await;
    }
    let mut r = StatusCode::NO_CONTENT.into_response();
    for c in clear_cookies() {
        r.headers_mut().append(header::SET_COOKIE, HeaderValue::from_str(&c).unwrap());
    }
    Ok(r)
}

pub async fn userinfo(State(a): S, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    let c = a.bearer(&h)?;
    Ok(Json(json!({ "sub": c.sub, "name": c.name, "roles": c.roles })))
}
