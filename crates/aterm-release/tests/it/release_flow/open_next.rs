// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship release --open-next` ([`crate::release`]'s `Run::open_next`) against the fake world
//! ([`super::World`]): the next version committed on main's tip with no line started.

use super::{BUMP, JOURNAL_FILE, NEXT, OLD, RUNNING_FILE, SELECTION_FILE, TIP, World, bump};
use crate::release::{Refusal, ReleaseOptions};

fn open_next() -> ReleaseOptions {
    ReleaseOptions {
        open_next: true,
        ..ReleaseOptions::default()
    }
}

fn at(calls: &[String], call: &str) -> usize {
    calls
        .iter()
        .position(|c| c == call)
        .unwrap_or_else(|| panic!("{call} was not called: {calls:?}"))
}

/// THE NEXT VERSION OPENS WITH NO LINE. Main declares 0.5.0, which is cut (its tag is on
/// origin) and whose handoff fixtures the tip holds. `--open-next` pushes the bump to 0.6.0 on
/// that tip and stops: no journal, no candidate, no fleet claim, no step of a release. It asks
/// the fixtures and the bump's commit gates of the refreshed tip before the engine is touched,
/// and the engine before the bump.
#[test]
fn open_next_writes_the_bump_from_the_tips_cutter_and_starts_no_line() {
    let mut w = World::new();
    let (code, last) = w.run(open_next());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        last,
        format!(
            "OPENED aterm 0.6.0 at {BUMP} on main (0.5.0 -> 0.6.0) — nothing public; no line \
             started. Start its line: {} --commit {BUMP}",
            crate::ledger::RELEASE_COMMAND
        )
    );
    assert_eq!(w.tip(), BUMP, "the bump is main's tip");
    assert_eq!(w.versions[BUMP], "0.6.0");
    assert_eq!(w.count("bump"), 1, "{:?}", w.calls);
    let order = [
        "fixtures-at bbbb v0.5.0",
        "commit-gates-dry",
        "sync",
        "bump",
    ]
    .map(|call| at(&w.calls, call));
    assert!(order.is_sorted(), "{:?}", w.calls);
    assert_eq!(
        w.fetches, 1,
        "origin is refreshed once, by the tip's cutter"
    );
    assert!(w.journal().is_none(), "no release journal");
    assert!(
        !w.release_dir().join(SELECTION_FILE).exists(),
        "no candidate"
    );
    for line_work in [
        "fleet-select",
        "fleet-existing",
        "fleet-boundary",
        "audit",
        "seal-inputs",
        "validation-tools",
        "reserve",
    ] {
        assert_eq!(w.count(line_work), 0, "{line_work}: {:?}", w.calls);
    }
    assert!(
        !w.release_dir().join(RUNNING_FILE).exists(),
        "the run claim goes with the run"
    );
    assert!(
        w.out
            .iter()
            .any(|line| line.contains(r#""event":"open-next""#)
                && line.contains(&format!(r#""commit":"{BUMP}""#))),
        "{:#?}",
        w.out
    );

    // The line then starts where it was opened, as any line starts at a commit of main.
    let (code, last) = w.run(ReleaseOptions {
        commit: Some(BUMP.into()),
        prepare: true,
        ..super::mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("PREPARED aterm 0.6.0"), "{last}");
    let journal = w.journal().expect("the line's journal");
    assert_eq!(
        (journal.version.as_str(), journal.commit.as_str()),
        ("0.6.0", BUMP)
    );
    assert_eq!(w.count("bump"), 1, "starting the line bumps nothing more");
}

/// A TIP WITHOUT THE PREDECESSOR'S FIXTURES IS REFUSED. The published release's after-cut has
/// not landed its handoff fixtures, so a version opened now could start no line: nothing is
/// pushed, the engine is not touched, and the remedy names the clone that cut it.
/// NEGATIVE CONTROL: without the question the bump is pushed and main declares a version whose
/// every line the handoff-fixture gate then refuses.
#[test]
fn open_next_refuses_a_tip_without_the_predecessors_fixtures() {
    let mut w = World::new();
    w.predecessor_fixtures = Ok(false);
    let (code, last) = w.run(open_next());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(
        last.contains("does not hold v0.5.0's handoff fixtures"),
        "{last}"
    );
    assert!(
        last.contains("finish v0.5.0 in the clone that cut it"),
        "{last}"
    );
    assert!(last.ends_with("--open-next"), "{last}");
    assert_eq!(w.count("bump"), 0, "{:?}", w.calls);
    assert_eq!(w.count("sync"), 0, "{:?}", w.calls);
    assert_eq!(w.tip(), TIP, "main is as it was");
    assert!(w.repairs.asked.is_empty(), "no source red, no worker");
}

/// Fixtures the next cut's gate would refuse are a refusal in the gate's own words, never a
/// pass and never a missing directory.
#[test]
fn open_next_refuses_fixtures_the_gate_would_refuse() {
    let mut w = World::new();
    w.predecessor_fixtures = Err(Refusal::new(
        "v0.5.0's handoff fixtures at bbbbbbbbb: desk two has no socket record",
        "repair them on main, then run it again",
    ));
    let (code, last) = w.run(open_next());
    assert_eq!(code, 1, "{last}");
    assert!(last.contains("desk two has no socket record"), "{last}");
    assert_eq!(w.count("bump"), 0);
}

/// MAIN ALREADY DECLARES AN UNUSED VERSION: the next version is open, and opening another
/// would skip it. The remedy starts its line.
#[test]
fn open_next_refuses_while_the_declared_version_is_unused() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    let (code, last) = w.run(open_next());
    assert_eq!(code, 1, "{last}");
    assert!(
        last.contains("origin/main already declares 0.6.0, which no release has used"),
        "{last}"
    );
    assert!(last.ends_with(&format!("--commit {TIP}")), "{last}");
    assert_eq!(w.count("bump"), 0);
    assert_eq!(
        w.fetches, 0,
        "refused from this clone's refs, before any dispatch"
    );
}

/// THE TIP'S OWN CUTTER OPENS THE VERSION. A launcher built from any other commit freezes the
/// tip's cutter and execs it before it reads origin, asks a gate or touches the engine, so a
/// defect in opening a version is repaired by a landing on main.
/// NEGATIVE CONTROL: asking the fixtures or syncing the engine before the dispatch lets the
/// finished release's cutter decide whether the next version can open.
#[test]
fn open_next_dispatches_to_the_tips_cutter_before_origin_or_the_engine() {
    let mut w = World::new();
    w.own_cutter = false;
    w.exec_result = Ok(());
    let (code, _) = w.run(open_next());
    assert_eq!(code, 0, "{:#?}", w.out);
    assert_eq!(w.exec_commit.as_deref(), Some(TIP));
    assert_eq!(w.fetches, 0);
    for later in [
        "fixtures-at bbbb v0.5.0",
        "commit-gates-dry",
        "sync",
        "bump",
    ] {
        assert_eq!(w.count(later), 0, "{later}: {:?}", w.calls);
    }
    assert_eq!(w.tip(), TIP);
}

/// AN UNFINISHED AFTER-CUT IN THIS CLONE DOES NOT HOLD THE NEXT VERSION. The clone that cut
/// 0.6.0 still owes its site half (its journal is unfinished), and its fixtures are on main.
/// `--open-next` opens 0.7.0 and leaves that journal byte for byte, so a bare run still
/// resumes 0.6.0's after-cut.
/// NEGATIVE CONTROL: selecting through the journal resumes 0.6.0 ("its bump is made") and
/// opens nothing.
#[test]
fn open_next_runs_beside_an_unfinished_after_cut_and_leaves_its_journal() {
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    w.origin_tags.insert("v0.6.0".into(), BUMP.into());
    w.ledger.push("0.6.0".into());
    let path = w.release_dir().join(JOURNAL_FILE);
    let mut journal = w.journal().unwrap();
    journal.done.push("site-deferred".into());
    assert!(!journal.finished());
    journal.save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let tip = w.tip();
    w.calls.clear();

    let (code, last) = w.run(open_next());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("OPENED aterm 0.7.0 at "), "{last}");
    assert_eq!(w.versions[&w.tip()], "0.7.0");
    assert_ne!(w.tip(), tip);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the journal is untouched"
    );
    assert!(!w.release_dir().join(SELECTION_FILE).exists());
    assert_eq!(w.count("after-cut 0.6.0"), 0, "{:?}", w.calls);

    // The unfinished release still resumes from its journal.
    w.calls.clear();
    let (code, last) = w.run(super::mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    assert_eq!(w.count("after-cut 0.6.0"), 1, "{:?}", w.calls);
    assert_eq!(w.count("bump"), 0, "{:?}", w.calls);
}

/// A RED OF THE BUMP'S OWN GATES PUSHES NOTHING, and both halves of its remedy name
/// `--open-next`: the fix lands on main, where the next attempt writes its commit.
#[test]
fn open_next_blocked_by_the_preflight_names_itself_again() {
    let mut w = World::new();
    w.blocks = vec![
        Refusal::new("changelog — [Unreleased] has no entry", "write one").failing("changelog"),
    ];
    let (code, last) = w.run(open_next());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(
        last.contains("the preflight BLOCKED (1 finding(s))"),
        "{last}"
    );
    let command = crate::ledger::RELEASE_COMMAND;
    assert_eq!(
        super::remedy_halves(&last),
        (
            &*format!("land the fix on main, then {command} --open-next"),
            &*format!("fix it, then {command} --open-next")
        )
    );
    assert_eq!(w.count("bump"), 0);
    assert_eq!(w.count("sync"), 0);
    assert!(w.journal().is_none());
}

/// ONE RED OF THE OPENING OPENS ONE WORKER, HOWEVER MAIN MOVES. The bump's commit gates are red
/// on main; each try judges whatever commit the tip then is. The failure names the version it
/// opens after, so the second red, at another tip, names the worker already on it.
/// NEGATIVE CONTROL: keyed as a cut's inputs (the version and the tip), the red at the moved
/// tip is a new input hash and spawns a second worker.
#[test]
fn a_red_open_next_opens_one_worker_however_main_moves() {
    let blocked = || {
        vec![
            Refusal::new("changelog — [Unreleased] has no entry", "write one").failing("changelog"),
        ]
    };
    let mut w = World::new();
    w.blocks = blocked();
    let (code, last) = w.run(open_next());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert_eq!(w.repairs.asked.len(), 1);
    assert_eq!(w.repairs.asked[0].opening.as_deref(), Some("0.5.0"));
    assert_eq!(w.repairs.asked[0].commit, TIP);
    assert_eq!(w.repairs.spawned.len(), 1, "{:#?}", w.out);

    // main moves and still declares 0.5.0; the same red at its new tip.
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.5.0".into());
    w.blocks = blocked();
    let (code, last) = w.run(open_next());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert_eq!(w.repairs.asked.len(), 2);
    assert_eq!(w.repairs.asked[1].commit, NEXT, "judged at the moved tip");
    assert_eq!(w.repairs.asked[1].opening.as_deref(), Some("0.5.0"));
    assert_eq!(w.repairs.spawned.len(), 1, "no second worker: {:#?}", w.out);
    assert!(
        w.out
            .iter()
            .any(|line| line.contains("was opened on these inputs already")),
        "{:#?}",
        w.out
    );
    assert_eq!(w.count("bump"), 0);
}

/// MAIN MOVED WHILE THE TIP'S CUTTER WAS FROZEN. The commit lands on the refreshed tip, so the
/// questions are asked of it: a main whose version another writer already moved is refused
/// rather than bumped twice.
#[test]
fn open_next_asks_the_refreshed_tip_not_the_selected_one() {
    let mut w = World::new();
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.main_at_next_fetch = Some(vec![OLD.into(), TIP.into(), NEXT.into()]);
    let (code, last) = w.run(open_next());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(
        last.contains("origin/main's version moved from 0.5.0 to 0.6.0"),
        "{last}"
    );
    assert_eq!(w.count("bump"), 0);

    // A main that moved without opening a version is asked at its new tip and opened there.
    let mut w = World::new();
    w.versions.insert(NEXT.into(), "0.5.0".into());
    w.main_at_next_fetch = Some(vec![OLD.into(), TIP.into(), NEXT.into()]);
    let (code, last) = w.run(open_next());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("fixtures-at dddd v0.5.0"), 1, "{:?}", w.calls);
    assert_eq!(w.history, [OLD, TIP, NEXT, BUMP]);
}

/// A DRY RUN ASKS THE SAME QUESTIONS AND WRITES NOTHING: no claim of the clone, no push, the
/// bump built privately.
#[test]
fn a_dry_open_next_judges_the_private_bump_and_changes_no_ref() {
    let mut w = World::new();
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..open_next()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("DRY RUN aterm 0.6.0: --open-next would commit"),
        "{last}"
    );
    assert_eq!(w.count("bump"), 0);
    assert_eq!(w.count("sync"), 0);
    assert_eq!(w.count("preview-bump"), 1);
    assert_eq!(w.tip(), TIP);

    let mut refused = World::new();
    refused.predecessor_fixtures = Ok(false);
    let (code, last) = refused.run(ReleaseOptions {
        dry_run: true,
        ..open_next()
    });
    assert_eq!(code, 1, "{last}");
    assert!(last.contains("a real run would refuse"), "{last}");
    assert_eq!(refused.count("preview-bump"), 0);
}

/// One run per clone: a live release here keeps the checkout, and `--open-next` refuses
/// rather than move it.
#[test]
fn open_next_refuses_beside_a_live_run_of_this_clone() {
    let mut w = World::new();
    std::fs::create_dir_all(w.release_dir()).unwrap();
    std::fs::write(
        w.release_dir().join(RUNNING_FILE),
        "pid 777\nstarted Thu Oct  9 01:00:00 2026\nlogs /logs/x\n",
    )
    .unwrap();
    w.alive.insert(777, "Thu Oct  9 01:00:00 2026".into());
    let (code, last) = w.run(open_next());
    assert_eq!(code, 1, "{last}");
    assert!(last.contains("777"), "{last}");
    assert_eq!(w.count("bump"), 0);
    assert_eq!(w.count("move bbbb"), 0, "the checkout is the live run's");
}

/// `--open-next` starts no line, so it takes none of a line's flags.
#[test]
fn open_next_takes_no_line_flag() {
    for extra in [
        vec!["--bump"],
        vec!["--commit", "abc"],
        vec!["--fix", "abc"],
        vec!["--prepare"],
        vec!["--qualify"],
        vec!["--forget"],
        vec!["--mac-only"],
        vec!["--abandon", "v0.6.0"],
        vec!["--prefreeze"],
    ] {
        let mut args: Vec<String> = vec!["release".into(), "--open-next".into()];
        args.extend(extra.iter().map(ToString::to_string));
        let parsed = crate::cli::parse(&args);
        assert!(parsed.is_err(), "{extra:?} parsed: {parsed:?}");
    }
    let parsed = crate::cli::parse(&[
        "release".into(),
        "--open-next".into(),
        "--dry-run".into(),
        "--verbose".into(),
    ]);
    let Ok(crate::cli::Cmd::Release(opts)) = parsed else {
        panic!("{parsed:?}");
    };
    assert!(opts.open_next && opts.dry_run && opts.verbose);
}
