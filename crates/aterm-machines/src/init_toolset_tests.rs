// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::bind;
use crate::items::defaults::tests::ctx;
use crate::model::{Mode, Outcome};
use crate::port::Fake;
use aterm_converge::{Class, Layer, NoJournal, Scope};
use std::sync::Arc;

/// A line the fake atpkg leaves for its next pass by.
const LEFT: &str = "left for the next pass";

fn left_for_next_pass(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| line.contains(LEFT))
        .map(str::to_owned)
        .collect()
}

/// atpkg's words as the tests speak them: one update flag, and [`LEFT`].
pub(crate) const ATPKG: Atpkg = Atpkg {
    update_flags: &["--defer-busy-flip"],
    left_for_next_pass,
};

/// A store the passes change: what the read finds before and after them, each pass's
/// arguments and exit, and the passes a stop ended (a pass reads its stop once, as it
/// starts, and one that reads it ends at once).
#[derive(Default)]
struct Store {
    found: Mutex<Vec<Found>>,
    ran: Mutex<Vec<Vec<String>>>,
    exits: Mutex<Vec<Ran>>,
    ended: Mutex<Vec<String>>,
}

fn item(store: &Arc<Store>, wait: Option<Duration>) -> Toolset {
    let (read, run) = (Arc::clone(store), Arc::clone(store));
    Toolset::new(
        Box::new(move || {
            let mut found = read.found.lock().unwrap();
            if found.len() > 1 {
                found.remove(0)
            } else {
                found[0].clone()
            }
        }),
        Box::new(move |args: &[String], budget, stopped: &dyn Fn() -> bool| {
            run.ran.lock().unwrap().push(args.to_vec());
            if stopped() {
                run.ended.lock().unwrap().push(args[0].clone());
                return Err(RunFailed::TimedOut(budget));
            }
            let mut exits = run.exits.lock().unwrap();
            Ok(if exits.is_empty() {
                Ran::ok("")
            } else {
                exits.remove(0)
            })
        }),
        wait,
        ATPKG,
    )
}

fn store(found: Vec<Found>, exits: Vec<Ran>) -> Arc<Store> {
    Arc::new(Store {
        found: Mutex::new(found),
        exits: Mutex::new(exits),
        ..Store::default()
    })
}

/// The item's row, the machine seeing nothing of it but the stops its passes hold.
fn run(item: &Toolset, mode: Mode) -> aterm_converge::ItemReport {
    let sys = Fake::default();
    let row = run_on(item, mode, &sys);
    let calls = sys.calls();
    assert!(
        calls.is_empty() || calls == ["defer stops", "stops restored"],
        "{calls:?}"
    );
    row
}

fn run_on(item: &Toolset, mode: Mode, sys: &Fake) -> aterm_converge::ItemReport {
    let ctx = ctx(&[]);
    let bound = bind([(item as &dyn Item, place())], sys, &ctx);
    let mut rows =
        aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
            .unwrap();
    rows.remove(0)
}

fn missing() -> Found {
    Found::Missing("2 of 7 missing (claude, muse)".into())
}

fn complete() -> Found {
    Found::Complete("7 active".into())
}

/// The passes a window's launch lane runs: seed, then update with the unattended lanes'
/// flag, each waiting for the store lock.
#[test]
fn a_missing_program_is_installed_by_seed_then_update_and_read_back() {
    let store = store(vec![missing(), complete()], Vec::new());
    let item = item(&store, Some(WAIT_LOCK));
    let row = run(&item, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(row.to.as_deref(), Some("7 active"));
    assert_eq!(
        (row.layer, row.class),
        (Some(Layer::Base), Some(Class::Install))
    );
    assert_eq!(
        *store.ran.lock().unwrap(),
        [
            &["seed", "--wait-lock", "1800"][..],
            &["update", "--defer-busy-flip", "--wait-lock", "1800"]
        ]
    );
}

/// The second run finds the set installed and runs no pass; a plan never runs one.
#[test]
fn a_complete_toolset_runs_nothing_and_a_plan_runs_nothing() {
    let done = store(vec![complete()], Vec::new());
    let row = run(&item(&done, Some(WAIT_LOCK)), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
    assert!(done.ran.lock().unwrap().is_empty());

    let wanted = store(vec![missing()], Vec::new());
    let row = run(&item(&wanted, Some(WAIT_LOCK)), Mode::Plan);
    assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
    assert!(wanted.ran.lock().unwrap().is_empty());
}

/// A decline, Automatic updates off, `auto_install = false` and the rest of atpkg's
/// refusals read as not applicable, carrying why, and run nothing: init never undoes a
/// person's answer.
#[test]
fn what_atpkg_may_not_install_is_said_and_left_alone() {
    let off = store(vec![Found::Off("switched off".into())], Vec::new());
    let row = run(&item(&off, Some(WAIT_LOCK)), Mode::Apply);
    assert_eq!(row.outcome, Outcome::NotApplicable, "{row:?}");
    assert_eq!(row.why.as_deref(), Some("switched off"));
    assert!(off.ran.lock().unwrap().is_empty());

    let unread = store(vec![Found::Unread("no store".into())], Vec::new());
    let row = run(&item(&unread, Some(WAIT_LOCK)), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Unknown, "{row:?}");
    assert!(unread.ran.lock().unwrap().is_empty());
}

/// A pass that fails ends the row Failed carrying atpkg's words (the line it points back to
/// too), and update never runs after a failed seed; an update that installs part of the set
/// and fails the rest (exit 1) is Failed the same way.
#[test]
fn a_failed_or_partial_install_is_the_rows_outcome_in_atpkgs_words() {
    let failed = store(
        vec![missing()],
        vec![Ran::exit(1, "atpkg: seed: the store is not writable\n")],
    );
    let row = run(&item(&failed, Some(WAIT_LOCK)), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    let why = row.why.unwrap_or_default();
    assert!(
        why.contains("atpkg: seed: the store is not writable"),
        "{why}"
    );
    assert_eq!(failed.ran.lock().unwrap().len(), 1);

    let partial = store(
        vec![missing()],
        vec![
            Ran::ok(""),
            Ran::exit(
                1,
                "atpkg: muse: download refused\natpkg: default set incomplete: muse\n",
            ),
        ],
    );
    let row = run(&item(&partial, Some(WAIT_LOCK)), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    let why = row.why.unwrap_or_default();
    for line in [
        "atpkg: muse: download refused",
        "atpkg: default set incomplete: muse",
    ] {
        assert!(why.contains(line), "{why}");
    }
    assert_eq!(partial.ran.lock().unwrap().len(), 2);
}

/// An update that exits 0 having left a program for its next pass (a signature check that
/// did not finish) stands the row aside in atpkg's words, never Failed. CONTROL: the same
/// line beside a set the read finds complete (an upgrade left for later) is Changed.
#[test]
fn a_program_left_for_the_next_pass_stands_the_row_aside() {
    let said = format!("atpkg: claude 2.1.300 {LEFT}");
    let deferred = store(
        vec![missing(), missing()],
        vec![
            Ran::ok(""),
            Ran::ok(&format!("atpkg: codex current\n{said}\n")),
        ],
    );
    let row = run(&item(&deferred, Some(WAIT_LOCK)), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert!(
        matches!(&row.stood_aside, Some(StoodAside::Lock(why)) if why.contains(&said)),
        "{row:?}"
    );

    let upgraded = store(
        vec![missing(), complete()],
        vec![Ran::ok(""), Ran::ok(&format!("{said}\n"))],
    );
    let row = run(&item(&upgraded, Some(WAIT_LOCK)), Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(row.stood_aside, None);
}

/// `--no-wait` passes no `--wait-lock`, and a store another pass holds stands the row
/// aside on the lock (exit 75), as no network stands it aside offline (exit 69).
#[test]
fn a_held_store_and_no_network_stand_the_row_aside() {
    for (exit, wait, aside_lock) in [(CONTENDED, None, true), (OFFLINE, Some(WAIT_LOCK), false)] {
        let held = store(
            vec![missing()],
            vec![Ran::ok(""), Ran::exit(exit, "atpkg: busy or offline\n")],
        );
        let row = run(&item(&held, wait), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert_eq!(
            matches!(row.stood_aside, Some(StoodAside::Lock(_))),
            aside_lock,
            "{row:?}"
        );
        assert_eq!(
            matches!(row.stood_aside, Some(StoodAside::Offline(_))),
            !aside_lock,
            "{row:?}"
        );
        let ran = held.ran.lock().unwrap();
        assert_eq!(ran[0][0], "seed");
        assert_eq!(ran[1][0], "update");
        assert_eq!(
            ran[1].iter().any(|arg| arg == "--wait-lock"),
            wait.is_some(),
            "{ran:?}"
        );
    }
}

/// A stop while a pass runs ends that pass (the port ends its process group) and starts no
/// other; a stop read once a pass finished starts no later one. Either way the item says on
/// standard error, while it still holds the stops, where among the passes the stop came
/// (during the pass it ended, or after the one that finished and before the next), and the
/// stop is sent again once it lets them go. In a live run that ends the run as the stop
/// would have, before any row is reported, so the said line is what the person sees of the
/// row; the Fake records the stop sent again instead. NEGATIVE
/// CONTROL: with no stop both passes run to the end, nothing is said and nothing is sent
/// again.
#[test]
fn a_stop_ends_the_running_pass_starts_no_other_and_is_sent_again() {
    let stop = "stop sent again: signal 2";
    let seed = "`aterm pkg seed`";
    let update = "`aterm pkg update --defer-busy-flip`";
    let between = format!("after {seed}, before {update}");
    // The Fake's stop readings: the check before seed, seed's own, the check after it, then
    // the same three for update.
    for (stop_after, ended, ran, when) in [
        (Some(0), &[][..], 0, Some(format!("before {seed}"))),
        (
            Some(1),
            &["seed"][..],
            1,
            Some(format!("during {seed}, which was ended")),
        ),
        (Some(2), &[], 1, Some(between.clone())),
        (Some(3), &[], 1, Some(between.clone())),
        (
            Some(4),
            &["update"],
            2,
            Some(format!("during {update}, which was ended")),
        ),
        (Some(5), &[], 2, Some(format!("after {update}"))),
        (None, &[], 2, None),
    ] {
        let store = store(vec![missing(), complete()], Vec::new());
        let mut sys = Fake::default();
        sys.stop_after = stop_after;
        let sys = Arc::new(sys);
        let heard = Arc::new(Mutex::new(Vec::new()));
        let item = item(&store, None).saying({
            let (heard, sys) = (Arc::clone(&heard), Arc::clone(&sys));
            Arc::new(move |line: &str| {
                heard.lock().unwrap().push((line.to_owned(), sys.calls()));
            })
        });
        // Exercise the item's stop boundaries directly: the setup engine also checks
        // stops before entering an item, which would consume this scripted sequence.
        let applied = item.apply_with_guards(&*sys, &|| Ok(()), &|| Ok(()));
        assert_eq!(*store.ended.lock().unwrap(), ended, "{stop_after:?}");
        assert_eq!(store.ran.lock().unwrap().len(), ran, "{stop_after:?}");
        let calls = sys.calls();
        assert_eq!(calls.first().map(String::as_str), Some("defer stops"));
        assert_eq!(
            calls.iter().any(|call| call == stop),
            when.is_some(),
            "{stop_after:?}: {calls:?}"
        );
        let heard = heard.lock().unwrap().clone();
        match when {
            Some(when) => {
                assert!(applied.is_err(), "{applied:?}");
                let [(line, held)] = heard.as_slice() else {
                    panic!("{stop_after:?}: one line said: {heard:?}");
                };
                assert!(line.starts_with(STOPPED_LEAD), "{line}");
                assert!(
                    line.contains(&format!("stopped (SIGINT) {when};")),
                    "{line}"
                );
                assert_eq!(
                    held,
                    &["defer stops"],
                    "{stop_after:?}: said while the stops are still held"
                );
            }
            None => {
                assert!(applied.is_ok(), "{applied:?}");
                assert!(heard.is_empty(), "{heard:?}");
            }
        }
    }
}

#[test]
fn the_item_is_named_by_its_id_its_family_and_the_base_layer() {
    for word in [ID, FAMILY, "base"] {
        assert!(selected(word), "{word}");
    }
    for word in ["base.pkg.toolse", "workstation", "base.clt", "develop"] {
        assert!(!selected(word), "{word}");
    }
}
