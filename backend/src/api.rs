use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chaindeal_core::SignedTx;
use futures::Stream;
use rmpv::Value as Mp;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::authz::{forbid, Authz, Denied};
use crate::chain::{Node, SubmitError};
use crate::documents::Documents;
use crate::sim::{SimControl, SimPatch};

#[derive(Clone)]
pub struct AppState {
    pub node: Arc<Node>,
    pub sim: Arc<SimControl>,
    /// Access-token verification; `None` only when explicitly disabled for local dev.
    pub authz: Option<Arc<Authz>>,
    /// Contract-document issuance and verification (market attestation key).
    pub docs: Arc<Documents>,
}

impl From<Denied> for ApiError {
    fn from(d: Denied) -> Self {
        ApiError(d.0, d.1)
    }
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("{e:#}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
}

#[derive(Deserialize)]
struct Paging {
    limit: Option<u32>,
    before: Option<u64>,
}

fn limit(p: &Paging, default: u32) -> u32 {
    p.limit.unwrap_or(default).clamp(1, 200)
}

fn opt_str(v: &Option<String>) -> Mp {
    v.as_deref().filter(|s| !s.is_empty()).map(Mp::from).unwrap_or(Mp::Nil)
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/metrics", get(metrics))
        .route("/api/policies", get(policies))
        .route("/api/blocks", get(list_blocks))
        .route("/api/blocks/:key", get(get_block))
        .route("/api/txs", get(list_txs))
        .route("/api/tx", post(submit_tx))
        .route("/api/tx/:hash", get(get_tx))
        .route("/api/accounts", get(search_accounts))
        .route("/api/accounts/lookup", get(lookup_accounts))
        .route("/api/accounts/:addr", get(get_account))
        .route("/api/deals", get(list_deals))
        .route("/api/deals/:id", get(get_deal))
        .route("/api/chain/verify", get(verify_chain))
        .route("/api/sim", get(sim_get).post(sim_patch))
        .route("/api/cluster", get(cluster))
        .route("/api/deals/:id/document", get(deal_document))
        .route("/api/documents/verify", get(verify_document))
        .route("/api/attestation", get(attestation))
        .route("/api/events", get(events))
        .with_state(state)
        // Defence in depth for API responses (the UI's own headers come from nginx).
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ))
        .layer(SetResponseHeaderLayer::overriding(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")))
        .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
        ))
}

async fn health(State(s): State<AppState>) -> ApiResult {
    let tip = s.node.db.reader().tip().await?;
    Ok(Json(json!({
        "ok": true,
        "node": s.node.node_id,
        "role": if s.node.is_leader() { "leader" } else { "follower" },
        "height": tip.map(|b| b.header.height),
    })))
}

async fn stats(State(s): State<AppState>) -> ApiResult {
    let n = &s.node;
    let db = n.db.reader();
    let mut v = db.call("cd_stats", vec![]).await?;
    v["node"] = json!(n.node_id);
    v["difficulty"] = json!(n.difficulty);
    let treasury = db.get_accounts(&[chaindeal_core::TREASURY_ADDRESS.to_string()]).await?;
    v["treasury"] = json!(treasury.first().map(|a| a.balance).unwrap_or(0));
    Ok(Json(v))
}

async fn metrics(State(s): State<AppState>) -> ApiResult {
    Ok(Json(s.node.metrics().await?))
}

async fn policies() -> ApiResult {
    Ok(Json(json!(chaindeal_core::contract::POLICIES)))
}

async fn list_blocks(State(s): State<AppState>, Query(p): Query<Paging>) -> ApiResult {
    let before = p.before.map(Mp::from).unwrap_or(Mp::Nil);
    Ok(Json(s.node.db.reader().call("cd_list_blocks", vec![before, Mp::from(limit(&p, 20))]).await?))
}

async fn get_block(State(s): State<AppState>, Path(key): Path<String>) -> ApiResult {
    let arg = match key.parse::<u64>() {
        Ok(h) => Mp::from(h),
        Err(_) => Mp::from(key),
    };
    match s.node.db.reader().call("cd_get_block", vec![arg]).await? {
        Value::Null => Err(not_found("block")),
        v => Ok(Json(v)),
    }
}

async fn list_txs(State(s): State<AppState>, Query(p): Query<Paging>) -> ApiResult {
    Ok(Json(s.node.db.reader().call("cd_recent_txs", vec![Mp::from(limit(&p, 30))]).await?))
}

async fn get_tx(State(s): State<AppState>, Path(hash): Path<String>) -> ApiResult {
    match s.node.db.call("cd_get_tx", vec![Mp::from(hash)]).await? {
        Value::Null => Err(not_found("transaction")),
        v => Ok(Json(v)),
    }
}

async fn submit_tx(State(s): State<AppState>, headers: HeaderMap, Json(tx): Json<SignedTx>) -> Result<(StatusCode, Json<Value>), ApiError> {
    // The ed25519 signature proves who *signed*; the access token proves which
    // *account* is acting, and that account must own the signing wallet.
    if let Some(authz) = &s.authz {
        let c = authz.claims(&headers).await?;
        if !c.has_scope("deals:write") {
            return Err(forbid("deals:write scope required").into());
        }
        let owns = c.wallets.iter().any(|w| w == &tx.body.from);
        if !owns && !c.has_scope("tx:any") {
            return Err(forbid("this wallet is not linked to your account — link it on the Wallet page first").into());
        }
    }
    match s.node.submit(tx).await {
        Ok(r) => Ok((StatusCode::ACCEPTED, Json(json!({ "hash": r.hash, "status": r.status, "deal_id": r.deal_id })))),
        Err(SubmitError::Invalid(m)) => Err(ApiError(StatusCode::BAD_REQUEST, m)),
        Err(SubmitError::Rejected(m)) => Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, m)),
        Err(SubmitError::Internal(e)) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct AccountQuery {
    q: Option<String>,
    kind: Option<String>,
    offset: Option<u32>,
    limit: Option<u32>,
}

/// Directory search: `{ total, items }`, ordered by settled deals.
async fn search_accounts(State(s): State<AppState>, Query(q): Query<AccountQuery>) -> ApiResult {
    let args = vec![
        opt_str(&q.q),
        opt_str(&q.kind),
        Mp::from(q.offset.unwrap_or(0)),
        Mp::from(q.limit.unwrap_or(50).clamp(1, 200)),
    ];
    Ok(Json(s.node.db.reader().call("cd_search_accounts", args).await?))
}

#[derive(Deserialize)]
struct Lookup {
    addrs: String,
}

/// Batch name resolution for the UI: `?addrs=0x..,0x..` (max 200).
async fn lookup_accounts(State(s): State<AppState>, Query(q): Query<Lookup>) -> ApiResult {
    let addrs: Vec<String> = q.addrs.split(',').filter(|a| !a.is_empty()).take(200).map(str::to_string).collect();
    Ok(Json(json!(s.node.db.reader().get_accounts(&addrs).await?)))
}

async fn get_account(State(s): State<AppState>, Path(addr): Path<String>) -> ApiResult {
    let db = s.node.db.reader();
    let acct = db.get_accounts(std::slice::from_ref(&addr)).await?;
    let Some(account) = acct.into_iter().next() else {
        return Err(not_found("account"));
    };
    let deals = db.call("cd_deals_for", vec![Mp::from(addr.clone()), Mp::from(100u32)]).await?;
    let txs = db.call("cd_account_txs", vec![Mp::from(addr), Mp::from(50u32)]).await?;
    Ok(Json(json!({ "account": account, "deals": deals, "txs": txs })))
}

#[derive(Deserialize)]
struct DealQuery {
    limit: Option<u32>,
    offset: Option<u32>,
    #[serde(rename = "type")]
    dtype: Option<String>,
    status: Option<String>,
}

async fn list_deals(State(s): State<AppState>, Query(q): Query<DealQuery>) -> ApiResult {
    let deals = s
        .node
        .db
        .reader()
        .list_deals(
            q.limit.unwrap_or(100).clamp(1, 500),
            q.offset.unwrap_or(0),
            q.dtype.as_deref().filter(|v| !v.is_empty()),
            q.status.as_deref().filter(|v| !v.is_empty()),
        )
        .await?;
    Ok(Json(json!(deals)))
}

async fn get_deal(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let deal = s.node.db.reader().get_deals(&[id]).await?;
    deal.into_iter().next().map(|d| Json(json!(d))).ok_or_else(|| not_found("deal"))
}

#[derive(Deserialize)]
struct VerifyQuery {
    from: Option<u64>,
    count: Option<u32>,
    prev: Option<String>,
}

/// Verifies a range of blocks; the UI walks the whole chain in chunks.
/// The attested contract document for a deal (record + market signature).
async fn deal_document(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult {
    match s.docs.issue(&s.node.db.reader(), &id).await? {
        Some(doc) => Ok(Json(doc)),
        None => Err(not_found("deal")),
    }
}

#[derive(Deserialize)]
struct VerifyDoc {
    deal: String,
    hash: String,
    sig: String,
}

/// Verifies a document against this market's chain and attestation key.
async fn verify_document(State(s): State<AppState>, Query(q): Query<VerifyDoc>) -> ApiResult {
    let valid_hex = |v: &str, n: usize| v.len() == n && v.bytes().all(|c| c.is_ascii_hexdigit());
    if !valid_hex(&q.hash, 64) || !valid_hex(&q.sig, 128) || q.deal.len() > 64 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "malformed verification request".into()));
    }
    Ok(Json(s.docs.verify(&s.node.db.reader(), &q.deal, &q.hash.to_lowercase(), &q.sig.to_lowercase()).await?))
}

/// This market's attestation identity: public key and chain id.
async fn attestation(State(s): State<AppState>) -> ApiResult {
    let chain_id = s.docs.chain_id(&s.node.db.reader()).await?;
    Ok(Json(json!({ "alg": "Ed25519", "key_id": s.docs.key_id, "public_key": s.docs.public_key, "chain_id": chain_id })))
}

async fn verify_chain(State(s): State<AppState>, Query(q): Query<VerifyQuery>) -> ApiResult {
    let report = s
        .node
        .verify_range(q.from.unwrap_or(0), q.count.unwrap_or(200).clamp(1, 500), q.prev.filter(|p| !p.is_empty()))
        .await?;
    Ok(Json(json!(report)))
}

async fn sim_get(State(s): State<AppState>) -> ApiResult {
    let mut v = s.sim.shared_snapshot().await;
    v["control_role"] = json!(if s.authz.is_some() { "operator" } else { "anyone" });
    Ok(Json(v))
}

/// Changing the market requires `sim:control` and an operator/admin role.
async fn sim_patch(State(s): State<AppState>, headers: HeaderMap, Json(p): Json<SimPatch>) -> ApiResult {
    if let Some(authz) = &s.authz {
        let c = authz.claims(&headers).await?;
        if !c.has_scope("sim:control") || !(c.has_role("operator") || c.has_role("admin")) {
            return Err(forbid("operator role required to control the market").into());
        }
        tracing::info!(user = %c.sub, "simulator reconfigured");
    }
    s.sim.patch(p).await?;
    let mut v = s.sim.shared_snapshot().await;
    v["control_role"] = json!(if s.authz.is_some() { "operator" } else { "anyone" });
    Ok(Json(v))
}

/// Cluster topology: live API nodes (with the leader) and the Tarantool
/// instances this node talks to (master, and the replica behind the read pool).
async fn cluster(State(s): State<AppState>) -> ApiResult {
    let db = &s.node.db;
    let nodes = db.call("cd_nodes", vec![Mp::from(10u32)]).await?;
    let leader = db.lease_holder("producer").await?;
    let master = db.call("cd_instance_info", vec![]).await?;
    let replica = if db.has_reader() { Some(db.reader().call("cd_instance_info", vec![]).await?) } else { None };
    Ok(Json(json!({
        "serving_node": s.node.node_id,
        "leader": leader,
        "nodes": nodes,
        "master": master,
        "read_replica": replica,
    })))
}

async fn events(State(s): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(s.node.events.subscribe())
        .filter_map(|m| m.ok())
        .map(|(seq, m)| Ok(Event::default().id(seq.to_string()).data(m)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}
