use clap::{Parser, ValueEnum};
use plugin_cli::cli::{write_completions, Cli, Command};

#[test]
fn completions_command_parses_known_shells_and_rejects_unknown_ones() {
    let cli = Cli::try_parse_from(["buz-plugin", "completions", "bash"]).unwrap();
    match cli.command {
        Command::Completions { shell } => assert_eq!(shell, clap_complete::Shell::Bash),
        _ => panic!("expected the completions command"),
    }

    assert!(Cli::try_parse_from(["buz-plugin", "completions", "tcsh"]).is_err());
}

#[test]
fn writes_completion_scripts_for_every_supported_shell() {
    for shell in clap_complete::Shell::value_variants() {
        let mut script = Vec::new();
        write_completions(*shell, &mut script).unwrap();
        let script = String::from_utf8(script).expect("completion script is UTF-8");

        assert!(!script.trim().is_empty(), "{shell}: empty script");
        assert!(
            script.contains("buz-plugin"),
            "{shell}: binary name missing"
        );
        for command in [
            "init",
            "validate",
            "keygen",
            "index",
            "mirror",
            "completions",
        ] {
            assert!(script.contains(command), "{shell}: `{command}` missing");
        }
    }
}
