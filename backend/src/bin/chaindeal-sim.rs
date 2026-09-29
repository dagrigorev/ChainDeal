//! `chaindeal-sim`: the live market simulator as its own service.
//!
//! It streams every signed transaction to the chain nodes over gRPC
//! (`LedgerService.SubmitTxs`, one long-lived bidirectional stream) and gets a receipt
//! for each. It authenticates as the OAuth confidential client `chaindeal-sim`
//! (client credentials) and reads chain state from the read replica.
//!
//! Environment:
//!   LEDGER_GRPC_URL         http://backend:9090
//!   AUTH_TOKEN_URL          http://auth:8080/oauth/token
//!   SIM_CLIENT_ID           chaindeal-sim
//!   SIM_CLIENT_SECRET       (Secret)
//!   TARANTOOL_ADDR / TARANTOOL_READ_ADDR / TARANTOOL_USER / TARANTOOL_PASSWORD
//!   CHAINDEAL_SIM, CHAINDEAL_SIM_RATE, CHAINDEAL_SIM_PRESSURE, CHAINDEAL_SIM_AGENTS
//!   HEALTH_ADDR             0.0.0.0:8080 (GET /healthz)

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::routing::get;
use chaindeal_backend::chain::now_ms;
use chaindeal_backend::grpc::{LedgerSubmitter, TokenSource};
use chaindeal_backend::{db, sim};
use futures::future::BoxFuture;
use serde::Deserialize;
use tracing_subscriber::EnvFilter;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// OAuth 2.1 client credentials against the identity service, cached until
/// shortly before expiry.
struct ClientCredentials {
    http: reqwest::Client,
    url: String,
    id: String,
    secret: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

impl TokenSource for ClientCredentials {
    fn token(&self) -> BoxFuture<'_, Result<(String, u64)>> {
        Box::pin(async move {
            let r = self
                .http
                .post(&self.url)
                .basic_auth(&self.id, Some(&self.secret))
                .form(&[("grant_type", "client_credentials"), ("scope", "deals:write tx:any")])
                .send()
                .await?;
            if !r.status().is_success() {
                return Err(anyhow!("token endpoint answered {}: {}", r.status(), r.text().await.unwrap_or_default()));
            }
            let t: TokenResponse = r.json().await?;
            Ok((t.access_token, now_ms() / 1000 + t.expires_in))
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Ok(path) = std::env::var("CHAINDEAL_ENV_FILE") {
        dotenvy::from_path(&path).with_context(|| format!("CHAINDEAL_ENV_FILE {path}"))?;
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let (user, password) = (env("TARANTOOL_USER", "chaindeal"), env("TARANTOOL_PASSWORD", "chaindeal"));
    let mut db = db::Db::connect(&env("TARANTOOL_ADDR", "127.0.0.1:3301"), &user, &password).await?;
    let ro = env("TARANTOOL_READ_ADDR", "");
    if !ro.is_empty() {
        db = db.with_reader(&ro, &user, &password).await?;
    }

    let tokens = Arc::new(ClientCredentials {
        http: reqwest::Client::builder().timeout(Duration::from_secs(5)).build()?,
        url: env("AUTH_TOKEN_URL", "http://auth:8080/oauth/token"),
        id: env("SIM_CLIENT_ID", "chaindeal-sim"),
        secret: std::env::var("SIM_CLIENT_SECRET").ok().filter(|s| !s.is_empty()).context("SIM_CLIENT_SECRET must be set")?,
    });
    let endpoint = env("LEDGER_GRPC_URL", "http://backend:9090");
    tracing::info!(%endpoint, client = %tokens.id, "streaming transactions to the LedgerService");
    let submitter = Arc::new(LedgerSubmitter::spawn(endpoint, tokens));

    let id = env("HOSTNAME", "chaindeal-sim");
    let sim = sim::Sim::new(
        id,
        db,
        submitter,
        env("CHAINDEAL_SIM_AGENTS", "6000").parse()?,
        sim::SimConfig {
            running: env("CHAINDEAL_SIM", "on") == "on",
            rate: env("CHAINDEAL_SIM_RATE", "10").parse()?,
            pressure: env("CHAINDEAL_SIM_PRESSURE", "1").parse()?,
            ..Default::default()
        },
    );
    let runner = tokio::spawn(sim.clone().run());

    let health = axum::Router::new().route("/healthz", get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind(env("HEALTH_ADDR", "0.0.0.0:8080")).await?;
    tokio::select! {
        r = axum::serve(listener, health) => r?,
        _ = shutdown() => {}
        _ = runner => return Err(anyhow!("simulator stopped")),
    }
    sim.release().await;
    tracing::info!("simulator released its lease");
    Ok(())
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
}
