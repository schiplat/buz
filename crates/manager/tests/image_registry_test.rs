use manager::image_registry::{
    check_image_at, parse_bearer_challenge, ImageAvailability, ImageReference,
};
use reqwest::Client;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

#[test]
fn parses_docker_hub_defaults_tags_and_digests() {
    assert_eq!(
        ImageReference::parse("busybox").unwrap(),
        ImageReference {
            registry: "docker.io".into(),
            repository: "library/busybox".into(),
            reference: "latest".into(),
        }
    );
    assert_eq!(
        ImageReference::parse("schiplat/tools:1.2").unwrap(),
        ImageReference {
            registry: "docker.io".into(),
            repository: "schiplat/tools".into(),
            reference: "1.2".into(),
        }
    );
    assert_eq!(
        ImageReference::parse(&format!("ghcr.io/team/app@sha256:{}", "AB".repeat(32)))
            .unwrap()
            .reference,
        format!("sha256:{}", "ab".repeat(32))
    );
    assert_eq!(
        ImageReference::parse("registry.example:5000/team/app:v2")
            .unwrap()
            .registry,
        "registry.example:5000"
    );
    assert!(ImageReference::parse("repo/app@sha512:abcd").is_err());
    assert!(ImageReference::parse("repo/app@sha256:123").is_err());
}

#[test]
fn rejects_registry_hosts_that_are_not_plain_https_authorities() {
    for raw in [
        "registry.example?query/team/app:1",
        "registry.example#fragment/team/app:1",
        "registry.example:abc/team/app:1",
    ] {
        let image = ImageReference::parse(raw).unwrap();
        assert!(image.registry_base().is_err(), "accepted {raw}");
    }
}

#[test]
fn manifest_path_keeps_digest_reference_intact() {
    let image = ImageReference::parse(&format!("team/app@sha256:{}", "ab".repeat(32))).unwrap();
    let base = Url::parse("https://registry.example/").unwrap();
    assert_eq!(
        image.manifest_url(&base).unwrap().path(),
        format!("/v2/team/app/manifests/sha256:{}", "ab".repeat(32))
    );
}

#[test]
fn parses_bearer_challenges_with_quoted_commas() {
    let challenge = parse_bearer_challenge(
            "Bearer realm=\"https://auth.example/token\",service=\"registry.example\",scope=\"repository:team/a,b:pull\"",
        )
        .unwrap();
    assert_eq!(challenge.realm, "https://auth.example/token");
    assert_eq!(challenge.service.as_deref(), Some("registry.example"));
    assert_eq!(challenge.scope.as_deref(), Some("repository:team/a,b:pull"));
    assert!(parse_bearer_challenge("Basic realm=\"https://auth.example\"").is_none());
}

async fn serve_once(response: String) -> (Url, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (Url::parse(&format!("http://{address}/")).unwrap(), task)
}

fn test_client(timeout: Duration) -> Client {
    Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

#[tokio::test]
async fn checks_exists_not_found_authentication_and_timeout() {
    let image = ImageReference::parse("example.test/team/app:1").unwrap();
    let (base, task) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into()).await;
    assert_eq!(
        check_image_at(&test_client(Duration::from_secs(1)), &image, &base, true).await,
        ImageAvailability::Exists
    );
    task.await.unwrap();

    let (base, task) =
        serve_once("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into()).await;
    assert_eq!(
        check_image_at(&test_client(Duration::from_secs(1)), &image, &base, true).await,
        ImageAvailability::NotFound
    );
    task.await.unwrap();

    let (base, task) = serve_once(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"local\"\r\nContent-Length: 0\r\n\r\n".into(),
        )
        .await;
    assert!(matches!(
        check_image_at(&test_client(Duration::from_secs(1)), &image, &base, true).await,
        ImageAvailability::Unverified(_)
    ));
    task.await.unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await;
    });
    let base = Url::parse(&format!("http://{address}/")).unwrap();
    assert!(matches!(
        check_image_at(&test_client(Duration::from_millis(20)), &image, &base, true).await,
        ImageAvailability::Unverified(_)
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn falls_back_to_bounded_get_when_head_is_unsupported() {
    use std::sync::{Arc, Mutex};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_server = Arc::clone(&seen);
    let task = tokio::spawn(async move {
        for request_number in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..length]).to_string();
            seen_server.lock().unwrap().push(request.clone());
            let response = if request_number == 0 {
                "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let image = ImageReference::parse("example.test/team/app:1").unwrap();
    let base = Url::parse(&format!("http://{address}/")).unwrap();
    assert_eq!(
        check_image_at(&test_client(Duration::from_secs(1)), &image, &base, true).await,
        ImageAvailability::Exists
    );
    task.await.unwrap();
    let seen = seen.lock().unwrap();
    assert!(seen[0].starts_with("HEAD /v2/team/app/manifests/1 "));
    assert!(seen[1].starts_with("GET /v2/team/app/manifests/1 "));
    assert!(seen[1].to_ascii_lowercase().contains("range: bytes=0-0"));
}

#[tokio::test]
async fn rejects_cross_origin_bearer_token_realms() {
    use manager::image_registry::{fetch_bearer_token, BearerChallenge};

    let registry = Url::parse("https://registry.example/").unwrap();
    let challenge = BearerChallenge {
        realm: "http://127.0.0.1:9/token".into(),
        service: None,
        scope: None,
    };
    let error = fetch_bearer_token(
        &test_client(Duration::from_secs(1)),
        &challenge,
        &registry,
        true,
    )
    .await
    .unwrap_err();
    assert!(error.contains("not an approved authentication host"));
}

#[tokio::test]
async fn follows_anonymous_bearer_challenge() {
    use std::sync::{Arc, Mutex};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_server = Arc::clone(&seen);
    let task = tokio::spawn(async move {
        for request_number in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..length]).to_string();
            seen_server.lock().unwrap().push(request.clone());
            let response = match request_number {
                    0 => format!(
                        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"http://{address}/token\",service=\"registry.test\",scope=\"repository:team/app:pull\"\r\nContent-Length: 0\r\n\r\n"
                    ),
                    1 => "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"token\":\"abc\"}".into(),
                    _ => "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into(),
                };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let image = ImageReference::parse("example.test/team/app:1").unwrap();
    let base = Url::parse(&format!("http://{address}/")).unwrap();
    assert_eq!(
        check_image_at(&test_client(Duration::from_secs(1)), &image, &base, true).await,
        ImageAvailability::Exists
    );
    task.await.unwrap();
    let seen = seen.lock().unwrap();
    assert!(seen[1]
        .starts_with("GET /token?service=registry.test&scope=repository%3Ateam%2Fapp%3Apull "));
    assert!(seen[2]
        .to_ascii_lowercase()
        .contains("authorization: bearer abc"));
}
