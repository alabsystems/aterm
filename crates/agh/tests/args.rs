// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The command line's rules (the plan's §4.6.24), each judged by its kind, exit and verb and
//! by the flag or argument its error names.

use std::path::PathBuf;
use std::time::Duration;

use agh::Kind;
use agh::cli::args::{Format, Global, Invocation, ParseError, Parsed, parse, parse_duration};

fn args(line: &[&str]) -> Vec<String> {
    line.iter().map(|a| (*a).to_string()).collect()
}

fn run(line: &[&str]) -> Invocation {
    match parse(&args(line)) {
        Ok(Parsed::Run(_, inv)) => inv,
        other => panic!("{line:?} is not a run: {other:?}"),
    }
}

fn refused(line: &[&str]) -> ParseError {
    parse(&args(line)).expect_err("refused")
}

/// A usage error for `verb` whose why names `names`.
fn usage(line: &[&str], verb: &str, names: &str) {
    let e = refused(line);
    assert_eq!(e.error.kind, Kind::Usage, "{line:?}: {}", e.error.why);
    assert_eq!(e.error.exit(), 2);
    assert_eq!(e.verb, verb, "{line:?}");
    assert!(
        e.error.why.contains(names),
        "{line:?}: {} should name {names}",
        e.error.why
    );
}

#[test]
fn nothing_is_bare() {
    assert_eq!(parse(&[]).expect("bare"), Parsed::Bare);
    assert_eq!(parse(&args(&["--meta"])).expect("bare"), Parsed::Bare);
}

#[test]
fn global_flags_stand_before_and_after_the_verb() {
    let before = run(&["--format", "ndjson", "--limit=3", "repo", "view", "o/r"]);
    let after = run(&["repo", "view", "o/r", "--format=ndjson", "--limit", "3"]);
    assert_eq!(before, after);
    assert_eq!(before.global.format, Format::Ndjson);
    assert_eq!(before.global.limit, Some(3));
    assert_eq!(before.path, ["repo", "view"]);
    assert_eq!(before.verb_name(), "repo.view");
    assert_eq!(before.positionals, ["o/r"]);
}

#[test]
fn dashdash_ends_the_flags() {
    let inv = run(&["cat", "--", "--meta"]);
    assert_eq!(inv.positionals, ["--meta"]);
    assert!(!inv.global.meta);
}

#[test]
fn repo_flag_and_rest() {
    let inv = run(&["-R", "o/r", "release", "view", "v1"]);
    assert_eq!(inv.global.repo.as_deref(), Some("o/r"));
    assert_eq!(inv.rest(), ["v1"]);
    let inv = run(&["release", "view", "o/r", "v1"]);
    assert_eq!(inv.rest(), ["v1"]);
    let inv = run(&["-R", "o/r", "cat"]);
    assert!(inv.positionals.is_empty());
    assert!(inv.rest().is_empty());
}

#[test]
fn rest_skips_content_with_or_without_the_repo_flag() {
    let inv = run(&["-R", "o/r", "find", "@v1", "src"]);
    assert_eq!(inv.rest(), ["src"]);
    let inv = run(&["find", "o/r@v1", "src"]);
    assert_eq!(inv.rest(), ["src"]);
}

#[test]
fn a_refused_path_exits_one() {
    let e = refused(&["release", "create", "o/r"]);
    assert_eq!((e.error.kind, e.error.exit()), (Kind::Refused, 1));
    assert_eq!(e.verb, "release.create");
    let e = refused(&["pr", "list"]);
    assert_eq!(e.verb, "pr");
}

#[test]
fn a_verb_word_alone_names_its_verbs() {
    usage(&["release"], "-", "release list");
}

#[test]
fn an_unknown_verb() {
    usage(&["frobnicate"], "-", "frobnicate");
}

#[test]
fn an_unknown_flag() {
    usage(&["repo", "view", "o/r", "--bogus"], "repo.view", "--bogus");
    usage(
        &["--include-drafts", "release", "view", "o/r"],
        "-",
        "--include-drafts",
    );
}

#[test]
fn a_value_on_a_flag_that_takes_none() {
    usage(&["repo", "view", "o/r", "--meta=1"], "repo.view", "--meta");
}

#[test]
fn a_flag_with_no_value() {
    usage(
        &["repo", "view", "o/r", "--format"],
        "repo.view",
        "--format",
    );
}

#[test]
fn a_repeated_flag() {
    usage(
        &["repo", "view", "o/r", "--meta", "--meta"],
        "repo.view",
        "--meta",
    );
    usage(
        &["--as", "a", "--profile", "b", "repo", "view", "o/r"],
        "-",
        "--profile",
    );
    usage(
        &["repo", "view", "o/r", "--profile", "a", "--as=b"],
        "repo.view",
        "--as",
    );
}

#[test]
fn as_with_anonymous() {
    usage(
        &["repo", "view", "o/r", "--as", "dev", "--anonymous"],
        "repo.view",
        "--anonymous",
    );
}

#[test]
fn value_ranges() {
    usage(
        &["repo", "view", "o/r", "--limit", "0"],
        "repo.view",
        "--limit",
    );
    usage(
        &["repo", "view", "o/r", "--max-pages", "1001"],
        "repo.view",
        "--max-pages",
    );
    usage(
        &["repo", "view", "o/r", "--format", "xml"],
        "repo.view",
        "--format",
    );
    usage(
        &["repo", "view", "o/r", "--timeout", "soon"],
        "repo.view",
        "--timeout",
    );
    assert_eq!(
        run(&["repo", "view", "o/r", "--max-pages", "1000"])
            .global
            .max_pages,
        Some(1000)
    );
}

#[test]
fn timeout_forms() {
    assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    assert_eq!(parse_duration("45"), Some(Duration::from_secs(45)));
    for bad in ["0", "", "s", "1d", "-1", "1.5m"] {
        assert_eq!(parse_duration(bad), None, "{bad}");
    }
    let inv = run(&[
        "release",
        "wait",
        "o/r",
        "v1",
        "published",
        "--timeout",
        "2m",
    ]);
    assert_eq!(inv.global.timeout, Some(Duration::from_secs(120)));
}

#[test]
fn jq_with_another_format() {
    usage(
        &["repo", "view", "o/r", "--jq", ".x", "--format", "tsv"],
        "repo.view",
        "--jq",
    );
}

#[test]
fn help_anywhere_before_dashdash() {
    assert_eq!(
        parse(&args(&["release", "view", "-h", "o/r"])).expect("help"),
        Parsed::Help(args(&["release", "view"]))
    );
    assert_eq!(
        parse(&args(&["--help", "--format", "json", "release"])).expect("help"),
        Parsed::Help(args(&["release"]))
    );
    let inv = run(&["cat", "--", "-h"]);
    assert_eq!(inv.positionals, ["-h"]);
}

#[test]
fn positionals_too_few_and_too_many() {
    usage(&["compare", "o/r"], "compare", "RANGE");
    usage(&["repo", "view", "o/r", "extra"], "repo.view", "extra");
    usage(&["release", "wait", "o/r", "v1"], "release.wait", "COND");
    let inv = run(&["release", "wait", "o/r", "v1", "published", "assets=2"]);
    assert_eq!(inv.rest(), ["v1", "published", "assets=2"]);
}

#[test]
fn repo_flag_on_a_verb_without_repo() {
    usage(
        &["-R", "o/r", "repo", "list", "alabsystems"],
        "repo.list",
        "-R",
    );
    assert!(matches!(
        parse(&args(&["-R", "o/r", "api", "/repositories/1"])),
        Ok(Parsed::Run(..))
    ));
}

#[test]
fn a_repeat_flag_keeps_every_value() {
    let inv = run(&[
        "release",
        "download",
        "o/r",
        "v1",
        "--asset",
        "a",
        "--asset=b",
        "--dir",
        "d",
    ]);
    assert_eq!(inv.flag_all("--asset"), ["a", "b"]);
    assert_eq!(inv.flag("--dir"), Some("d"));
    assert!(inv.has("--dir"));
    assert!(!inv.has("--clobber"));
}

#[test]
fn short_flags_take_the_next_argument() {
    let inv = run(&[
        "api", "/user", "-X", "HEAD", "-f", "a=1", "-F", "b=2", "-q", ".x",
    ]);
    assert_eq!(inv.flag("--method"), Some("HEAD"));
    assert_eq!(inv.flag_all("--raw-field"), ["a=1"]);
    assert_eq!(inv.flag_all("--field"), ["b=2"]);
    assert_eq!(inv.global.jq.as_deref(), Some(".x"));
}

#[test]
fn replay_alone_runs_the_tape() {
    let expected = Global {
        replay: Some(PathBuf::from("t.atape")),
        ..Global::default()
    };
    assert_eq!(
        parse(&args(&["--replay", "t.atape"])).expect("taped"),
        Parsed::ReplayTaped(expected)
    );
}

#[test]
fn record_alone_needs_a_verb() {
    usage(&["--record", "t.atape"], "-", "--record");
}

#[test]
fn record_and_replay_of_one_file() {
    usage(
        &["--record", "x", "--replay", "x", "repo", "view", "o/r"],
        "repo.view",
        "--record",
    );
}

#[test]
fn tapes_are_anonymous() {
    usage(
        &["--record", "t.atape", "--as", "dev", "repo", "view", "o/r"],
        "repo.view",
        "--as",
    );
    usage(&["--replay", "t.atape", "--as", "dev"], "-", "--as");
    usage(&["--replay", "t.atape", "--profile", "dev"], "-", "--as");
}

#[test]
fn a_bare_replay_takes_no_other_flag() {
    usage(&["--replay", "t.atape", "--format", "tsv"], "-", "--format");
    usage(&["-R", "o/r", "--replay", "t.atape"], "-", "--repo");
    usage(&["--replay", "t.atape", "--meta"], "-", "--meta");
}

#[test]
fn local_verbs_and_downloads_cannot_be_taped() {
    usage(&["--replay", "t.atape", "whoami"], "whoami", "whoami");
    usage(
        &["auth", "add", "n", "--replay", "t.atape"],
        "auth.add",
        "auth add",
    );
    usage(
        &[
            "release",
            "download",
            "o/r",
            "v1",
            "--asset",
            "a",
            "--replay=t.atape",
        ],
        "release.download",
        "release download",
    );
}

#[test]
fn tape_flags_in_either_form_on_either_side() {
    for line in [
        &["--record", "t.atape", "repo", "view", "o/r"][..],
        &["repo", "view", "o/r", "--record=t.atape"][..],
    ] {
        assert_eq!(run(line).global.record, Some(PathBuf::from("t.atape")));
    }
    for line in [
        &["--replay=t.atape", "repo", "view", "o/r"][..],
        &["repo", "view", "o/r", "--replay", "t.atape"][..],
    ] {
        assert_eq!(run(line).global.replay, Some(PathBuf::from("t.atape")));
    }
}

#[test]
fn an_empty_tape_path_is_usage() {
    usage(
        &["repo", "view", "o/r", "--record="],
        "repo.view",
        "--record",
    );
    usage(&["--replay=", "repo", "view", "o/r"], "-", "--replay");
}

#[test]
fn replay_with_record_and_no_verb_rerecords_the_tape() {
    let expected = Global {
        replay: Some(PathBuf::from("a.atape")),
        record: Some(PathBuf::from("b.atape")),
        ..Global::default()
    };
    assert_eq!(
        parse(&args(&["--replay", "a.atape", "--record", "b.atape"])).expect("taped"),
        Parsed::ReplayTaped(expected)
    );
}

#[test]
fn timeout_is_at_most_a_day() {
    let inv = run(&["repo", "view", "o/r", "--timeout", "24h"]);
    assert_eq!(inv.global.timeout, Some(Duration::from_secs(86_400)));
    for long in ["86401", "25h", "1441m", "18446744073709551615"] {
        usage(
            &["repo", "view", "o/r", "--timeout", long],
            "repo.view",
            "--timeout",
        );
    }
}
