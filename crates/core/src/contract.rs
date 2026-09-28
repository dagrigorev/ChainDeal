//! The escrow "smart deal" contract. Pure and deterministic: given the same
//! state, transaction and block time it always produces the same result.
//!
//! Lifecycle:
//! ```text
//! Proposed ──accept──▶ Accepted ──fund──▶ Funded ──ship──▶ Shipped ──confirm / claim──▶ Completed
//!  │ │ │                  │ │               │ │ └─dispute─┐    │
//!  │ │ └────cancel────────┴─┼──cancel*──────┘ │           ▼    └──dispute──▶ Disputed ──resolve──▶ Resolved
//!  │ └─decline──▶ Declined  │                 │
//!  └──deadline──▶ Expired ◀─┘ (buyer never     └─deadline──▶ Failed (seller never delivered:
//!                             funded)                         refund + bond slashed to buyer)
//! ```
//! `cancel*` from Funded: always by the seller (full refund), by the buyer only
//! where the policy grants a right of withdrawal (B2C consumer protection).
//! Deadlines are enforced by an explicit `expire_deal` transaction, which
//! either party or the arbiter may send once the deadline has passed.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::types::*;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ContractError(pub String);

fn err<T>(msg: impl Into<String>) -> Result<T, ContractError> {
    Err(ContractError(msg.into()))
}

/// Per-deal-type rules.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct DealPolicy {
    pub deal_type: DealType,
    /// Protocol fee taken from the seller's payout, in basis points.
    pub fee_bps: u64,
    /// Performance bond the seller locks when committing, in basis points of the amount.
    pub seller_bond_bps: u64,
    /// After shipment, the seller may claim payment once this many seconds pass without a dispute.
    pub release_window_secs: u64,
    pub arbiter_required: bool,
    /// Buyer may cancel a funded, not-yet-shipped deal for a full refund.
    pub buyer_can_withdraw: bool,
    /// A proposal lapses (Expired) if not accepted within this many seconds.
    pub accept_window_secs: u64,
    /// An accepted deal lapses (Expired, buyer default) if not funded in time.
    pub fund_window_secs: u64,
    /// A funded deal fails (seller default) if not delivered in time.
    pub delivery_window_secs: u64,
    pub summary: &'static str,
}

pub const POLICIES: [DealPolicy; 4] = [
    DealPolicy {
        deal_type: DealType::C2C,
        fee_bps: 100,
        seller_bond_bps: 0,
        release_window_secs: 120,
        arbiter_required: false,
        buyer_can_withdraw: false,
        accept_window_secs: 180,
        fund_window_secs: 180,
        delivery_window_secs: 300,
        summary: "Peer-to-peer escrow. Arbiter optional; without one, disputes are disabled and the release window protects the seller.",
    },
    DealPolicy {
        deal_type: DealType::C2B,
        fee_bps: 100,
        seller_bond_bps: 0,
        release_window_secs: 180,
        arbiter_required: false,
        buyer_can_withdraw: false,
        accept_window_secs: 180,
        fund_window_secs: 180,
        delivery_window_secs: 300,
        summary: "Individual selling to a business (e.g. trade-in, freelance work). Arbiter optional.",
    },
    DealPolicy {
        deal_type: DealType::B2C,
        fee_bps: 150,
        seller_bond_bps: 0,
        release_window_secs: 300,
        arbiter_required: true,
        buyer_can_withdraw: true,
        accept_window_secs: 180,
        fund_window_secs: 240,
        delivery_window_secs: 300,
        summary: "Consumer protection: arbiter mandatory, longer inspection window, buyer may withdraw before shipment.",
    },
    DealPolicy {
        deal_type: DealType::B2B,
        fee_bps: 50,
        seller_bond_bps: 1000,
        release_window_secs: 240,
        arbiter_required: true,
        buyer_can_withdraw: false,
        accept_window_secs: 240,
        fund_window_secs: 240,
        delivery_window_secs: 420,
        summary: "Lowest fee. Seller locks a 10% performance bond, slashed to the buyer if a dispute goes mostly the buyer's way.",
    },
];

pub fn policy_for(t: DealType) -> DealPolicy {
    *POLICIES.iter().find(|p| p.deal_type == t).expect("all deal types have a policy")
}

fn bps(amount: Amount, bps: u64) -> Amount {
    // u128 avoids overflow on large amounts.
    ((amount as u128 * bps as u128) / 10_000) as Amount
}

/// In-memory slice of chain state that a block's transactions touch.
/// The node loads it from storage, applies transactions, then persists the
/// touched entries atomically.
#[derive(Debug, Clone, Default)]
pub struct WorkingState {
    pub accounts: BTreeMap<String, Account>,
    pub deals: BTreeMap<String, Deal>,
    pub touched_accounts: BTreeSet<String>,
    pub touched_deals: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TxEffect {
    pub deal_id: Option<String>,
}

impl WorkingState {
    fn acct(&self, addr: &str) -> Result<&Account, ContractError> {
        self.accounts
            .get(addr)
            .ok_or_else(|| ContractError(format!("account {addr} is not registered")))
    }

    fn acct_mut(&mut self, addr: &str) -> Result<&mut Account, ContractError> {
        if !self.accounts.contains_key(addr) {
            return err(format!("account {addr} is not registered"));
        }
        self.touched_accounts.insert(addr.to_string());
        Ok(self.accounts.get_mut(addr).unwrap())
    }

    fn treasury_mut(&mut self) -> &mut Account {
        self.touched_accounts.insert(TREASURY_ADDRESS.to_string());
        self.accounts
            .entry(TREASURY_ADDRESS.to_string())
            .or_insert_with(treasury_account)
    }

    fn deal(&self, id: &str) -> Result<&Deal, ContractError> {
        self.deals
            .get(id)
            .ok_or_else(|| ContractError(format!("deal {id} not found")))
    }

    fn put_deal(&mut self, d: Deal) {
        self.touched_deals.insert(d.id.clone());
        self.deals.insert(d.id.clone(), d);
    }

    fn lock(&mut self, addr: &str, amount: Amount) -> Result<(), ContractError> {
        let a = self.acct_mut(addr)?;
        if a.balance < amount {
            return err(format!(
                "insufficient balance: need {}, have {}",
                format_amount(amount),
                format_amount(a.balance)
            ));
        }
        a.balance -= amount;
        a.escrowed += amount;
        Ok(())
    }

    /// Moves `amount` out of `locker`'s escrow into `to`'s spendable balance.
    fn release(&mut self, locker: &str, amount: Amount, to: &str) -> Result<(), ContractError> {
        if amount == 0 {
            return Ok(());
        }
        let l = self.acct_mut(locker)?;
        l.escrowed = l
            .escrowed
            .checked_sub(amount)
            .ok_or_else(|| ContractError("escrow accounting underflow".into()))?;
        if to == TREASURY_ADDRESS {
            self.treasury_mut().balance += amount;
        } else {
            self.acct_mut(to)?.balance += amount;
        }
        Ok(())
    }
}

pub fn treasury_account() -> Account {
    Account {
        address: TREASURY_ADDRESS.into(),
        pubkey: String::new(),
        name: "Protocol Treasury".into(),
        kind: PartyKind::Business,
        balance: 0,
        escrowed: 0,
        created_at: 0,
        deals_completed: 0,
        disputes: 0,
        rating_sum: 0,
        rating_count: 0,
        defaults: 0,
    }
}

/// Applies a transaction; on error the state is left untouched.
/// Signature checks happen before this (see [`crate::verify_tx`]).
///
/// Rollback snapshots only the entries this transaction can possibly touch
/// (sender, referenced accounts, the deal, its parties, the treasury), so the
/// cost is O(1) regardless of how large the working state is.
pub fn apply_tx(state: &mut WorkingState, tx: &SignedTx, now: u64) -> Result<TxEffect, ContractError> {
    let mut accts: Vec<String> = vec![tx.body.from.clone(), TREASURY_ADDRESS.to_string()];
    accts.extend(tx.body.action.referenced_addresses().into_iter().map(str::to_string));
    let mut deals: Vec<String> = vec![format!("D-{}", &tx.hash[..12.min(tx.hash.len())])];
    if let Some(id) = tx.body.action.deal_id() {
        if let Some(d) = state.deals.get(id) {
            accts.push(d.seller.clone());
            accts.push(d.buyer.clone());
            accts.extend(d.arbiter.clone());
        }
        deals.push(id.to_string());
    }
    let saved_a: Vec<(String, Option<Account>)> =
        accts.into_iter().map(|k| { let v = state.accounts.get(&k).cloned(); (k, v) }).collect();
    let saved_d: Vec<(String, Option<Deal>)> =
        deals.into_iter().map(|k| { let v = state.deals.get(&k).cloned(); (k, v) }).collect();

    let res = apply_inner(state, tx, now);
    if res.is_err() {
        // Restore in reverse so duplicate keys end at their original value.
        // Touched markers may remain; persisting an unchanged entry is harmless.
        for (k, v) in saved_a.into_iter().rev() {
            match v {
                Some(a) => { state.accounts.insert(k, a); }
                None => { state.accounts.remove(&k); }
            }
        }
        for (k, v) in saved_d.into_iter().rev() {
            match v {
                Some(d) => { state.deals.insert(k, d); }
                None => { state.deals.remove(&k); }
            }
        }
    }
    res
}

fn apply_inner(st: &mut WorkingState, tx: &SignedTx, now: u64) -> Result<TxEffect, ContractError> {
    let from = tx.body.from.as_str();
    let action = &tx.body.action;

    if let Action::Register { name, kind } = action {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return err("name must be 1-64 characters");
        }
        if st.accounts.contains_key(from) {
            return err("address is already registered");
        }
        st.touched_accounts.insert(from.to_string());
        st.accounts.insert(
            from.to_string(),
            Account {
                address: from.to_string(),
                pubkey: tx.body.pubkey.clone(),
                name: name.to_string(),
                kind: *kind,
                balance: REGISTER_GRANT,
                escrowed: 0,
                created_at: now,
                deals_completed: 0,
                disputes: 0,
                rating_sum: 0,
                rating_count: 0,
                defaults: 0,
            },
        );
        return Ok(TxEffect::default());
    }

    st.acct(from)?;

    match action {
        Action::Register { .. } => unreachable!(),

        Action::Transfer { to, amount, memo } => {
            if *amount == 0 {
                return err("amount must be positive");
            }
            if to == from {
                return err("cannot transfer to yourself");
            }
            if memo.len() > 140 {
                return err("memo too long");
            }
            st.acct(to)?;
            let a = st.acct_mut(from)?;
            if a.balance < *amount {
                return err("insufficient balance");
            }
            a.balance -= amount;
            st.acct_mut(to)?.balance += amount;
            Ok(TxEffect::default())
        }

        Action::CreateDeal { role, counterparty, title, description, items, arbiter } => {
            let title = title.trim();
            if title.is_empty() || title.chars().count() > 120 {
                return err("title must be 1-120 characters");
            }
            if description.chars().count() > 2000 {
                return err("description too long");
            }
            if counterparty == from {
                return err("counterparty must be a different account");
            }
            if items.is_empty() || items.len() > 50 {
                return err("a deal needs 1-50 line items");
            }
            let mut amount: Amount = 0;
            for it in items {
                if it.name.trim().is_empty() || it.qty == 0 || it.unit_price == 0 {
                    return err("every line item needs a name, quantity and price");
                }
                amount = (it.qty as u64)
                    .checked_mul(it.unit_price)
                    .and_then(|line| amount.checked_add(line))
                    .ok_or_else(|| ContractError("amount overflow".into()))?;
            }
            let (seller, buyer) = match role {
                Role::Seller => (from.to_string(), counterparty.clone()),
                Role::Buyer => (counterparty.clone(), from.to_string()),
            };
            let deal_type = DealType::classify(st.acct(&seller)?.kind, st.acct(&buyer)?.kind);
            let policy = policy_for(deal_type);
            if let Some(a) = arbiter {
                st.acct(a)?;
                if a == &seller || a == &buyer {
                    return err("arbiter must be independent of both parties");
                }
            } else if policy.arbiter_required {
                return err(format!("{deal_type:?} deals require an arbiter"));
            }

            let id = format!("D-{}", &tx.hash[..12]);
            if st.deals.contains_key(&id) {
                return err("deal already exists");
            }
            let bond = bps(amount, policy.seller_bond_bps);
            let mut deal = Deal {
                id: id.clone(),
                title: title.to_string(),
                description: description.trim().to_string(),
                deal_type,
                seller: seller.clone(),
                buyer,
                arbiter: arbiter.clone(),
                proposer: from.to_string(),
                items: items.clone(),
                amount,
                fee: bps(amount, policy.fee_bps),
                bond,
                bond_locked: false,
                status: DealStatus::Proposed,
                tracking: None,
                created_at: now,
                updated_at: now,
                shipped_at: None,
                release_after: None,
                buyer_refund_bps: None,
                dispute_reason: None,
                history: vec![],
                deadline: Some(now + policy.accept_window_secs),
            };
            // A seller commits (and locks the bond) by signing the deal.
            if *role == Role::Seller && bond > 0 {
                st.lock(&seller, bond)?;
                deal.bond_locked = true;
            }
            push_event(&mut deal, DealStatus::Proposed, tx, now, "deal proposed");
            st.put_deal(deal);
            Ok(TxEffect { deal_id: Some(id) })
        }

        Action::AcceptDeal { deal_id } => {
            let mut d = st.deal(deal_id)?.clone();
            expect_status(&d, &[DealStatus::Proposed])?;
            let counterparty = if d.proposer == d.seller { &d.buyer } else { &d.seller };
            if from != counterparty {
                return err("only the counterparty can accept this deal");
            }
            expect_in_time(&d, now)?;
            if from == d.seller && d.bond > 0 && !d.bond_locked {
                st.lock(&d.seller, d.bond)?;
                d.bond_locked = true;
            }
            let window = policy_for(d.deal_type).fund_window_secs;
            d.deadline = Some(now + window);
            transition(st, d, DealStatus::Accepted, tx, now, &format!("terms accepted; buyer must fund within {window}s"))
        }

        Action::FundDeal { deal_id } => {
            let mut d = st.deal(deal_id)?.clone();
            expect_status(&d, &[DealStatus::Accepted])?;
            expect_party(from, &d.buyer, "buyer")?;
            expect_in_time(&d, now)?;
            st.lock(&d.buyer, d.amount)?;
            let window = policy_for(d.deal_type).delivery_window_secs;
            d.deadline = Some(now + window);
            let note = format!("{} DEAL locked in escrow; seller must deliver within {window}s", format_amount(d.amount));
            transition(st, d, DealStatus::Funded, tx, now, &note)
        }

        Action::MarkShipped { deal_id, tracking } => {
            let mut d = st.deal(deal_id)?.clone();
            expect_status(&d, &[DealStatus::Funded])?;
            expect_party(from, &d.seller, "seller")?;
            expect_in_time(&d, now)?;
            if tracking.len() > 120 {
                return err("tracking reference too long");
            }
            let window = policy_for(d.deal_type).release_window_secs;
            d.tracking = Some(tracking.trim().to_string()).filter(|t| !t.is_empty());
            d.shipped_at = Some(now);
            d.release_after = Some(now + window);
            d.deadline = None;
            let note = format!("delivered; auto-release in {window}s unless disputed");
            transition(st, d, DealStatus::Shipped, tx, now, &note)
        }

        Action::ConfirmReceipt { deal_id, rating } => {
            let d = st.deal(deal_id)?.clone();
            expect_status(&d, &[DealStatus::Shipped])?;
            expect_party(from, &d.buyer, "buyer")?;
            if !(1..=5).contains(rating) {
                return err("rating must be 1-5");
            }
            let s = st.acct_mut(&d.seller)?;
            s.rating_sum += *rating as u32;
            s.rating_count += 1;
            settle_complete(st, d, tx, now, &format!("buyer confirmed receipt, rated {rating}/5"))
        }

        Action::ClaimRelease { deal_id } => {
            let d = st.deal(deal_id)?.clone();
            expect_status(&d, &[DealStatus::Shipped])?;
            expect_party(from, &d.seller, "seller")?;
            let due = d.release_after.unwrap_or(u64::MAX);
            if now < due {
                return err(format!("release window still open for {}s", due - now));
            }
            settle_complete(st, d, tx, now, "release window elapsed; seller claimed payment")
        }

        Action::OpenDispute { deal_id, reason } => {
            let mut d = st.deal(deal_id)?.clone();
            expect_status(&d, &[DealStatus::Funded, DealStatus::Shipped])?;
            if from != d.buyer && from != d.seller {
                return err("only a party to the deal can open a dispute");
            }
            if d.arbiter.is_none() {
                return err("this deal has no arbiter, so disputes are disabled");
            }
            let reason = reason.trim();
            if reason.is_empty() || reason.len() > 500 {
                return err("dispute reason must be 1-500 characters");
            }
            d.dispute_reason = Some(reason.to_string());
            d.deadline = None;
            for p in [d.buyer.clone(), d.seller.clone()] {
                st.acct_mut(&p)?.disputes += 1;
            }
            transition(st, d, DealStatus::Disputed, tx, now, reason)
        }

        Action::ResolveDispute { deal_id, buyer_refund_bps, note } => {
            let mut d = st.deal(deal_id)?.clone();
            expect_status(&d, &[DealStatus::Disputed])?;
            if d.arbiter.as_deref() != Some(from) {
                return err("only the appointed arbiter can resolve this dispute");
            }
            if *buyer_refund_bps > 10_000 {
                return err("refund share cannot exceed 100%");
            }
            let policy = policy_for(d.deal_type);
            let refund = bps(d.amount, *buyer_refund_bps as u64);
            let seller_part = d.amount - refund;
            let fee = bps(seller_part, policy.fee_bps);
            st.release(&d.buyer, refund, &d.buyer.clone())?;
            st.release(&d.buyer, seller_part - fee, &d.seller.clone())?;
            st.release(&d.buyer, fee, TREASURY_ADDRESS)?;
            if d.bond_locked {
                let bond_to = if *buyer_refund_bps > 5_000 { d.buyer.clone() } else { d.seller.clone() };
                st.release(&d.seller, d.bond, &bond_to)?;
                d.bond_locked = false;
            }
            d.fee = fee;
            d.buyer_refund_bps = Some(*buyer_refund_bps);
            let msg = format!(
                "arbiter awarded {:.2}% to buyer. {}",
                *buyer_refund_bps as f64 / 100.0,
                note.trim()
            );
            transition(st, d, DealStatus::Resolved, tx, now, msg.trim())
        }

        Action::CancelDeal { deal_id, reason } => {
            let mut d = st.deal(deal_id)?.clone();
            let is_buyer = from == d.buyer;
            let is_seller = from == d.seller;
            if !is_buyer && !is_seller {
                return err("only a party to the deal can cancel it");
            }
            match d.status {
                DealStatus::Proposed | DealStatus::Accepted => {}
                DealStatus::Funded => {
                    if is_buyer && !policy_for(d.deal_type).buyer_can_withdraw {
                        return err(format!(
                            "{:?} buyers cannot withdraw after funding; ask the seller to cancel or open a dispute",
                            d.deal_type
                        ));
                    }
                    st.release(&d.buyer, d.amount, &d.buyer.clone())?;
                }
                s => return err(format!("cannot cancel a deal in status {s:?}")),
            }
            if d.bond_locked {
                st.release(&d.seller, d.bond, &d.seller.clone())?;
                d.bond_locked = false;
            }
            let note = if reason.trim().is_empty() { "cancelled".to_string() } else { reason.trim().to_string() };
            transition(st, d, DealStatus::Cancelled, tx, now, &note)
        }

        Action::DeclineDeal { deal_id, reason } => {
            let mut d = st.deal(deal_id)?.clone();
            expect_status(&d, &[DealStatus::Proposed])?;
            let counterparty = if d.proposer == d.seller { d.buyer.clone() } else { d.seller.clone() };
            if from != counterparty {
                return err("only the counterparty can decline a proposal");
            }
            if reason.len() > 500 {
                return err("reason too long");
            }
            if d.bond_locked {
                st.release(&d.seller, d.bond, &d.seller.clone())?;
                d.bond_locked = false;
            }
            let note = if reason.trim().is_empty() { "declined".to_string() } else { reason.trim().to_string() };
            transition(st, d, DealStatus::Declined, tx, now, &note)
        }

        Action::ExpireDeal { deal_id } => {
            let mut d = st.deal(deal_id)?.clone();
            if from != d.buyer && from != d.seller && d.arbiter.as_deref() != Some(from) {
                return err("only a party or the arbiter can enforce a deadline");
            }
            let due = match d.deadline {
                Some(t) => t,
                None => return err(format!("no deadline is running while the deal is {:?}", d.status)),
            };
            if now < due {
                return err(format!("deadline not reached; {}s remaining", due - now));
            }
            match d.status {
                DealStatus::Proposed => {
                    if d.bond_locked {
                        st.release(&d.seller, d.bond, &d.seller.clone())?;
                        d.bond_locked = false;
                    }
                    transition(st, d, DealStatus::Expired, tx, now, "proposal lapsed without acceptance")
                }
                DealStatus::Accepted => {
                    if d.bond_locked {
                        st.release(&d.seller, d.bond, &d.seller.clone())?;
                        d.bond_locked = false;
                    }
                    st.acct_mut(&d.buyer)?.defaults += 1;
                    transition(st, d, DealStatus::Expired, tx, now, "buyer defaulted: escrow never funded")
                }
                DealStatus::Funded => {
                    st.release(&d.buyer, d.amount, &d.buyer.clone())?;
                    let mut note = "seller defaulted: not delivered in time; buyer refunded".to_string();
                    if d.bond_locked {
                        st.release(&d.seller, d.bond, &d.buyer.clone())?;
                        d.bond_locked = false;
                        note.push_str(&format!(", {} DEAL bond slashed to buyer", format_amount(d.bond)));
                    }
                    st.acct_mut(&d.seller)?.defaults += 1;
                    transition(st, d, DealStatus::Failed, tx, now, &note)
                }
                s => err(format!("deadlines do not apply to a deal in status {s:?}")),
            }
        }
    }
}

fn settle_complete(
    st: &mut WorkingState,
    mut d: Deal,
    tx: &SignedTx,
    now: u64,
    note: &str,
) -> Result<TxEffect, ContractError> {
    st.release(&d.buyer, d.amount - d.fee, &d.seller.clone())?;
    st.release(&d.buyer, d.fee, TREASURY_ADDRESS)?;
    if d.bond_locked {
        st.release(&d.seller, d.bond, &d.seller.clone())?;
        d.bond_locked = false;
    }
    for p in [d.buyer.clone(), d.seller.clone()] {
        st.acct_mut(&p)?.deals_completed += 1;
    }
    transition(st, d, DealStatus::Completed, tx, now, note)
}

fn expect_status(d: &Deal, allowed: &[DealStatus]) -> Result<(), ContractError> {
    if allowed.contains(&d.status) {
        Ok(())
    } else {
        err(format!("deal is {:?}; expected one of {:?}", d.status, allowed))
    }
}

/// Steps with a running deadline cannot be taken once it has lapsed —
/// the only way forward is `expire_deal`.
fn expect_in_time(d: &Deal, now: u64) -> Result<(), ContractError> {
    match d.deadline {
        Some(t) if now >= t => err("deadline has passed; the deal can only be expired"),
        _ => Ok(()),
    }
}

fn expect_party(from: &str, who: &str, role: &str) -> Result<(), ContractError> {
    if from == who {
        Ok(())
    } else {
        err(format!("only the {role} can do this"))
    }
}

fn push_event(d: &mut Deal, status: DealStatus, tx: &SignedTx, now: u64, note: &str) {
    d.history.push(DealEvent {
        status,
        by: tx.body.from.clone(),
        tx_hash: tx.hash.clone(),
        at: now,
        note: note.to_string(),
    });
}

fn transition(
    st: &mut WorkingState,
    mut d: Deal,
    to: DealStatus,
    tx: &SignedTx,
    now: u64,
    note: &str,
) -> Result<TxEffect, ContractError> {
    d.status = to;
    d.updated_at = now;
    if to.is_terminal() {
        d.deadline = None;
    }
    push_event(&mut d, to, tx, now, note);
    let id = d.id.clone();
    st.put_deal(d);
    Ok(TxEffect { deal_id: Some(id) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::sign_action;

    struct Actor {
        sk: String,
        addr: String,
    }

    fn actor(seed: u8) -> Actor {
        let sk = hex::encode([seed; 32]);
        let (_, addr) = crate::crypto::keypair_from_secret(&sk).unwrap();
        Actor { sk, addr }
    }

    fn run(st: &mut WorkingState, who: &Actor, action: Action, now: u64) -> Result<TxEffect, ContractError> {
        let tx = sign_action(&who.sk, action, now, now * 1000).unwrap();
        apply_tx(st, &tx, now)
    }

    fn register(st: &mut WorkingState, a: &Actor, kind: PartyKind) {
        run(st, a, Action::Register { name: "x".into(), kind }, 1).unwrap();
    }

    fn total_supply(st: &WorkingState) -> u64 {
        st.accounts.values().map(|a| a.balance + a.escrowed).sum()
    }

    fn setup(seller_kind: PartyKind, buyer_kind: PartyKind) -> (WorkingState, Actor, Actor, Actor) {
        let mut st = WorkingState::default();
        let (s, b, arb) = (actor(1), actor(2), actor(3));
        register(&mut st, &s, seller_kind);
        register(&mut st, &b, buyer_kind);
        register(&mut st, &arb, PartyKind::Business);
        (st, s, b, arb)
    }

    fn create(st: &mut WorkingState, s: &Actor, b: &Actor, arb: Option<&Actor>, price: u64) -> String {
        run(
            st,
            s,
            Action::CreateDeal {
                role: Role::Seller,
                counterparty: b.addr.clone(),
                title: "Widgets".into(),
                description: String::new(),
                items: vec![LineItem { name: "widget".into(), qty: 2, unit_price: price }],
                arbiter: arb.map(|a| a.addr.clone()),
            },
            2,
        )
        .unwrap()
        .deal_id
        .unwrap()
    }

    #[test]
    fn b2b_happy_path_with_bond_and_fee() {
        let (mut st, s, b, arb) = setup(PartyKind::Business, PartyKind::Business);
        let supply = total_supply(&st);
        let id = create(&mut st, &s, &b, Some(&arb), 1000_00);
        let d = &st.deals[&id];
        assert_eq!(d.deal_type, DealType::B2B);
        assert_eq!(d.amount, 2000_00);
        assert_eq!(d.bond, 200_00);
        assert!(d.bond_locked);
        assert_eq!(st.accounts[&s.addr].escrowed, 200_00);

        run(&mut st, &b, Action::AcceptDeal { deal_id: id.clone() }, 3).unwrap();
        run(&mut st, &b, Action::FundDeal { deal_id: id.clone() }, 4).unwrap();
        run(&mut st, &s, Action::MarkShipped { deal_id: id.clone(), tracking: "TRK1".into() }, 5).unwrap();
        run(&mut st, &b, Action::ConfirmReceipt { deal_id: id.clone(), rating: 5 }, 6).unwrap();

        let d = &st.deals[&id];
        assert_eq!(d.status, DealStatus::Completed);
        assert_eq!(d.history.len(), 5);
        let fee = 2000_00 * 50 / 10_000;
        assert_eq!(st.accounts[&s.addr].balance, REGISTER_GRANT + 2000_00 - fee);
        assert_eq!(st.accounts[&s.addr].escrowed, 0);
        assert_eq!(st.accounts[&b.addr].balance, REGISTER_GRANT - 2000_00);
        assert_eq!(st.accounts[TREASURY_ADDRESS].balance, fee);
        assert_eq!(total_supply(&st), supply);
    }

    #[test]
    fn b2c_requires_arbiter_and_allows_withdrawal() {
        let (mut st, s, b, arb) = setup(PartyKind::Business, PartyKind::Consumer);
        let e = run(
            &mut st,
            &s,
            Action::CreateDeal {
                role: Role::Seller,
                counterparty: b.addr.clone(),
                title: "Phone".into(),
                description: String::new(),
                items: vec![LineItem { name: "phone".into(), qty: 1, unit_price: 5 }],
                arbiter: None,
            },
            2,
        );
        assert!(e.unwrap_err().0.contains("require an arbiter"));

        let id = create(&mut st, &s, &b, Some(&arb), 300_00);
        run(&mut st, &b, Action::AcceptDeal { deal_id: id.clone() }, 3).unwrap();
        run(&mut st, &b, Action::FundDeal { deal_id: id.clone() }, 4).unwrap();
        assert_eq!(st.accounts[&b.addr].escrowed, 600_00);
        run(&mut st, &b, Action::CancelDeal { deal_id: id.clone(), reason: "changed mind".into() }, 5).unwrap();
        assert_eq!(st.deals[&id].status, DealStatus::Cancelled);
        assert_eq!(st.accounts[&b.addr].balance, REGISTER_GRANT);
        assert_eq!(st.accounts[&b.addr].escrowed, 0);
    }

    #[test]
    fn c2c_buyer_cannot_withdraw_and_seller_can_claim_after_window() {
        let (mut st, s, b, _) = setup(PartyKind::Consumer, PartyKind::Consumer);
        let id = create(&mut st, &s, &b, None, 10_00);
        run(&mut st, &b, Action::AcceptDeal { deal_id: id.clone() }, 3).unwrap();
        run(&mut st, &b, Action::FundDeal { deal_id: id.clone() }, 4).unwrap();
        assert!(run(&mut st, &b, Action::CancelDeal { deal_id: id.clone(), reason: String::new() }, 5).is_err());
        assert!(run(&mut st, &b, Action::OpenDispute { deal_id: id.clone(), reason: "x".into() }, 5).is_err());
        run(&mut st, &s, Action::MarkShipped { deal_id: id.clone(), tracking: String::new() }, 10).unwrap();
        let early = run(&mut st, &s, Action::ClaimRelease { deal_id: id.clone() }, 11);
        assert!(early.unwrap_err().0.contains("still open"));
        run(&mut st, &s, Action::ClaimRelease { deal_id: id.clone() }, 10 + 120).unwrap();
        assert_eq!(st.deals[&id].status, DealStatus::Completed);
    }

    #[test]
    fn dispute_resolution_splits_escrow_and_slashes_bond() {
        let (mut st, s, b, arb) = setup(PartyKind::Business, PartyKind::Business);
        let supply = total_supply(&st);
        let id = create(&mut st, &s, &b, Some(&arb), 500_00);
        run(&mut st, &b, Action::AcceptDeal { deal_id: id.clone() }, 3).unwrap();
        run(&mut st, &b, Action::FundDeal { deal_id: id.clone() }, 4).unwrap();
        run(&mut st, &b, Action::OpenDispute { deal_id: id.clone(), reason: "never arrived".into() }, 5).unwrap();
        assert!(run(&mut st, &b, Action::ResolveDispute { deal_id: id.clone(), buyer_refund_bps: 10_000, note: String::new() }, 6).is_err());
        run(&mut st, &arb, Action::ResolveDispute { deal_id: id.clone(), buyer_refund_bps: 7_500, note: "partial".into() }, 6).unwrap();

        let amount = 1000_00u64;
        let refund = amount * 3 / 4;
        let seller_part = amount - refund;
        let fee = seller_part * 50 / 10_000;
        let bond = 100_00;
        assert_eq!(st.accounts[&b.addr].balance, REGISTER_GRANT - amount + refund + bond);
        assert_eq!(st.accounts[&s.addr].balance, REGISTER_GRANT - bond + seller_part - fee);
        assert_eq!(st.accounts[&s.addr].escrowed + st.accounts[&b.addr].escrowed, 0);
        assert_eq!(total_supply(&st), supply);
    }

    #[test]
    fn decline_refunds_bond_and_only_counterparty_may_decline() {
        let (mut st, s, b, arb) = setup(PartyKind::Business, PartyKind::Business);
        let id = create(&mut st, &s, &b, Some(&arb), 100_00);
        assert_eq!(st.accounts[&s.addr].escrowed, 20_00);
        assert!(run(&mut st, &s, Action::DeclineDeal { deal_id: id.clone(), reason: String::new() }, 3).is_err());
        run(&mut st, &b, Action::DeclineDeal { deal_id: id.clone(), reason: "price too high".into() }, 3).unwrap();
        assert_eq!(st.deals[&id].status, DealStatus::Declined);
        assert_eq!(st.accounts[&s.addr].escrowed, 0);
        assert_eq!(st.accounts[&s.addr].balance, REGISTER_GRANT);
    }

    #[test]
    fn unfunded_deal_expires_as_buyer_default() {
        let (mut st, s, b, _) = setup(PartyKind::Consumer, PartyKind::Consumer);
        let id = create(&mut st, &s, &b, None, 10_00);
        run(&mut st, &b, Action::AcceptDeal { deal_id: id.clone() }, 3).unwrap();
        let due = st.deals[&id].deadline.unwrap();
        assert_eq!(due, 3 + 180);
        let early = run(&mut st, &s, Action::ExpireDeal { deal_id: id.clone() }, due - 1);
        assert!(early.unwrap_err().0.contains("not reached"));
        let late = run(&mut st, &b, Action::FundDeal { deal_id: id.clone() }, due);
        assert!(late.unwrap_err().0.contains("deadline has passed"));
        run(&mut st, &s, Action::ExpireDeal { deal_id: id.clone() }, due).unwrap();
        assert_eq!(st.deals[&id].status, DealStatus::Expired);
        assert_eq!(st.deals[&id].deadline, None);
        assert_eq!(st.accounts[&b.addr].defaults, 1);
    }

    #[test]
    fn undelivered_deal_fails_with_refund_and_bond_slash() {
        let (mut st, s, b, arb) = setup(PartyKind::Business, PartyKind::Business);
        let supply = total_supply(&st);
        let id = create(&mut st, &s, &b, Some(&arb), 500_00);
        run(&mut st, &b, Action::AcceptDeal { deal_id: id.clone() }, 3).unwrap();
        run(&mut st, &b, Action::FundDeal { deal_id: id.clone() }, 4).unwrap();
        let due = st.deals[&id].deadline.unwrap();
        run(&mut st, &arb, Action::ExpireDeal { deal_id: id.clone() }, due + 5).unwrap();
        let d = &st.deals[&id];
        assert_eq!(d.status, DealStatus::Failed);
        assert!(!d.bond_locked);
        assert_eq!(st.accounts[&b.addr].balance, REGISTER_GRANT + 100_00);
        assert_eq!(st.accounts[&s.addr].balance, REGISTER_GRANT - 100_00);
        assert_eq!(st.accounts[&s.addr].defaults, 1);
        assert_eq!(total_supply(&st), supply);
        // Shipping in time stops the clock.
        let id2 = create(&mut st, &s, &b, Some(&arb), 10_00);
        run(&mut st, &b, Action::AcceptDeal { deal_id: id2.clone() }, 10).unwrap();
        run(&mut st, &b, Action::FundDeal { deal_id: id2.clone() }, 11).unwrap();
        run(&mut st, &s, Action::MarkShipped { deal_id: id2.clone(), tracking: String::new() }, 12).unwrap();
        assert!(run(&mut st, &b, Action::ExpireDeal { deal_id: id2 }, 10_000).is_err());
    }

    #[test]
    fn failed_tx_leaves_state_untouched() {
        let (mut st, s, b, arb) = setup(PartyKind::Business, PartyKind::Business);
        let before = (st.accounts.clone(), st.deals.clone());
        let r = run(
            &mut st,
            &s,
            Action::CreateDeal {
                role: Role::Seller,
                counterparty: b.addr.clone(),
                title: "Too big".into(),
                description: String::new(),
                items: vec![LineItem { name: "x".into(), qty: 1, unit_price: REGISTER_GRANT * 20 }],
                arbiter: Some(arb.addr.clone()),
            },
            2,
        );
        assert!(r.unwrap_err().0.contains("insufficient"));
        assert_eq!((st.accounts, st.deals), before);
    }
}
