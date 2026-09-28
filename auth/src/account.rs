//! Self-service account management and the admin API. Everything requires a
//! bearer access token from this service; admin actions additionally re-check
//! the caller's *current* roles and status in the database.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chaindeal_authn::Claims;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::*;
use crate::crypto;

type S = State<Arc<App>>;
type R = Result<Json<Value>, ApiError>;

async fn me_user(a: &App, c: &Claims) -> Result<Value, ApiError> {
    a.user(&c.sub).await?.filter(|u| u["status"] == "active").ok_or_else(|| unauthorized("account unavailable"))
}

pub async fn me(State(a): S, h: HeaderMap) -> R {
    let c = a.bearer(&h)?;
    let u = me_user(&a, &c).await?;
    let mut v = a.user_view(&u).await?;
    let sessions: Vec<Value> = a
        .db
        .by_idx("sessions", &c.sub, 100)
        .await?
        .into_iter()
        .map(|s| json!({ "id": s["id"], "created_at": s["created_at"], "ip": s["ip"], "ua": s["ua"], "current": s["id"] == c.sid.as_str() }))
        .collect();
    v["sessions"] = json!(sessions);
    v["scope"] = json!(c.scope);
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct PasswordChange {
    current: String,
    new: String,
}

pub async fn change_password(State(a): S, h: HeaderMap, Json(p): Json<PasswordChange>) -> R {
    let c = a.bearer(&h)?;
    let ip = client_ip(&h);
    if !a.limiter.allow(&format!("pw:{}", c.sub), 5.0, 5.0) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "slow_down", "too many attempts".into()));
    }
    let u = me_user(&a, &c).await?;
    if !a.verify_password(p.current, u["password"].as_str().unwrap_or_default().into()).await {
        a.audit("password_change_failed", Some(&c.sub), &ip, json!({})).await;
        return Err(bad("invalid_request", "current password is incorrect"));
    }
    if let Some(msg) = password_problem(&p.new, &a.email_of(&u)) {
        return Err(bad("invalid_request", msg));
    }
    a.db.update_user(&c.sub, json!({ "password": a.hash_password(p.new).await? })).await?;
    // Other devices must sign in again with the new password.
    let n = a.revoke_all_sessions(&c.sub, Some(&c.sid)).await?;
    a.audit("password_changed", Some(&c.sub), &ip, json!({ "sessions_revoked": n })).await;
    Ok(Json(json!({ "ok": true, "sessions_revoked": n })))
}

#[derive(Deserialize)]
pub struct ChallengeReq {
    address: String,
}

/// Step 1 of wallet linking: a one-time challenge for the wallet to sign.
pub async fn wallet_challenge(State(a): S, h: HeaderMap, Json(r): Json<ChallengeReq>) -> R {
    let c = a.bearer(&h)?;
    me_user(&a, &c).await?;
    if !chaindeal_core::is_address(&r.address) {
        return Err(bad("invalid_request", "not a ChainDeal address"));
    }
    let id = crypto::random_token();
    let nonce = crypto::random_token();
    a.db.put("challenges", &id, &c.sub, (now() + CHALLENGE_TTL) as f64, json!({ "user_id": c.sub, "address": r.address, "nonce": nonce })).await?;
    Ok(Json(json!({ "challenge_id": id, "message": chaindeal_authn::wallet_link_message(&nonce), "expires_in": CHALLENGE_TTL })))
}

#[derive(Deserialize)]
pub struct LinkReq {
    challenge_id: String,
    pubkey: String,
    signature: String,
}

/// Step 2: proof of possession. The ed25519 signature over the challenge must
/// verify, and the public key must hash to the claimed address.
pub async fn wallet_link(State(a): S, h: HeaderMap, Json(r): Json<LinkReq>) -> R {
    let c = a.bearer(&h)?;
    let ip = client_ip(&h);
    me_user(&a, &c).await?;
    let ch = a.db.consume("challenges", &r.challenge_id).await?.ok_or_else(|| bad("invalid_request", "challenge expired or already used"))?;
    if ch["user_id"] != c.sub.as_str() {
        return Err(forbidden("challenge belongs to another account"));
    }
    let address = ch["address"].as_str().unwrap_or_default();
    let pk: [u8; 32] = hex::decode(&r.pubkey).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| bad("invalid_request", "bad public key"))?;
    if chaindeal_core::address_from_pubkey(&pk) != address {
        return Err(bad("invalid_request", "public key does not match the address"));
    }
    let sig: [u8; 64] = hex::decode(&r.signature).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| bad("invalid_request", "bad signature"))?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|_| bad("invalid_request", "bad public key"))?;
    let msg = chaindeal_authn::wallet_link_message(ch["nonce"].as_str().unwrap_or_default());
    if vk.verify(msg.as_bytes(), &Signature::from_bytes(&sig)).is_err() {
        a.audit("wallet_link_failed", Some(&c.sub), &ip, json!({ "address": address })).await;
        return Err(bad("invalid_request", "signature does not verify"));
    }
    let doc = json!({ "address": address, "user_id": c.sub, "linked_at": now() });
    if !a.db.insert("wallets", address, &c.sub, 0.0, doc).await? {
        return Err(ApiError(StatusCode::CONFLICT, "conflict", "this wallet is already linked to an account".into()));
    }
    a.audit("wallet_linked", Some(&c.sub), &ip, json!({ "address": address })).await;
    Ok(Json(json!({ "ok": true, "address": address })))
}

pub async fn wallet_unlink(State(a): S, h: HeaderMap, Path(address): Path<String>) -> R {
    let c = a.bearer(&h)?;
    me_user(&a, &c).await?;
    let owned = a.wallets_of(&c.sub).await?.iter().any(|w| w["address"] == address.as_str());
    if !owned {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found", "wallet not linked to this account".into()));
    }
    a.db.delete("wallets", &address).await?;
    a.audit("wallet_unlinked", Some(&c.sub), &client_ip(&h), json!({ "address": address })).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn revoke_session(State(a): S, h: HeaderMap, Path(id): Path<String>) -> R {
    let c = a.bearer(&h)?;
    if !a.revoke_session(&c.sub, &id).await? {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found", "no such session".into()));
    }
    a.audit("session_revoked", Some(&c.sub), &client_ip(&h), json!({ "session": id })).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn logout_all(State(a): S, h: HeaderMap) -> R {
    let c = a.bearer(&h)?;
    let n = a.revoke_all_sessions(&c.sub, None).await?;
    a.audit("logout_all", Some(&c.sub), &client_ip(&h), json!({ "sessions_revoked": n })).await;
    Ok(Json(json!({ "ok": true, "sessions_revoked": n })))
}

// ---- admin ------------------------------------------------------------------------

/// Token must carry `users:admin` AND the account must still be an active admin.
async fn require_admin(a: &App, h: &HeaderMap) -> Result<Claims, ApiError> {
    let c = a.bearer(h)?;
    if !c.has_scope("users:admin") {
        return Err(forbidden("users:admin scope required"));
    }
    let u = me_user(a, &c).await?;
    if !App::roles_of(&u).iter().any(|r| r == "admin") {
        return Err(forbidden("admin role required"));
    }
    Ok(c)
}

#[derive(Deserialize)]
pub struct UsersQuery {
    offset: Option<u32>,
    limit: Option<u32>,
    email: Option<String>,
}

pub async fn admin_users(State(a): S, h: HeaderMap, Query(q): Query<UsersQuery>) -> R {
    require_admin(&a, &h).await?;
    let (total, users) = match q.email.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        // Exact-match search through the blind index (emails are encrypted).
        Some(e) => {
            let u = a.db.user_by_email_index(&a.keys.blind_index(&crypto::normalize_email(e))).await?;
            (u.is_some() as u64, u.into_iter().collect())
        }
        None => a.db.users_page(q.offset.unwrap_or(0), q.limit.unwrap_or(50).clamp(1, 200)).await?,
    };
    let mut items = Vec::new();
    for u in &users {
        items.push(a.user_view(u).await?);
    }
    Ok(Json(json!({ "total": total, "items": items })))
}

#[derive(Deserialize)]
pub struct UserPatch {
    roles: Option<Vec<String>>,
    status: Option<String>,
    unlock: Option<bool>,
}

pub async fn admin_update_user(State(a): S, h: HeaderMap, Path(id): Path<String>, Json(p): Json<UserPatch>) -> R {
    let c = require_admin(&a, &h).await?;
    let ip = client_ip(&h);
    a.user(&id).await?.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not_found", "no such user".into()))?;
    let mut patch = serde_json::Map::new();
    let roles_changed = p.roles.is_some();
    if let Some(mut roles) = p.roles {
        roles.retain(|r| ROLES.contains(&r.as_str()));
        if !roles.iter().any(|r| r == "user") {
            roles.insert(0, "user".into());
        }
        roles.dedup();
        if id == c.sub && !roles.iter().any(|r| r == "admin") {
            return Err(bad("invalid_request", "you cannot remove your own admin role"));
        }
        patch.insert("roles".into(), json!(roles));
    }
    if let Some(s) = &p.status {
        if s != "active" && s != "disabled" {
            return Err(bad("invalid_request", "status must be active or disabled"));
        }
        if id == c.sub && s == "disabled" {
            return Err(bad("invalid_request", "you cannot disable your own account"));
        }
        patch.insert("status".into(), json!(s));
    }
    if p.unlock == Some(true) {
        patch.insert("failed_logins".into(), json!(0));
        patch.insert("locked_until".into(), json!(0));
    }
    let u = a.db.update_user(&id, Value::Object(patch.clone())).await?.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not_found", "no such user".into()))?;
    let mut revoked = 0;
    if p.status.as_deref() == Some("disabled") || roles_changed {
        // Disabled users lose every session; role changes force re-authentication
        // so no token keeps privileges that were just removed.
        revoked = a.revoke_all_sessions(&id, None).await?;
    }
    a.audit("admin_user_updated", Some(&id), &ip, json!({ "by": c.sub, "changes": patch, "sessions_revoked": revoked })).await;
    Ok(Json(a.user_view(&u).await?))
}

/// Deletes an account entirely: sessions and refresh tokens are revoked,
/// wallet bindings released (so the wallets can be linked elsewhere) and the
/// user record removed. The audit trail is kept.
pub async fn admin_delete_user(State(a): S, h: HeaderMap, Path(id): Path<String>) -> R {
    let c = require_admin(&a, &h).await?;
    if id == c.sub {
        return Err(bad("invalid_request", "you cannot delete your own account"));
    }
    let u = a.user(&id).await?.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not_found", "no such user".into()))?;
    let sessions = a.revoke_all_sessions(&id, None).await?;
    let wallets = a.db.delete_by_idx("wallets", &id).await?;
    a.db.delete_by_idx("challenges", &id).await?;
    a.db.delete("users", &id).await?;
    a.audit("admin_user_deleted", Some(&id), &client_ip(&h),
        json!({ "by": c.sub, "name": u["name"], "sessions_revoked": sessions, "wallets_released": wallets })).await;
    Ok(Json(json!({ "ok": true, "sessions_revoked": sessions, "wallets_released": wallets })))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    limit: Option<u32>,
    user_id: Option<String>,
}

pub async fn admin_audit(State(a): S, h: HeaderMap, Query(q): Query<AuditQuery>) -> R {
    require_admin(&a, &h).await?;
    let items = a.db.audit_list(q.limit.unwrap_or(100).clamp(1, 500), q.user_id.as_deref().filter(|s| !s.is_empty())).await?;
    Ok(Json(json!({ "items": items })))
}
