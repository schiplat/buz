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

/// How long discovery + JWKS are trusted before a refresh.
const DISCOVERY_TTL: Duration = Duration::from_secs(3600);

/// Allowed clock skew between Buz and the provider on `exp`/`iat`.
const CLOCK_SKEW_SECONDS: u64 = 60;

/// How old a back-channel logout token may be (OpenID Connect Back-Channel
/// Logout 1.0 §2.6 recommends a short window; replay within it is harmless
/// because revocation is idempotent).
const BACKCHANNEL_MAX_AGE_SECONDS: i64 = 300;

/// The event member a back-channel logout token must carry.
const BACKCHANNEL_LOGOUT_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";

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

/// A back-channel logout token (`OpenID Connect Back-Channel Logout 1.0`).
/// `iss`/`aud`/`exp` are validated by the JWT layer; `exp` is optional on
/// logout tokens, so only `iat` freshness is enforced here.
#[derive(Debug, Deserialize)]
pub struct LogoutTokenClaims {
    #[serde(default)]
    pub sub: Option<String>,
    pub iat: i64,
    #[serde(default)]
    pub sid: Option<String>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub events: Option<serde_json::Value>,
}

/// The fully-resolved sign-in configuration: precedence **database row >
/// `[auth.oidc]` file > defaults**, with `redirect_uri` derived from
/// `server.public_base_url` when no explicit override is set.
#[derive(Debug, Clone)]
pub struct EffectiveOidc {
    pub enabled: bool,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub auto_provision: bool,
    /// Where each saved value came from: `database` once an administrator
    /// has saved settings, otherwise `file`.
    pub source: &'static str,
}

/// The deployment's public base URL, validated for provider URL derivation
/// (callback, back-channel logout): absolute http(s), no trailing slash.
fn derived_base(public_base_url: &str) -> Result<String, String> {
    let base = public_base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(
            "provider URLs cannot be derived: set server.public_base_url first \
             (or fill an explicit redirect_uri)"
                .into(),
        );
    }
    if !base.starts_with("https://")
        && !base.starts_with("http://127.0.0.1")
        && !base.starts_with("http://localhost")
    {
        return Err("server.public_base_url must be an absolute http(s) URL".into());
    }
    Ok(base.to_string())
}

impl EffectiveOidc {
    /// The callback URL a provider must have registered, derived from the
    /// deployment's public base URL when no override is configured.
    pub fn derived_redirect_uri(public_base_url: &str) -> Result<String, String> {
        Ok(format!(
            "{}/api/v1/auth/oidc/callback",
            derived_base(public_base_url)?
        ))
    }

    /// The back-channel logout URL a provider must have registered (the
    /// settings page shows it next to the callback).
    pub fn derived_backchannel_logout_uri(public_base_url: &str) -> Result<String, String> {
        Ok(format!(
            "{}/api/v1/auth/oidc/backchannel-logout",
            derived_base(public_base_url)?
        ))
    }

    /// The post-logout landing a provider returns the browser to after
    /// RP-initiated logout (register this alongside the callback).
    pub fn derived_post_logout_redirect_uri(public_base_url: &str) -> Result<String, String> {
        Ok(format!(
            "{}/auth/sso/logged-out",
            derived_base(public_base_url)?
        ))
    }

    /// The `OidcConfig::validate` rules applied to the resolved shape.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let issuer = self.issuer.trim();
        if issuer.is_empty() {
            return Err("issuer is required when SSO is enabled".into());
        }
        if issuer.ends_with('/') {
            return Err("issuer must not end with '/'".into());
        }
        let is_local_http = issuer.starts_with("http://127.0.0.1")
            || issuer.starts_with("http://localhost")
            || issuer.starts_with("http://[::1]");
        if !issuer.starts_with("https://") && !is_local_http {
            return Err("issuer must be https (http allowed only for localhost                         development)"
                .into());
        }
        if self.client_id.trim().is_empty() {
            return Err("client_id is required when SSO is enabled".into());
        }
        if self.client_secret.trim().is_empty() {
            return Err("client secret is required when SSO is enabled".into());
        }
        let redirect = self.redirect_uri.trim();
        if redirect.is_empty() {
            return Err("redirect_uri is required when SSO is enabled".into());
        }
        if !redirect.starts_with("https://") && !redirect.starts_with("http://") {
            return Err("redirect_uri must be an absolute http(s) URL".into());
        }
        if !self.scopes.iter().any(|s| s == "openid") {
            return Err("scopes must include 'openid'".into());
        }
        Ok(())
    }
}

/// Resolve the effective configuration **from the database row only** —
/// `[auth.oidc]` in manager.toml no longer feeds the runtime (it is imported
/// into the row once at startup; see `import_file_seed` in
/// `api/oidc_settings.rs`). An absent row simply means SSO is off until an
/// administrator saves settings from the web. `jwt_secret` decrypts the
/// stored client secret. Validation is the caller's job (`validate`).
pub fn resolve_effective(
    row: Option<&crate::store::oidc_settings::OidcSettingsRow>,
    public_base_url: &str,
    jwt_secret: &str,
) -> Result<EffectiveOidc, String> {
    let Some(row) = row else {
        return Ok(EffectiveOidc {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: String::new(),
            scopes: vec!["openid".into(), "profile".into(), "email".into()],
            auto_provision: true,
            source: "database",
        });
    };

    let scopes: Vec<String> = serde_json::from_str(&row.scopes_json)
        .unwrap_or_else(|_| vec!["openid".into(), "profile".into(), "email".into()]);
    let client_secret = if row.client_secret_ciphertext.is_empty() {
        // The save form keeps an existing secret without re-entering it; a
        // row without one is an invalid configuration (validate refuses).
        String::new()
    } else {
        crate::crypto::decrypt(&row.client_secret_ciphertext, jwt_secret)
            .map_err(|e| format!("cannot decrypt the stored OIDC client secret: {e}"))?
    };
    let redirect_uri = match row.redirect_uri_override.as_deref() {
        Some(override_uri) if !override_uri.trim().is_empty() => override_uri.trim().to_string(),
        _ => EffectiveOidc::derived_redirect_uri(public_base_url)?,
    };
    Ok(EffectiveOidc {
        enabled: row.enabled,
        issuer: row.issuer.trim().trim_end_matches('/').to_string(),
        client_id: row.client_id.trim().to_string(),
        client_secret,
        redirect_uri,
        scopes,
        auto_provision: row.auto_provision,
        source: "database",
    })
}

/// Result of the settings-page "Test connection" probe: the provider's
/// discovery document was fetched and its `iss` compared to the configured
/// issuer. No client_id/secret involvement — this only proves reachability
/// and sane discovery, not credentials.
#[derive(Debug, serde::Serialize)]
pub struct DiscoveryProbe {
    pub issuer_match: bool,
    pub discovered_issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

/// Fetch and validate `{issuer}/.well-known/openid-configuration` with a
/// one-shot client (no redirects, 10s timeout). Admin-triggered from the
/// settings UI before a save.
pub async fn probe_discovery(issuer: &str) -> Result<DiscoveryProbe, String> {
    let issuer = issuer.trim().trim_end_matches('/').to_string();
    if issuer.is_empty() {
        return Err("issuer is empty".into());
    }
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let discovery_url = format!("{issuer}/.well-known/openid-configuration");
    let discovery: DiscoveryDocument = http
        .get(&discovery_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("discovery request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("discovery response unreadable: {e}"))?;
    Ok(DiscoveryProbe {
        issuer_match: discovery.issuer.trim().trim_end_matches('/') == issuer,
        discovered_issuer: discovery.issuer,
        authorization_endpoint: discovery.authorization_endpoint,
        token_endpoint: discovery.token_endpoint,
        jwks_uri: discovery.jwks_uri,
    })
}

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    /// Present when the provider supports RP-initiated logout
    /// (`OpenID Connect Session Management` / its `end_session` successor).
    #[serde(default)]
    end_session_endpoint: Option<String>,
    /// How the provider accepts client authentication at the token endpoint.
    /// Absent/empty → the RFC 6749 default for confidential clients
    /// (`client_secret_basic`); a declared list narrows the choice. Buz sends
    /// form credentials when `client_secret_post` is declared, Basic auth
    /// otherwise — providers that support both accept either.
    #[serde(default)]
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

/// The negotiated token-endpoint client authentication (see
/// [`DiscoveryDocument::token_endpoint_auth_methods_supported`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenAuth {
    /// `client_id`/`client_secret` in the form body.
    ClientSecretPost,
    /// `client_id`/`client_secret` in an `Authorization: Basic` header.
    ClientSecretBasic,
}

impl TokenAuth {
    fn from_discovery(methods: Option<&[String]>) -> Self {
        let declared = methods.map(|m| !m.is_empty()).unwrap_or(false);
        if declared && !methods.unwrap().iter().any(|m| m == "client_secret_post") {
            TokenAuth::ClientSecretBasic
        } else {
            TokenAuth::ClientSecretPost
        }
    }
}

#[derive(Debug, Clone)]
struct CachedEndpoints {
    authorization_endpoint: String,
    token_endpoint: String,
    #[allow(dead_code)] // kept alongside discovery for JWKS-refresh debugging
    jwks_uri: String,
    jwks: JwkSet,
    end_session_endpoint: Option<String>,
    token_auth: TokenAuth,
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
    /// Build the runtime from the resolved configuration. `None` when SSO is
    /// disabled or the configuration is invalid — callers keep serving local
    /// login either way (SSO misconfiguration never prevents the server from
    /// starting).
    pub async fn build_effective(effective: &EffectiveOidc) -> Option<Arc<Self>> {
        if !effective.enabled {
            return None;
        }
        if let Err(error) = effective.validate() {
            tracing::error!("oidc disabled — configuration invalid: {error}");
            return None;
        }
        if effective.client_secret.trim().is_empty() {
            tracing::error!("oidc disabled — client secret is empty");
            return None;
        }
        let runtime = Arc::new(Self {
            issuer: effective.issuer.trim().trim_end_matches('/').to_string(),
            client_id: effective.client_id.trim().to_string(),
            client_secret: effective.client_secret.trim().to_string(),
            redirect_uri: effective.redirect_uri.trim().to_string(),
            scopes: effective.scopes.clone(),
            auto_provision: effective.auto_provision,
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
            end_session_endpoint: discovery
                .end_session_endpoint
                .filter(|url| !url.trim().is_empty()),
            token_auth: TokenAuth::from_discovery(
                discovery.token_endpoint_auth_methods_supported.as_deref(),
            ),
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

    /// The provider's end-session endpoint, when it advertises one
    /// (RP-initiated logout). `None` → sign-out is purely local.
    pub async fn end_session_endpoint(&self) -> Result<Option<String>, AppError> {
        Ok(self.endpoints(false).await?.end_session_endpoint.clone())
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
    /// client authentication per discovery — see [`TokenAuth`]). Returns the
    /// raw id_token JWT.
    async fn exchange_code(
        &self,
        token_endpoint: &str,
        token_auth: TokenAuth,
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
        // Basic auth keeps the secret out of the body; post auth keeps it in
        // the form — per what the provider declared it accepts.
        let mut params = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_verifier", code_verifier),
        ];
        let mut request = self.http.post(token_endpoint);
        if token_auth == TokenAuth::ClientSecretBasic {
            request = request.basic_auth(&self.client_id, Some(&self.client_secret));
        } else {
            params.push(("client_id", self.client_id.as_str()));
            params.push(("client_secret", self.client_secret.as_str()));
        }
        let response: TokenResponse = request
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
            .exchange_code(
                &endpoints.token_endpoint,
                endpoints.token_auth,
                code,
                code_verifier,
            )
            .await?;
        self.validate_id_token(&id_token, expected_nonce).await
    }

    async fn decode_id_token(&self, id_token: &str) -> Result<IdTokenClaims, AppError> {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.leeway = CLOCK_SKEW_SECONDS;
        self.decode_with_jwks(id_token, &validation).await
    }

    /// Validate a back-channel logout token (`OpenID Connect Back-Channel
    /// Logout 1.0`): RS256 against the provider JWKS, `iss`/`aud` (and `exp`
    /// when the provider includes one), no `nonce`, the logout event member,
    /// and a fresh `iat`.
    ///
    /// Returns the subject to revoke, or `None` for a valid `sid`-only token
    /// (this deployment tracks users, not provider session ids).
    pub async fn validate_backchannel_logout_token(
        &self,
        logout_token: &str,
    ) -> Result<Option<String>, AppError> {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.leeway = CLOCK_SKEW_SECONDS;
        // `exp` is optional on logout tokens: validate it when present, do
        // not require it (the JWT layer only requires what the set names).
        validation.required_spec_claims.clear();
        let claims: LogoutTokenClaims = self.decode_with_jwks(logout_token, &validation).await?;

        if claims.nonce.is_some() {
            return Err(AppError::AuthFailed(
                "oidc logout token must not carry a nonce".into(),
            ));
        }
        let has_event = claims
            .events
            .as_ref()
            .and_then(|value| value.as_object())
            .is_some_and(|events| events.contains_key(BACKCHANNEL_LOGOUT_EVENT));
        if !has_event {
            return Err(AppError::AuthFailed(
                "oidc logout token lacks the back-channel logout event".into(),
            ));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if claims.iat > now + CLOCK_SKEW_SECONDS as i64 {
            return Err(AppError::AuthFailed(
                "oidc logout token iat is in the future".into(),
            ));
        }
        if now.saturating_sub(claims.iat) > BACKCHANNEL_MAX_AGE_SECONDS {
            return Err(AppError::AuthFailed("oidc logout token is stale".into()));
        }

        match claims.sub {
            Some(sub) if !sub.trim().is_empty() => Ok(Some(sub)),
            _ => {
                tracing::warn!(
                    sid = %claims.sid.as_deref().unwrap_or(""),
                    "oidc logout token carries no subject — ignored (sid-only)"
                );
                Ok(None)
            }
        }
    }

    /// Decode + verify a provider-signed JWT against the cached JWKS,
    /// refreshing once when the token's `kid` is unknown (provider key
    /// rotation). `validation` carries the caller's claim rules.
    async fn decode_with_jwks<T: serde::de::DeserializeOwned>(
        &self,
        token: &str,
        validation: &Validation,
    ) -> Result<T, AppError> {
        let header = decode_header(token)
            .map_err(|e| AppError::AuthFailed(format!("oidc token malformed: {e}")))?;
        if header.alg != Algorithm::RS256 {
            return Err(AppError::AuthFailed("oidc token must be RS256".into()));
        }
        let Some(kid) = header.kid else {
            return Err(AppError::AuthFailed("oidc token has no kid".into()));
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
            match decode::<T>(token, &key, validation) {
                Ok(data) => return Ok(data.claims),
                Err(e) => {
                    last_error = Some(AppError::AuthFailed(format!(
                        "oidc token validation failed: {e}"
                    )));
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| AppError::AuthFailed("oidc token validation failed".into())))
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
