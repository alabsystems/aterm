// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Disk Custody band's projection, layout, paint and hits.

use std::sync::Arc;

use aterm_vitals::history::WEEK_BUCKETS;
use aterm_vitals::model::{DeedKind, DiskFacts, DiskLevel};
use aterm_vitals::sys::BudgetFacts;
use aterm_vitals::sys::held::{Bytes, Class, Holding, Holdings, Mark, Walk, Worktree};

use super::*;
use crate::deck_face::deck_metrics;
use crate::stats_deck::paint_tests::AT;
use crate::stats_deck::tests::input;
use crate::stats_paint::Instrument;

const GB: u64 = 1_000_000_000;
const MIN_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
/// The fixture's scale.
const S: f32 = 2.0;

fn holding(
    project: &str,
    worktree: Worktree,
    (mark, keys, exe): (Mark, Vec<u64>, Option<&str>),
    bytes: u64,
    class: Class,
) -> Holding {
    Holding {
        project: project.into(),
        worktree,
        path: format!("/u/{project}"),
        tab: (!keys.is_empty()).then_some(1),
        keys,
        mark,
        exe: exe.map(str::to_owned),
        bytes: Bytes::Measured(bytes),
        class,
        next: held::Next::Nothing,
    }
}

/// A watched 1 TB volume 600 GB used: three holdings (trust's build output, costly, its
/// session gone; this tab's claude in its worktree, rebuildable; proofs, kept, no session),
/// aterm freed 41 GB today and its newest deed removed 12 GB of build caches 14 minutes ago, a
/// 41 GB removal two days ago, three weeks of hourly free space, and a budget 200 GB under its
/// target.
pub(super) fn custody() -> Snapshot {
    let mut s = Snapshot {
        seq: 1,
        at_unix_ms: AT,
        volume: Some(VolumeSample {
            total: 1000 * GB,
            free: 400 * GB,
            reserved: 0,
            mount: "/System/Volumes/Data".into(),
        }),
        ..Snapshot::default()
    };
    let rows = vec![
        holding(
            "aterm-s595dac",
            Worktree::Linked,
            (Mark::AtWork, vec![7], Some("claude")),
            76 * GB,
            Class::Rebuildable,
        ),
        holding(
            "trust",
            Worktree::Build,
            (Mark::Gone, Vec::new(), None),
            123 * GB,
            Class::Costly,
        ),
        holding(
            "proofs",
            Worktree::Folder,
            (Mark::NoSession, Vec::new(), None),
            31 * GB,
            Class::Kept,
        ),
    ];
    let removed_at = (AT - 48 * HOUR_MS).div_euclid(HOUR_MS) * HOUR_MS;
    s.facts.disk = Some(DiskFacts {
        level: Some(DiskLevel::Ok),
        warn: Some(50 * GB),
        floor: Some(30 * GB),
        emergency: Some(10 * GB),
        watch: true,
        freed_today: Some((41 * GB, AT - 14 * MIN_MS, "build caches".into())),
        deeds: vec![
            DiskDeed {
                at_unix_ms: AT - 3 * HOUR_MS,
                n: 1,
                kind: DeedKind::Checked,
                by: DeedBy::Aterm,
                bytes: None,
                free: Some(410 * GB),
            },
            DiskDeed {
                at_unix_ms: AT - 14 * MIN_MS,
                n: 1,
                kind: DeedKind::Removed("build caches".into()),
                by: DeedBy::Aterm,
                bytes: Some(12 * GB),
                free: None,
            },
        ],
        removals: vec![(removed_at, 41 * GB)],
        held: Some(Arc::new(Holdings {
            rows,
            walk: Walk::Complete,
            oldest_walk_s: Some(30),
        })),
        held_age_s: Some(5),
        journal_read: true,
        cleanup: Some(true),
        budget: BudgetRead::Sized(BudgetFacts {
            cap: 800 * GB,
            cap_written: true,
            kept: 450 * GB,
            target: 350 * GB,
            rebuildable: 150 * GB,
            partial: false,
            sized_s: 60,
        }),
        ..DiskFacts::default()
    });
    let now_k = AT.div_euclid(HOUR_MS);
    for k in now_k + 1 - WEEK_BUCKETS as i64..=now_k {
        let removed = if k * HOUR_MS >= removed_at {
            41 * GB
        } else {
            0
        };
        let used = 560 * GB + 100_000_000 * u64::try_from(k - now_k + 504).unwrap_or(0);
        s.free_weeks.note(
            aterm_vitals::ring::Taken::at(k * HOUR_MS + 1, 1_000),
            1000 * GB - used + removed,
        );
    }
    s
}

/// The band's projection of `snap` for [`input`]'s front tab (session 7) and clock.
fn doc_of(snap: &Snapshot) -> Doc {
    let m = deck_metrics();
    with_ctx(&input(snap), &m, doc)
}

/// The band painted `w_pt` wide at the fixture's scale.
fn painted(snap: &Snapshot, w_pt: f64) -> Painted {
    painted_at(snap, w_pt, S)
}

/// The band painted `w_pt` wide at `scale`.
fn painted_at(snap: &Snapshot, w_pt: f64, scale: f32) -> Painted {
    let g = DeckGeom {
        w: (w_pt * f64::from(scale)) as f32,
        h: height_px(f64::from(scale)) as f32,
        pad: 12.0 * scale,
        scale,
        font_px: 12.0 * scale,
    };
    paint(&g, 0.0, &input(snap), &deck_metrics())
}

/// The budget `snap` holds, sized.
fn sized(snap: &Snapshot) -> BudgetFacts {
    match snap.facts.disk.as_ref().map(|d| d.budget) {
        Some(BudgetRead::Sized(b)) => b,
        other => panic!("a sized budget: {other:?}"),
    }
}

/// `snap` with its disk facts changed by `f`.
fn with_disk(mut snap: Snapshot, f: impl FnOnce(&mut DiskFacts)) -> Snapshot {
    f(snap.facts.disk.as_mut().expect("disk facts"));
    snap
}

/// Every text `p` drew.
fn texts(p: &Painted) -> Vec<&str> {
    p.boxes.iter().map(|b| b.text.as_str()).collect()
}

/// PROJECTION. Each part says what the Snapshot holds, in the lens's words: Space now's words
/// with the cap, its bar the volume with its classes, the three level lines and the cap; the
/// three weeks merged two columns to one with the removal moved with them, the range they span
/// in the caption, said as the lens's chart says it; what aterm freed today and its newest deed
/// (not the older check) with its bytes and age kept whole by a cut; the holdings largest first,
/// a filled mark in this tab's ink for this tab's claude, a hollow one for the session gone and
/// none where no session claims it; and the used space 200 GB under the budget's cap. CONTROLS:
/// a deed by a tab names the tab; holdings that share a name are told apart.
#[test]
fn the_band_projects_each_part_from_the_snapshot() {
    let snap = custody();
    let d = doc_of(&snap);

    let space = words::space_line(Resource::Disk, &snap).expect("the volume is read");
    assert_eq!(d.space.words[0], format!("{space} \u{b7} cap 800 GB used"));
    assert_eq!(
        d.space.words[1..],
        [
            "600 GB used \u{b7} 400 GB free \u{b7} cap 800 GB used",
            "600 GB used \u{b7} 400 GB free",
            "600 GB used"
        ],
        "the total goes first, then the cap: {space}"
    );
    let bar = d.space.bar.as_ref().expect("the bar");
    assert_eq!(bar.v.used(), 600 * GB);
    let spans = bar.spans.expect("the classes");
    assert_eq!(
        (spans.rebuildable, spans.costly, spans.kept),
        (76 * GB, 123 * GB, 31 * GB)
    );
    assert_eq!(
        bar.lines,
        [
            ("warn", 50 * GB, false),
            ("clean", 30 * GB, false),
            ("emergency", 10 * GB, true)
        ]
    );
    assert_eq!(bar.cap, Some(800 * GB));
    assert!(
        d.space.said.contains("The budget's cap is 800 GB used."),
        "{}",
        d.space.said
    );

    let (trace, (lo, hi)) = d.weeks.trace.as_ref().expect("three weeks are read");
    assert_eq!(trace.vals.len(), 84, "168 columns merged two to one");
    assert_eq!(trace.removals.len(), 1);
    assert!(trace.vals.iter().all(Option::is_some));
    let read: Vec<f64> = trace.vals.iter().flatten().copied().collect();
    assert!(
        read.iter().all(|v| (*lo..=*hi).contains(v)) && hi > lo,
        "the axis spans every column: {lo}..{hi}"
    );
    assert!(
        d.weeks.words[0].starts_with("used ")
            && d.weeks.words[0].contains(&format!("{lo:.0}\u{2013}{hi:.0} GB")),
        "the caption says the strip's range: {:?}",
        d.weeks.words
    );
    assert!(
        d.weeks.said.starts_with("GB used over the last 3 weeks: ")
            && d.weeks.said.contains("Removed 41 gigabytes on"),
        "{}",
        d.weeks.said
    );

    assert_eq!(
        d.did.forms[0],
        (
            "aterm freed 41 GB today".to_owned(),
            "aterm removed build caches 12 GB \u{b7} 14 min ago".to_owned()
        )
    );
    assert_eq!(
        d.did.forms[1].1,
        "removed build caches 12 GB \u{b7} 14 min ago"
    );
    let tail = " 12 GB \u{b7} 14 min ago".len();
    assert_eq!(
        d.did.tails,
        [(0, tail), (0, tail)],
        "a cut keeps bytes and age"
    );

    let drawn: Vec<_> = d
        .held
        .holders
        .iter()
        .map(|h| {
            (
                h.mark,
                h.project.as_str(),
                h.exe.as_deref(),
                h.size.as_str(),
            )
        })
        .collect();
    assert_eq!(
        drawn,
        [
            (Some((false, false)), "trust", None, "123 GB"),
            (Some((true, true)), "aterm-s595dac", Some("claude"), "76 GB"),
            (None, "proofs", None, "31 GB"),
        ]
    );
    assert_eq!(
        tells(&d.held.holders),
        [None, None, None],
        "no name is shared"
    );
    assert!(
        d.held
            .said
            .starts_with("Held by, largest first: trust build, session gone")
    );

    // CONTROL: a holding no walk has sized ranks nowhere, and two that share a name (aterm's
    // state and its cache, both `aterm`) say their kinds of folder; with none sized, Held by
    // says so rather than listing dashes.
    let mut shared = custody();
    let rows = |extra: Vec<Holding>| {
        let mut rows = extra;
        rows.push(Holding {
            bytes: Bytes::Unmeasured(aterm_vitals::sys::UNAVAILABLE_WORDS[0]),
            ..holding(
                "unsized",
                Worktree::Folder,
                (Mark::NoSession, Vec::new(), None),
                0,
                Class::Kept,
            )
        });
        Some(Arc::new(Holdings {
            rows,
            walk: Walk::Complete,
            oldest_walk_s: Some(30),
        }))
    };
    if let Some(d) = shared.facts.disk.as_mut() {
        d.held = rows(vec![
            Holding {
                path: "/u/Library/Application Support/aterm".into(),
                ..holding(
                    "aterm",
                    Worktree::Aterm,
                    (Mark::NoSession, Vec::new(), None),
                    2 * GB,
                    Class::Kept,
                )
            },
            holding(
                "aterm",
                Worktree::Cache,
                (Mark::NoSession, Vec::new(), None),
                40 * GB,
                Class::Rebuildable,
            ),
        ]);
    }
    let named = |snap: &Snapshot| -> Vec<(String, Option<String>)> {
        let holders = doc_of(snap).held.holders;
        let tells = tells(&holders);
        holders.into_iter().map(|h| h.project).zip(tells).collect()
    };
    let pair = |p: &str, k: &str| (p.to_owned(), Some(k.to_owned()));
    assert_eq!(
        named(&shared),
        [pair("aterm", "cache"), pair("aterm", "Application Support")],
        "aterm's own folder says the folder it is in, never a kind it may not be"
    );
    // CONTROL: told apart only among the holdings a width draws: the first alone shares no
    // name with another drawn.
    assert_eq!(tells(&doc_of(&shared).held.holders[..1]), [None]);
    // aterm's state and its caches folder are both its own: the folders they are in tell them
    // apart.
    let own = |path: &str, bytes| Holding {
        path: path.into(),
        ..holding(
            "aterm",
            Worktree::Aterm,
            (Mark::NoSession, Vec::new(), None),
            bytes,
            Class::Kept,
        )
    };
    if let Some(d) = shared.facts.disk.as_mut() {
        d.held = rows(vec![
            own("/u/Library/Caches/aterm", 3 * GB),
            own("/u/Library/Application Support/aterm", 5 * GB),
        ]);
    }
    assert_eq!(
        named(&shared),
        [
            pair("aterm", "Application Support"),
            pair("aterm", "Caches")
        ]
    );
    let mut unsized_only = custody();
    if let Some(d) = unsized_only.facts.disk.as_mut() {
        d.held = rows(Vec::new());
    }
    assert_eq!(
        doc_of(&unsized_only).held.none,
        Some(("Held by".to_owned(), "not sized yet".to_owned()))
    );

    assert_eq!(
        d.next.forms,
        [
            (
                "build output 150 GB of 350 GB budget".to_owned(),
                "under the cap by 200 GB: the budget's pass takes nothing".to_owned()
            ),
            (
                "build output 150 GB of 350 GB".to_owned(),
                "under the cap: takes nothing".to_owned()
            ),
        ]
    );

    let mut tab = custody();
    if let Some(d) = tab.facts.disk.as_mut() {
        d.deeds[1].by = DeedBy::Tab(Some(7));
    }
    assert_eq!(
        doc_of(&tab).did.forms[0].1,
        "a tab removed build caches 12 GB \u{b7} 14 min ago",
        "CONTROL: a session's deed names its tab, `a tab` where no tab is read"
    );
}

/// NEXT PASS. The verdict is the budget pass's own: used space measured now against the cap.
/// Over it the pass removes build output no live session uses, unless removal is off, nothing
/// rebuildable is sized, or its last pass found no more it may take; under it the pass takes
/// nothing, whatever the sizing's floors. Its figures name the budget's own count (build output,
/// not Space now's rebuildable span), `≥` on both where the sizing was cut short, and the
/// sizing's age once it is old. CONTROLS: build output counted over its target by cloned files,
/// with used space under the cap, is not over; a look that has not read removal yet claims
/// nothing of it.
#[test]
fn next_pass_says_what_the_budgets_pass_does_now() {
    let over_cap = |snap: Snapshot| {
        let mut snap = snap;
        if let Some(v) = snap.volume.as_mut() {
            v.free = 150 * GB;
        }
        snap
    };
    let o = doc_of(&over_cap(custody()));
    assert_eq!(
        o.next.forms[0].1,
        "over the cap by 50 GB: the next pass removes build output no live session uses"
    );
    assert!(
        o.next
            .said
            .starts_with("Next pass: Used space is 50 GB over the 800 GB cap")
    );

    let off = doc_of(&with_disk(over_cap(custody()), |d| d.cleanup = Some(false)));
    assert_eq!(
        off.next.forms[0].1,
        "over the cap by 50 GB \u{b7} cleanup off (disk.auto_free_gib = 0)"
    );
    let unknown = doc_of(&with_disk(over_cap(custody()), |d| d.cleanup = None));
    assert_eq!(
        unknown.next.forms[0].1, o.next.forms[0].1,
        "CONTROL: a removal setting not read yet is not read as off"
    );
    let held = doc_of(&with_disk(over_cap(custody()), |d| {
        d.budget_unmet_unix = Some(AT / 1000 - 2 * 3600);
    }));
    assert_eq!(
        held.next.forms[0].1,
        "over the cap by 50 GB \u{b7} the last pass found no more it may take"
    );
    assert!(held.next.said.contains("2 h ago"), "{}", held.next.said);
    let none = doc_of(&with_disk(over_cap(custody()), |d| {
        d.budget = BudgetRead::Sized(BudgetFacts {
            rebuildable: 0,
            ..sized(&custody())
        });
    }));
    assert_eq!(
        none.next.forms[0].1,
        "over the cap by 50 GB: no build output is sized to take"
    );

    // Cut short and under the cap: the figures are floors, the verdict is exact.
    let partial = doc_of(&with_disk(custody(), |d| {
        d.budget = BudgetRead::Sized(BudgetFacts {
            partial: true,
            ..sized(&custody())
        });
    }));
    assert_eq!(
        partial.next.forms[1],
        (
            "build output \u{2265} 150 GB of \u{2265} 350 GB".to_owned(),
            "under the cap: takes nothing".to_owned()
        )
    );
    assert!(partial.next.said.contains("lower bounds"));
    // CONTROL: cloned files count build output past the target while used space is under the
    // cap: the pass does not run, and the band does not say it will.
    let clones = doc_of(&with_disk(custody(), |d| {
        d.budget = BudgetRead::Sized(BudgetFacts {
            rebuildable: 900 * GB,
            kept: 0,
            target: 800 * GB,
            ..sized(&custody())
        });
    }));
    assert_eq!(
        clones.next.forms[0].1,
        "under the cap by 200 GB: the budget's pass takes nothing"
    );
    let old = doc_of(&with_disk(custody(), |d| {
        d.budget = BudgetRead::Sized(BudgetFacts {
            sized_s: 3 * 3600,
            ..sized(&custody())
        });
    }));
    assert_eq!(
        old.next.forms[0].0,
        "build output 150 GB of 350 GB budget \u{b7} sized 3 h ago"
    );
    let off = doc_of(&with_disk(custody(), |d| d.budget = BudgetRead::Off));
    assert_eq!(
        off.next.forms[0],
        (
            "Next pass".to_owned(),
            "budget off (disk.cap_used_gib)".to_owned()
        )
    );
}

/// WHAT ATERM DID, CUT. At a width that cannot hold the newest deed whole, its words are cut and
/// its bytes and age kept whole after them: the figure a person reads it for stays. CONTROL: the
/// same line with no tail to keep is cut at its end, and loses them.
#[test]
fn a_cut_deed_keeps_its_bytes_and_age() {
    let snap = with_disk(custody(), |d| {
        d.freed_today = None;
        d.deeds[1].kind = DeedKind::Removed("aterm-tjd3qd42nw4-b build dir and its tooling".into());
    });
    let tail = " 12 GB \u{b7} 14 min ago";
    let cut_deed = |p: &Painted| {
        p.boxes
            .iter()
            .any(|b| b.text.contains("removed") && b.text.ends_with('\u{2026}'))
    };
    let m = deck_metrics();
    let mut w = 1400.0;
    let p = loop {
        let p = painted(&snap, w);
        if cut_deed(&p) {
            break p;
        }
        w -= 10.0;
        assert!(w >= 300.0, "PRECONDITION: some width cuts the deed");
    };
    assert!(
        p.boxes.iter().any(|b| b.text == tail),
        "the cut deed keeps its bytes and age at {w} pt: {:?}",
        texts(&p)
    );
    // CONTROL: with no tail kept the same width cuts the deed at its end.
    let mut doc = doc_of(&snap);
    doc.did.tails.clear();
    let g = DeckGeom {
        w: (w * f64::from(S)) as f32,
        h: height_px(f64::from(S)) as f32,
        pad: 12.0 * S,
        scale: S,
        font_px: 12.0 * S,
    };
    let plain = paint_doc(&g, 0.0, &input(&snap), &m, &doc, &[7]);
    assert!(cut_deed(&plain), "CONTROL: {:?}", texts(&plain));
    assert!(
        !plain.boxes.iter().any(|b| b.text.contains("14 min ago")),
        "CONTROL: cut at its end, the age is lost: {:?}",
        texts(&plain)
    );
}

/// UNREAD. Before anything is read every part says so and no figure reads 0: Space now's bar
/// is a hollow outline over no fill and its words "not read yet", the three weeks are "not
/// read yet", and Held by, What aterm did and Next pass are not read yet (an unread journal is
/// not an empty one, nor an unread watch an off one); disk facts whose journal the host has not
/// read yet still say it is not read. CONTROLS: the fixture's bar draws the used fill in the
/// machine's ink, which the unread one lacks, and its texts carry figures; disk facts read with
/// the watch off and the journal read empty say the watch is off and the journal holds nothing.
#[test]
fn a_band_with_nothing_read_draws_every_figure_unread() {
    let unread = Snapshot::default();
    let d = doc_of(&unread);
    assert_eq!(d.space.bar, None);
    assert_eq!(d.space.words, ["not read yet"]);
    assert!(d.weeks.trace.is_none());
    assert_eq!(
        d.held.none,
        Some(("Held by".to_owned(), "not read yet".to_owned()))
    );
    assert_eq!(
        d.next.forms,
        [("Next pass".to_owned(), "not read yet".to_owned())]
    );
    assert_eq!(d.next.said, "Next pass: not read yet.");
    assert_eq!(
        d.did.forms,
        [("What aterm did".to_owned(), "not read yet".to_owned())]
    );
    assert!(d.did.quiet && d.next.quiet);
    let mut off = Snapshot::default();
    off.facts.disk = Some(DiskFacts::default());
    let o = doc_of(&off);
    assert_eq!(
        o.did.forms,
        [("What aterm did".to_owned(), "not read yet".to_owned())],
        "disk facts before the host read the journal: not read"
    );
    assert_eq!(
        o.next.forms[0].1, "no disk watch in this aterm",
        "CONTROL: read, the watch off"
    );
    off.facts.disk = Some(DiskFacts {
        journal_read: true,
        ..DiskFacts::default()
    });
    assert_eq!(
        doc_of(&off).did.forms[0].1,
        "nothing in the last 3 weeks",
        "CONTROL: read, the journal empty"
    );

    let p = painted(&unread, 1400.0);
    let all = texts(&p);
    assert!(all.contains(&"not read yet"), "{all:?}");
    for t in &all {
        assert!(
            !t.split(|c: char| !c.is_ascii_digit()).any(|n| n == "0"),
            "no figure reads 0: {t:?} in {all:?}"
        );
    }
    let inks = input(&unread).inks;
    let machine_fill = |prims: &[DrawPrim]| {
        prims.iter().any(|prim| {
            matches!(prim, DrawPrim::Panel { fill, .. } if *fill == rgba(inks.machine, 0xFF))
        })
    };
    assert!(!machine_fill(&p.prims), "the unread bar has no fill");
    assert!(
        p.prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Stroke { .. })),
        "the unread bar is an outline"
    );
    let read = painted(&custody(), 1400.0);
    assert!(machine_fill(&read.prims), "CONTROL: the read bar is filled");
    assert!(
        texts(&read).iter().any(|t| t.contains("41 GB")),
        "CONTROL: {:?}",
        texts(&read)
    );
}

/// The parts `l` lays, left to right.
fn parts(l: &Laid) -> Vec<Part> {
    l.parts.iter().map(|p| p.part).collect()
}

/// [`custody`] with `n` holdings, largest first, of distinct names.
fn holding_many(n: usize) -> Snapshot {
    with_disk(custody(), |d| {
        d.held = Some(Arc::new(Holdings {
            rows: (0..n)
                .map(|i| {
                    holding(
                        &format!("worktree-{i}"),
                        Worktree::Linked,
                        (Mark::NoSession, Vec::new(), None),
                        (60 - i as u64) * GB,
                        Class::Rebuildable,
                    )
                })
                .collect(),
            walk: Walk::Complete,
            oldest_walk_s: Some(30),
        }));
    })
}

/// SHEDDING. From 3000 pt down to 120 pt, with three, six and nine holdings, the band keeps
/// Space now at every width and sheds the others in one fixed order, each once: the three
/// weeks, Held by, What aterm did, Next pass, and the label last; a part shed at one width is
/// shed at every narrower one. Holdings fall a whole column (two) at a time from the most it
/// lists ([`HELD_MOST`]), down to one column, before any part goes; the volume's total and the
/// long words of What aterm did and Next pass go before the three-week strip; and the parts stay
/// apart and inside the margins. CONTROL: every part at its widest does not fit 600 pt, so the
/// steps are what make it fit.
#[test]
fn the_band_sheds_parts_in_a_fixed_order_as_it_narrows() {
    let m = deck_metrics();
    for (n, snap) in [(3, custody()), (6, gallery_scene()), (9, holding_many(9))] {
        let d = doc_of(&snap);
        let most = n.min(HELD_MOST);
        let wide = lay(&m, &d, 3000.0);
        assert_eq!(
            parts(&wide),
            [Part::Space, Part::Weeks, Part::Did, Part::Held, Part::Next],
            "every part at 3000 pt ({n} holdings)"
        );
        assert_eq!(
            wide.choice.held,
            Some(most),
            "every holding it lists at 3000 pt"
        );

        let mut gone: Vec<Part> = Vec::new();
        let mut held = most;
        let mut label_gone = false;
        let mut weeks_seen = true;
        let mut w = 3000.0;
        while w >= 120.0 {
            let l = lay(&m, &d, w);
            let kept = parts(&l);
            assert_eq!(kept.first(), Some(&Part::Space), "Space now at {w} pt");
            for part in &gone {
                assert!(!kept.contains(part), "{part:?} shed above {w} pt is back");
            }
            for part in [Part::Weeks, Part::Did, Part::Held, Part::Next] {
                if !kept.contains(&part) && !gone.contains(&part) {
                    gone.push(part);
                }
            }
            if let Some(k) = l.choice.held {
                assert!(k <= held, "holdings only fall: {k} after {held} at {w} pt");
                assert!(
                    k == most || (k >= 2 && k % 2 == 0),
                    "holdings fall a whole column at a time: {k} of {most} at {w} pt"
                );
                held = k;
            }
            if weeks_seen && l.choice.weeks.is_none() {
                weeks_seen = false;
                assert!(
                    l.choice.words >= 1 && l.choice.did == Some(1) && l.choice.next == Some(1),
                    "the total and the long words go before the strip: {:?} at {w} pt",
                    l.choice
                );
                assert_eq!(
                    l.choice.held,
                    Some(2),
                    "one column left before the strip goes"
                );
            }
            assert!(
                !(label_gone && l.choice.label),
                "the label is back at {w} pt"
            );
            label_gone |= !l.choice.label;
            if !l.choice.label {
                assert!(gone.len() == 4, "the label goes last: {gone:?} at {w} pt");
            }
            let start = if l.choice.label { PARTS_X } else { LEFT };
            let mut x = start;
            for p in &l.parts {
                assert!(
                    p.x + EPS >= x,
                    "{:?} overlaps the part before it at {w} pt",
                    p.part
                );
                assert!(p.w >= 0.0);
                x = p.x + p.w + GAP;
            }
            let end = l.parts.last().map_or(start, |p| p.x + p.w);
            assert!(
                end <= (w - RIGHT).max(start) + EPS,
                "inside the margin at {w} pt"
            );
            w -= 10.0;
        }
        assert_eq!(
            gone,
            [Part::Weeks, Part::Held, Part::Did, Part::Next],
            "the fixed order ({n} holdings)"
        );
        assert!(label_gone, "the label goes at the narrowest");
    }

    // CONTROL: every part at its widest is too wide for 600 pt.
    let d = doc_of(&custody());
    let widest = Choice::widest(&d);
    let ws = widths(&m, &d, &widest);
    let need: f64 = ws.iter().map(|(_, w)| w).sum::<f64>() + GAP * (ws.len() - 1) as f64;
    assert!(need > 600.0 - RIGHT - PARTS_X, "CONTROL: {need}");
    assert!(
        parts(&lay(&m, &d, 600.0)).len() < 5,
        "CONTROL: 600 pt sheds"
    );
}

/// Whether two text boxes cross.
fn cross(a: &super::super::TextBox, b: &super::super::TextBox) -> bool {
    a.r[0] < b.r[2] - EPS && b.r[0] < a.r[2] - EPS && a.r[1] < b.r[3] - EPS && b.r[1] < a.r[3] - EPS
}

/// Each mark `p`, `w` pt wide, drew other than text, the clip, the band's ground and its top
/// rule, as its left and right edges in pt at `scale`.
fn marks(p: &Painted, w: f64, scale: f64) -> Vec<(f64, f64)> {
    let pt = |x: f32| f64::from(x) / scale;
    p.prims
        .iter()
        .filter_map(|prim| match *prim {
            DrawPrim::Panel { x, w, .. }
            | DrawPrim::Stroke { x, w, .. }
            | DrawPrim::Capsule { x, w, .. }
            | DrawPrim::Dashed { x, w, .. } => Some((pt(x), pt(x + w))),
            DrawPrim::Line { x1, x2, .. } => Some((pt(x1.min(x2)), pt(x1.max(x2)))),
            DrawPrim::Dot { cx, r, .. } => Some((pt(cx - r), pt(cx + r))),
            DrawPrim::Ring { cx, r_outer, .. } => Some((pt(cx - r_outer), pt(cx + r_outer))),
            _ => None,
        })
        // The ground and the top rule span the band; nothing else does.
        .filter(|&(a, b)| !(a <= EPS && b + 1.0 >= w))
        .collect()
}

/// What of `p`, `w` pt wide at `scale`, lies outside where it belongs: a text off its part (or
/// the label's column), past the margin or out of the band's height, two texts that cross, and a
/// mark off its part's span widened by half the room between parts (the cap's caret and the
/// removal ticks reach a little past a bar or strip).
fn misplaced(p: &Painted, w: f64, scale: f64) -> Vec<String> {
    let mut out = Vec::new();
    let span = |a: f64, b: f64, pad: f64| {
        (p.laid.choice.label && a + EPS >= LEFT && b <= PARTS_X + EPS)
            || p.laid
                .parts
                .iter()
                .any(|part| a + EPS >= part.x - pad && b <= part.x + part.w + pad + EPS)
    };
    for (i, a) in p.boxes.iter().enumerate() {
        if !(a.r[1] >= -EPS && a.r[3] <= HEIGHT_PT + EPS) {
            out.push(format!("{:?} out of the band's height", a.text));
        }
        if a.r[2] > w - RIGHT + EPS {
            out.push(format!("{:?} past the margin", a.text));
        }
        if !span(a.r[0], a.r[2], 0.0) {
            out.push(format!("{:?} off its part", a.text));
        }
        for b in &p.boxes[i + 1..] {
            if cross(a, b) {
                out.push(format!("{:?} crosses {:?}", a.text, b.text));
            }
        }
    }
    for (a, b) in marks(p, w, scale) {
        if !span(a, b, GAP / 2.0) {
            out.push(format!("a mark at {a:.1}..{b:.1} pt off its part"));
        }
    }
    out
}

/// NO OVERLAP. At every width from 3000 pt to 120 pt, at 1× and 2×, read (three and six
/// holdings) and unread, no text the band draws crosses another, each lies within the band's
/// height, within its part (or the label's column) and inside the right margin, and every mark
/// (bars, ticks, the caret, the strip, the session marks) lies on its part's span. CONTROLS: the
/// checker finds a part's marks and texts once that part is laid elsewhere, and a text moved
/// onto its neighbour.
#[test]
fn no_text_crosses_another_or_leaves_its_part_at_any_width() {
    for snap in [custody(), gallery_scene(), Snapshot::default()] {
        for scale in [1.0_f32, 2.0] {
            let mut w = 3000.0;
            while w >= 120.0 {
                let p = painted_at(&snap, w, scale);
                let wrong = misplaced(&p, w, f64::from(scale));
                assert!(
                    wrong.is_empty(),
                    "at {w} pt {scale}×: {wrong:?} in {:?}",
                    p.laid
                );
                w -= 10.0;
            }
        }
    }
    // CONTROL: the parts' marks and texts are found off their part once the part's span moves.
    let mut p = painted(&custody(), 1600.0);
    assert!(
        misplaced(&p, 1600.0, f64::from(S)).is_empty(),
        "PRECONDITION"
    );
    for part in &mut p.laid.parts {
        part.x += 40.0;
    }
    let wrong = misplaced(&p, 1600.0, f64::from(S));
    assert!(
        wrong.iter().any(|w| w.starts_with("a mark")),
        "CONTROL: {wrong:?}"
    );
    assert!(
        wrong
            .iter()
            .any(|w| w.ends_with("off its part") && w.starts_with('"')),
        "CONTROL"
    );
    // CONTROL: a text moved onto the one after it is caught.
    let p = painted(&custody(), 1600.0);
    let (a, b) = (&p.boxes[1], &p.boxes[2]);
    let mut moved = b.clone();
    let dx = a.r[0] - moved.r[0];
    let dy = a.r[1] - moved.r[1];
    moved.r = [
        moved.r[0] + dx,
        moved.r[1] + dy,
        moved.r[2] + dx,
        moved.r[3] + dy,
    ];
    assert!(cross(a, &moved), "CONTROL: {a:?} {moved:?}");
}

/// HITS. Each drawn part is a hit over its own span of the band, and the band as a whole is
/// one after them, each the Disk lens on the machine's scope as the Deck's Disk row opens it;
/// a reader meets one meter a drawn part, named after the band. CONTROL: a narrower band draws
/// and says fewer parts.
#[test]
fn each_part_is_a_hit_on_the_disk_lens_and_a_meter() {
    let snap = custody();
    let p = painted(&snap, 1600.0);
    let kinds: Vec<HitKind> = p.hits.iter().map(|h| h.kind).collect();
    assert_eq!(
        kinds,
        [
            HitKind::DiskCustody(Part::Space),
            HitKind::DiskCustody(Part::Weeks),
            HitKind::DiskCustody(Part::Did),
            HitKind::DiskCustody(Part::Held),
            HitKind::DiskCustody(Part::Next),
            HitKind::DiskCustody(Part::Band),
        ]
    );
    for (hit, placed) in p.hits.iter().zip(&p.laid.parts) {
        assert_eq!(hit.instrument(), Instrument::Disk);
        assert_eq!(hit.bar(), Bar::Global);
        assert!((f64::from(hit.rect.0) - placed.x * f64::from(S)).abs() < 1.0);
        assert_eq!(hit.rect.3, height_px(f64::from(S)) as f32);
    }
    let band = p.hits.last().expect("the band's hit");
    assert_eq!(band.rect.2, (1600.0 * S).round(), "the whole width");

    let m = deck_metrics();
    let names: Vec<String> = meters(&input(&snap), &m, ((1600.0 * S), S))
        .into_iter()
        .map(|(name, words)| {
            assert!(!words.is_empty(), "{name} says something");
            name
        })
        .collect();
    assert_eq!(
        names,
        [
            "Disk Custody: space",
            "Disk Custody: used over 3 weeks",
            "Disk Custody: what aterm did",
            "Disk Custody: held by",
            "Disk Custody: next pass",
        ]
    );
    let narrow = meters(&input(&snap), &m, ((400.0 * S), S));
    assert!(narrow.len() < names.len(), "CONTROL: {narrow:?}");
    assert_eq!(
        painted(&snap, 400.0).hits.len(),
        narrow.len() + 1,
        "CONTROL"
    );
}

/// RESERVATION. Where the Deck takes room its reservation is its rows, this band and Network
/// State, each a fixed height at any scale. The compact form takes only its own lane height.
/// CONTROL: the Deck's rows alone are short of it by exactly the two bands.
#[test]
fn the_deck_reserves_the_band_with_its_rows() {
    use crate::stats_deck::{StatsBars, deck_px, reserved_deck_px};
    use crate::stats_plan::DeckForm;
    for scale in [1.0, 2.0, 3.0] {
        for form in [DeckForm::Folded, DeckForm::Full] {
            let rows = deck_px(StatsBars::Deck, form, scale, 34);
            let reserved = reserved_deck_px(StatsBars::Deck, form, scale, 34);
            assert_eq!(
                reserved,
                rows + height_px(scale) + crate::network_deck::height_px(scale),
                "{scale}× {form:?}"
            );
            assert_eq!(height_px(scale), (30.0 * scale) as usize);
            assert!(
                reserved - rows > crate::network_deck::height_px(scale),
                "CONTROL"
            );
        }
        assert_eq!(
            reserved_deck_px(StatsBars::Off, DeckForm::Folded, scale, 34),
            0
        );
        assert_eq!(
            reserved_deck_px(StatsBars::Sill, DeckForm::Folded, scale, 34),
            crate::stats_sill::compact_height(scale)
        );
    }
}

/// THE WINDOWS. The process's first window, which carries the System Resources band, draws
/// this band between the Deck's rows and Network State, its hits inside the card; a later
/// window, which shows the Sill, draws none, nor does any window with the bars off.
#[test]
fn only_the_window_that_carries_the_deck_draws_the_band() {
    let crate::stats_band::tests::TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = crate::stats_band::tests::two_windows();
    app.vitals.snapshot = Some(Arc::new(custody()));
    let disk_hits = |app: &crate::App, wid| -> Vec<crate::stats_paint::Hit> {
        let _ = app.stats_deck_fp(wid);
        app.windows[&wid]
            .stats_hits
            .borrow()
            .iter()
            .filter(|h| matches!(h.kind, HitKind::DiskCustody(_)))
            .copied()
            .collect()
    };
    let f = app.deck_frame(first).expect("the first window's deck");
    let band = disk_hits(&app, first);
    assert!(!band.is_empty(), "the first window draws the band");
    let scale = f64::from(f.geom.scale);
    let network = crate::network_deck::height_px(scale) as f32;
    let top = f.geom.h - network - height_px(scale) as f32;
    for hit in &band {
        assert_eq!(hit.rect.1, top, "under the Deck's rows, over Network State");
    }
    let network_tops: Vec<f32> = app.windows[&first]
        .stats_hits
        .borrow()
        .iter()
        .filter(|h| matches!(h.kind, HitKind::Network(_)))
        .map(|h| h.rect.1)
        .collect();
    assert!(
        network_tops
            .iter()
            .all(|&y| y >= top + height_px(scale) as f32),
        "Network State under the band: {network_tops:?}"
    );
    assert!(
        disk_hits(&app, second).is_empty(),
        "the Sill window draws none"
    );

    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    assert!(app.deck_frame(first).is_none(), "off: no bars, no band");
}

/// The Light scene ([`Owner::Light`]) with the disk custody a busy Mac has, every figure agreeing
/// with the scene's own Disk row: its 2 TB volume 1.309 TB used; six holdings (151 GB
/// rebuildable, 123 GB costly, 53 GB kept); three removals (41 GB two days ago, 29 GB five hours
/// ago and 12 GB fourteen minutes ago, the last two the 41 GB freed today), each a drop in three
/// weeks of hourly free space that otherwise fills by 82 GB; and the budget's unwritten cap, 80%
/// of the volume, with the rebuildable 151 GB under its 442 GB target.
///
/// [`Owner::Light`]: crate::stats_deck::paint_tests::owner_scenes::Owner::Light
fn gallery_scene() -> Snapshot {
    use crate::stats_deck::paint_tests::owner_scenes::{Owner, owner_scene};
    let mut s = owner_scene(Owner::Light);
    let total = 2000 * GB;
    let used = 1309 * GB;
    let rows = vec![
        holding(
            "trust",
            Worktree::Build,
            (Mark::Gone, Vec::new(), None),
            123 * GB,
            Class::Costly,
        ),
        holding(
            "aterm-s595dac",
            Worktree::Linked,
            (Mark::AtWork, vec![7], Some("claude")),
            76 * GB,
            Class::Rebuildable,
        ),
        holding(
            "aterm-tjd3qd42nw4-b",
            Worktree::Linked,
            (Mark::AtWork, vec![9], Some("targo")),
            58 * GB,
            Class::Rebuildable,
        ),
        holding(
            "proofs",
            Worktree::Folder,
            (Mark::NoSession, Vec::new(), None),
            31 * GB,
            Class::Kept,
        ),
        holding(
            "publication",
            Worktree::Checkout,
            (Mark::NoSession, Vec::new(), None),
            22 * GB,
            Class::Kept,
        ),
        holding(
            "trust-cg-tjd3qd42nw4",
            Worktree::Linked,
            (Mark::Gone, Vec::new(), None),
            17 * GB,
            Class::Rebuildable,
        ),
    ];
    let removals = [
        (AT - 48 * HOUR_MS, 41 * GB),
        (AT - 5 * HOUR_MS, 29 * GB),
        (AT - 14 * MIN_MS, 12 * GB),
    ];
    let removed = |ms: i64| -> u64 {
        removals
            .iter()
            .filter(|(at, _)| *at <= ms)
            .map(|(_, b)| b)
            .sum()
    };
    let deed = |at, bytes, what: &str| DiskDeed {
        at_unix_ms: at,
        n: 1,
        kind: DeedKind::Removed(what.into()),
        by: DeedBy::Aterm,
        bytes: Some(bytes),
        free: None,
    };
    let rebuildable = 151 * GB;
    let cap = total / 5 * 4;
    let kept = used - rebuildable;
    let mut disk = s.facts.disk.clone().expect("the scene's disk facts");
    disk.freed_today = Some((41 * GB, AT - 14 * MIN_MS, "build caches".into()));
    disk.deeds = vec![
        deed(AT - 48 * HOUR_MS, 41 * GB, "stale build output"),
        deed(AT - 5 * HOUR_MS, 29 * GB, "stale build output"),
        DiskDeed {
            at_unix_ms: AT - 3 * HOUR_MS,
            n: 1,
            kind: DeedKind::Checked,
            by: DeedBy::Aterm,
            bytes: None,
            free: Some(total - used - 12 * GB),
        },
        deed(AT - 14 * MIN_MS, 12 * GB, "build caches"),
    ];
    disk.removals = removals.to_vec();
    disk.held = Some(Arc::new(Holdings {
        rows,
        walk: Walk::Complete,
        oldest_walk_s: Some(30),
    }));
    disk.held_age_s = Some(5);
    disk.journal_read = true;
    disk.cleanup = Some(true);
    disk.budget = BudgetRead::Sized(BudgetFacts {
        cap,
        cap_written: false,
        kept,
        target: cap - kept,
        rebuildable,
        partial: false,
        sized_s: 60,
    });
    s.facts.disk = Some(disk);
    s.volume = Some(VolumeSample {
        total,
        free: total - used,
        reserved: 0,
        mount: "/".into(),
    });
    // Three weeks of hourly free space: filling evenly by 82 GB to what it would be with
    // nothing removed, less each removal from its hour on; the hour now read last.
    let now_k = AT.div_euclid(HOUR_MS);
    let first = now_k + 1 - WEEK_BUCKETS as i64;
    let weeks = (now_k - first) as u64;
    let filled = |k: i64| used + 82 * GB - 82 * GB * (now_k - k) as u64 / weeks.max(1);
    for k in first..=now_k {
        let at = k * HOUR_MS + 1;
        s.free_weeks.note(
            aterm_vitals::ring::Taken::at(at, 1_000),
            total - filled(k) + removed(at),
        );
    }
    s.free_weeks.note(
        aterm_vitals::ring::Taken::at(AT - 1_000, 1_000),
        total - used,
    );
    s
}

/// GALLERY. The whole System Resources band (the Deck's rows, this band and Network State) as a
/// window draws it, at 1400, 900 and 520 pt, 2×, dark and light, read ([`gallery_scene`]) and
/// with nothing read: each draws this band's parts as hits between the Deck's rows and Network
/// State, every part at 1400 pt read. The frames a person looks at are written only where
/// `$ATERM_DISK_BAND_GALLERY` names a folder: rasterising them is a look, not a check.
#[test]
fn disk_band_gallery() {
    use crate::stats_deck::paint_tests::light_inks;
    use crate::stats_deck::paint_tests::owner_scenes::{owner_inks, owner_input};
    use crate::stats_deck::{StatsBars, deck_paint, reserved_deck_px};
    use crate::stats_plan::DeckForm;
    let dir = std::env::var_os("ATERM_DISK_BAND_GALLERY").map(std::path::PathBuf::from);
    if dir.is_some() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    }
    let net = aterm_types::network_state::NetworkStateSnapshot::default();
    let scenes = [("read", gallery_scene()), ("unread", Snapshot::default())];
    let mut written = 0;
    for (name, snap) in &scenes {
        for w in [1400.0_f64, 900.0, 520.0] {
            for (theme, inks) in [("dark", owner_inks()), ("light", light_inks())] {
                let s = 2.0;
                let input = DeckInput {
                    inks,
                    form: DeckForm::Folded,
                    network: Some(&net),
                    ..owner_input(snap)
                };
                let h = reserved_deck_px(StatsBars::Deck, DeckForm::Folded, s, (16.0 * s) as usize);
                let g = DeckGeom {
                    w: (w * s) as f32,
                    h: h as f32,
                    pad: (12.0 * s) as f32,
                    scale: s as f32,
                    font_px: (13.0 * s) as f32,
                };
                let (prims, hits) = deck_paint(&g, &input);
                let band: Vec<Part> = hits
                    .iter()
                    .filter_map(|h| match h.kind {
                        HitKind::DiskCustody(part) => Some(part),
                        _ => None,
                    })
                    .collect();
                assert_eq!(band.last(), Some(&Part::Band), "{name} {w} pt: {band:?}");
                assert!(band.contains(&Part::Space), "{name} {w} pt: {band:?}");
                if *name == "read" && w >= 1400.0 {
                    assert_eq!(
                        band,
                        [
                            Part::Space,
                            Part::Weeks,
                            Part::Did,
                            Part::Held,
                            Part::Next,
                            Part::Band
                        ],
                        "every part at 1400 pt"
                    );
                }
                let Some(dir) = &dir else {
                    written += 1;
                    continue;
                };
                let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
                    &prims,
                    g.w as u32,
                    g.h as u32,
                    1.0,
                    [0, 0, 0, 0],
                );
                let png = crate::app_introspect::encode_rgba8_png(&rgba, pw, ph)
                    .expect("the frame encodes");
                let file = dir.join(format!("band_{name}_{w}pt_2x_{theme}.png"));
                std::fs::write(&file, png).expect("the PNG writes");
                written += 1;
            }
        }
    }
    assert_eq!(written, 12);
}
