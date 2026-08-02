//! Unit tests for `client::config_source` pure helpers.
//! Included via `config_source.rs` under `#[cfg(test)]`.

use std::collections::HashMap;
use std::path::Path;

use super::*;

fn step_with(value: serde_json::Value) -> serde_json::Value {
    value
}

#[test]
fn config_url_missing_is_error() {
    let step = serde_json::json!({});
    assert!(config_url(&step).is_err());
}

#[test]
fn config_url_extracts_signed_url() {
    let step = serde_json::json!({ "config_auth": { "url": "https://oss.example.com/obj?Expires=1", "object_key": "config_tpl/a/1.0" } });
    assert_eq!(
        config_url(&step).unwrap(),
        "https://oss.example.com/obj?Expires=1"
    );
    assert_eq!(archive_label(&step), "config_tpl/a/1.0");
    assert_eq!(archive_label(&serde_json::json!({})), "configs.tar.gz");
}

#[test]
fn env_render_spec_defaults() {
    let step = serde_json::json!({
        "env_render": { "template": "env.example" }
    });
    let spec = env_render_spec(&step).unwrap().unwrap();
    assert_eq!(spec.template, "env.example");
    assert_eq!(spec.output, "env.example.rendered");
    assert_eq!(spec.mode, 0o600);
    assert!(matches!(spec.on_missing, MissingPolicy::Fail));
}

#[test]
fn env_render_spec_full() {
    let step = serde_json::json!({
        "env_render": {
            "template": "env.example",
            "output": ".env",
            "mode": "0640",
            "on_missing": "warn",
        }
    });
    let spec = env_render_spec(&step).unwrap().unwrap();
    assert_eq!(spec.output, ".env");
    assert_eq!(spec.mode, 0o640);
    assert!(matches!(spec.on_missing, MissingPolicy::Warn));
}

#[test]
fn env_render_spec_rejects_missing_template() {
    let step = serde_json::json!({ "env_render": { "output": ".env" } });
    assert!(env_render_spec(&step).unwrap_err().contains("template"));
}

#[test]
fn env_render_spec_rejects_bad_mode() {
    let step = serde_json::json!({ "env_render": { "template": "t", "mode": "notoctal" } });
    assert!(env_render_spec(&step).unwrap_err().contains("octal"));
}

#[test]
fn resolve_dirs() {
    let root = Path::new("/ws");
    let abs = step_with(serde_json::json!({ "config_dir": "/opt/manager" }));
    assert_eq!(resolve_local_dir(&abs, root, 3), Path::new("/opt/manager"));
    assert_eq!(resolve_remote_dir(&abs, "j1", 3), "/opt/manager");

    let rel = step_with(serde_json::json!({ "config_dir": "stack" }));
    assert_eq!(resolve_local_dir(&rel, root, 3), Path::new("/ws/stack"));
    assert_eq!(resolve_remote_dir(&rel, "j1", 3), "/tmp/buz-config/stack");

    let none = step_with(serde_json::json!({}));
    assert_eq!(resolve_local_dir(&none, root, 3), Path::new("/ws/config-3"));
    assert_eq!(resolve_remote_dir(&none, "j1", 3), "/tmp/buz-config/j1-3");
}

#[test]
fn set_changed_env_sets_keys() {
    let mut env = HashMap::new();
    set_changed_env(&mut env, &[]);
    assert_eq!(env.get("BUZ_ENV_CHANGED").map(String::as_str), Some("0"));
    assert!(!env.contains_key("BUZ_ENV_CHANGED_KEYS"));

    let mut env = HashMap::new();
    set_changed_env(&mut env, &["A".into(), "B".into()]);
    assert_eq!(env.get("BUZ_ENV_CHANGED").map(String::as_str), Some("1"));
    assert_eq!(
        env.get("BUZ_ENV_CHANGED_KEYS").map(String::as_str),
        Some("A,B")
    );
}

fn sample_tarball(dir: &Path) -> Vec<u8> {
    let file = dir.join("env.example");
    std::fs::write(
        &file,
        "MANAGER_IMAGE=${vars.IMAGE}\nMANAGER_TOKEN=${secrets.TOKEN}\n",
    )
    .unwrap();
    let tar_gz = dir.join("bundle.tar.gz");
    let status = std::process::Command::new("tar")
        .current_dir(dir)
        .args(["-czf", "bundle.tar.gz", "env.example"])
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(&tar_gz).unwrap()
}

#[test]
fn unpack_extracts_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let tarball = sample_tarball(tmp.path());
    let dest = tmp.path().join("out");
    unpack(&tarball, &dest).unwrap();
    assert!(dest.join("env.example").exists());
}

#[test]
fn unpack_fails_on_garbage() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out");
    assert!(unpack(b"not a tarball", &dest).is_err());
}

fn value_map() -> ValueMap {
    let mut m = ValueMap::new();
    m.secrets.insert("TOKEN".into(), "tok_secret_value".into());
    m.vars
        .insert("IMAGE".into(), "reg.example.com/a/b:1.0".into());
    m
}

#[test]
fn render_env_merges_and_detects_change() {
    let tmp = tempfile::tempdir().unwrap();
    let tarball = sample_tarball(tmp.path());
    let dest = tmp.path().join("cfg");
    unpack(&tarball, &dest).unwrap();

    let spec = EnvRenderSpec {
        template: "env.example".into(),
        output: ".env".into(),
        mode: 0o600,
        on_missing: MissingPolicy::Fail,
    };
    let (content, changed, forced) =
        render_env_with_old(&dest, &spec, &value_map(), "configs.tar.gz", "").unwrap();
    assert!(content.contains("MANAGER_IMAGE=reg.example.com/a/b:1.0"));
    assert!(content.contains("MANAGER_TOKEN=tok_secret_value"));
    assert_eq!(
        changed,
        vec!["MANAGER_IMAGE".to_string(), "MANAGER_TOKEN".to_string()]
    );
    assert_eq!(forced, vec!["tok_secret_value".to_string()]);
}

#[test]
fn render_env_fails_fast_on_missing_secret() {
    let tmp = tempfile::tempdir().unwrap();
    let tarball = sample_tarball(tmp.path());
    let dest = tmp.path().join("cfg");
    unpack(&tarball, &dest).unwrap();

    let spec = EnvRenderSpec {
        template: "env.example".into(),
        output: ".env".into(),
        mode: 0o600,
        on_missing: MissingPolicy::Fail,
    };
    let m = value_map();
    // remove the token → template references an unresolved secret
    let mut m2 = ValueMap::new();
    m2.secrets = m
        .secrets
        .clone()
        .into_iter()
        .filter(|(k, _)| k != "TOKEN")
        .collect();
    m2.vars = m.vars.clone();
    let err = render_env_with_old(&dest, &spec, &m2, "configs.tar.gz", "")
        .unwrap_err()
        .to_string();
    assert!(err.contains("unresolved reference"), "got: {err}");
}

#[test]
fn render_env_unchanged_detects_no_change() {
    let tmp = tempfile::tempdir().unwrap();
    let tarball = sample_tarball(tmp.path());
    let dest = tmp.path().join("cfg");
    unpack(&tarball, &dest).unwrap();

    let spec = EnvRenderSpec {
        template: "env.example".into(),
        output: ".env".into(),
        mode: 0o600,
        on_missing: MissingPolicy::Fail,
    };
    let (content, _, _) =
        render_env_with_old(&dest, &spec, &value_map(), "configs.tar.gz", "").unwrap();
    // Rendering against identical old content yields no changed keys.
    let (_, changed, _) =
        render_env_with_old(&dest, &spec, &value_map(), "configs.tar.gz", &content).unwrap();
    assert!(changed.is_empty());
}

#[test]
fn write_atomic_writes_mode_and_replaces() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("sub").join(".env");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    write_atomic(&out, "A=1\n", 0o600).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "A=1\n");
    write_atomic(&out, "A=2\n", 0o600).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "A=2\n");
    // No leftover temp files
    let leftovers: Vec<_> = std::fs::read_dir(out.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("buz-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}
