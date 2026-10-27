//! `buz-plugin mirror check` — compare the namespace revision sentinel
//! between the canonical origin and a mirror. Same protocol as the
//! Manager's *Check mirror* action: two tiny GETs, falling back to a full
//! index byte-compare when a sentinel is missing (the index is generated
//! byte-deterministically, so equal bytes mean equal content).

use crate::layout;
use anyhow::Context;
use serde::Serialize;
use std::time::Duration;

#[derive(Serialize, Debug)]
pub struct MirrorReport {
    /// `in-sync` | `out-of-sync` | `canonical-unreachable` | `mirror-unreachable`
    pub status: String,
    pub canonical_revision: Option<String>,
    pub mirror_revision: Option<String>,
    pub detail: Option<String>,
}

#[derive(serde::Deserialize)]
struct RevisionSentinel {
    revision: String,
    #[serde(default)]
    updated_at: Option<String>,
}

fn http() -> anyhow::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("cannot build the HTTP client")
}

/// GET a URL; a 404 means "absent" (None), anything else non-success is an
/// error.
fn get_opt(http: &reqwest::blocking::Client, url: &str) -> anyhow::Result<Option<String>> {
    let resp = http
        .get(url)
        .send()
        .with_context(|| format!("request to {url} failed"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        anyhow::bail!(
            "source returned HTTP {} for {url} — the file does not exist or the path is wrong",
            resp.status()
        );
    }
    Ok(Some(
        resp.text().with_context(|| format!("cannot read {url}"))?,
    ))
}

fn fetch_revision(
    http: &reqwest::blocking::Client,
    base: &str,
) -> anyhow::Result<Option<RevisionSentinel>> {
    match get_opt(http, &layout::join_url(base, layout::REVISION_FILE))? {
        None => Ok(None),
        Some(raw) => serde_json::from_str(&raw)
            .map(Some)
            .with_context(|| format!("invalid revision.json under {base}")),
    }
}

fn fetch_index(http: &reqwest::blocking::Client, base: &str) -> anyhow::Result<String> {
    get_opt(http, &layout::join_url(base, layout::INDEX_FILE))?
        .with_context(|| format!("no index.json under {base} — a drift check needs both indexes"))
}

pub fn check(
    http: &reqwest::blocking::Client,
    source_index_url: &str,
    mirror_index_url: &str,
) -> anyhow::Result<MirrorReport> {
    let canonical_base = layout::base_url(source_index_url);
    let mirror_base = layout::base_url(mirror_index_url);

    let canonical = match fetch_revision(http, &canonical_base) {
        Ok(sentinel) => sentinel,
        Err(e) => {
            return Ok(MirrorReport {
                status: "canonical-unreachable".into(),
                canonical_revision: None,
                mirror_revision: None,
                detail: Some(e.to_string()),
            })
        }
    };
    let mirror = match fetch_revision(http, &mirror_base) {
        Ok(sentinel) => sentinel,
        Err(e) => {
            return Ok(MirrorReport {
                status: "mirror-unreachable".into(),
                canonical_revision: canonical.as_ref().map(|s| s.revision.clone()),
                mirror_revision: None,
                detail: Some(e.to_string()),
            })
        }
    };

    match (canonical, mirror) {
        (Some(canon), Some(mirr)) => {
            let status = if canon.revision == mirr.revision {
                "in-sync"
            } else {
                "out-of-sync"
            };
            Ok(MirrorReport {
                status: status.into(),
                canonical_revision: Some(canon.revision),
                mirror_revision: Some(mirr.revision),
                detail: canon.updated_at,
            })
        }
        (canon, mirr) => {
            let canonical_index = fetch_index(http, &canonical_base)?;
            let mirror_index = fetch_index(http, &mirror_base)?;
            let status = if canonical_index == mirror_index {
                "in-sync"
            } else {
                "out-of-sync"
            };
            Ok(MirrorReport {
                status: status.into(),
                canonical_revision: canon.map(|s| s.revision),
                mirror_revision: mirr.map(|s| s.revision),
                detail: Some(
                    "no revision.json sentinel on one side — compared the full index".into(),
                ),
            })
        }
    }
}

pub fn run(source: &str, mirror: &str, json: bool) -> anyhow::Result<()> {
    let http = http()?;
    let report = check(&http, source, mirror)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        match report.status.as_str() {
            "in-sync" => println!(
                "in-sync (revision {})",
                report.canonical_revision.as_deref().unwrap_or("unknown")
            ),
            "out-of-sync" => println!(
                "out-of-sync — canonical rev {:?}, mirror rev {:?}; re-sync the mirror",
                report.canonical_revision, report.mirror_revision
            ),
            other => println!("{other}: {}", report.detail.as_deref().unwrap_or("")),
        }
        if let Some(detail) = &report.detail {
            println!("detail: {detail}");
        }
    }
    if report.status != "in-sync" {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal HTTP server: serves `revision.json` (or 404 when None) and
    /// `index.json` from static bodies for a bounded number of requests.
    fn spawn_server(revision: Option<&'static str>, index_body: &'static str) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            tx.send(listener.local_addr().unwrap().to_string()).unwrap();
            for _ in 0..8 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(_) => break,
                };
                let mut buffer = [0u8; 8192];
                let read = std::io::Read::read(&mut stream, &mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let path = request.split(' ').nth(1).unwrap_or("/").to_string();
                let (status, body): (&str, String) = if path.ends_with("revision.json") {
                    match revision {
                        Some(body) => ("200 OK", body.to_string()),
                        None => ("404 Not Found", String::new()),
                    }
                } else if path.ends_with("index.json") {
                    ("200 OK", index_body.to_string())
                } else {
                    ("404 Not Found", String::new())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
        });
        format!("http://{}", rx.recv().unwrap())
    }

    const INDEX: &str = r#"{"steps":[{"id":"buz/hello","version":"0.1.0","file":"hello/0.1.0.yaml","sha256":"aa"}]}"#;

    #[test]
    fn in_sync_when_revisions_match() {
        let revision = r#"{"revision":"abc123","updated_at":"2026-10-27T00:00:00Z"}"#;
        let canonical = spawn_server(Some(revision), INDEX);
        let mirror = spawn_server(Some(revision), INDEX);
        let http = http().unwrap();
        let report = check(
            &http,
            &format!("{canonical}/index.json"),
            &format!("{mirror}/index.json"),
        )
        .unwrap();
        assert_eq!(report.status, "in-sync", "{report:?}");
        assert_eq!(report.canonical_revision.as_deref(), Some("abc123"));
        assert_eq!(report.mirror_revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn out_of_sync_when_mirror_revision_differs() {
        let canonical = spawn_server(Some(r#"{"revision":"abc123"}"#), INDEX);
        let mirror = spawn_server(Some(r#"{"revision":"stale999"}"#), INDEX);
        let http = http().unwrap();
        let report = check(
            &http,
            &format!("{canonical}/index.json"),
            &format!("{mirror}/index.json"),
        )
        .unwrap();
        assert_eq!(report.status, "out-of-sync", "{report:?}");
        assert_eq!(report.canonical_revision.as_deref(), Some("abc123"));
        assert_eq!(report.mirror_revision.as_deref(), Some("stale999"));
    }

    #[test]
    fn falls_back_to_full_index_compare_without_sentinels() {
        let canonical = spawn_server(None, INDEX);
        let mirror = spawn_server(None, INDEX);
        let http = http().unwrap();
        let report = check(
            &http,
            &format!("{canonical}/index.json"),
            &format!("{mirror}/index.json"),
        )
        .unwrap();
        assert_eq!(report.status, "in-sync", "{report:?}");
        assert!(report.detail.unwrap_or_default().contains("full index"));
    }

    #[test]
    fn reports_unreachable_canonical() {
        // Port 1 on localhost is closed in test environments.
        let mirror = spawn_server(Some(r#"{"revision":"abc123"}"#), INDEX);
        let http = http().unwrap();
        let report = check(
            &http,
            "http://127.0.0.1:1/index.json",
            &format!("{mirror}/index.json"),
        )
        .unwrap();
        assert_eq!(report.status, "canonical-unreachable", "{report:?}");
    }
}
