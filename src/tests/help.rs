//! `--help` snapshots.

use super::*;

#[test]
fn cli_definition_is_consistent() {
    Cli::command().debug_assert();
}

fn render_help_snapshot(path: &[&str]) -> String {
    let mut cli = Cli::command()
        .term_width(100)
        .color(clap::ColorChoice::Never);
    cli.build();
    let mut command = &mut cli;
    for name in path {
        command = command
            .find_subcommand_mut(name)
            .unwrap_or_else(|| panic!("subcommand `{name}` exists"));
    }
    let help = command.render_help().to_string();
    assert_no_emoji(&help);
    help
}

#[test]
fn help_snapshot_top_level() {
    insta::assert_snapshot!("help_top_level", render_help_snapshot(&[]));
}

#[test]
fn help_snapshot_endpoint() {
    insta::assert_snapshot!("help_endpoint", render_help_snapshot(&["endpoint"]));
}

#[test]
fn help_snapshot_endpoint_list() {
    insta::assert_snapshot!(
        "help_endpoint_list",
        render_help_snapshot(&["endpoint", "list"])
    );
}

#[test]
fn help_snapshot_tunnel() {
    insta::assert_snapshot!("help_tunnel", render_help_snapshot(&["tunnel"]));
}

#[test]
fn help_snapshot_tunnel_events() {
    insta::assert_snapshot!(
        "help_tunnel_events",
        render_help_snapshot(&["tunnel", "events"])
    );
}

#[test]
fn help_snapshot_anon() {
    insta::assert_snapshot!("help_anon", render_help_snapshot(&["anon"]));
}

#[test]
fn help_snapshot_anon_create() {
    insta::assert_snapshot!(
        "help_anon_create",
        render_help_snapshot(&["anon", "create"])
    );
}

#[test]
fn help_snapshot_anon_tunnel() {
    insta::assert_snapshot!(
        "help_anon_tunnel",
        render_help_snapshot(&["anon", "tunnel"])
    );
}

#[test]
fn help_snapshot_monitor() {
    insta::assert_snapshot!("help_monitor", render_help_snapshot(&["monitor"]));
}

#[test]
fn help_snapshot_monitor_create() {
    insta::assert_snapshot!(
        "help_monitor_create",
        render_help_snapshot(&["monitor", "create"])
    );
}

#[test]
fn help_snapshot_cases_run() {
    insta::assert_snapshot!("help_cases_run", render_help_snapshot(&["cases", "run"]));
}

#[test]
fn help_snapshot_cases() {
    insta::assert_snapshot!("help_cases", render_help_snapshot(&["cases"]));
}

#[test]
fn help_snapshot_cases_replay() {
    insta::assert_snapshot!(
        "help_cases_replay",
        render_help_snapshot(&["cases", "replay"])
    );
}

#[test]
fn help_snapshot_cases_runs_wait() {
    insta::assert_snapshot!(
        "help_cases_runs_wait",
        render_help_snapshot(&["cases", "runs", "wait"])
    );
}

#[test]
fn help_snapshot_share_create() {
    insta::assert_snapshot!(
        "help_share_create",
        render_help_snapshot(&["share", "create"])
    );
}

#[test]
fn help_snapshot_completions() {
    let help = render_help_snapshot(&["completions"]);
    assert!(
        help.contains("[possible values: bash, zsh, fish, powershell, elvish]"),
        "completions help must list the visible shell names:\n{help}"
    );
    assert!(
        !help.contains("power-shell"),
        "power-shell is a hidden alias and must not appear in help:\n{help}"
    );
    insta::assert_snapshot!("help_completions", help);
}
