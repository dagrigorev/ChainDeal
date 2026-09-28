//! Access-token format shared by the auth service (issuer) and the API
//! (verifier): compact JWS, **EdDSA (Ed25519) only**.
//!
//! Accepting exactly one algorithm, chosen by the verifier rather than read
//! from the token, rules out `alg: none` and algorithm-confusion attacks.
//! Verification checks signature, `kid`, issuer, audience, expiry and
//! not-before with a small clock-skew allowance.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Audience of tokens accepted by the ChainDeal API.
pub const API_AUDIENCE: &str = "chaindeal-api";
const LEEWAY_SECS: u64 = 30;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("unsupported token algorithm")]
    Algorithm,
    #[error("unknown signing key")]
    UnknownKey,
    #[error("invalid signature")]
    Signature,
    #[error("token expired")]
    Expired,
    #[error("token not yet valid")]
    NotYetValid,
    #[error("wrong issuer or audience")]
    Audience,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: u64,
    pub nbf: u64,
    pub iat: u64,
    pub jti: String,
    /// Space-separated OAuth scopes granted to this token.
    pub scope: String,
    pub client_id: String,
    #[serde(default)]
    pub roles: Vec<String>,
    /// Wallet addresses proven (by signature) to belong to `sub`.
    #[serde(default)]
    pub wallets: Vec<String>,
    /// Login session id, for revocation and "sign out everywhere".
    #[serde(default)]
    pub sid: String,
    #[serde(default)]
    pub name: String,
}

impl Claims {
    pub fn has_scope(&self, s: &str) -> bool {
        self.scope.split(' ').any(|x| x == s)
    }
    pub fn has_role(&self, r: &str) -> bool {
        self.roles.iter().any(|x| x == r)
    }
}

#[derive(Serialize, Deserialize)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// Key id: first 16 hex chars of sha256(public key).
pub fn key_id(vk: &VerifyingKey) -> String {
    hex::encode(&Sha256::digest(vk.as_bytes())[..8])
}

/// An Ed25519 signing key with its id.
pub struct Issuer {
    key: SigningKey,
    pub kid: String,
}

impl Issuer {
    pub fn from_seed_hex(seed_hex: &str) -> Result<Self, TokenError> {
        let bytes: [u8; 32] = hex::decode(seed_hex.trim())
            .map_err(|_| TokenError::Malformed)?
            .try_into()
            .map_err(|_| TokenError::Malformed)?;
        let key = SigningKey::from_bytes(&bytes);
        let kid = key_id(&key.verifying_key());
        Ok(Issuer { key, kid })
    }

    /// Signs an access token (`typ: at+jwt`, RFC 9068).
    pub fn sign(&self, claims: &Claims) -> String {
        self.sign_value(&serde_json::to_value(claims).expect("claims"), "at+jwt")
    }

    /// Signs arbitrary claims, e.g. an OIDC ID token (`typ: JWT`).
    pub fn sign_value(&self, claims: &serde_json::Value, typ: &str) -> String {
        let header = Header { alg: "EdDSA".into(), typ: typ.into(), kid: self.kid.clone() };
        let h = B64.encode(serde_json::to_vec(&header).expect("header"));
        let p = B64.encode(serde_json::to_vec(claims).expect("claims"));
        let input = format!("{h}.{p}");
        let sig = self.key.sign(input.as_bytes());
        format!("{input}.{}", B64.encode(sig.to_bytes()))
    }

    pub fn jwk(&self) -> Jwk {
        Jwk::from_key(&self.key.verifying_key())
    }
}

/// RFC 8037 OKP JSON Web Key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub kid: String,
    #[serde(rename = "use")]
    pub use_: String,
    pub alg: String,
}

impl Jwk {
    pub fn from_key(vk: &VerifyingKey) -> Self {
        Jwk {
            kty: "OKP".into(),
            crv: "Ed25519".into(),
            x: B64.encode(vk.as_bytes()),
            kid: key_id(vk),
            use_: "sig".into(),
            alg: "EdDSA".into(),
        }
    }

    pub fn verifying_key(&self) -> Result<VerifyingKey, TokenError> {
        if self.kty != "OKP" || self.crv != "Ed25519" {
            return Err(TokenError::Algorithm);
        }
        let bytes: [u8; 32] = B64
            .decode(&self.x)
            .map_err(|_| TokenError::Malformed)?
            .try_into()
            .map_err(|_| TokenError::Malformed)?;
        VerifyingKey::from_bytes(&bytes).map_err(|_| TokenError::Malformed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

/// Verifies a token against a key set, issuer and audience at time `now`.
pub fn verify(token: &str, keys: &JwkSet, issuer: &str, audience: &str, now: u64) -> Result<Claims, TokenError> {
    let mut parts = token.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) => (h, p, s),
        _ => return Err(TokenError::Malformed),
    };
    let header: Header = serde_json::from_slice(&B64.decode(h).map_err(|_| TokenError::Malformed)?)
        .map_err(|_| TokenError::Malformed)?;
    if header.alg != "EdDSA" {
        return Err(TokenError::Algorithm);
    }
    let jwk = keys.keys.iter().find(|k| k.kid == header.kid).ok_or(TokenError::UnknownKey)?;
    let vk = jwk.verifying_key()?;
    let sig: [u8; 64] = B64
        .decode(s)
        .map_err(|_| TokenError::Malformed)?
        .try_into()
        .map_err(|_| TokenError::Malformed)?;
    vk.verify(format!("{h}.{p}").as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| TokenError::Signature)?;
    let claims: Claims = serde_json::from_slice(&B64.decode(p).map_err(|_| TokenError::Malformed)?)
        .map_err(|_| TokenError::Malformed)?;
    if claims.iss != issuer || claims.aud != audience {
        return Err(TokenError::Audience);
    }
    if now > claims.exp + LEEWAY_SECS {
        return Err(TokenError::Expired);
    }
    if claims.nbf > now + LEEWAY_SECS {
        return Err(TokenError::NotYetValid);
    }
    Ok(claims)
}

/// Message a wallet signs to prove it belongs to a user account. The fixed
/// prefix provides domain separation, so the signature can never be replayed
/// as a transaction signature (which signs a 64-char hex tx hash instead).
pub fn wallet_link_message(challenge: &str) -> String {
    format!("ChainDeal account link\nchallenge: {challenge}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(now: u64) -> Claims {
        Claims {
            iss: "https://chaindeal.localhost".into(),
            sub: "u1".into(),
            aud: API_AUDIENCE.into(),
            exp: now + 300,
            nbf: now,
            iat: now,
            jti: "j".into(),
            scope: "openid deals:write".into(),
            client_id: "web".into(),
            roles: vec!["user".into()],
            wallets: vec![],
            sid: "s".into(),
            name: "n".into(),
        }
    }

    #[test]
    fn roundtrip_and_rejections() {
        let iss = Issuer::from_seed_hex(&"11".repeat(32)).unwrap();
        let keys = JwkSet { keys: vec![iss.jwk()] };
        let t = iss.sign(&claims(1000));
        let c = verify(&t, &keys, "https://chaindeal.localhost", API_AUDIENCE, 1000).unwrap();
        assert!(c.has_scope("deals:write") && !c.has_scope("deals"));
        assert_eq!(verify(&t, &keys, "https://evil", API_AUDIENCE, 1000), Err(TokenError::Audience));
        assert_eq!(verify(&t, &keys, "https://chaindeal.localhost", API_AUDIENCE, 2000), Err(TokenError::Expired));
        // Tampered payload.
        let mut parts: Vec<&str> = t.split('.').collect();
        let forged = B64.encode(serde_json::to_vec(&Claims { roles: vec!["admin".into()], ..claims(1000) }).unwrap());
        parts[1] = &forged;
        assert_eq!(verify(&parts.join("."), &keys, "https://chaindeal.localhost", API_AUDIENCE, 1000), Err(TokenError::Signature));
        // alg:none / other algorithms are refused outright.
        let none = format!("{}.{}.", B64.encode(br#"{"alg":"none","typ":"JWT","kid":"x"}"#), t.split('.').nth(1).unwrap());
        assert_eq!(verify(&none, &keys, "https://chaindeal.localhost", API_AUDIENCE, 1000), Err(TokenError::Algorithm));
        // A different key is unknown.
        let other = Issuer::from_seed_hex(&"22".repeat(32)).unwrap();
        assert_eq!(verify(&other.sign(&claims(1000)), &keys, "https://chaindeal.localhost", API_AUDIENCE, 1000), Err(TokenError::UnknownKey));
    }
}
