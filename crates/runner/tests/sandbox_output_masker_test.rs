use runner::client::poll::SandboxOutputMasker;

#[test]
fn catches_secrets_split_across_frames() {
    let mut masker = SandboxOutputMasker::new(&["secret123".to_string()], &[]);
    assert_eq!(masker.push("before secre", false), "befo");
    assert_eq!(masker.push("t123 after", true), "re *** after");
}
