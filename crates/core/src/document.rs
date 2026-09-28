//! Deal records: the data behind a ChainDeal contract document.
//!
//! A *deal record* is the canonical, language-independent content of a deal's
//! contract: parties, terms, outcome, and every lifecycle event together with
//! the block that sealed it and a Merkle inclusion proof. Its SHA-256 is the
//! document number, so the Russian and US renderings of one deal share it.
//!
//! The market attests a record by signing its hash (plus this chain's genesis
//! hash) with a key only the market holds. Verification rebuilds the record
//! from the chain and checks hash, signature and proofs — which is why a
//! document can only be verified by the market (and chain) that issued it.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::crypto::{sha256, sha256_hex};
use crate::types::{Amount, Deal, DealStatus, PartyKind};

pub const RECORD_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PartyRecord {
    pub address: String,
    pub name: String,
    pub kind: PartyKind,
}

/// One step of a Merkle inclusion proof: the sibling hash and which side it is on.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProofStep {
    pub sibling: String,
    /// True when the sibling is the *left* input of the parent hash.
    pub left: bool,
}

/// A lifecycle event, anchored to the block that sealed its transaction.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EventRecord {
    pub status: DealStatus,
    pub by: String,
    pub at: u64,
    pub note: String,
    pub tx_hash: String,
    pub block_height: u64,
    pub block_hash: String,
    pub merkle_root: String,
    pub proof: Vec<ProofStep>,
}

/// Money movement at the time of the record, in minor units.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Settlement {
    pub amount: Amount,
    pub fee: Amount,
    pub bond: Amount,
    pub to_seller: Amount,
    pub to_buyer: Amount,
    pub bond_to: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DealRecord {
    pub version: u32,
    /// Genesis block hash: identifies the chain (and so the market) of record.
    pub chain_id: String,
    pub deal_id: String,
    pub deal_type: crate::types::DealType,
    pub title: String,
    pub description: String,
    pub seller: PartyRecord,
    pub buyer: PartyRecord,
    pub arbiter: Option<PartyRecord>,
    pub proposer: String,
    pub items: Vec<crate::types::LineItem>,
    pub status: DealStatus,
    /// Terminal deals produce a final, immutable record.
    pub is_final: bool,
    pub created_at: u64,
    pub updated_at: u64,
    pub tracking: Option<String>,
    pub buyer_refund_bps: Option<u16>,
    pub dispute_reason: Option<String>,
    pub settlement: Settlement,
    pub events: Vec<EventRecord>,
}

/// Canonical hash of a record (serde field order is the canonical encoding).
pub fn record_hash(r: &DealRecord) -> String {
    sha256_hex(&serde_json::to_vec(r).expect("record serializes"))
}

/// Human-readable document number: `CD-` + first 16 hex digits of the hash, grouped.
pub fn document_number(hash: &str) -> String {
    let h = hash.to_uppercase();
    format!("CD-{}-{}-{}-{}", &h[0..4], &h[4..8], &h[8..12], &h[12..16])
}

/// Derives the settlement figures from a deal's final state.
pub fn settlement_of(d: &Deal) -> Settlement {
    use DealStatus::*;
    let (to_seller, to_buyer, bond_to) = match d.status {
        Completed => (d.amount - d.fee, 0, (d.bond > 0).then(|| "seller".to_string())),
        Resolved => {
            let refund = ((d.amount as u128 * d.buyer_refund_bps.unwrap_or(0) as u128) / 10_000) as Amount;
            let bond_to = (d.bond > 0).then(|| if d.buyer_refund_bps.unwrap_or(0) > 5_000 { "buyer" } else { "seller" }.to_string());
            (d.amount - refund - d.fee, refund, bond_to)
        }
        Failed => (0, d.amount, (d.bond > 0).then(|| "buyer".to_string())),
        Cancelled if d.history.iter().any(|e| e.status == Funded) => (0, d.amount, (d.bond > 0 && d.history.iter().any(|e| e.status == Accepted)).then(|| "seller".to_string())),
        _ => (0, 0, None),
    };
    Settlement { amount: d.amount, fee: if matches!(d.status, Completed | Resolved) { d.fee } else { 0 }, bond: d.bond, to_seller, to_buyer, bond_to }
}

// ---- Merkle inclusion proofs (mirrors crypto::merkle_root exactly) -----------

fn leaf(h: &str) -> [u8; 32] {
    hex::decode(h).ok().and_then(|b| b.try_into().ok()).unwrap_or_else(|| sha256(h.as_bytes()))
}

fn parent(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(a);
    h.update(b);
    h.finalize().into()
}

/// Proof that `hashes[index]` is included under `merkle_root(hashes)`.
pub fn merkle_proof(hashes: &[String], index: usize) -> Vec<ProofStep> {
    let mut level: Vec<[u8; 32]> = hashes.iter().map(|h| leaf(h)).collect();
    let mut i = index;
    let mut proof = Vec::new();
    while level.len() > 1 {
        let (sibling, left) = if i % 2 == 1 { (level[i - 1], true) } else { (*level.get(i + 1).unwrap_or(&level[i]), false) };
        proof.push(ProofStep { sibling: hex::encode(sibling), left });
        level = level.chunks(2).map(|p| parent(&p[0], p.get(1).unwrap_or(&p[0]))).collect();
        i /= 2;
    }
    proof
}

pub fn verify_merkle_proof(tx_hash: &str, proof: &[ProofStep], root: &str) -> bool {
    let mut acc = leaf(tx_hash);
    for s in proof {
        let Some(sib) = hex::decode(&s.sibling).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()) else { return false };
        acc = if s.left { parent(&sib, &acc) } else { parent(&acc, &sib) };
    }
    hex::encode(acc) == root
}

// ---- market attestation ---------------------------------------------------------

/// Domain-separated message the market signs for a record.
pub fn attestation_message(chain_id: &str, record_hash: &str) -> String {
    format!("ChainDeal document attestation v{RECORD_VERSION}\nchain: {chain_id}\nrecord: {record_hash}")
}

pub fn attestation_key_id(pubkey: &[u8; 32]) -> String {
    hex::encode(&sha256(pubkey)[..8])
}

pub fn attest(key: &SigningKey, chain_id: &str, record_hash: &str) -> String {
    hex::encode(key.sign(attestation_message(chain_id, record_hash).as_bytes()).to_bytes())
}

pub fn verify_attestation(pubkey_hex: &str, chain_id: &str, record_hash: &str, sig_hex: &str) -> bool {
    let Some(pk) = hex::decode(pubkey_hex).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()) else { return false };
    let Some(sig) = hex::decode(sig_hex).ok().and_then(|b| <[u8; 64]>::try_from(b).ok()) else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else { return false };
    vk.verify(attestation_message(chain_id, record_hash).as_bytes(), &Signature::from_bytes(&sig)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::merkle_root;

    #[test]
    fn merkle_proofs_match_root_for_every_size_and_index() {
        for n in 1..=9 {
            let hashes: Vec<String> = (0..n).map(|i| sha256_hex(format!("tx{i}").as_bytes())).collect();
            let root = merkle_root(&hashes);
            for (i, h) in hashes.iter().enumerate() {
                let p = merkle_proof(&hashes, i);
                assert!(verify_merkle_proof(h, &p, &root), "n={n} i={i}");
                assert!(!verify_merkle_proof(&sha256_hex(b"other"), &p, &root));
            }
        }
    }

    #[test]
    fn attestation_binds_chain_and_record() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let pk = hex::encode(key.verifying_key().to_bytes());
        let sig = attest(&key, "chainA", "abc");
        assert!(verify_attestation(&pk, "chainA", "abc", &sig));
        assert!(!verify_attestation(&pk, "chainB", "abc", &sig), "another chain's documents are not valid here");
        assert!(!verify_attestation(&pk, "chainA", "abd", &sig), "an altered record is not valid");
        assert_eq!(document_number("0123456789abcdef00"), "CD-0123-4567-89AB-CDEF");
    }
}
