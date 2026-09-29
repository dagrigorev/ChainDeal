//! Node logic: transaction admission, block production, metrics and chain verification.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use chaindeal_core::contract::treasury_account;
use chaindeal_core::*;
use serde::Serialize;
use serde_json::json;
use tokio::sync::{broadcast, Mutex};

use crate::db::Db;

const MAX_TXS_PER_BLOCK: u32 = 1000;
const MINING_THREADS: usize = 4;
const METRIC_WINDOW_SECS: u64 = 120;
/// Leader lease: renewed every LEASE_RENEW, lost after LEASE_TTL without renewal.
const LEASE_NAME: &str = "producer";
const LEASE_TTL: f64 = 6.0;
const LEASE_RENEW: Duration = Duration::from_secs(2);
/// How often each node publishes its outbox and tails the shared event log.
const BUS_TICK: Duration = Duration::from_millis(200);
/// Event log and metrics retention in Tarantool.
const RETENTION_SECS: u64 = 900;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// Proof-of-work across several threads; each strides through the nonce space.
pub fn mine_parallel(header: BlockHeader, threads: usize) -> (BlockHeader, String) {
    let found = AtomicBool::new(false);
    let result = std::sync::Mutex::new(None);
    std::thread::scope(|s| {
        for t in 0..threads {
            let mut h = header.clone();
            h.nonce = t as u64;
            let (found, result) = (&found, &result);
            s.spawn(move || {
                while !found.load(Ordering::Relaxed) {
                    let hash = block_hash(&h);
                    if meets_difficulty(&hash, h.difficulty) {
                        if !found.swap(true, Ordering::SeqCst) {
                            *result.lock().unwrap() = Some((h.clone(), hash));
                        }
                        return;
                    }
                    h.nonce += threads as u64;
                }
            });
        }
    });
    result.into_inner().unwrap().expect("a miner always finds a nonce")
}

#[derive(Clone, Copy)]
pub enum Metric {
    Admitted = 0,
    Refused = 1,
    Confirmed = 2,
    Rejected = 3,
}

/// A stateless API node. Any number can run against one Tarantool master:
/// they elect a leader through a lease stored in Tarantool, and exchange
/// live events and metrics through Tarantool too, so every node's SSE
/// clients see the same stream no matter which node produced the block.
pub struct Node {
    pub db: Db,
    /// Local fan-out to this node's SSE subscribers (fed by the shared log).
    /// `(seq, json)`: the event and its sequence number in the shared log.
    pub events: broadcast::Sender<(u64, String)>,
    pub node_id: String,
    pub difficulty: u32,
    /// Serialises block production within this process; across processes the
    /// lease and the tip check in `cd_commit_block` do the same.
    produce_lock: Mutex<()>,
    leader: AtomicBool,
    /// Never takes the leader lease (CHAINDEAL_ROLE=follower), e.g. a node
    /// under a debugger, whose pauses must not stall block production.
    follower_only: bool,
    started_at: u64,
    outbox: std::sync::Mutex<Vec<String>>,
    pending_metrics: std::sync::Mutex<HashMap<u64, [u32; 4]>>,
}

#[derive(Debug)]
pub enum SubmitError {
    Invalid(String),
    Rejected(String),
    Internal(anyhow::Error),
}

#[derive(Serialize)]
pub struct VerifyReport {
    pub valid: bool,
    pub blocks_checked: u64,
    pub txs_checked: u64,
    pub errors: Vec<String>,
    pub elapsed_ms: u64,
    /// Continue from here (range verification); `done` once past the tip.
    pub next_from: u64,
    pub last_hash: Option<String>,
    pub tip_height: u64,
    pub done: bool,
}

fn to_signed(r: &TxRecord) -> SignedTx {
    SignedTx { hash: r.hash.clone(), body: r.body.clone(), signature: r.signature.clone() }
}

impl Node {
    pub fn new(db: Db, node_id: String, difficulty: u32) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            db,
            events,
            node_id,
            difficulty,
            produce_lock: Mutex::new(()),
            leader: AtomicBool::new(false),
            follower_only: false,
            started_at: now_ms() / 1000,
            outbox: Default::default(),
            pending_metrics: Default::default(),
        }
    }

    pub fn follower_only(mut self, yes: bool) -> Self {
        self.follower_only = yes;
        self
    }

    pub fn is_leader(&self) -> bool {
        self.leader.load(Ordering::Relaxed)
    }

    /// Queue an event for the shared log; the bus task publishes it.
    fn emit(&self, v: serde_json::Value) {
        let mut o = self.outbox.lock().unwrap();
        o.push(v.to_string());
        // Never let a stalled database grow the outbox without bound.
        if o.len() > 5000 {
            o.drain(..1000);
        }
    }

    pub fn record(&self, m: Metric, n: u32) {
        if n == 0 {
            return;
        }
        let sec = now_ms() / 1000;
        self.pending_metrics.lock().unwrap().entry(sec).or_insert([0; 4])[m as usize] += n;
    }

    /// Dense per-second series (oldest first) for the last two minutes,
    /// aggregated across every node in the cluster.
    pub async fn metrics(&self) -> Result<serde_json::Value> {
        let now = now_ms() / 1000;
        let from = now - METRIC_WINDOW_SECS + 1;
        let map: HashMap<u64, [u32; 4]> = self.db.reader().metrics(from).await?.into_iter().collect();
        let series: Vec<[u32; 4]> = (from..=now).map(|s| map.get(&s).copied().unwrap_or([0; 4])).collect();
        let mut totals = [0u64; 4];
        for c in &series {
            for i in 0..4 {
                totals[i] += c[i] as u64;
            }
        }
        Ok(json!({
            "now": now,
            "fields": ["admitted", "refused", "confirmed", "rejected"],
            "series": series,
            "totals": { "admitted": totals[0], "refused": totals[1], "confirmed": totals[2], "rejected": totals[3] },
        }))
    }

    /// Leader election + heartbeat + housekeeping.
    pub async fn run_leadership(self: Arc<Self>) {
        let mut tick = tokio::time::interval(LEASE_RENEW);
        let mut n = 0u64;
        loop {
            tick.tick().await;
            n += 1;
            let won = !self.follower_only && self.db.lease(LEASE_NAME, &self.node_id, LEASE_TTL).await.unwrap_or(false);
            if won != self.leader.swap(won, Ordering::SeqCst) {
                tracing::info!(node = %self.node_id, "{}", if won { "became leader: producing blocks and running the simulator" } else { "lost leadership; serving as follower" });
                self.emit(json!({ "type": "leader", "node": self.node_id, "leader": won }));
            }
            let info = json!({
                "role": if won { "leader" } else { "follower" },
                "follower_only": self.follower_only,
                "started_at": self.started_at,
                "version": env!("CARGO_PKG_VERSION"),
                "reader": self.db.has_reader(),
            });
            let _ = self.db.heartbeat(&self.node_id, &info.to_string()).await;
            if won && n.is_multiple_of(30) {
                let _ = self.db.prune(RETENTION_SECS).await;
            }
        }
    }

    /// Hands the lease back on graceful shutdown so another node takes over
    /// immediately instead of waiting for the lease to expire.
    pub async fn release_leadership(&self) {
        if self.leader.swap(false, Ordering::SeqCst) {
            // A negative TTL writes an already-expired lease (only if we hold it).
            match self.db.lease(LEASE_NAME, &self.node_id, -1.0).await {
                Ok(_) => tracing::info!(node = %self.node_id, "released leadership"),
                Err(e) => tracing::warn!("could not release leadership: {e:#}"),
            }
        }
    }

    /// Publishes this node's outbox and metrics, and tails the shared log
    /// into local SSE subscribers.
    pub async fn run_bus(self: Arc<Self>) {
        let mut last = self.db.events_head().await.unwrap_or(0);
        let mut tick = tokio::time::interval(BUS_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut n = 0u64;
        loop {
            tick.tick().await;
            n += 1;
            let batch: Vec<String> = std::mem::take(&mut *self.outbox.lock().unwrap());
            if !batch.is_empty() {
                if let Err(e) = self.db.publish(batch).await {
                    tracing::warn!("event publish failed: {e:#}");
                }
            }
            if n.is_multiple_of(5) {
                let rows: Vec<(u64, [u32; 4])> = self.pending_metrics.lock().unwrap().drain().collect();
                if !rows.is_empty() {
                    if let Err(e) = self.db.metric_add(&rows).await {
                        tracing::warn!("metrics flush failed: {e:#}");
                    }
                }
            }
            match self.db.events_since(last, 1000).await {
                Ok(events) => {
                    for (seq, data) in events {
                        last = seq;
                        let _ = self.events.send((seq, data));
                    }
                }
                Err(e) => tracing::warn!("event tail failed: {e:#}"),
            }
        }
    }

    /// Loads every account and deal the given transactions can touch.
    async fn load_state(&self, txs: &[SignedTx]) -> Result<WorkingState> {
        let deal_ids: BTreeSet<String> =
            txs.iter().filter_map(|t| t.body.action.deal_id().map(str::to_string)).collect();
        let deal_ids: Vec<String> = deal_ids.into_iter().collect();
        let deals = self.db.get_deals(&deal_ids).await?;

        let mut addrs: BTreeSet<String> = BTreeSet::new();
        addrs.insert(TREASURY_ADDRESS.to_string());
        for t in txs {
            addrs.insert(t.body.from.clone());
            addrs.extend(t.body.action.referenced_addresses().into_iter().map(str::to_string));
        }
        for d in &deals {
            addrs.insert(d.seller.clone());
            addrs.insert(d.buyer.clone());
            addrs.extend(d.arbiter.clone());
        }
        let addrs: Vec<String> = addrs.into_iter().collect();
        let accounts = self.db.get_accounts(&addrs).await?;

        Ok(WorkingState {
            accounts: accounts.into_iter().map(|a| (a.address.clone(), a)).collect(),
            deals: deals.into_iter().map(|d| (d.id.clone(), d)).collect(),
            ..Default::default()
        })
    }

    /// Creates the genesis block if the chain is empty.
    pub async fn ensure_genesis(&self) -> Result<()> {
        let _g = self.produce_lock.lock().await;
        if self.db.tip().await?.is_some() {
            return Ok(());
        }
        let header = BlockHeader {
            height: 0,
            prev_hash: "0".repeat(64),
            timestamp: now_ms() / 1000,
            merkle_root: merkle_root(&[]),
            tx_count: 0,
            difficulty: self.difficulty,
            nonce: 0,
            producer: self.node_id.clone(),
        };
        let (header, hash) = tokio::task::spawn_blocking(move || mine_parallel(header, MINING_THREADS)).await?;
        let block = Block { header, hash, tx_hashes: vec![] };
        let treasury = treasury_account();
        self.db.commit_block(Some(&block), &[], &[&treasury], &[]).await?;
        tracing::info!(hash = %block.hash, "genesis block created");
        Ok(())
    }

    /// Validates and queues a signed transaction. The transaction is dry-run
    /// on top of the current mempool so obviously-failing actions are refused
    /// immediately; the block producer re-validates everything.
    pub async fn submit(&self, tx: SignedTx) -> Result<TxRecord, SubmitError> {
        let res = self.admit(tx).await;
        match &res {
            Ok(r) => {
                self.record(Metric::Admitted, 1);
                self.emit(json!({ "type": "pending", "hash": r.hash, "action": r.body.action.name() }));
            }
            Err(SubmitError::Invalid(e) | SubmitError::Rejected(e)) => {
                self.record(Metric::Refused, 1);
                self.emit(json!({ "type": "refused", "error": e }));
            }
            Err(SubmitError::Internal(_)) => {}
        }
        res
    }

    async fn admit(&self, tx: SignedTx) -> Result<TxRecord, SubmitError> {
        verify_tx_at(&tx, now_ms()).map_err(|e| SubmitError::Invalid(e.to_string()))?;

        let pending = self.db.mempool_take(MAX_TXS_PER_BLOCK).await.map_err(SubmitError::Internal)?;
        let mut batch: Vec<SignedTx> = pending.iter().map(to_signed).collect();
        batch.push(tx.clone());
        let mut st = self.load_state(&batch).await.map_err(SubmitError::Internal)?;
        let now = now_ms() / 1000;
        for p in &batch[..batch.len() - 1] {
            let _ = apply_tx(&mut st, p, now);
        }
        let effect = apply_tx(&mut st, &tx, now).map_err(|e| SubmitError::Rejected(e.0))?;

        let record = TxRecord {
            hash: tx.hash,
            body: tx.body,
            signature: tx.signature,
            status: TxStatus::Pending,
            error: None,
            block_height: None,
            deal_id: effect.deal_id,
        };
        self.db.mempool_add(&record).await.map_err(|e| {
            let msg = e.to_string();
            if msg.contains("duplicate") { SubmitError::Invalid(msg) } else { SubmitError::Internal(e) }
        })?;
        Ok(record)
    }

    /// Drains the mempool into a new block. Returns the block if one was made.
    pub async fn produce_block(&self) -> Result<Option<Block>> {
        let _g = self.produce_lock.lock().await;
        let pending = self.db.mempool_take(MAX_TXS_PER_BLOCK).await?;
        if pending.is_empty() {
            return Ok(None);
        }
        let tip = self.db.tip().await?.ok_or_else(|| anyhow!("chain has no genesis block"))?;
        let now = (now_ms() / 1000).max(tip.header.timestamp);
        let height = tip.header.height + 1;

        let signed: Vec<SignedTx> = pending.iter().map(to_signed).collect();
        let mut st = self.load_state(&signed).await?;
        let before: HashMap<String, DealStatus> = st.deals.iter().map(|(k, d)| (k.clone(), d.status)).collect();
        let mut confirmed = Vec::new();
        let mut rejected = Vec::new();
        for (rec, tx) in pending.into_iter().zip(signed.iter()) {
            let outcome = verify_tx(tx)
                .map_err(|e| e.to_string())
                .and_then(|_| apply_tx(&mut st, tx, now).map_err(|e| e.0));
            match outcome {
                Ok(effect) => confirmed.push(TxRecord {
                    status: TxStatus::Confirmed,
                    block_height: Some(height),
                    deal_id: effect.deal_id,
                    ..rec
                }),
                Err(e) => rejected.push(TxRecord { status: TxStatus::Rejected, error: Some(e), block_height: None, ..rec }),
            }
        }

        let block = if confirmed.is_empty() {
            None
        } else {
            let tx_hashes: Vec<String> = confirmed.iter().map(|t| t.hash.clone()).collect();
            let header = BlockHeader {
                height,
                prev_hash: tip.hash.clone(),
                timestamp: now,
                merkle_root: merkle_root(&tx_hashes),
                tx_count: tx_hashes.len() as u32,
                difficulty: self.difficulty,
                nonce: 0,
                producer: self.node_id.clone(),
            };
            let (header, hash) = tokio::task::spawn_blocking(move || mine_parallel(header, MINING_THREADS)).await?;
            Some(Block { header, hash, tx_hashes })
        };

        let accounts: Vec<&Account> = st.touched_accounts.iter().filter_map(|a| st.accounts.get(a)).collect();
        let deals: Vec<&Deal> = st.touched_deals.iter().filter_map(|d| st.deals.get(d)).collect();
        let all_txs: Vec<TxRecord> = confirmed.iter().chain(rejected.iter()).cloned().collect();
        self.db.commit_block(block.as_ref(), &all_txs, &accounts, &deals).await?;
        self.record(Metric::Confirmed, confirmed.len() as u32);
        self.record(Metric::Rejected, rejected.len() as u32);

        for r in &rejected {
            tracing::debug!(hash = %r.hash, error = ?r.error, "transaction rejected");
            self.emit(json!({ "type": "rejected", "hash": r.hash, "from": r.body.from, "error": r.error }));
        }
        if let Some(b) = &block {
            // Every deal whose status changed in this block, for live visualisation.
            let transitions: Vec<serde_json::Value> = deals
                .iter()
                .filter(|d| before.get(&d.id) != Some(&d.status))
                .map(|d| json!({
                    "id": d.id, "from": before.get(&d.id), "to": d.status,
                    "deal_type": d.deal_type, "amount": d.amount, "title": d.title,
                }))
                .collect();
            tracing::info!(height = b.header.height, txs = b.header.tx_count, rejected = rejected.len(), "block produced");
            self.emit(json!({
                "type": "block",
                "height": b.header.height,
                "hash": b.hash,
                "tx_count": b.header.tx_count,
                "rejected": rejected.len(),
                "transitions": transitions,
                "accounts": st.touched_accounts,
            }));
        }
        Ok(block)
    }

    /// Produces blocks while this node holds the leader lease.
    pub async fn run_producer(&self, interval: Duration) {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut genesis_checked = false;
        loop {
            tick.tick().await;
            if !self.is_leader() {
                continue;
            }
            if !genesis_checked {
                match self.ensure_genesis().await {
                    Ok(()) => genesis_checked = true,
                    Err(e) => tracing::error!("genesis failed: {e:#}"),
                }
            }
            if let Err(e) = self.produce_block().await {
                tracing::error!("block production failed: {e:#}");
            }
        }
    }

    /// Re-derives hashes, PoW, links, Merkle roots and signatures for
    /// `count` blocks starting at `from`. `prev` is the hash of block
    /// `from - 1` from the previous range, so links are checked across calls.
    pub async fn verify_range(&self, from: u64, count: u32, prev: Option<String>) -> Result<VerifyReport> {
        let started = std::time::Instant::now();
        let db = self.db.reader();
        let tip_height = db.tip().await?.map(|b| b.header.height).unwrap_or(0);
        let mut errors = Vec::new();
        let (mut blocks, mut txs) = (0u64, 0u64);
        let mut prev = prev;
        let mut next = from;
        let batch = db.blocks_range(from, count).await?;
        for (b, records) in batch {
            let h = b.header.height;
            if h != next {
                errors.push(format!("block {next} is missing"));
            }
            next = h + 1;
            blocks += 1;
            if block_hash(&b.header) != b.hash {
                errors.push(format!("block {h}: stored hash does not match header"));
            }
            if !meets_difficulty(&b.hash, b.header.difficulty) {
                errors.push(format!("block {h}: insufficient proof-of-work"));
            }
            match &prev {
                Some(p) if *p != b.header.prev_hash => errors.push(format!("block {h}: broken link to previous block")),
                None if h != 0 => {} // first block of a range without a known predecessor
                _ => {}
            }
            prev = Some(b.hash.clone());
            if merkle_root(&b.tx_hashes) != b.header.merkle_root {
                errors.push(format!("block {h}: merkle root mismatch"));
            }
            let stored: BTreeSet<&str> = records.iter().map(|r| r.hash.as_str()).collect();
            for th in &b.tx_hashes {
                if !stored.contains(th.as_str()) {
                    errors.push(format!("block {h}: transaction {th} missing from storage"));
                }
            }
            for r in &records {
                txs += 1;
                if let Err(e) = verify_tx(&to_signed(r)) {
                    errors.push(format!("block {h}: tx {}: {e}", &r.hash[..12.min(r.hash.len())]));
                }
                if !b.tx_hashes.contains(&r.hash) {
                    errors.push(format!("block {h}: tx {} not committed by the block", r.hash));
                }
            }
            if errors.len() > 50 {
                break;
            }
        }
        Ok(VerifyReport {
            valid: errors.is_empty(),
            blocks_checked: blocks,
            txs_checked: txs,
            errors,
            elapsed_ms: started.elapsed().as_millis() as u64,
            next_from: next,
            last_hash: prev,
            tip_height,
            done: next > tip_height,
        })
    }
}
