use manager::ssh_probe::probe;
use manager::ssh_probe::ProbeParams;

#[tokio::test]
async fn agent_type_returns_clear_failure() {
    let result = probe(ProbeParams {
        host: "127.0.0.1".into(),
        port: 22,
        username: "root".into(),
        credential_type: "agent".into(),
        credential_value: String::new(),
        credential_passphrase: None,
    })
    .await;
    assert!(!result.ok);
    assert!(
        result.message.to_lowercase().contains("agent"),
        "message={}",
        result.message
    );
}

#[tokio::test]
async fn empty_host_fails_fast() {
    let result = probe(ProbeParams {
        host: String::new(),
        port: 22,
        username: "root".into(),
        credential_type: "vault".into(),
        credential_value: "x".into(),
        credential_passphrase: None,
    })
    .await;
    assert!(!result.ok);
    assert!(result.message.contains("host address"));
}

#[tokio::test]
async fn key_without_material_fails() {
    let result = probe(ProbeParams {
        host: "127.0.0.1".into(),
        port: 22,
        username: "root".into(),
        credential_type: "key_path".into(),
        credential_value: String::new(),
        credential_passphrase: None,
    })
    .await;
    assert!(!result.ok);
    assert!(result.message.to_lowercase().contains("private key"));
}
