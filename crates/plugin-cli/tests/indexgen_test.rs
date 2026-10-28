use plugin_cli::indexgen::*;
use std::fs;
use std::path::{Path, PathBuf};

const SPEC: &str = r#"id: alice/oss-upload
version: 1.0.0
publisher: community
author: alice
source_repo: https://github.com/alice/my-buz-steps
engine: shell
requires_buz: ">=1.0.0"
inputs:
  bucket:
    type: string
    required: true
  file:
    type: string
    required: true
run: echo ok
"#;

fn write_repo(dir: &Path) -> PathBuf {
    let root = dir.join("repo");
    let plugin = root.join("plugins").join("alice").join("oss-upload");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("1.0.0.yaml"), SPEC).unwrap();
    root
}

fn options(root: &Path) -> Options {
    Options {
        plugins_dir: root.join("plugins"),
        revision: None,
        updated_at: None,
        sequence: None,
        no_meta: false,
        check: false,
        sign_key: None,
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Byte-for-byte comparison against committed golden files. This is the
/// compatibility contract in one artifact: indentation, key order,
/// trailing newline, entry sorting, and every field value. A diff here
/// shows exactly which of those changed, and reviewing the golden file
/// diff is reviewing the contract.
#[test]
fn generated_manifests_match_the_golden_files() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    copy_tree(&fixtures().join("repo"), &repo);
    run(&Options {
        revision: Some("8d367d8".into()),
        updated_at: Some("2026-10-27T00:00:00Z".into()),
        ..options(&repo)
    })
    .unwrap();

    let golden_root = fixtures().join("golden");
    let expected_files = [
        "plugins/index.json",
        "plugins/revision.json",
        "plugins/alice/hello/meta.json",
        "plugins/alice/world/meta.json",
    ];
    for relative in expected_files {
        let expected = fs::read_to_string(golden_root.join(relative))
            .unwrap_or_else(|e| panic!("golden {relative}: {e}"));
        let actual = fs::read_to_string(repo.join(relative))
            .unwrap_or_else(|e| panic!("generated {relative}: {e}"));
        assert_eq!(
            actual, expected,
            "{relative} differs from its golden file — if the change is intended, \
                 update tests/fixtures/golden/{relative}"
        );
    }

    // Nothing else may be produced.
    let mut produced: Vec<String> = walk_manifests(&repo.join("plugins"))
        .unwrap()
        .iter()
        .map(|p| {
            p.strip_prefix(&repo)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    produced.sort();
    let mut expected = expected_files.map(str::to_string).to_vec();
    expected.sort();
    assert_eq!(produced, expected, "unexpected manifest files");
}

#[test]
fn check_passes_on_fresh_output_and_names_what_drifted() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    copy_tree(&fixtures().join("repo"), &repo);
    let mut opts = Options {
        revision: Some("8d367d8".into()),
        updated_at: Some("2026-10-27T00:00:00Z".into()),
        ..options(&repo)
    };
    run(&opts).unwrap();

    // Fresh output → check passes.
    opts.check = true;
    run(&opts).unwrap();

    // A hand-edited manifest → check fails naming that file.
    let index = repo.join("plugins/index.json");
    let tampered = fs::read_to_string(&index).unwrap().replacen(
        "\"version\": \"1.0.0\"",
        "\"version\": \"9.9.9\"",
        1,
    );
    fs::write(&index, tampered).unwrap();
    let error = run(&opts).unwrap_err().to_string();
    assert!(
        error.contains("plugins/index.json is out of date"),
        "{error}"
    );

    // A deleted spec leaves a stale meta.json → check fails naming it too.
    run(&Options {
        check: false,
        ..opts.clone()
    })
    .ok();
    fs::remove_file(repo.join("plugins/alice/world/1.0.0.yaml")).unwrap();
    let error = run(&opts).unwrap_err().to_string();
    assert!(error.contains("alice/world/meta.json is stale"), "{error}");
}

/// The catalog sequence is the anti-rollback anchor: it must advance when
/// the entries change and stay put when they do not (a no-op regeneration
/// must not force a new signature, or the mirror drift check would report
/// drift after every CI run).
#[test]
fn sequence_advances_only_when_entries_change() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    copy_tree(&fixtures().join("repo"), &repo);
    let mut opts = Options {
        updated_at: Some("2026-10-27T00:00:00Z".into()),
        ..options(&repo)
    };
    run(&opts).unwrap();
    let index_path = repo.join("plugins/index.json");
    let first = fs::read_to_string(&index_path).unwrap();
    assert!(first.contains("\"sequence\": 1"), "{first}");

    // Same entries, a later issue time: the header — and the bytes — stay
    // exactly where they were.
    opts.updated_at = Some("2026-11-01T00:00:00Z".into());
    run(&opts).unwrap();
    assert_eq!(fs::read_to_string(&index_path).unwrap(), first);

    // A spec edit advances both the sequence and the issue time.
    let spec = repo.join("plugins/alice/world/1.0.0.yaml");
    let body = fs::read_to_string(&spec).unwrap();
    fs::write(&spec, format!("{body}# changed\n")).unwrap();
    run(&opts).unwrap();
    let second = fs::read_to_string(&index_path).unwrap();
    assert!(second.contains("\"sequence\": 2"), "{second}");
    assert!(second.contains("2026-11-01T00:00:00Z"), "{second}");
    assert_ne!(second, first);
}

#[test]
fn explicit_sequence_pins_the_catalog_version() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    copy_tree(&fixtures().join("repo"), &repo);
    let mut opts = Options {
        updated_at: Some("2026-10-27T00:00:00Z".into()),
        ..options(&repo)
    };
    opts.sequence = Some(42);
    run(&opts).unwrap();
    let index_path = repo.join("plugins/index.json");
    assert!(fs::read_to_string(&index_path)
        .unwrap()
        .contains("\"sequence\": 42"));

    // Without an override the next content change continues from the pin.
    opts.sequence = None;
    let spec = repo.join("plugins/alice/world/1.0.0.yaml");
    let body = fs::read_to_string(&spec).unwrap();
    fs::write(&spec, format!("{body}# changed\n")).unwrap();
    run(&opts).unwrap();
    assert!(fs::read_to_string(&index_path)
        .unwrap()
        .contains("\"sequence\": 43"));

    // 0 never names a catalog version.
    opts.sequence = Some(0);
    let error = run(&opts).unwrap_err().to_string();
    assert!(error.contains("--sequence must be at least 1"), "{error}");
}

/// `--sign-key` signs the review ledger beside the index in the same run:
/// under a trust anchor the manager refuses an unsigned ledger, so one
/// publish must produce both signatures (and `--check` both stale ones).
#[test]
fn sign_key_covers_the_review_ledger_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    copy_tree(&fixtures().join("repo"), &repo);
    let (secret, public) = shared::plugin_signature::generate_keypair().unwrap();
    let key_path = repo.join("key.json");
    fs::write(
        &key_path,
        format!("{{\"key_id\": \"k\", \"secret_key\": \"{secret}\"}}"),
    )
    .unwrap();
    let ledger_path = repo.join("plugins/reviews.json");
    fs::write(&ledger_path, "{\"reviews\": []}\n").unwrap();
    let opts = Options {
        updated_at: Some("2026-10-27T00:00:00Z".into()),
        sign_key: Some(key_path),
        ..options(&repo)
    };

    run(&opts).unwrap();
    let index = fs::read_to_string(repo.join("plugins/index.json")).unwrap();
    let sig = fs::read_to_string(repo.join("plugins/index.json.sig")).unwrap();
    assert_eq!(
        shared::plugin_signature::verify_document(
            index.as_bytes(),
            &sig,
            &public,
            shared::plugin_signature::SIGNATURE_FILE
        )
        .unwrap(),
        "k"
    );
    let ledger = fs::read_to_string(&ledger_path).unwrap();
    let ledger_sig = fs::read_to_string(repo.join("plugins/reviews.json.sig")).unwrap();
    shared::plugin_signature::verify_document(
        ledger.as_bytes(),
        &ledger_sig,
        &public,
        shared::plugin_signature::REVIEWS_SIGNATURE_FILE,
    )
    .unwrap();

    // An edited ledger makes --check fail naming the ledger signature.
    let mut check_opts = opts.clone();
    check_opts.check = true;
    fs::write(&ledger_path, "{\"reviews\": [{\"sha256\": \"bb\"}]}\n").unwrap();
    let error = run(&check_opts).unwrap_err().to_string();
    assert!(error.contains("reviews.json.sig is out of date"), "{error}");

    // No ledger → no ledger signature, index signing unaffected.
    fs::remove_file(&ledger_path).unwrap();
    fs::remove_file(repo.join("plugins/reviews.json.sig")).unwrap();
    run(&opts).unwrap();
    assert!(!repo.join("plugins/reviews.json.sig").exists());
}

#[test]
fn rejects_duplicate_id_version() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_repo(dir.path());
    let plugin = root.join("plugins/alice/oss-upload");
    fs::write(plugin.join("1.0.0-dup.yaml"), SPEC).unwrap();
    let error = run(&options(&root)).unwrap_err().to_string();
    assert!(
        error.contains("duplicate id+version alice/oss-upload@1.0.0"),
        "{error}"
    );
}

#[test]
fn rejects_multiple_namespaces_in_one_source_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_repo(dir.path());
    let other = root.join("plugins/bob/upload");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join("1.0.0.yaml"),
        SPEC.replacen("alice/oss-upload", "bob/upload", 1),
    )
    .unwrap();
    let error = run(&options(&root)).unwrap_err().to_string();
    assert!(error.contains("several namespaces (alice, bob)"), "{error}");
    assert!(error.contains("one source root per namespace"), "{error}");
}
