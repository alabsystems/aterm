// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn background_is_only_a_repeatable_tree_sample() {
    let parse = |args: &[&str]| super::parse(args.iter().copied());
    assert!(
        parse(&["release", "--tree", "--background"])
            .unwrap()
            .background
    );
    for args in [
        vec!["test", "--background"],
        vec!["release", "--background"],
        vec!["release", "--bundle", "--app", "app", "--background"],
    ] {
        assert!(parse(&args).is_err());
    }
}
use super::*;

fn ok(args: &[&str]) -> Args {
    parse(args.iter().copied()).unwrap_or_else(|e| panic!("{args:?}: {}", e.message()))
}

/// The bare command is the dev set on the cone; checks and sets combine
/// in one order without repeats.
#[test]
fn checks_and_sets_combine_in_one_order_and_the_default_is_dev() {
    let a = ok(&[]);
    assert_eq!(a.checks, DEV);
    assert!(!a.all && a.packages.is_empty() && !a.release && a.narrows());
    assert_eq!(ok(&["dev"]).checks, DEV);
    assert_eq!(
        ok(&["test", "lint", "test", "fmt"]).checks,
        [Check::Fmt, Check::Lint, Check::Test]
    );
    let a = ok(&["dev", "forge"]);
    assert_eq!(a.checks.len(), DEV.len() + 1);
    assert!(a.checks.contains(&Check::Forge));
    assert!(!ok(&["fmt", "census"]).narrows());
}

/// Only a run whose lint, test, doc or checkers builds its `-p` with this host's cargo asks
/// whether the host's graph holds each spec. CONTROL: `export-scan` and `cells` read `-p`
/// too ([`Args::narrows`]), and are not asked.
#[test]
fn only_a_check_that_builds_its_selection_here_hands_cargo_its_specs() {
    let p = "windows-sys@0.61.2";
    for (args, here) in [
        (vec!["-p", p], true),
        (vec!["lint", "-p", p], true),
        (vec!["test", "-p", p], true),
        (vec!["doc", "-p", p], true),
        (vec!["checkers", "-p", p], true),
        (vec!["export-scan", "-p", p], false),
        (vec!["cells", "-p", p], false),
        (vec!["fmt", "census", "-p", p], false),
        (vec!["lint"], false),
        (vec!["lint", "--all"], false),
    ] {
        let a = ok(&args);
        assert_eq!(a.builds_packages_here(), here, "{args:?}");
    }
    assert!(ok(&["export-scan", "-p", p]).narrows());
    assert!(ok(&["cells", "-p", p]).narrows());
}

#[test]
fn release_implies_the_whole_workspace_and_needs_its_bundle() {
    let a = ok(&["release", "--app", "/x/aterm.app"]);
    assert!(a.release && a.all);
    assert_eq!(
        a.checks,
        CHECKS
            .iter()
            .map(|s| s.check)
            .filter(|c| RELEASE.contains(c))
            .collect::<Vec<_>>()
    );
    assert!(parse(["release"]).is_err(), "release needs its bundle");
    assert!(parse(["release", "--app", "a", "-p", "x"]).is_err());
    assert!(parse(["release", "cells", "--app", "a", "--cell", "win"]).is_err());
    assert!(
        parse(["release", "--checks=test", "--base", "previous-hop"]).is_err(),
        "a release never narrows its tree to the fix hop's cone"
    );
}

/// Binary proofs go only with a release's test check and its frozen context. Release-only
/// lint proofs remain separate; another check cannot take binaries. A development run takes
/// neither, only the input-drift bracket.
#[test]
fn a_release_resume_carries_the_binaries_that_passed_only_into_its_test_run() {
    assert_eq!(
        ok(&["release", "--tree", "--events", "/l/06-verify.events"])
            .events
            .as_deref(),
        Some(std::path::Path::new("/l/06-verify.events"))
    );
    let a = ok(&[
        "release",
        "--checks=test",
        "--passed-binaries",
        "/l/06-verify.passed-binaries",
        "--carry-context",
        &"ab".repeat(32),
    ]);
    assert_eq!(a.checks, [Check::Test]);
    assert_eq!(
        a.passed_binaries.as_deref(),
        Some(std::path::Path::new("/l/06-verify.passed-binaries"))
    );
    assert!(
        ok(&[
            "release",
            "--passed-binaries",
            "/p",
            "--carry-context",
            &"ab".repeat(32),
            "--app",
            "/a",
        ])
        .checks
        .contains(&Check::Test)
    );
    // A development run carries nothing: it takes neither proofs nor their context, and
    // brackets its inputs only when asked, as `verify land`'s judgment asks.
    let context = "ab".repeat(32);
    for selection in [vec!["test", "--all"], vec!["--base", "previous-main"]] {
        for extra in [
            &[
                "--passed-binaries",
                "/p",
                "--carry-context",
                context.as_str(),
            ][..],
            &["--carry-context", context.as_str()],
            &["--passed-binaries", "/p"],
        ] {
            let mut args = selection.clone();
            args.extend(extra);
            assert!(parse(args.iter().copied()).is_err(), "{args:?}");
        }
        let mut args = selection.clone();
        args.push(super::INPUT_DRIFT);
        let parsed = ok(&args);
        assert!(parsed.input_drift && !parsed.release);
        assert!(!ok(&selection).input_drift);
    }
    for args in [
        &["fmt", super::INPUT_DRIFT][..],
        &["release", "--checks=test", super::INPUT_DRIFT],
    ] {
        assert!(parse(args.iter().copied()).is_err(), "{args:?}");
    }
    for args in [
        &[
            "release",
            "--checks",
            "fmt",
            "--passed-binaries",
            "/p",
            "--carry-context",
            "abababababababababababababababababababababababababababababababab",
        ][..],
        &[
            "release",
            "--bundle",
            "--passed-binaries",
            "/p",
            "--carry-context",
            "abababababababababababababababababababababababababababababababab",
            "--app",
            "/a",
        ],
        &[
            "fmt",
            "--passed-binaries",
            "/p",
            "--carry-context",
            "abababababababababababababababababababababababababababababababab",
        ],
    ] {
        assert!(parse(args.iter().copied()).is_err(), "{args:?}");
    }
    assert!(
        parse(["release", "--checks=test", "--passed-binaries", "/p",]).is_err(),
        "proofs without their frozen context are not reusable"
    );
    assert!(
        parse(["release", "--checks=test", "--carry-context", "AA"]).is_err(),
        "the salt is one canonical SHA-256"
    );
    assert_eq!(
        ok(&["--base", "abc", "--events", "/e"]).events.as_deref(),
        Some(std::path::Path::new("/e")),
        "a dev run keeps its events (verify land's judgment)"
    );
}

/// Lint package proofs have their own release-only wire and share the frozen item context.
/// Negative controls: a dev lint, another release check, and proofs without context are
/// refused before a file is read.
#[test]
fn a_release_resume_accepts_lint_package_proofs_only_for_lint() {
    let context = "cd".repeat(32);
    let parsed = ok(&[
        "release",
        "--checks=lint",
        "--passed-lint-packages",
        "/l/lint.jsonl",
        "--carry-context",
        &context,
    ]);
    assert_eq!(parsed.checks, [Check::Lint]);
    assert_eq!(
        parsed.passed_lint_packages.as_deref(),
        Some(std::path::Path::new("/l/lint.jsonl"))
    );
    assert!(
        parse([
            "lint",
            "--all",
            "--passed-lint-packages",
            "/p",
            "--carry-context",
            &context,
        ])
        .is_err()
    );
    assert!(
        parse([
            "release",
            "--checks=fmt",
            "--passed-lint-packages",
            "/p",
            "--carry-context",
            &context,
        ])
        .is_err()
    );
    assert!(parse(["release", "--checks=lint", "--passed-lint-packages", "/p",]).is_err());
}

/// AppKit proofs are release-only, group-local executable evidence. The first run may name
/// only its context so it can produce proofs; a resume names both.
#[test]
fn appkit_group_proofs_belong_only_to_a_release_appkit_run() {
    let context = "ef".repeat(32);
    let first = ok(&[
        "release",
        "--bundle",
        "--checks=appkit",
        "--carry-context",
        &context,
    ]);
    assert_eq!(first.checks, [Check::Appkit]);
    assert!(first.passed_appkit_groups.is_none());

    let resumed = ok(&[
        "release",
        "--bundle",
        "--checks=appkit",
        "--passed-appkit-groups",
        "/l/appkit.jsonl",
        "--carry-context",
        &context,
    ]);
    assert_eq!(resumed.checks, [Check::Appkit]);
    assert_eq!(
        resumed.passed_appkit_groups.as_deref(),
        Some(std::path::Path::new("/l/appkit.jsonl"))
    );

    for args in [
        vec![
            "appkit",
            "--passed-appkit-groups",
            "/p",
            "--carry-context",
            &context,
        ],
        vec![
            "release",
            "--checks=lint",
            "--passed-appkit-groups",
            "/p",
            "--carry-context",
            &context,
        ],
        vec!["release", "--checks=appkit", "--passed-appkit-groups", "/p"],
    ] {
        assert!(parse(args).is_err());
    }
}

/// `release --tree` and `release --bundle --app A` run its halves, both its whole set, and
/// `--checks` some of its checks; each is a release (whole workspace, its halt), `--app` goes
/// exactly with `measure`. Controls: each flag without `release`, a listed check outside the
/// release or its half, an unknown one, and `--app` beside no `measure`, are usage errors.
#[test]
fn release_narrows_to_its_halves_and_to_the_checks_listed() {
    let in_order = |set: &[Check]| -> Vec<Check> {
        CHECKS
            .iter()
            .map(|s| s.check)
            .filter(|c| set.contains(c))
            .collect()
    };
    let tree = ok(&["release", "--tree"]);
    assert!(tree.release && tree.all && tree.app.is_none());
    assert_eq!(tree.checks, in_order(&RELEASE_TREE));
    let bundle = ok(&["release", "--bundle", "--app", "a"]);
    assert!(bundle.release && bundle.all);
    assert_eq!(bundle.checks, in_order(&RELEASE_BUNDLE));
    assert_eq!(
        ok(&["release", "--tree", "--bundle", "--app", "a"]).checks,
        ok(&["release", "--app", "a"]).checks
    );
    let resume = ok(&["release", "--checks", "spec,test", "--checks=fmt"]);
    assert!(resume.release && resume.all);
    assert_eq!(resume.checks, [Check::Fmt, Check::Test, Check::Spec]);
    assert_eq!(
        ok(&["release", "--bundle", "--checks", "appkit"]).checks,
        [Check::Appkit],
        "appkit reads no bundle"
    );
    assert_eq!(
        ok(&["release", "--checks", "measure", "--app", "a"]).checks,
        [Check::Measure]
    );
    // Help and list are Ok before any refusal, as they were.
    assert!(ok(&["release", "--tree", "--help"]).help);
    assert_eq!(ok(&["release", "--tree", "--help"]).checks, tree.checks);
    assert!(ok(&["list", "--tree"]).list);
    for args in [
        &["--tree"][..],
        &["lint", "--bundle"],
        &["test", "--checks", "test"],
        &["release", "--checks", "cells"],
        &["release", "--tree", "--checks", "appkit"],
        &["release", "--checks", "tset"],
        &["release", "--checks", "test,"],
        &["release", "--tree", "--app", "a"],
        &["release", "--checks", "lint", "--app", "a"],
        &["release", "--bundle"],
        &["release", "--checks", "fmt", "--fix"],
    ] {
        assert!(parse(args.iter().copied()).is_err(), "{args:?}");
    }
}

#[test]
fn contradictions_and_unknowns_are_usage_errors() {
    for args in [
        &["-p", "x", "--all"][..],
        &["--base", "main", "--all"],
        &["lnit"],
        &["--full"],
        &["-p"],
        &["-p="],
        &["-p=--all"],
        &["-p", "--all"],
        &["--test-threads", "0"],
        &["--test-cores", "0"],
        &["--test-seed", "-1"],
        &["--test-seed", "not-a-seed"],
        &["--test-jobs", "2"],
        &["--timeout", "45m"],
        &["--admit", "later"],
        &["list", "fmt"],
        // --fix rewrites sources, so it runs alone, and only with fmt or census.
        &["lint", "--fix"],
        &["--fix"],
        &["fmt", "lint", "--fix"],
        &["fmt", "census", "--fix"],
        &["release", "--checks", "census", "--fix"],
        &["release", "--app", "a", "--fix"],
        // --cell narrows `cells` to cells forge knows.
        &["lint", "--cell", "win"],
        &["cells", "--cell", "plan9"],
    ] {
        assert!(parse(args.iter().copied()).is_err(), "{args:?}");
    }
    assert!(ok(&["fmt", "--fix"]).fix);
    assert!(ok(&["census", "--fix"]).fix);
    assert_eq!(
        ok(&["cells", "--cell", "win", "--cell=linux"]).cells,
        ["win", "linux"]
    );
}

#[test]
fn values_parse_into_their_types() {
    let a = ok(&[
        "lint",
        "-p",
        "a",
        "--package",
        "b",
        "--scope=c",
        "-pd",
        "-p=e",
    ]);
    assert_eq!(a.packages, ["a", "b", "c", "d", "e"]);
    let a = ok(&[
        "test",
        "--test-threads=3",
        "--test-cores=10",
        "--test-seed=17",
        "--timeout",
        "off",
        "--log",
        "/tmp/x.log",
        "--no-windows",
        "--base",
        "main",
    ]);
    assert_eq!(a.test_threads.map(NonZeroU32::get), Some(3));
    assert_eq!(a.test_cores.map(NonZeroU32::get), Some(10));
    assert_eq!(a.test_seed, Some(17));
    assert_eq!(a.timeout, Some(None));
    assert_eq!(a.log.as_deref(), Some(std::path::Path::new("/tmp/x.log")));
    assert!(a.no_windows);
    assert!(!a.fail_fast, "test failures keep their whole sweep");
    assert!(ok(&["test", "--fail-fast"]).fail_fast);
    assert_eq!(a.base.as_deref(), Some("main"));
    assert_eq!(a.admit, aterm_capacity::Admit::Wait, "wait by default");
    assert_eq!(
        ok(&["measure", "--admit", "now"]).admit,
        aterm_capacity::Admit::Now
    );
    assert_eq!(
        ok(&["measure", "--admit=wait"]).admit,
        aterm_capacity::Admit::Wait
    );
    assert!(parse(["measure", "--admit", "soon"]).is_err());
    assert_eq!(
        ok(&["--timeout", "90"]).timeout,
        Some(Some(Duration::from_secs(90)))
    );
    assert_eq!(ok(&["test"]).admit, atpkg::admit_disk::Admit::Wait);
    assert_eq!(
        ok(&["test", "--admit", "now"]).admit,
        atpkg::admit_disk::Admit::Now
    );
}

/// A round of `verify land` names the judged commit and the file of its shared paths beside
/// `--base`: the dev set, whose crates the run reads from those paths. Each flag needs the
/// other and `--base`, and `-p`, `--all` and `release` replace the cone they select, so each
/// such command is usage. CONTROL: the same words without the two flags parse as a judgment.
#[test]
fn a_round_names_its_judged_commit_and_shared_paths_beside_the_base() {
    let round = ok(&["--base", "tip", "--judged", "judged", "--shared", "/s"]);
    assert_eq!(round.checks, DEV);
    assert_eq!(
        (
            round.base.as_deref(),
            round.judged.as_deref(),
            round.shared.as_deref()
        ),
        (
            Some("tip"),
            Some("judged"),
            Some(std::path::Path::new("/s"))
        )
    );
    assert!(round.narrows() && !round.all);
    let judgment = ok(&["--base", "tip"]);
    assert_eq!((judgment.judged, judgment.shared), (None, None));
    for refused in [
        &["--base", "tip", "--judged", "judged"][..],
        &["--base", "tip", "--shared", "/s"],
        &["--judged", "judged", "--shared", "/s"],
        &["--judged", "judged", "--shared", "/s", "--all"],
        &["--judged", "judged", "--shared", "/s", "-p", "aterm-grid"],
        &[
            "--base", "tip", "--judged", "judged", "--shared", "/s", "--all",
        ],
        &[
            "--base", "tip", "--judged", "judged", "--shared", "/s", "-p", "x",
        ],
        &["release", "--tree", "--judged", "judged", "--shared", "/s"],
        &["--base", "tip", "--judged", "--shared", "/s"],
        &["--base", "tip", "--judged", "judged", "--shared"],
    ] {
        assert!(parse(refused.iter().copied()).is_err(), "{refused:?}");
    }
    let alone = parse(["--base", "tip", "--judged", "judged"]).unwrap_err();
    assert!(
        alone.message().contains("go together"),
        "{}",
        alone.message()
    );
    let baseless = parse(["--judged", "judged", "--shared", "/s"]).unwrap_err();
    assert!(
        baseless
            .message()
            .contains("select a round's cone since --base"),
        "{}",
        baseless.message()
    );
}

/// A round of a merge a person made says so (`--by-hand`), and the word means nothing without
/// the round it describes. CONTROL: a round without it is of the merge Git made.
#[test]
fn a_round_of_a_hand_merge_says_so_and_the_word_needs_its_round() {
    let round = ["--base", "tip", "--judged", "judged", "--shared", "/s"];
    assert!(!ok(&round).by_hand);
    let by_hand = ok(&[
        "--base",
        "tip",
        "--judged",
        "judged",
        "--shared",
        "/s",
        "--by-hand",
    ]);
    assert!(by_hand.by_hand && by_hand.narrows() && !by_hand.all);
    assert_eq!(by_hand.checks, DEV);
    for refused in [
        &["--by-hand"][..],
        &["--base", "tip", "--by-hand"],
        &["--all", "--by-hand"],
        &["-p", "aterm-grid", "--by-hand"],
    ] {
        let refusal = parse(refused.iter().copied()).unwrap_err();
        assert!(
            refusal.message().contains("it needs --judged and --shared"),
            "{refused:?}: {}",
            refusal.message()
        );
    }
}
