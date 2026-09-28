use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::http::HeaderValue;
use chaindeal_backend::{api, authz, chain, db, documents, sim};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn".into()))
        .init();

    let (user, password) = (env("TARANTOOL_USER", "chaindeal"), env("TARANTOOL_PASSWORD", "chaindeal"));
    let tt_addr = env("TARANTOOL_ADDR", "127.0.0.1:3301");
    let mut db = db::Db::connect(&tt_addr, &user, &password).await?;
    tracing::info!("connected to tarantool master at {tt_addr}");
    // Optional read pool (replicas) for list/stat/verify queries.
    if let Ok(ro) = std::env::var("TARANTOOL_READ_ADDR") {
        if !ro.is_empty() {
            db = db.with_reader(&ro, &user, &password).await?;
            tracing::info!("using tarantool read pool at {ro}");
        }
    }

    // In Kubernetes the pod name is a stable, unique node id.
    let node_id = env("CHAINDEAL_NODE_ID", &env("HOSTNAME", "node-1"));
    let difficulty = env("CHAINDEAL_DIFFICULTY", &chaindeal_core::DEFAULT_DIFFICULTY.to_string()).parse()?;
    let node = Arc::new(chain::Node::new(db, node_id.clone(), difficulty));

    // Cluster plumbing: leader lease + heartbeat, and the shared event/metrics bus.
    tokio::spawn(node.clone().run_leadership());
    tokio::spawn(node.clone().run_bus());

    // Block production runs on whichever node holds the lease.
    let interval = Duration::from_millis(env("CHAINDEAL_BLOCK_MS", "2000").parse()?);
    let producer = node.clone();
    tokio::spawn(async move { producer.run_producer(interval).await });

    // Live market simulator, also leader-only; controllable via /api/sim.
    let sim = sim::Sim::new(
        node.clone(),
        env("CHAINDEAL_SIM_AGENTS", "6000").parse()?,
        sim::SimConfig {
            running: env("CHAINDEAL_SIM", "on") == "on",
            rate: env("CHAINDEAL_SIM_RATE", "10").parse()?,
            pressure: env("CHAINDEAL_SIM_PRESSURE", "1").parse()?,
            noise: 0.02,
        },
    );
    tokio::spawn(sim.clone().run());

    // Access tokens come from the auth microservice. Authorization can only be
    // turned off explicitly (CHAINDEAL_AUTH=off), never by forgetting config.
    let authz = if env("CHAINDEAL_AUTH", "on") == "off" {
        tracing::warn!("CHAINDEAL_AUTH=off: transaction and simulator endpoints are UNPROTECTED");
        None
    } else {
        let jwks = env("AUTH_JWKS_URL", "http://auth:8080/oauth/jwks");
        let issuer = env("AUTH_ISSUER", "https://chaindeal.localhost");
        tracing::info!("verifying access tokens from {issuer} (keys: {jwks})");
        Some(Arc::new(authz::Authz::new(jwks, issuer)))
    };
    // Market attestation key for contract documents (shared by all nodes via a Secret).
    let docs = Arc::new(documents::Documents::new(std::env::var("CHAINDEAL_ATTESTATION_KEY").ok().filter(|k| !k.is_empty()))?);
    tracing::info!(key_id = %docs.key_id, "document attestation key loaded");
    let mut app = api::router(api::AppState { node: node.clone(), sim, authz, docs });
    // Optionally serve the built frontend (compose image). In Kubernetes nginx serves it.
    if let Ok(dir) = std::env::var("STATIC_DIR") {
        let index = format!("{dir}/index.html");
        app = app.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)));
    }
    // Same-origin by default; cross-origin callers must be listed explicitly.
    if let Ok(origins) = std::env::var("CHAINDEAL_CORS_ORIGINS") {
        let list: Vec<HeaderValue> = origins.split(',').filter_map(|o| o.trim().parse().ok()).collect();
        if !list.is_empty() {
            app = app.layer(CorsLayer::new().allow_origin(list).allow_methods(tower_http::cors::Any).allow_headers(tower_http::cors::Any));
        }
    }
    let app = app.layer(TraceLayer::new_for_http());

    let bind = env("BIND_ADDR", "0.0.0.0:8080");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(node = %node_id, "ChainDeal node listening on http://{bind}");
    axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
    node.release_leadership().await;
    Ok(())
}

/// Stop accepting connections on SIGTERM (Kubernetes rolling updates) or Ctrl-C.
async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
    tracing::info!("shutting down");
}
