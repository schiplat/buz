//! Ensure `dashboard/dist` exists before rust-embed compiles.
//! Release builds require a real Vite build; debug may use a stub.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist = manifest_dir.join("../../dashboard/dist");
    let index = dist.join("index.html");

    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed={}", index.display());

    if index.is_file() {
        return;
    }

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    if profile == "release" {
        panic!(
            "dashboard/dist/index.html is missing.\n\
             Build the UI before compiling buz-manager:\n\
               cd dashboard && npm ci && npm run build\n\
             Or: make build-dashboard"
        );
    }

    fs::create_dir_all(&dist).expect("create dashboard/dist stub");
    fs::write(
        &index,
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"/><title>Buz</title></head>
<body>
  <p>Frontend stub — run <code>cd dashboard &amp;&amp; npm run build</code> then rebuild manager.</p>
</body>
</html>
"#,
    )
    .expect("write stub index.html");
    println!("cargo:warning=dashboard/dist missing; wrote debug stub index.html");
}
