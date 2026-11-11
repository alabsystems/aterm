// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The candidate line on real git: a bare origin whose main moved past the line's base, a rival
//! clone that pushes, and the release's work clone (`crate::line`).

use crate::ledger::{self, GitRunner, Origin, RefWrite, RunOut};
use crate::line::{self, Admitted, Fix, Hop, Line, Merged, Reason, Recorded, Stop};

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use ledger::testing::Git;

const VERSION: &str = "0.26.0";

/// Real git merge-tree/commit-tree and the actual changelog gate. The old head is red;
/// the carried tree is green and excludes main's unrelated change. NEGATIVE CONTROL:
/// judging before carrying (the former dry flow) returns the old head's red.
#[test]
fn dry_fix_step_one_judges_the_carried_tree() {
    let f = Fixture::merging("dry-tree");
    run(&f.rival, &["checkout", "-q", "--detach", &f.shas.g]);
    let bad = commit(
        &f.rival,
        "orphan changelog",
        &[(
            "CHANGELOG.md",
            "# Changelog\n\n- Orphan entry.\n\n## [Unreleased]\n\n- Change.\n",
        )],
    );
    let line = Line::start(VERSION, &bad, 1);
    commit(
        &f.rival,
        "unrelated main change",
        &[("unrelated.txt", "main only\n")],
    );
    let repair = commit(
        &f.rival,
        "repair orphan",
        &[(
            "CHANGELOG.md",
            "# Changelog\n\n## [Unreleased]\n\n- Orphan entry.\n- Change.\n",
        )],
    );
    let fix = Fix {
        commit: repair,
        subject: "repair orphan".into(),
        paths: vec!["CHANGELOG.md".into()],
    };
    let git = Git::new(&f.rival);
    assert!(crate::gates::changelog_gate_at(&git, &bad, VERSION).is_err());
    let hops = line::carry(&git, &line, std::slice::from_ref(&fix)).unwrap();
    let planned = &hops[0].commit;
    crate::gates::changelog_gate_at(&git, planned, VERSION).unwrap();
    assert!(
        !git.git(&["cat-file", "-e", &format!("{planned}:unrelated.txt")])
            .unwrap()
            .success()
    );
    let before = run(&f.rival, &["show-ref"]);
    let head = rev(&f.rival, "HEAD");
    let (code, last, rows) =
        super::release_flow::real_carry_fixture(&f.rival, &line, &[fix], true, true);
    assert_eq!(code, 0, "{last}\n{rows:#?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("preflight passes") && row.contains(&planned[..9]))
    );
    assert_eq!(run(&f.rival, &["show-ref"]), before);
    assert_eq!(rev(&f.rival, "HEAD"), head);
}

#[test]
fn dry_carry_judges_real_doc_conflicts_and_clean_hops() {
    let f = Fixture::merging("dry-carry");
    run(&f.rival, &["checkout", "-q", "--detach", &f.shas.g]);
    let base = commit(
        &f.rival,
        "base for carry",
        &[("docs/note.md", "base\n"), ("src/fix.rs", "old\n")],
    );
    let head = commit(&f.rival, "line docs", &[("docs/note.md", "line\n")]);
    run(&f.rival, &["checkout", "-q", "--detach", &base]);
    let conflict = commit(
        &f.rival,
        "fix with conflicting docs",
        &[("docs/note.md", "main\n"), ("src/fix.rs", "fixed\n")],
    );
    run(&f.rival, &["checkout", "-q", "--detach", &base]);
    let clean = commit(&f.rival, "clean fix", &[("src/fix.rs", "fixed\n")]);
    let line = Line::start(VERSION, &head, 1);
    let fix = |commit: &str| Fix {
        commit: commit.into(),
        subject: run(&f.rival, &["show", "-s", "--format=%s", commit])
            .trim()
            .into(),
        paths: vec!["src/fix.rs".into()],
    };
    let before = run(&f.rival, &["show-ref"]);
    let head_before = rev(&f.rival, "HEAD");
    let conflict = fix(&conflict);
    let stop =
        line::carry(&Git::new(&f.rival), &line, std::slice::from_ref(&conflict)).unwrap_err();
    let (dry_code, dry_last, dry) = super::release_flow::real_carry_fixture(
        &f.rival,
        &line,
        std::slice::from_ref(&conflict),
        true,
        false,
    );
    println!("docs-conflict dry: {dry_code} {dry_last}");
    assert_eq!(dry_code, 1, "{dry_last}\n{dry:#?}");
    assert!(
        dry.iter()
            .any(|row| row.contains("WOULD REFUSE") && row.contains(&stop.why))
    );
    assert!(dry.iter().any(|row| row.contains(&stop.remedy)));
    let (real_code, real_last, real) = super::release_flow::real_carry_fixture(
        &f.rival,
        &line,
        std::slice::from_ref(&conflict),
        false,
        false,
    );
    println!("docs-conflict real: {real_code} {real_last}");
    assert_eq!(real_code, 1, "{real_last}\n{real:#?}");
    assert!(real_last.contains(&stop.why) && real_last.contains(&stop.remedy));
    let clean = fix(&clean);
    let (code, last, rows) = super::release_flow::real_carry_fixture(
        &f.rival,
        &line,
        std::slice::from_ref(&clean),
        true,
        false,
    );
    println!("clean dry: {code} {last}");
    assert_eq!(code, 0, "{last}\n{rows:#?}");
    assert!(last.contains("would go ahead"));
    let hops = line::carry(&Git::new(&f.rival), &line, &[clean]).unwrap();
    assert_eq!(hops.len(), 1);
    assert!(rows.iter().any(|row| row.contains(&hops[0].commit[..9])));
    assert_eq!(run(&f.rival, &["show-ref"]), before, "carry writes no refs");
    assert_eq!(rev(&f.rival, "HEAD"), head_before);

    // Execute the printed re-author recipe through its commit, with publication omitted.
    let recipe = stop
        .detail
        .iter()
        .position(|row| row.starts_with("re-make plan only"))
        .expect("docs-only conflicts offer a recipe");
    let destination = f._root.path().join("reauthored");
    let commands = stop.detail[recipe + 1..recipe + 5].join("\n").replace(
        "<new-worktree>",
        &crate::release::shell_word(&destination.display().to_string()),
    );
    let output = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-eu", "-c", &commands])
        .current_dir(&f.rival)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(destination.join("src/fix.rs")).unwrap(),
        "fixed\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("docs/note.md")).unwrap(),
        "line\n"
    );
    assert_eq!(rev(&destination, "HEAD^"), head);
    println!(
        "fixture rows: docs_conflict dry=refused real=refused; clean dry=go-ahead real=1-hop; recipe=code-only; ref_writes=0"
    );
}

#[test]
fn qualify_hints_rank_test_file_then_crate_and_judge_each_patch() {
    let f = Fixture::merging("qualify-hints");
    let base = commit(
        &f.rival,
        "red base",
        &[
            (
                "crates/red/src/tests.rs",
                "#[test]\nfn broken() { assert!(super::value()); }\n",
            ),
            ("crates/red/src/lib.rs", "fn value() -> bool { false }\n"),
            ("docs/note.md", "base\n"),
            ("crates/other/src/lib.rs", "fn unrelated() {}\n"),
        ],
    );
    let head = commit(&f.rival, "line doc change", &[("docs/note.md", "line\n")]);
    run(&f.rival, &["checkout", "-q", "--detach", &base]);
    let clean = commit(
        &f.rival,
        "fix test",
        &[(
            "crates/red/src/tests.rs",
            "#[test]\nfn broken() { let _ = super::value(); }\n",
        )],
    );
    let conflict = commit(
        &f.rival,
        "fix crate with docs",
        &[
            ("crates/red/src/lib.rs", "fn value() -> bool { true }\n"),
            ("docs/note.md", "main\n"),
        ],
    );
    let unrelated = commit(
        &f.rival,
        "unrelated",
        &[("crates/other/src/lib.rs", "fn unrelated() { }\n")],
    );
    let git = Git::new(&f.rival);
    run(
        &f.rival,
        &["update-ref", "refs/remotes/origin/main", &unrelated],
    );
    let mut line = Line::start(VERSION, &base, 1);
    line.hops.push(Hop {
        commit: head.clone(),
        ..Hop::default()
    });
    let before = rev(&f.rival, "HEAD");
    let hints =
        line::failure_hints(&git, &line, "test tests::broken ... FAILED\nFAIL red --lib").unwrap();
    let mut checks = crate::check_record::Checks::default();
    checks.tree = base.clone();
    checks.failed.push(crate::check_record::Failed {
        check: "test".into(),
        log: "fixture-tree.log".into(),
        unrecorded: false,
    });
    let mut refusal = checks.fail_refusal(&head).unwrap();
    let count = |rows: &[String]| {
        rows.iter()
            .filter(|row| row.starts_with(&clean) || row.starts_with(&conflict))
            .count()
    };
    assert_eq!(
        count(&refusal.detail),
        0,
        "baseline FAIL has no commit hints"
    );
    refusal.detail.extend(hints.clone());
    assert_eq!(count(&refusal.detail), 2);
    println!(
        "fixture refusal candidates: before=0 after=2; carry verdicts: clean=1 docs_conflict=1; unrelated=0"
    );
    assert!(
        hints[1].contains(&clean) && hints[1].contains("[test file]: carries cleanly"),
        "{hints:?}"
    );
    assert!(
        hints[2].contains(&conflict) && hints[2].contains("[crate]: conflicts in docs/note.md"),
        "{hints:?}"
    );
    assert!(hints[3].contains("re-make plan only") && hints[4].contains(&head));
    assert!(!hints.join("\n").contains(&unrelated));
    assert_eq!(rev(&f.rival, "HEAD"), before, "hints never carry a patch");
    // Negative control: neither main fix touches this failing crate.
    let control = line::failure_hints(&git, &line, "FAIL other --lib").unwrap();
    assert!(control.iter().any(|s| s.contains(&unrelated)));
    run(
        &f.rival,
        &["update-ref", "refs/remotes/origin/main", &conflict],
    );
    let control = line::failure_hints(&git, &line, "FAIL other --lib").unwrap();
    assert_eq!(control[1], "no main commit since the base touches it");
    // Negative control for the doc-only plan: a source conflict needs an authored fix.
    run(&f.rival, &["checkout", "-q", "--detach", &head]);
    let source_head = commit(
        &f.rival,
        "line source change",
        &[("crates/red/src/lib.rs", "fn value() -> bool { panic!() }\n")],
    );
    line.hops[0].commit = source_head;
    let conflict = line::failure_hints(&git, &line, "FAIL red --lib").unwrap();
    assert!(
        conflict
            .iter()
            .any(|s| s.contains("conflicts in crates/red/src/lib.rs"))
    );
    assert!(!conflict.iter().any(|s| s.contains("re-make plan")));
}

#[test]
fn supersession_is_durable_ordered_and_keeps_mains_tree() {
    let fixture = Fixture::new("supersede");
    let git = fixture.git();
    let origin = LocalOrigin::new(&fixture);
    let old = Line::start(VERSION, &fixture.shas.g, 1);
    let new = Line::start(VERSION, &fixture.shas.fix_b, 2);
    assert_eq!(line::supersession(&git, VERSION).unwrap(), None);
    let record = line::Supersession {
        previous: old.clone(),
        replacement: new.clone(),
        reason: "unfinishable cutter".into(),
    };
    let tree = rev(&fixture.origin, "main^{tree}");
    let merge = line::supersede(&git, &origin, &record).unwrap();
    assert_eq!(rev(&fixture.origin, "main^{tree}"), tree);
    assert_eq!(
        line::supersession(&git, VERSION).unwrap(),
        Some(record.clone())
    );
    assert_eq!(
        crate::line_claim::current(&git, VERSION, Some(old.clone())).unwrap(),
        Some(new.clone()),
        "main's retirement overrides a claim still naming the old base"
    );
    assert_eq!(line::supersede(&git, &origin, &record).unwrap(), merge);
    assert_eq!(origin.writes.get(), 1);

    // verify land pushes a merge with the branch first and the old main second.
    let branch = run(
        &fixture.work,
        &[
            "commit-tree",
            &format!("{}^{{tree}}", fixture.shas.tip),
            "-p",
            &fixture.shas.tip,
            "-m",
            "worker branch commit",
        ],
    );
    let landed = run(
        &fixture.work,
        &[
            "commit-tree",
            &format!("{merge}^{{tree}}"),
            "-p",
            branch.trim(),
            "-p",
            &merge,
            "-m",
            "Merge origin/main into worker",
        ],
    );
    let landed = landed.trim();
    run(
        &fixture.work,
        &["push", "origin", &format!("{landed}:refs/heads/main")],
    );
    origin.refresh_main().unwrap();
    let control = run(
        &fixture.work,
        &[
            "log",
            "origin/main",
            "--first-parent",
            "--format=%B%x00",
            "--fixed-strings",
            "--grep=Aterm-Line-Supersession: ",
        ],
    );
    assert!(control.trim().is_empty(), "today's reader returns None");
    assert_eq!(
        line::supersession(&git, VERSION).unwrap(),
        Some(record.clone())
    );
    assert_eq!(
        crate::line_claim::current(&git, VERSION, Some(old.clone())).unwrap(),
        Some(new.clone())
    );

    let newer = line::Supersession {
        previous: new,
        replacement: Line::start(VERSION, &fixture.shas.frozen, 3),
        reason: "second explicit retirement".into(),
    };
    line::supersede(&git, &origin, &newer).unwrap();
    assert_eq!(line::supersession(&git, VERSION).unwrap(), Some(newer));
    // Negative control: a stale writer cannot restore the first replacement.
    assert!(line::supersede(&git, &origin, &record).is_err());
    let backwards = line::Supersession {
        previous: record.replacement,
        replacement: old,
        reason: "backwards".into(),
    };
    assert!(line::supersede(&git, &origin, &backwards).is_err());
}

/// A replacement binds the selected main commit while the retirement record follows a
/// concurrent main landing. A second cutter cannot replace that committed decision.
#[test]
fn supersession_retries_moving_main_without_reselecting_the_base() {
    let f = Fixture::merging("supersede-moving-main");
    // This release is still on VERSION, before the fixture's next bump and frozen changes.
    run(
        &f.origin,
        &["update-ref", "refs/heads/main", &f.shas.rename],
    );
    run(&f.work, &["fetch", "-q", "origin", "main"]);
    let old = Line::start(VERSION, &f.shas.g, 1);
    assert!(matches!(
        line::hop_tree(&f.git(), old.head(), &f.shas.fix_c).unwrap(),
        Merged::Conflict(_)
    ));
    let selected = f.origin_main();
    let replacement = Line::start(VERSION, &selected, 2);
    // The old staged refusal leaves the full-tree backport workaround: the later fix
    // conflicts, but copying main's complete tree onto the line passes admission.
    let backport = run(
        &f.work,
        &[
            "commit-tree",
            &format!("{selected}^{{tree}}"),
            "-p",
            old.head(),
            "-m",
            "full-tree backport workaround",
        ],
    )
    .trim()
    .to_owned();
    neutral_merge(&f, &backport, false);
    assert!(matches!(
        line::admit(&f.git(), &old, &backport),
        Ok(Admitted::Fix(_))
    ));
    let record = line::Supersession {
        previous: old.clone(),
        replacement: replacement.clone(),
        reason: "staged bundle failure".into(),
    };
    let mut origin = LocalOrigin::new(&f);
    origin.before_write = RefCell::new(Box::new(|attempt| {
        if attempt == 1 {
            f.rival_moves_main("fn concurrent_main() {}\n");
        }
    }));
    line::supersede(&f.git(), &origin, &record).unwrap();
    assert_eq!(origin.writes.get(), 2);
    assert_eq!(line::supersession(&f.git(), VERSION).unwrap(), Some(record));
    assert_eq!(replacement.head(), selected);
    line::verify(&f.git(), &replacement).unwrap();
    assert_eq!(rev(&f.origin, "main^{tree}"), rev(&f.rival, "HEAD^{tree}"));
    let competitor = line::Supersession {
        previous: old,
        replacement: Line::start(VERSION, &f.origin_main(), 3),
        reason: "second cutter".into(),
    };
    assert!(line::supersede(&f.git(), &origin, &competitor).is_err());
}

/// NEGATIVE CONTROL: the former tip comparison expires after main edits a carried path.
/// Step 4 must still verify the recorded hop against its immutable landed content.
#[test]
fn a_recorded_backport_reverifies_after_main_edits_its_paths() {
    let f = Fixture::merging("expired-backport");
    run(
        &f.origin,
        &["update-ref", "refs/heads/main", &f.shas.rename],
    );
    run(&f.work, &["fetch", "-q", "origin", "main"]);
    let mut old = Line::start(VERSION, &f.shas.g, 1);
    let selected = f.origin_main();
    let backport = run(
        &f.work,
        &[
            "commit-tree",
            &format!("{selected}^{{tree}}"),
            "-p",
            old.head(),
            "-m",
            "full-tree backport",
        ],
    )
    .trim()
    .to_owned();
    run(&f.rival, &["fetch", "-q", "origin", "main"]);
    run(&f.rival, &["reset", "-q", "--hard", "origin/main"]);
    commit(
        &f.rival,
        "concurrent unrelated landing",
        &[("later.txt", "main moved\n")],
    );
    run(&f.rival, &["push", "-q", "origin", "main"]);
    run(&f.work, &["fetch", "-q", "origin", "main"]);
    neutral_merge(&f, &backport, false);
    assert!(
        !line::landed_since(&f.git(), old.head())
            .unwrap()
            .contains(&backport)
    );
    let admitted = fix_of(&f.git(), &old, &backport);
    old.hops = line::carry(&f.git(), &old, &[admitted]).unwrap();
    let origin = LocalOrigin::new(&f);
    line::record(
        &f.git(),
        &origin,
        VERSION,
        old.head(),
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    line::verify(&f.git(), &old).unwrap();
    let tip = f.rival_moves_main("fn later_main() {}\n");
    origin.refresh_main().unwrap();
    line::verify(&f.git(), &old).unwrap();
    let replacement = Line::start(VERSION, &tip, 2);
    let retirement = line::Supersession {
        previous: old,
        replacement: replacement.clone(),
        reason: "replace the older line".into(),
    };
    line::supersede(&f.git(), &origin, &retirement).unwrap();
    line::verify(&f.git(), &replacement).unwrap();
    assert_eq!(
        line::supersession(&f.git(), VERSION).unwrap(),
        Some(retirement)
    );
}

/// Real history input to the release flow, including the old reader's negative control.
pub(super) fn hidden_supersession() -> line::Supersession {
    let fixture = Fixture::new("hidden-flow");
    let origin = LocalOrigin::new(&fixture);
    let record = line::Supersession {
        previous: Line::start(VERSION, &fixture.shas.g, 1),
        replacement: Line::start(VERSION, &fixture.shas.fix_b, 2),
        reason: "explicit retirement".into(),
    };
    let merge = line::supersede(&fixture.git(), &origin, &record).unwrap();
    let branch = run(
        &fixture.work,
        &[
            "commit-tree",
            &format!("{}^{{tree}}", fixture.shas.tip),
            "-p",
            &fixture.shas.tip,
            "-m",
            "worker branch commit",
        ],
    );
    let landed = run(
        &fixture.work,
        &[
            "commit-tree",
            &format!("{merge}^{{tree}}"),
            "-p",
            branch.trim(),
            "-p",
            &merge,
            "-m",
            "Merge origin/main into worker",
        ],
    );
    run(
        &fixture.work,
        &[
            "push",
            "origin",
            &format!("{}:refs/heads/main", landed.trim()),
        ],
    );
    origin.refresh_main().unwrap();
    assert!(
        run(
            &fixture.work,
            &[
                "log",
                "origin/main",
                "--first-parent",
                "--format=%B%x00",
                "--fixed-strings",
                "--grep=Aterm-Line-Supersession: "
            ]
        )
        .trim()
        .is_empty()
    );
    line::supersession(&fixture.git(), VERSION)
        .unwrap()
        .unwrap()
}

#[test]
fn contradictory_retirements_refuse_instead_of_picking_git_log_order() {
    for disconnected in [false, true] {
        let fixture = Fixture::new("contradictory");
        let origin = LocalOrigin::new(&fixture);
        let record = line::Supersession {
            previous: Line::start(VERSION, &fixture.shas.g, 1),
            replacement: Line::start(VERSION, &fixture.shas.fix_b, 2),
            reason: "first retirement".into(),
        };
        let merge = line::supersede(&fixture.git(), &origin, &record).unwrap();
        let mut contradictory = record.clone();
        contradictory.reason = "different decision".into();
        contradictory.replacement = Line::start(VERSION, &fixture.shas.frozen, 3);
        if disconnected {
            contradictory.previous = Line::start(VERSION, &fixture.shas.n1, 1);
        }
        let message = format!(
            "competing record\n\nAterm-Line-Supersession: {}\n",
            aterm_json::to_string(&contradictory).unwrap()
        );
        let commit = run(
            &fixture.work,
            &[
                "commit-tree",
                &format!("{merge}^{{tree}}"),
                "-p",
                &merge,
                "-m",
                &message,
            ],
        );
        run(
            &fixture.work,
            &[
                "push",
                "origin",
                &format!("{}:refs/heads/main", commit.trim()),
            ],
        );
        origin.refresh_main().unwrap();
        assert!(line::supersession(&fixture.git(), VERSION).is_err());
        assert!(
            crate::line_claim::current(&fixture.git(), VERSION, Some(record.previous)).is_err()
        );
        assert!(line::supersede(&fixture.git(), &origin, &contradictory).is_err());
        assert_eq!(
            origin.writes.get(),
            1,
            "ambiguous reads permit no further write"
        );
    }
}

#[test]
fn retirement_chain_ignores_log_order_and_rejects_cycles() {
    struct Log(Vec<line::Supersession>);
    impl GitRunner for Log {
        fn git(&self, _: &[&str]) -> ledger::Result<RunOut> {
            Ok(RunOut {
                status: 0,
                stdout: self
                    .0
                    .iter()
                    .map(|record| {
                        format!(
                            "Aterm-Line-Supersession: {}\n\0",
                            aterm_json::to_string(record).unwrap()
                        )
                    })
                    .collect::<String>()
                    .into_bytes(),
                stderr: Vec::new(),
            })
        }
    }
    let a = Line::start(VERSION, "a", 1);
    let b = Line::start(VERSION, "b", 2);
    let c = Line::start(VERSION, "c", 3);
    let first = line::Supersession {
        previous: a.clone(),
        replacement: b.clone(),
        reason: "first".into(),
    };
    let second = line::Supersession {
        previous: b,
        replacement: c,
        reason: "second".into(),
    };
    for records in [
        vec![first.clone(), second.clone()],
        vec![second.clone(), first.clone()],
        vec![first.clone(), second.clone(), first.clone()],
    ] {
        assert_eq!(
            line::supersession(&Log(records), VERSION).unwrap(),
            Some(second.clone())
        );
    }
    let cycle = line::Supersession {
        previous: second.replacement.clone(),
        replacement: a,
        reason: "cycle".into(),
    };
    assert!(line::supersession(&Log(vec![first, second, cycle]), VERSION).is_err());
}

#[test]
fn every_public_surface_prevents_supersession() {
    assert!(line::require_unpublished([false; 4]).is_ok());
    for index in 0..4 {
        let mut public = [false; 4];
        public[index] = true;
        assert!(
            line::require_unpublished(public).is_err(),
            "surface {index}"
        );
    }
}

/// The commits the template's history holds, oldest first.
#[derive(Clone)]
struct Shas {
    /// `seed`, the root: it declares v0.26.0.
    seed: String,
    /// The line's base, `feat: the base`.
    g: String,
    /// `fix: a1`, on a branch from G, landed by `merge`.
    fix_a: String,
    /// `feat: main moves c3`, on main while the branch was out.
    n1: String,
    /// `Merge remote-tracking branch 'origin/main' into fix-a` (parents `fix_a`, `n1`), pushed
    /// fast-forward: how landings look on aterm's main.
    merge: String,
    /// `fix: c1`, which edits a line that main changed after G: it conflicts with G.
    fix_c: String,
    /// `fix: b2`, a clean fix that descends from `fix_a`.
    fix_b: String,
    /// `fix: d`, which `d_revert` takes back.
    d_fix: String,
    /// `revert: fix: d`: its tree lacks `d_fix`'s change, and its history holds `d_fix`.
    d_revert: String,
    /// `refactor: src/c.rs is src/c2.rs`, a pure rename of a file main changed after G.
    rename: String,
    /// `fix: b5 and the toolchain pin`, moving `rust-toolchain.toml`.
    frozen: String,
    /// `release: a claim`, appending to `RELEASES.ledger`.
    ledger: String,
    /// `bump: v0.27.0`.
    bump: String,
    /// main's tip.
    tip: String,
    /// `fix: a3`, on origin's `side` branch only.
    off: String,
}

struct Fixture {
    _root: aterm_tempfile::TempDir,
    origin: PathBuf,
    rival: PathBuf,
    work: PathBuf,
    shas: Shas,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        static BUILT: crate::template::Template<Shas> = crate::template::Template::new();
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-release-line-{name}-"))
            .suffix(".noindex")
            .tempdir()
            .unwrap();
        let root = dir.path().to_path_buf();
        let shas = BUILT.lay(&root, build);
        Fixture {
            origin: root.join("origin.git"),
            rival: root.join("rival"),
            work: root.join("work"),
            _root: dir,
            shas,
        }
    }

    /// [`Fixture::new`] for a test that makes a hop's tree ([`line::hop_tree`]), which needs a
    /// git ≥ [`line::MIN_GIT`]: on an older one the test is COULD NOT RUN.
    fn merging(name: &str) -> Fixture {
        let fix = Fixture::new(name);
        if let Err(stop) = line::git_ready(&fix.git()) {
            aterm_spec::host::could_not_run(stop.why);
        }
        fix
    }

    fn git(&self) -> Git {
        Git::new(&self.work)
    }

    fn origin_main(&self) -> String {
        rev(&self.origin, "main")
    }

    /// The rival lands one commit on main: `src/a.rs` gains `line`.
    fn rival_moves_main(&self, line: &str) -> String {
        run(&self.rival, &["fetch", "-q", "origin", "main"]);
        run(&self.rival, &["reset", "-q", "--hard", "origin/main"]);
        let path = self.rival.join("src/a.rs");
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str(line);
        text.push('\n');
        fs::write(&path, text).unwrap();
        run(
            &self.rival,
            &["commit", "-q", "-am", &format!("rival: {line}")],
        );
        run(&self.rival, &["push", "-q", "origin", "main"]);
        rev(&self.rival, "HEAD")
    }
}

const A: &str = "fn a1() {}\nfn a2() {}\nfn a3() {}\nfn a4() {}\nfn a5() {}\n";
const B: &str = "fn b1() {}\nfn b2() {}\nfn b3() {}\nfn b4() {}\nfn b5() {}\n";
const C: &str = "fn c1() {}\nfn c2() {}\nfn c3() {}\n";

fn build(root: &Path) -> Shas {
    let origin = root.join("origin.git");
    run(root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    ledger::testing::config(&origin, &[]);
    run(root, &["clone", "-q", origin.to_str().unwrap(), "rival"]);
    let rival = root.join("rival");
    identity(&rival);
    run(&rival, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let seed = commit(
        &rival,
        "seed",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.26.0\"\n",
            ),
            ("RELEASES.ledger", "1783354739 0.25.0\n"),
            ("rust-toolchain.toml", "[toolchain]\nchannel = \"trust\"\n"),
            ("src/a.rs", A),
            ("src/b.rs", B),
            ("src/c.rs", C),
        ],
    );
    let g = commit(&rival, "feat: the base", &[("src/d.rs", "fn d() {}\n")]);
    run(&rival, &["push", "-q", "-u", "origin", "main"]);

    run(&rival, &["checkout", "-q", "-b", "fix-a", &g]);
    let fix_a = commit(
        &rival,
        "fix: a1",
        &[("src/a.rs", &A.replace("a1() {}", "a1() { fixed }"))],
    );
    run(&rival, &["checkout", "-q", "main"]);
    let n1 = commit(
        &rival,
        "feat: main moves c3",
        &[("src/c.rs", &C.replace("c3() {}", "c3() { main }"))],
    );
    run(&rival, &["push", "-q", "origin", "main"]);
    run(&rival, &["checkout", "-q", "fix-a"]);
    run(
        &rival,
        &["merge", "-q", "--no-ff", "--no-edit", "origin/main"],
    );
    let merge = rev(&rival, "HEAD");
    run(&rival, &["push", "-q", "origin", "fix-a:main"]);
    run(&rival, &["checkout", "-q", "-B", "main", &merge]);

    let c_main = fs::read_to_string(rival.join("src/c.rs"))
        .unwrap()
        .replace("c1() {}", "c1() { main }");
    commit(&rival, "feat: main moves c1", &[("src/c.rs", &c_main)]);
    let fix_c = commit(
        &rival,
        "fix: c1",
        &[(
            "src/c.rs",
            &c_main.replace("c1() { main }", "c1() { fixed }"),
        )],
    );
    let fix_b = commit(
        &rival,
        "fix: b2",
        &[("src/b.rs", &B.replace("b2() {}", "b2() { fixed }"))],
    );
    let d_fix = commit(&rival, "fix: d", &[("src/d.rs", "fn d() { fixed }\n")]);
    let d_revert = commit(&rival, "revert: fix: d", &[("src/d.rs", "fn d() {}\n")]);
    run(&rival, &["mv", "src/c.rs", "src/c2.rs"]);
    let rename = commit(&rival, "refactor: src/c.rs is src/c2.rs", &[]);
    let b_fixed = fs::read_to_string(rival.join("src/b.rs")).unwrap();
    let frozen = commit(
        &rival,
        "fix: b5 and the toolchain pin",
        &[
            ("src/b.rs", &b_fixed.replace("b5() {}", "b5() { fixed }")),
            (
                "rust-toolchain.toml",
                "[toolchain]\nchannel = \"trust-next\"\n",
            ),
        ],
    );
    let ledger = commit(
        &rival,
        "release: a claim",
        &[("RELEASES.ledger", "1783354739 0.25.0\n1790000000 0.26.0\n")],
    );
    let bump = commit(
        &rival,
        "bump: v0.27.0",
        &[(
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.27.0\"\n",
        )],
    );
    run(&rival, &["push", "-q", "origin", "main"]);
    let tip = bump.clone();

    run(&rival, &["checkout", "-q", "-b", "side", &fix_b]);
    let a_side = fs::read_to_string(rival.join("src/a.rs"))
        .unwrap()
        .replace("a3() {}", "a3() { off main }");
    let off = commit(&rival, "fix: a3", &[("src/a.rs", &a_side)]);
    run(&rival, &["push", "-q", "origin", "side"]);
    run(&rival, &["checkout", "-q", "main"]);

    run(root, &["clone", "-q", origin.to_str().unwrap(), "work"]);
    identity(&root.join("work"));
    Shas {
        seed,
        g,
        fix_a,
        n1,
        merge,
        fix_c,
        fix_b,
        d_fix,
        d_revert,
        rename,
        frozen,
        ledger,
        bump,
        tip,
        off,
    }
}

fn identity(repo: &Path) {
    ledger::testing::config(
        repo,
        &[
            ("user.email", "ship-test@aterm.invalid"),
            ("user.name", "aterm ship test"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ],
    );
}

/// Write each file, then commit them all: the new commit's sha.
fn commit(repo: &Path, message: &str, files: &[(&str, &str)]) -> String {
    for (path, text) in files {
        let path = repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    run(repo, &["add", "-A"]);
    run(repo, &["commit", "-q", "-m", message]);
    rev(repo, "HEAD")
}

fn run(cwd: &Path, args: &[&str]) -> String {
    run_env::<&str>(cwd, args, &[])
}

fn run_env<V: AsRef<OsStr>>(cwd: &Path, args: &[&str], env: &[(&str, V)]) -> String {
    let mut git = ledger::testing::git();
    for (key, value) in env {
        git.env(key, value);
    }
    let out = git
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rev(repo: &Path, name: &str) -> String {
    run(repo, &["rev-parse", name]).trim().to_string()
}

fn parents(repo: &Path, commit: &str) -> Vec<String> {
    run(repo, &["rev-list", "--parents", "-n", "1", commit])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    ledger::testing::git()
        .current_dir(repo)
        .args(["merge-base", "--is-ancestor", a, b])
        .output()
        .unwrap()
        .status
        .success()
}

fn short(sha: &str) -> &str {
    &sha[..12]
}

/// `fix`'s patch, as `git diff` writes it, applied by `git apply` to `base`'s tree in an index of
/// its own: the tree a hop of `fix` onto `base` must have, computed without `merge-tree`.
fn applied(fix: &Fixture, base: &str, commit: &str) -> String {
    let index = fix.work.join(".git/oracle-index");
    let patch = fix.work.join(".git/oracle.patch");
    let diff = run(
        &fix.work,
        &[
            "diff",
            "--binary",
            "--no-renames",
            &format!("{commit}^"),
            commit,
        ],
    );
    fs::write(&patch, diff).unwrap();
    let env = [("GIT_INDEX_FILE", index.as_path())];
    run_env(&fix.work, &["read-tree", base], &env);
    run_env(
        &fix.work,
        &["apply", "--cached", patch.to_str().unwrap()],
        &env,
    );
    let tree = run_env(&fix.work, &["write-tree"], &env).trim().to_string();
    fs::remove_file(&index).unwrap();
    tree
}

/// `base`'s tree plus `path`, committed with `parent`: a hop nobody carried.
fn tampered(fix: &Fixture, base: &str, parent: &str, path: &str) -> String {
    let index = fix.work.join(".git/tamper-index");
    let env = [("GIT_INDEX_FILE", index.as_path())];
    run_env(&fix.work, &["read-tree", base], &env);
    let blob_file = fix.work.join(".git/tamper-blob");
    fs::write(&blob_file, "planted\n").unwrap();
    let blob = run(
        &fix.work,
        &["hash-object", "-w", blob_file.to_str().unwrap()],
    );
    let info = format!("100644,{},{path}", blob.trim());
    run_env(
        &fix.work,
        &["update-index", "--add", "--cacheinfo", &info],
        &env,
    );
    let tree = run_env(&fix.work, &["write-tree"], &env).trim().to_string();
    fs::remove_file(&index).unwrap();
    run(
        &fix.work,
        &["commit-tree", &tree, "-p", parent, "-m", "tampered"],
    )
    .trim()
    .to_string()
}

/// A hop of `carries` onto `parent` with the tree [`line::hop_tree`] gives it, made by hand.
fn hand_hop(fix: &Fixture, parent: &str, carries: &str) -> Hop {
    let Merged::Tree(tree) = line::hop_tree(&fix.git(), parent, carries).unwrap() else {
        panic!("{carries} conflicts onto {parent}");
    };
    let commit = run(
        &fix.work,
        &["commit-tree", &tree, "-p", parent, "-m", "by hand"],
    )
    .trim()
    .to_string();
    let paths = run(
        &fix.work,
        &[
            "diff-tree",
            "-r",
            "--name-only",
            "--no-renames",
            parent,
            &commit,
        ],
    )
    .lines()
    .map(str::to_string)
    .collect();
    Hop {
        commit,
        carries: carries.to_string(),
        why: "by hand".to_string(),
        paths,
    }
}

fn fix_of(git: &dyn GitRunner, l: &Line, rev: &str) -> Fix {
    match line::admit(git, l, rev) {
        Ok(Admitted::Fix(fix)) => fix,
        other => panic!("{rev} is not admitted as a fix: {other:?}"),
    }
}

/// The stop `result` ended in, which must have `reason`.
fn stopped<T: Debug>(result: Result<T, Stop>, reason: Reason) -> Stop {
    match result {
        Err(stop) => {
            assert_eq!(stop.reason, reason, "{stop:?}");
            assert_eq!(stop.could_not_run(), reason.could_not_run(), "{stop:?}");
            stop
        }
        Ok(done) => panic!("wanted {reason:?}, and it did: {done:?}"),
    }
}

/// A commit of G's tree on G under a pinned author and committer at `time`: a sibling of every
/// other one, whose sha the message and the time alone decide.
fn sibling(fix: &Fixture, message: &str, time: u64) -> Fix {
    let date = format!("{time} +0000");
    let env = [
        ("GIT_AUTHOR_NAME", "aterm ship test"),
        ("GIT_AUTHOR_EMAIL", "ship-test@aterm.invalid"),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_NAME", "aterm ship test"),
        ("GIT_COMMITTER_EMAIL", "ship-test@aterm.invalid"),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    let g = &fix.shas.g;
    let commit = run_env(
        &fix.work,
        &[
            "commit-tree",
            &format!("{g}^{{tree}}"),
            "-p",
            g,
            "-m",
            message,
        ],
        &env,
    )
    .trim()
    .to_string();
    Fix {
        commit,
        subject: message.to_string(),
        paths: Vec::new(),
    }
}

/// File-only typed origin for the real-object line fixtures. The production
/// GitRunner never needs a remote command; this seam owns all origin effects.
struct LocalOrigin<'a> {
    fix: &'a Fixture,
    writes: Cell<u32>,
    refreshes: Cell<u32>,
    before_write: RefCell<Box<dyn FnMut(u32) + 'a>>,
    no_effect: bool,
    failure: Option<String>,
    fail_after_effect: bool,
}

impl<'a> LocalOrigin<'a> {
    fn new(fix: &'a Fixture) -> Self {
        Self {
            fix,
            writes: Cell::new(0),
            refreshes: Cell::new(0),
            before_write: RefCell::new(Box::new(|_| {})),
            no_effect: false,
            failure: None,
            fail_after_effect: false,
        }
    }

    fn checked(repo: &Path, args: &[&str]) -> Result<String, String> {
        let out = Git::new(repo).git(args).map_err(|e| e.to_string())?;
        if out.success() {
            Ok(out.stdout_utf8().trim().to_string())
        } else {
            Err(out.stderr_utf8())
        }
    }
}

impl Origin for LocalOrigin<'_> {
    fn refresh_main(&self) -> Result<String, String> {
        self.refreshes.set(self.refreshes.get() + 1);
        Self::checked(&self.fix.work, &["fetch", "-q", "origin", "main"])?;
        Self::checked(&self.fix.work, &["rev-parse", "origin/main"])
    }

    fn write(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<RefWrite, String> {
        assert_eq!(reference, "refs/heads/main");
        let expected = before.expect("record always supplies the exact observed main");
        let n = self.writes.get() + 1;
        self.writes.set(n);
        (self.before_write.borrow_mut())(n);
        if let Some(why) = &self.failure
            && !self.fail_after_effect
        {
            return Err(why.clone());
        }
        if self.no_effect {
            return Ok(RefWrite::Written);
        }
        let observed = Self::checked(&self.fix.origin, &["rev-parse", reference])?;
        if observed != expected {
            return Ok(RefWrite::Conflict);
        }
        // Import only the exact new local object, then CAS the bare fixture ref.
        Self::checked(
            &self.fix.origin,
            &["fetch", "-q", self.fix.work.to_str().unwrap(), after],
        )?;
        Self::checked(
            &self.fix.origin,
            &["update-ref", reference, after, expected],
        )?;
        if let Some(why) = &self.failure {
            return Err(why.clone());
        }
        Ok(RefWrite::Written)
    }

    fn get(&self, _: &str) -> Result<Option<String>, String> {
        Err("line uses a complete authenticated main refresh".into())
    }
    fn import(&self, _: &str) -> Result<(), String> {
        Err("line refresh imports its own exact objects".into())
    }
    fn peel(&self, _: &str) -> Result<Option<String>, String> {
        Err("line does not read tags".into())
    }
    fn tag_object(&self, _: &str) -> Result<Vec<u8>, String> {
        Err("line does not read tags".into())
    }
    fn delete(&self, _: &str, _: &str) -> Result<RefWrite, String> {
        Err("line deletes nothing".into())
    }
    fn control_transaction(
        &self,
        _: crate::ledger::ControlRefTransaction,
    ) -> Result<RefWrite, String> {
        Err("line writes only main".into())
    }
}

/// Fail the test if production line plumbing tries to use Git for origin I/O.
struct LocalOnly<'a>(&'a dyn GitRunner);

impl GitRunner for LocalOnly<'_> {
    fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
        assert!(!matches!(
            args.first(),
            Some(&"fetch" | &"push" | &"ls-remote")
        ));
        self.0.git(args)
    }
}

#[test]
fn a_clean_hop_is_the_landed_fixs_patch_on_the_base_and_verifies() {
    let fix = Fixture::merging("clean");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 1_790_000_000);
    let admitted = fix_of(&git, &start, &s.fix_b);
    assert_eq!(
        admitted,
        Fix {
            commit: s.fix_b.clone(),
            subject: "fix: b2".to_string(),
            paths: vec!["src/b.rs".to_string()],
        }
    );
    let hops = line::carry(&git, &start, &[admitted]).unwrap();
    let [hop] = hops.as_slice() else {
        panic!("one fix makes one hop: {hops:?}");
    };
    assert_eq!(parents(&fix.work, &hop.commit), [s.g.as_str()]);
    assert_eq!(
        rev(&fix.work, &format!("{}^{{tree}}", hop.commit)),
        applied(&fix, &s.g, &s.fix_b),
        "the hop is G with fix: b2's patch, and none of main's movement since G"
    );
    assert_eq!(
        (hop.carries.as_str(), hop.why.as_str(), hop.paths.as_slice()),
        (
            s.fix_b.as_str(),
            "fix: b2",
            ["src/b.rs".to_string()].as_slice()
        )
    );
    assert_eq!(
        run(&fix.work, &["log", "-1", "--format=%s", &hop.commit]).trim(),
        format!("release line v0.26.0 hop 1: carries {} (fix: b2)", s.fix_b)
    );
    let who = "--format=%an <%ae> %ad %cn <%ce> %cd";
    assert_eq!(
        run(&fix.work, &["log", "-1", who, &hop.commit]),
        run(&fix.work, &["log", "-1", who, &s.fix_b]),
        "the hop keeps the fix's author and committer"
    );

    let mut carried = start.clone();
    carried.hops = hops.clone();
    assert_eq!(carried.head(), hop.commit);
    assert!(line::verify_pending(&git, &carried).is_ok());
    assert_eq!(
        line::recorded_prefix(&git, &carried).unwrap(),
        start,
        "a fleet claim must not advertise an unrecorded hop"
    );
    let mut invalid = carried.clone();
    invalid.hops[0].paths.clear();
    assert!(
        line::verify_pending(&git, &invalid).is_err(),
        "the pre-record proof still checks the actual patch"
    );
    assert!(run(&fix.work, &["status", "--porcelain"]).is_empty());
    run(&fix.work, &["reset", "-q", "--keep", &hop.commit]);
    let mut real =
        crate::release_steps::Real::new(&fix.work, Some(&fix.work), true, false).unwrap();
    assert!(crate::release::Backend::rewind_selection(&mut real, &invalid).is_err());
    assert_eq!(
        rev(&fix.work, "HEAD"),
        hop.commit,
        "an unproved pending line cannot be rewound"
    );
    crate::release::Backend::rewind_selection(&mut real, &carried).unwrap();
    assert_eq!(rev(&fix.work, "HEAD"), start.head());
    let unrecorded = stopped(line::verify(&git, &carried), Reason::Unrecorded);
    assert!(
        unrecorded.remedy.contains("--forget"),
        "a journaled head origin/main lacks lost its record, and no re-run records it: \
         {unrecorded:?}"
    );
    let recorded = line::record(
        &git,
        &LocalOrigin::new(&fix),
        VERSION,
        carried.head(),
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    let Recorded::Pushed { merge, attempt: 1 } = recorded else {
        panic!("a quiet main takes the record merge on the first push: {recorded:?}");
    };
    assert_eq!(fix.origin_main(), merge);
    assert_eq!(line::recorded_prefix(&git, &carried).unwrap(), carried);
    assert_eq!(
        parents(&fix.origin, &merge),
        [s.tip.clone(), hop.commit.clone()]
    );
    assert_eq!(
        rev(&fix.origin, &format!("{merge}^{{tree}}")),
        rev(&fix.origin, &format!("{}^{{tree}}", s.tip)),
        "the record merge leaves main's tree as it was"
    );
    let summary = line::verify(&git, &carried).unwrap();
    assert!(summary.contains("1 hop(s)"), "{summary}");
    assert!(summary.contains(short(&s.fix_b)), "{summary}");
}

#[test]
fn a_fix_that_conflicts_with_the_head_is_refused_with_its_paths() {
    let fix = Fixture::merging("conflict");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 0);
    let fix_c = fix_of(&git, &start, &s.fix_c);
    let stop = stopped(
        line::carry(&git, &start, std::slice::from_ref(&fix_c)),
        Reason::Conflict,
    );
    assert!(stop.why.contains("src/c.rs"), "{stop:?}");
    let hunks = stop.detail.join("\n");
    assert!(hunks.contains("@@") && hunks.contains("<<<<<<<"), "{hunks}");
    assert!(
        hunks.contains("fn c1() {}") && hunks.contains("fn c1() { fixed }"),
        "{hunks}"
    );
    assert!(
        !hunks.contains("re-make plan only"),
        "code conflicts have no exclusion recipe"
    );
    assert!(
        stop.remedy.contains(short(&s.g))
            && stop
                .remedy
                .contains("every changed path matches one landed main commit"),
        "{stop:?}"
    );
    let (code, last, rows) = super::release_flow::real_fix_fixture(
        &fix.work,
        &start,
        std::slice::from_ref(&fix_c),
        true,
    );
    assert_eq!(code, 1, "{last}\n{rows:#?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("<<<<<<<") && row.contains("@@")),
        "{rows:#?}"
    );
    let after_b = stopped(
        line::carry(&git, &start, &[fix_of(&git, &start, &s.fix_b), fix_c]),
        Reason::Conflict,
    );
    assert!(
        after_b
            .remedy
            .contains(&format!("--fix {}`", short(&s.fix_b))),
        "a fix authored on the line's head lacks fix: b2, carried first in this run: {after_b:?}"
    );
    assert!(
        after_b
            .detail
            .iter()
            .any(|row| row.contains(short(&s.fix_b)) && row.contains(" as "))
    );
    assert_eq!(fix.origin_main(), s.tip, "a refused carry pushes nothing");
}

/// Main has already landed the later-base fix. A backport copies its resolved content onto
/// the line head and lands under a main-first, tree-neutral merge. NEGATIVE CONTROL: the old
/// history-only admission omits this commit from landed_since and refuses it as OffMain.
#[test]
fn an_exact_backport_under_a_neutral_merge_is_admitted_and_dry_carried() {
    for main_first in [true, false] {
        let f = Fixture::merging("contained-backport");
        let line = Line::start(VERSION, &f.shas.g, 0);
        let main_before = f.origin_main();
        // This fix from a later base conflicts; main never merged it against the line head.
        assert!(matches!(
            line::hop_tree(&f.git(), line.head(), &f.shas.fix_c).unwrap(),
            Merged::Conflict(_)
        ));
        run(&f.work, &["checkout", "-q", "--detach", line.head()]);
        let main_content = run(&f.work, &["show", "origin/main:src/c2.rs"]);
        fs::remove_file(f.work.join("src/c.rs")).unwrap();
        let backport = commit(
            &f.work,
            "backport main's c fix and rename",
            &[("src/c2.rs", &main_content)],
        );
        let merge = neutral_merge(&f, &backport, main_first);
        assert_eq!(
            parents(&f.work, &merge),
            if main_first {
                [main_before.clone(), backport.clone()]
            } else {
                [backport.clone(), main_before.clone()]
            }
        );
        assert_eq!(
            rev(&f.work, &format!("{merge}^{{tree}}")),
            rev(&f.work, &format!("{main_before}^{{tree}}"))
        );
        assert!(
            !line::landed_since(&f.git(), line.head())
                .unwrap()
                .contains(&backport)
        );

        let admitted = fix_of(&f.git(), &line, &backport);
        let before = run(&f.work, &["show-ref"]);
        let (code, last, rows) = super::release_flow::real_fix_fixture(
            &f.work,
            &line,
            std::slice::from_ref(&admitted),
            true,
        );
        assert_eq!(code, 0, "{last}\n{rows:#?}");
        assert_eq!(run(&f.work, &["show-ref"]), before);
        let mut carried = line.clone();
        carried.hops = line::carry(&f.git(), &line, &[admitted]).unwrap();
        assert_eq!(
            run(&f.work, &["show", &format!("{}:src/c2.rs", carried.head())]),
            main_content
        );
        assert!(!line::tree_has(&f.git(), carried.head(), "src/c.rs").unwrap());
        assert_eq!(
            run(&f.work, &["show", &format!("{}:src/b.rs", carried.head())]),
            B,
            "unrelated main content stays out"
        );
        line::verify_pending(&f.git(), &carried).unwrap();
        line::record(
            &f.git(),
            &LocalOrigin::new(&f),
            VERSION,
            carried.head(),
            line::MAX_RECORD_ATTEMPTS,
        )
        .unwrap();
        line::verify(&f.git(), &carried).unwrap();
    }
}

/// NEGATIVE CONTROL: a later neutral merge drops both the backport and its landing
/// from landed_since. The retained content witness still admits the full-tree backport.
#[test]
fn a_backport_cut_off_by_a_later_neutral_merge_stays_admitted() {
    let f = Fixture::merging("cut-off-backport");
    run(
        &f.origin,
        &["update-ref", "refs/heads/main", &f.shas.rename],
    );
    run(&f.work, &["fetch", "-q", "origin", "main"]);
    let line = Line::start(VERSION, &f.shas.g, 0);
    let main = f.origin_main();
    let backport = run(
        &f.work,
        &[
            "commit-tree",
            &format!("{main}^{{tree}}"),
            "-p",
            line.head(),
            "-m",
            "full-tree backport",
        ],
    )
    .trim()
    .to_owned();
    let landing = neutral_merge(&f, &backport, false);
    assert!(
        line::landed_since(&f.git(), line.head())
            .unwrap()
            .contains(&backport)
    );
    // This branch already held all landing content before learning of the backport.
    run(&f.rival, &["fetch", "-q", "origin", "main"]);
    run(&f.rival, &["reset", "-q", "--hard", &main]);
    let parallel = commit(
        &f.rival,
        "parallel main change",
        &[("src/c2.rs", "later content\n")],
    );
    let merged = run(
        &f.rival,
        &[
            "commit-tree",
            &format!("{parallel}^{{tree}}"),
            "-p",
            &parallel,
            "-p",
            &landing,
            "-m",
            "merge redundant landing",
        ],
    )
    .trim()
    .to_owned();
    run(
        &f.rival,
        &["push", "-q", "origin", &format!("{merged}:refs/heads/main")],
    );
    run(&f.work, &["fetch", "-q", "origin", "main"]);
    let landed = line::landed_since(&f.git(), line.head()).unwrap();
    assert!(!landed.contains(&landing));
    assert!(!landed.contains(&backport));
    assert!(landed.contains(&main));
    let fix = fix_of(&f.git(), &line, &backport);
    for dry in [true, false] {
        let (code, last, rows) = super::release_flow::forgotten_fix_fixture(
            &f.work,
            &line,
            std::slice::from_ref(&fix),
            dry,
        );
        assert_eq!(code, 0, "{last}\n{rows:#?}");
    }
    let carried = Line {
        hops: line::carry(&f.git(), &line, &[fix]).unwrap(),
        ..line
    };
    line::verify_pending(&f.git(), &carried).unwrap();
}

/// Historical content is a witness even after a revert or a deleted path's return.
#[test]
fn historical_witnesses_survive_reverts_and_deleted_paths_reappearing() {
    for deletion in [false, true] {
        let f = Fixture::merging("historical-witness");
        let line = Line::start(VERSION, &f.shas.g, 0);
        run(&f.work, &["checkout", "-q", "--detach", "origin/main"]);
        let original = fs::read_to_string(f.work.join("src/d.rs")).unwrap();
        if deletion {
            fs::remove_file(f.work.join("src/d.rs")).unwrap();
        } else {
            fs::write(f.work.join("src/d.rs"), "historical fix\n").unwrap();
        }
        let witness = commit(&f.work, "land content", &[]);
        let reverted = commit(&f.work, "restore path", &[("src/d.rs", &original)]);
        run(
            &f.work,
            &[
                "push",
                "-q",
                "origin",
                &format!("{reverted}:refs/heads/main"),
            ],
        );
        run(&f.work, &["fetch", "-q", "origin", "main"]);
        assert!(
            line::landed_since(&f.git(), line.head())
                .unwrap()
                .contains(&witness)
        );
        run(&f.work, &["checkout", "-q", "--detach", line.head()]);
        if deletion {
            fs::remove_file(f.work.join("src/d.rs")).unwrap();
        } else {
            fs::write(f.work.join("src/d.rs"), "historical fix\n").unwrap();
        }
        let backport = commit(&f.work, "historical backport", &[]);
        neutral_merge(&f, &backport, true);
        let fix = fix_of(&f.git(), &line, &backport);
        let carried = Line {
            hops: line::carry(&f.git(), &line, &[fix]).unwrap(),
            ..line.clone()
        };
        line::verify_pending(&f.git(), &carried).unwrap();
        // NEGATIVE CONTROL: history cannot authorize content never present in
        // any witness, even when the other paths and the parent match.
        run(&f.work, &["checkout", "-q", "--detach", line.head()]);
        let invented = commit(
            &f.work,
            "invented backport",
            &[("src/d.rs", "never landed\n")],
        );
        neutral_merge(&f, &invented, true);
        stopped(line::admit(&f.git(), &line, &invented), Reason::OffMain);
    }
}

/// A side merge's matching tree is no authority when main reaches it only through
/// a neutral merge. NEGATIVE CONTROL: scanning all ancestors admits this forged backport.
#[test]
fn an_unlanded_side_merge_cannot_witness_a_backport() {
    let f = Fixture::merging("side-witness");
    let line = Line::start(VERSION, &f.shas.g, 0);
    run(&f.work, &["checkout", "-q", "--detach", line.head()]);
    let backport = commit(
        &f.work,
        "unlanded content",
        &[("src/d.rs", "never landed\n")],
    );
    let main = f.origin_main();
    let side = run(
        &f.work,
        &[
            "commit-tree",
            &format!("{backport}^{{tree}}"),
            "-p",
            &main,
            "-p",
            &backport,
            "-m",
            "side merge holds unlanded content",
        ],
    )
    .trim()
    .to_owned();
    neutral_merge(&f, &side, true);
    let landed = line::landed_since(&f.git(), line.head()).unwrap();
    assert!(!landed.contains(&side));
    assert!(!landed.contains(&backport));
    stopped(line::admit(&f.git(), &line, &backport), Reason::OffMain);
    let forged = Line {
        hops: vec![hand_hop(&f, line.head(), &backport)],
        ..line
    };
    stopped(line::verify_pending(&f.git(), &forged), Reason::OffMain);
}

/// Retaining a backport in history is insufficient: every changed path must match, including
/// a deletion or mode. A hand-built hop cannot bypass admission's content obligation.
#[test]
fn a_neutral_backport_main_does_not_hold_is_refused_by_admit_and_verify() {
    for main_first in [true, false] {
        for mismatch in ["content", "deletion", "mode", "parent"] {
            let f = Fixture::merging(&format!("backport-{mismatch}"));
            let line = Line::start(VERSION, &f.shas.g, 0);
            run(&f.work, &["checkout", "-q", "--detach", line.head()]);
            // One matching changed path must not excuse a second mismatching one.
            let b_main = run(&f.work, &["show", "origin/main:src/b.rs"]);
            match mismatch {
                "content" => {
                    fs::write(f.work.join("src/d.rs"), "not main\n").unwrap();
                }
                "deletion" => {
                    fs::remove_file(f.work.join("src/d.rs")).unwrap();
                }
                "mode" => {
                    run(&f.work, &["config", "core.filemode", "false"]);
                    run(&f.work, &["update-index", "--chmod=+x", "src/d.rs"]);
                }
                "parent" => {
                    commit(
                        &f.work,
                        "not the line head",
                        &[("context", "outside the line\n")],
                    );
                }
                _ => unreachable!(),
            }
            let backport = commit(
                &f.work,
                "backport with unlanded change",
                &[("src/b.rs", &b_main)],
            );
            neutral_merge(&f, &backport, main_first);
            let stop = stopped(line::admit(&f.git(), &line, &backport), Reason::OffMain);
            let (code, last, rows) = super::release_flow::real_fix_fixture(
                &f.work,
                &line,
                &[Fix {
                    commit: backport.clone(),
                    subject: "backport".into(),
                    paths: vec![],
                }],
                true,
            );
            assert_eq!(code, 1, "{last}\n{rows:#?}");
            assert!(last.contains(&stop.why), "{last}");
            let forged = Line {
                hops: vec![hand_hop(&f, line.head(), &backport)],
                ..line
            };
            stopped(line::verify_pending(&f.git(), &forged), Reason::OffMain);
        }
    }
}

/// Land a fixture-only history record, in the parent order that today's code fails to follow.
fn neutral_merge(f: &Fixture, backport: &str, main_first: bool) -> String {
    let tip = f.origin_main();
    let (first, second) = if main_first {
        (tip.as_str(), backport)
    } else {
        (backport, tip.as_str())
    };
    let merge = run(
        &f.work,
        &[
            "commit-tree",
            &format!("{tip}^{{tree}}"),
            "-p",
            first,
            "-p",
            second,
            "-m",
            "backport already contained in main",
        ],
    )
    .trim()
    .to_owned();
    run(
        &f.work,
        &["push", "-q", "origin", &format!("{merge}:refs/heads/main")],
    );
    run(&f.work, &["fetch", "-q", "origin", "main"]);
    merge
}

#[test]
fn a_fix_whose_change_the_head_has_carries_nothing_and_is_refused() {
    let fix = Fixture::merging("empty");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 0);
    let fix_a = fix_of(&git, &start, &s.fix_a);
    let hops = line::carry(&git, &start, std::slice::from_ref(&fix_a)).unwrap();
    let has_it = Line::start(VERSION, &hops[0].commit, 0);
    stopped(line::carry(&git, &has_it, &[fix_a]), Reason::Empty);
}

#[test]
fn carry_refuses_a_hand_built_fix_that_moves_a_frozen_path_or_the_version() {
    let fix = Fixture::merging("carry-guard");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 0);
    let by_hand = |commit: &str| Fix {
        commit: commit.to_string(),
        subject: "by hand".to_string(),
        paths: Vec::new(),
    };
    for (commit, reason) in [
        (s.frozen.as_str(), Reason::Frozen),
        (s.bump.as_str(), Reason::Version),
        (short(&s.fix_b), Reason::Spelling),
    ] {
        stopped(line::carry(&git, &start, &[by_hand(commit)]), reason);
    }
}

#[test]
fn a_merge_is_refused_listing_each_sides_commits() {
    let fix = Fixture::new("merge");
    let (git, s) = (fix.git(), &fix.shas);
    let stop = stopped(
        line::admit(&git, &Line::start(VERSION, &s.g, 0), &s.merge),
        Reason::Merge,
    );
    let at = |line: &str| {
        stop.detail
            .iter()
            .position(|d| d.contains(line))
            .unwrap_or_else(|| panic!("no {line:?} in {:#?}", stop.detail))
    };
    let side_1 = at(&format!("its parent 1 ({})", short(&s.fix_a)));
    let main_moved = at(&format!("  {} feat: main moves c3", s.n1));
    let side_2 = at(&format!("its parent 2 ({})", short(&s.n1)));
    let own = at(&format!("  {} fix: a1", s.fix_a));
    assert!(
        side_1 < main_moved && main_moved < side_2 && side_2 < own,
        "each side lists the commits that parent lacks: {:#?}",
        stop.detail
    );
}

#[test]
fn a_commit_off_main_a_root_or_no_commit_is_refused() {
    let fix = Fixture::new("off-main");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 0);
    stopped(line::admit(&git, &start, &s.off), Reason::OffMain);
    stopped(line::admit(&git, &start, &s.seed), Reason::Root);
    for rev in ["no-such-branch", "-p", ""] {
        stopped(line::admit(&git, &start, rev), Reason::NoCommit);
    }
}

#[test]
fn a_frozen_path_or_a_version_bump_is_refused() {
    let fix = Fixture::new("frozen");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 0);
    for (commit, reason) in [
        (&s.frozen, Reason::Frozen),
        (&s.ledger, Reason::Frozen),
        (&s.bump, Reason::Version),
    ] {
        stopped(line::admit(&git, &start, commit), reason);
    }
}

#[test]
fn a_carried_fix_resumes_and_one_the_line_holds_is_refused() {
    let fix = Fixture::merging("carried");
    let (git, s) = (fix.git(), &fix.shas);
    let mut l = Line::start(VERSION, &s.g, 0);
    assert!(!l.carried(&s.fix_b));
    l.hops = line::carry(&git, &l, &[fix_of(&git, &l, &s.fix_b)]).unwrap();
    assert!(l.carried(&s.fix_b));
    assert_eq!(line::admit(&git, &l, &s.fix_b).unwrap(), Admitted::Carried);
    stopped(line::admit(&git, &l, &s.g), Reason::Held);
    line::record(
        &git,
        &LocalOrigin::new(&fix),
        VERSION,
        l.head(),
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    stopped(line::admit(&git, &l, l.head()), Reason::Held);
    stopped(
        line::admit(&git, &Line::start(VERSION, &s.d_revert, 0), &s.d_fix),
        Reason::Held,
    );
}

#[test]
fn fixes_carry_in_ancestry_order_whatever_order_they_are_named_in() {
    let fix = Fixture::merging("order");
    let (git, s) = (fix.git(), &fix.shas);
    let mut l = Line::start(VERSION, &s.g, 0);
    let (a, b) = (fix_of(&git, &l, &s.fix_a), fix_of(&git, &l, &s.fix_b));
    let commits = |fixes: Vec<Fix>| -> Vec<String> {
        line::order(&git, fixes)
            .unwrap()
            .into_iter()
            .map(|f| f.commit)
            .collect()
    };
    let ancestry = vec![s.fix_a.clone(), s.fix_b.clone()];
    assert_eq!(commits(vec![b.clone(), a.clone()]), ancestry);
    assert_eq!(commits(vec![a.clone(), b.clone()]), ancestry);
    assert_eq!(commits(vec![b.clone(), a.clone(), b.clone()]), ancestry);

    let hops = line::carry(&git, &l, &line::order(&git, vec![b, a]).unwrap()).unwrap();
    assert_eq!(
        hops.iter().map(|h| h.carries.as_str()).collect::<Vec<_>>(),
        [s.fix_a.as_str(), s.fix_b.as_str()]
    );
    assert_eq!(parents(&fix.work, &hops[0].commit), [s.g.as_str()]);
    assert_eq!(
        parents(&fix.work, &hops[1].commit),
        [hops[0].commit.as_str()]
    );
    assert_eq!(
        run(&fix.work, &["log", "-1", "--format=%s", &hops[1].commit]).trim(),
        format!("release line v0.26.0 hop 2: carries {} (fix: b2)", s.fix_b)
    );
    l.hops = hops;
    line::record(
        &git,
        &LocalOrigin::new(&fix),
        VERSION,
        l.head(),
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    let summary = line::verify(&git, &l).unwrap();
    assert!(summary.contains("2 hop(s)"), "{summary}");
}

#[test]
fn unrelated_fixes_go_by_committer_time_then_sha_whatever_order_they_are_named_in() {
    let fix = Fixture::new("order-ties");
    let git = fix.git();
    let t = 1_790_000_000;
    let (older, newer) = (0..)
        .map(|i| {
            (
                sibling(&fix, &format!("older {i}"), t),
                sibling(&fix, &format!("newer {i}"), t + 1),
            )
        })
        .find(|(older, newer)| older.commit > newer.commit)
        .unwrap();
    let (a, b) = (
        sibling(&fix, "same a", t + 2),
        sibling(&fix, "same b", t + 2),
    );
    let (low, high) = if a.commit < b.commit { (a, b) } else { (b, a) };
    let commits = |fixes: Vec<Fix>| -> Vec<String> {
        line::order(&git, fixes)
            .unwrap()
            .into_iter()
            .map(|f| f.commit)
            .collect()
    };
    for named in [
        vec![newer.clone(), older.clone()],
        vec![older.clone(), newer.clone()],
    ] {
        assert_eq!(
            commits(named),
            [older.commit.clone(), newer.commit.clone()],
            "the older commit first, though its sha is the higher"
        );
    }
    for named in [
        vec![high.clone(), low.clone()],
        vec![low.clone(), high.clone()],
    ] {
        assert_eq!(
            commits(named),
            [low.commit.clone(), high.commit.clone()],
            "at one time, the lower sha first"
        );
    }
}

#[test]
fn a_hops_tree_is_the_commits_alone_whatever_this_clone_holds() {
    let fix = Fixture::merging("steered");
    let (git, s) = (fix.git(), &fix.shas);
    let conflict = Merged::Conflict(vec!["src/c.rs".to_string()]);
    assert_eq!(line::hop_tree(&git, &s.g, &s.fix_c).unwrap(), conflict);

    // The checkout's .gitattributes and an attributes file of this clone's config each make
    // src/c.rs a union merge, which would take both sides of the conflict.
    let union = "src/c.rs merge=union\n";
    fs::write(fix.work.join(".gitattributes"), union).unwrap();
    let attributes = fix.work.join(".git/aterm-attributes");
    fs::write(&attributes, union).unwrap();
    run(
        &fix.work,
        &[
            "config",
            "core.attributesFile",
            attributes.to_str().unwrap(),
        ],
    );
    assert_eq!(
        line::hop_tree(&git, &s.g, &s.fix_c).unwrap(),
        conflict,
        "neither the checkout nor a global attributes file steers the merge"
    );

    // The clone's info/attributes and merge.default, which no flag reaches, stop it.
    let info = fix.work.join(".git/info/attributes");
    fs::create_dir_all(info.parent().unwrap()).unwrap();
    fs::write(&info, "src/a.rs merge=union\n").unwrap();
    assert_eq!(
        line::hop_tree(&git, &s.g, &s.fix_c).unwrap(),
        conflict,
        "an attribute on a path the fix does not move picks no driver it meets"
    );
    fs::write(&info, union).unwrap();
    stopped(line::hop_tree(&git, &s.g, &s.fix_c), Reason::Steered);
    stopped(
        line::carry(
            &git,
            &Line::start(VERSION, &s.g, 0),
            &[fix_of(&git, &Line::start(VERSION, &s.g, 0), &s.fix_c)],
        ),
        Reason::Steered,
    );
    fs::remove_file(&info).unwrap();
    run(&fix.work, &["config", "merge.default", "union"]);
    stopped(line::hop_tree(&git, &s.g, &s.fix_c), Reason::Steered);
    run(&fix.work, &["config", "--unset", "merge.default"]);
    assert_eq!(line::hop_tree(&git, &s.g, &s.fix_c).unwrap(), conflict);
}

#[test]
fn a_rename_carries_as_a_deletion_and_an_addition() {
    let fix = Fixture::merging("rename");
    let (git, s) = (fix.git(), &fix.shas);
    assert_eq!(
        line::hop_tree(&git, &s.g, &s.rename).unwrap(),
        Merged::Conflict(vec!["src/c.rs".to_string()]),
        "G changed src/c.rs's lines after it was renamed on main: a modify/delete conflict, \
         never a merge into src/c2.rs that rename detection would make"
    );
}

#[test]
fn the_record_merge_rebuilds_on_a_rival_push_and_keeps_mains_tree() {
    let fix = Fixture::merging("record-race");
    let s = fix.shas.clone();
    let mut l = Line::start(VERSION, &s.g, 0);
    l.hops = line::carry(&fix.git(), &l, &[fix_of(&fix.git(), &l, &s.fix_b)]).unwrap();
    let rival_tip = RefCell::new(String::new());
    let git = fix.git();
    let origin = LocalOrigin::new(&fix);
    *origin.before_write.borrow_mut() = Box::new(|n| {
        if n == 1 {
            *rival_tip.borrow_mut() = fix.rival_moves_main("fn a6() {}");
        }
    });
    let recorded =
        line::record(&git, &origin, VERSION, l.head(), line::MAX_RECORD_ATTEMPTS).unwrap();
    let Recorded::Pushed { merge, attempt: 2 } = recorded else {
        panic!("the first push lost to the rival, the rebuild landed: {recorded:?}");
    };
    let rival_tip = rival_tip.borrow().clone();
    assert_eq!(fix.origin_main(), merge);
    assert_eq!(
        parents(&fix.origin, &merge),
        [rival_tip.clone(), l.head().to_string()]
    );
    assert_eq!(
        rev(&fix.origin, &format!("{merge}^{{tree}}")),
        rev(&fix.origin, &format!("{rival_tip}^{{tree}}")),
        "the rebuilt record merge keeps the rival's main, byte for byte"
    );
    assert!(is_ancestor(&fix.origin, l.head(), "main"));
    assert_eq!(
        line::record(&git, &origin, VERSION, l.head(), line::MAX_RECORD_ATTEMPTS).unwrap(),
        Recorded::Already { tip: merge },
        "a re-run finds the head on main and pushes nothing"
    );
    assert_eq!(origin.writes.get(), 2);
}

#[test]
fn the_record_stops_could_not_run_when_every_push_loses() {
    let fix = Fixture::merging("record-cap");
    let s = fix.shas.clone();
    let mut l = Line::start(VERSION, &s.g, 0);
    l.hops = line::carry(&fix.git(), &l, &[fix_of(&fix.git(), &l, &s.fix_b)]).unwrap();
    let git = fix.git();
    let origin = LocalOrigin::new(&fix);
    *origin.before_write.borrow_mut() = Box::new(|n| {
        fix.rival_moves_main(&format!("fn rival{n}() {{}}"));
    });
    stopped(
        line::record(&git, &origin, VERSION, l.head(), 3),
        Reason::Raced,
    );
    assert_eq!(origin.writes.get(), 3);
    assert!(!is_ancestor(&fix.origin, l.head(), "main"));
}

#[test]
fn an_unreachable_origin_or_a_push_that_lands_nothing_could_not_run() {
    let fix = Fixture::merging("offline");
    let s = &fix.shas;
    let mut l = Line::start(VERSION, &s.g, 0);
    l.hops = line::carry(&fix.git(), &l, &[fix_of(&fix.git(), &l, &s.fix_b)]).unwrap();
    let mut no_effect = LocalOrigin::new(&fix);
    no_effect.no_effect = true;
    stopped(
        line::record(
            &fix.git(),
            &no_effect,
            VERSION,
            l.head(),
            line::MAX_RECORD_ATTEMPTS,
        ),
        Reason::Unlanded,
    );
    let gone = fix.work.join("no-such-origin.git");
    run(
        &fix.work,
        &["remote", "set-url", "origin", gone.to_str().unwrap()],
    );
    stopped(
        line::record(
            &fix.git(),
            &LocalOrigin::new(&fix),
            VERSION,
            l.head(),
            line::MAX_RECORD_ATTEMPTS,
        ),
        Reason::Unreachable,
    );
}

#[test]
fn an_unknown_record_write_stops_without_retry_or_a_race_word_guess() {
    for after_effect in [false, true] {
        let fix = Fixture::merging("record-unknown");
        let git = fix.git();
        let mut line = Line::start(VERSION, &fix.shas.g, 0);
        line.hops = line::carry(&git, &line, &[fix_of(&git, &line, &fix.shas.fix_b)]).unwrap();
        let mut origin = LocalOrigin::new(&fix);
        origin.failure = Some(
            "native outcome unknown: [rejected] non-fast-forward; remote may have committed".into(),
        );
        origin.fail_after_effect = after_effect;
        stopped(
            line::record(&LocalOnly(&git), &origin, VERSION, line.head(), 5),
            Reason::Unreachable,
        );
        assert_eq!(origin.writes.get(), 1, "unknown never becomes a retry");
        assert_eq!(
            origin.refreshes.get(),
            1,
            "unknown is propagated before any post-read"
        );
        assert_eq!(
            is_ancestor(&fix.origin, line.head(), "main"),
            after_effect,
            "both possible outcomes remain unknown to the caller"
        );
    }
}

#[test]
fn verify_refuses_each_way_a_line_can_be_wrong_and_passes_the_honest_one() {
    let fix = Fixture::merging("verify");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 0);
    let honest = line::carry(&git, &start, &[fix_of(&git, &start, &s.fix_b)]).unwrap();
    let real = honest[0].clone();
    let hop_a = line::carry(&git, &start, &[fix_of(&git, &start, &s.fix_a)]).unwrap()[0].clone();
    line::record(
        &git,
        &LocalOrigin::new(&fix),
        VERSION,
        &hop_a.commit,
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    let on = |base: &str, hops: Vec<Hop>| Line {
        hops,
        ..Line::start(VERSION, base, 0)
    };

    let planted = tampered(&fix, &real.commit, &s.g, "src/extra.rs");
    let reparented = run(
        &fix.work,
        &[
            "commit-tree",
            &format!("{}^{{tree}}", real.commit),
            "-p",
            &s.n1,
            "-m",
            "reparented",
        ],
    )
    .trim()
    .to_string();
    let duplicate = hand_hop(&fix, &real.commit, &s.fix_b);
    let cases = [
        (
            on(
                &s.g,
                vec![Hop {
                    commit: planted,
                    paths: vec!["src/b.rs".to_string(), "src/extra.rs".to_string()],
                    ..real.clone()
                }],
            ),
            Reason::Tree,
        ),
        (
            on(
                &s.g,
                vec![Hop {
                    commit: reparented,
                    ..real.clone()
                }],
            ),
            Reason::Parent,
        ),
        (
            on(&s.g, vec![hand_hop(&fix, &s.g, &s.merge)]),
            Reason::Merge,
        ),
        (
            on(&s.g, vec![hand_hop(&fix, &s.g, &s.off)]),
            Reason::OffMain,
        ),
        (
            on(&s.d_revert, vec![hand_hop(&fix, &s.d_revert, &s.d_fix)]),
            Reason::Held,
        ),
        (
            on(&hop_a.commit, vec![hand_hop(&fix, &hop_a.commit, &s.fix_a)]),
            Reason::Empty,
        ),
        (
            on(&s.g, vec![hand_hop(&fix, &s.g, &s.frozen)]),
            Reason::Frozen,
        ),
        (
            on(&s.g, vec![hand_hop(&fix, &s.g, &s.bump)]),
            Reason::Version,
        ),
        (
            on(
                &s.g,
                vec![Hop {
                    paths: Vec::new(),
                    ..real.clone()
                }],
            ),
            Reason::Paths,
        ),
        (on(&s.g, vec![real.clone(), duplicate]), Reason::Duplicate),
        (
            on(
                &s.g,
                vec![Hop {
                    carries: short(&s.fix_b).to_string(),
                    ..real.clone()
                }],
            ),
            Reason::Spelling,
        ),
    ];
    for (l, reason) in cases {
        line::record(
            &git,
            &LocalOrigin::new(&fix),
            VERSION,
            l.head(),
            line::MAX_RECORD_ATTEMPTS,
        )
        .unwrap();
        stopped(line::verify(&git, &l), reason);
    }
    let honest = on(&s.g, vec![real.clone()]);
    line::record(
        &git,
        &LocalOrigin::new(&fix),
        VERSION,
        honest.head(),
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    line::verify(&git, &honest).unwrap();

    let lacked = "0123456789abcdef0123456789abcdef01234567";
    for (l, reason) in [
        (on(&s.off, Vec::new()), Reason::BaseOffMain),
        (
            Line {
                version: "0.27.0".to_string(),
                ..start.clone()
            },
            Reason::BaseVersion,
        ),
        (on(short(&s.g), Vec::new()), Reason::Spelling),
        (
            on(
                &s.g,
                vec![Hop {
                    commit: rev(&fix.work, &format!("{}^{{tree}}", real.commit)),
                    ..real.clone()
                }],
            ),
            Reason::Spelling,
        ),
        (on(lacked, Vec::new()), Reason::Unread),
        (
            on(
                &s.g,
                vec![Hop {
                    carries: lacked.to_string(),
                    ..real.clone()
                }],
            ),
            Reason::Unread,
        ),
    ] {
        stopped(line::verify(&git, &l), reason);
    }
}

/// Record-only history is not itself landing evidence. A rival hop can still be an exact
/// backport of landed content, while an invented change has neither proof. NEGATIVE CONTROL:
/// treating every ancestor as landed admits the invented change and verifies its forged hop.
#[test]
fn record_only_history_requires_landed_content_evidence() {
    let fix = Fixture::merging("record-only");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 0);
    let mut rival = start.clone();
    rival.hops = line::carry(&git, &rival, &[fix_of(&git, &rival, &s.fix_b)]).unwrap();
    line::record(
        &git,
        &LocalOrigin::new(&fix),
        VERSION,
        rival.head(),
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    let rival_hop = rival.head().to_string();
    assert!(
        is_ancestor(&fix.work, &rival_hop, "origin/main"),
        "the record puts the rival's hop in main's history"
    );

    let planted = tampered(&fix, &s.g, &s.g, "src/planted.rs");
    let tip = fix.origin_main();
    let keeps = run(
        &fix.work,
        &[
            "commit-tree",
            &format!("{tip}^{{tree}}"),
            "-p",
            &tip,
            "-p",
            &planted,
            "-m",
            "keeps main's tree",
        ],
    )
    .trim()
    .to_string();
    run(
        &fix.work,
        &["push", "-q", "origin", &format!("{keeps}:refs/heads/main")],
    );
    run(&fix.work, &["fetch", "-q", "origin", "main"]);
    assert!(is_ancestor(&fix.work, &planted, "origin/main"));

    let landed = line::landed_since(&git, &s.g).unwrap();
    for main in [&s.fix_a, &s.n1, &s.merge, &s.fix_b, &s.tip] {
        assert!(landed.contains(main), "{main} landed: {landed:?}");
    }
    for unlanded in [&rival_hop, &planted] {
        assert!(!landed.contains(unlanded), "{unlanded}: {landed:?}");
    }
    assert_eq!(fix_of(&git, &start, &rival_hop).commit, rival_hop);
    let stop = stopped(line::admit(&git, &start, &planted), Reason::OffMain);
    assert!(stop.why.contains("never landed in main's tree"), "{stop:?}");
    let forged = Line {
        hops: vec![hand_hop(&fix, &s.g, &planted)],
        ..start.clone()
    };
    line::record(
        &git,
        &LocalOrigin::new(&fix),
        VERSION,
        forged.head(),
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    let stop = stopped(line::verify(&git, &forged), Reason::OffMain);
    assert!(
        stop.why.starts_with("hop 1: ") && stop.remedy.contains("--forget"),
        "{stop:?}"
    );
    assert_eq!(fix_of(&git, &start, &s.fix_b).commit, s.fix_b);
    line::verify(&git, &rival).unwrap();
}

/// `tree_has` reads a file of a commit's tree (never a directory or another path's prefix), and
/// could not run for a commit the clone lacks.
#[test]
fn tree_has_reads_a_file_its_absence_or_could_not_run() {
    let fix = Fixture::new("tree-has");
    let (git, s) = (fix.git(), &fix.shas);
    assert!(line::tree_has(&git, &s.g, "src/d.rs").unwrap());
    assert!(
        !line::tree_has(&git, &s.seed, "src/d.rs").unwrap(),
        "G adds it"
    );
    for not_a_file in ["src", "src/d", "d.rs"] {
        assert!(
            !line::tree_has(&git, &s.g, not_a_file).unwrap(),
            "{not_a_file}"
        );
    }
    stopped(
        line::tree_has(&git, "0123456789abcdef0123456789abcdef01234567", "src/d.rs"),
        Reason::Unread,
    );
}

#[test]
fn a_rebuilt_hop_is_the_same_commit() {
    let fix = Fixture::merging("rebuild");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 0);
    let first = line::carry(&git, &start, &[fix_of(&git, &start, &s.fix_b)]).unwrap();
    run(&fix.work, &["config", "user.name", "another releaser"]);
    run(
        &fix.work,
        &["config", "user.email", "another@aterm.invalid"],
    );
    let again = line::carry(&git, &start, &[fix_of(&git, &start, &s.fix_b)]).unwrap();
    assert_eq!(
        first, again,
        "a re-run under another identity, later, makes the same hop commit"
    );
}

#[test]
fn this_git_runs_a_line() {
    let fix = Fixture::new("git-version");
    let words = line::git_ready(&fix.git())
        .unwrap_or_else(|stop| aterm_spec::host::could_not_run(stop.why));
    assert!(
        line::parse_git_version(&words).is_some_and(|v| v >= line::MIN_GIT),
        "{words}"
    );
}

/// A git that refuses a hop's options (2.39) and one that takes them and still detects renames
/// (2.43 to 2.49) are each COULD NOT RUN, read before any merge. NEGATIVE CONTROL: drop
/// `hop_tree`'s version read, and the 2.49 git's merge gives a tree.
#[test]
fn an_older_git_could_not_run_a_line() {
    assert_eq!(
        line::parse_git_version("git version 2.39.5 (Apple Git-154)"),
        Some((2, 39))
    );
    assert_eq!(
        line::parse_git_version("git version 2.45.1.windows.1\n"),
        Some((2, 45))
    );

    struct Old;
    impl GitRunner for Old {
        fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
            let (status, stdout) = match args.first() {
                Some(&"version") => (0, "git version 2.39.5 (Apple Git-154)\n"),
                _ => (129, ""),
            };
            Ok(RunOut {
                status,
                stdout: stdout.as_bytes().to_vec(),
                stderr: b"error: unknown option `attr-source=4b825dc642cb'".to_vec(),
            })
        }
    }
    stopped(line::git_ready(&Old), Reason::OldGit);
    let stop = stopped(line::hop_tree(&Old, "head", "fix"), Reason::OldGit);
    assert!(
        stop.why.contains("2.39.5"),
        "it names the git found: {stop:?}"
    );

    /// Every call answers as a real 2.49 does for a clean merge: no `merge.default`, no path
    /// moved that an attribute could steer, and the merged tree.
    struct Detecting;
    impl GitRunner for Detecting {
        fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
            let (status, stdout): (i32, &[u8]) = if args.first() == Some(&"version") {
                (0, b"git version 2.49.0\n")
            } else if args.contains(&"merge.default") {
                (1, b"")
            } else if args.contains(&"diff-tree") {
                (0, b"")
            } else {
                (0, b"4b825dc642cb6eb9a060e54bf8d69288fbee4904\0")
            };
            Ok(RunOut {
                status,
                stdout: stdout.to_vec(),
                stderr: Vec::new(),
            })
        }
    }
    stopped(line::git_ready(&Detecting), Reason::OldGit);
    let stop = stopped(line::hop_tree(&Detecting, "head", "fix"), Reason::OldGit);
    assert!(
        stop.why.contains("2.49.0"),
        "it names the git found: {stop:?}"
    );
}

#[test]
fn a_stop_becomes_a_refusal_that_could_not_run_exactly_when_its_reason_judged_nothing() {
    for (reason, could_not_run) in [
        (Reason::Conflict, false),
        (Reason::Held, false),
        (Reason::Unrecorded, false),
        (Reason::Unread, true),
        (Reason::Steered, true),
        (Reason::Unreachable, true),
    ] {
        let stop = Stop {
            reason,
            why: "why".to_string(),
            remedy: "remedy".to_string(),
            detail: vec!["detail".to_string()],
        };
        assert_eq!(stop.could_not_run(), could_not_run, "{reason:?}");
        let refusal = crate::release::Refusal::from(stop);
        assert_eq!(
            (
                refusal.why.as_str(),
                refusal.remedy.as_str(),
                refusal.detail.as_slice(),
                refusal.could_not_run
            ),
            (
                "why",
                "remedy",
                ["detail".to_string()].as_slice(),
                could_not_run
            ),
            "{reason:?}"
        );
    }
}

/// The release journal keeps its line through its own save and load; a journal written before
/// lines has none, and reads as a line at its commit with no hops.
#[test]
fn a_line_round_trips_through_the_journals_toml() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(crate::release::JOURNAL_FILE);
    let journal = crate::release::Journal {
        version: VERSION.to_string(),
        commit: "5".repeat(40),
        started: 1_790_000_100,
        line: Some(Line {
            hops: vec![
                Hop {
                    commit: "3".repeat(40),
                    carries: "4".repeat(40),
                    why: "fix: b2".to_string(),
                    paths: vec!["src/b.rs".to_string()],
                },
                Hop {
                    commit: "5".repeat(40),
                    carries: "6".repeat(40),
                    why: "fix: \"quoted\" = x".to_string(),
                    paths: Vec::new(),
                },
            ],
            ..Line::start(VERSION, &"2".repeat(40), 1_790_000_000)
        }),
        ..Default::default()
    };
    journal.save(&path).unwrap();
    let read = crate::release::Journal::load(&path).unwrap().unwrap();
    assert_eq!(read, journal, "{}", fs::read_to_string(&path).unwrap());
    assert_eq!(read.line().head(), read.commit);

    let before_lines = format!(
        "version = \"0.26.0\"\ncommit = \"{c}\"\nprover = \"/p\"\ntrustc = \"t\"\nstarted = 7\n",
        c = "2".repeat(40)
    );
    fs::write(&path, before_lines).unwrap();
    let old = crate::release::Journal::load(&path).unwrap().unwrap();
    assert_eq!(old.line, None);
    assert_eq!(old.line(), Line::start(VERSION, &"2".repeat(40), 7));

    let bare = format!(
        "version = \"0.26.0\"\ncommit = \"{c}\"\nprover = \"/p\"\ntrustc = \"t\"\n\n[line]\n\
         version = \"0.26.0\"\nbase = \"{c}\"\n",
        c = "2".repeat(40)
    );
    fs::write(&path, bare).unwrap();
    let read = crate::release::Journal::load(&path).unwrap().unwrap();
    assert_eq!(read.line, Some(Line::start(VERSION, &"2".repeat(40), 0)));
}

/// The cut signs only the head of a line that verifies: the journal's line must end at the
/// published commit and, with hops, hold `line::verify`, before the verified bundle's other
/// gates are read (a sound line, and a journal from before lines, go on to the next gate, which
/// refuses here because nothing was judged). NEGATIVE CONTROLS: drop the head test, and a line
/// that ends elsewhere reaches the next gate; drop the verify, and a hop whose carried commit
/// was swapped does too.
#[test]
fn the_cut_signs_only_the_head_of_a_line_that_verifies() {
    let fix = Fixture::merging("cut");
    let (git, s) = (fix.git(), &fix.shas);
    let start = Line::start(VERSION, &s.g, 1_790_000_000);
    let admitted = fix_of(&git, &start, &s.fix_b);
    let mut carried = start.clone();
    carried.hops = line::carry(&git, &start, &[admitted]).unwrap();
    line::record(
        &git,
        &LocalOrigin::new(&fix),
        VERSION,
        carried.head(),
        line::MAX_RECORD_ATTEMPTS,
    )
    .unwrap();
    let head = carried.head().to_string();
    let journal = fix
        .work
        .join(".git")
        .join(crate::release::RELEASE_DIR)
        .join(crate::release::JOURNAL_FILE);
    let cut = |line: Option<Line>| {
        crate::release::Journal {
            version: VERSION.to_string(),
            commit: head.clone(),
            line,
            ..Default::default()
        }
        .save(&journal)
        .unwrap();
        crate::publish::verified_release_bundle_with_inputs(
            &git,
            &fix.work,
            &head,
            VERSION,
            "/stage2/bin trustc 0",
            || Ok("inputs".into()),
        )
        .err()
        .expect("nothing was judged or built here")
        .to_string()
    };
    let next = "has not judged R's commit-only gates";
    let sound = cut(Some(carried.clone()));
    assert!(sound.contains(next), "{sound}");
    let before_lines = cut(None);
    assert!(before_lines.contains(next), "{before_lines}");

    let elsewhere = cut(Some(start));
    assert!(
        elsewhere.contains(&format!("its line's head {}", short(&s.g))),
        "{elsewhere}"
    );
    let mut swapped = carried;
    swapped.hops[0].carries.clone_from(&s.d_fix);
    let lied = cut(Some(swapped));
    assert!(
        lied.contains("line::verify") && lied.contains("hop 1's tree"),
        "{lied}"
    );
}
