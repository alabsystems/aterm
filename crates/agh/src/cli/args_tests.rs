// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `Invocation`'s crate-private methods: `repo` and `content` with a literal and a target.
//! The router is a `Validated` the test builds by a struct literal, unvalidated.

use std::collections::BTreeMap;

use super::{Parsed, parse};
use crate::config::{ConfigOrigin, Validated};
use crate::policy::{Named, Router};
use crate::slug::Slug;

fn router() -> Router {
    let mut targets = BTreeMap::new();
    targets.insert(
        "channel".to_string(),
        Slug::parse("alabsystems/aterm").expect("the target's slug"),
    );
    Router::from_validated(Validated {
        profiles: BTreeMap::new(),
        owners: BTreeMap::new(),
        default_read: Vec::new(),
        targets,
        vault: false,
        origin: ConfigOrigin::Text,
        writers: Vec::new(),
    })
}

fn invocation(args: &[&str]) -> super::Invocation {
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    match parse(&args) {
        Ok(Parsed::Run(_, inv)) => inv,
        other => panic!("not a run: {other:?}"),
    }
}

#[test]
fn a_literal_repo_is_named_literally() {
    let inv = invocation(&["repo", "view", "fixture-dev/packs"]);
    let (slug, named) = inv.repo(&router()).expect("a literal");
    assert_eq!(slug, Slug::parse("fixture-dev/packs").expect("slug"));
    match named {
        Named::Literal(t) => assert_eq!(t.slug(), &slug),
        other => panic!("not a literal: {other:?}"),
    }
}

#[test]
fn a_target_resolves_through_the_router() {
    let inv = invocation(&["repo", "view", "@channel"]);
    let (slug, named) = inv.repo(&router()).expect("a target");
    assert_eq!(slug, Slug::parse("alabsystems/aterm").expect("slug"));
    assert_eq!(named, Named::Target("channel".to_string()));
}

#[test]
fn an_unknown_target_is_usage() {
    let inv = invocation(&["repo", "view", "@nowhere"]);
    let error = inv.repo(&router()).expect_err("no such target");
    assert_eq!(error.exit(), 2);
    assert_eq!(error.next, "agh help repo view");
}

#[test]
fn repo_and_content_errors_name_the_verbs_help() {
    for line in [&["repo", "view", "nope"][..], &["repo", "view", "o/r@"][..]] {
        let error = invocation(line).repo(&router()).expect_err("no repository");
        assert_eq!(error.exit(), 2, "{line:?}");
        assert_eq!(error.next, "agh help repo view", "{line:?}");
    }
    for line in [
        &["cat", "o/r@"][..],
        &["cat", "o/r@v1:a/../b"][..],
        &["cat", "@nowhere:README.md"][..],
        &["-R", "nope", "cat", "README.md"][..],
    ] {
        let error = invocation(line)
            .content(&router())
            .expect_err("bad content");
        assert_eq!(error.exit(), 2, "{line:?}");
        assert_eq!(error.next, "agh help cat", "{line:?}");
    }
}

#[test]
fn repo_flag_names_the_repository() {
    let inv = invocation(&["-R", "@channel", "repo", "view"]);
    let (slug, named) = inv.repo(&router()).expect("the -R target");
    assert_eq!(slug, Slug::parse("alabsystems/aterm").expect("slug"));
    assert_eq!(named, Named::Target("channel".to_string()));
}

#[test]
fn content_reads_the_repository_and_the_reference() {
    let inv = invocation(&["cat", "fixture-dev/packs@v1:src/lib.rs"]);
    let (slug, named, at) = inv.content(&router()).expect("content");
    assert_eq!(slug, Slug::parse("fixture-dev/packs").expect("slug"));
    assert!(matches!(named, Named::Literal(_)));
    assert_eq!(at.reference.as_deref(), Some("v1"));
    assert_eq!(at.path.as_deref(), Some("src/lib.rs"));

    let inv = invocation(&["-R", "@channel", "cat", "@main:README.md"]);
    let (slug, named, at) = inv.content(&router()).expect("content under -R");
    assert_eq!(slug, Slug::parse("alabsystems/aterm").expect("slug"));
    assert_eq!(named, Named::Target("channel".to_string()));
    assert_eq!(at.reference.as_deref(), Some("main"));
    assert_eq!(at.path.as_deref(), Some("README.md"));
}

/// A run and its verb, parsed with no tape flag.
fn verb_and_invocation(args: &[&str]) -> (&'static super::Verb, super::Invocation) {
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    match parse(&args) {
        Ok(Parsed::Run(verb, inv)) => (verb, inv),
        other => panic!("not a run: {other:?}"),
    }
}

#[test]
fn rule_six_judges_a_taped_line_under_the_replay() {
    let judged = |verb: &super::Verb, inv: &super::Invocation| {
        super::tape_rule(verb, &inv.global)
            .and_then(|()| super::tape_targets(verb, &inv.global, &inv.positionals, &inv.flags))
    };
    for (line, names) in [
        (&["whoami"][..], "whoami"),
        (&["policy", "check"][..], "policy check"),
        (
            &["auth", "git-credential", "get"][..],
            "auth git-credential",
        ),
        (&["repo", "view", "@channel"][..], "@channel"),
        (&["cat", "@channel:README.md"][..], "@channel"),
        (&["ls", "@channel"][..], "@channel"),
        (&["log", "@channel@main"][..], "@channel"),
    ] {
        let (verb, mut inv) = verb_and_invocation(line);
        assert!(judged(verb, &inv).is_ok(), "{line:?} untaped");
        inv.global.replay = Some(std::path::PathBuf::from("t.atape"));
        let error = judged(verb, &inv).expect_err("refused under a tape");
        assert_eq!(error.exit(), 2, "{line:?}");
        assert!(error.why.contains(names), "{line:?}: {}", error.why);
    }
    for line in [
        &["repo", "view", "o/r"][..],
        &["--as", "dev", "repo", "view", "o/r"][..],
        &["-R", "o/r", "cat", "@main:README.md"][..],
        &["cat", "o/r@main:README.md"][..],
    ] {
        let (verb, mut inv) = verb_and_invocation(line);
        inv.global.replay = Some(std::path::PathBuf::from("t.atape"));
        assert!(judged(verb, &inv).is_ok(), "{line:?}");
    }
}

#[test]
fn a_bare_replay_reads_or_writes_no_file_its_tape_names() {
    for (line, named) in [
        (
            &["release", "download", "o/r", "v1", "--asset", "x"][..],
            ".",
        ),
        (
            &[
                "release", "download", "o/r", "v1", "--asset", "x", "--dir", "d",
            ][..],
            "d",
        ),
        (
            &[
                "release",
                "download",
                "o/r",
                "v1",
                "--asset",
                "x",
                "--output",
                "f",
                "--clobber",
            ][..],
            "f",
        ),
        (
            &[
                "release",
                "create",
                "o/r",
                "v1",
                "--draft",
                "--notes-file",
                "n1",
            ][..],
            "n1",
        ),
        (
            &["release", "edit", "o/r", "v1", "--notes-file", "n2"][..],
            "n2",
        ),
        (
            &["release", "ensure", "o/r", "v1", "--notes-file", "n3"][..],
            "n3",
        ),
    ] {
        let (verb, inv) = verb_and_invocation(line);
        let error = super::bare_replay_rule(verb, &inv).expect_err("a file the tape names");
        assert_eq!(error.exit(), 2, "{line:?}");
        assert!(error.why.ends_with(named), "{line:?}: {}", error.why);
    }
    for line in [
        &[
            "release", "download", "o/r", "v1", "--asset", "x", "--output", "-",
        ][..],
        &["release", "view", "o/r", "v1"][..],
        &["release", "upload", "o/r", "v1", "a.bin"][..],
        &["release", "ensure", "o/r", "v1", "--file", "a.bin"][..],
        &["release", "edit", "o/r", "v1", "--notes", "text"][..],
    ] {
        let (verb, inv) = verb_and_invocation(line);
        assert!(super::bare_replay_rule(verb, &inv).is_ok(), "{line:?}");
    }
}

#[test]
fn the_tape_flags_stripped_are_the_ones_parsed() {
    let strip = |line: &[&str]| {
        let args: Vec<String> = line.iter().map(|a| (*a).to_string()).collect();
        crate::cli::taping::without_tape_flags(&args)
    };
    assert_eq!(
        strip(&["--record", "t.atape", "api", "/x", "-f", "--record=1"]),
        ["api", "/x", "-f", "--record=1"]
    );
    assert_eq!(
        strip(&["api", "/x", "--jq", "--replay", "--record=t.atape"]),
        ["api", "/x", "--jq", "--replay"]
    );
    assert_eq!(
        strip(&[
            "--replay=a",
            "repo",
            "view",
            "o/r",
            "--record",
            "b",
            "--",
            "--record",
            "c"
        ]),
        ["repo", "view", "o/r", "--", "--record", "c"]
    );
    let kept = strip(&["--record", "t.atape", "api", "/x", "-f", "--record=1"]);
    let (_, inv) = verb_and_invocation(&kept.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(inv.flag_all("--raw-field"), ["--record=1"]);
}
