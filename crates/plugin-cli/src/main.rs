//! `buz-plugin` — plugin authoring & publishing toolchain.
//!
//! Responsibility boundary (docs/architecture → Plugin Tooling): this CLI is
//! the authoring moment of the contract (`shared::step_spec` is the single
//! authority); the Manager re-runs the same validation when it consumes a
//! template. Feedback here, enforcement there — never two implementations.

mod indexgen;
mod layout;
mod mirror;
mod scaffold;
mod validate;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "buz-plugin",
    version,
    about = "Plugin authoring & publishing toolchain for Buz"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold plugins/<ns>/<id>/ with a starter spec and README
    Init {
        /// Plugin id as <namespace>/<name>, e.g. buz/hello
        id: String,
        /// Repository root containing the plugins/ directory
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// community | official (community requires --author and --source-repo)
        #[arg(long, default_value = "community")]
        publisher: String,
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        source_repo: Option<String>,
        #[arg(long, default_value = "shell")]
        engine: String,
        #[arg(long, default_value = "0.1.0")]
        version: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// Validate spec files or directories with the Manager's save-time gate
    Validate {
        /// Spec files or directories containing *.yaml / *.yml
        paths: Vec<PathBuf>,
    },
    /// Generate per-namespace indexes (+ meta.json + revision sentinels)
    Index {
        #[arg(long, default_value = "plugins")]
        plugins_dir: PathBuf,
        /// Additionally write a merged repo-root-relative index (site view)
        #[arg(long)]
        merged: Option<PathBuf>,
        /// Write revision.json sentinels with this source revision (CI commit sha)
        #[arg(long)]
        revision: Option<String>,
        /// Timestamp recorded beside the revision (ISO-8601, display only)
        #[arg(long)]
        updated_at: Option<String>,
        /// Generate the index for one namespace only
        #[arg(long)]
        namespace: Option<String>,
        /// Skip per-plugin meta.json output
        #[arg(long, default_value_t = false)]
        no_meta: bool,
    },
    /// Compare the revision sentinel between canonical origin and mirror
    #[command(name = "mirror")]
    MirrorCheck {
        /// Canonical source index.json URL
        source: String,
        /// Mirror index.json URL
        mirror: String,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
}

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
        Command::Index {
            plugins_dir,
            merged,
            revision,
            updated_at,
            namespace,
            no_meta,
        } => indexgen::run(&indexgen::Options {
            plugins_dir: plugins_dir.clone(),
            merged: merged.clone(),
            revision: revision.clone(),
            updated_at: updated_at.clone(),
            namespace: namespace.clone(),
            no_meta: *no_meta,
        }),
        Command::MirrorCheck {
            source,
            mirror,
            json,
        } => mirror::run(source, mirror, *json),
    };
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
