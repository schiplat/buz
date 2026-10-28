use plugin_cli::mirror::*;

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

const INDEX: &str =
    r#"{"steps":[{"id":"buz/hello","version":"0.1.0","file":"hello/0.1.0.yaml","sha256":"aa"}]}"#;

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
