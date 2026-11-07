// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's footprint record ([`MachineFootprints`]): what runs in this machine's
//! checkouts bound a run to write, and that it only ever lowers a claim.

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use super::*;
use crate::Footprints;
use crate::floor::GIB;

/// The cold budget the tests' kind writes into empty directories.
const COLD: u64 = 72 * GIB;

/// The work every selection below does, but [`other_work`]'s.
const WORK: &str = "doc lint test";

/// A selection of [`WORK`] over `crates`; `None` for the whole workspace.
fn sel(crates: Option<&[&str]>) -> Selection {
    Selection {
        work: WORK.to_owned(),
        crates: crates.map(|c| c.iter().map(|n| (*n).to_owned()).collect()),
    }
}

/// A run that found its directories holding `start` GiB and grew them by `grew` GiB, in the
/// checkout whose directories are tagged `dirs`.
fn run(start: u64, grew: u64, dirs: u64, at_ms: u64) -> Footprint {
    Footprint {
        peak: (start + grew) * GIB,
        grew: grew * GIB,
        at_ms,
        dirs: Some(dirs),
        scope: None,
    }
}

/// THE COMMON CASE: a fresh worktree runs a selection another worktree here ran. That run found
/// its directories holding the verify tool's own 2 GiB build, as every fresh worktree's do, and
/// grew them by 3 GiB; directories holding as much claim the 3 GiB it grew them by, empty ones
/// the 5 GiB it takes to bring them to its peak, and fuller ones (a `targo test` first) no less
/// than its growth. NEGATIVE CONTROL: a record with no run of it covers nothing, so the run
/// claims as before.
#[test]
fn a_selection_run_in_another_checkout_bounds_what_a_fresh_one_writes() {
    let mut m = MachineFootprints::default();
    let net = sel(Some(&["aterm-net", "aterm-secrets"]));
    m.push(net.clone(), run(2, 3, 1, 10));
    let at = |holds: u64| m.cover(&net, holds, COLD).expect("covered");
    assert_eq!(
        at(2 * GIB),
        Cover {
            bytes: 3 * GIB,
            selections: 1,
            runs: 1,
            exact: true,
            bounded: false,
            floor: 2 * GIB
        }
    );
    assert_eq!(at(0).bytes, 5 * GIB);
    assert_eq!(at(4 * GIB).bytes, 3 * GIB);
    assert_eq!(
        MachineFootprints::default().cover(&net, 2 * GIB, COLD),
        None
    );
}

/// A LONG-LIVED CHECKOUT is read from the floor, not from what it holds: the selection's only runs
/// began warm (a failed first run had built most of it, and only passing runs are kept), so each
/// grew its directories by 1 GiB to a 7 GiB peak, while a fresh run of another selection began
/// from the 2 GiB every run here begins from. A checkout holding 25 GiB of other selections'
/// units may lack every unit of this one: it claims the 5 GiB that bring a fresh checkout to the
/// peak, not the 1 GiB the warm runs grew theirs by. NEGATIVE CONTROL: read above what the
/// checkout holds, as a run measured on its own directories is, it would claim 1 GiB; and with
/// no fresh run kept, the warm runs' own start is the floor.
#[test]
fn a_checkout_holding_more_than_a_fresh_one_claims_no_less_than_a_fresh_one() {
    let mut m = MachineFootprints::default();
    let warm = sel(Some(&["aterm-gui"]));
    m.push(warm.clone(), run(6, 1, 1, 10));
    m.push(warm.clone(), run(6, 1, 2, 20));
    assert_eq!(
        m.cover(&warm, 25 * GIB, COLD).map(|c| (c.bytes, c.floor)),
        Some((GIB, 6 * GIB))
    );
    m.push(sel(Some(&["aterm-hash"])), run(2, 3, 3, 30));
    let long_lived = m.cover(&warm, 25 * GIB, COLD).expect("covered");
    assert_eq!((long_lived.bytes, long_lived.floor), (5 * GIB, 2 * GIB));
    assert!(GIB < long_lived.bytes, "not what the warm runs grew by");
    let fresh = m.cover(&warm, 2 * GIB, COLD).expect("covered");
    assert_eq!(fresh.bytes, 5 * GIB);
    let emptier = m.cover(&warm, GIB, COLD).expect("covered");
    assert_eq!((emptier.bytes, emptier.floor), (6 * GIB, GIB));
}

/// A selection's figure is the most of its runs' (a light run never hides a heavy one), and a
/// run's peak beyond what the directories hold is read no further than the cold budget, as a run
/// measured on them is ([`Footprints::need`]).
#[test]
fn a_selections_figure_is_the_most_of_its_runs() {
    let mut m = MachineFootprints::default();
    let s = sel(Some(&["a"]));
    m.push(s.clone(), run(0, 6, 1, 10));
    m.push(s.clone(), run(6, 0, 2, 20));
    m.push(s.clone(), run(1, 1, 3, 30));
    assert_eq!(
        m.cover(&s, 0, COLD).map(|c| (c.bytes, c.runs)),
        Some((6 * GIB, 3))
    );
    let mut stale = MachineFootprints::default();
    stale.push(s.clone(), run(300, 1, 1, 10));
    assert_eq!(stale.cover(&s, 0, COLD).map(|c| c.bytes), Some(COLD));
}

/// A NEW SELECTION IS COVERED by one holding it (the same work over more crates, or the whole
/// workspace) or by selections of the same work whose crates together name all of its, each
/// crate's cheapest, their figures summed; the least cover wins, each selection's figure no less
/// than a run of one it holds grew by (`a b` holds `b`, whose run grew 9 GiB, so it bounds
/// nothing below 9 GiB though its own run grew 4). Work of another kind, or a crate no run named
/// with no workspace run beside it, covers nothing.
#[test]
fn wider_selections_or_crates_held_together_cover_a_new_selection() {
    let mut m = MachineFootprints::default();
    m.push(sel(Some(&["a", "b"])), run(0, 4, 1, 10));
    m.push(sel(Some(&["c"])), run(0, 2, 2, 20));
    m.push(sel(Some(&["b"])), run(0, 9, 3, 30));
    let a_c = m.cover(&sel(Some(&["a", "c"])), 0, COLD).expect("covered");
    assert_eq!(
        a_c,
        Cover {
            bytes: 11 * GIB,
            selections: 2,
            runs: 2,
            exact: false,
            bounded: true,
            floor: 0
        }
    );
    // One selection holding both its crates is read alone, at no less than the 9 GiB `b` grew.
    let b = m.cover(&sel(Some(&["a", "b"])), 0, COLD).expect("covered");
    assert_eq!((b.bytes, b.selections, b.exact), (9 * GIB, 1, true));
    let only_a = m.cover(&sel(Some(&["a"])), 0, COLD).expect("covered");
    assert_eq!(
        (only_a.bytes, only_a.selections, only_a.exact),
        (9 * GIB, 1, false)
    );
    assert_eq!(m.cover(&sel(Some(&["a", "d"])), 0, COLD), None);
    assert_eq!(
        m.cover(&sel(None), 0, COLD),
        None,
        "no run held the workspace"
    );
    m.push(sel(None), run(0, 61, 4, 40));
    let d = m
        .cover(&sel(Some(&["a", "d"])), 0, COLD)
        .expect("the workspace holds it");
    assert_eq!((d.bytes, d.selections), (61 * GIB, 1));
    assert_eq!(
        m.cover(&sel(Some(&["a", "c"])), 0, COLD).map(|c| c.bytes),
        Some(11 * GIB),
        "the least cover"
    );
    let other_work = Selection {
        work: "cells:tree lint test".to_owned(),
        crates: Some(vec!["a".to_owned()]),
    };
    assert_eq!(m.cover(&other_work, 0, COLD), None);
    // No crates selected is held by any run of the same work, never summed from none.
    let none = m.cover(&sel(Some(&[])), 0, COLD).expect("held");
    assert_eq!((none.bytes, none.selections), (2 * GIB, 1));
}

/// A run that did a selection's work and more bounds it, over its crates or summed with others;
/// a narrowing word (pulled cells, a compile-only run) only bounds work narrowed at least as far,
/// and less work never bounds more.
#[test]
fn a_run_of_more_checks_over_its_crates_bounds_one_of_fewer() {
    let at = |work: &str, crates: &[&str]| Selection {
        work: work.to_owned(),
        crates: Some(crates.iter().map(|c| (*c).to_owned()).collect()),
    };
    let mut m = MachineFootprints::default();
    m.push(
        at("cells:tree doc lint test", &["a", "b"]),
        run(0, 3, 1, 10),
    );
    m.push(at("--compile-only lint test", &["c"]), run(0, 2, 2, 20));
    m.push(
        at("cells--pulled cells:tree lint", &["d"]),
        run(0, 5, 3, 30),
    );
    let lint_a = at("lint test", &["a"]);
    let c = m
        .cover(&lint_a, 0, COLD)
        .expect("more checks over more crates bound it");
    assert_eq!((c.bytes, c.selections, c.exact), (3 * GIB, 1, false));
    assert_eq!(
        m.cover(&at("lint", &["c"]), 0, COLD),
        None,
        "--compile-only changes what the rest write"
    );
    assert_eq!(
        m.cover(&at("--compile-only lint test", &["c"]), 0, COLD)
            .map(|c| (c.bytes, c.exact)),
        Some((2 * GIB, true))
    );
    assert_eq!(
        m.cover(&at("cells:tree lint", &["d"]), 0, COLD),
        None,
        "pulled cells change what cells lints"
    );
    assert_eq!(
        m.cover(
            &at("cells:tree doc lint test wasm-smoke:tree", &["a"]),
            0,
            COLD
        ),
        None,
        "less work never bounds more"
    );
    assert_eq!(m.cover(&at("lint", &["a", "e"]), 0, COLD), None);
    m.push(at("doc lint", &["e"]), run(0, 1, 4, 40));
    let summed = m
        .cover(&at("lint", &["a", "e"]), 0, COLD)
        .expect("each crate's cheapest of work bounding its");
    assert_eq!((summed.bytes, summed.selections), (4 * GIB, 2));
}

/// The record lowers a claim and never raises one: only a claim read from no run on its own
/// directories, the cold budget less what they hold, is lowered, and only to a figure below it.
/// NEGATIVE CONTROL: with no cover the estimate is the cold budget's, as before.
#[test]
fn the_machines_record_only_ever_lowers_a_claim() {
    let fresh = Footprints::default().need(2 * GIB, COLD, 7, Some(1));
    assert_eq!(fresh.bytes, 70 * GIB);
    let cover = |bytes: u64| Cover {
        bytes,
        selections: 1,
        runs: 1,
        exact: true,
        bounded: false,
        floor: 2 * GIB,
    };
    let lowered = fresh.or_machine(Some(cover(3 * GIB)));
    assert_eq!((lowered.bytes, lowered.runs), (3 * GIB, 0));
    assert_eq!(lowered.machine, Some(cover(3 * GIB)));
    let said = lowered.to_string();
    for words in [
        "3 GiB, the most a run of it in this machine's checkouts wrote",
        "beyond 2 GiB (the least any run the record keeps began from, or these directories hold \
         now, 2 GiB)",
        "of 1 run(s) the machine's record keeps",
        "no run of it measured on these directories",
    ] {
        assert!(said.contains(words), "{words:?} in {said}");
    }
    assert_eq!(fresh.or_machine(Some(cover(80 * GIB))), fresh);
    assert_eq!(fresh.or_machine(None), fresh);
    let mut here = Footprints::default();
    here.push(Footprint {
        peak: 9 * GIB,
        grew: 7 * GIB,
        at_ms: 1,
        dirs: Some(7),
        scope: Some(1),
    });
    let measured = here.need(2 * GIB, COLD, 7, Some(1));
    assert_eq!(measured.bytes, 7 * GIB);
    assert_eq!(measured.or_machine(Some(cover(GIB))), measured);
}

/// The record as the file holds it reads back the same, a selection's crates and a run's
/// directories' tag included; text of another schema (1, which spelled a narrowing word without
/// `--`), or none, reads as no record.
#[test]
fn the_record_reads_back_what_it_wrote() {
    let mut m = MachineFootprints::default();
    m.push(sel(Some(&["a", "b"])), run(2, 3, 11, 10));
    m.push(sel(None), run(0, 61, 12, 20));
    m.push(
        Selection {
            work: "appkit:tree".to_owned(),
            crates: None,
        },
        run(0, 2, 13, 30),
    );
    assert_eq!(MachineFootprints::decode(&m.encode()), m);
    assert_eq!(
        MachineFootprints::decode(
            &m.encode()
                .replace(&format!("\"schema\":{SCHEMA}"), "\"schema\":1")
        ),
        MachineFootprints::default()
    );
    assert_eq!(
        MachineFootprints::decode("not json"),
        MachineFootprints::default()
    );
}

/// Each selection keeps its last [`KEPT`] runs, and past [`SELECTIONS`] the one whose newest run
/// is oldest goes, never the one just kept.
#[test]
fn a_record_keeps_the_last_runs_of_the_selections_run_most_recently() {
    let mut m = MachineFootprints::default();
    let s = sel(Some(&["a"]));
    for i in 0..(KEPT as u64 + 3) {
        m.push(s.clone(), run(0, i + 1, 1, i));
    }
    let runs = &m.rows()[0].runs;
    assert_eq!(runs.len(), KEPT);
    assert_eq!(runs[0].grew, 4 * GIB, "the oldest went");
    let mut wide = MachineFootprints::default();
    wide.push(sel(Some(&["old"])), run(0, 1, 1, 0));
    for i in 1..=SELECTIONS as u64 {
        let name = format!("c{i}");
        wide.push(sel(Some(&[name.as_str()])), run(0, 1, 1, 1000 + i));
    }
    assert_eq!(wide.rows().len(), SELECTIONS);
    assert!(
        wide.rows()
            .iter()
            .all(|r| r.selection != sel(Some(&["old"])))
    );
    wide.push(sel(Some(&["late"])), run(0, 1, 1, 1));
    assert!(
        wide.rows()
            .iter()
            .any(|r| r.selection == sel(Some(&["late"]))),
        "the run just kept stays"
    );
}

/// Runs ending at once in many checkouts each keep theirs: every keep reads and writes the
/// record under its lock, so none writes over a read another's write has made stale.
#[test]
fn runs_kept_at_once_each_keep_theirs() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("machine-footprint-")
        .tempdir()
        .expect("scratch");
    let path = dir.path().join("verify/disk-footprint.json");
    let threads: Vec<_> = (0..8u64)
        .map(|t| {
            let path = path.clone();
            std::thread::spawn(move || {
                for i in 0..6u64 {
                    let name = format!("t{t}-{i}");
                    MachineFootprints::keep(&path, sel(Some(&[name.as_str()])), run(0, 1, t, i))
                        .expect("kept");
                }
            })
        })
        .collect();
    for t in threads {
        until_within(MUST_HAPPEN, "a keeper to keep its runs", || {
            t.is_finished().then_some(())
        });
        t.join().expect("a keeper");
    }
    assert_eq!(MachineFootprints::load(&path).rows().len(), 48);
}

/// A RUN IS BOUNDED BY RUNS THAT DID ALL ITS WORK ([`Selection::does`]), not only by runs of the
/// same words: a whole-workspace run of `cells:tree doc lint test` narrowed to the cells the cone
/// pulled (20 GiB) bounds a run of fewer checks (its narrowing of `cells` is no matter to one
/// that runs none), one whose `cells` reads the selection (its `cells:tree` stands for it), and
/// one narrowed further; runs of fewer checks over crates that together hold a run's are summed.
/// NEGATIVE CONTROLS, each the direction that would under-claim: a run narrowed by a word the
/// claimant lacks (`--compile-only`, which narrows all of its work, `cells--pulled`,
/// `cells--cell=`) bounds nothing for a claimant that does what it narrows, nor does a list
/// narrower than the claimant's, nor a run that did not do one of its words (`appkit:tree`,
/// `as-release`).
#[test]
fn a_run_is_bounded_by_runs_that_did_all_its_work() {
    let work = |work: &str, crates: Option<&[&str]>| Selection {
        work: work.to_owned(),
        crates: crates.map(|c| c.iter().map(|n| (*n).to_owned()).collect()),
    };
    let mut m = MachineFootprints::default();
    m.push(
        work("cells--pulled cells:tree doc lint test", None),
        run(0, 20, 1, 10),
    );
    m.push(work("--compile-only test", None), run(0, 5, 2, 20));
    m.push(
        work("cells--cell=linux,win cells:tree", None),
        run(0, 3, 3, 30),
    );
    let bytes =
        |w: &str, crates: Option<&[&str]>| m.cover(&work(w, crates), 0, COLD).map(|c| c.bytes);
    assert_eq!(bytes("doc lint test", Some(&["a"])), Some(20 * GIB));
    assert_eq!(
        bytes("cells cells--pulled lint", Some(&["a"])),
        Some(20 * GIB)
    );
    assert_eq!(bytes("cells--pulled cells:tree", None), Some(20 * GIB));
    assert_eq!(
        bytes("--compile-only test", Some(&["a"])),
        Some(5 * GIB),
        "the least"
    );
    assert_eq!(bytes("--compile-only lint test", None), Some(20 * GIB));
    assert_eq!(
        bytes("test", None),
        Some(20 * GIB),
        "not the compile-only run's 5"
    );
    assert_eq!(bytes("cells--cell=win cells:tree", None), Some(3 * GIB));
    assert_eq!(
        bytes("cells--cell=mac,win cells:tree", None),
        None,
        "mac it never built"
    );
    assert_eq!(
        bytes("cells:tree", None),
        None,
        "every cell's whole graph, which none built"
    );
    assert_eq!(bytes("appkit:tree", None), None);
    assert_eq!(bytes("as-release lint test", Some(&["a"])), None);

    let mut parts = MachineFootprints::default();
    parts.push(work("doc lint test", Some(&["a"])), run(0, 2, 4, 40));
    parts.push(work("lint test", Some(&["b"])), run(0, 1, 5, 50));
    let summed = parts
        .cover(&work("lint test", Some(&["a", "b"])), 0, COLD)
        .expect("a's and b's runs together");
    assert_eq!(
        (summed.bytes, summed.selections, summed.exact),
        (3 * GIB, 2, false)
    );
    assert_eq!(
        parts.cover(&work("doc lint test", Some(&["a", "b"])), 0, COLD),
        None,
        "b's run did no doc"
    );
}

/// A WIDER SELECTION BOUNDS NOTHING BELOW WHAT A NARROWER ONE WROTE: runs kept from different
/// builds of the tree disagree, as on this Mac on 2026-11-06, when a whole-workspace run of every
/// check peaked below what a cold whole-workspace `test --compile-only` grew. A whole-workspace
/// `doc lint test` run that grew 10 GiB, beside a whole-workspace `test` run that grew 15 GiB,
/// bounds a `lint test -p a` (which only it covers) to 15 GiB, and `test -p a` to 15 GiB too.
/// NEGATIVE CONTROL: without the `test` run the same claim is the wider run's own 10 GiB.
#[test]
fn a_wider_selection_bounds_nothing_below_what_a_narrower_one_grew() {
    let work = |work: &str, crates: Option<&[&str]>| Selection {
        work: work.to_owned(),
        crates: crates.map(|c| c.iter().map(|n| (*n).to_owned()).collect()),
    };
    let mut m = MachineFootprints::default();
    m.push(work("doc lint test", None), run(0, 10, 1, 10));
    let lint_test = work("lint test", Some(&["a"]));
    assert_eq!(
        m.cover(&lint_test, 0, COLD).map(|c| c.bytes),
        Some(10 * GIB)
    );
    m.push(work("test", None), run(0, 15, 2, 20));
    assert_eq!(
        m.cover(&lint_test, 0, COLD).map(|c| c.bytes),
        Some(15 * GIB)
    );
    assert_eq!(
        m.cover(&work("test", Some(&["a"])), 0, COLD)
            .map(|c| c.bytes),
        Some(15 * GIB)
    );
}

/// THE RUN ITSELF IS HELD TO THE GROWTH BOUND, whatever covers it: a sum of narrower selections'
/// figures reads no lower than what a run of a selection the run holds grew. A `test -p a -p b`
/// run grew 9 GiB, and `lint test` runs of `a` and of `b` read 2 GiB each; a `lint test -p a -p
/// b` does all of the `test` run's work over the same crates, so it claims 9 GiB, not the 4 its
/// parts sum to (the `test` run did no lint, so it covers nothing itself). NEGATIVE CONTROL:
/// without the `test` run the sum stands at 4 GiB, unraised.
#[test]
fn a_summed_cover_reads_no_less_than_a_run_the_claimant_holds_grew() {
    let work = |work: &str, crates: &[&str]| Selection {
        work: work.to_owned(),
        crates: Some(crates.iter().map(|n| (*n).to_owned()).collect()),
    };
    let claimant = work("lint test", &["a", "b"]);
    let mut parts = MachineFootprints::default();
    parts.push(work("lint test", &["a"]), run(0, 2, 1, 10));
    parts.push(work("lint test", &["b"]), run(0, 2, 2, 20));
    let summed = parts
        .cover(&claimant, 0, COLD)
        .expect("a's and b's together");
    assert_eq!(
        (summed.bytes, summed.selections, summed.bounded),
        (4 * GIB, 2, false)
    );
    let mut m = parts.clone();
    m.push(work("test", &["a", "b"]), run(0, 9, 3, 30));
    let bounded = m.cover(&claimant, 0, COLD).expect("covered");
    assert_eq!(
        (bounded.bytes, bounded.selections, bounded.bounded),
        (9 * GIB, 2, true)
    );
}
