//! Command-line surface for `buz-runner`.
//!
//! The types live in the library so they can be exercised from `tests/`;
//! `main` only wires them up.

use clap::{CommandFactory, Parser, Subcommand};

/// Buz Runner — lightweight cross‑network task execution agent.
#[derive(Parser)]
#[command(
    name = "buz-runner",
    version = shared::version::VERSION,
    about = "Buz Runner — task execution agent"
)]
pub struct Cli {
    /// Path to TOML configuration file
    #[arg(
        short,
        long,
        global = true,
        default_value = "conf/runner.toml",
        value_hint = clap::ValueHint::FilePath
    )]
    pub config: String,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Print version information and exit
    Version,

    /// Print a shell completion script for `buz-runner` to stdout.
    ///
    /// Example: `buz-runner completions fish > ~/.config/fish/completions/buz-runner.fish`
    Completions {
        /// Shell the script is generated for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// Write a completion script for `buz-runner` to `out`.
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
