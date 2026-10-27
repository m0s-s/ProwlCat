//! The command line is the scripting interface: it must behave exactly as it always has, and the
//! interactive interface must never open when there is no terminal to show it on.
//!
//! Every process here has piped stdout and a null stdin, so none of them is a terminal.

use std::process::{Command, Output, Stdio};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/sample_breach.txt");

fn prowlcat(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_prowlcat"));
    cmd.args(args)
        .env_remove("PROWLCAT_UI")
        .env_remove("PROWLCAT_LOG_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("run prowlcat")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exit code")
}

#[test]
fn bare_command_without_a_terminal_is_the_classic_usage_error() {
    let o = prowlcat(&[], &[]);
    assert_eq!(code(&o), 2);
    let err = text(&o.stderr);
    assert!(err.contains("requires a subcommand"), "{err}");
    assert!(err.contains("Usage: prowlcat"), "{err}");
    assert!(o.stdout.is_empty());
}

#[test]
fn ui_cli_gives_the_same_classic_error() {
    let plain = prowlcat(&[], &[]);
    for o in [prowlcat(&["--ui", "cli"], &[]), prowlcat(&[], &[("PROWLCAT_UI", "cli")])] {
        assert_eq!(code(&o), 2);
        assert_eq!(text(&o.stderr), text(&plain.stderr), "byte for byte the message users already know");
    }
}

#[test]
fn the_interface_refuses_to_start_without_a_terminal() {
    for args in [&["tui"][..], &["--ui", "tui"], &["--ui", "tui", "stats", "-p", "."], &["tui", "acme.corp", "-p", "."]] {
        let o = prowlcat(args, &[]);
        assert_eq!(code(&o), 2, "{args:?}");
        let err = text(&o.stderr);
        assert!(err.contains("needs a terminal"), "{args:?}: {err}");
        assert!(err.contains("--ui cli"), "the message says how to get the scripting mode");
        assert!(o.stdout.is_empty(), "no escape sequences leak into a pipe");
    }
}

#[test]
fn env_var_selects_the_interface_too() {
    let o = prowlcat(&[], &[("PROWLCAT_UI", "tui")]);
    assert_eq!(code(&o), 2);
    assert!(text(&o.stderr).contains("needs a terminal"));
}

#[test]
fn contradictory_flags_are_an_error() {
    let o = prowlcat(&["--ui", "cli", "tui"], &[]);
    assert_eq!(code(&o), 2);
    assert!(text(&o.stderr).contains("conflicts with `--ui cli`"), "{}", text(&o.stderr));
}

#[test]
fn unknown_ui_value_is_rejected() {
    let o = prowlcat(&["--ui", "fancy", "stats", "-p", FIXTURE], &[]);
    assert_eq!(code(&o), 2);
    assert!(text(&o.stderr).contains("invalid value"), "{}", text(&o.stderr));
}

#[test]
fn search_output_is_identical_in_every_classic_spelling() {
    let args = ["search", "user@example.com", "-p", FIXTURE, "--no-progress"];
    let base = prowlcat(&args, &[]);
    assert_eq!(code(&base), 0, "{}", text(&base.stderr));
    let out = text(&base.stdout);
    assert!(out.contains("ProwlCat | 1 targets"), "{out}");
    assert!(out.contains("user@example.com"));
    assert!(!out.contains('\x1b'), "no terminal control sequences in piped output");

    let with_flag = prowlcat(&["--ui", "cli", "search", "user@example.com", "-p", FIXTURE, "--no-progress"], &[]);
    let with_auto = prowlcat(&["--ui", "auto", "search", "user@example.com", "-p", FIXTURE, "--no-progress"], &[]);
    let with_env = prowlcat(&args, &[("PROWLCAT_UI", "cli")]);
    for other in [with_flag, with_auto, with_env] {
        assert_eq!(code(&other), 0);
        // Elapsed time differs run to run; everything else is identical.
        let strip = |s: &str| s.lines().filter(|l| !l.starts_with("ProwlCat |")).collect::<Vec<_>>().join("\n");
        assert_eq!(strip(&text(&other.stdout)), strip(&out));
    }
}

#[test]
fn exit_codes_still_mean_matches_none_and_error() {
    let hit = prowlcat(&["search", "user@example.com", "-p", FIXTURE, "--no-progress"], &[]);
    assert_eq!(code(&hit), 0);
    let miss = prowlcat(&["search", "nobody@nowhere.invalid", "-p", FIXTURE, "--no-progress"], &[]);
    assert_eq!(code(&miss), 1);
    let err = prowlcat(&["search", "a@b.com", "-p", "/definitely/not/here", "--no-progress"], &[]);
    assert_eq!(code(&err), 2);
    let none = prowlcat(&["search", "-p", FIXTURE, "--no-progress"], &[]);
    assert_eq!(code(&none), 2);
    assert!(text(&none.stderr).contains("no targets provided"));
}

#[test]
fn stats_and_export_are_untouched() {
    let stats = prowlcat(&["stats", "-p", FIXTURE], &[]);
    assert_eq!(code(&stats), 0);
    assert!(text(&stats.stdout).starts_with("ProwlCat stats"));

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("hits.json");
    let o = prowlcat(&["search", "user@example.com", "-p", FIXTURE, "--no-progress", "-o", out.to_str().unwrap()], &[]);
    assert_eq!(code(&o), 0);
    assert!(std::fs::read_to_string(&out).unwrap().contains("user@example.com"));
}

#[test]
fn output_flags_are_not_silently_ignored_by_the_interface() {
    let o = prowlcat(&["--ui", "tui", "search", "x.com", "-p", FIXTURE, "-o", "out.json"], &[]);
    assert_eq!(code(&o), 2);
    assert!(text(&o.stderr).contains("--output"), "{}", text(&o.stderr));
}

#[test]
fn help_documents_both_modes() {
    let o = prowlcat(&["--help"], &[]);
    assert_eq!(code(&o), 0);
    let help = text(&o.stdout);
    assert!(help.contains("--ui <MODE>"), "{help}");
    assert!(help.contains("tui"));
    assert!(help.contains("PROWLCAT_UI"));
    let sub = prowlcat(&["tui", "--help"], &[]);
    assert!(text(&sub.stdout).contains("--no-animation"));
}
