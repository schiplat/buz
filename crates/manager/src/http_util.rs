//! Shared base for outbound HTTP clients.
//!
//! Every `reqwest::Client` in the manager should be built through
//! [`client_builder`] so one switch governs proxy behavior across all of
//! them (plugin sync, registry lookups, clock probes, notifications,
//! GitHub sync).

use reqwest::Proxy;

/// When `BUZ_HTTP_NO_SYSTEM_PROXY` is set (any value except `0`), skip the
/// OS-level proxy probe entirely and honor proxy environment variables
/// (`http_proxy` / `https_proxy` / `all_proxy` / `no_proxy`) instead.
///
/// Why opt-in: on macOS the probe runs synchronously inside
/// `Client::build()` — `SCDynamicStoreCopyProxiesWithOptions` — and on some
/// machines that call stalls 10s+ per process (measured: LaunchServices
/// resolving the bundle ID of an unregistered dev binary), stalling every
/// pipeline save and test that builds a client. Linux deployments are
/// unaffected either way (no system-proxy store; env vars only), and the
/// explicit env-var parsing below keeps their semantics identical when the
/// switch is on.
pub fn no_system_proxy_enabled() -> bool {
    match std::env::var("BUZ_HTTP_NO_SYSTEM_PROXY") {
        Ok(v) => !v.is_empty() && !v.eq_ignore_ascii_case("0"),
        Err(_) => false,
    }
}

/// Base builder for outbound clients: apply the proxy switch, leave the
/// rest (timeouts, redirect policy) to the caller.
pub fn client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    if !no_system_proxy_enabled() {
        return builder;
    }
    let var = |lower: &str, upper: &str| {
        std::env::var(lower)
            .or_else(|_| std::env::var(upper))
            .ok()
            .filter(|v| !v.trim().is_empty())
    };
    let no_proxy_list = var("no_proxy", "NO_PROXY").map(|v| reqwest::NoProxy::from_string(&v));
    let mut builder = builder.no_proxy();
    let add = |mut builder: reqwest::ClientBuilder, proxy: reqwest::Result<Proxy>| {
        if let Ok(p) = proxy {
            let p = match &no_proxy_list {
                Some(n) => p.no_proxy(n.clone()),
                None => p,
            };
            builder = builder.proxy(p);
        }
        builder
    };
    if let Some(url) = var("all_proxy", "ALL_PROXY") {
        builder = add(builder, Proxy::all(&url));
    }
    if let Some(url) = var("http_proxy", "HTTP_PROXY") {
        builder = add(builder, Proxy::http(&url));
    }
    if let Some(url) = var("https_proxy", "HTTPS_PROXY") {
        builder = add(builder, Proxy::https(&url));
    }
    builder
}
