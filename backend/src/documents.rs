//! Contract documents: issuance and verification of attested deal records.
//! See `chaindeal_core::document` for the record format and the trust model.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use chaindeal_core::document::*;
use chaindeal_core::{block_hash, Block, TxRecord};
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::OnceCell;

use crate::db::Db;

pub struct Documents {
    key: SigningKey,
    pub key_id: String,
    pub public_key: String,
    chain_id: OnceCell<String>,
}

#[derive(Deserialize)]
struct BlockWithTxs {
    block: Block,
}

impl Documents {
    /// `seed_hex`: 32-byte Ed25519 seed shared by every node of this market.
    pub fn new(seed_hex: Option<String>) -> Result<Self> {
        let seed: [u8; 32] = match seed_hex {
            Some(h) => hex::decode(h.trim())?.try_into().map_err(|_| anyhow!("attestation key must be 32 bytes"))?,
            None => {
                tracing::warn!("CHAINDEAL_ATTESTATION_KEY not set: using a random per-process key (documents won't verify across nodes or restarts)");
                rand::random()
            }
        };
        let key = SigningKey::from_bytes(&seed);
        let pk = key.verifying_key().to_bytes();
        Ok(Documents { key_id: attestation_key_id(&pk), public_key: hex::encode(pk), key, chain_id: OnceCell::new() })
    }

    /// Genesis hash: the identity of this chain.
    pub async fn chain_id(&self, db: &Db) -> Result<String> {
        self.chain_id
            .get_or_try_init(|| async {
                let v = db.call("cd_get_block", vec![rmpv::Value::from(0u64)]).await?;
                let b: BlockWithTxs = serde_json::from_value(v).map_err(|e| anyhow!("genesis: {e}"))?;
                Ok::<_, anyhow::Error>(b.block.hash)
            })
            .await
            .cloned()
    }

    /// Rebuilds a deal's record from chain state (deal, parties, event proofs).
    pub async fn build(&self, db: &Db, deal_id: &str) -> Result<Option<DealRecord>> {
        let Some(deal) = db.get_deals(&[deal_id.to_string()]).await?.into_iter().next() else { return Ok(None) };
        let mut addrs = vec![deal.seller.clone(), deal.buyer.clone()];
        addrs.extend(deal.arbiter.clone());
        let accounts: HashMap<String, _> = db.get_accounts(&addrs).await?.into_iter().map(|a| (a.address.clone(), a)).collect();
        let party = |a: &str| -> Result<PartyRecord> {
            let acc = accounts.get(a).ok_or_else(|| anyhow!("account {a} missing"))?;
            Ok(PartyRecord { address: a.to_string(), name: acc.name.clone(), kind: acc.kind })
        };

        let mut blocks: HashMap<u64, Block> = HashMap::new();
        let mut events = Vec::with_capacity(deal.history.len());
        for e in &deal.history {
            let tx: TxRecord = serde_json::from_value(db.call("cd_get_tx", vec![rmpv::Value::from(e.tx_hash.clone())]).await?)
                .map_err(|err| anyhow!("tx {}: {err}", e.tx_hash))?;
            let height = tx.block_height.ok_or_else(|| anyhow!("tx {} not yet in a block", e.tx_hash))?;
            if let std::collections::hash_map::Entry::Vacant(slot) = blocks.entry(height) {
                let v = db.call("cd_get_block", vec![rmpv::Value::from(height)]).await?;
                let b: BlockWithTxs = serde_json::from_value(v).map_err(|err| anyhow!("block {height}: {err}"))?;
                slot.insert(b.block);
            }
            let b = &blocks[&height];
            let index = b.tx_hashes.iter().position(|h| h == &e.tx_hash).ok_or_else(|| anyhow!("tx not committed by block {height}"))?;
            events.push(EventRecord {
                status: e.status,
                by: e.by.clone(),
                at: e.at,
                note: e.note.clone(),
                tx_hash: e.tx_hash.clone(),
                block_height: height,
                block_hash: b.hash.clone(),
                merkle_root: b.header.merkle_root.clone(),
                proof: merkle_proof(&b.tx_hashes, index),
            });
        }

        Ok(Some(DealRecord {
            version: RECORD_VERSION,
            chain_id: self.chain_id(db).await?,
            deal_id: deal.id.clone(),
            deal_type: deal.deal_type,
            title: deal.title.clone(),
            description: deal.description.clone(),
            seller: party(&deal.seller)?,
            buyer: party(&deal.buyer)?,
            arbiter: deal.arbiter.as_deref().map(party).transpose()?,
            proposer: deal.proposer.clone(),
            items: deal.items.clone(),
            status: deal.status,
            is_final: deal.status.is_terminal(),
            created_at: deal.created_at,
            updated_at: deal.updated_at,
            tracking: deal.tracking.clone(),
            buyer_refund_bps: deal.buyer_refund_bps,
            dispute_reason: deal.dispute_reason.clone(),
            settlement: settlement_of(&deal),
            events,
        }))
    }

    /// Issues the attested document payload for a deal.
    pub async fn issue(&self, db: &Db, deal_id: &str) -> Result<Option<Value>> {
        let Some(record) = self.build(db, deal_id).await? else { return Ok(None) };
        let hash = record_hash(&record);
        let signature = attest(&self.key, &record.chain_id, &hash);
        Ok(Some(json!({
            "number": document_number(&hash),
            "hash": hash,
            "record": record,
            "attestation": { "alg": "Ed25519", "key_id": self.key_id, "public_key": self.public_key, "signature": signature },
            "verify_path": format!("/#/verify?deal={}&hash={}&sig={}", record.deal_id, hash, signature),
        })))
    }

    /// Verifies a presented document (deal id + record hash + market signature)
    /// against this market's chain and key.
    pub async fn verify(&self, db: &Db, deal_id: &str, hash: &str, sig: &str) -> Result<Value> {
        let mut checks = Vec::new();
        let mut push = |name: &str, ok: bool, detail: String| checks.push(json!({ "name": name, "ok": ok, "detail": detail }));
        let chain_id = self.chain_id(db).await?;

        let sig_ok = verify_attestation(&self.public_key, &chain_id, hash, sig);
        push("Issued by this market", sig_ok, if sig_ok {
            format!("Signature verifies with market key {} for chain {}…", self.key_id, &chain_id[..16])
        } else {
            "The signature was not made by this market's attestation key for this chain, or the hash was altered.".into()
        });

        let Some(record) = self.build(db, deal_id).await? else {
            push("Deal exists on this chain", false, format!("No deal {deal_id} on this chain."));
            return Ok(json!({ "valid": false, "checks": checks }));
        };
        let current = record_hash(&record);
        let matches = current == hash;
        push("Content matches the chain", matches, if matches {
            "Rebuilding the record from the chain gives exactly the hash printed on the document.".into()
        } else if !record.is_final {
            "The deal has progressed since this document was issued; request a fresh copy.".into()
        } else {
            "The document's hash differs from the chain's record: the document was altered or is not from this deal.".into()
        });

        let mut anchored = 0;
        for e in &record.events {
            let proof_ok = verify_merkle_proof(&e.tx_hash, &e.proof, &e.merkle_root);
            let v = db.call("cd_get_block", vec![rmpv::Value::from(e.block_height)]).await?;
            let header_ok = serde_json::from_value::<BlockWithTxs>(v)
                .map(|b| block_hash(&b.block.header) == e.block_hash && b.block.header.merkle_root == e.merkle_root)
                .unwrap_or(false);
            if proof_ok && header_ok {
                anchored += 1;
            }
        }
        let all = anchored == record.events.len();
        push("Every event is sealed in a block", all, format!("{anchored} of {} events proven by Merkle inclusion in blocks whose hashes re-derive.", record.events.len()));

        Ok(json!({
            "valid": sig_ok && matches && all,
            "final": record.is_final,
            "number": document_number(hash),
            "current_number": document_number(&current),
            "status": record.status,
            "checks": checks,
        }))
    }
}
