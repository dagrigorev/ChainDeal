//! Authorization for the chain API. Access tokens are EdDSA JWTs issued by the
//! auth microservice; this module verifies them against its published JWKS
//! (cached, refreshed periodically and on an unknown `kid` for key rotation).

use std::time::{Duration, Instant};

use axum::http::{header, HeaderMap, StatusCode};
use chaindeal_authn::{verify, Claims, JwkSet, TokenError, API_AUDIENCE};
use tokio::sync::RwLock;

use crate::chain::now_ms;

const REFRESH_EVERY: Duration = Duration::from_secs(300);
const MIN_REFETCH: Duration = Duration::from_secs(30);

pub struct Authz {
    jwks_url: String,
    issuer: String,
    http: reqwest::Client,
    keys: RwLock<(JwkSet, Option<Instant>)>,
}

#[derive(Debug)]
pub struct Denied(pub StatusCode, pub String);

impl Authz {
    pub fn new(jwks_url: String, issuer: String) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().expect("http client");
        Authz { jwks_url, issuer, http, keys: RwLock::new((JwkSet::default(), None)) }
    }

    async fn refetch(&self, force: bool) {
        {
            let k = self.keys.read().await;
            let fresh = k.1.map(|t| t.elapsed() < if force { MIN_REFETCH } else { REFRESH_EVERY });
            if fresh == Some(true) {
                return;
            }
        }
        match self.http.get(&self.jwks_url).send().await.and_then(|r| r.error_for_status()) {
            Ok(r) => match r.json::<JwkSet>().await {
                Ok(set) => *self.keys.write().await = (set, Some(Instant::now())),
                Err(e) => tracing::warn!("bad JWKS from {}: {e}", self.jwks_url),
            },
            Err(e) => tracing::warn!("cannot fetch JWKS from {}: {e}", self.jwks_url),
        }
    }

    /// Verifies the bearer token on a request.
    pub async fn claims(&self, h: &HeaderMap) -> Result<Claims, Denied> {
        let token = h
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| Denied(StatusCode::UNAUTHORIZED, "sign in required".into()))?;
        self.refetch(false).await;
        let now = now_ms() / 1000;
        let mut res = verify(token, &self.keys.read().await.0, &self.issuer, API_AUDIENCE, now);
        if res == Err(TokenError::UnknownKey) {
            self.refetch(true).await; // the issuer may have rotated its key
            res = verify(token, &self.keys.read().await.0, &self.issuer, API_AUDIENCE, now);
        }
        res.map_err(|e| Denied(StatusCode::UNAUTHORIZED, format!("invalid access token: {e}")))
    }
}

pub fn forbid(msg: &str) -> Denied {
    Denied(StatusCode::FORBIDDEN, msg.to_string())
}
