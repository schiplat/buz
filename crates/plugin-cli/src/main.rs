//! `buz-plugin` — plugin authoring & publishing toolchain.
//!
//! Responsibility boundary (docs/architecture → Plugin Tooling): this CLI is
//! the authoring moment of the contract (`shared::plugin_spec` is the single
//! authority); the Manager re-runs the same validation when it consumes a
//! template. Feedback here, enforcement there — never two implementations.

use plugin_cli::cli::{print_completions, Cli, Command};
use plugin_cli::{indexgen, mirror, scaffold, validate};

use clap::Parser;

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Init {
            id,
            dir,
            publisher,
            author,
            source_repo,
            engine,
            version,
            description,
        } => scaffold::run(
            id,
            dir,
            publisher,
            author.as_deref(),
            source_repo.as_deref(),
            engine,
            version,
            description.as_deref(),
        ),
        Command::Validate { paths } => validate::run(paths),
        Command::Keygen { key_id, out } => scaffold::keygen(key_id, out.as_deref()),
        Command::Index {
            plugins_dir,
            revision,
            updated_at,
            sequence,
            no_meta,
            check,
            sign_key,
        } => indexgen::run(&indexgen::Options {
            plugins_dir: plugins_dir.clone(),
            revision: revision.clone(),
            updated_at: updated_at.clone(),
            sequence: *sequence,
            no_meta: *no_meta,
            check: *check,
            sign_key: sign_key.clone(),
        }),
        Command::MirrorCheck {
            source,
            mirror,
            json,
        } => mirror::run(source, mirror, *json),
        Command::Completions { shell } => print_completions(*shell),
    };
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
