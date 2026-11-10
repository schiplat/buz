//! `buz-plugin` — plugin authoring & publishing toolchain.
//!
//! Responsibility boundary (docs/architecture → Plugin Tooling): this CLI is
//! the authoring moment of the contract (`shared::plugin_spec` is the single
//! authority); the Manager re-runs the same validation when it consumes a
//! template. Feedback here, enforcement there — never two implementations.

use plugin_cli::cli::{print_completions, Cli, Command};
use plugin_cli::{indexgen, mirror, release_check, reviews, revision, scaffold, validate};

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
        Command::Revision {
            index,
            output,
            print,
        } => revision::run(&revision::Options {
            index: index.clone(),
            output: output.clone(),
            print_only: *print,
        }),
        Command::Reviews {
            plugins_dir,
            auto,
            date,
            commit,
            scope,
            ids,
        } => reviews::run(&reviews::Options {
            plugins_dir: plugins_dir.clone(),
            auto: *auto,
            date: date.clone(),
            commit: commit.clone(),
            scope: scope.clone(),
            ids: ids.clone(),
        }),
        Command::CheckRelease {
            committed,
            generated,
            published,
            plugins_dir,
        } => release_check::run(&release_check::Options {
            committed: committed.clone(),
            generated: generated.clone(),
            published: published.clone(),
            plugins_dir: plugins_dir.clone(),
        }),
        Command::Completions { shell } => print_completions(*shell),
    };
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
