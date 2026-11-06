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
/// crate's cheapest, their figures summed; the least cover wins. Work of another kind, or a crate
/// no run named with no workspace run beside it, covers nothing.
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
            bytes: 6 * GIB,
            selections: 2,
            runs: 2,
            exact: false,
            floor: 0
        }
    );
    // `b` is cheapest through `a b` (4 GiB), not its own 9 GiB run, and one selection holding
    // both its crates is read alone.
    let b = m.cover(&sel(Some(&["a", "b"])), 0, COLD).expect("covered");
    assert_eq!((b.bytes, b.selections, b.exact), (4 * GIB, 1, true));
    let only_a = m.cover(&sel(Some(&["a"])), 0, COLD).expect("covered");
    assert_eq!(
        (only_a.bytes, only_a.selections, only_a.exact),
        (4 * GIB, 1, false)
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
        Some(6 * GIB),
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

/// A run that did a selection's work and more of the words its keeper names as adding work alone
/// (for verify, its checks that build) bounds it, over its crates or summed with others; a word of
/// any other kind (pulled cells, a compile-only run) must be the same in both, and less work never
/// bounds more. NEGATIVE CONTROL: read by the same work alone ([`MachineFootprints::cover`]),
/// the record covers none of these.
#[test]
fn a_run_of_more_checks_over_its_crates_bounds_one_of_fewer() {
    let more: Vec<String> = ["cells", "doc", "lint", "test"]
        .iter()
        .map(|w| (*w).to_owned())
        .collect();
    let at = |work: &str, crates: &[&str]| Selection {
        work: work.to_owned(),
        crates: Some(crates.iter().map(|c| (*c).to_owned()).collect()),
    };
    let mut m = MachineFootprints::default();
    m.push(at("cells doc lint test", &["a", "b"]), run(0, 3, 1, 10));
    m.push(at("lint test --compile-only", &["c"]), run(0, 2, 2, 20));
    m.push(at("cells lint pulled-cells", &["d"]), run(0, 5, 3, 30));
    let lint_a = at("lint test", &["a"]);
    assert_eq!(m.cover(&lint_a, 0, COLD), None, "the same work alone");
    let c = m
        .cover_by(&lint_a, 0, COLD, &more)
        .expect("more checks over more crates bound it");
    assert_eq!((c.bytes, c.selections, c.exact), (3 * GIB, 1, false));
    assert_eq!(
        m.cover_by(&at("lint", &["c"]), 0, COLD, &more),
        None,
        "--compile-only changes what the rest write"
    );
    assert_eq!(
        m.cover_by(&at("lint test --compile-only", &["c"]), 0, COLD, &more)
            .map(|c| (c.bytes, c.exact)),
        Some((2 * GIB, true))
    );
    assert_eq!(
        m.cover_by(&at("cells lint", &["d"]), 0, COLD, &more),
        None,
        "pulled cells change what cells lints"
    );
    assert_eq!(
        m.cover_by(
            &at("cells doc lint test pulled-cells", &["a"]),
            0,
            COLD,
            &more
        ),
        None,
        "less work never bounds more"
    );
    assert_eq!(m.cover_by(&at("lint", &["a", "e"]), 0, COLD, &more), None);
    m.push(at("doc lint", &["e"]), run(0, 1, 4, 40));
    let summed = m
        .cover_by(&at("lint", &["a", "e"]), 0, COLD, &more)
        .expect("each crate's cheapest of work bounding its");
    assert_eq!((summed.bytes, summed.selections), (4 * GIB, 2));
    assert_eq!(m.cover(&at("lint", &["a", "e"]), 0, COLD), None);
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
/// directories' tag included; text of another schema, or none, reads as no record.
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
        MachineFootprints::decode(&m.encode().replace("\"schema\":1", "\"schema\":9")),
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
