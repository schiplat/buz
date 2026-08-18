//! Unit tests for `client::config_source` pure helpers.
//! Included via `config_source.rs` under `#[cfg(test)]`.

use std::collections::HashMap;
use std::path::Path;

use super::*;

fn step_with(value: serde_json::Value) -> serde_json::Value {
    value
}

#[test]
fn needs_prepare_detects_auth_and_render() {
    assert!(!needs_prepare(&serde_json::json!({})));
    assert!(!needs_prepare(
        &serde_json::json!({ "config_dest": "/opt/x" })
    ));
    assert!(needs_prepare(&serde_json::json!({
        "config_auth": { "url": "https://example.com/a" }
    })));
    assert!(needs_prepare(&serde_json::json!({
        "config_render": { "template": "env.example", "output": ".env" }
    })));
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
fn config_render_specs_defaults() {
    let step = serde_json::json!({
        "config_render": { "template": "env.example" }
    });
    let specs = config_render_specs(&step).unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].template, "env.example");
    assert_eq!(specs[0].output, "env.example.rendered");
    assert_eq!(specs[0].mode, 0o600);
    assert!(matches!(specs[0].on_missing, MissingPolicy::Fail));
}

#[test]
fn config_render_specs_full_array() {
    let step = serde_json::json!({
        "config_render": [{
            "template": "env.example",
            "output": ".env",
            "mode": "0640",
            "on_missing": "warn",
        }]
    });
    let specs = config_render_specs(&step).unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].output, ".env");
    assert_eq!(specs[0].mode, 0o640);
    assert!(matches!(specs[0].on_missing, MissingPolicy::Warn));
}

#[test]
fn config_render_specs_rejects_missing_template() {
    let step = serde_json::json!({ "config_render": { "output": ".env" } });
    assert!(config_render_specs(&step).unwrap_err().contains("template"));
}

#[test]
fn config_render_specs_rejects_bad_mode() {
    let step = serde_json::json!({ "config_render": { "template": "t", "mode": "notoctal" } });
    assert!(config_render_specs(&step).unwrap_err().contains("octal"));
}

#[test]
fn resolve_dirs() {
    let root = Path::new("/ws");
    let abs = step_with(serde_json::json!({ "config_dest": "/opt/manager" }));
    assert_eq!(resolve_local_dir(&abs, root, 3), Path::new("/opt/manager"));
    assert_eq!(resolve_remote_dir(&abs, "j1", 3), "/opt/manager");

    let rel = step_with(serde_json::json!({ "config_dest": "stack" }));
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
fn render_env_rebuilds_and_detects_change() {
    let tmp = tempfile::tempdir().unwrap();
    let tarball = sample_tarball(tmp.path());
    let dest = tmp.path().join("cfg");
    unpack(&tarball, &dest).unwrap();

    let spec = EnvRenderSpec {
        template: "env.example".into(),
        output: ".env".into(),
        mode: 0o600,
        on_missing: MissingPolicy::Fail,
        backup_keep: 0,
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
fn render_env_rebuild_drops_stale_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let tarball = sample_tarball(tmp.path());
    let dest = tmp.path().join("cfg");
    unpack(&tarball, &dest).unwrap();

    let spec = EnvRenderSpec {
        template: "env.example".into(),
        output: ".env".into(),
        mode: 0o600,
        on_missing: MissingPolicy::Fail,
        backup_keep: 0,
    };
    // Old `.env` carries an operator-added key that the template no longer
    // defines; a full rebuild must drop it instead of preserving it.
    let old = "STALE_KEY=should_be_removed\nMANAGER_TOKEN=old\n";
    let (content, _changed, _forced) =
        render_env_with_old(&dest, &spec, &value_map(), "configs.tar.gz", old).unwrap();
    assert!(
        !content.contains("STALE_KEY"),
        "stale key leaked: {content}"
    );
    assert!(content.contains("MANAGER_TOKEN=tok_secret_value"));
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
        backup_keep: 0,
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
        backup_keep: 0,
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

#[tokio::test]
async fn prepare_local_render_only_writes_env() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("cfg");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(
        dest.join("env.example"),
        "MANAGER_IMAGE=${vars.IMAGE}\nMANAGER_TOKEN=${secrets.TOKEN}\n",
    )
    .unwrap();

    let step = serde_json::json!({
        "config_dest": dest.to_string_lossy(),
        "config_render": [{
            "template": "env.example",
            "output": ".env",
            "mode": "0600",
            "on_missing": "fail",
        }]
    });
    let client = reqwest::Client::new();
    let prep = prepare_local(&client, &step, tmp.path(), 0, &value_map())
        .await
        .unwrap();
    assert_eq!(
        prep.env.get("BUZ_CONFIG_DIR").map(String::as_str),
        Some(dest.to_string_lossy().as_ref())
    );
    assert_eq!(
        prep.env.get("BUZ_ENV_CHANGED").map(String::as_str),
        Some("1")
    );
    let env = std::fs::read_to_string(dest.join(".env")).unwrap();
    assert!(env.contains("MANAGER_TOKEN=tok_secret_value"));
    assert!(prep.forced_redact.contains(&"tok_secret_value".to_string()));
}

#[tokio::test]
async fn prepare_local_render_only_missing_dest_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-dir");
    let step = serde_json::json!({
        "config_dest": missing.to_string_lossy(),
        "config_render": { "template": "env.example", "output": ".env" }
    });
    let client = reqwest::Client::new();
    let err = prepare_local(&client, &step, tmp.path(), 0, &value_map())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not exist"), "got: {err}");
}

#[tokio::test]
async fn prepare_local_render_only_missing_template_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("cfg");
    std::fs::create_dir_all(&dest).unwrap();
    let step = serde_json::json!({
        "config_dest": dest.to_string_lossy(),
        "config_render": { "template": "missing.tpl", "output": ".env" }
    });
    let client = reqwest::Client::new();
    let err = prepare_local(&client, &step, tmp.path(), 0, &value_map())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("template"), "got: {err}");
}

#[test]
fn apply_renders_unions_changed_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("cfg");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("a.tpl"), "A=${vars.IMAGE}\n").unwrap();
    std::fs::write(dest.join("b.tpl"), "B=${secrets.TOKEN}\n").unwrap();
    let specs = vec![
        EnvRenderSpec {
            template: "a.tpl".into(),
            output: "a.env".into(),
            mode: 0o600,
            on_missing: MissingPolicy::Fail,
            backup_keep: 0,
        },
        EnvRenderSpec {
            template: "b.tpl".into(),
            output: "b.env".into(),
            mode: 0o600,
            on_missing: MissingPolicy::Fail,
            backup_keep: 0,
        },
    ];
    let (changed, _) =
        apply_renders_local(&dest, &specs, &value_map(), "render-only", &mut Vec::new()).unwrap();
    assert_eq!(changed, vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn apply_renders_on_missing_warn_does_not_fail() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("cfg");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("env.example"), "X=${secrets.MISSING}\nY=ok\n").unwrap();
    let specs = vec![EnvRenderSpec {
        template: "env.example".into(),
        output: ".env".into(),
        mode: 0o600,
        on_missing: MissingPolicy::Warn,
        backup_keep: 0,
    }];
    apply_renders_local(&dest, &specs, &value_map(), "render-only", &mut Vec::new()).unwrap();
    let env = std::fs::read_to_string(dest.join(".env")).unwrap();
    assert!(env.contains("Y=ok"), "got: {env}");
    assert!(
        env.contains("WARNING") || env.contains("X="),
        "warn mode should keep the line or emit a warning: {env}"
    );
}

fn list_backups(dir: &Path, base: &str) -> Vec<String> {
    let prefix = format!("{base}.bak.");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix))
        .collect();
    names.sort();
    names
}

#[test]
fn backup_output_local_writes_timestamped_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join(".env");
    std::fs::write(&out, "OLD=1\n").unwrap();
    backup_output_local(&out, "OLD=1\n", 3).unwrap();
    let backups = list_backups(tmp.path(), ".env");
    assert_eq!(backups.len(), 1, "backups: {backups:?}");
    let backup = std::fs::read_to_string(tmp.path().join(&backups[0])).unwrap();
    assert_eq!(backup, "OLD=1\n");
}

#[test]
fn prune_backups_local_keeps_only_newest() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join(".env");
    for ts in [
        "20260101-000000",
        "20260101-000001",
        "20260101-000002",
        "20260101-000003",
    ] {
        std::fs::write(tmp.path().join(format!(".env.bak.{ts}")), "x").unwrap();
    }
    prune_backups_local(&out, 3).unwrap();
    let backups = list_backups(tmp.path(), ".env");
    assert_eq!(
        backups,
        vec![
            ".env.bak.20260101-000001".to_string(),
            ".env.bak.20260101-000002".to_string(),
            ".env.bak.20260101-000003".to_string(),
        ]
    );
}
