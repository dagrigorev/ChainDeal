//! gRPC `chaindeal.v1.LedgerService` (proto/chaindeal/v1/ledger.proto): streaming
//! transaction data between services and to browsers.
//!
//! Every node serves it on its gRPC port, as native gRPC (HTTP/2, services)
//! and gRPC-Web (browsers, via the ingress). The live streams are fed by the
//! same shared event log as the rest of the cluster, so a client sees every
//! node's activity whichever node it is connected to.
//!
//! Also here: [`LedgerSubmitter`], the client side of `SubmitTxs` used by the
//! market simulator service.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chaindeal_core::{canonical_body, SignedTx, TxBody, TxRecord};
use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex, OnceCell};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::authz::{Authz, Denied};
use crate::chain::{now_ms, Node, SubmitError};
use crate::db::Db;
use crate::sim::{Receipt, Submitter};

pub mod pb {
    tonic::include_proto!("chaindeal.v1");
    /// Encoded descriptors, for server reflection (`grpcurl list`).
    pub const DESCRIPTOR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ledger_descriptor.bin"));
}

use pb::ledger_service_server::{LedgerService, LedgerServiceServer};
use pb::tx_receipt::Outcome;

/// Blocks sent per catch-up batch in `FollowBlocks`.
const CATCH_UP_BATCH: u32 = 20;
/// Sealed blocks kept in memory for `Watch(with_transactions)` and live `FollowBlocks`.
const BLOCK_CACHE: usize = 64;

// ------------------------------------------------------------------ server

pub struct LedgerNode {
    node: Arc<Node>,
    authz: Option<Arc<Authz>>,
    blocks: Arc<BlockCache>,
    /// Flips to true on shutdown: open streams end so clients reconnect elsewhere.
    stopping: watch::Receiver<bool>,
}

/// Serves the LedgerService (plus reflection) until `stopping` flips.
pub async fn serve(addr: SocketAddr, node: Arc<Node>, authz: Option<Arc<Authz>>, stopping: watch::Receiver<bool>) -> Result<()> {
    let svc = LedgerNode { blocks: Arc::new(BlockCache::new(node.db.clone())), node, authz, stopping: stopping.clone() };
    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(pb::DESCRIPTOR)
        .build_v1()?;
    let mut stop = stopping;
    tracing::info!("gRPC LedgerService (gRPC + gRPC-Web) listening on {addr}");
    tonic::transport::Server::builder()
        // Keeps idle streams alive through proxies and detects dead peers.
        .http2_keepalive_interval(Some(Duration::from_secs(20)))
        // gRPC-Web from browsers arrives as HTTP/1.1 or HTTP/2.
        .accept_http1(true)
        .layer(tonic_web::GrpcWebLayer::new())
        .add_service(LedgerServiceServer::new(svc))
        .add_service(reflection)
        .serve_with_shutdown(addr, async move { stopped(&mut stop).await })
        .await?;
    Ok(())
}

type EventStream = ReceiverStream<Result<pb::LedgerEvent, Status>>;
type BlockStream = ReceiverStream<Result<pb::SealedBlock, Status>>;
type ReceiptStream = ReceiverStream<Result<pb::TxReceipt, Status>>;

#[tonic::async_trait]
impl LedgerService for LedgerNode {
    type WatchStream = EventStream;
    type FollowBlocksStream = BlockStream;
    type SubmitTxsStream = ReceiptStream;

    async fn watch(&self, req: Request<pb::WatchRequest>) -> Result<Response<EventStream>, Status> {
        let pb::WatchRequest { after_seq, with_transactions } = req.into_inner();
        // Subscribe before replaying, so nothing falls between history and live.
        let mut live = self.node.events.subscribe();
        let (out, rx) = mpsc::channel(256);
        let (node, blocks, mut stopping) = (self.node.clone(), self.blocks.clone(), self.stopping.clone());
        tokio::spawn(async move {
            let send = |seq: u64, data: String| {
                let (out, blocks) = (out.clone(), blocks.clone());
                async move {
                    match to_event(seq, &data, with_transactions.then_some(&*blocks)).await {
                        Some(ev) => out.send(Ok(ev)).await.is_ok(),
                        None => true,
                    }
                }
            };
            let mut last = after_seq;
            if after_seq > 0 && !replay(&node.db, &mut last, &send).await {
                return;
            }
            loop {
                let item = tokio::select! {
                    item = live.recv() => item,
                    _ = stopped(&mut stopping) => {
                        let _ = out.send(Err(Status::unavailable("node shutting down; reconnect"))).await;
                        return;
                    }
                };
                match item {
                    Ok((seq, data)) if seq > last => {
                        last = seq;
                        if !send(seq, data).await {
                            return; // client went away
                        }
                    }
                    Ok(_) => {}
                    // Too slow for the live feed: fill the gap from the log.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if !replay(&node.db, &mut last, &send).await {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn follow_blocks(&self, req: Request<pb::FollowBlocksRequest>) -> Result<Response<BlockStream>, Status> {
        let pb::FollowBlocksRequest { from_height, to_height } = req.into_inner();
        let mut live = self.node.events.subscribe();
        let (out, rx) = mpsc::channel(8); // small: a slow client throttles the catch-up
        let (node, mut stopping) = (self.node.clone(), self.stopping.clone());
        tokio::spawn(async move {
            let mut next = from_height;
            loop {
                let tip = match node.db.tip().await {
                    Ok(t) => t.map(|b| b.header.height).unwrap_or(0),
                    Err(e) => {
                        let _ = out.send(Err(Status::unavailable(e.to_string()))).await;
                        return;
                    }
                };
                // Catch up to the tip (bounded by to_height), in batches.
                while next <= tip && (to_height == 0 || next <= to_height) {
                    // History comes from the read replica; the last stretch from the
                    // master, which the replica may not have caught up with yet.
                    let db = if tip - next > 2 * CATCH_UP_BATCH as u64 { node.db.reader() } else { node.db.clone() };
                    let batch = match load_blocks(&db, next, CATCH_UP_BATCH).await {
                        Ok(b) if !b.is_empty() => b,
                        Ok(_) => break,
                        Err(e) => {
                            let _ = out.send(Err(Status::unavailable(e.to_string()))).await;
                            return;
                        }
                    };
                    for b in batch {
                        let h = b.header.as_ref().map(|h| h.height).unwrap_or(next);
                        if to_height != 0 && h > to_height {
                            return;
                        }
                        next = h + 1;
                        if out.send(Ok(b)).await.is_err() {
                            return;
                        }
                    }
                }
                if to_height != 0 && next > to_height {
                    return;
                }
                // Wait for the next sealed block (or a gap in the feed).
                loop {
                    let item = tokio::select! {
                        item = live.recv() => item,
                        _ = stopped(&mut stopping) => {
                            let _ = out.send(Err(Status::unavailable("node shutting down; resume from the last height"))).await;
                            return;
                        }
                    };
                    match item {
                        Ok((_, data)) if data.contains(r#""type":"block""#) => break,
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => break,
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn submit_txs(&self, req: Request<Streaming<pb::SubmitTxsRequest>>) -> Result<Response<ReceiptStream>, Status> {
        // The ed25519 signature proves who signed; the access token proves which
        // account (or service) is acting — the same rules as POST /api/tx.
        let claims = match &self.authz {
            Some(a) => {
                let c = a.claims(&req.metadata().clone().into_headers()).await.map_err(status_of)?;
                if !c.has_scope("deals:write") {
                    return Err(Status::permission_denied("deals:write scope required"));
                }
                Some(c)
            }
            None => None,
        };
        let mut inbound = req.into_inner();
        let (out, rx) = mpsc::channel(256);
        let (node, mut stopping) = (self.node.clone(), self.stopping.clone());
        tokio::spawn(async move {
            loop {
                let msg = tokio::select! {
                    m = inbound.message() => m,
                    _ = stopped(&mut stopping) => {
                        let _ = out.send(Err(Status::unavailable("node shutting down; reconnect"))).await;
                        return;
                    }
                };
                let m = match msg {
                    Ok(Some(m)) => m,
                    Ok(None) => return, // client finished
                    Err(e) => {
                        tracing::debug!("SubmitTxs stream error: {e}");
                        return;
                    }
                };
                // Streams outlive tokens: re-check expiry on every message.
                if let Some(c) = &claims {
                    if c.exp <= now_ms() / 1000 {
                        let _ = out.send(Err(Status::unauthenticated("access token expired; reconnect with a fresh one"))).await;
                        return;
                    }
                }
                let receipt = admit(&node, claims.as_ref(), m).await;
                if out.send(Ok(receipt)).await.is_err() {
                    return;
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// Resolves once the node starts shutting down.
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

fn status_of(d: Denied) -> Status {
    match d.0.as_u16() {
        401 => Status::unauthenticated(d.1),
        403 => Status::permission_denied(d.1),
        _ => Status::internal(d.1),
    }
}

async fn admit(node: &Node, claims: Option<&chaindeal_authn::Claims>, m: pb::SubmitTxsRequest) -> pb::TxReceipt {
    let receipt = |outcome: Outcome, error: String, deal_id: Option<String>| pb::TxReceipt {
        r#ref: m.r#ref,
        hash: m.hash.clone(),
        outcome: outcome as i32,
        error,
        deal_id: deal_id.unwrap_or_default(),
    };
    let body: TxBody = match serde_json::from_str(&m.body_json) {
        Ok(b) => b,
        Err(e) => return receipt(Outcome::Invalid, format!("malformed body_json: {e}"), None),
    };
    if let Some(c) = claims {
        if !c.wallets.iter().any(|w| w == &body.from) && !c.has_scope("tx:any") {
            return receipt(Outcome::Forbidden, "this wallet is not linked to your account".into(), None);
        }
    }
    let tx = SignedTx { hash: m.hash.clone(), body, signature: m.signature.clone() };
    match node.submit(tx).await {
        Ok(r) => receipt(Outcome::Admitted, String::new(), r.deal_id),
        Err(SubmitError::Invalid(e)) => receipt(Outcome::Invalid, e, None),
        Err(SubmitError::Rejected(e)) => receipt(Outcome::Rejected, e, None),
        Err(SubmitError::Internal(e)) => {
            tracing::error!("SubmitTxs: {e:#}");
            receipt(Outcome::Error, "internal error".into(), None)
        }
    }
}

/// Sends every logged event after `last` (advancing it). False once the client is gone.
async fn replay<F, Fut>(db: &Db, last: &mut u64, send: &F) -> bool
where
    F: Fn(u64, String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    loop {
        let batch = match db.events_since(*last, 500).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("event replay failed: {e:#}");
                return true;
            }
        };
        let n = batch.len();
        for (seq, data) in batch {
            *last = seq;
            if !send(seq, data).await {
                return false;
            }
        }
        if n < 500 {
            return true;
        }
    }
}

// ------------------------------------------------------------------ conversions

/// Maps a shared-log event (JSON, see `Node::emit`) to its protobuf form.
async fn to_event(seq: u64, data: &str, blocks: Option<&BlockCache>) -> Option<pb::LedgerEvent> {
    use pb::ledger_event::Event;
    let v: Value = serde_json::from_str(data).ok()?;
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let event = match v["type"].as_str()? {
        "pending" => Event::Pending(pb::TxPending { hash: s("hash"), action: s("action") }),
        "refused" => Event::Refused(pb::TxRefused { error: s("error") }),
        "rejected" => Event::Rejected(pb::TxRejected { hash: s("hash"), from: s("from"), error: s("error") }),
        "leader" => Event::Leader(pb::LeaderChanged { node: s("node"), leader: v["leader"].as_bool().unwrap_or(false) }),
        "block" => {
            let height = v["height"].as_u64()?;
            let transactions = match blocks {
                Some(cache) => match cache.get(height).await {
                    Ok(b) => b.transactions.clone(),
                    Err(e) => {
                        tracing::warn!("block {height} for Watch: {e:#}");
                        vec![]
                    }
                },
                None => vec![],
            };
            let transitions = v["transitions"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|t| pb::DealTransition {
                            id: t["id"].as_str().unwrap_or_default().into(),
                            from: t["from"].as_str().unwrap_or_default().into(),
                            to: t["to"].as_str().unwrap_or_default().into(),
                            deal_type: t["deal_type"].as_str().unwrap_or_default().into(),
                            amount: t["amount"].as_u64().unwrap_or(0),
                            title: t["title"].as_str().unwrap_or_default().into(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let accounts = v["accounts"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            Event::Block(pb::BlockSealed {
                height,
                hash: s("hash"),
                tx_count: v["tx_count"].as_u64().unwrap_or(0) as u32,
                rejected: v["rejected"].as_u64().unwrap_or(0) as u32,
                transitions,
                accounts,
                transactions,
            })
        }
        _ => return None,
    };
    Some(pb::LedgerEvent { seq, event: Some(event) })
}

fn enum_str<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

pub fn tx_to_pb(r: &TxRecord) -> pb::Transaction {
    pb::Transaction {
        hash: r.hash.clone(),
        body_json: String::from_utf8(canonical_body(&r.body)).unwrap_or_default(),
        signature: r.signature.clone(),
        from: r.body.from.clone(),
        action: r.body.action.name().to_string(),
        status: enum_str(&r.status),
        error: r.error.clone().unwrap_or_default(),
        block_height: r.block_height.unwrap_or(0),
        deal_id: r.deal_id.clone().unwrap_or_default(),
    }
}

#[derive(Deserialize)]
struct BlockWithTxs {
    block: chaindeal_core::Block,
    #[serde(default)]
    txs: Vec<TxRecord>,
}

fn block_to_pb(b: BlockWithTxs) -> pb::SealedBlock {
    let h = b.block.header;
    // Transactions in block (Merkle) order.
    let order: HashMap<&str, usize> = b.block.tx_hashes.iter().enumerate().map(|(i, t)| (t.as_str(), i)).collect();
    let mut txs = b.txs;
    txs.retain(|t| order.contains_key(t.hash.as_str()));
    txs.sort_by_key(|t| order[t.hash.as_str()]);
    pb::SealedBlock {
        header: Some(pb::BlockHeader {
            height: h.height,
            prev_hash: h.prev_hash,
            timestamp: h.timestamp,
            merkle_root: h.merkle_root,
            tx_count: h.tx_count,
            difficulty: h.difficulty,
            nonce: h.nonce,
            producer: h.producer,
        }),
        hash: b.block.hash,
        transactions: txs.iter().map(tx_to_pb).collect(),
    }
}

async fn load_blocks(db: &Db, from: u64, limit: u32) -> Result<Vec<pb::SealedBlock>> {
    let v = db.call("cd_blocks_range", vec![rmpv::Value::from(from), rmpv::Value::from(limit)]).await?;
    let list: Vec<BlockWithTxs> = serde_json::from_value(v).map_err(|e| anyhow!("blocks from {from}: {e}"))?;
    Ok(list.into_iter().map(block_to_pb).collect())
}

/// Recently sealed blocks, loaded once per node however many streams want them.
struct BlockCache {
    db: Db,
    map: Mutex<BTreeMap<u64, Arc<OnceCell<Arc<pb::SealedBlock>>>>>,
}

impl BlockCache {
    fn new(db: Db) -> Self {
        BlockCache { db, map: Mutex::new(BTreeMap::new()) }
    }

    async fn get(&self, height: u64) -> Result<Arc<pb::SealedBlock>> {
        let cell = {
            let mut m = self.map.lock().await;
            let cell = m.entry(height).or_default().clone();
            while m.len() > BLOCK_CACHE {
                m.pop_first();
            }
            cell
        };
        cell.get_or_try_init(|| async {
            // From the master: the event can arrive before the replica has the block.
            let v = self.db.call("cd_get_block", vec![rmpv::Value::from(height)]).await?;
            let b: BlockWithTxs = serde_json::from_value(v).map_err(|e| anyhow!("block {height}: {e}"))?;
            Ok::<_, anyhow::Error>(Arc::new(block_to_pb(b)))
        })
        .await
        .cloned()
    }
}

// ------------------------------------------------------------------ client

/// Where an access token for the stream comes from (OAuth client credentials).
pub trait TokenSource: Send + Sync {
    /// A bearer token and its expiry (Unix seconds).
    fn token(&self) -> BoxFuture<'_, Result<(String, u64)>>;
}

type Pending = (SignedTx, oneshot::Sender<Receipt>);

/// Client of `LedgerService.SubmitTxs`: one long-lived bidirectional stream carrying
/// every transaction, with receipts matched back by `ref`. Reconnects on
/// failure, and rolls over to a fresh stream before its token expires.
pub struct LedgerSubmitter {
    queue: mpsc::Sender<Pending>,
}

impl LedgerSubmitter {
    pub fn spawn(endpoint: String, tokens: Arc<dyn TokenSource>) -> Self {
        let (queue, rx) = mpsc::channel(512);
        tokio::spawn(run_submitter(endpoint, tokens, rx));
        LedgerSubmitter { queue }
    }
}

impl Submitter for LedgerSubmitter {
    fn submit(&self, tx: SignedTx) -> BoxFuture<'_, Receipt> {
        Box::pin(async move {
            let (reply, wait) = oneshot::channel();
            if self.queue.send((tx, reply)).await.is_err() {
                return Receipt::Failed("submitter stopped".into());
            }
            match tokio::time::timeout(Duration::from_secs(15), wait).await {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => Receipt::Failed("stream closed before a receipt arrived".into()),
                Err(_) => Receipt::Failed("no receipt within 15 s".into()),
            }
        })
    }
}

async fn run_submitter(endpoint: String, tokens: Arc<dyn TokenSource>, mut queue: mpsc::Receiver<Pending>) {
    use pb::ledger_service_client::LedgerServiceClient;
    let mut next_ref = 0u64;
    let mut backoff = Duration::from_millis(500);
    loop {
        let (token, exp) = match tokens.token().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("SubmitTxs: no access token: {e:#}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
                continue;
            }
        };
        let channel = match tonic::transport::Endpoint::from_shared(endpoint.clone()).map(|e| e.connect_timeout(Duration::from_secs(5))) {
            Ok(e) => match e.connect().await {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!("SubmitTxs: cannot reach {endpoint}: {err}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                    continue;
                }
            },
            Err(e) => {
                tracing::error!("SubmitTxs: bad endpoint {endpoint}: {e}");
                return;
            }
        };
        let (sender, outgoing) = mpsc::channel::<pb::SubmitTxsRequest>(256);
        let mut req = Request::new(ReceiverStream::new(outgoing));
        req.metadata_mut().insert("authorization", format!("Bearer {token}").parse().expect("ascii token"));
        let mut inbound = match LedgerServiceClient::new(channel).submit_txs(req).await {
            Ok(r) => r.into_inner(),
            Err(s) => {
                tracing::warn!("SubmitTxs: stream refused: {s}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
                continue;
            }
        };
        backoff = Duration::from_millis(500);
        tracing::info!(%endpoint, "SubmitTxs stream open");

        // Roll over 30 s before the token expires: stop sending, drain receipts.
        let rollover = tokio::time::Instant::now() + Duration::from_secs(exp.saturating_sub(now_ms() / 1000).saturating_sub(30));
        let mut sender = Some(sender);
        let mut waiting: HashMap<u64, oneshot::Sender<Receipt>> = HashMap::new();
        loop {
            tokio::select! {
                item = queue.recv(), if sender.is_some() => {
                    let Some((tx, reply)) = item else { return }; // simulator gone
                    next_ref += 1;
                    let msg = pb::SubmitTxsRequest {
                        r#ref: next_ref,
                        hash: tx.hash.clone(),
                        body_json: String::from_utf8(canonical_body(&tx.body)).unwrap_or_default(),
                        signature: tx.signature,
                    };
                    waiting.insert(next_ref, reply);
                    if sender.as_ref().unwrap().send(msg).await.is_err() {
                        break;
                    }
                }
                _ = tokio::time::sleep_until(rollover), if sender.is_some() => {
                    sender = None; // half-close: the node finishes what it has, then ends the stream
                }
                r = inbound.message() => match r {
                    Ok(Some(rc)) => {
                        if let Some(w) = waiting.remove(&rc.r#ref) {
                            let _ = w.send(receipt_of(rc));
                        }
                    }
                    Ok(None) => break,
                    Err(s) => {
                        tracing::warn!("SubmitTxs stream ended: {s}");
                        break;
                    }
                }
            }
        }
        for (_, w) in waiting.drain() {
            let _ = w.send(Receipt::Failed("stream closed before a receipt arrived".into()));
        }
    }
}

fn receipt_of(r: pb::TxReceipt) -> Receipt {
    match Outcome::try_from(r.outcome).unwrap_or(Outcome::Unspecified) {
        Outcome::Admitted => Receipt::Admitted { deal_id: Some(r.deal_id).filter(|d| !d.is_empty()) },
        Outcome::Invalid | Outcome::Rejected | Outcome::Forbidden => Receipt::Refused(r.error),
        Outcome::Error | Outcome::Unspecified => Receipt::Failed(r.error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaindeal_core::{sign_action, verify_tx, Action, Block, BlockHeader, PartyKind, TxStatus};
    use pb::ledger_event::Event;

    fn record(name: &str, height: u64) -> TxRecord {
        let secret = hex::encode([7u8; 32]);
        let tx = sign_action(&secret, Action::Register { name: name.into(), kind: PartyKind::Consumer }, 1, 1_700_000_000_000).unwrap();
        TxRecord { hash: tx.hash, body: tx.body, signature: tx.signature, status: TxStatus::Confirmed, error: None, block_height: Some(height), deal_id: None }
    }

    #[tokio::test]
    async fn log_events_map_to_protobuf() {
        let ev = to_event(9, r#"{"type":"pending","hash":"ab","action":"register"}"#, None).await.unwrap();
        assert_eq!(ev.seq, 9);
        assert!(matches!(ev.event, Some(Event::Pending(p)) if p.hash == "ab" && p.action == "register"));

        let block = r#"{"type":"block","height":42,"hash":"h","tx_count":3,"rejected":1,
            "transitions":[{"id":"D-1","from":null,"to":"proposed","deal_type":"C2C","amount":500,"title":"Bike"},
                           {"id":"D-2","from":"funded","to":"delivered","deal_type":"B2B","amount":7,"title":"Racks"}],
            "accounts":["0xa","0xb"]}"#;
        let Some(Event::Block(b)) = to_event(10, block, None).await.unwrap().event else { panic!("not a block") };
        assert_eq!((b.height, b.tx_count, b.rejected), (42, 3, 1));
        assert_eq!(b.transitions[0].from, "", "a new deal has no previous status");
        assert_eq!((b.transitions[1].from.as_str(), b.transitions[1].to.as_str()), ("funded", "delivered"));
        assert_eq!(b.accounts, ["0xa", "0xb"]);
        assert!(b.transactions.is_empty(), "transactions only when requested");

        assert!(to_event(11, r#"{"type":"something-new"}"#, None).await.is_none());
        assert!(to_event(12, "not json", None).await.is_none());
    }

    #[test]
    fn streamed_transactions_are_independently_verifiable_and_in_block_order() {
        let (a, b) = (record("Ann", 5), record("Bob", 5));
        let header = BlockHeader { height: 5, prev_hash: "p".into(), timestamp: 1, merkle_root: "m".into(), tx_count: 2, difficulty: 1, nonce: 0, producer: "n".into() };
        let block = Block { header, hash: "h".into(), tx_hashes: vec![b.hash.clone(), a.hash.clone()] };
        // The database returns transactions in index order; the stream uses Merkle order.
        let pb = block_to_pb(BlockWithTxs { block, txs: vec![a.clone(), b.clone()] });
        assert_eq!(pb.transactions.iter().map(|t| t.hash.as_str()).collect::<Vec<_>>(), [b.hash.as_str(), a.hash.as_str()]);
        for t in &pb.transactions {
            // body_json is exactly what was signed: a client can rebuild and verify the tx.
            let body: TxBody = serde_json::from_str(&t.body_json).unwrap();
            verify_tx(&SignedTx { hash: t.hash.clone(), body, signature: t.signature.clone() }).unwrap();
            assert_eq!((t.status.as_str(), t.block_height, t.action.as_str()), ("confirmed", 5, "register"));
        }
    }
}
