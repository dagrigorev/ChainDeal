//! Synthetic economy shared by the bulk history loader and the live simulator.
//!
//! Agents have deterministic keys (derived from their index), so both tools —
//! and restarts — control the same accounts without storing secrets. Each deal
//! gets a *scenario* up front (happy path or one of several failure paths);
//! the [`decide`] director then reads the deal's actual on-chain state and
//! returns the next step. Because decisions are driven by real state, rejected
//! transactions (e.g. insufficient funds) naturally divert deals into failure.

use chaindeal_core::contract::policy_for;
use chaindeal_core::*;
use rand::seq::SliceRandom;
use rand::Rng;
use serde::Serialize;

pub const AGENT_SEED: &str = "chaindeal-sim-agent";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Arbiter,
    Business,
    Consumer,
}

/// Index layout: `[0, arbiters)` arbiters, then businesses, then consumers.
#[derive(Debug, Clone, Copy)]
pub struct Roster {
    pub arbiters: u32,
    pub businesses: u32,
    pub consumers: u32,
}

impl Roster {
    pub fn from_total(total: u32) -> Self {
        let total = total.max(12);
        let arbiters = (total / 150).max(3);
        let businesses = total / 5;
        Roster { arbiters, businesses, consumers: total - arbiters - businesses }
    }
    pub fn total(&self) -> u32 {
        self.arbiters + self.businesses + self.consumers
    }
    pub fn role(&self, i: u32) -> AgentRole {
        if i < self.arbiters {
            AgentRole::Arbiter
        } else if i < self.arbiters + self.businesses {
            AgentRole::Business
        } else {
            AgentRole::Consumer
        }
    }
    pub fn kind(&self, i: u32) -> PartyKind {
        match self.role(i) {
            AgentRole::Consumer => PartyKind::Consumer,
            _ => PartyKind::Business,
        }
    }
    pub fn pick<R: Rng>(&self, rng: &mut R, role: AgentRole) -> u32 {
        match role {
            AgentRole::Arbiter => rng.gen_range(0..self.arbiters),
            AgentRole::Business => self.arbiters + rng.gen_range(0..self.businesses),
            AgentRole::Consumer => self.arbiters + self.businesses + rng.gen_range(0..self.consumers),
        }
    }
    fn pick_kind<R: Rng>(&self, rng: &mut R, k: PartyKind) -> u32 {
        self.pick(rng, if k == PartyKind::Business { AgentRole::Business } else { AgentRole::Consumer })
    }
}

pub fn agent_secret(i: u32) -> String {
    sha256_hex(format!("{AGENT_SEED}:{i}").as_bytes())
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub index: u32,
    pub secret: String,
    pub address: String,
}

pub fn agent(i: u32) -> Agent {
    let secret = agent_secret(i);
    let (_, address) = keypair_from_secret(&secret).expect("derived secret is valid");
    Agent { index: i, secret, address }
}

fn h(i: u32, salt: u32) -> usize {
    let b = sha256(format!("{i}:{salt}").as_bytes());
    u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
}

const FIRST: [&str; 30] = [
    "Ada", "Liam", "Noah", "Emma", "Olivia", "Mateo", "Sofia", "Yuki", "Aarav", "Zara", "Ivan", "Chloe", "Lucas", "Mia",
    "Omar", "Leila", "Hugo", "Nina", "Kofi", "Elena", "Jonas", "Priya", "Diego", "Freya", "Tariq", "Ines", "Mika", "Anya",
    "Felix", "Rosa",
];
const LAST: [&str; 25] = [
    "Novak", "Reyes", "Kowalski", "Haddad", "Lindqvist", "Okafor", "Moreau", "Tanaka", "Silva", "Petrov", "Nguyen",
    "Fischer", "Rossi", "Kaur", "Duarte", "Andersen", "Costa", "Ibrahim", "Varga", "Murphy", "Sato", "Weber", "Park",
    "Ortiz", "Laine",
];
const PREFIX: [&str; 20] = [
    "Northwind", "Bluepeak", "Ironleaf", "Solace", "Quarry", "Harbor", "Brightline", "Cobalt", "Juniper", "Meridian",
    "Atlas", "Kestrel", "Driftwood", "Lumen", "Granite", "Orchard", "Vector", "Saffron", "Tidewater", "Summit",
];
const SECTOR: [&str; 15] = [
    "Logistics", "Electronics", "Foods", "Textiles", "Hardware", "Pharma", "Furniture", "Software", "Energy", "Print",
    "Robotics", "Coffee", "Outfitters", "Metals", "Studio",
];
const SUFFIX: [&str; 8] = ["Ltd", "GmbH", "Inc", "BV", "Oy", "SA", "& Co", "Group"];
const COURT: [&str; 8] = ["TrustCourt", "Fairway", "Equitas", "Balance", "Verity", "Keystone", "Clearwater", "Halcyon"];
const COURT_KIND: [&str; 3] = ["Arbitration", "Dispute Services", "Mediation"];

pub fn agent_name(r: &Roster, i: u32) -> String {
    match r.role(i) {
        AgentRole::Arbiter => format!("{} {}", COURT[i as usize % COURT.len()], COURT_KIND[(i as usize / COURT.len()) % 3]),
        AgentRole::Business => format!(
            "{} {} {}",
            PREFIX[h(i, 1) % PREFIX.len()],
            SECTOR[h(i, 2) % SECTOR.len()],
            SUFFIX[h(i, 3) % SUFFIX.len()]
        ),
        AgentRole::Consumer => format!(
            "{} {}. {}",
            FIRST[h(i, 1) % FIRST.len()],
            (b'A' + (h(i, 2) % 26) as u8) as char,
            LAST[h(i, 3) % LAST.len()]
        ),
    }
}

struct Product {
    name: &'static str,
    min: u64,
    max: u64,
    qty: u32,
}

const fn p(name: &'static str, min: u64, max: u64, qty: u32) -> Product {
    Product { name, min, max, qty }
}

// Prices in whole DEAL. Ranges keep most deals affordable for a 10,000 DEAL
// starting balance, so "insufficient funds" failures stay a realistic minority.
const C2C: [Product; 10] = [
    p("Road bike", 150, 900, 1), p("Used smartphone", 80, 600, 1), p("Vintage camera", 60, 700, 1),
    p("Sofa", 100, 800, 1), p("Concert tickets", 40, 250, 4), p("Board game collection", 30, 200, 1),
    p("Gaming console", 150, 450, 1), p("Designer jacket", 60, 400, 1), p("Vinyl records", 10, 60, 8),
    p("Espresso machine", 80, 500, 1),
];
const C2B: [Product; 8] = [
    p("Logo design package", 250, 1500, 1), p("Translation (per page)", 20, 60, 20), p("Laptop trade-in", 200, 900, 1),
    p("Photography session", 200, 1200, 1), p("Bookkeeping hours", 40, 90, 12), p("Handmade ceramics", 15, 80, 20),
    p("Code review hours", 60, 140, 10), p("Voice-over recording", 150, 700, 1),
];
const B2C: [Product; 10] = [
    p("Noise-cancelling headphones", 120, 400, 1), p("Smartwatch", 150, 450, 1), p("Coffee subscription (3 mo)", 45, 90, 1),
    p("Running shoes", 70, 220, 2), p("Standing desk", 250, 800, 1), p("Air purifier", 120, 450, 1),
    p("Winter coat", 90, 400, 1), p("Tablet", 250, 900, 1), p("Cookware set", 60, 300, 1), p("E-bike service plan", 80, 250, 1),
];
const B2B: [Product; 10] = [
    p("Copy paper (pallet)", 180, 320, 6), p("42U server rack", 900, 2200, 3), p("Container slot (40ft)", 250, 600, 6),
    p("Cloud credits (1k)", 50, 120, 20), p("Steel coil (t)", 400, 900, 4), p("Office chairs", 120, 350, 12),
    p("Roasted coffee (50kg)", 300, 700, 4), p("Industrial sensors", 60, 180, 20), p("Consulting days", 500, 1200, 4),
    p("Packaging film (roll)", 30, 90, 30),
];

fn catalog(t: DealType) -> &'static [Product] {
    match t {
        DealType::C2C => &C2C,
        DealType::C2B => &C2B,
        DealType::B2C => &B2C,
        DealType::B2B => &B2B,
    }
}

/// How a deal is meant to end. Failure scenarios are scaled by `pressure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Complete,
    SellerClaims,
    CounterpartyDeclines,
    ProposalLapses,
    ProposerWithdraws,
    BuyerDefaults,
    SellerCancels,
    BuyerWithdraws,
    SellerDefaults,
    DisputeBuyerWins,
    DisputeSellerWins,
    DisputeSplit,
}

impl Scenario {
    pub fn is_failure(self) -> bool {
        !matches!(self, Scenario::Complete | Scenario::SellerClaims)
    }
    fn needs_arbiter(self) -> bool {
        matches!(self, Scenario::DisputeBuyerWins | Scenario::DisputeSellerWins | Scenario::DisputeSplit)
    }
}

fn choose_scenario<R: Rng>(rng: &mut R, t: DealType, pressure: f64) -> Scenario {
    use Scenario::*;
    let withdraw = if policy_for(t).buyer_can_withdraw { 4.0 } else { 0.0 };
    let table = [
        (Complete, 52.0), (SellerClaims, 7.0),
        (CounterpartyDeclines, 7.0), (ProposalLapses, 3.0), (ProposerWithdraws, 2.5), (BuyerDefaults, 3.5),
        (SellerCancels, 2.5), (BuyerWithdraws, withdraw), (SellerDefaults, 3.5),
        (DisputeBuyerWins, 3.5), (DisputeSellerWins, 3.0), (DisputeSplit, 4.0),
    ];
    let weight = |(s, w): &(Scenario, f64)| if s.is_failure() { w * pressure } else { *w };
    let total: f64 = table.iter().map(weight).sum();
    let mut x = rng.gen_range(0.0..total);
    for e in &table {
        x -= weight(e);
        if x <= 0.0 {
            return e.0;
        }
    }
    Complete
}

/// Per-deal intent tracked by the driver (not stored on chain).
#[derive(Debug, Clone)]
pub struct Plan {
    pub scenario: Scenario,
    pub ruling_bps: u16,
    pub rating: u8,
    /// Rejected attempts; after two, the deal is steered to a deadline failure.
    pub failures: u8,
    /// Status when we last acted, to avoid re-sending before the block lands.
    pub sent_status: Option<DealStatus>,
    pub sent_at: u64,
}

impl Plan {
    pub fn new<R: Rng>(rng: &mut R, scenario: Scenario) -> Self {
        let ruling_bps = match scenario {
            Scenario::DisputeBuyerWins => rng.gen_range(7_500..=10_000),
            Scenario::DisputeSellerWins => rng.gen_range(0..=2_500),
            _ => rng.gen_range(3_000..=7_000),
        } / 100 * 100;
        let rating = *[5u8, 5, 5, 4, 4, 3, 2].choose(rng).unwrap();
        Plan { scenario, ruling_bps, rating, failures: 0, sent_status: None, sent_at: 0 }
    }

    /// Picks a plausible plan for a deal we did not create (restart / adoption).
    pub fn adopt<R: Rng>(rng: &mut R, d: &Deal) -> Self {
        let s = if d.status == DealStatus::Disputed {
            *[Scenario::DisputeBuyerWins, Scenario::DisputeSellerWins, Scenario::DisputeSplit].choose(rng).unwrap()
        } else {
            choose_scenario(rng, d.deal_type, 1.0)
        };
        Plan::new(rng, s)
    }

    /// After repeated rejections, stop retrying and let the deadline decide.
    pub fn record_failure(&mut self) {
        self.failures += 1;
        if self.failures >= 2 {
            self.scenario = match self.scenario {
                Scenario::Complete | Scenario::SellerClaims => Scenario::BuyerDefaults,
                s => s,
            };
        }
    }
}

pub struct NewDeal {
    pub proposer: u32,
    pub action: Action,
    pub plan: Plan,
    pub deal_type: DealType,
}

pub fn new_deal<R: Rng>(rng: &mut R, r: &Roster, pressure: f64) -> NewDeal {
    let t = *[DealType::C2C, DealType::C2C, DealType::C2C, DealType::B2C, DealType::B2C, DealType::B2C, DealType::B2C,
        DealType::B2B, DealType::B2B, DealType::C2B, DealType::C2B]
        .choose(rng)
        .unwrap();
    let (sk, bk) = match t {
        DealType::C2C => (PartyKind::Consumer, PartyKind::Consumer),
        DealType::C2B => (PartyKind::Consumer, PartyKind::Business),
        DealType::B2C => (PartyKind::Business, PartyKind::Consumer),
        DealType::B2B => (PartyKind::Business, PartyKind::Business),
    };
    let seller = r.pick_kind(rng, sk);
    let mut buyer = r.pick_kind(rng, bk);
    while buyer == seller {
        buyer = r.pick_kind(rng, bk);
    }
    let scenario = choose_scenario(rng, t, pressure);
    let policy = policy_for(t);
    let arbiter = if policy.arbiter_required || scenario.needs_arbiter() || rng.gen_bool(0.5) {
        Some(r.pick(rng, AgentRole::Arbiter))
    } else {
        None
    };

    let cat = catalog(t);
    let n_items = if t == DealType::B2B { rng.gen_range(1..=3) } else if rng.gen_bool(0.2) { 2 } else { 1 };
    let mut items: Vec<LineItem> = Vec::new();
    for prod in cat.choose_multiple(rng, n_items) {
        let price = rng.gen_range(prod.min..=prod.max) * 100 + [0, 0, 50, 99].choose(rng).unwrap();
        items.push(LineItem { name: prod.name.to_string(), qty: rng.gen_range(1..=prod.qty), unit_price: price });
    }
    let title = if items.len() == 1 { items[0].name.clone() } else { format!("{} + {} more", items[0].name, items.len() - 1) };

    let seller_proposes = rng.gen_bool(0.7);
    let (proposer, counterparty, role) =
        if seller_proposes { (seller, buyer, Role::Seller) } else { (buyer, seller, Role::Buyer) };
    NewDeal {
        proposer,
        deal_type: t,
        plan: Plan::new(rng, scenario),
        action: Action::CreateDeal {
            role,
            counterparty: agent(counterparty).address,
            title,
            description: String::new(),
            items,
            arbiter: arbiter.map(|a| agent(a).address),
        },
    }
}

pub enum Step {
    Act { actor: String, action: Action },
    WaitUntil(u64),
    Done,
}

const DECLINE: [&str; 5] = ["price too high", "no longer needed", "found another supplier", "terms unacceptable", ""];
const CANCEL: [&str; 4] = ["out of stock", "cannot fulfil this order", "listing withdrawn", "sold elsewhere"];
const WITHDRAW: [&str; 3] = ["changed my mind", "found it cheaper", "ordered by mistake"];
const DISPUTE: [&str; 6] = [
    "item not as described", "arrived damaged", "partial delivery", "wrong model shipped",
    "service not performed to spec", "never arrived",
];

/// Decides the next step for a deal from its *actual* on-chain state.
pub fn decide<R: Rng>(rng: &mut R, d: &Deal, plan: &Plan, now: u64) -> Step {
    use DealStatus::*;
    use Scenario::*;
    if d.status.is_terminal() {
        return Step::Done;
    }
    let act = |actor: &str, action: Action| Step::Act { actor: actor.to_string(), action };
    let id = d.id.clone();
    let counterparty = if d.proposer == d.seller { d.buyer.as_str() } else { d.seller.as_str() };

    // A lapsed deadline is enforced first — whoever notices acts as keeper.
    if let Some(t) = d.deadline {
        if now >= t {
            let keeper = match d.status {
                Funded => d.buyer.as_str(),
                Accepted => d.seller.as_str(),
                _ => d.proposer.as_str(),
            };
            return act(keeper, Action::ExpireDeal { deal_id: id });
        }
    }

    match (d.status, plan.scenario) {
        (Proposed, CounterpartyDeclines) => act(counterparty, Action::DeclineDeal { deal_id: id, reason: DECLINE.choose(rng).unwrap().to_string() }),
        (Proposed, ProposalLapses) => Step::WaitUntil(d.deadline.unwrap_or(now)),
        (Proposed, ProposerWithdraws) => act(&d.proposer, Action::CancelDeal { deal_id: id, reason: "proposal withdrawn".into() }),
        (Proposed, _) => act(counterparty, Action::AcceptDeal { deal_id: id }),

        (Accepted, BuyerDefaults) => Step::WaitUntil(d.deadline.unwrap_or(now)),
        (Accepted, _) => act(&d.buyer, Action::FundDeal { deal_id: id }),

        (Funded, SellerCancels) => act(&d.seller, Action::CancelDeal { deal_id: id, reason: CANCEL.choose(rng).unwrap().to_string() }),
        (Funded, BuyerWithdraws) if policy_for(d.deal_type).buyer_can_withdraw => {
            act(&d.buyer, Action::CancelDeal { deal_id: id, reason: WITHDRAW.choose(rng).unwrap().to_string() })
        }
        (Funded, SellerDefaults) => Step::WaitUntil(d.deadline.unwrap_or(now)),
        (Funded, _) => {
            let tracking = format!("{}-{:08}", ["DHL", "UPS", "PNL", "FREIGHT", "COURIER"].choose(rng).unwrap(), rng.gen_range(0..100_000_000));
            act(&d.seller, Action::MarkShipped { deal_id: id, tracking })
        }

        (Shipped, SellerClaims) => match d.release_after {
            Some(t) if now < t => Step::WaitUntil(t),
            _ => act(&d.seller, Action::ClaimRelease { deal_id: id }),
        },
        (Shipped, DisputeBuyerWins | DisputeSellerWins | DisputeSplit) if d.arbiter.is_some() => {
            act(&d.buyer, Action::OpenDispute { deal_id: id, reason: DISPUTE.choose(rng).unwrap().to_string() })
        }
        (Shipped, _) => act(&d.buyer, Action::ConfirmReceipt { deal_id: id, rating: plan.rating }),

        (Disputed, _) => match &d.arbiter {
            Some(a) => act(a, Action::ResolveDispute { deal_id: id, buyer_refund_bps: plan.ruling_bps, note: String::new() }),
            None => Step::Done,
        },
        _ => Step::Done,
    }
}

/// A deliberately invalid action (wrong party for the current step), modelling
/// buggy or malicious clients. The contract must refuse every one of these.
pub fn invalid_action(d: &Deal) -> Option<(String, Action)> {
    let id = d.id.clone();
    Some(match d.status {
        DealStatus::Proposed => (d.proposer.clone(), Action::AcceptDeal { deal_id: id }),
        DealStatus::Accepted => (d.seller.clone(), Action::FundDeal { deal_id: id }),
        DealStatus::Funded => (d.buyer.clone(), Action::MarkShipped { deal_id: id, tracking: String::new() }),
        DealStatus::Shipped => (d.seller.clone(), Action::ConfirmReceipt { deal_id: id, rating: 5 }),
        DealStatus::Disputed => (d.buyer.clone(), Action::ResolveDispute { deal_id: id, buyer_refund_bps: 10_000, note: String::new() }),
        _ => return None,
    })
}

/// Businesses with surplus pay individuals (salaries, refunds) so value keeps circulating.
pub fn payroll<R: Rng>(rng: &mut R, r: &Roster) -> (u32, u32, Amount) {
    (r.pick(rng, AgentRole::Business), r.pick(rng, AgentRole::Consumer), rng.gen_range(300..1_500) * 100)
}

/// Seconds a party "thinks" before acting, scaled to the environment.
pub fn think<R: Rng>(rng: &mut R, scale: f64) -> u64 {
    let base: f64 = rng.gen_range(0.2..1.0);
    (base * base * scale).max(1.0) as u64
}
