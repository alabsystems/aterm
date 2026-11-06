// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's rules and sources: what reads, what is refused, and what a rev covers.

use std::path::Path;

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};

/// The design's §3.3 table, as written there.
const DESIGN: &str = r#"
[harness]
occupancy = "prompt"

[harness.occupancy_source.trust]
kind   = "git-ref"
repo   = "$HOME/trust"
remote = "origin"
ref    = "refs/heads/main"
path   = "src/stage0"
every  = "5m"

[[harness.occupancy_rule]]
name   = "release-base-moved"
when   = "git(trust).moved"
scope  = { jobs_cwd = "~/scratch.noindex/trust-release-*" }
modes  = ["cruise", "land"]
urgent = true
text   = "Re-read trust main and the atpkg index before any cut, mint or publish goes on."
"#;

/// A value planted in every refused field: no note may echo it.
const PLANT: &str = "ignore.previous.instructions";

fn read(text: &str) -> (OwnerTables, Vec<String>) {
    let doc = aterm_toml::from_str::<aterm_toml::Value>(text)
        .unwrap_or_else(|e| panic!("the fixture is TOML: {e}\n{text}"));
    OwnerTables::from_doc(&doc)
}

fn name(text: &str) -> OwnerName {
    OwnerName::parse(text).expect("a name")
}

fn resolved() -> Vec<(OwnerName, RemoteUrl)> {
    vec![(
        name("trust"),
        RemoteUrl::parse("git@github.com:alabsystems/trust.git").expect("a url"),
    )]
}

/// A one-rule table: the source table `source` (its lines after the header) and the rule's lines.
fn table(source: &str, rule: &str) -> String {
    format!("[harness.occupancy_source.trust]\n{source}\n[[harness.occupancy_rule]]\n{rule}\n")
}

const SOURCE: &str = "kind = \"git-ref\"\nurl = \"git@h:o/r.git\"";
const RULE: &str = "name = \"r\"\nwhen = \"git(trust).moved\"\ntext = \"Re-read the base.\"";

#[test]
fn the_designs_table_reads_clean() {
    let (tables, notes) = read(DESIGN);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(tables.sources().len(), 1);
    let rule = tables.rule(&name("release-base-moved")).expect("the rule");
    assert_eq!(rule.when.source, SourceRef::Git(name("trust")));
    assert_eq!(rule.when.sys, None);
    assert!(rule.when.session.is_empty());
    assert_eq!(rule.modes, [Autopilot::Cruise, Autopilot::Land]);
    assert!(rule.urgent);
    assert_eq!(rule.mode, RuleMode::Live);
    assert_eq!(
        rule.scope.as_ref().map(CwdGlob::canonical),
        Some("~/scratch.noindex/trust-release-*")
    );
    let source = tables.source(&name("trust")).expect("the source");
    let SourceKind::Git(git) = &source.kind else {
        panic!("a git source: {source:?}");
    };
    assert_eq!(
        git.origin,
        Origin::Repo {
            path: Checked("$HOME/trust".into()),
            remote: Checked("origin".into()),
        }
    );
    assert_eq!(git.git_ref.as_str(), "refs/heads/main");
    assert_eq!(git.path.as_ref().map(Checked::as_str), Some("src/stage0"));
    assert_eq!(git.every_ms, 5 * 60_000);
    let debug = format!("{tables:?}");
    assert!(
        !debug.contains("Re-read trust main"),
        "Debug never prints the text: {debug}"
    );
}

#[test]
fn defaults_fill_what_is_left_out() {
    let (tables, notes) = read(&table(SOURCE, RULE));
    assert!(notes.is_empty(), "{notes:?}");
    let rule = &tables.rules()[0];
    assert_eq!(rule.modes, [Autopilot::Cruise]);
    assert!(!rule.urgent);
    assert_eq!(rule.mode, RuleMode::Live);
    assert_eq!(rule.scope, None);
    let SourceKind::Git(git) = &tables.sources()[0].kind else {
        panic!("git");
    };
    assert_eq!(git.git_ref.as_str(), DEFAULT_REF);
    assert_eq!(git.every_ms, GIT_EVERY_MS);
    let (tables, notes) = read(
        "[harness.occupancy_source.pins]\nkind = \"atpkg\"\nprogram = \"trust\"\n\
         [[harness.occupancy_rule]]\nname = \"a\"\nwhen = \"atpkg(trust).moved and hold>=10m and slots>=1 for=20m\"\n\
         text = \"The index moved.\"\nmode = \"dry-run\"\n",
    );
    assert!(notes.is_empty(), "{notes:?}");
    let rule = &tables.rules()[0];
    assert_eq!(
        rule.when.source,
        SourceRef::Atpkg {
            program: Checked("trust".into()),
            source: name("pins"),
        }
    );
    assert_eq!(rule.when.session, [SessionTerm::HoldAtLeast(10 * 60_000)]);
    assert!(rule.when.sys.as_ref().is_some_and(SysWait::names_slots));
    assert_eq!(rule.mode, RuleMode::DryRun);
    assert_eq!(tables.sources()[0].every_ms(), ATPKG_EVERY_MS);
}

/// EVERY D5 REFUSAL REFUSES ITS RULE (or its source, and every rule naming it) with one closed
/// note naming the key and echoing nothing written. Each fixture differs from a table that reads
/// in that one place; `path` of 64 reads where 65 does not (F4's NEGATIVE CONTROL).
#[test]
fn each_refusal_refuses_its_rule_with_a_closed_note() {
    let path64 = format!("path = \"{}\"", "p".repeat(64));
    let path65 = format!("path = \"{}\"", "p".repeat(65));
    let (tables, notes) = read(&table(&format!("{SOURCE}\n{path64}"), RULE));
    assert!(
        notes.is_empty() && tables.rules().len() == 1,
        "64 is admitted: {notes:?}"
    );
    let long_text = format!("text = \"{}\"", "t".repeat(TEXT_MAX + 1));
    let max_text = format!("text = \"{}\"", "t".repeat(TEXT_MAX));
    let (tables, notes) = read(&table(
        SOURCE,
        &format!("name = \"r\"\nwhen = \"git(trust).moved\"\n{max_text}"),
    ));
    assert!(
        notes.is_empty() && tables.rules().len() == 1,
        "200 is admitted: {notes:?}"
    );
    let (tables, notes) = read(&table(
        SOURCE,
        "name = \"r\"\nwhen = \"git(trust).moved\"\ntext = \"Re-read the cafe\\u0301 menu \\u2728 \\u2014 \\u4e2d.\"",
    ));
    assert!(
        notes.is_empty() && tables.rules().len() == 1,
        "a mark on its letter, an emoji, a dash and CJK are admitted: {notes:?}"
    );
    let rule = |extra: &str, when: &str, text: &str| {
        format!("name = \"r\"\nwhen = \"{when}\"\ntext = \"{text}\"\n{extra}")
    };
    let when = |w: &str| rule("", w, "Re-read the base.");
    let source_with = |line: &str| format!("{SOURCE}\n{line}");
    for (source, rule, key) in [
        (
            SOURCE.to_string(),
            when(&format!("git(trust).moved and at:19:00 and {PLANT}")),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and in:5m"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and free>=10GiB"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and disk=ok"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and file:~/x"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and gone:~/x"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and pid:42"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and claim=release:trust"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and git(trust).moved"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and workflow.returned"),
            "when",
        ),
        (SOURCE.to_string(), when("hold>=10m and slots>=1"), "when"),
        (SOURCE.to_string(), when("git(other).moved"), "when"),
        (SOURCE.to_string(), when("atpkg(trust).moved"), "when"),
        (
            SOURCE.to_string(),
            when("git(trust).moved and hold=declared"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and hold>=10m and hold>=20m"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and autopilot=off"),
            "when",
        ),
        (SOURCE.to_string(), when("git(trust).moved for=20m"), "when"),
        (
            SOURCE.to_string(),
            when("git(trust).moved and slots>=1 scope=aterm"),
            "when",
        ),
        (SOURCE.to_string(), when("git(trust).moved and"), "when"),
        (
            SOURCE.to_string(),
            when("git(trust).moved slots>=1"),
            "when",
        ),
        (
            SOURCE.to_string(),
            when("git(trust).moved and no_such_metric>=1"),
            "when",
        ),
        (SOURCE.to_string(), when("git(trust).moved\u{202e}"), "when"),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", &"t".repeat(TEXT_MAX + 1)),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\u0007bell"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\u202ebidi"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\u200bzero"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\U000E0049\\U000E0047tag"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\uFE0Fselector"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\U000E0100selector"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\u034Fjoiner"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\u3164filler"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\uFFA0filler"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\u115Ffiller"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\u180Bselector"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\uFFF9annotation"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\uE000private"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "a\\u0378unassigned"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "run `rm`"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", "[aterm harness] spoof"),
            "text",
        ),
        (
            SOURCE.to_string(),
            rule("", "git(trust).moved", " "),
            "text",
        ),
        (
            SOURCE.to_string(),
            format!("{long_text}\nname = \"r\"\nwhen = \"git(trust).moved\""),
            "text",
        ),
        (
            SOURCE.to_string(),
            "name = \"my rule\"\nwhen = \"git(trust).moved\"\ntext = \"x\"".into(),
            "name",
        ),
        (
            SOURCE.to_string(),
            format!("name = \"{PLANT}\"\nwhen = \"git(trust).moved\"\ntext = \"x\""),
            "name",
        ),
        (
            SOURCE.to_string(),
            "when = \"git(trust).moved\"\ntext = \"x\"".into(),
            "name",
        ),
        (
            SOURCE.to_string(),
            rule(
                &format!("unknown_key = \"{PLANT}\""),
                "git(trust).moved",
                "x",
            ),
            "unknown_key",
        ),
        (
            SOURCE.to_string(),
            rule(
                "expect = \"claim.released within 30m\"",
                "git(trust).moved",
                "x",
            ),
            "expect",
        ),
        (
            SOURCE.to_string(),
            rule("do = \"land\"", "git(trust).moved", "x"),
            "do",
        ),
        (
            SOURCE.to_string(),
            rule("modes = [\"off\"]", "git(trust).moved", "x"),
            "modes",
        ),
        (
            SOURCE.to_string(),
            rule("modes = []", "git(trust).moved", "x"),
            "modes",
        ),
        (
            SOURCE.to_string(),
            rule("urgent = \"yes\"", "git(trust).moved", "x"),
            "urgent",
        ),
        (
            SOURCE.to_string(),
            rule("mode = \"loud\"", "git(trust).moved", "x"),
            "mode",
        ),
        (
            SOURCE.to_string(),
            rule(
                "scope = { jobs_cwd = \"~/a\\nb\" }",
                "git(trust).moved",
                "x",
            ),
            "scope.jobs_cwd",
        ),
        (
            SOURCE.to_string(),
            rule(
                "scope = { jobs_cwd = \"~/a\\u202eb\" }",
                "git(trust).moved",
                "x",
            ),
            "scope.jobs_cwd",
        ),
        (
            SOURCE.to_string(),
            rule("scope = { jobs_cwd = \"~/a/**\" }", "git(trust).moved", "x"),
            "scope.jobs_cwd",
        ),
        (
            SOURCE.to_string(),
            rule("scope = { jobs_cwd = \"a/b\" }", "git(trust).moved", "x"),
            "scope.jobs_cwd",
        ),
        (
            SOURCE.to_string(),
            rule("scope = { jobs_cwd = \"~/../x\" }", "git(trust).moved", "x"),
            "scope.jobs_cwd",
        ),
        (
            SOURCE.to_string(),
            rule(
                "scope = { claim = \"release:trust/*\" }",
                "git(trust).moved",
                "x",
            ),
            "scope.claim",
        ),
        (
            source_with("ref = \"refs/heads/../x\""),
            RULE.into(),
            "trust.ref",
        ),
        (source_with("ref = \"main\""), RULE.into(), "trust.ref"),
        (
            source_with(&format!("ref = \"refs/heads/{}\"", "r".repeat(49))),
            RULE.into(),
            "trust.ref",
        ),
        (
            source_with("path = \"/src/stage0\""),
            RULE.into(),
            "trust.path",
        ),
        (
            source_with("path = \"src/../x\""),
            RULE.into(),
            "trust.path",
        ),
        (source_with("path = \"-x\""), RULE.into(), "trust.path"),
        (source_with("path = \"a/-x\""), RULE.into(), "trust.path"),
        (
            source_with("ref = \"refs/heads/x.lock\""),
            RULE.into(),
            "trust.ref",
        ),
        (
            source_with("ref = \"refs/heads/.x\""),
            RULE.into(),
            "trust.ref",
        ),
        (
            source_with("ref = \"refs/heads/-x\""),
            RULE.into(),
            "trust.ref",
        ),
        (
            source_with("ref = \"refs/heads/a//b\""),
            RULE.into(),
            "trust.ref",
        ),
        (source_with(&path65), RULE.into(), "trust.path"),
        (source_with("every = \"10s\""), RULE.into(), "trust.every"),
        (source_with("every = \"2h\""), RULE.into(), "trust.every"),
        (source_with("every = 300"), RULE.into(), "trust.every"),
        (
            source_with(&format!("color = \"{PLANT}\"")),
            RULE.into(),
            "trust.color",
        ),
        (
            source_with("remote = \"origin\""),
            RULE.into(),
            "trust.remote",
        ),
        (source_with("repo = \"$HOME/trust\""), RULE.into(), "trust.url"),
        (
            "kind = \"svn\"\nurl = \"git@h:o/r.git\"".into(),
            RULE.into(),
            "trust.kind",
        ),
        ("url = \"git@h:o/r.git\"".into(), RULE.into(), "trust.kind"),
        (
            "kind = \"git-ref\"".into(),
            RULE.into(),
            "harness.occupancy_source.trust:",
        ),
        (
            "kind = \"git-ref\"\nurl = \"http://h/o/r\"".into(),
            RULE.into(),
            "trust.url",
        ),
        (
            "kind = \"git-ref\"\nurl = \"file:///Users//u/r.git\"".into(),
            RULE.into(),
            "trust.url",
        ),
        (
            "kind = \"git-ref\"\nurl = \"/Users//u/scratch.noindex/r\"".into(),
            RULE.into(),
            "trust.url",
        ),
        (
            "kind = \"git-ref\"\nrepo = \"~/.trust\"".into(),
            RULE.into(),
            "trust.repo",
        ),
        (
            "kind = \"git-ref\"\nrepo = \"trust\"".into(),
            RULE.into(),
            "trust.repo",
        ),
        (
            "kind = \"git-ref\"\nrepo = \"$HOME/trust\"\nremote = \"-x\"".into(),
            RULE.into(),
            "trust.remote",
        ),
        (
            "kind = \"atpkg\"\nprogram = \"Trust\"".into(),
            RULE.into(),
            "trust.program",
        ),
    ] {
        let text = table(&source, &rule);
        let (tables, notes) = read(&text);
        assert!(tables.rules().is_empty(), "{text}\n{tables:?}");
        let of_source = key.starts_with("trust.") || key.starts_with("harness.occupancy_source");
        assert_eq!(
            notes.len(),
            1 + usize::from(of_source),
            "a refused source refuses the rule naming it too: {text}\n{notes:?}"
        );
        assert!(notes[0].contains(key), "{key}: {notes:?}");
        if of_source {
            assert!(notes[0].ends_with("the source is ignored"), "{notes:?}");
            assert!(
                notes[1].contains(WhenRefusal::Undeclared.words()),
                "{notes:?}"
            );
        }
        for note in &notes {
            assert!(
                note.ends_with("the rule is ignored") || note.ends_with("the source is ignored"),
                "{note}"
            );
            assert!(
                !note.contains(PLANT),
                "a note echoes nothing written: {note}"
            );
            assert!(!note.chars().any(hidden), "{note:?}");
        }
    }
}

#[test]
fn a_bad_name_a_list_too_long_and_a_repeat_each_drop_their_own() {
    let (tables, notes) = read(&format!(
        "[harness.occupancy_source.\"Bad Name\"]\n{SOURCE}\n[harness.occupancy_source.trust]\n{SOURCE}\n"
    ));
    assert_eq!(tables.sources().len(), 1);
    assert_eq!(notes.len(), 1);
    assert!(!notes[0].contains("Bad Name"), "{notes:?}");
    let mut text = format!("[harness.occupancy_source.trust]\n{SOURCE}\n");
    for at in 0..=OWNER_RULES_MAX {
        text.push_str(&format!(
            "[[harness.occupancy_rule]]\nname = \"r{at}\"\nwhen = \"git(trust).moved\"\ntext = \"x\"\n"
        ));
    }
    text.push_str(
        "[[harness.occupancy_rule]]\nname = \"r0\"\nwhen = \"git(trust).moved\"\ntext = \"x\"\n",
    );
    let (tables, notes) = read(&text);
    assert_eq!(tables.rules().len(), OWNER_RULES_MAX);
    assert_eq!(notes.len(), 2, "{notes:?}");
    let (tables, notes) = read(&format!(
        "[harness.occupancy_source.trust]\n{SOURCE}\n[[harness.occupancy_rule]]\n{RULE}\n[[harness.occupancy_rule]]\n{RULE}\n"
    ));
    assert_eq!(tables.rules().len(), 1);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("names a rule already read"), "{notes:?}");
    let (_, notes) = read("[harness]\noccupancy_rule = \"x\"\noccupancy_source = 3\n");
    assert_eq!(notes.len(), 2, "{notes:?}");
}

/// EACH CAP READS ITS LIMIT AND REFUSES ONE PAST IT: 8 sources (a 9th is ignored with its note),
/// 8 terms in a `when`, `hold>=48h`, and a span written without a leading zero (`every = "5m"`,
/// not `"05m"`). NEGATIVE CONTROL: each limit itself reads clean.
#[test]
fn each_cap_reads_its_limit_and_refuses_one_past_it() {
    let sources = |n: usize| {
        (0..n)
            .map(|at| format!("[harness.occupancy_source.s{at}]\n{SOURCE}\n"))
            .collect::<String>()
    };
    let (tables, notes) = read(&sources(OWNER_SOURCES_MAX));
    assert_eq!(tables.sources().len(), OWNER_SOURCES_MAX);
    assert!(notes.is_empty(), "{notes:?}");
    let (tables, notes) = read(&sources(OWNER_SOURCES_MAX + 1));
    assert_eq!(tables.sources().len(), OWNER_SOURCES_MAX);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].ends_with("the source is ignored"), "{notes:?}");

    let eight = "git(trust).moved and hold=background and hold>=10m and autopilot=cruise and \
                 slots>=1 and cpu<25% and mem<critical and thermal<serious";
    let when = |w: &str| {
        read(&table(
            SOURCE,
            &format!("name = \"r\"\nwhen = \"{w}\"\ntext = \"x\""),
        ))
    };
    for (w, refused) in [
        (eight.to_string(), None),
        (format!("{eight} and load<0.5"), Some(WhenRefusal::TooMany)),
        ("git(trust).moved and hold>=48h".to_string(), None),
        (
            "git(trust).moved and hold>=49h".to_string(),
            Some(WhenRefusal::Session),
        ),
        (
            "git(trust).moved and hold>=010m".to_string(),
            Some(WhenRefusal::Session),
        ),
    ] {
        let (tables, notes) = when(&w);
        match refused {
            None => assert!(
                notes.is_empty() && tables.rules().len() == 1,
                "{w}: {notes:?}"
            ),
            Some(why) => {
                assert!(tables.rules().is_empty(), "{w}");
                assert!(
                    notes.len() == 1 && notes[0].contains(why.words()),
                    "{w}: {notes:?}"
                );
            }
        }
    }
    let (tables, notes) = read(&table(&format!("{SOURCE}\nevery = \"5m\""), RULE));
    assert!(notes.is_empty() && tables.sources().len() == 1, "{notes:?}");
    let (tables, notes) = read(&table(&format!("{SOURCE}\nevery = \"05m\""), RULE));
    assert!(tables.sources().is_empty(), "{notes:?}");
    assert!(notes[0].contains("trust.every"), "{notes:?}");
}

/// A YES BEFORE A `repo =` SOURCE'S URL IS READ IS NO YES TO THE RULE OVER THAT URL: its rev has
/// no `resolved_url` line, so the resolved rule is another rev, unconfirmed by it, and its
/// snapshot is stale once the URL is read. NEGATIVE CONTROL `RevSkipsUrl`: a form without the
/// URL keeps the one rev across the read.
#[test]
fn a_yes_before_the_url_is_read_is_no_yes_after() {
    let (tables, _) = read(DESIGN);
    let rule = tables.rule(&name("release-base-moved")).expect("the rule");
    assert!(!canonical(rule, tables.sources(), &[]).contains("resolved_url"));
    let unread = Snapshot::of(&tables, &rule.name, &[]).expect("reads");
    assert!(unread.still_current(&tables, &[]));
    assert!(!unread.still_current(&tables, &resolved()));
    assert_eq!(
        RuleState::of(
            &tables.rev(rule, &resolved()),
            &[unread.rev],
            RuleMode::Live
        ),
        RuleState::Unconfirmed
    );
    assert_eq!(
        fault::with(Fault::RevSkipsUrl, || tables.rev(rule, &[])),
        fault::with(Fault::RevSkipsUrl, || tables.rev(rule, &resolved()))
    );
}

/// ONLY THE ROOT TABLE IS READ: a copy below another table's header is no owner table here
/// (the config reader's misplaced note says so).
#[test]
fn a_misplaced_copy_is_not_read() {
    let misplaced = format!("[theme]\n{}", DESIGN.replace("[harness", "[theme.harness"));
    let (tables, notes) = read(&misplaced);
    assert!(
        tables.rules().is_empty() && tables.sources().is_empty(),
        "{tables:?}"
    );
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        OwnerTables::from_aterm_toml("font_px = \n[harness]\n"),
        (OwnerTables::default(), Vec::new())
    );
}

/// The design table's rule, rev and canonical form, its repo's remote resolved as given.
fn design_rev(text: &str, resolved: &Resolved) -> Rev {
    let (tables, notes) = read(text);
    assert!(notes.is_empty(), "{notes:?}\n{text}");
    let rule = tables.rules().first().expect("the rule");
    tables.rev(rule, resolved)
}

/// A REV COVERS EVERY FIELD: each of the rule's and its source's fields edited one at a time
/// gives a new rev, and an optional field left out is no value it could be written as (no
/// `path` is not `path = "none"`); key order, spacing, a default written out and `when`'s
/// spelling do not. NEGATIVE CONTROL `RevSkipsField`: a canonical form that leaves the text out
/// keeps the rev over an edit of the text.
#[test]
fn a_rev_covers_every_field_and_nothing_else() {
    let base = design_rev(DESIGN, &resolved());
    assert_eq!(base.hex().len(), 64);
    assert_eq!(base.short(), base.hex()[..12]);
    assert_eq!(Rev::parse(&base.hex()), Some(base));
    let edits = [
        (
            "name   = \"release-base-moved\"",
            "name   = \"release-base-movee\"",
        ),
        (
            "when   = \"git(trust).moved\"",
            "when   = \"git(trust).moved and hold>=10m\"",
        ),
        ("trust-release-*", "trust-release-1*"),
        ("modes  = [\"cruise\", \"land\"]", "modes  = [\"cruise\"]"),
        ("urgent = true", "urgent = false"),
        ("urgent = true", "urgent = true\nmode = \"dry-run\""),
        ("before any cut", "before any  cut"),
        ("repo   = \"$HOME/trust\"", "repo   = \"$HOME/trust2\""),
        ("remote = \"origin\"", "remote = \"upstream\""),
        (
            "ref    = \"refs/heads/main\"",
            "ref    = \"refs/heads/release\"",
        ),
        ("path   = \"src/stage0\"", "path   = \"src/stage1\""),
        ("path   = \"src/stage0\"", ""),
        ("every  = \"5m\"", "every  = \"6m\""),
    ];
    for (from, to) in edits {
        assert!(DESIGN.contains(from), "{from}");
        let edited = DESIGN.replace(from, to);
        assert_ne!(design_rev(&edited, &resolved()), base, "{from} -> {to}");
    }
    let pathless = DESIGN.replace("path   = \"src/stage0\"\n", "");
    let none = DESIGN.replace("path   = \"src/stage0\"", "path   = \"none\"");
    assert_ne!(
        design_rev(&pathless, &resolved()),
        design_rev(&none, &resolved()),
        "no path is not the path none"
    );
    let edited = DESIGN.replace("before any cut", "before any  cut");
    let rename = DESIGN.replace("release-base-moved", "release-base-movee");
    assert_eq!(
        fault::with(Fault::RevSkipsField, || design_rev(&edited, &resolved())),
        fault::with(Fault::RevSkipsField, || design_rev(DESIGN, &resolved())),
        "a form without the text keeps the rev over its edit"
    );
    assert_ne!(
        fault::with(Fault::RevSkipsField, || design_rev(&rename, &resolved())),
        fault::with(Fault::RevSkipsField, || design_rev(DESIGN, &resolved()))
    );
    let reordered = r#"
[[harness.occupancy_rule]]
text = "Re-read trust main and the atpkg index before any cut, mint or publish goes on."
urgent = true
modes = ["land", "cruise", "land"]
scope = { jobs_cwd = "~/scratch.noindex/trust-release-*/" }
when = "git(trust).moved"
name = "release-base-moved"
mode = "live"

[harness.occupancy_source.trust]
every = "300s"
path = "src/stage0"
ref = "refs/heads/main"
repo = "$HOME/trust"
kind = "git-ref"
"#;
    assert_eq!(design_rev(reordered, &resolved()), base);
    let spaced = DESIGN.replace(
        "when   = \"git(trust).moved\"",
        "when = \"  git(trust).moved   and slots>=1  and hold>=10m \"",
    );
    let ordered = DESIGN.replace(
        "when   = \"git(trust).moved\"",
        "when = \"git(trust).moved and hold>=600s and slots>=1\"",
    );
    assert_eq!(
        design_rev(&spaced, &resolved()),
        design_rev(&ordered, &resolved())
    );
}

/// A REV COVERS THE RESOLVED URL: the same table over a rewritten remote is a new rev, so a
/// yes given over one remote is no yes over another. NEGATIVE CONTROL `RevSkipsUrl`.
#[test]
fn a_rev_covers_the_resolved_url() {
    let other = vec![(
        name("trust"),
        RemoteUrl::parse("git@evil.example:o/trust.git").expect("a url"),
    )];
    assert_ne!(design_rev(DESIGN, &resolved()), design_rev(DESIGN, &other));
    assert_ne!(design_rev(DESIGN, &resolved()), design_rev(DESIGN, &[]));
    assert_eq!(
        fault::with(Fault::RevSkipsUrl, || design_rev(DESIGN, &resolved())),
        fault::with(Fault::RevSkipsUrl, || design_rev(DESIGN, &other)),
    );
    let (tables, _) = read(&table(SOURCE, RULE));
    let rule = &tables.rules()[0];
    assert!(canonical(rule, tables.sources(), &[]).contains("resolved_url=\"git@h:o/r.git\""));
}

/// A RULE IS READ ONLY UNDER A YES TO ITS WHOLE CURRENT REV. NEGATIVE CONTROL `ConfirmByName`: a
/// state that counts any yes on record reads the edited table under the old one's.
#[test]
fn a_rules_state_is_its_whole_revs() {
    let live = design_rev(DESIGN, &resolved());
    let dry_text = DESIGN.replace("urgent = true", "urgent = true\nmode = \"dry-run\"");
    let dry = design_rev(&dry_text, &resolved());
    assert_eq!(
        RuleState::of(&live, &[live], RuleMode::Live),
        RuleState::Live
    );
    assert_eq!(
        RuleState::of(&dry, &[dry], RuleMode::DryRun),
        RuleState::DryRun
    );
    assert_eq!(
        RuleState::of(&live, &[], RuleMode::Live),
        RuleState::Unconfirmed
    );
    let edited = design_rev(
        &DESIGN.replace("urgent = true", "urgent = false"),
        &resolved(),
    );
    assert_eq!(
        RuleState::of(&edited, &[live], RuleMode::Live),
        RuleState::Unconfirmed
    );
    assert_eq!(
        RuleState::of(&dry, &[live], RuleMode::DryRun),
        RuleState::Unconfirmed
    );
    assert_eq!(
        fault::with(Fault::ConfirmByName, || RuleState::of(
            &edited,
            &[live],
            RuleMode::Live
        )),
        RuleState::Live
    );
    assert_eq!(
        [RuleState::Live, RuleState::DryRun, RuleState::Unconfirmed].map(RuleState::word),
        ["live", "dry-run", "unconfirmed"]
    );
}

/// A YES IS TO A SNAPSHOT: once the table in force no longer hashes to it, it confirms nothing.
/// NEGATIVE CONTROL `ConfirmStaleSnapshot`: a check by name alone calls the edited table current.
#[test]
fn a_snapshot_is_stale_once_its_table_is_edited() {
    let (tables, _) = read(DESIGN);
    let snap = Snapshot::of(&tables, &name("release-base-moved"), &resolved()).expect("reads");
    assert!(snap.still_current(&tables, &resolved()));
    assert_eq!(snap.rev, design_rev(DESIGN, &resolved()));
    let (edited, _) = read(&DESIGN.replace("urgent = true", "urgent = false"));
    assert!(!snap.still_current(&edited, &resolved()));
    assert!(
        !snap.still_current(&tables, &[]),
        "an unresolved remote is another rev"
    );
    let (gone, _) = read(&DESIGN.replace("release-base-moved", "other-name"));
    assert!(!snap.still_current(&gone, &resolved()));
    assert!(fault::with(Fault::ConfirmStaleSnapshot, || {
        snap.still_current(&edited, &resolved())
    }));
}

#[test]
fn a_cwd_glob_matches_its_folder_and_below_within_components() {
    let glob = CwdGlob::parse("~/scratch.noindex/trust-release-*").expect("a glob");
    let home = Path::new("/Users//x");
    for (cwd, want) in [
        ("/Users//x/scratch.noindex/trust-release-0.11/sub", true),
        ("/Users//x/scratch.noindex/trust-release-0.11", true),
        ("/Users//x/scratch.noindex/trust-release-", true),
        ("/Users//x/scratch.noindex/trust-release", false),
        ("/Users//x/scratch.noindex/other/trust-release-1", false),
        ("/Users//y/scratch.noindex/trust-release-1", false),
        ("/Users//x/scratch.noindex", false),
        ("scratch.noindex/trust-release-1", false),
    ] {
        assert_eq!(glob.matches(Path::new(cwd), home), want, "{cwd}");
    }
    let abs = CwdGlob::parse("/Volumes/w/*-build/x*y").expect("a glob");
    assert!(abs.matches(Path::new("/Volumes/w/a-build/xzzy/deep"), home));
    assert!(abs.matches(Path::new("/Volumes/w/-build/xy"), home));
    assert!(!abs.matches(Path::new("/Volumes/w/a-build/xzz"), home));
    assert!(!abs.matches(Path::new("/Volumes/w/a-built/xy"), home));
    for (text, want) in [
        ("~/a\nb", GlobRefusal::Control),
        ("~/a\u{202e}b", GlobRefusal::Control),
        ("~/a\u{200b}b", GlobRefusal::Control),
        ("a/b", GlobRefusal::Relative),
        ("~/a/**", GlobRefusal::DoubleStar),
        ("~/a//b", GlobRefusal::Dot),
        ("~/.config/x", GlobRefusal::Dot),
        ("~/a/../b", GlobRefusal::Dot),
        ("~/caf\u{e9}", GlobRefusal::Unicode),
        ("~/a b", GlobRefusal::Unicode),
    ] {
        assert_eq!(CwdGlob::parse(text), Err(want), "{text:?}");
    }
    assert_eq!(
        CwdGlob::parse(&format!("~/{}", "a".repeat(GLOB_MAX))),
        Err(GlobRefusal::Long)
    );
}

/// WHAT A PERSON SEES IS PRINTED FROM THE PARSED FORM: `when` canonical, never as written; the
/// source's kind, host and path, auth mode and every; the reach and the urgency. Each part is
/// asserted as the parsed form gives it, never a sentence around it. A name with a bidi
/// character never reaches a rendering: it is refused at read.
#[test]
fn the_rendering_is_printed_from_the_parsed_form() {
    let written = "git(trust).moved   and  hold>=600s";
    let text = DESIGN.replace(
        "when   = \"git(trust).moved\"",
        &format!("when = \"{written}\""),
    );
    let (tables, _) = read(&text);
    let rule = tables.rule(&name("release-base-moved")).expect("the rule");
    let snap = Snapshot::of(&tables, &rule.name, &resolved()).expect("reads");
    let shown = &snap.rendering;
    assert!(!shown.contains(written), "{shown}");
    assert_eq!(rule.when.canonical(), "git(trust).moved and hold>=10m");
    let url = &resolved()[0].1;
    let SourceKind::Git(git) = &tables.sources()[0].kind else {
        panic!("git");
    };
    for part in [
        rule.name.as_str(),
        &snap.rev.short(),
        rule.mode.word(),
        &rule.when.canonical(),
        tables.sources()[0].kind_word(),
        git.git_ref.as_str(),
        &format!("{}/{}", url.host(), url.path()),
        git.path.as_ref().map(Checked::as_str).expect("a path"),
        url.auth().words(),
        "5m",
        "Cruise",
        "Land",
        rule.scope
            .as_ref()
            .map(CwdGlob::canonical)
            .expect("a scope"),
        rule.text.as_str(),
    ] {
        assert!(shown.contains(part), "{part}\n{shown}");
    }
    let calm = DESIGN.replace("urgent = true", "urgent = false");
    let (calm, _) = read(&calm);
    let calm = Snapshot::of(&calm, &rule.name, &resolved()).expect("reads");
    assert_eq!(
        calm.rendering.lines().count() + 1,
        shown.lines().count(),
        "urgency is its own line"
    );

    let unread = Snapshot::of(&tables, &rule.name, &[]).expect("reads");
    assert!(
        !unread.rendering.contains(url.host()) && !unread.rendering.contains(url.auth().words()),
        "an unread URL is not rendered: {}",
        unread.rendering
    );
    assert!(
        unread.rendering.contains("$HOME/trust") && unread.rendering.contains("origin"),
        "{}",
        unread.rendering
    );
    let (unscoped, _) = read(&table(SOURCE, RULE));
    let snap = Snapshot::of(&unscoped, &name("r"), &[]).expect("reads");
    assert!(
        snap.rendering.contains("Cruise") && !snap.rendering.contains("Land"),
        "{}",
        snap.rendering
    );
    let rule = unscoped.rule(&name("r")).expect("the rule");
    let (workflow, _) = read(&table(
        SOURCE,
        "name = \"w\"\nwhen = \"workflow.returned\"\ntext = \"x\"",
    ));
    let workflow = rendering(&workflow.rules()[0], workflow.sources(), &[]);
    let source_line = |shown: &str| shown.lines().nth(2).map(str::to_string);
    let undeclared = rendering(rule, &[], &[]);
    assert!(
        source_line(&undeclared).is_some_and(|l| l.contains("trust")),
        "{undeclared}"
    );
    assert_ne!(
        source_line(&undeclared),
        source_line(&workflow),
        "a git rule given no sources is never rendered as a workflow rule"
    );
    let bidi = DESIGN.replace(
        "name   = \"release-base-moved\"",
        "name = \"release\u{202e}moved\"",
    );
    let (tables, notes) = read(&bidi);
    assert!(tables.rules().is_empty() && notes.len() == 1, "{notes:?}");
    assert!(!notes[0].chars().any(hidden));
}
