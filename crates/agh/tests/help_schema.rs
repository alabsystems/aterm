// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Structural tests for `agh` help, schema, and version output.

use std::sync::Arc;

use agh::cli::help::{brief, manual, render, verb_page};
use agh::cli::schema::schema;
use agh::cli::verbs::{GLOBAL_FLAGS, Noun, REFUSED, VERBS, find};
use agh::cli::{Captured, Ctx};
use agh::wire::Endpoints;
use agh::wire::door::is_authorization;
use agh::wire::scripted::ScriptedTransport;
use agh::{API_VERSION, Kind, VERSION};
use aterm_json::Value;

use crate::common;

/// Run one command line in a fresh test context: the exit and what was captured.
fn run(args: &[&str]) -> (u8, Captured) {
    let root = common::scratch("help");
    let (mut ctx, captured) = Ctx::for_tests(
        root.path(),
        Arc::new(ScriptedTransport::new()),
        Endpoints::github_com(),
        common::env(),
    );
    let exit = common::run_cli(&mut ctx, args);
    (exit, captured)
}

#[test]
fn no_flag_name_mentions_a_token() {
    for flag in GLOBAL_FLAGS
        .iter()
        .chain(VERBS.iter().flat_map(|v| v.flags.iter()))
    {
        assert!(!flag.name.contains("token"), "{}", flag.name);
    }
}

#[test]
fn the_manual_spells_no_header_name_and_no_ctl_verb() {
    let manual = manual();
    let words = manual.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    for word in words {
        assert!(!is_authorization(word), "the manual spells {word}");
    }
    assert!(!manual.contains("aterm ctl"));
}

#[test]
fn help_and_dash_dash_help_print_the_manual() {
    for line in [&["help"][..], &["--help"], &["-h"]] {
        let (exit, captured) = run(line);
        assert_eq!(exit, 0, "{line:?}: {}", captured.stderr_text());
        assert_eq!(captured.stdout_text(), manual(), "{line:?}");
        assert!(captured.stderr_text().is_empty());
    }
}

#[test]
fn bare_agh_prints_the_brief() {
    let (exit, captured) = run(&[]);
    assert_eq!(exit, 0);
    assert_eq!(captured.stdout_text(), brief());
}

#[test]
fn a_verb_help_flag_prints_its_page() {
    let verb = find(&["release", "view"]).expect("release view");
    let (exit, captured) = run(&["release", "view", "--help"]);
    assert_eq!(exit, 0);
    assert_eq!(captured.stdout_text(), verb_page(verb));
    let (exit, captured) = run(&["help", "release", "view"]);
    assert_eq!(exit, 0);
    assert_eq!(captured.stdout_text(), verb_page(verb));
}

#[test]
fn every_usage_line_is_in_the_brief_and_every_page_in_the_manual() {
    let brief = brief();
    let manual = manual();
    for verb in VERBS {
        assert!(brief.contains(verb.usage), "{verb:?}");
        assert!(manual.contains(&verb_page(verb)), "{verb:?}");
    }
    for pointer in [
        "agh help VERB",
        "agh help exit",
        "agh help jq",
        "agh help routing",
    ] {
        assert!(brief.contains(pointer), "{pointer}");
    }
}

#[test]
fn a_verb_page_carries_its_flags_gh_and_example() {
    for verb in VERBS {
        let page = verb_page(verb);
        assert!(
            page.starts_with(&format!("usage: {}\n", verb.usage)),
            "{verb:?}"
        );
        for flag in verb.flags {
            assert!(page.contains(flag.name), "{verb:?}: {}", flag.name);
            if let Some(short) = flag.short {
                assert!(
                    page.contains(&format!("-{short}, {}", flag.name)),
                    "{verb:?}"
                );
            }
        }
        if verb.gh != "-" {
            assert!(page.contains(verb.gh), "{verb:?}");
        }
        assert!(page.contains(verb.example), "{verb:?}");
    }
    let api = verb_page(find(&["api"]).expect("api"));
    assert!(
        api.contains("--raw-field VALUE..."),
        "a Repeat flag's VALUE...: {api}"
    );
}

#[test]
fn only_release_wait_tells_of_a_background_wait() {
    let wait = verb_page(find(&["release", "wait"]).expect("release wait"));
    let marker = "Claude Code";
    assert!(wait.contains(marker));
    for verb in VERBS.iter().filter(|v| v.path != ["release", "wait"]) {
        assert!(!verb_page(verb).contains(marker), "{verb:?}");
    }
}

#[test]
fn authored_lines_fit_in_100_characters() {
    let usages: Vec<String> = VERBS
        .iter()
        .flat_map(|v| [format!("  {}", v.usage), format!("usage: {}", v.usage)])
        .collect();
    for line in manual().lines() {
        if usages.iter().any(|usage| line == usage) {
            continue;
        }
        assert!(
            line.chars().count() <= 100,
            "{} chars: {line}",
            line.chars().count()
        );
    }
}

#[test]
fn the_pages_carry_their_tables() {
    let exit = render(&["exit".to_string()]).expect("exit");
    for row in agh::error::exit_rows() {
        assert!(
            exit.contains(row.slug) && exit.contains(row.meaning),
            "{}",
            row.slug
        );
    }
    let jq = render(&["jq".to_string()]).expect("jq");
    for name in [
        "select",
        "from_entries",
        "@base64d",
        "reduce",
        "$ENV",
        "?//",
    ] {
        assert!(jq.contains(name), "{name}");
    }
    let routing = render(&["routing".to_string()]).expect("routing");
    assert!(routing.contains("agh whoami --for REPO"), "{routing}");
    assert!(routing.contains("alabsystems"), "{routing}");
}

#[test]
fn help_takes_the_longest_verb_path() {
    let view = find(&["release", "view"]).expect("release view");
    let words: Vec<String> = ["release", "view", "alabsystems/aterm"]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(render(&words).expect("a page"), verb_page(view));
    let (exit, captured) = run(&["help", "release", "view", "alabsystems/aterm", "v1"]);
    assert_eq!((exit, captured.stdout_text()), (0, verb_page(view)));
}

#[test]
fn a_word_that_begins_verbs_lists_them() {
    let page = render(&["release".to_string()]).expect("release");
    for verb in VERBS.iter().filter(|v| v.path[0] == "release") {
        assert!(page.contains(verb.usage), "{verb:?}");
    }
    assert!(!page.contains("agh repo view"));
}

#[test]
fn help_for_nothing_known_is_usage() {
    let (exit, captured) = run(&["help", "nosuch"]);
    assert_eq!(exit, 2);
    assert!(captured.stdout_text().is_empty());
    assert!(captured.stderr_text().contains("nosuch"));
    let e = render(&["nosuch".to_string()]).expect_err("no such page");
    assert_eq!(e.kind, Kind::Usage);
    assert_eq!(e.next, "agh help");
}

#[test]
fn help_for_a_refused_verb_is_its_row() {
    let row = REFUSED
        .iter()
        .find(|r| r.path == ["auth", "token"])
        .expect("auth token is refused");
    let (exit, captured) = run(&["help", "auth", "token"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let page = captured.stdout_text();
    assert!(page.contains(row.why), "{page}");
    assert!(page.contains(row.next), "{page}");
    let (exit, captured) = run(&["auth", "token", "--help"]);
    assert_eq!(exit, 0);
    assert_eq!(captured.stdout_text(), page);
}

#[test]
fn schema_exit_has_seventeen_rows() {
    let (exit, captured) = run(&["schema", "exit"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let rows = common::stdout_json(&captured);
    let rows = rows.as_array().expect("an array");
    assert_eq!(rows.len(), 17);
    assert_eq!(rows[0]["exit"], Value::from(0u64));
    assert_eq!(rows[0]["slug"], Value::from("ok"));
    let keys: Vec<&String> = rows[1].as_object().expect("a row").keys().collect();
    assert_eq!(keys, ["exit", "meaning", "next", "slug"]);
}

#[test]
fn schema_nouns_and_verbs() {
    let (_, captured) = run(&["schema", "nouns"]);
    let nouns = common::stdout_json(&captured);
    assert_eq!(nouns.as_array().map(Vec::len), Some(20));
    let (_, captured) = run(&["schema", "verbs"]);
    let verbs = common::stdout_json(&captured);
    let verbs = verbs.as_array().expect("an array");
    assert_eq!(verbs.len(), 28);
    let download = verbs
        .iter()
        .find(|v| v["path"] == Value::from("release.download"))
        .expect("release.download");
    assert_eq!(download["noun"], Value::from("download"));
    assert_eq!(download["class"], Value::from("read"));
    let asset = download["flags"]
        .as_array()
        .and_then(|flags| flags.iter().find(|f| f["name"] == Value::from("--asset")))
        .expect("--asset");
    assert_eq!(asset["takes"], Value::from("repeat"));
    assert_eq!(asset["short"], Value::Null);
    let api = verbs
        .iter()
        .find(|v| v["path"] == Value::from("api"))
        .expect("api");
    assert_eq!(api["class"], Value::from("by-method"));
    let method = api["flags"]
        .as_array()
        .and_then(|flags| flags.iter().find(|f| f["name"] == Value::from("--method")))
        .expect("--method");
    assert_eq!(method["short"], Value::from("X"));
    let list = verbs
        .iter()
        .find(|v| v["path"] == Value::from("release.list"))
        .expect("release.list");
    assert_eq!(list["noun"], Value::from("release-list"));
}

#[test]
fn schema_of_a_noun_is_its_row() {
    let (exit, captured) = run(&["schema", "release"]);
    assert_eq!(exit, 0);
    let row = common::stdout_json(&captured);
    assert_eq!(row["name"], Value::from("release"));
    assert_eq!(row["order"], Value::Null);
    assert!(
        row["derived"]
            .as_array()
            .is_some_and(|d| d.contains(&Value::from("latest")))
    );
    assert_eq!(
        schema(Some("release-list")).expect("a noun")["order"],
        Value::from("created_at desc, then id desc")
    );
    let whole = schema(None).expect("everything");
    let keys: Vec<&String> = whole.as_object().expect("an object").keys().collect();
    assert_eq!(keys, ["exit", "nouns", "verbs"]);
    for noun in Noun::ALL {
        assert_eq!(
            schema(Some(noun.slug())).expect("each noun")["name"],
            Value::from(noun.slug())
        );
    }
}

#[test]
fn schema_of_nothing_known_is_usage() {
    let (exit, captured) = run(&["schema", "nosuch"]);
    assert_eq!(exit, 2);
    assert!(captured.stdout_text().is_empty());
    let e = schema(Some("nosuch")).expect_err("no such schema");
    assert_eq!(e.kind, Kind::Usage);
    assert_eq!(e.next, "agh schema");
}

#[test]
fn version_prints_both_versions() {
    let (exit, captured) = run(&["version"]);
    assert_eq!(exit, 0);
    assert_eq!(
        captured.stdout_text(),
        format!("agh {VERSION} (api {API_VERSION})\n")
    );
}
