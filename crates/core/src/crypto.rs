use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::types::{Action, BlockHeader, SignedTx, TxBody};

/// Transactions older or further in the future than this are rejected.
pub const TX_MAX_CLOCK_SKEW_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("malformed hex in {0}")]
    BadHex(&'static str),
    #[error("invalid public key")]
    BadPublicKey,
    #[error("invalid secret key")]
    BadSecretKey,
    #[error("sender address does not match public key")]
    AddressMismatch,
    #[error("transaction hash does not match body")]
    HashMismatch,
    #[error("signature verification failed")]
    BadSignature,
    #[error("transaction timestamp outside of the accepted window")]
    StaleTimestamp,
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha256(data))
}

/// Address = `0x` + first 20 bytes of sha256(pubkey), hex-encoded.
pub fn address_from_pubkey(pubkey: &[u8]) -> String {
    format!("0x{}", hex::encode(&sha256(pubkey)[..20]))
}

pub fn is_address(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Canonical bytes of a transaction body (serde_json with fixed field order).
pub fn canonical_body(body: &TxBody) -> Vec<u8> {
    serde_json::to_vec(body).expect("TxBody is always serializable")
}

pub fn tx_hash(body: &TxBody) -> String {
    sha256_hex(&canonical_body(body))
}

/// Derives the public key (hex) and address for a 32-byte secret key.
pub fn keypair_from_secret(secret_hex: &str) -> Result<(String, String), CryptoError> {
    let key = signing_key(secret_hex)?;
    let pk = key.verifying_key().to_bytes();
    Ok((hex::encode(pk), address_from_pubkey(&pk)))
}

fn signing_key(secret_hex: &str) -> Result<SigningKey, CryptoError> {
    let bytes: [u8; 32] = hex::decode(secret_hex)
        .map_err(|_| CryptoError::BadHex("secret"))?
        .try_into()
        .map_err(|_| CryptoError::BadSecretKey)?;
    Ok(SigningKey::from_bytes(&bytes))
}

/// Builds and signs a transaction. `from`/`pubkey` are derived from the key.
pub fn sign_action(
    secret_hex: &str,
    action: Action,
    nonce: u64,
    timestamp_ms: u64,
) -> Result<SignedTx, CryptoError> {
    let key = signing_key(secret_hex)?;
    let pk = key.verifying_key().to_bytes();
    let body = TxBody {
        from: address_from_pubkey(&pk),
        pubkey: hex::encode(pk),
        nonce,
        timestamp: timestamp_ms,
        action,
    };
    let hash = tx_hash(&body);
    let sig = key.sign(hash.as_bytes());
    Ok(SignedTx {
        hash,
        body,
        signature: hex::encode(sig.to_bytes()),
    })
}

/// Checks hash integrity, address/public-key binding and the signature.
pub fn verify_tx(tx: &SignedTx) -> Result<(), CryptoError> {
    if tx_hash(&tx.body) != tx.hash {
        return Err(CryptoError::HashMismatch);
    }
    let pk: [u8; 32] = hex::decode(&tx.body.pubkey)
        .map_err(|_| CryptoError::BadHex("pubkey"))?
        .try_into()
        .map_err(|_| CryptoError::BadPublicKey)?;
    if address_from_pubkey(&pk) != tx.body.from {
        return Err(CryptoError::AddressMismatch);
    }
    let vk = VerifyingKey::from_bytes(&pk).map_err(|_| CryptoError::BadPublicKey)?;
    let sig: [u8; 64] = hex::decode(&tx.signature)
        .map_err(|_| CryptoError::BadHex("signature"))?
        .try_into()
        .map_err(|_| CryptoError::BadSignature)?;
    vk.verify(tx.hash.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| CryptoError::BadSignature)
}

/// `verify_tx` plus a freshness check against the node clock.
pub fn verify_tx_at(tx: &SignedTx, now_ms: u64) -> Result<(), CryptoError> {
    verify_tx(tx)?;
    if tx.body.timestamp.abs_diff(now_ms) > TX_MAX_CLOCK_SKEW_MS {
        return Err(CryptoError::StaleTimestamp);
    }
    Ok(())
}

/// Binary Merkle root over hex tx hashes (last node duplicated on odd levels).
pub fn merkle_root(hashes: &[String]) -> String {
    if hashes.is_empty() {
        return sha256_hex(b"");
    }
    let mut level: Vec<[u8; 32]> = hashes
        .iter()
        .map(|h| {
            hex::decode(h)
                .ok()
                .and_then(|b| b.try_into().ok())
                .unwrap_or_else(|| sha256(h.as_bytes()))
        })
        .collect();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| {
                let mut h = Sha256::new();
                h.update(pair[0]);
                h.update(pair.get(1).unwrap_or(&pair[0]));
                h.finalize().into()
            })
            .collect();
    }
    hex::encode(level[0])
}

pub fn block_hash(header: &BlockHeader) -> String {
    sha256_hex(&serde_json::to_vec(header).expect("header is serializable"))
}

pub fn meets_difficulty(hash: &str, difficulty: u32) -> bool {
    hash.bytes().take(difficulty as usize).all(|b| b == b'0')
        && hash.len() >= difficulty as usize
}

/// Proof-of-work: increments `header.nonce` until the hash meets the difficulty.
pub fn mine(header: &mut BlockHeader) -> String {
    loop {
        let h = block_hash(header);
        if meets_difficulty(&h, header.difficulty) {
            return h;
        }
        header.nonce = header.nonce.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PartyKind;

    const SK: &str = "0101010101010101010101010101010101010101010101010101010101010101";

    #[test]
    fn sign_and_verify_roundtrip() {
        let tx = sign_action(
            SK,
            Action::Register {
                name: "Alice".into(),
                kind: PartyKind::Consumer,
            },
            7,
            1_000,
        )
        .unwrap();
        assert!(is_address(&tx.body.from));
        verify_tx(&tx).unwrap();
        assert_eq!(verify_tx_at(&tx, 1_000 + TX_MAX_CLOCK_SKEW_MS + 1), Err(CryptoError::StaleTimestamp));
    }

    #[test]
    fn tampering_is_detected() {
        let mut tx = sign_action(
            SK,
            Action::Transfer { to: "0x00".into(), amount: 5, memo: String::new() },
            1,
            1,
        )
        .unwrap();
        tx.body.action = Action::Transfer { to: "0x00".into(), amount: 500, memo: String::new() };
        assert_eq!(verify_tx(&tx), Err(CryptoError::HashMismatch));
        tx.hash = tx_hash(&tx.body);
        assert_eq!(verify_tx(&tx), Err(CryptoError::BadSignature));
    }

    #[test]
    fn json_roundtrip_keeps_hash() {
        let tx = sign_action(SK, Action::AcceptDeal { deal_id: "D-1".into() }, 3, 3).unwrap();
        let back: SignedTx = serde_json::from_str(&serde_json::to_string(&tx).unwrap()).unwrap();
        verify_tx(&back).unwrap();
    }

    #[test]
    fn merkle_and_pow() {
        let a = sha256_hex(b"a");
        let b = sha256_hex(b"b");
        assert_ne!(merkle_root(&[a.clone(), b.clone()]), merkle_root(&[b, a.clone()]));
        assert_eq!(merkle_root(std::slice::from_ref(&a)).len(), 64);
        let mut h = BlockHeader {
            height: 1,
            prev_hash: "0".repeat(64),
            timestamp: 1,
            merkle_root: merkle_root(&[a]),
            tx_count: 1,
            difficulty: 2,
            nonce: 0,
            producer: "test".into(),
        };
        let hash = mine(&mut h);
        assert!(hash.starts_with("00"));
        assert_eq!(block_hash(&h), hash);
    }
}
