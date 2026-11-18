//! Generic OIDC provider runtime: discovery, PKCE, token exchange, and
//! id_token validation. Speaks standards only — no provider-specific
//! endpoints or claims (see `docs platform/oidc-sso.md`).
//!
//! The runtime is built once at startup (`OidcRuntime::build`) and held on
//! `AppState`. It carries the resolved client credentials and caches the
//! discovery document + JWKS with a TTL; a `kid` the cache does not know
//! forces one refresh so provider key rotation is picked up immediately.
//!
//! Secrets discipline: the client secret stays inside this struct; id_tokens
//! and access tokens are validated/discarded here and are never logged.

use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use shared::error::AppError;

use crate::config::OidcConfig;

/// How long discovery + JWKS are trusted before a refresh.
const DISCOVERY_TTL: Duration = Duration::from_secs(3600);

/// Allowed clock skew between Buz and the provider on `exp`/`iat`.
const CLOCK_SKEW_SECONDS: u64 = 60;

/// The identity claims Buz consumes. `iss`/`aud`/`exp`/`iat` are validated by
/// the JWT layer and intentionally not modeled here.
#[derive(Debug, Deserialize)]
pub struct IdTokenClaims {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub groups: Option<Vec<String>>,
    #[serde(default)]
    pub nonce: Option<String>,
}

/// The resolved identity of one sign-in. `subject` is the account key.
#[derive(Debug, Clone)]
pub struct OidcIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Debug, Clone)]
struct CachedEndpoints {
    authorization_endpoint: String,
    token_endpoint: String,
    #[allow(dead_code)] // kept alongside discovery for JWKS-refresh debugging
    jwks_uri: String,
    jwks: JwkSet,
}

pub struct OidcRuntime {
    pub issuer: String,
    pub client_id: String,
    client_secret: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    /// Provision a plain Buz user on first sign-in of an unbound identity
    /// (the handler refuses unbound identities when false).
    pub auto_provision: bool,
    http: reqwest::Client,
    cached: RwLock<Option<(Instant, CachedEndpoints)>>,
}

impl OidcRuntime {
    /// Build the runtime from config. `None` when SSO is disabled or the
    /// configuration is invalid — callers keep serving local login either
    /// way (SSO misconfiguration never prevents the server from starting).
    pub async fn build(config: &OidcConfig) -> Option<Arc<Self>> {
        if !config.enabled {
            return None;
        }
        if let Err(error) = config.validate() {
            tracing::error!("oidc disabled — configuration invalid: {error}");
            return None;
        }
        let client_secret = match config.resolved_client_secret() {
            Ok(secret) => secret,
            Err(error) => {
                tracing::error!("oidc disabled — {error}");
                return None;
            }
        };
        let runtime = Arc::new(Self {
            issuer: config.issuer.trim().trim_end_matches('/').to_string(),
            client_id: config.client_id.trim().to_string(),
            client_secret,
            redirect_uri: config.redirect_uri.trim().to_string(),
            scopes: config.scopes.clone(),
            auto_provision: config.auto_provision,
            http: reqwest::Client::builder()
                // Provider endpoints only; never follow redirects — discovery
                // and JWKS URLs come from admin config and must answer directly.
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .ok()?,
            cached: RwLock::new(None),
        });
        // Fail fast on a plainly wrong issuer: an unreachable or lying
        // discovery document disables SSO at startup, not at first sign-in.
        if let Err(error) = runtime.endpoints(false).await {
            tracing::error!("oidc disabled — discovery failed at startup: {error}");
            return None;
        }
        tracing::info!(issuer = %runtime.issuer, "oidc sign-in enabled");
        Some(runtime)
    }

    /// Discovery + JWKS, cached with a TTL. `force` bypasses the TTL — used
    /// when a token carries a `kid` the cached set does not know.
    async fn endpoints(&self, force: bool) -> Result<CachedEndpoints, AppError> {
        if !force {
            let cached = self.cached.read().await;
            if let Some((at, endpoints)) = cached.as_ref() {
                if at.elapsed() < DISCOVERY_TTL {
                    return Ok(endpoints.clone());
                }
            }
        }
        let discovery_url = format!("{}/.well-known/openid-configuration", self.issuer);
        let discovery: DiscoveryDocument = self
            .http
            .get(&discovery_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| AppError::AuthFailed(format!("oidc discovery failed: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::AuthFailed(format!("oidc discovery unreadable: {e}")))?;
        // A discovery document claiming a different issuer is a hostile or
        // misconfigured provider — refuse before any token is accepted.
        if discovery.issuer.trim().trim_end_matches('/') != self.issuer {
            return Err(AppError::AuthFailed(format!(
                "oidc discovery issuer mismatch: expected {}, got {}",
                self.issuer, discovery.issuer
            )));
        }
        let jwks: JwkSet = self
            .http
            .get(&discovery.jwks_uri)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| AppError::AuthFailed(format!("oidc jwks fetch failed: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::AuthFailed(format!("oidc jwks unreadable: {e}")))?;
        let endpoints = CachedEndpoints {
            authorization_endpoint: discovery.authorization_endpoint,
            token_endpoint: discovery.token_endpoint,
            jwks_uri: discovery.jwks_uri,
            jwks,
        };
        *self.cached.write().await = Some((Instant::now(), endpoints.clone()));
        Ok(endpoints)
    }

    /// Resolve (and cache) the authorization endpoint for `/oidc/start`.
    /// A provider that is down or lying fails here — at click time, with a
    /// redirect to the SPA error page — rather than at callback time.
    pub async fn authorization_endpoint(&self) -> Result<String, AppError> {
        Ok(self.endpoints(false).await?.authorization_endpoint)
    }

    /// The authorize redirect target for one flow. `state`/`nonce` are
    /// base64url already (no additional encoding needed); `challenge` is
    /// `BASE64URL(SHA256(verifier))` per PKCE S256.
    pub fn authorize_url(
        &self,
        authorization_endpoint: &str,
        state: &str,
        nonce: &str,
        challenge: &str,
    ) -> String {
        let scope = self.scopes.join(" ");
        format!(
            "{authorization_endpoint}\
             ?response_type=code\
             &client_id={}\
             &redirect_uri={}\
             &scope={}\
             &state={state}\
             &nonce={nonce}\
             &code_challenge={challenge}\
             &code_challenge_method=S256",
            percent_encode(&self.client_id),
            percent_encode(&self.redirect_uri),
            percent_encode(&scope),
        )
    }

    /// Exchange `code` for an id_token (confidential client, PKCE S256,
    /// `client_secret_post`). Returns the raw id_token JWT.
    async fn exchange_code(
        &self,
        token_endpoint: &str,
        code: &str,
        code_verifier: &str,
    ) -> Result<String, AppError> {
        #[derive(Deserialize)]
        struct TokenResponse {
            #[serde(default)]
            id_token: Option<String>,
            #[serde(default)]
            error: Option<String>,
            #[serde(default)]
            error_description: Option<String>,
        }
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("code_verifier", code_verifier),
        ];
        let response: TokenResponse = self
            .http
            .post(token_endpoint)
            .form(&params)
            .send()
            .await
            .map_err(|e| AppError::AuthFailed(format!("oidc token request failed: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::AuthFailed(format!("oidc token response unreadable: {e}")))?;
        if let Some(error) = response.error {
            return Err(AppError::AuthFailed(format!(
                "oidc token rejected: {} {}",
                error,
                response.error_description.unwrap_or_default()
            )));
        }
        response
            .id_token
            .ok_or_else(|| AppError::AuthFailed("oidc token response carried no id_token".into()))
    }

    /// Validate the id_token: RS256 signature against the provider's JWKS
    /// (refreshing once on an unknown `kid`), `iss`/`aud`/`exp`/`iat` with a
    /// 60-second skew, and the flow's `nonce`. Returns the identity claims.
    pub async fn validate_id_token(
        &self,
        id_token: &str,
        expected_nonce: &str,
    ) -> Result<OidcIdentity, AppError> {
        let claims = self.decode_id_token(id_token).await?;
        if claims.nonce.as_deref() != Some(expected_nonce) {
            return Err(AppError::AuthFailed("oidc nonce mismatch".into()));
        }
        Ok(OidcIdentity {
            subject: claims.sub,
            email: claims.email.filter(|e| !e.trim().is_empty()),
            display_name: claims
                .name
                .or(claims.preferred_username)
                .filter(|n| !n.trim().is_empty()),
            groups: claims.groups.unwrap_or_default(),
        })
    }

    /// Complete sign-in round trip: consume the provider's token endpoint and
    /// validate the returned id_token against the flow's nonce.
    pub async fn redeem(
        &self,
        code: &str,
        code_verifier: &str,
        expected_nonce: &str,
    ) -> Result<OidcIdentity, AppError> {
        let endpoints = self.endpoints(false).await?;
        let id_token = self
            .exchange_code(&endpoints.token_endpoint, code, code_verifier)
            .await?;
        self.validate_id_token(&id_token, expected_nonce).await
    }

    async fn decode_id_token(&self, id_token: &str) -> Result<IdTokenClaims, AppError> {
        let header = decode_header(id_token)
            .map_err(|e| AppError::AuthFailed(format!("oidc id_token malformed: {e}")))?;
        if header.alg != Algorithm::RS256 {
            return Err(AppError::AuthFailed("oidc id_token must be RS256".into()));
        }
        let Some(kid) = header.kid else {
            return Err(AppError::AuthFailed("oidc id_token has no kid".into()));
        };
        let mut last_error = None;
        for force in [false, true] {
            let endpoints = self.endpoints(force).await?;
            let Some(jwk) = endpoints.jwks.find(&kid) else {
                last_error = Some(AppError::AuthFailed(format!("oidc jwks has no key {kid}")));
                continue;
            };
            let key = DecodingKey::from_jwk(jwk)
                .map_err(|e| AppError::AuthFailed(format!("oidc jwk unusable: {e}")))?;
            let mut validation = Validation::new(Algorithm::RS256);
            validation.set_issuer(&[&self.issuer]);
            validation.set_audience(&[&self.client_id]);
            validation.leeway = CLOCK_SKEW_SECONDS;
            match decode::<IdTokenClaims>(id_token, &key, &validation) {
                Ok(data) => return Ok(data.claims),
                Err(e) => {
                    last_error = Some(AppError::AuthFailed(format!(
                        "oidc id_token validation failed: {e}"
                    )));
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| AppError::AuthFailed("oidc id_token validation failed".into())))
    }
}

/// Generate `bytes` of provider-grade entropy as base64url (state, nonce,
/// PKCE verifier). The plaintext exists only in the redirect URL / flow row
/// hash; nothing here is ever persisted in the clear.
pub fn random_b64url(bytes: usize) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// PKCE S256 `code_challenge` for a verifier.
pub fn s256_challenge(verifier: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// SHA-256 hex digest — the at-rest form of `state` and tickets.
pub fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC 3986 unreserved characters pass through; everything else is
/// percent-encoded (client_id, redirect_uri, scope in the authorize URL).
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_pkce_s256_vector() {
        // RFC 7636 appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn percent_encode_passes_unreserved_only() {
        assert_eq!(percent_encode("aB3-._~"), "aB3-._~");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("x&y=z"), "x%26y%3Dz");
        assert_eq!(percent_encode("ü"), "%C3%BC");
    }

    #[test]
    fn random_values_have_the_expected_shape() {
        let state = random_b64url(32);
        assert_eq!(state.len(), 43, "32 bytes → 43 base64url chars");
        let verifier = random_b64url(64);
        assert!((43..=128).contains(&verifier.len()), "PKCE verifier length");
        assert_ne!(random_b64url(32), random_b64url(32));
    }

    #[test]
    fn sha256_hex_is_stable() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
