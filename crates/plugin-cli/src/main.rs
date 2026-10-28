//! `buz-plugin` — plugin authoring & publishing toolchain.
//!
//! Responsibility boundary (docs/architecture → Plugin Tooling): this CLI is
//! the authoring moment of the contract (`shared::plugin_spec` is the single
//! authority); the Manager re-runs the same validation when it consumes a
//! template. Feedback here, enforcement there — never two implementations.

use plugin_cli::{indexgen, mirror, scaffold, validate};

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
    /// Create an Ed25519 keypair for signing the index
    Keygen {
        /// Key id recorded in the signature (e.g. "buz-official-2026")
        #[arg(long, default_value = "default")]
        key_id: String,
        /// Write the keypair as JSON here instead of stdout
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Generate the source-root manifest (+ meta.json + revision sentinel)
    Index {
        /// Source root containing the namespace directories
        #[arg(long, default_value = "plugins")]
        plugins_dir: PathBuf,
        /// Write revision.json with this source revision (CI commit sha)
        #[arg(long)]
        revision: Option<String>,
        /// Timestamp recorded beside the revision (ISO-8601, display only)
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
        #[arg(long)]
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
    };
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
