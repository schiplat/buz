//! Command-line surface for `buz-plugin`.
//!
//! The types live in the library so they can be exercised from `tests/`;
//! `main` only wires them up.

use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "buz-plugin",
    version,
    about = "Plugin authoring & publishing toolchain for Buz"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Scaffold plugins/<ns>/<id>/ with a starter spec and README
    Init {
        /// Plugin id as <namespace>/<name>, e.g. buz/hello
        id: String,
        /// Repository root containing the plugins/ directory
        #[arg(long, default_value = ".", value_hint = clap::ValueHint::DirPath)]
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
        #[arg(value_hint = clap::ValueHint::FilePath)]
        paths: Vec<PathBuf>,
    },
    /// Create an Ed25519 keypair for signing the index
    Keygen {
        /// Key id recorded in the signature (e.g. "buz-official-2026")
        #[arg(long, default_value = "default")]
        key_id: String,
        /// Write the keypair as JSON here instead of stdout
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        out: Option<PathBuf>,
    },
    /// Generate the source-root manifest (+ per-plugin meta.json)
    Index {
        /// Source root containing the namespace directories
        #[arg(long, default_value = "plugins", value_hint = clap::ValueHint::DirPath)]
        plugins_dir: PathBuf,
        /// Timestamp recorded in the index header (ISO-8601, display only)
        #[arg(long)]
        updated_at: Option<String>,
        /// Pin the catalog sequence instead of deriving it from the committed
        /// index (it otherwise advances only when the entries change)
        #[arg(long)]
        sequence: Option<u64>,
        /// Skip per-plugin meta.json output
        #[arg(long, default_value_t = false)]
        no_meta: bool,
        /// Verify the manifests on disk instead of writing (CI gate)
        #[arg(long, default_value_t = false)]
        check: bool,
        /// Sign index.json with this Ed25519 secret key (JSON from `keygen`)
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        sign_key: Option<PathBuf>,
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
    /// Derive revision.json from the catalog published in the index
    Revision {
        /// Source-root index.json to derive from
        #[arg(
            long,
            default_value = "plugins/index.json",
            value_hint = clap::ValueHint::FilePath
        )]
        index: PathBuf,
        /// Write the sentinel here (ignored with --print)
        #[arg(
            long,
            default_value = "plugins/revision.json",
            value_hint = clap::ValueHint::FilePath
        )]
        output: PathBuf,
        /// Print the derived value without writing revision.json
        #[arg(long, default_value_t = false)]
        print: bool,
    },
    /// Generate or refresh the review ledger (ci badges and human first reviews)
    Reviews {
        /// Source root containing index.json and reviews.json
        #[arg(long, default_value = "plugins", value_hint = clap::ValueHint::DirPath)]
        plugins_dir: PathBuf,
        /// Refresh ci-tracked entries for already-blessed buz/* ids
        #[arg(long, default_value_t = false)]
        auto: bool,
        /// Review date (auto: today UTC; required in manual mode)
        #[arg(long)]
        date: Option<String>,
        /// Review commit (auto: current HEAD; required in manual mode)
        #[arg(long)]
        commit: Option<String>,
        /// What the human review covered (manual mode, shown to users)
        #[arg(long, default_value = "static-lint + manual")]
        scope: String,
        /// Template ids to emit manual entries for
        ids: Vec<String>,
    },
    /// Verify that a publish signs exactly the committed catalog
    #[command(name = "check-release")]
    CheckRelease {
        /// The committed plugins/index.json
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        committed: PathBuf,
        /// The freshly regenerated index.json
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        generated: PathBuf,
        /// The previous release's index.json (enables the rollback check)
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        published: Option<PathBuf>,
        /// Source root whose per-plugin meta.json must match the committed index
        #[arg(long, value_hint = clap::ValueHint::DirPath)]
        plugins_dir: Option<PathBuf>,
    },
    /// Verify index, reviews, revision, and per-plugin meta agree
    #[command(name = "check-manifests")]
    CheckManifests {
        /// Source root containing the generated manifests
        #[arg(long, default_value = "plugins", value_hint = clap::ValueHint::DirPath)]
        plugins_dir: PathBuf,
    },
    /// Verify every plugin directory ships a non-empty README.md
    #[command(name = "check-readmes")]
    CheckReadmes {
        /// Source root to scan for plugin directories
        #[arg(long, default_value = "plugins", value_hint = clap::ValueHint::DirPath)]
        plugins_dir: PathBuf,
    },
    /// Print a shell completion script for `buz-plugin` to stdout.
    ///
    /// Example: `buz-plugin completions bash > ~/.local/share/bash-completion/completions/buz-plugin`
    Completions {
        /// Shell the script is generated for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// Write a completion script for `buz-plugin` to `out`.
///
/// Generated from the same `Cli` definition the binary parses, so the script
/// cannot drift from the real flags and subcommands.
pub fn write_completions(
    shell: clap_complete::Shell,
    out: &mut dyn std::io::Write,
) -> std::io::Result<()> {
    let mut script = Vec::new();
    let mut command = Cli::command();
    let bin_name = command.get_name().to_string();
    clap_complete::generate(shell, &mut command, bin_name, &mut script);
    out.write_all(&script)
}

/// Write a completion script to stdout, treating a closed pipe (`… | head`)
/// as a normal end of output rather than an error.
pub fn print_completions(shell: clap_complete::Shell) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    match write_completions(shell, &mut out) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(()),
    }
}
