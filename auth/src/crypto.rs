//! Secret handling for the auth service. Nothing sensitive is stored in the
//! clear:
//! - passwords: Argon2id (OWASP parameters), verified in constant time
//! - emails: AES-256-GCM, bound to the owning record with associated data;
//!   looked up through an HMAC-SHA256 blind index
//! - codes, refresh tokens, sessions: only SHA-256 hashes are stored

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD as B64};
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// 256-bit random value, base64url (43 chars). Used for codes, tokens, ids.
pub fn random_token() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    B64.encode(b)
}

pub fn random_id() -> String {
    let mut b = [0u8; 16];
    OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

/// Storage key for a bearer secret: the secret itself never reaches the DB.
pub fn hash_secret(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// RFC 7636 S256: BASE64URL(SHA256(verifier)) == challenge.
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let ok_len = (43..=128).contains(&verifier.len());
    let ok_chars = verifier.bytes().all(|c| c.is_ascii_alphanumeric() || b"-._~".contains(&c));
    ok_len && ok_chars && ct_eq(&B64.encode(Sha256::digest(verifier.as_bytes())), challenge)
}

fn argon() -> Argon2<'static> {
    // OWASP 2024+: Argon2id, m = 19 MiB, t = 2, p = 1.
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(19_456, 2, 1, None).expect("params"))
}

pub fn hash_password(pw: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    argon().hash_password(pw.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| anyhow!("hash: {e}"))
}

pub fn verify_password(pw: &str, phc: &str) -> bool {
    PasswordHash::new(phc).map(|h| argon().verify_password(pw.as_bytes(), &h).is_ok()).unwrap_or(false)
}

/// A valid hash of a random password, verified against when the account
/// doesn't exist so that response timing doesn't reveal registered emails.
pub fn dummy_hash() -> String {
    hash_password(&random_token()).expect("dummy hash")
}

pub struct DataKeys {
    aead: Aes256Gcm,
    index: Vec<u8>,
}

impl DataKeys {
    pub fn new(data_key_hex: &str, index_key_hex: &str) -> Result<Self> {
        let dk = hex::decode(data_key_hex.trim())?;
        let ik = hex::decode(index_key_hex.trim())?;
        if dk.len() != 32 || ik.len() != 32 {
            return Err(anyhow!("AUTH_DATA_KEY and AUTH_INDEX_KEY must be 32 bytes (64 hex chars)"));
        }
        Ok(DataKeys { aead: Aes256Gcm::new_from_slice(&dk)?, index: ik })
    }

    /// Encrypts `plain`, binding it to `aad` (the owning record id).
    pub fn seal(&self, plain: &str, aad: &str) -> Result<String> {
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let ct = self
            .aead
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain.as_bytes(), aad: aad.as_bytes() })
            .map_err(|_| anyhow!("encrypt"))?;
        let mut out = nonce.to_vec();
        out.extend(ct);
        Ok(STANDARD.encode(out))
    }

    pub fn open(&self, sealed: &str, aad: &str) -> Result<String> {
        let raw = STANDARD.decode(sealed)?;
        if raw.len() < 13 {
            return Err(anyhow!("ciphertext too short"));
        }
        let (nonce, ct) = raw.split_at(12);
        let pt = self
            .aead
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad: aad.as_bytes() })
            .map_err(|_| anyhow!("decrypt"))?;
        Ok(String::from_utf8(pt)?)
    }

    /// Deterministic keyed index for exact-match lookups of an encrypted value.
    pub fn blind_index(&self, value: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.index).expect("hmac key");
        mac.update(value.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

pub fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_rfc7636_vector() {
        // RFC 7636 Appendix B.
        assert!(pkce_matches("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!pkce_matches("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXX", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!pkce_matches("short", "x"));
    }

    #[test]
    fn email_encryption_is_bound_to_record() {
        let k = DataKeys::new(&"01".repeat(32), &"02".repeat(32)).unwrap();
        let sealed = k.seal("alice@example.com", "user-1").unwrap();
        assert_eq!(k.open(&sealed, "user-1").unwrap(), "alice@example.com");
        assert!(k.open(&sealed, "user-2").is_err(), "ciphertext moved to another row must not decrypt");
        assert_ne!(sealed, k.seal("alice@example.com", "user-1").unwrap(), "random nonce per encryption");
        assert_eq!(k.blind_index("a@b.c"), k.blind_index("a@b.c"));
    }

    #[test]
    fn passwords() {
        let h = hash_password("correct horse battery staple").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &h));
        assert!(!verify_password("wrong", &h));
    }
}
