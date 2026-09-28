//! chaindeal-bulk — generates a realistic multi-week history (1M+ transactions)
//! as a *real* chain: every transaction is ed25519-signed by an agent, applied
//! through the same contract code as the node, sealed into proof-of-work blocks
//! and committed atomically to Tarantool. Deals follow the same scenario mix as
//! the live simulator, including declines, lapses, defaults and disputes, plus
//! a small share of invalid transactions that the contract rejects.
//!
//! Runs against an empty database (before the node starts):
//!   chaindeal-bulk --txs 1000000 --days 7 --agents 6000

#![allow(clippy::inconsistent_digit_grouping)]

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::time::Instant;

use anyhow::{bail, Result};
use chaindeal_backend::chain::{mine_parallel, now_ms};
use chaindeal_backend::db::Db;
use chaindeal_backend::synth::{self, Plan, Roster, Step};
use chaindeal_core::contract::treasury_account;
use chaindeal_core::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

struct Args {
    txs: u64,
    days: u64,
    agents: u32,
    block_secs: u64,
    threads: usize,
    seed: u64,
    pressure: f64,
    difficulty: u32,
}

fn args() -> Args {
    let mut a = Args { txs: 1_000_000, days: 7, agents: 6000, block_secs: 60, threads: 8, seed: 7, pressure: 1.0, difficulty: DEFAULT_DIFFICULTY };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i + 1 < argv.len() {
        let v = &argv[i + 1];
        match argv[i].as_str() {
            "--txs" => a.txs = v.parse().expect("--txs"),
            "--days" => a.days = v.parse().expect("--days"),
            "--agents" => a.agents = v.parse().expect("--agents"),
            "--block-secs" => a.block_secs = v.parse().expect("--block-secs"),
            "--threads" => a.threads = v.parse().expect("--threads"),
            "--seed" => a.seed = v.parse().expect("--seed"),
            "--pressure" => a.pressure = v.parse().expect("--pressure"),
            "--difficulty" => a.difficulty = v.parse().expect("--difficulty"),
            other => panic!("unknown flag {other}"),
        }
        i += 2;
    }
    a
}

/// Parties react within about a minute — the contract's deadlines are minutes
/// long, so the history must run on the same clock or every deal would lapse.
const REACTION_SECS: f64 = 75.0;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Something to do in the current block.
struct Job {
    agent: u32,
    action: Action,
    /// Deal this action advances (None for new deals, payroll and registration).
    deal: Option<String>,
    /// Plan attached to a newly proposed deal.
    new_plan: Option<Plan>,
    noise: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = args();
    let db = Db::connect(&env("TARANTOOL_ADDR", "127.0.0.1:3301"), &env("TARANTOOL_USER", "chaindeal"), &env("TARANTOOL_PASSWORD", "chaindeal")).await?;
    if db.tip().await?.is_some() {
        bail!("the chain is not empty — reset the database first (make db-reset)");
    }
    let started = Instant::now();
    let mut rng = StdRng::seed_from_u64(a.seed);
    let roster = Roster::from_total(a.agents);
    let agents: Vec<synth::Agent> = (0..roster.total()).map(synth::agent).collect();
    let by_addr: HashMap<String, u32> = agents.iter().map(|g| (g.address.clone(), g.index)).collect();

    let end = now_ms() / 1000 - 120;
    let start = end - a.days * 86_400;
    let n_blocks = (a.days * 86_400 / a.block_secs).max(4);
    println!(
        "Generating ~{} txs over {} days ({} blocks of {}s), {} agents ({} arbiters, {} businesses, {} individuals)",
        a.txs, a.days, n_blocks, a.block_secs, roster.total(), roster.arbiters, roster.businesses, roster.consumers
    );

    // Genesis, back-dated to the start of the history.
    let genesis = BlockHeader {
        height: 0, prev_hash: "0".repeat(64), timestamp: start, merkle_root: merkle_root(&[]),
        tx_count: 0, difficulty: a.difficulty, nonce: 0, producer: "bulk-loader".into(),
    };
    let (gh, ghash) = mine_parallel(genesis, a.threads);
    db.commit_block(Some(&Block { header: gh, hash: ghash.clone(), tx_hashes: vec![] }), &[], &[&treasury_account()], &[]).await?;

    let mut st = WorkingState::default();
    st.accounts.insert(TREASURY_ADDRESS.into(), treasury_account());
    let mut prev_hash = ghash;
    let mut height = 0u64;
    let mut heap: BinaryHeap<Reverse<(u64, u64, String)>> = BinaryHeap::new();
    let mut plans: HashMap<String, Plan> = HashMap::new();
    let mut seq = 0u64;
    let (mut done_txs, mut confirmed_total, mut rejected_total, mut deals_opened) = (0u64, 0u64, 0u64, 0u64);
    let mut outcome: HashMap<DealStatus, u64> = HashMap::new();
    let mut avg_per_deal = 5.5f64;
    let reg_blocks = 3u64;

    for b in 1..=n_blocks {
        let t = start + b * a.block_secs;
        let mut jobs: Vec<Job> = Vec::new();

        // Onboarding: every agent registers in the first few blocks.
        if b <= reg_blocks {
            let per = roster.total().div_ceil(reg_blocks as u32);
            for i in ((b as u32 - 1) * per)..(b as u32 * per).min(roster.total()) {
                jobs.push(Job { agent: i, action: Action::Register { name: synth::agent_name(&roster, i), kind: roster.kind(i) }, deal: None, new_plan: None, noise: false });
            }
        }

        // Deals whose next step is due.
        while let Some(Reverse((at, _, _))) = heap.peek() {
            if *at > t {
                break;
            }
            let Reverse((_, _, id)) = heap.pop().unwrap();
            let (Some(d), Some(plan)) = (st.deals.get(&id), plans.get(&id)) else { continue };
            match synth::decide(&mut rng, d, plan, t) {
                Step::Done => {}
                Step::WaitUntil(w) => {
                    seq += 1;
                    heap.push(Reverse((w.max(t) + 1, seq, id)));
                }
                Step::Act { actor, action } => {
                    if rng.gen_bool((0.02 * a.pressure).min(1.0)) {
                        if let Some((bad_actor, bad)) = synth::invalid_action(d) {
                            jobs.push(Job { agent: by_addr[&bad_actor], action: bad, deal: None, new_plan: None, noise: true });
                        }
                    }
                    jobs.push(Job { agent: by_addr[&actor], action, deal: Some(id), new_plan: None, noise: false });
                }
            }
        }

        // New demand: paced to hit the target, with a day/night cycle.
        if b > reg_blocks {
            let remaining_blocks = (n_blocks - b + 1) as f64;
            let remaining_txs = a.txs.saturating_sub(done_txs) as f64;
            let hour = ((t % 86_400) as f64) / 3600.0;
            let diurnal = 1.0 + 0.6 * ((hour - 14.0) / 24.0 * std::f64::consts::TAU).cos();
            let rate = (remaining_txs / remaining_blocks / avg_per_deal).max(0.0) * diurnal;
            let n = rate.floor() as u64 + u64::from(rng.gen_bool(rate.fract().clamp(0.0, 1.0)));
            for _ in 0..n {
                if rng.gen_bool(0.06) {
                    let (from, to, amount) = synth::payroll(&mut rng, &roster);
                    let rich = st.accounts.get(&agents[from as usize].address).is_some_and(|x| x.balance > amount + 5_000_00);
                    if rich {
                        jobs.push(Job { agent: from, action: Action::Transfer { to: agents[to as usize].address.clone(), amount, memo: "payroll".into() }, deal: None, new_plan: None, noise: false });
                    }
                    continue;
                }
                let nd = synth::new_deal(&mut rng, &roster, a.pressure);
                jobs.push(Job { agent: nd.proposer, action: nd.action, deal: None, new_plan: Some(nd.plan), noise: false });
                deals_opened += 1;
            }
        }
        if jobs.is_empty() {
            continue;
        }

        // Apply in a plausible order within the block window.
        let mut confirmed: Vec<TxRecord> = Vec::new();
        let mut rejected: Vec<TxRecord> = Vec::new();
        let span = a.block_secs * 1000;
        for job in jobs {
            let ts = (t - a.block_secs) * 1000 + rng.gen_range(0..span);
            let tx = sign_action(&agents[job.agent as usize].secret, job.action, rng.gen::<u64>() >> 11, ts)?;
            match apply_tx(&mut st, &tx, t) {
                Ok(effect) => {
                    if let (Some(plan), Some(id)) = (job.new_plan, effect.deal_id.clone()) {
                        plans.insert(id.clone(), plan);
                        seq += 1;
                        heap.push(Reverse((t + synth::think(&mut rng, REACTION_SECS), seq, id)));
                    }
                    if let Some(id) = &job.deal {
                        seq += 1;
                        heap.push(Reverse((t + synth::think(&mut rng, REACTION_SECS), seq, id.clone())));
                    }
                    confirmed.push(TxRecord { hash: tx.hash, body: tx.body, signature: tx.signature, status: TxStatus::Confirmed, error: None, block_height: Some(height + 1), deal_id: effect.deal_id });
                }
                Err(e) => {
                    if let Some(id) = &job.deal {
                        if let Some(p) = plans.get_mut(id) {
                            p.record_failure();
                        }
                        seq += 1;
                        heap.push(Reverse((t + synth::think(&mut rng, REACTION_SECS), seq, id.clone())));
                    }
                    let _ = job.noise;
                    rejected.push(TxRecord { hash: tx.hash, body: tx.body, signature: tx.signature, status: TxStatus::Rejected, error: Some(e.0), block_height: None, deal_id: None });
                }
            }
        }

        let block = if confirmed.is_empty() {
            None
        } else {
            let tx_hashes: Vec<String> = confirmed.iter().map(|x| x.hash.clone()).collect();
            let header = BlockHeader {
                height: height + 1, prev_hash: prev_hash.clone(), timestamp: t, merkle_root: merkle_root(&tx_hashes),
                tx_count: tx_hashes.len() as u32, difficulty: a.difficulty, nonce: 0, producer: "bulk-loader".into(),
            };
            let (header, hash) = mine_parallel(header, a.threads);
            Some(Block { header, hash, tx_hashes })
        };

        {
            let accounts: Vec<&Account> = st.touched_accounts.iter().filter_map(|x| st.accounts.get(x)).collect();
            let deals: Vec<&Deal> = st.touched_deals.iter().filter_map(|x| st.deals.get(x)).collect();
            let all: Vec<TxRecord> = confirmed.iter().chain(rejected.iter()).cloned().collect();
            db.commit_block(block.as_ref(), &all, &accounts, &deals).await?;
        }
        if let Some(bl) = block {
            height += 1;
            prev_hash = bl.hash;
        }
        done_txs += (confirmed.len() + rejected.len()) as u64;
        confirmed_total += confirmed.len() as u64;
        rejected_total += rejected.len() as u64;

        // Closed deals are never touched again: drop them to keep memory flat.
        let closed: Vec<String> = st.touched_deals.iter().filter(|id| st.deals.get(*id).is_some_and(|d| d.status.is_terminal())).cloned().collect();
        for id in closed {
            if let Some(d) = st.deals.remove(&id) {
                *outcome.entry(d.status).or_default() += 1;
            }
            plans.remove(&id);
        }
        st.touched_accounts.clear();
        st.touched_deals.clear();
        if deals_opened > 1000 {
            avg_per_deal = (done_txs.saturating_sub(roster.total() as u64)) as f64 / deals_opened as f64;
        }

        if b % 100 == 0 || b == n_blocks {
            let secs = started.elapsed().as_secs_f64();
            println!(
                "block {:>5}/{n_blocks}  txs {:>9}  deals {:>8}  open {:>6}  {:.0} tx/s",
                b, done_txs, deals_opened, st.deals.len(), done_txs as f64 / secs
            );
        }
    }

    println!("\nDone in {:.0}s: {} blocks, {} txs ({} confirmed, {} rejected), {} deals opened, {} still open",
        started.elapsed().as_secs_f64(), height, done_txs, confirmed_total, rejected_total, deals_opened, st.deals.len());
    let mut o: Vec<_> = outcome.into_iter().collect();
    o.sort_by_key(|(_, n)| Reverse(*n));
    for (s, n) in o {
        println!("  {:<10} {:>8}  ({:.1}%)", format!("{s:?}"), n, 100.0 * n as f64 / deals_opened.max(1) as f64);
    }
    Ok(())
}
