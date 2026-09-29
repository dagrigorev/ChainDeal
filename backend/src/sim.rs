//! Live market simulator: a population of agent wallets that open, advance,
//! decline, dispute and abandon deals at a target rate (default 10 tx/s).
//!
//! It runs as its own service (`chaindeal-sim`). Every action is a real signed
//! transaction streamed to the chain nodes over gRPC (`LedgerService.SubmitTxs`), so
//! the simulator exercises exactly the path other clients use, including
//! refusals. It reads deal state from the chain's read replica, "adopts" open
//! deals left by the bulk loader or a previous run, and acts as keeper for
//! lapsed deadlines.
//!
//! One instance is active at a time (a lease in Tarantool). Its config and a
//! live snapshot live in Tarantool too, so any API node can serve and change
//! them through `SimControl`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use chaindeal_core::*;
use futures::future::BoxFuture;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::chain::now_ms;
use crate::db::Db;
use crate::synth::{self, Agent, Plan, Roster, Step};

const TICK: Duration = Duration::from_millis(100);
const MAX_ACTIVE: usize = 6000;
/// After acting, wait this long for the block before assuming the tx failed.
const CONFIRM_GRACE_SECS: u64 = 12;
const KV_CONFIG: &str = "sim_config";
const KV_SNAPSHOT: &str = "sim_snapshot";
/// Only one simulator instance acts at a time (e.g. during a rolling update).
pub const SIM_LEASE: &str = "simulator";
const SIM_LEASE_TTL: f64 = 6.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimConfig {
    pub running: bool,
    /// Target transactions per second.
    pub rate: f64,
    /// Multiplier on failure scenarios (0 = only happy paths, 1 = baseline, 3 = chaos).
    pub pressure: f64,
    /// Fraction of actions deliberately sent by the wrong party (buggy clients).
    pub noise: f64,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig { running: true, rate: 10.0, pressure: 1.0, noise: 0.02 }
    }
}

#[derive(Debug, Deserialize)]
pub struct SimPatch {
    pub running: Option<bool>,
    pub rate: Option<f64>,
    pub pressure: Option<f64>,
    pub noise: Option<f64>,
}

/// What happened to a submitted transaction at admission.
#[derive(Debug)]
pub enum Receipt {
    Admitted { deal_id: Option<String> },
    /// Refused by the node (invalid, or the contract says no).
    Refused(String),
    /// Transport or node failure; nothing is known about the transaction.
    Failed(String),
}

/// Where the simulator sends its transactions (the gRPC stream in production).
pub trait Submitter: Send + Sync {
    fn submit(&self, tx: SignedTx) -> BoxFuture<'_, Receipt>;
}

/// The shared config and snapshot in Tarantool: read and changed by API nodes
/// (`/api/sim`), followed by the simulator service.
pub struct SimControl {
    db: Db,
    config: std::sync::Mutex<SimConfig>,
}

impl SimControl {
    pub fn new(db: Db, defaults: SimConfig) -> Self {
        SimControl { db, config: std::sync::Mutex::new(defaults) }
    }

    pub fn config(&self) -> SimConfig {
        self.config.lock().unwrap().clone()
    }

    /// Applies a patch to the shared (cluster-wide) config.
    pub async fn patch(&self, p: SimPatch) -> anyhow::Result<SimConfig> {
        self.refresh().await;
        let c = {
            let mut c = self.config.lock().unwrap();
            if let Some(v) = p.running { c.running = v; }
            if let Some(v) = p.rate { c.rate = v.clamp(0.5, 200.0); }
            if let Some(v) = p.pressure { c.pressure = v.clamp(0.0, 5.0); }
            if let Some(v) = p.noise { c.noise = v.clamp(0.0, 0.5); }
            c.clone()
        };
        self.db.kv_set(KV_CONFIG, &serde_json::to_string(&c)?).await?;
        Ok(c)
    }

    /// Pulls the shared config; seeds it from the defaults if absent.
    pub async fn refresh(&self) {
        match self.db.kv_get(KV_CONFIG).await {
            Ok(Some(raw)) => {
                if let Ok(c) = serde_json::from_str::<SimConfig>(&raw) {
                    *self.config.lock().unwrap() = c;
                }
            }
            Ok(None) => {
                let c = self.config();
                let _ = self.db.kv_set(KV_CONFIG, &serde_json::to_string(&c).unwrap_or_default()).await;
            }
            Err(e) => tracing::warn!("sim config read failed: {e:#}"),
        }
    }

    /// Cluster-wide view: the simulator's latest published snapshot, the
    /// shared config, and which simulator instance is active.
    pub async fn shared_snapshot(&self) -> serde_json::Value {
        self.refresh().await;
        let mut v = match self.db.kv_get(KV_SNAPSHOT).await {
            Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({})),
            _ => serde_json::json!({}),
        };
        v["config"] = serde_json::json!(self.config());
        v["runner"] = serde_json::json!(self.db.lease_holder(SIM_LEASE).await.ok().flatten());
        v["leader"] = serde_json::json!(self.db.lease_holder("producer").await.ok().flatten());
        v
    }
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct SimStats {
    pub sent: u64,
    pub admitted: u64,
    pub refused: u64,
    pub noise_sent: u64,
    pub deals_opened: u64,
    pub adopted: u64,
    pub registered: u32,
    pub by_action: BTreeMap<String, u64>,
    pub scenarios: BTreeMap<String, u64>,
    pub outcomes: BTreeMap<String, u64>,
    pub recent_refusals: VecDeque<String>,
}

struct Tracked {
    plan: Plan,
    next_at: u64,
    last_status: Option<DealStatus>,
}

struct State {
    bootstrapped: bool,
    rng: StdRng,
    tokens: f64,
    deals: HashMap<String, Tracked>,
    unregistered: VecDeque<u32>,
    registered: Vec<bool>,
    stats: SimStats,
}

pub struct Sim {
    /// This instance's id, for the lease.
    id: String,
    /// Master connection (lease, config, snapshot) with the read pool for chain state.
    db: Db,
    submitter: Arc<dyn Submitter>,
    control: SimControl,
    roster: Roster,
    agents: Vec<Agent>,
    by_addr: HashMap<String, u32>,
    state: tokio::sync::Mutex<State>,
}

impl Sim {
    pub fn new(id: String, db: Db, submitter: Arc<dyn Submitter>, total_agents: u32, defaults: SimConfig) -> Arc<Self> {
        let roster = Roster::from_total(total_agents);
        let agents: Vec<Agent> = (0..roster.total()).map(synth::agent).collect();
        let by_addr = agents.iter().map(|a| (a.address.clone(), a.index)).collect();
        Arc::new(Sim {
            id,
            control: SimControl::new(db.clone(), defaults),
            db,
            submitter,
            roster,
            by_addr,
            state: tokio::sync::Mutex::new(State {
                bootstrapped: false,
                rng: StdRng::from_entropy(),
                tokens: 0.0,
                deals: HashMap::new(),
                unregistered: VecDeque::new(),
                registered: vec![false; agents.len()],
                stats: SimStats::default(),
            }),
            agents,
        })
    }

    pub async fn snapshot(&self) -> serde_json::Value {
        let st = self.state.lock().await;
        let mut active: BTreeMap<String, u64> = BTreeMap::new();
        for t in st.deals.values() {
            let k = t.last_status.map(|s| format!("{s:?}").to_lowercase()).unwrap_or_else(|| "opening".into());
            *active.entry(k).or_default() += 1;
        }
        serde_json::json!({
            "config": self.control.config(),
            "agents": self.roster.total(),
            "active": st.deals.len(),
            "active_by_status": active,
            "stats": st.stats,
        })
    }

    pub async fn run(self: Arc<Self>) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut n = 0u64;
        let mut active = false;
        loop {
            tick.tick().await;
            n += 1;
            if n % 10 == 1 {
                self.control.refresh().await;
            }
            if n % 20 == 1 {
                let won = self.db.lease(SIM_LEASE, &self.id, SIM_LEASE_TTL).await.unwrap_or(false);
                if won != active {
                    tracing::info!(id = %self.id, "{}", if won { "simulator active" } else { "simulator standing by (another instance holds the lease)" });
                    active = won;
                }
            }
            if !active {
                // Forget local state so a future term starts from the chain, not stale memory.
                let mut st = self.state.lock().await;
                if st.bootstrapped {
                    st.bootstrapped = false;
                    st.deals.clear();
                }
                continue;
            }
            {
                let mut st = self.state.lock().await;
                if !st.bootstrapped {
                    drop(st);
                    if let Err(e) = self.bootstrap().await {
                        tracing::error!("simulator bootstrap failed: {e:#}");
                        continue;
                    }
                    st = self.state.lock().await;
                    st.bootstrapped = true;
                }
            }
            if n.is_multiple_of(10) {
                let snap = self.snapshot().await;
                let _ = self.db.kv_set(KV_SNAPSHOT, &snap.to_string()).await;
            }
            let cfg = self.control.config();
            if !cfg.running {
                continue;
            }
            let mut st = self.state.lock().await;
            st.tokens = (st.tokens + cfg.rate * TICK.as_secs_f64()).min(cfg.rate.max(1.0));
            if let Err(e) = self.step(&mut st, &cfg).await {
                tracing::warn!("simulator step failed: {e:#}");
            }
        }
    }

    /// Hands the lease back on shutdown so a replacement starts at once.
    pub async fn release(&self) {
        let _ = self.db.lease(SIM_LEASE, &self.id, -1.0).await;
    }

    /// Finds which agents exist on chain and adopts open deals between agents.
    async fn bootstrap(&self) -> anyhow::Result<()> {
        let mut st = self.state.lock().await;
        st.deals.clear();
        st.registered = vec![false; self.agents.len()];
        st.stats.adopted = 0;
        let addrs: Vec<String> = self.agents.iter().map(|a| a.address.clone()).collect();
        for chunk in addrs.chunks(1000) {
            for a in self.db.reader().get_accounts(chunk).await? {
                if let Some(&i) = self.by_addr.get(&a.address) {
                    st.registered[i as usize] = true;
                }
            }
        }
        let mut missing: Vec<u32> = (0..self.roster.total()).filter(|&i| !st.registered[i as usize]).collect();
        // Arbiters first, the rest in random order so early deals have variety.
        let (mut arb, mut rest): (Vec<u32>, Vec<u32>) = missing.drain(..).partition(|&i| i < self.roster.arbiters);
        rest.shuffle(&mut st.rng);
        arb.extend(rest);
        st.unregistered = arb.into();
        st.stats.registered = st.registered.iter().filter(|r| **r).count() as u32;

        let now = now_ms() / 1000;
        let open = self.db.reader().list_deals(MAX_ACTIVE as u32, 0, None, Some("open")).await?;
        for d in open {
            if [&d.seller, &d.buyer].iter().all(|a| self.by_addr.contains_key(*a)) {
                let plan = Plan::adopt(&mut st.rng, &d);
                let next_at = now + st.rng.gen_range(0..20);
                st.deals.insert(d.id.clone(), Tracked { plan, next_at, last_status: Some(d.status) });
                st.stats.adopted += 1;
            }
        }
        tracing::info!(
            registered = st.stats.registered,
            missing = st.unregistered.len(),
            adopted = st.stats.adopted,
            "simulator ready"
        );
        Ok(())
    }

    async fn send(&self, st: &mut State, agent: u32, action: Action) -> Result<Option<String>, String> {
        let name = action.name().to_string();
        let tx = sign_action(&self.agents[agent as usize].secret, action, st.rng.gen::<u64>() >> 11, now_ms())
            .map_err(|e| e.to_string())?;
        st.stats.sent += 1;
        *st.stats.by_action.entry(name.clone()).or_default() += 1;
        match self.submitter.submit(tx).await {
            Receipt::Admitted { deal_id } => {
                st.stats.admitted += 1;
                Ok(deal_id)
            }
            Receipt::Refused(e) => {
                st.stats.refused += 1;
                st.stats.recent_refusals.push_front(format!("{name}: {e}"));
                st.stats.recent_refusals.truncate(12);
                Err(e)
            }
            Receipt::Failed(e) => Err(e),
        }
    }

    async fn step(&self, st: &mut State, cfg: &SimConfig) -> anyhow::Result<()> {
        let now = now_ms() / 1000;

        // 1. Onboard agents (bounded, so deals keep flowing during bootstrap).
        let mut regs = 0;
        while st.tokens >= 1.0 && regs < 5 {
            let Some(i) = st.unregistered.pop_front() else { break };
            let action = Action::Register { name: synth::agent_name(&self.roster, i), kind: self.roster.kind(i) };
            st.tokens -= 1.0;
            regs += 1;
            if self.send(st, i, action).await.is_ok() {
                st.registered[i as usize] = true;
                st.stats.registered += 1;
            }
        }

        // 2. Advance deals that are due, reading their real on-chain state.
        let mut due: Vec<(u64, String)> =
            st.deals.iter().filter(|(_, t)| t.next_at <= now).map(|(k, t)| (t.next_at, k.clone())).collect();
        due.sort();
        due.truncate((st.tokens.floor() as usize).clamp(0, 200));
        if !due.is_empty() {
            let ids: Vec<String> = due.into_iter().map(|(_, k)| k).collect();
            let fetched: HashMap<String, Deal> =
                self.db.reader().get_deals(&ids).await?.into_iter().map(|d| (d.id.clone(), d)).collect();
            for id in ids {
                if st.tokens < 1.0 {
                    break;
                }
                let Some(d) = fetched.get(&id) else {
                    // Not sealed yet (just created) — look again shortly.
                    if let Some(t) = st.deals.get_mut(&id) { t.next_at = now + 1; }
                    continue;
                };
                self.advance(st, cfg, d, now).await;
            }
        }

        // 3. Spend what's left on new deals, payroll and the occasional bad client.
        while st.tokens >= 1.0 {
            st.tokens -= 1.0;
            let registered = st.stats.registered as f64 / self.roster.total() as f64;
            if registered < 0.1 {
                break;
            }
            let roll: f64 = st.rng.gen();
            if roll < 0.07 {
                let (from, to, amount) = synth::payroll(&mut st.rng, &self.roster);
                if !st.registered[from as usize] || !st.registered[to as usize] {
                    continue;
                }
                let bal = self.db.reader().get_accounts(&[self.agents[from as usize].address.clone()]).await?;
                if bal.first().is_some_and(|a| a.balance > amount + 5_000_00) {
                    let to = self.agents[to as usize].address.clone();
                    let _ = self.send(st, from, Action::Transfer { to, amount, memo: "payroll".into() }).await;
                }
            } else if st.deals.len() < MAX_ACTIVE {
                self.open_deal(st, cfg, now).await;
            }
        }
        Ok(())
    }

    async fn open_deal(&self, st: &mut State, cfg: &SimConfig, now: u64) {
        for _ in 0..20 {
            let nd = synth::new_deal(&mut st.rng, &self.roster, cfg.pressure);
            let Action::CreateDeal { counterparty, arbiter, .. } = &nd.action else { unreachable!() };
            let ok = st.registered[nd.proposer as usize]
                && self.by_addr.get(counterparty).is_some_and(|&i| st.registered[i as usize])
                && arbiter.as_ref().is_none_or(|a| self.by_addr.get(a).is_some_and(|&i| st.registered[i as usize]));
            if !ok {
                continue;
            }
            let scenario = format!("{:?}", nd.plan.scenario);
            if let Ok(Some(id)) = self.send(st, nd.proposer, nd.action).await {
                st.stats.deals_opened += 1;
                *st.stats.scenarios.entry(scenario).or_default() += 1;
                let next_at = now + 3 + synth::think(&mut st.rng, 8.0);
                st.deals.insert(id, Tracked { plan: nd.plan, next_at, last_status: None });
            }
            return;
        }
    }

    async fn advance(&self, st: &mut State, cfg: &SimConfig, d: &Deal, now: u64) {
        let Some(mut t) = st.deals.remove(&d.id) else { return };
        t.last_status = Some(d.status);
        if d.status.is_terminal() {
            *st.stats.outcomes.entry(format!("{:?}", d.status).to_lowercase()).or_default() += 1;
            return;
        }
        // Our last action hasn't landed yet: give the block time before retrying.
        if t.plan.sent_status == Some(d.status) {
            if now < t.plan.sent_at + CONFIRM_GRACE_SECS {
                t.next_at = now + 1;
                st.deals.insert(d.id.clone(), t);
                return;
            }
            t.plan.record_failure(); // it was rejected inside the block
            t.plan.sent_status = None;
        }

        // Occasionally a buggy client sends the wrong party's action.
        if st.rng.gen_bool((cfg.noise * cfg.pressure.max(0.2)).clamp(0.0, 1.0)) {
            if let Some((actor, action)) = synth::invalid_action(d) {
                if let Some(&i) = self.by_addr.get(&actor) {
                    st.stats.noise_sent += 1;
                    st.tokens -= 1.0;
                    let _ = self.send(st, i, action).await;
                }
            }
        }

        match synth::decide(&mut st.rng, d, &t.plan, now) {
            Step::Done => {}
            Step::WaitUntil(at) => {
                t.next_at = at.max(now) + 1 + st.rng.gen_range(0..3);
                st.deals.insert(d.id.clone(), t);
            }
            Step::Act { actor, action } => {
                // Deals involving human wallets: only act for our own agents.
                let Some(&i) = self.by_addr.get(&actor) else { return };
                st.tokens -= 1.0;
                match self.send(st, i, action).await {
                    Ok(_) => {
                        t.plan.sent_status = Some(d.status);
                        t.plan.sent_at = now;
                        t.next_at = now + 2 + synth::think(&mut st.rng, 10.0);
                    }
                    Err(_) => {
                        t.plan.record_failure();
                        t.next_at = now + 3 + synth::think(&mut st.rng, 10.0);
                    }
                }
                st.deals.insert(d.id.clone(), t);
            }
        }
    }
}
