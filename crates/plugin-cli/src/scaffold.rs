//! `buz-plugin init` — scaffold a plugin per the publishing layout
//! (docs → Plugin Contract → Publishing layout).

use crate::layout;
use crate::validate;
use anyhow::Context;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub fn run(
    id: &str,
    dir: &Path,
    publisher: &str,
    author: Option<&str>,
    source_repo: Option<&str>,
    engine: &str,
    version: &str,
    description: Option<&str>,
) -> anyhow::Result<()> {
    if !layout::is_valid_id(id) {
        anyhow::bail!(
            "plugin id {id:?} must be '<namespace>/<name>' with lowercase [a-z0-9-] parts"
        );
    }
    if !layout::is_semver(version) {
        anyhow::bail!("version {version:?} must be three-part semver");
    }
    let (namespace, name) = id.split_once('/').expect("validated two-part id");
    let plugin_dir = dir.join("plugins").join(namespace).join(name);
    let spec_path = plugin_dir.join(format!("{version}.yaml"));
    if spec_path.exists() {
        anyhow::bail!(
            "{}: spec already exists — init never overwrites",
            spec_path.display()
        );
    }

    let description = description
        .map(str::to_string)
        .unwrap_or_else(|| format!("One-line description of what {id} does."));
    let mut spec = format!(
        r#"id: {id}
version: {version}
publisher: {publisher}
"#
    );
    if let Some(author) = author {
        spec.push_str(&format!("author: {author}\n"));
    }
    if let Some(source_repo) = source_repo {
        spec.push_str(&format!("source_repo: {source_repo}\n"));
    }
    spec.push_str(&format!(
        r#"license: Apache-2.0
engine: {engine}
requires_buz: ">={}"
description: {description}
inputs:
  example:
    type: string
    required: true
    description: Replace with the plugin's real inputs.
run: echo "hello from {id} — $INPUT_EXAMPLE"
"#,
        env!("CARGO_PKG_VERSION")
    ));

    let plugin_dir_display = plugin_dir.display().to_string();
    let readme = format!(
        r#"# {id}

{description}

## Inputs

| name | type | required | description |
|------|------|----------|-------------|
| example | string | yes | Replace with the plugin's real inputs. |

## Usage

```yaml
steps:
  - name: Run {name}
    uses: {id}@{version}
    with:
      example: value
```

Run `buz-plugin validate {plugin_dir_display}` before committing.
"#
    );

    std::fs::create_dir_all(&plugin_dir)
        .with_context(|| format!("cannot create {}", plugin_dir.display()))?;
    std::fs::write(&spec_path, spec)
        .with_context(|| format!("cannot write {}", spec_path.display()))?;
    let readme_path = plugin_dir.join("README.md");
    std::fs::write(&readme_path, readme)
        .with_context(|| format!("cannot write {}", readme_path.display()))?;
    println!("scaffolded {} (+ README.md)", spec_path.display());

    // The template starts minimal on purpose: validate immediately and
    // surface what the contract still requires (e.g. community attribution).
    match validate::check(&[plugin_dir.clone()]) {
        Ok(0) => println!("spec passes the contract gate — ready to commit"),
        Ok(failures) => {
            println!("{failures} check(s) failed — fix the items above, then re-run validate")
        }
        Err(error) => println!("validation could not run: {error:#}"),
    }
    Ok(())
}

/// `buz-plugin keygen` — Ed25519 keypair for signing the index.
///
/// Writes `{ key_id, secret_key, public_key }` (base64). The secret stays
/// local (CI secret); the public key goes into every Manager that should
/// trust this source (`plugin_index.public_key`).
pub fn keygen(key_id: &str, out: Option<&Path>) -> anyhow::Result<()> {
    let (secret, public) =
        shared::plugin_signature::generate_keypair().map_err(|e| anyhow::anyhow!("{e}"))?;
    let document = serde_json::json!({
        "key_id": key_id,
        "secret_key": secret,
        "public_key": public,
    });
    let rendered = format!("{}\n", serde_json::to_string_pretty(&document)?);
    match out {
        Some(path) => {
            std::fs::write(path, rendered)
                .with_context(|| format!("cannot write {}", path.display()))?;
            // Owner-only: the file holds the signing secret.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            println!("wrote {} (key id: {key_id}, mode 600)", path.display());
            eprintln!(
                "warning: {} contains the PRIVATE signing key — keep it in a secret store \
                 (CI secret / vault), never commit it",
                path.display()
            );
            println!("public key (safe to publish; configure as plugin_index_public_key):");
            println!("{}", document["public_key"].as_str().unwrap_or(""));
        }
        None => {
            eprintln!(
                "warning: this output contains the PRIVATE signing key — do not paste it into \
                 logs, issues or chat; store it in a secret manager"
            );
            print!("{rendered}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scaffolds_spec_and_readme_that_pass_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        run(
            "alice/hello",
            dir.path(),
            "community",
            Some("alice"),
            Some("https://github.com/alice/buz-plugins"),
            "shell",
            "0.1.0",
            Some("Say hello."),
        )
        .unwrap();
        let spec_path = dir.path().join("plugins/alice/hello/0.1.0.yaml");
        assert!(spec_path.exists());
        assert!(dir.path().join("plugins/alice/hello/README.md").exists());
        let spec = fs::read_to_string(&spec_path).unwrap();
        assert!(spec.contains("id: alice/hello"));
        // The scaffold pins the contract version it was generated against; derive it
        // from the crate so a release bump does not break the test.
        let expected = format!("requires_buz: \">={}\"", env!("CARGO_PKG_VERSION"));
        assert!(
            spec.contains(&expected),
            "scaffolded spec must gate on the current contract: {spec}"
        );
        assert!(spec.contains("$INPUT_EXAMPLE"));
        // The scaffold must clear the same gate the Manager applies.
        assert_eq!(
            validate::check(&[dir.path().join("plugins/alice/hello")]).unwrap(),
            0
        );
    }

    #[test]
    fn never_overwrites_an_existing_spec() {
        let dir = tempfile::tempdir().unwrap();
        run(
            "alice/hello",
            dir.path(),
            "community",
            Some("alice"),
            Some("https://github.com/alice/buz-plugins"),
            "shell",
            "0.1.0",
            None,
        )
        .unwrap();
        let error = run(
            "alice/hello",
            dir.path(),
            "community",
            Some("alice"),
            Some("https://github.com/alice/buz-plugins"),
            "shell",
            "0.1.0",
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("init never overwrites"), "{error}");
    }

    #[test]
    fn rejects_invalid_ids() {
        let dir = tempfile::tempdir().unwrap();
        let error = run(
            "hello",
            dir.path(),
            "community",
            None,
            None,
            "shell",
            "0.1.0",
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("'<namespace>/<name>'"), "{error}");
    }
}
