//! chaindeal-auth — the identity microservice: OAuth 2.1 / OpenID Connect
//! authorization server, user management and wallet-ownership proofs.
//! Stateless pods; all state lives in its own Tarantool (auth-db).

mod account;
mod app;
mod crypto;
mod db;
mod oauth;
mod pages;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::http::{header, HeaderValue};
use axum::routing::{delete, get, patch, post};
use axum::Router;
use chaindeal_authn::{Issuer, JwkSet};
use serde_json::json;
use tokio::sync::Semaphore;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::app::{now, App, Limiter};

fn env(k: &str) -> Result<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty()).with_context(|| format!("{k} must be set"))
}

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string())
}

fn origin_of(url: &str) -> String {
    let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
    match url[scheme_end..].find('/') {
        Some(i) => url[..scheme_end + i].to_string(),
        None => url.to_string(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Local development: settings and secrets from a file written by
    // scripts/dev-cluster.sh (variables already set take precedence).
    if let Ok(path) = std::env::var("CHAINDEAL_ENV_FILE") {
        dotenvy::from_path(&path).with_context(|| format!("CHAINDEAL_ENV_FILE {path} (run scripts/dev-cluster.sh up)"))?;
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn".into()))
        .init();

    let db = db::Db::connect(&env_or("AUTH_DB_ADDR", "127.0.0.1:3302"), &env_or("AUTH_DB_USER", "auth"), &env("AUTH_DB_PASSWORD")?).await?;
    let signer = Issuer::from_seed_hex(&env("AUTH_SIGNING_KEY")?).map_err(|e| anyhow::anyhow!("AUTH_SIGNING_KEY: {e}"))?;
    let jwks = JwkSet { keys: vec![signer.jwk()] };
    let redirect_uris: Vec<String> = env_or("AUTH_WEB_REDIRECTS", "https://chaindeal.localhost/callback")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut origins: Vec<String> = redirect_uris.iter().map(|u| origin_of(u)).collect();
    origins.dedup();

    let app = Arc::new(App {
        db,
        keys: crypto::DataKeys::new(&env("AUTH_DATA_KEY")?, &env("AUTH_INDEX_KEY")?)?,
        signer,
        jwks,
        iss: env_or("AUTH_ISSUER", "https://chaindeal.localhost"),
        redirect_uris,
        origins: origins.clone(),
        seed_secret_hash: std::env::var("AUTH_SEED_CLIENT_SECRET").ok().filter(|s| !s.is_empty()).map(|s| crypto::hash_secret(&s)),
        sim_secret_hash: std::env::var("AUTH_SIM_CLIENT_SECRET").ok().filter(|s| !s.is_empty()).map(|s| crypto::hash_secret(&s)),
        limiter: Limiter::default(),
        hashing: Semaphore::new(4),
        dummy_hash: crypto::dummy_hash(),
    });
    bootstrap_admin(&app).await?;

    // Pages are script-free, so the CSP can forbid scripts outright. form-action
    // must also allow the post-login redirect targets.
    let csp = format!(
        "default-src 'none'; style-src 'self'; img-src 'self' data:; form-action 'self' {}; frame-ancestors 'none'; base-uri 'none'",
        origins.join(" ")
    );
    let css = include_str!("../assets/auth.css");

    let router = Router::new()
        .route("/.well-known/openid-configuration", get(oauth::discovery))
        .route("/oauth/jwks", get(oauth::jwks))
        .route("/oauth/authorize", get(oauth::authorize))
        .route("/oauth/authorize/resume", get(oauth::resume))
        .route("/oauth/login", post(oauth::login))
        .route("/oauth/register", get(oauth::register_page).post(oauth::register))
        .route("/oauth/token", post(oauth::token))
        .route("/oauth/logout", post(oauth::logout))
        .route("/oauth/userinfo", get(oauth::userinfo))
        .route("/oauth/me", get(account::me))
        .route("/oauth/me/password", post(account::change_password))
        .route("/oauth/me/wallets/challenge", post(account::wallet_challenge))
        .route("/oauth/me/wallets", post(account::wallet_link))
        .route("/oauth/me/wallets/:address", delete(account::wallet_unlink))
        .route("/oauth/me/sessions/:id", delete(account::revoke_session))
        .route("/oauth/me/logout-all", post(account::logout_all))
        .route("/oauth/admin/users", get(account::admin_users))
        .route("/oauth/admin/users/:id", patch(account::admin_update_user).delete(account::admin_delete_user))
        .route("/oauth/admin/audit", get(account::admin_audit))
        .route(
            "/oauth/assets/auth.css",
            get(move || async move { ([(header::CONTENT_TYPE, "text/css"), (header::CACHE_CONTROL, "public, max-age=3600")], css) }),
        )
        .route("/oauth/healthz", get(|| async { "ok" }))
        .with_state(app)
        .layer(SetResponseHeaderLayer::overriding(header::CONTENT_SECURITY_POLICY, HeaderValue::from_str(&csp)?))
        .layer(SetResponseHeaderLayer::overriding(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000; includeSubDomains")))
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")))
        .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
        .layer(TraceLayer::new_for_http());

    let bind = env_or("BIND_ADDR", "0.0.0.0:8080");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("chaindeal-auth listening on http://{bind}");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Creates the first administrator from env when the user table is empty.
async fn bootstrap_admin(a: &App) -> Result<()> {
    let (Ok(email), Ok(password)) = (env("AUTH_BOOTSTRAP_ADMIN_EMAIL"), env("AUTH_BOOTSTRAP_ADMIN_PASSWORD")) else {
        return Ok(());
    };
    if a.db.count("users").await? > 0 {
        return Ok(());
    }
    let email = crypto::normalize_email(&email);
    let id = crypto::random_id();
    let doc = json!({
        "id": id, "email_enc": a.keys.seal(&email, &id)?, "name": "Administrator",
        "password": a.hash_password(password).await?, "roles": ["user", "operator", "admin"],
        "status": "active", "created_at": now(), "failed_logins": 0, "locked_until": 0,
    });
    if a.db.insert("users", &id, &a.keys.blind_index(&email), 0.0, doc).await? {
        a.audit("bootstrap_admin_created", Some(&id), "startup", json!({})).await;
        tracing::info!("bootstrap administrator created");
    }
    Ok(())
}
