//! Browser wallet: key generation, transaction signing and independent
//! verification of blocks, all running the exact same Rust code as the node.

use chaindeal_core as core;
use chaindeal_core::{Action, Block, DealType, LineItem, PartyKind, SignedTx};
use ed25519_dalek::SigningKey;
use rand_core::{OsRng, RngCore};
use wasm_bindgen::prelude::*;

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<String, JsError> {
    serde_json::to_string(v).map_err(js_err)
}

fn wallet_json(secret_hex: &str) -> Result<String, JsError> {
    let (pubkey, address) = core::keypair_from_secret(secret_hex).map_err(js_err)?;
    Ok(serde_json::json!({ "secret": secret_hex, "pubkey": pubkey, "address": address }).to_string())
}

/// Creates a new ed25519 keypair. Returns `{secret, pubkey, address}` as JSON.
#[wasm_bindgen(js_name = generateWallet)]
pub fn generate_wallet() -> Result<String, JsError> {
    let key = SigningKey::generate(&mut OsRng);
    wallet_json(&hex::encode(key.to_bytes()))
}

/// Restores `{secret, pubkey, address}` from a hex secret key.
#[wasm_bindgen(js_name = walletFromSecret)]
pub fn wallet_from_secret(secret_hex: &str) -> Result<String, JsError> {
    wallet_json(secret_hex.trim())
}

/// Signs an action (JSON, e.g. `{"type":"fund_deal","deal_id":"D-..."}`).
/// Returns the signed transaction JSON ready to POST to `/api/tx`.
#[wasm_bindgen(js_name = signAction)]
pub fn sign_action(secret_hex: &str, action_json: &str, now_ms: f64) -> Result<String, JsError> {
    let action: Action = serde_json::from_str(action_json).map_err(js_err)?;
    let nonce = OsRng.next_u64() >> 11; // stays within JS safe-integer range
    let tx = core::sign_action(secret_hex, action, nonce, now_ms as u64).map_err(js_err)?;
    to_json(&tx)
}

/// Verifies a transaction signature. Returns an empty string when valid,
/// otherwise the reason.
#[wasm_bindgen(js_name = verifyTx)]
pub fn verify_tx(tx_json: &str) -> String {
    match serde_json::from_str::<SignedTx>(tx_json) {
        Ok(tx) => core::verify_tx(&tx).err().map(|e| e.to_string()).unwrap_or_default(),
        Err(e) => format!("malformed transaction: {e}"),
    }
}

/// Recomputes a block's hash, proof-of-work and Merkle root locally.
/// Returns an empty string when the block is internally consistent.
#[wasm_bindgen(js_name = verifyBlock)]
pub fn verify_block(block_json: &str) -> String {
    let b: Block = match serde_json::from_str(block_json) {
        Ok(b) => b,
        Err(e) => return format!("malformed block: {e}"),
    };
    if core::block_hash(&b.header) != b.hash {
        return "hash does not match header".into();
    }
    if !core::meets_difficulty(&b.hash, b.header.difficulty) {
        return "proof-of-work does not meet difficulty".into();
    }
    if core::merkle_root(&b.tx_hashes) != b.header.merkle_root {
        return "merkle root does not match transactions".into();
    }
    String::new()
}

#[wasm_bindgen(js_name = classifyDeal)]
pub fn classify_deal(seller_kind: &str, buyer_kind: &str) -> Result<String, JsError> {
    let parse = |k: &str| -> Result<PartyKind, JsError> {
        serde_json::from_value(serde_json::Value::String(k.into())).map_err(js_err)
    };
    let t = DealType::classify(parse(seller_kind)?, parse(buyer_kind)?);
    Ok(format!("{t:?}"))
}

/// Previews the economics of a deal using the on-chain policy.
/// Returns `{deal_type, amount, fee, bond, seller_payout, policy}`.
#[wasm_bindgen(js_name = quoteDeal)]
pub fn quote_deal(deal_type: &str, items_json: &str) -> Result<String, JsError> {
    let t: DealType = serde_json::from_value(serde_json::Value::String(deal_type.into())).map_err(js_err)?;
    let items: Vec<LineItem> = serde_json::from_str(items_json).map_err(js_err)?;
    let amount: u64 = items.iter().map(|i| i.qty as u64 * i.unit_price).sum();
    let p = core::policy_for(t);
    let fee = (amount as u128 * p.fee_bps as u128 / 10_000) as u64;
    let bond = (amount as u128 * p.seller_bond_bps as u128 / 10_000) as u64;
    to_json(&serde_json::json!({
        "deal_type": t,
        "amount": amount,
        "fee": fee,
        "bond": bond,
        "seller_payout": amount - fee,
        "policy": p,
    }))
}

#[wasm_bindgen]
pub fn policies() -> Result<String, JsError> {
    to_json(&core::contract::POLICIES)
}

/// Signs an account-link challenge (proof that this browser holds the key).
/// The message is domain-separated, so the signature can't double as a
/// transaction signature. Returns `{pubkey, signature}` as JSON.
#[wasm_bindgen(js_name = signLinkChallenge)]
pub fn sign_link_challenge(secret_hex: &str, challenge_message: &str) -> Result<String, JsError> {
    use ed25519_dalek::Signer;
    if !challenge_message.starts_with(&chaindeal_authn::wallet_link_message("")) {
        return Err(JsError::new("refusing to sign: not a ChainDeal account-link challenge"));
    }
    let bytes: [u8; 32] = hex::decode(secret_hex.trim())
        .map_err(js_err)?
        .try_into()
        .map_err(|_| JsError::new("secret must be 32 bytes"))?;
    let key = SigningKey::from_bytes(&bytes);
    let sig = key.sign(challenge_message.as_bytes());
    Ok(serde_json::json!({ "pubkey": hex::encode(key.verifying_key().to_bytes()), "signature": hex::encode(sig.to_bytes()) }).to_string())
}

#[wasm_bindgen(js_name = isAddress)]
pub fn is_address(s: &str) -> bool {
    core::is_address(s)
}
