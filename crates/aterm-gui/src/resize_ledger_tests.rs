// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeMap;

use aterm_models_render::resize_render_model;
use aterm_spec::interp;

use super::*;

const LIFE: u64 = 7;

fn report(ordinal: u64, from: u16, to: u16, alt: bool, seq: (u64, u64)) -> ResizeReport {
    ResizeReport {
        ordinal,
        from: (from, 80),
        to: (to, 80),
        alt,
        content_seq_before: seq.0,
        content_seq: seq.1,
        ..ResizeReport::default()
    }
}

fn copy_of(reports: &[ResizeReport], probe: EngineProbe) -> JournalCopy {
    JournalCopy {
        reports: reports.to_vec(),
        probe,
    }
}

#[track_caller]
fn here() -> Attribution {
    Attribution {
        site: Site::caller(Cause::Pass, Location::caller()),
        win: Some((80, 24)),
        chrome: Some(1),
    }
}

fn alt_flap(first_ordinal: u64, seq: u64) -> [ResizeReport; 2] {
    let mut shrink = report(first_ordinal, 24, 23, true, (seq, seq + 1));
    shrink.demoted = 1;
    let mut grow = report(first_ordinal + 1, 23, 24, true, (seq + 1, seq + 2));
    grow.appended = 1;
    [shrink, grow]
}

fn probe(ordinal: u64, content_seq: u64, full_clears: u64) -> EngineProbe {
    EngineProbe {
        lifetime: LIFE,
        ordinal,
        content_seq,
        full_clears,
        screen_replaced: 0,
        debt: aterm_core::terminal::RenderDebt::default(),
        geom: (24, 80),
        main_screen: false,
    }
}

fn run_verdict(ledger: &ResizeLedger, id: u64) -> Verdict {
    ledger
        .runs
        .iter()
        .find(|run| run.id == id)
        .expect("run")
        .verdict
}

#[test]
fn grouping_joins_adjoining_resizes_within_the_gap() {
    let mut ledger = ResizeLedger::default();
    let [shrink, grow] = alt_flap(1, 10);
    ledger.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
    ledger.ingest(
        &copy_of(&[shrink, grow], probe(2, 12, 0)),
        Some(here()),
        1_000,
    );
    assert_eq!((ledger.high_row, ledger.high_run, ledger.flaps), (2, 1, 1));
    assert!(ledger.runs[0].net_zero());

    let mut apart = ResizeLedger::default();
    let mut trim = shrink;
    (trim.demoted, trim.trimmed) = (0, 1);
    apart.ingest(&copy_of(&[trim], probe(1, 11, 0)), Some(here()), 1_000);
    apart.ingest(
        &copy_of(&[trim, grow], probe(2, 12, 0)),
        Some(here()),
        1_300,
    );
    assert_eq!((apart.high_run, apart.flaps), (2, 0));

    let mut drawn = ResizeLedger::default();
    drawn.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
    let mut grow_after_draw = report(2, 23, 24, true, (15, 16));
    grow_after_draw.appended = 1;
    drawn.ingest(
        &copy_of(&[shrink, grow_after_draw], probe(2, 16, 0)),
        Some(here()),
        1_005,
    );
    assert_eq!((drawn.high_run, drawn.flaps), (1, 1));
    assert!(drawn.runs[0].drew, "the run saw the draw");
    assert_eq!(drawn.evaluate(probe(2, 16, 0), 2_005).len(), 1, "a risk");

    let mut cleared = ResizeLedger::default();
    cleared.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
    let mut grow_after_clear = grow_after_draw;
    grow_after_clear.full_clears = 1;
    cleared.ingest(
        &copy_of(&[shrink, grow_after_clear], probe(2, 16, 1)),
        Some(here()),
        1_005,
    );
    assert_eq!((cleared.high_run, cleared.flaps), (2, 0));
    assert_eq!(run_verdict(&cleared, 1), Verdict::Healed);
    assert_eq!(cleared.render, Render::Ok);

    let mut trimmed = ResizeLedger::default();
    let mut trim = report(1, 24, 23, true, (10, 11));
    trim.trimmed = 1;
    trimmed.ingest(&copy_of(&[trim], probe(1, 11, 0)), Some(here()), 1_000);
    trimmed.ingest(
        &copy_of(&[trim, report(2, 23, 24, true, (15, 16))], probe(2, 16, 0)),
        Some(here()),
        1_005,
    );
    assert_eq!((trimmed.high_run, trimmed.flaps), (2, 0));

    let mut held = ResizeLedger::default();
    let [mut shrink, mut back] = alt_flap(1, 10);
    shrink.stashed = 1;
    (back.appended, back.revealed, back.restored_top) = (0, 1, 1);
    held.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
    held.evaluate(probe(1, 11, 0), 1_300);
    assert_eq!(held.render, Render::Displaced, "meanwhile");
    held.ingest(
        &copy_of(&[shrink, back], probe(2, 12, 0)),
        Some(here()),
        1_600,
    );
    assert_eq!((held.high_run, held.flaps), (1, 1));
    assert_eq!(run_verdict(&held, 1), Verdict::None);
    assert_eq!(held.render, Render::Ok);

    let mut after = ResizeLedger::default();
    let mut shrink = report(1, 24, 23, true, (10, 11));
    shrink.demoted = 1;
    let mut grow = report(2, 23, 24, true, (15, 16));
    grow.appended = 1;
    let mut again = report(3, 24, 23, true, (16, 17));
    again.demoted = 1;
    after.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
    after.ingest(
        &copy_of(&[shrink, grow], probe(2, 16, 0)),
        Some(here()),
        1_005,
    );
    assert!(again.adjoins(&grow), "nothing drawn since the grow");
    after.ingest(
        &copy_of(&[shrink, grow, again], probe(3, 17, 0)),
        Some(here()),
        1_405,
    );
    assert_eq!((after.high_run, after.flaps), (2, 1));
    after.evaluate(probe(3, 17, 0), 2_100);
    assert_eq!(run_verdict(&after, 1), Verdict::DesyncRisk);

    let mut drag = ResizeLedger::default();
    let mut down1 = report(1, 24, 23, true, (10, 11));
    down1.demoted = 1;
    let mut down2 = report(2, 23, 22, true, (15, 16));
    down2.demoted = 1;
    let mut up = report(3, 22, 23, true, (20, 21));
    up.appended = 1;
    drag.ingest(&copy_of(&[down1], probe(1, 11, 0)), Some(here()), 1_000);
    drag.ingest(
        &copy_of(&[down1, down2], probe(2, 16, 0)),
        Some(here()),
        1_020,
    );
    assert_eq!(drag.high_run, 2, "23 -> 22 does not take 24 -> 23 back");
    drag.ingest(
        &copy_of(&[down1, down2, up], probe(3, 21, 0)),
        Some(here()),
        1_040,
    );
    assert_eq!((drag.high_run, drag.flaps), (2, 1), "the turn joins");

    let mut found = ResizeLedger::default();
    let mut shrink = report(1, 24, 23, true, (10, 11));
    shrink.demoted = 1;
    let mut grow = report(2, 23, 24, true, (15, 16));
    grow.appended = 1;
    found.ingest(&copy_of(&[shrink, grow], probe(2, 16, 0)), None, 9_000);
    assert_eq!((found.high_run, found.unledgered), (2, 2));
    assert!(found.rows.iter().all(|row| !row.timed));
    let mut half = ResizeLedger::default();
    half.ingest(
        &copy_of(&[shrink, grow], probe(2, 16, 0)),
        Some(here()),
        9_000,
    );
    assert_eq!((half.high_run, half.unledgered), (2, 1));
    assert!(half.rows[1].timed && !half.rows[0].timed);
    let mut quiet = ResizeLedger::default();
    let [shrink, grow] = alt_flap(1, 10);
    quiet.ingest(&copy_of(&[shrink, grow], probe(2, 12, 0)), None, 9_000);
    assert_eq!((quiet.high_run, quiet.flaps), (1, 1));
}

#[test]
fn a_grow_hands_rows_back_to_the_run_that_stashed_them() {
    let shrink = |ordinal, from, to, seq: u64| {
        let mut r = report(ordinal, from, to, true, (seq, seq + 1));
        (r.demoted, r.stashed) = (1, 1);
        r
    };
    let grow = |ordinal, from, to, seq: u64, restored: bool| {
        let mut r = report(ordinal, from, to, true, (seq, seq + 1));
        if restored {
            (r.revealed, r.restored_top) = (1, 1);
        } else {
            r.appended = 1;
        }
        r
    };
    let book = |ledger: &mut ResizeLedger, reports: &[ResizeReport], at: u64| {
        let last = reports.last().expect("a report");
        ledger.ingest(
            &copy_of(reports, probe(last.ordinal, last.content_seq, 0)),
            Some(here()),
            at,
        );
    };
    let journal = [
        shrink(1, 24, 23, 10),
        shrink(2, 23, 22, 11),
        grow(3, 22, 23, 12, true),
        grow(4, 23, 24, 13, true),
    ];

    for (label, times) in [
        ("apart", [0, 300, 600, 900]),
        ("joined", [0, 300, 301, 302]),
    ] {
        let mut ledger = ResizeLedger::default();
        for (i, at) in times.into_iter().enumerate() {
            book(&mut ledger, &journal[..=i], at);
        }
        assert_eq!((ledger.high_run, ledger.flaps), (1, 1), "{label}");
        let run = &ledger.runs[0];
        assert_eq!(
            (run.displaced, run.held, run.verdict),
            (0, (0, 0), Verdict::None),
            "{label}"
        );
        assert_eq!(ledger.evaluate(probe(4, 40, 0), 5_000), vec![], "{label}");
        assert_eq!(ledger.render(), Render::Ok, "{label}");
    }

    let mut dropped = ResizeLedger::default();
    let mut control = journal;
    control[3] = grow(4, 23, 24, 13, false);
    for (i, at) in [0, 300, 600, 900].into_iter().enumerate() {
        book(&mut dropped, &control[..=i], at);
    }
    assert_eq!((dropped.high_run, dropped.runs[0].displaced), (1, -1));
    assert_eq!(dropped.evaluate(probe(4, 40, 0), 5_000).len(), 1, "a risk");
    assert_eq!(run_verdict(&dropped, 1), Verdict::DesyncRisk);
    assert_eq!(dropped.render(), Render::DesyncRisk);

    let mut other = ResizeLedger::default();
    book(&mut other, &journal[..1], 0);
    assert_eq!(other.runs[0].held, (1, 0));
    let back = grow(2, 23, 24, 11, true);
    assert_eq!(other.credit(&back, None), (1, 0), "credited elsewhere");
    assert_eq!(
        (
            other.runs[0].displaced,
            other.runs[0].held,
            other.runs[0].verdict
        ),
        (0, (0, 0), Verdict::None)
    );
    assert_eq!(own_shift(&back, (1, 0)), 0, "the grow's run counts nothing");
    let mut joining = ResizeLedger::default();
    book(&mut joining, &journal[..1], 0);
    assert_eq!(joining.credit(&back, Some(1)), (0, 0));
    assert_eq!(
        (joining.runs[0].displaced, joining.runs[0].held),
        (-1, (0, 0))
    );
}

#[test]
fn a_lost_report_credits_no_older_run() {
    let mut ledger = ResizeLedger::default();
    let mut first = report(1, 24, 23, true, (10, 11));
    (first.demoted, first.stashed) = (1, 1);
    ledger.ingest(&copy_of(&[first], probe(1, 11, 0)), Some(here()), 0);
    assert_eq!(ledger.runs[0].held, (1, 0));
    let mut lost = report(2, 23, 22, true, (11, 12));
    (lost.demoted, lost.stashed) = (1, 1);
    let mut grow = report(3, 22, 23, true, (12, 13));
    (grow.revealed, grow.restored_top) = (1, 1);
    let journal = [first, lost, grow];
    ledger.ingest(&copy_of(&journal[2..], probe(3, 13, 0)), None, 1_000);
    assert_eq!(ledger.lost, 1);
    let run1 = ledger.runs.iter().find(|run| run.id == 1).expect("run 1");
    assert_eq!((run1.displaced, run1.held), (-1, (0, 0)), "no credit");
    assert_eq!(ledger.evaluate(probe(3, 40, 0), 5_000), vec![]);
    assert_eq!(run_verdict(&ledger, 1), Verdict::Unverified);
    assert_eq!(ledger.render(), Render::Unverified);
}

#[test]
fn verdict_table() {
    let mut trim = ResizeLedger::default();
    let mut shrink = report(1, 24, 23, true, (10, 11));
    shrink.trimmed = 1;
    trim.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 0);
    assert_eq!(run_verdict(&trim, 1), Verdict::None);
    trim.evaluate(probe(1, 40, 0), 5_000);
    assert_eq!(trim.render(), Render::Ok);

    let [mut shrink, mut grow] = alt_flap(1, 10);
    shrink.stashed = 1;
    (grow.appended, grow.revealed, grow.restored_top) = (0, 1, 1);
    let mut undone = ResizeLedger::default();
    undone.ingest(&copy_of(&[shrink, grow], probe(2, 12, 0)), Some(here()), 0);
    assert_eq!(run_verdict(&undone, 1), Verdict::None);
    assert_eq!(undone.evaluate(probe(2, 20, 0), 5_000), vec![]);
    assert_eq!(undone.render(), Render::Ok);
    let wire = undone.reply(0, None);
    assert!(
        wire.contains("revealed=1 appended=0 restored=1 shift=+1"),
        "{wire}"
    );
    assert!(
        wire.contains("net=zero displaced=+0 verdict=none"),
        "{wire}"
    );
    let mut pushed = report(1, 24, 23, true, (10, 11));
    (pushed.pushed, pushed.stashed) = (1, 1);
    let mut back = report(2, 23, 24, true, (11, 12));
    back.restored_bottom = 1;
    let mut corner = ResizeLedger::default();
    corner.ingest(&copy_of(&[pushed, back], probe(2, 12, 0)), Some(here()), 0);
    assert_eq!(run_verdict(&corner, 1), Verdict::None);

    let flap = alt_flap(1, 10);
    let mut healed = ResizeLedger::default();
    healed.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 0);
    assert_eq!(healed.evaluate(probe(2, 12, 0), 10), vec![]);
    assert_eq!(
        healed.render(),
        Render::Displaced,
        "silent: displaced, not drawn"
    );
    assert_eq!(healed.evaluate(probe(2, 13, 1), 15), vec![]);
    assert_eq!(run_verdict(&healed, 1), Verdict::Healed);
    assert_eq!(healed.render(), Render::Ok);

    let mut risk = ResizeLedger::default();
    risk.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 100);
    assert_eq!(
        risk.evaluate(probe(2, 13, 0), 105),
        vec![],
        "drawn, but inside the grace"
    );
    assert_eq!(risk.render(), Render::Displaced);
    assert_eq!(
        risk.evaluate(probe(2, 13, 0), 1_100),
        vec![Transition::Risk {
            run: 1,
            displaced: -1,
            net_zero: true,
            at_ms: 100
        }]
    );
    assert_eq!(risk.render(), Render::DesyncRisk);
    assert_eq!(
        risk.evaluate(probe(2, 14, 0), 2_000),
        vec![],
        "recorded once"
    );
    assert_eq!(
        risk.evaluate(probe(2, 15, 1), 2_500),
        vec![Transition::Healed {
            run: 1,
            after_ms: 2_400
        }]
    );
    assert_eq!(risk.render(), Render::Ok);
    assert_eq!(risk.risks, 1);
    let wire = risk.reply(0, None);
    assert!(
        wire.contains("verdict=healed first_out_ms=5 healed_ms=2400"),
        "{wire}"
    );

    let mut silent = ResizeLedger::default();
    silent.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 0);
    silent.evaluate(probe(2, 12, 0), 60_000);
    assert_eq!(run_verdict(&silent, 1), Verdict::Silent);
    assert_eq!(silent.render(), Render::Displaced);

    let mut changed = ResizeLedger::default();
    let mut down = report(1, 24, 22, true, (10, 11));
    down.demoted = 2;
    changed.ingest(&copy_of(&[down], probe(1, 11, 0)), Some(here()), 0);
    assert_eq!(changed.evaluate(probe(1, 11, 0), 5_000), vec![]);
    assert_eq!(changed.render(), Render::Displaced, "not drawn yet");
    assert_eq!(changed.evaluate(probe(1, 30, 0), 6_000), vec![]);
    assert_eq!(run_verdict(&changed, 1), Verdict::Unverified);
    assert_eq!(changed.render(), Render::Unverified);
    assert_eq!(changed.risks, 0);
    let wire = changed.reply(0, Some(1));
    assert!(
        wire.contains("render=unverified")
            && wire.contains("net=changed displaced=-2 verdict=unverified"),
        "the run render= names is listed whatever the selection: {wire}"
    );
    assert_eq!(changed.evaluate(probe(1, 31, 1), 7_000), vec![]);
    assert_eq!(run_verdict(&changed, 1), Verdict::Healed);
    assert_eq!(changed.render(), Render::Ok);

    let mut reflow = ResizeLedger::default();
    let mut wide = report(1, 24, 24, false, (10, 12));
    wide.to = (24, 100);
    wide.reflowed = true;
    wide.demoted = 3;
    reflow.ingest(&copy_of(&[wide], probe(1, 12, 0)), Some(here()), 0);
    assert_eq!(run_verdict(&reflow, 1), Verdict::Reflow);
    reflow.evaluate(probe(1, 20, 0), 5_000);
    assert_eq!(reflow.render(), Render::Ok);

    let mut primary = ResizeLedger::default();
    let mut down = report(1, 24, 23, false, (10, 11));
    down.demoted = 1;
    primary.ingest(&copy_of(&[down], probe(1, 11, 0)), Some(here()), 0);
    primary.evaluate(probe(1, 30, 0), 5_000);
    assert_eq!(run_verdict(&primary, 1), Verdict::None);
    assert_eq!(primary.render(), Render::Ok);
}

#[test]
fn a_later_resize_is_not_the_app_drawing() {
    let mut ledger = ResizeLedger::default();
    let flap = alt_flap(1, 10);
    ledger.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 0);
    let mut third = report(3, 24, 23, true, (12, 13));
    third.trimmed = 1;
    ledger.ingest(
        &copy_of(&[flap[0], flap[1], third], probe(3, 13, 0)),
        Some(here()),
        300,
    );
    ledger.evaluate(probe(3, 13, 0), 5_000);
    assert!(!ledger.runs[0].drew);
    assert_eq!(run_verdict(&ledger, 1), Verdict::Silent);
}

#[test]
fn a_late_stale_copy_books_nothing_and_keeps_the_counts() {
    let mut ledger = ResizeLedger::default();
    let mut journal: Vec<ResizeReport> = Vec::new();
    for ordinal in 1..=40u64 {
        let r = report(ordinal, 24, 24, true, (ordinal, ordinal + 1));
        journal.push(r);
        let start = journal.len().saturating_sub(16);
        ledger.ingest(
            &copy_of(&journal[start..], probe(ordinal, ordinal + 1, 0)),
            Some(here()),
            ordinal * 1_000,
        );
    }
    let before = ledger.reply(1, None);
    let stale = copy_of(&journal[journal.len() - 16..], probe(40, 41, 0));
    let mut r41 = report(41, 24, 23, true, (41, 42));
    r41.demoted = 1;
    journal.push(r41);
    let fresh = copy_of(&journal[journal.len() - 16..], probe(41, 42, 0));
    ledger.ingest(&fresh, None, 41_000);
    assert_eq!(
        (ledger.high_row, ledger.unledgered, ledger.lost),
        (41, 1, 0)
    );
    let at_41 = (ledger.high_row, ledger.high_run, ledger.flaps, ledger.risks);

    assert_eq!(ledger.ingest(&stale, Some(here()), 41_001), vec![]);
    assert_eq!(
        (ledger.high_row, ledger.high_run, ledger.flaps, ledger.risks),
        at_41
    );
    assert_eq!((ledger.unledgered, ledger.lost), (1, 0));
    assert_eq!(ledger.last_ordinal, 41);
    assert!(!ledger.runs.back().expect("run 41").drew, "no fake draw");
    let after = ledger.reply(1, None);
    assert!(after.starts_with("OK 2 total=41 "), "{before}\n{after}");

    let site = here();
    ledger.ingest(&fresh, Some(site), 41_002);
    assert_eq!((ledger.high_row, ledger.unledgered), (41, 0));
    assert_eq!(ledger.rows.back().expect("row").attribution, site);
}

#[test]
fn a_stale_probe_neither_draws_nor_heals() {
    let mut ledger = ResizeLedger::default();
    let flap = alt_flap(1, 10);
    let stale = probe(0, 10, 0);
    ledger.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 100);
    assert_eq!(ledger.evaluate(stale, 101), vec![]);
    assert!(!ledger.runs[0].drew);
    assert_eq!(ledger.evaluate(probe(2, 12, 0), 1_600), vec![]);
    assert_eq!(run_verdict(&ledger, 1), Verdict::Silent, "never drawn");
    assert_eq!(ledger.render(), Render::Displaced);

    let mut ledger = ResizeLedger::default();
    let mut flap = alt_flap(5, 20);
    for r in &mut flap {
        r.full_clears = 3;
    }
    ledger.ingest(&copy_of(&flap, probe(6, 22, 3)), Some(here()), 100);
    let before_clear = EngineProbe {
        full_clears: 2,
        ..probe(4, 18, 2)
    };
    ledger.evaluate(before_clear, 102);
    assert_eq!(run_verdict(&ledger, 1), Verdict::Silent, "not healed");
    let ahead = EngineProbe {
        full_clears: 3,
        ..probe(7, 30, 3)
    };
    ledger.evaluate(ahead, 1_500);
    assert!(!ledger.runs[0].drew, "an unbooked resize is not a draw");
    let ahead_healed = EngineProbe {
        full_clears: 4,
        ..ahead
    };
    ledger.evaluate(ahead_healed, 1_600);
    assert_eq!(run_verdict(&ledger, 1), Verdict::Healed);
}

#[test]
fn a_restored_engine_rebases_the_open_runs() {
    let mut ledger = ResizeLedger::default();
    let mut flap = alt_flap(1, 100);
    for r in &mut flap {
        r.full_clears = 9;
    }
    ledger.ingest(&copy_of(&flap, probe(2, 102, 9)), Some(here()), 100);
    let reborn = |ordinal, content_seq, full_clears| EngineProbe {
        lifetime: LIFE + 1,
        ..probe(ordinal, content_seq, full_clears)
    };
    let fresh = report(1, 24, 24, true, (4, 5));
    ledger.ingest(&copy_of(&[fresh], reborn(1, 5, 0)), Some(here()), 200);
    assert_eq!((ledger.lifetime, ledger.last_ordinal), (Some(LIFE + 1), 1));
    assert_eq!((ledger.high_row, ledger.lost, ledger.unledgered), (3, 0, 1));
    assert_eq!(run_verdict(&ledger, 1), Verdict::Silent, "carried over");
    ledger.evaluate(probe(2, 500, 9), 2_000);
    assert!(!ledger.runs[0].drew);
    ledger.evaluate(reborn(1, 6, 1), 2_100);
    assert_eq!(run_verdict(&ledger, 1), Verdict::Healed);
    assert!(!ledger.behind(&reborn(1, 6, 1)));
    assert!(ledger.behind(&probe(2, 102, 9)), "another engine");
}

#[test]
fn ring_bounds_backfill_and_loss() {
    let mut ledger = ResizeLedger::default();
    let mut journal: Vec<ResizeReport> = Vec::new();
    for ordinal in 1..=200u64 {
        let r = report(ordinal, 24, 24, true, (ordinal, ordinal + 1));
        journal.push(r);
        let start = journal.len().saturating_sub(16);
        ledger.ingest(
            &copy_of(&journal[start..], probe(ordinal, ordinal + 1, 0)),
            Some(here()),
            ordinal * 1_000,
        );
    }
    assert_eq!(ledger.rows.len(), LEDGER_ROWS_CAP);
    assert_eq!(ledger.high_row, 200);
    assert_eq!(ledger.low_row(), Some(73), "72 evicted");
    assert!(ledger.runs.len() <= LEDGER_RUNS_CAP);
    assert_eq!((ledger.unledgered, ledger.lost), (0, 0));

    let r201 = report(201, 24, 23, true, (202, 203));
    let r202 = report(202, 23, 24, true, (203, 204));
    journal.extend([r201, r202]);
    let start = journal.len() - 16;
    assert!(ledger.behind(&probe(202, 204, 0)));
    ledger.ingest(
        &copy_of(&journal[start..], probe(202, 204, 0)),
        None,
        300_000,
    );
    assert_eq!(
        (ledger.high_row, ledger.unledgered, ledger.lost),
        (202, 2, 0)
    );
    assert!(!ledger.behind(&probe(202, 204, 0)));
    ledger.ingest(
        &copy_of(&journal[start..], probe(202, 204, 0)),
        Some(here()),
        300_001,
    );
    assert_eq!((ledger.high_row, ledger.unledgered), (202, 1));
    let last = ledger.rows.back().expect("row");
    assert!(matches!(last.attribution.site, Site::Caller { .. }));

    for ordinal in 203..=222u64 {
        journal.push(report(ordinal, 24, 24, true, (ordinal + 2, ordinal + 3)));
    }
    let start = journal.len() - 16;
    ledger.ingest(
        &copy_of(&journal[start..], probe(222, 225, 0)),
        None,
        400_000,
    );
    assert_eq!(ledger.lost, 4);
    assert_eq!(ledger.last_ordinal, 222);
}

#[test]
fn wire_goldens() {
    let mut ledger = ResizeLedger::default();
    let [mut shrink, grow] = alt_flap(1, 10);
    shrink.in_sync = true;
    let site: &'static Location<'static> = Location::caller();
    let attribution = Attribution {
        site: Site::caller(Cause::Chrome, site),
        win: Some((135, 63)),
        chrome: Some(2),
    };
    ledger.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(attribution), 7);
    ledger.ingest(
        &copy_of(&[shrink, grow], probe(2, 12, 0)),
        Some(Attribution::CTL_CROSS),
        7,
    );
    let at = format!("{}:{}", site.file(), site.line());
    assert_eq!(
        ledger.reply(0, None),
        format!(
            "OK 3 total=2 runs=1 flaps=1 risks=0 render=displaced unledgered=0 lost=0 nudge=idle nudged=0\n\
             resize 1 t=7 run=1 ord=1 from=80x24 to=80x23 alt=1 sync=1 trimmed=0 demoted=1 \
             pushed=0 revealed=0 appended=0 restored=0 shift=-1 cut=0 site=chrome at={at} win=135x63 chrome=2\n\
             resize 2 t=7 run=1 ord=2 from=80x23 to=80x24 alt=1 sync=0 trimmed=0 demoted=0 \
             pushed=0 revealed=0 appended=1 restored=0 shift=+0 cut=0 site=ctl-cross at=- win=- chrome=-\n\
             run 1 t=7 n=2 alt=1 geom=80x24>80x23>80x24 net=zero displaced=-1 \
             verdict=silent first_out_ms=- healed_ms=-\n"
        )
    );
    assert_eq!(
        ledger
            .reply(1, None)
            .lines()
            .nth(1)
            .map(|l| l.starts_with("resize 2 ")),
        Some(true)
    );
    assert_eq!(
        ledger.reply(0, Some(2)),
        "OK 1 total=2 runs=1 flaps=1 risks=0 render=displaced unledgered=0 lost=0 nudge=idle nudged=0\n\
         run 1 t=7 n=2 alt=1 geom=80x24>80x23>80x24 net=zero displaced=-1 \
         verdict=silent first_out_ms=- healed_ms=-\n"
    );
    assert!(!at.contains(' '), "a site is one token");
    assert_eq!(
        Transition::Risk {
            run: 3,
            displaced: -1,
            net_zero: true,
            at_ms: 40
        }
        .payload(),
        "desync-risk run=3 displaced=-1 net=zero at=40"
    );
    assert_eq!(
        Transition::Healed {
            run: 3,
            after_ms: 900
        }
        .payload(),
        "healed run=3 after_ms=900"
    );

    let mut run = Run::start(1, &report(1, 30, 29, true, (1, 2)), 0, (0, 0));
    for (i, rows) in (20..29u16).rev().enumerate() {
        run.extend(
            &report(2 + i as u64, rows + 1, rows, true, (0, 0)),
            0,
            (0, 0),
        );
    }
    assert_eq!(
        run.geom_chain(),
        "80x30>80x29>80x28>80x27>80x26>80x25>80x24>...>80x20"
    );
}

#[test]
fn the_outermost_entry_point_names_the_site() {
    assert_eq!(SiteScope::current(), None);
    let (outer, outer_line) = (SiteScope::enter(Cause::Chrome), line!());
    {
        let _inner = SiteScope::enter(Cause::Window);
        match SiteScope::current() {
            Some(Site::Caller { cause, at }) => {
                assert_eq!((cause, at.line()), (Cause::Chrome, outer_line));
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(SiteScope::current().is_some(), "the inner exit keeps it");
    drop(outer);
    assert_eq!(SiteScope::current(), None);
    let unwound = std::panic::catch_unwind(|| {
        let _scope = SiteScope::enter(Cause::Input);
        panic!("inside a resize entry point");
    });
    assert!(unwound.is_err());
    assert_eq!(SiteScope::current(), None, "an unwind leaves the scope");
}

#[test]
fn cmd_resizes_reads_the_real_engine_and_refuses_bad_args() {
    let session = crate::stub_session(1);
    let (ctx, term) = (&session.ctx, &session.term);
    {
        let mut t = crate::term_lock(term);
        *t = claude_shaped_alt(8);
        t.resize(7, 20);
        t.resize(8, 20);
    }
    let reply = cmd_resizes(ctx, term, "");
    let mut lines = reply.lines();
    assert_eq!(
        lines.next(),
        Some(
            "OK 3 total=2 runs=1 flaps=1 risks=0 render=ok unledgered=2 lost=0 nudge=idle nudged=0"
        ),
        "{reply}"
    );
    assert!(lines.next().is_some_and(|l| l.contains("demoted=1")
        && l.contains(" ord=1 ")
        && l.contains("site=- at=-")));
    assert!(
        lines
            .next()
            .is_some_and(|l| l.contains("appended=0 restored=1")),
        "the engine undid the quiet flap: {reply}"
    );
    assert!(
        lines
            .next()
            .is_some_and(|l| l.contains("net=zero displaced=+0 verdict=none"))
    );
    assert_eq!(
        cmd_resizes(ctx, term, "since=x"),
        "ERR usage: resizes [<n>] [since=<id>]\n"
    );
    assert_eq!(
        cmd_resizes(ctx, term, "bogus"),
        "ERR usage: resizes [<n>] [since=<id>]\n"
    );
    assert!(cmd_resizes(ctx, term, "since=2").starts_with("OK 0 "));
}

#[test]
fn the_window_pass_books_its_caller_the_grid_and_the_chrome() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let chrome = app.chrome_rows(wid);
    let (shrunk, shrink_line) = (app.apply_term_resize(wid, 23, 80), line!());
    let (grown, grow_line) = (app.apply_term_resize(wid, 24, 80), line!());
    assert!(shrunk && grown);
    assert!(!app.apply_term_resize(wid, 24, 80), "unchanged: no pass");
    app.resize_panes(wid);
    assert_eq!(SiteScope::current(), None, "every entry point left");
    let session = app.pool.get(0).expect("session 0");
    assert_eq!(crate::term_lock(&session.term).resize_ordinal(), 2);
    let reply = session
        .ctx
        .timeline
        .lock()
        .unwrap()
        .resizes()
        .reply(0, None);
    let rows: Vec<&str> = reply.lines().filter(|l| l.starts_with("resize ")).collect();
    assert_eq!(rows.len(), 2, "{reply}");
    let site = |line: u32| format!(" site=term at={}:{line} ", file!());
    assert!(
        rows[0].contains(" from=80x24 to=80x23 ")
            && rows[0].contains(&site(shrink_line))
            && rows[0].ends_with(&format!(" win=80x23 chrome={chrome}")),
        "{reply}"
    );
    assert!(
        rows[1].contains(" from=80x23 to=80x24 ")
            && rows[1].contains(&site(grow_line))
            && rows[1].ends_with(&format!(" win=80x24 chrome={chrome}")),
        "{reply}"
    );
    assert!(
        reply.contains(" unledgered=0 lost=0 nudge=idle nudged=0\n"),
        "{reply}"
    );
}

#[test]
fn a_chrome_regrid_books_site_chrome() {
    let sites = |app: &crate::App| -> Vec<String> {
        let reply = app
            .pool
            .get(0)
            .expect("session 0")
            .ctx
            .timeline
            .lock()
            .unwrap()
            .resizes()
            .reply(0, None);
        reply
            .lines()
            .filter(|l| l.starts_with("resize "))
            .filter_map(|l| l.split(' ').find(|w| w.starts_with("site=")))
            .map(str::to_owned)
            .collect()
    };
    let wid = crate::WindowId(0);
    let mut app = crate::App::headless_for_test();
    app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(1600, 1000));
    assert_eq!(sites(&app), ["site=window"], "the plain window pass");
    {
        let _site = SiteScope::enter(Cause::Chrome);
        app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(1600, 900));
    }
    assert_eq!(SiteScope::current(), None, "the scope left");
    assert_eq!(
        sites(&app),
        ["site=window", "site=chrome"],
        "the chrome pass"
    );
}

#[test]
fn a_changed_size_repaint_without_a_clear_is_unverified_not_a_risk() {
    let mut term = claude_shaped_alt(8);
    let mut ledger = ResizeLedger::default();
    term.resize(6, COLS);
    ledger.ingest(&JournalCopy::take(&term), Some(here()), 1_000);
    let run = ledger.runs.back().expect("run");
    assert_eq!((run.net_zero(), run.displaced), (false, -2));
    let mut repaint = String::from("\x1b[?2026h");
    let frame = frame(6, 1);
    for (r, text) in frame.iter().enumerate() {
        let _ = write!(repaint, "\x1b[{};1H{text}\x1b[K", r + 1);
    }
    repaint.push_str("\x1b[4;3H\x1b[?2026l");
    term.process(repaint.as_bytes());
    let shown: Vec<String> = (0..6)
        .map(|r| term.row_text(r).unwrap_or_default().trim_end().to_string())
        .collect();
    assert_eq!(shown, frame, "the screen is the app's frame");
    let risen = ledger.evaluate(EngineProbe::sample(&term), 2_500);
    assert_eq!(risen, vec![], "no risk recorded");
    assert_eq!(run_verdict(&ledger, 1), Verdict::Unverified);
    assert_eq!(ledger.render(), Render::Unverified);
}

const ROWS: u16 = 8;
const COLS: u16 = 20;
const CAP: i64 = 2;

fn frame(rows: u16, tick: u32) -> Vec<String> {
    (0..rows)
        .map(|r| {
            if r + 1 == rows {
                format!("F{tick}")
            } else {
                format!("L{r}")
            }
        })
        .collect()
}

fn claude_shaped_alt(rows: u16) -> Terminal {
    let mut t = Terminal::new(rows, COLS);
    t.process(b"\x1b[?1049h");
    let mut app = App {
        rows,
        believed: Vec::new(),
        tick: 0,
    };
    app.repaint(&mut t, rows);
    t
}

struct App {
    rows: u16,
    believed: Vec<String>,
    tick: u32,
}

impl App {
    fn repaint(&mut self, t: &mut Terminal, rows: u16) {
        self.rows = rows;
        self.believed = frame(rows, self.tick);
        let mut bytes = String::from("\x1b[?2026h\x1b[2J");
        for (r, text) in self.believed.iter().enumerate() {
            let _ = write!(bytes, "\x1b[{};1H{text}", r + 1);
        }
        let _ = write!(bytes, "\x1b[{};3H\x1b[?2026l", rows - 2);
        t.process(bytes.as_bytes());
    }

    fn diff(&mut self, t: &mut Terminal) {
        self.tick += 1;
        let last = usize::from(self.rows) - 1;
        let text = format!("F{}", self.tick);
        t.process(
            format!(
                "\x1b[?2026h\x1b[{};1H{text}\x1b[K\x1b[{};3H\x1b[?2026l",
                last + 1,
                self.rows - 2
            )
            .as_bytes(),
        );
        self.believed[last] = text;
    }
}

struct Rig {
    term: Terminal,
    app: App,
    ledger: ResizeLedger,
    now_ms: u64,
    drew: bool,
    /// Ground truth: a reader came by past the run gap while the grid
    /// was still one row short and its content displaced, and that
    /// displacement is still there (`blind`).
    blind: bool,
    evaluated: bool,
    /// `render=` at the last Evaluate: 0 ok or displaced, 1 desync-risk,
    /// 2 unverified.
    verdict: i64,
    /// The last evaluation left a run `silent` (`shown`).
    shown: bool,
    /// The displacement when the ledger's open run opened (`d0`).
    d0: i64,
    /// Output that drew nothing landed inside the open run (`out`).
    out: bool,
    /// What the last grow left on the bottom row until the app drew
    /// (`bot`): 1 the appended blank, 2 the row the undo handed back.
    bot: i64,
    /// Stand in for the HISTORICAL engine: drop the resize undo before
    /// every grow, so the grow can only append (the negative control).
    historical: bool,
    /// The presence classifier reads Claude Code (`judged`).
    judged: bool,
    /// The epoch judge's word at the last Evaluate: 0 ok, 1 `desync-risk`,
    /// 2 `unjudged` (`ev`).
    ev: i64,
}

impl Rig {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b[?1049h");
        let mut app = App {
            rows: ROWS,
            believed: Vec::new(),
            tick: 0,
        };
        app.repaint(&mut term, ROWS);
        Self {
            term,
            app,
            ledger: ResizeLedger::default(),
            now_ms: 1_000,
            drew: false,
            blind: false,
            evaluated: false,
            verdict: 0,
            shown: false,
            d0: 0,
            out: false,
            bot: 0,
            historical: false,
            judged: false,
            ev: 0,
        }
    }

    fn displacement(&self) -> i64 {
        let top = self.term.row_text(0).unwrap_or_default();
        let top = top.trim_end();
        (0..=CAP)
            .find(|k| {
                usize::try_from(*k)
                    .ok()
                    .and_then(|k| self.app.believed.get(k))
                    .is_some_and(|row| row == top)
            })
            .unwrap_or_else(|| panic!("row 0 {top:?} is no shift of {:?}", self.app.believed))
    }

    fn open_run(&self, probe: &EngineProbe) -> Option<&Run> {
        let (prev, run) = (self.ledger.rows.back()?, self.ledger.runs.back()?);
        let unhealed = probe.full_clears == prev.report.full_clears
            && probe.screen_replaced == prev.report.screen_replaced;
        let adjoins = prev.report.content_seq == probe.content_seq;
        let silent = run.verdict == Verdict::Silent;
        let in_gap = self.now_ms.saturating_sub(prev.t_ms) <= RUN_GAP_MS;
        (prev.run == run.id
            && run.verdict.joinable()
            && unhealed
            && ((in_gap && (adjoins || silent)) || (silent && !run.drew && adjoins)))
            .then_some(run)
    }

    fn project(&self) -> BTreeMap<&'static str, i64> {
        let probe = EngineProbe::sample(&self.term);
        let open = self.open_run(&probe);
        let open_id = open.map(|run| run.id);
        let closed: Vec<&Run> = self
            .ledger
            .runs
            .iter()
            .filter(|run| {
                Some(run.id) != open_id
                    && run.judged()
                    && !run.healed_by(probe.full_clears, probe.screen_replaced)
            })
            .collect();
        let led: i64 = closed
            .iter()
            .filter(|run| run.net_zero() && run.displaced < 0)
            .map(|run| -i64::from(run.displaced))
            .sum();
        let lr = closed.iter().any(|run| {
            run.net_zero() && run.displaced != 0 && (run.drew || probe.content_seq > run.quiet_seq)
        });
        let lb = closed
            .iter()
            .any(|run| !run.net_zero() && run.displaced < 0);
        let br = closed.iter().any(|run| {
            !run.net_zero() && run.displaced < 0 && (run.drew || probe.content_seq > run.quiet_seq)
        });
        let late = open.is_some()
            && self
                .ledger
                .rows
                .back()
                .is_some_and(|prev| self.now_ms.saturating_sub(prev.t_ms) > RUN_GAP_MS);
        let (ro, rs, rd, rw) = open.map_or((0, 0, 0, false), |run| {
            (
                1,
                i64::from(run.start.0 != ROWS),
                -i64::from(run.displaced),
                run.drew || (run.verdict == Verdict::Silent && probe.content_seq > run.quiet_seq),
            )
        });
        let (shift, pushed) = self.term.render_debt().rows();
        let debt = i64::from(pushed) - i64::from(shift);
        let eg = if debt == 0 {
            0
        } else {
            self.ledger
                .epochs()
                .current()
                .map_or(0, |epoch| i64::from(epoch.start.0 != ROWS))
        };
        BTreeMap::from([
            ("debt", debt),
            ("eg", eg),
            ("judged", i64::from(self.judged)),
            ("ev", self.ev),
            ("geom", i64::from(self.term.rows() != ROWS)),
            ("app_geom", i64::from(self.app.rows != ROWS)),
            ("disp", self.displacement()),
            ("ro", ro),
            ("rs", rs),
            ("rd", rd),
            ("rw", i64::from(rw)),
            ("late", i64::from(late)),
            ("led", led),
            ("lr", i64::from(lr)),
            ("lb", i64::from(lb)),
            ("br", i64::from(br)),
            ("blind", i64::from(self.blind)),
            ("drew", i64::from(self.drew)),
            ("evaluated", i64::from(self.evaluated)),
            ("verdict", self.verdict),
            ("shown", i64::from(self.shown)),
            ("ls", 0),
            (
                "st",
                i64::try_from(self.term.grid().resize_undo_rows()).unwrap_or(i64::MAX),
            ),
            ("d0", self.d0),
            ("bot", self.bot),
            ("out", i64::from(self.out)),
        ])
    }

    fn resize(&mut self, rows: u16) -> ResizeReport {
        if self.open_run(&EngineProbe::sample(&self.term)).is_none() {
            self.d0 = self.displacement();
            self.out = false;
        }
        self.term.resize(rows, COLS);
        let copy = JournalCopy::take(&self.term);
        self.now_ms += 1;
        self.ledger
            .ingest(&copy, Some(here_attribution()), self.now_ms);
        *copy.reports.last().expect("the resize was journalled")
    }

    fn diff(&mut self) {
        if self.displacement() >= 1 {
            self.drew = true;
        }
        if self.open_run(&EngineProbe::sample(&self.term)).is_some() {
            self.out = true;
        }
        self.app.diff(&mut self.term);
        self.bot = 0;
        self.now_ms += 1;
    }

    fn drive(&mut self, step: Step) -> &'static str {
        self.evaluated = false;
        match step {
            Step::Shrink => {
                let report = self.resize(ROWS - 1);
                match (report.demoted, report.trimmed, report.pushed) {
                    (1, 0, 0) => "Demote",
                    (0, 1, 0) => "Trim",
                    other => panic!("a one-row shrink did {other:?}"),
                }
            }
            Step::Grow => {
                if self.historical {
                    self.term.grid_mut().drop_resize_undo();
                }
                let report = self.resize(ROWS);
                self.bot = match (report.revealed, report.restored_top, report.appended) {
                    (0, 0, 1) => 1,
                    (1, 1, 0) => 2,
                    other => panic!("a one-row grow did {other:?}"),
                };
                if self.displacement() == 0 {
                    self.blind = false;
                }
                "Grow"
            }
            Step::Handler => {
                let rows = self.term.rows();
                if rows == self.app.rows {
                    self.diff();
                } else {
                    self.app.repaint(&mut self.term, rows);
                    self.drew = false;
                    self.blind = false;
                    self.bot = 0;
                    self.now_ms += 1;
                }
                "Step"
            }
            Step::Output => {
                self.term.process(b"\x1b[?1000h\x1b[?1000l");
                if self.open_run(&EngineProbe::sample(&self.term)).is_some() {
                    self.out = true;
                }
                self.now_ms += 1;
                "Output"
            }
            Step::Draw => {
                self.diff();
                "Draw"
            }
            Step::Classify => {
                self.judged = true;
                self.ledger.set_agent(true);
                "Classify"
            }
            Step::Evaluate => {
                if self.displacement() >= 1 && self.term.rows() != ROWS {
                    self.blind = true;
                }
                self.now_ms += RISK_AFTER_MS;
                let probe = EngineProbe::sample(&self.term);
                self.ledger.evaluate(probe, self.now_ms);
                self.evaluated = true;
                self.verdict = match self.ledger.run_render() {
                    Render::DesyncRisk => 1,
                    Render::Unverified => 2,
                    Render::Ok | Render::Displaced => 0,
                    Render::Unjudged => panic!("the run judge never says unjudged"),
                };
                self.ev = match self.ledger.epochs().current().map(|e| e.verdict) {
                    Some(EpochVerdict::DesyncRisk) => 1,
                    Some(EpochVerdict::Unjudged) => 2,
                    _ => 0,
                };
                self.shown = self
                    .ledger
                    .runs
                    .iter()
                    .any(|run| run.verdict == Verdict::Silent);
                "Evaluate"
            }
        }
    }
}

#[track_caller]
fn here_attribution() -> Attribution {
    Attribution {
        site: Site::caller(Cause::Term, Location::caller()),
        win: Some((COLS, ROWS)),
        chrome: Some(0),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Shrink,
    Grow,
    Handler,
    Draw,
    Output,
    Classify,
    Evaluate,
}

const STEPS: [Step; 7] = [
    Step::Shrink,
    Step::Grow,
    Step::Handler,
    Step::Draw,
    Step::Output,
    Step::Classify,
    Step::Evaluate,
];

fn conform(
    model: &aterm_spec::derive::Model,
    schedule: &[Step],
    seen: &mut BTreeMap<&'static str, usize>,
) -> Option<BTreeMap<&'static str, i64>> {
    conform_answered(&mut Answers::new(model), schedule, seen)
}

struct Answers<'m> {
    model: &'m aterm_spec::derive::Model,
    fired: BTreeMap<Projected, BTreeMap<&'static str, Projected>>,
    holds: std::collections::BTreeSet<Projected>,
}

type Projected = BTreeMap<&'static str, i64>;

impl<'m> Answers<'m> {
    fn new(model: &'m aterm_spec::derive::Model) -> Self {
        Self {
            model,
            fired: BTreeMap::new(),
            holds: std::collections::BTreeSet::new(),
        }
    }
}

fn conform_answered(
    answers: &mut Answers<'_>,
    schedule: &[Step],
    seen: &mut BTreeMap<&'static str, usize>,
) -> Option<BTreeMap<&'static str, i64>> {
    let model = answers.model;
    let mut rig = Rig::new();
    let mut state = rig.project();
    assert_eq!(
        state,
        model.init_state(),
        "the rig starts where the model does"
    );
    for &step in schedule {
        // The model bounds the displacement at CAP; a shrink past it is
        // outside the box, so the schedule stops there.
        if step == Step::Shrink && state["disp"] >= CAP {
            return None;
        }
        if step == Step::Classify && state["judged"] == 1 {
            return None;
        }
        let action = rig.drive(step);
        let known = answers
            .fired
            .get(&state)
            .and_then(|by_action| by_action.get(action))
            .cloned();
        if known.is_none() {
            assert!(
                model.action_enabled(action, &state),
                "{schedule:?}: real `{action}` from a state the model disables: {state:?}"
            );
        }
        if action == "Grow" && state["late"] == 1 {
            *seen.entry("Grow joins a late run").or_default() += 1;
        }
        let expected = known.unwrap_or_else(|| {
            let mut expected = state.clone();
            assert!(model.fire(action, &mut expected));
            answers
                .fired
                .entry(state.clone())
                .or_default()
                .insert(action, expected.clone());
            expected
        });
        let real = rig.project();
        assert_eq!(real, expected, "{schedule:?}: `{action}` from {state:?}");
        if !answers.holds.contains(&real) {
            for invariant in [
                "RiskIsFlagged",
                "NoSilentPass",
                "NoFalseVerdict",
                "DisplacedIsShown",
                "NoFalseDisplaced",
                "QuietFlapIsIdentity",
                "EpochFlagsTheResidual",
                "EpochNoFalseAlarm",
                "OnlyAJudgedProgramIsAccused",
                "DebtIsTheDisplacement",
                "Bounded",
            ] {
                assert!(
                    model.check_invariant(invariant, &real),
                    "{invariant}: {real:?}"
                );
            }
            answers.holds.insert(real.clone());
        }
        *seen.entry(action).or_default() += 1;
        state = real;
    }
    Some(state)
}

/// What the walk of [`resize_render_conformance`] saw, summed over the
/// schedules one thread replayed.
#[derive(Default)]
struct Tally {
    seen: BTreeMap<&'static str, usize>,
    verdicts: [usize; 3],
    shown: [usize; 2],
    epoch_words: [usize; 3],
    blind: usize,
    late: usize,
    max_disp: i64,
    schedules: usize,
}

impl Tally {
    fn add(&mut self, other: Self) {
        for (action, n) in other.seen {
            *self.seen.entry(action).or_default() += n;
        }
        for (a, b) in self.verdicts.iter_mut().zip(other.verdicts) {
            *a += b;
        }
        for (a, b) in self.shown.iter_mut().zip(other.shown) {
            *a += b;
        }
        for (a, b) in self.epoch_words.iter_mut().zip(other.epoch_words) {
            *a += b;
        }
        self.blind += other.blind;
        self.late += other.late;
        self.max_disp = self.max_disp.max(other.max_disp);
        self.schedules += other.schedules;
    }
}

fn extend(answers: &mut Answers<'_>, prefix: &[Step], t: &mut Tally) -> Vec<Vec<Step>> {
    let mut inside = Vec::new();
    for step in STEPS {
        if prefix.last() == Some(&step) {
            continue;
        }
        let mut schedule = prefix.to_vec();
        schedule.push(step);
        if !valid_geometry(&schedule) {
            continue;
        }
        t.schedules += 1;
        if let Some(end) = conform_answered(answers, &schedule, &mut t.seen) {
            if end["evaluated"] == 1 {
                let v = usize::try_from(end["verdict"]).expect("0..=2");
                t.verdicts[v] += 1;
                t.shown[usize::from(end["shown"] == 1)] += 1;
                t.epoch_words[usize::try_from(end["ev"]).expect("0..=2")] += 1;
            }
            t.blind += usize::from(end["blind"] == 1);
            t.late += usize::from(end["late"] == 1);
            t.max_disp = t.max_disp.max(end["disp"]);
            inside.push(schedule);
        }
    }
    inside
}

#[test]
fn resize_render_conformance() {
    let model = resize_render_model();
    let mut tally = Tally::default();
    let mut answers = Answers::new(&model);
    let roots: Vec<Vec<Step>> = extend(&mut answers, &[], &mut tally)
        .iter()
        .flat_map(|one| extend(&mut answers, one, &mut tally))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = aterm_cores::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let (model, roots, next) = (&model, &roots, &next);
                scope.spawn(move || {
                    let mut t = Tally::default();
                    let mut answers = Answers::new(model);
                    while let Some(root) =
                        roots.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    {
                        let mut stack = vec![root.clone()];
                        while let Some(prefix) = stack.pop() {
                            if prefix.len() < 6 {
                                stack.extend(extend(&mut answers, &prefix, &mut t));
                            }
                        }
                    }
                    t
                })
            })
            .collect();
        for worker in workers {
            tally.add(
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            );
        }
    });
    let Tally {
        seen,
        verdicts,
        shown,
        epoch_words,
        blind,
        late,
        max_disp,
        schedules,
    } = tally;
    for action in [
        "Demote",
        "Trim",
        "Grow",
        "Step",
        "Draw",
        "Output",
        "Classify",
        "Evaluate",
        "Grow joins a late run",
    ] {
        assert!(
            seen.get(action).is_some_and(|n| *n > 0),
            "{action} never driven: {seen:?}"
        );
    }
    assert!(
        verdicts.iter().all(|n| *n > 0),
        "ok, desync-risk and unverified all reached: {verdicts:?}"
    );
    assert!(
        shown[0] > 0 && shown[1] > 0,
        "render=displaced said and not said: {shown:?}"
    );
    assert!(
        epoch_words.iter().all(|n| *n > 0),
        "the epoch judge says ok, desync-risk and unjudged: {epoch_words:?}"
    );
    assert!(blind > 0, "the blind spot is reached, and named");
    assert!(late > 0, "a silent run outlives the run gap");
    assert_eq!(max_disp, CAP, "the displacement box is reached");
    assert!(schedules > 1_000, "{schedules} schedules");
}

#[test]
fn a_run_brought_back_to_zero_keeps_the_draw_it_saw() {
    let model = resize_render_model();
    let mut seen = BTreeMap::new();
    let schedule = [
        Step::Shrink,
        Step::Output,
        Step::Grow,
        Step::Draw,
        Step::Shrink,
        Step::Evaluate,
        Step::Grow,
        Step::Draw,
        Step::Shrink,
    ];
    let end = conform(&model, &schedule, &mut seen).expect("inside the box");
    assert_eq!((end["ro"], end["rd"], end["rw"]), (1, 0, 1), "{end:?}");
}

fn valid_geometry(schedule: &[Step]) -> bool {
    let mut shrunk = false;
    for step in schedule {
        match step {
            Step::Shrink if !shrunk => shrunk = true,
            Step::Grow if shrunk => shrunk = false,
            Step::Shrink | Step::Grow => return false,
            _ => {}
        }
    }
    true
}

#[test]
fn a_flap_split_by_a_frame_or_by_time_is_judged_on_the_real_engine() {
    let model = resize_render_model();
    let mut seen = BTreeMap::new();
    let tick = [
        Step::Shrink,
        Step::Draw,
        Step::Grow,
        Step::Handler,
        Step::Evaluate,
    ];
    let end = conform(&model, &tick, &mut seen).expect("inside the box");
    assert_eq!(
        (end["disp"], end["drew"], end["blind"], end["verdict"]),
        (1, 1, 0, 1)
    );
    let mut rig = Rig::new();
    for step in tick {
        rig.drive(step);
    }
    let reply = rig.ledger.reply(0, None);
    assert!(
        reply.contains(" flaps=1 risks=1 render=desync-risk "),
        "{reply}"
    );
    assert!(
        reply.contains("appended=1 restored=0 shift=+0"),
        "the grow appended: {reply}"
    );
    assert!(
        reply.contains("n=2 alt=1 geom=20x8>20x7>20x8 net=zero displaced=-1 verdict=desync-risk"),
        "one net-zero run: {reply}"
    );
    let [shrink, grow] = [&rig.ledger.rows[0].report, &rig.ledger.rows[1].report];
    assert!(!grow.adjoins(shrink), "the frame landed between the halves");
    let old = interp::with_buggy(&model, 5);
    let mut missed = old.init_state();
    for action in ["Demote", "Draw", "Grow", "Step", "Evaluate"] {
        assert!(old.fire(action, &mut missed), "{action}");
    }
    assert_eq!((missed["disp"], missed["verdict"]), (1, 2));
    assert!(!old.check_invariant("RiskIsFlagged", &missed), "{missed:?}");

    let slow = [Step::Shrink, Step::Evaluate, Step::Grow, Step::Evaluate];
    let end = conform(&model, &slow, &mut seen).expect("inside the box");
    assert_eq!((end["disp"], end["shown"], end["verdict"]), (0, 0, 0));
    let mut rig = Rig::new();
    for step in slow {
        rig.drive(step);
    }
    let reply = rig.ledger.reply(0, None);
    assert!(reply.contains(" flaps=1 risks=0 render=ok "), "{reply}");
    assert!(
        reply.contains("revealed=1 appended=0 restored=1 shift=+1"),
        "{reply}"
    );
    assert!(
        reply.contains("net=zero displaced=+0 verdict=none"),
        "{reply}"
    );
    let [shrink, grow] = [&rig.ledger.rows[0], &rig.ledger.rows[1]];
    assert!(
        grow.t_ms - shrink.t_ms > RUN_GAP_MS,
        "the halves are further apart than the run gap"
    );
    let timed_out = interp::with_buggy(&model, 3);
    let mut stuck = timed_out.init_state();
    for action in ["Demote", "Evaluate", "Grow", "Evaluate"] {
        assert!(timed_out.fire(action, &mut stuck), "{action}");
    }
    assert!(
        !timed_out.check_invariant("NoFalseDisplaced", &stuck),
        "{stuck:?}"
    );

    let twice = [
        Step::Shrink,
        Step::Evaluate,
        Step::Grow,
        Step::Shrink,
        Step::Draw,
        Step::Grow,
        Step::Handler,
        Step::Evaluate,
    ];
    let end = conform(&model, &twice, &mut seen).expect("inside the box");
    assert_eq!(
        (end["disp"], end["drew"], end["blind"], end["verdict"]),
        (1, 1, 0, 1)
    );
    let mut missed = old.init_state();
    for action in [
        "Demote", "Evaluate", "Grow", "Demote", "Draw", "Grow", "Step", "Evaluate",
    ] {
        assert!(old.fire(action, &mut missed), "{action}");
    }
    assert_eq!((missed["blind"], missed["verdict"]), (0, 2));
    assert!(!old.check_invariant("RiskIsFlagged", &missed), "{missed:?}");
}

#[test]
fn the_t2649_flap_is_undone_and_the_historical_engine_is_caught() {
    let model = resize_render_model();
    let schedule = [Step::Shrink, Step::Grow, Step::Handler, Step::Evaluate];
    let mut seen = BTreeMap::new();
    let end = conform(&model, &schedule, &mut seen).expect("inside the box");
    assert_eq!(
        (end["disp"], end["drew"], end["verdict"]),
        (0, 0, 0),
        "the quiet flap is an identity"
    );
    let mut rig = Rig::new();
    for step in schedule {
        rig.drive(step);
    }
    let reply = rig.ledger.reply(0, None);
    assert!(reply.contains(" render=ok "), "{reply}");
    assert!(
        reply.contains("revealed=1 appended=0 restored=1 shift=+1"),
        "{reply}"
    );
    assert!(
        reply.contains("net=zero displaced=+0 verdict=none"),
        "{reply}"
    );

    let split = [
        Step::Shrink,
        Step::Output,
        Step::Grow,
        Step::Handler,
        Step::Evaluate,
    ];
    let end = conform(&model, &split, &mut seen).expect("inside the box");
    assert_eq!((end["disp"], end["drew"], end["verdict"]), (1, 1, 1));

    let mut rig = Rig::new();
    rig.historical = true;
    for step in schedule {
        rig.drive(step);
    }
    assert_eq!(rig.verdict, 1, "the real ledger flags the historical flap");
    assert_eq!(rig.displacement(), 1, "one row up");
    let size_judge = rig.term.rows() != rig.app.rows;
    assert!(!size_judge, "the size-difference judge answers ok");
    let reply = rig.ledger.reply(0, None);
    assert!(reply.contains(" render=desync-risk "), "{reply}");
    assert!(
        reply.contains("net=zero displaced=-1 verdict=desync-risk"),
        "{reply}"
    );

    let historical = interp::with_buggy(&model, 1);
    let mut state = historical.init_state();
    for action in ["Demote", "Grow"] {
        assert!(historical.fire(action, &mut state), "{action}");
    }
    assert!(
        !historical.check_invariant("QuietFlapIsIdentity", &state),
        "{state:?}"
    );
    for action in ["Step", "Evaluate"] {
        assert!(historical.fire(action, &mut state), "{action}");
    }
    assert!(
        !historical.check_invariant("RiskIsFlagged", &state),
        "{state:?}"
    );
}

#[test]
fn the_assumed_repaint_answers_ok_on_a_tick_at_a_changed_size() {
    let model = resize_render_model();
    let schedule = [Step::Shrink, Step::Draw, Step::Evaluate];
    let mut seen = BTreeMap::new();
    let end = conform(&model, &schedule, &mut seen).expect("inside the box");
    assert_eq!(
        (end["disp"], end["drew"], end["br"], end["verdict"]),
        (1, 1, 1, 2)
    );
    let mut rig = Rig::new();
    for step in schedule {
        rig.drive(step);
    }
    assert_eq!(rig.displacement(), 1, "the screen is one row up");
    let reply = rig.ledger.reply(0, None);
    assert!(reply.contains(" render=unverified "), "{reply}");
    assert!(
        reply.contains("net=changed displaced=-1 verdict=unverified"),
        "{reply}"
    );

    let assumed = interp::with_buggy(&model, 2);
    let mut state = assumed.init_state();
    for action in ["Demote", "Draw", "Evaluate"] {
        assert!(assumed.fire(action, &mut state), "{action}");
    }
    assert_eq!(state["verdict"], 0, "the assumed repaint answers ok");
    assert!(
        !assumed.check_invariant("NoSilentPass", &state),
        "{state:?}"
    );
}

#[test]
fn a_quiet_flap_only_time_split_is_one_run_credited_back() {
    let model = resize_render_model();
    let schedule = [
        Step::Shrink,
        Step::Evaluate,
        Step::Grow,
        Step::Draw,
        Step::Evaluate,
    ];
    let mut seen = BTreeMap::new();
    let end = conform(&model, &schedule, &mut seen).expect("inside the box");
    assert_eq!(
        (end["disp"], end["drew"], end["blind"], end["verdict"]),
        (0, 0, 0, 0)
    );
    assert_eq!(seen.get("Grow joins a late run"), Some(&1));
    let mut rig = Rig::new();
    for step in schedule {
        rig.drive(step);
    }
    assert_eq!(rig.displacement(), 0, "the screen is the app's");
    assert_eq!(rig.ledger.runs[0].held, (0, 0), "the row was credited");
    let reply = rig.ledger.reply(0, None);
    assert!(reply.contains(" render=ok "), "{reply}");
    assert!(
        reply.contains("revealed=1 appended=0 restored=1 shift=+1"),
        "the grow's row still says what the engine did: {reply}"
    );
    assert!(
        reply.contains("run 1 ") && !reply.contains("run 2 "),
        "the late grow joined the shrink's run: {reply}"
    );
    assert!(
        reply.contains("net=zero displaced=+0 verdict=none"),
        "the row went back to the shrink's run: {reply}"
    );

    let dropped = [
        Step::Shrink,
        Step::Evaluate,
        Step::Output,
        Step::Grow,
        Step::Draw,
        Step::Evaluate,
    ];
    let end = conform(&model, &dropped, &mut seen).expect("inside the box");
    assert_eq!((end["disp"], end["drew"], end["verdict"]), (1, 1, 1));
    let mut rig = Rig::new();
    for step in dropped {
        rig.drive(step);
    }
    let reply = rig.ledger.reply(0, None);
    assert!(reply.contains(" render=desync-risk "), "{reply}");
    assert!(
        reply.contains("net=zero displaced=-1 verdict=desync-risk"),
        "{reply}"
    );

    let timed_out = interp::with_buggy(&model, 3);
    let mut state = timed_out.init_state();
    for action in ["Demote", "Evaluate", "Grow", "Draw", "Evaluate"] {
        assert!(timed_out.fire(action, &mut state), "{action}");
    }
    assert_eq!((state["disp"], state["verdict"]), (0, 2));
    assert!(
        !timed_out.check_invariant("NoFalseVerdict", &state),
        "{state:?}"
    );
}

fn owing(ordinal: u64, content_seq: u64, shift: i32, geom: (u16, u16)) -> EngineProbe {
    EngineProbe {
        debt: aterm_core::terminal::RenderDebt::of_rows(shift, 0),
        geom,
        ..probe(ordinal, content_seq, 0)
    }
}

const LATE_GROW: u64 = RUN_GAP_MS + 50;

fn split_flap(ledger: &mut ResizeLedger, at: u64) {
    let mut shrink = report(1, 24, 23, true, (10, 11));
    (shrink.demoted, shrink.stashed) = (1, 1);
    let mut grow = report(2, 23, 24, true, (15, 16));
    grow.appended = 1;
    ledger.ingest(
        &copy_of(&[shrink], owing(1, 11, -1, (23, 80))),
        Some(here()),
        at,
    );
    ledger.ingest(
        &copy_of(&[shrink, grow], owing(2, 16, -1, (24, 80))),
        Some(here()),
        at + LATE_GROW,
    );
}

fn epoch_lines(ledger: &ResizeLedger) -> Vec<String> {
    ledger
        .reply(0, None)
        .lines()
        .filter(|l| l.starts_with("epoch "))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_flap_an_app_frame_splits_is_judged_as_one_epoch() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    split_flap(&mut ledger, 1_000);
    assert_eq!(
        ledger.render(),
        Render::Displaced,
        "the risk wait is not over"
    );
    let out = ledger.evaluate(
        owing(2, 16, -1, (24, 80)),
        1_000 + LATE_GROW + RISK_AFTER_MS + 5,
    );
    assert_eq!(ledger.render(), Render::DesyncRisk);
    let risks: Vec<_> = out
        .iter()
        .filter(|t| matches!(t, Transition::EpochRisk { .. }))
        .collect();
    assert_eq!(risks.len(), 1, "{out:?}");
    assert!(matches!(
        risks[0],
        Transition::EpochRisk {
            epoch: 1,
            shift: -1,
            pushed: 0,
            drew: true,
            ..
        }
    ));
    assert_eq!(
        risks[0].payload(),
        "desync-risk epoch=1 displaced=-1 pushed=0 cut=0 drew=1 class=agent at=1000"
    );
    let again = ledger.evaluate(
        owing(2, 16, -1, (24, 80)),
        1_000 + LATE_GROW + RISK_AFTER_MS + 50,
    );
    assert!(again.is_empty(), "{again:?}");
    let lines = epoch_lines(&ledger);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        lines[0],
        "epoch 1 t=1000 geom=80x24>80x24 shift=-1 pushed=0 cut=0 drew=1 class=agent \
         verdict=desync-risk by=- tried=0"
    );
    assert!(
        ledger.reply(0, None).contains(" render=desync-risk "),
        "{}",
        ledger.reply(0, None)
    );
    let logs = ledger.take_logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert!(logs[0].contains("desync-risk") && logs[0].contains("class=agent"));
    assert!(ledger.take_logs().is_empty());
}

#[test]
fn an_undrawn_residual_is_judged_after_the_risk_wait() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    let mut shrink = report(1, 24, 23, true, (10, 11));
    (shrink.demoted, shrink.stashed) = (1, 1);
    let mut grow = report(2, 23, 24, true, (11, 12));
    grow.appended = 1;
    ledger.ingest(
        &copy_of(&[shrink, grow], owing(2, 12, -1, (24, 80))),
        Some(here()),
        1_000,
    );
    assert_eq!(ledger.render(), Render::Displaced);
    assert_eq!(ledger.look_at_ms(), Some(1_000 + RISK_AFTER_MS));
    ledger.evaluate(owing(2, 12, -1, (24, 80)), 1_000 + RISK_AFTER_MS);
    assert_eq!(ledger.render(), Render::DesyncRisk);
    assert!(epoch_lines(&ledger)[0].contains("drew=0"));
    assert_eq!(ledger.look_at_ms(), None, "judged: nothing more is owed");
}

#[test]
fn an_epoch_whose_geometry_is_not_back_is_only_displaced() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    let mut shrink = report(1, 24, 23, true, (10, 11));
    (shrink.demoted, shrink.stashed) = (1, 1);
    ledger.ingest(
        &copy_of(&[shrink], owing(1, 11, -1, (23, 80))),
        Some(here()),
        1_000,
    );
    ledger.evaluate(owing(1, 11, -1, (23, 80)), 1_000 + 10 * RISK_AFTER_MS);
    assert_eq!(ledger.render(), Render::Displaced);
    assert_eq!(ledger.look_at_ms(), None, "the next resize re-registers it");
}

#[test]
fn an_epoch_closes_when_the_engine_debt_returns_to_zero() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    let mut shrink = report(1, 24, 23, true, (10, 11));
    (shrink.demoted, shrink.stashed) = (1, 1);
    ledger.ingest(
        &copy_of(&[shrink], owing(1, 11, -1, (23, 80))),
        Some(here()),
        1_000,
    );
    assert_eq!(ledger.epochs().current().map(|e| e.id), Some(1));
    ledger.evaluate(owing(1, 11, 0, (23, 80)), 1_005);
    assert!(ledger.epochs().current().is_none());
    let lines = epoch_lines(&ledger);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("verdict=restored by=undo"), "{lines:?}");
    assert_eq!(ledger.epochs().state(), None, "the epoch owes nothing now");

    let mut quiet = ResizeLedger::default();
    let [shrink, mut grow] = alt_flap(1, 10);
    (grow.revealed, grow.restored_top, grow.appended) = (1, 1, 0);
    quiet.ingest(
        &copy_of(&[shrink, grow], probe(2, 12, 0)),
        Some(here()),
        1_000,
    );
    assert!(epoch_lines(&quiet).is_empty(), "a quiet flap owes no epoch");
    assert_eq!(quiet.look_at_ms(), None, "an idle machine arms no deadline");
}

#[test]
fn a_journal_overrun_cannot_hide_a_displacement() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    let mut shrink = report(40, 24, 23, true, (500, 501));
    (shrink.demoted, shrink.stashed) = (1, 0);
    let mut grow = report(41, 23, 24, true, (501, 502));
    grow.appended = 1;
    ledger.ingest(
        &copy_of(&[shrink, grow], owing(41, 502, -20, (24, 80))),
        Some(here()),
        1_000,
    );
    assert!(ledger.lost > 0, "the journal overran");
    ledger.evaluate(owing(41, 502, -20, (24, 80)), 1_000 + RISK_AFTER_MS);
    assert_eq!(ledger.render(), Render::DesyncRisk);
    assert!(epoch_lines(&ledger)[0].contains("shift=-20"));
}

#[test]
fn the_transition_names_who_healed_it() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    split_flap(&mut ledger, 1_000);
    ledger.evaluate(
        owing(2, 16, -1, (24, 80)),
        1_000 + LATE_GROW + RISK_AFTER_MS + 5,
    );
    assert_eq!(ledger.render(), Render::DesyncRisk);
    let healed = EngineProbe {
        full_clears: 1,
        ..owing(2, 17, 0, (24, 80))
    };
    let out = ledger.evaluate(healed, 1_000 + LATE_GROW + RISK_AFTER_MS + 300);
    let epoch_healed: Vec<_> = out
        .iter()
        .filter(|t| matches!(t, Transition::EpochHealed { .. }))
        .collect();
    assert_eq!(epoch_healed.len(), 1, "{out:?}");
    assert_eq!(
        epoch_healed[0].payload(),
        "healed epoch=1 after_ms=1300 by=app"
    );
    assert_eq!(ledger.render(), Render::Ok);
    assert!(epoch_lines(&ledger)[0].contains("verdict=healed by=app"));
}

#[test]
fn the_nudge_acts_only_on_an_untried_agent_epoch_at_desync_risk() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    split_flap(&mut ledger, 1_000);
    assert_eq!(ledger.nudge_view(), None, "the risk wait is not over");
    ledger.evaluate(
        owing(2, 16, -1, (24, 80)),
        1_000 + LATE_GROW + RISK_AFTER_MS + 5,
    );
    let view = ledger.nudge_view().expect("agent epoch at risk");
    assert_eq!(
        (view.epoch, view.shift, view.pushed, view.drew),
        (1, -1, 0, true)
    );
    assert!(ledger.agent_class());
    ledger.note_tried(view.epoch);
    assert_eq!(ledger.nudge_view(), None);
    assert!(epoch_lines(&ledger)[0].contains("tried=1"));
    let mut other = ResizeLedger::default();
    let mut shrink = report(1, 24, 23, true, (10, 11));
    (shrink.demoted, shrink.stashed) = (1, 1);
    let mut grow = report(2, 23, 24, true, (11, 12));
    grow.appended = 1;
    other.ingest(
        &copy_of(&[shrink, grow], owing(2, 12, -1, (24, 80))),
        Some(here()),
        1_000,
    );
    other.evaluate(
        owing(2, 12, -1, (24, 80)),
        1_000 + LATE_GROW + RISK_AFTER_MS,
    );
    assert_eq!(other.render(), Render::Unjudged);
    assert_eq!(other.nudge_view(), None);
    assert!(!other.agent_class());
}

#[test]
fn an_epoch_the_nudge_repainted_closes_by_nudge() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    split_flap(&mut ledger, 1_000);
    ledger.evaluate(
        owing(2, 16, -1, (24, 80)),
        1_000 + LATE_GROW + RISK_AFTER_MS + 5,
    );
    ledger.note_tried(1);
    ledger.set_nudge("restoring", 1);
    assert!(
        ledger
            .reply(0, None)
            .contains(" nudge=restoring nudged=1\n")
    );
    let healed = EngineProbe {
        full_clears: 2,
        ..owing(2, 20, 0, (24, 80))
    };
    let out = ledger.evaluate(healed, 1_000 + LATE_GROW + RISK_AFTER_MS + 900);
    let payloads: Vec<String> = out.iter().map(Transition::payload).collect();
    assert_eq!(payloads, ["healed epoch=1 after_ms=1900 by=nudge"]);
    assert!(epoch_lines(&ledger)[0].contains("verdict=healed by=nudge tried=1"));
    assert_eq!(ledger.render(), Render::Ok);

    // The same repaint first seen at the next resize (no reading came between them) closes
    // the epoch by the same rule: the attempt's.
    let mut late = ResizeLedger::default();
    late.set_agent(true);
    split_flap(&mut late, 1_000);
    late.evaluate(
        owing(2, 16, -1, (24, 80)),
        1_000 + LATE_GROW + RISK_AFTER_MS + 5,
    );
    late.note_tried(1);
    let mut drag = report(3, 24, 23, true, (20, 21));
    drag.full_clears = 2;
    let after = EngineProbe {
        full_clears: 2,
        ..owing(3, 21, 0, (23, 80))
    };
    late.ingest(
        &copy_of(&[drag], after),
        Some(here()),
        1_000 + LATE_GROW + RISK_AFTER_MS + 900,
    );
    let lines = epoch_lines(&late);
    assert!(
        lines[0].contains("verdict=healed by=nudge tried=1"),
        "{lines:?}"
    );
}

/// Leaving the alternate screen settles its debt whatever the nudge was holding: the program's
/// own act, read the same whichever of the nudge and this judge sees it first.
#[test]
fn a_program_that_left_the_alternate_screen_during_the_hold_closes_by_app() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?1049h");
    assert!(!EngineProbe::sample(&term).main_screen);
    term.process(b"\x1b[?1049l");
    let sampled = EngineProbe::sample(&term);
    assert!(sampled.main_screen);
    assert!(!sampled.debt.owed());

    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    split_flap(&mut ledger, 1_000);
    ledger.evaluate(
        owing(2, 16, -1, (24, 80)),
        1_000 + LATE_GROW + RISK_AFTER_MS + 5,
    );
    ledger.note_tried(1);
    let left = EngineProbe {
        screen_replaced: 1,
        main_screen: true,
        ..owing(2, 20, 0, (24, 80))
    };
    let out = ledger.evaluate(left, 1_000 + LATE_GROW + RISK_AFTER_MS + 900);
    let payloads: Vec<String> = out.iter().map(Transition::payload).collect();
    assert_eq!(payloads, ["healed epoch=1 after_ms=1900 by=app"]);
    assert!(epoch_lines(&ledger)[0].contains("verdict=healed by=app tried=1"));
}

#[test]
fn an_unknown_program_reads_unjudged_and_pushes_no_event() {
    let mut ledger = ResizeLedger::default();
    let mut shrink = report(1, 24, 23, true, (10, 11));
    (shrink.demoted, shrink.stashed) = (1, 1);
    let mut grow = report(2, 23, 24, true, (11, 12));
    grow.appended = 1;
    ledger.ingest(
        &copy_of(&[shrink, grow], owing(2, 12, -1, (24, 80))),
        Some(here()),
        1_000,
    );
    let out = ledger.evaluate(owing(2, 12, -1, (24, 80)), 1_000 + RISK_AFTER_MS);
    assert_eq!(ledger.render(), Render::Unjudged);
    assert!(out.is_empty(), "unjudged is never pushed: {out:?}");
    assert!(epoch_lines(&ledger)[0].contains("class=unknown verdict=unjudged"));
    assert!(ledger.reply(0, None).contains(" render=unjudged "));
    let logs = ledger.take_logs();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains("unjudged"), "{logs:?}");
}

#[test]
fn a_program_that_cleared_on_resize_is_learned() {
    let mut ledger = ResizeLedger::default();
    let mut shrink = report(1, 24, 23, true, (10, 11));
    (shrink.demoted, shrink.stashed) = (1, 1);
    ledger.ingest(
        &copy_of(&[shrink], owing(1, 11, -1, (23, 80))),
        Some(here()),
        1_000,
    );
    let cleared = EngineProbe {
        full_clears: 1,
        ..owing(1, 12, 0, (23, 80))
    };
    ledger.evaluate(cleared, 1_040);
    assert_eq!(run_verdict(&ledger, 1), Verdict::Healed);
    let mut shrink = report(2, 23, 22, true, (12, 13));
    (shrink.demoted, shrink.stashed) = (1, 1);
    shrink.full_clears = 1;
    let mut grow = report(3, 22, 23, true, (13, 14));
    grow.appended = 1;
    grow.full_clears = 1;
    let probe_at = |ord, seq| EngineProbe {
        full_clears: 1,
        ..owing(ord, seq, -1, (23, 80))
    };
    ledger.ingest(
        &copy_of(&[shrink, grow], probe_at(3, 14)),
        Some(here()),
        5_000,
    );
    ledger.evaluate(probe_at(3, 14), 5_000 + RISK_AFTER_MS);
    assert_eq!(ledger.render(), Render::DesyncRisk);
    assert!(
        epoch_lines(&ledger)
            .last()
            .unwrap()
            .contains("class=learned")
    );
}

#[test]
fn a_contended_terminal_is_asked_again_after_100ms() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    let mut grow = report(2, 23, 24, true, (11, 12));
    grow.appended = 1;
    let mut shrink = report(1, 24, 23, true, (10, 11));
    (shrink.demoted, shrink.stashed) = (1, 1);
    ledger.ingest(
        &copy_of(&[shrink, grow], owing(2, 12, -1, (24, 80))),
        Some(here()),
        1_000,
    );
    assert_eq!(ledger.look_at_ms(), Some(2_000));
    assert!(!ledger.look_due(1_999));
    assert!(ledger.look_due(2_000));
    ledger.defer_look(2_000);
    assert_eq!(ledger.look_at_ms(), Some(2_100));
    for _ in 1..20 {
        ledger.defer_look(2_100);
    }
    assert_eq!(ledger.look_at_ms(), Some(2_100 + 100), "the 20th retry");
    ledger.defer_look(3_000);
    assert_eq!(
        ledger.look_at_ms(),
        Some(3_000 + RISK_AFTER_MS),
        "then at the next risk wait: no spin"
    );
}

#[test]
fn a_restored_engine_seeded_with_debt_owes_an_epoch() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    ledger.evaluate(owing(0, 5, -1, (24, 80)), 10_000);
    assert_eq!(ledger.render(), Render::Displaced);
    ledger.evaluate(owing(0, 5, -1, (24, 80)), 10_000 + RISK_AFTER_MS);
    assert_eq!(ledger.render(), Render::DesyncRisk);
}

#[test]
fn a_column_flap_that_cut_cells_is_judged_until_a_repaint() {
    let mut term = Terminal::new(ROWS, COLS);
    term.process(b"\x1b[?1049h\x1b[2J");
    let rule = "\u{2500}".repeat(usize::from(COLS));
    for r in [ROWS - 3, ROWS - 1] {
        term.process(format!("\x1b[{};1H{rule}", r + 1).as_bytes());
    }
    term.process(format!("\x1b[{};1H> ", ROWS - 1).as_bytes());
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    term.resize(ROWS, COLS - 1);
    ledger.ingest(&JournalCopy::take(&term), Some(here()), 1_000);
    term.process(b"x");
    term.resize(ROWS, COLS);
    ledger.ingest(&JournalCopy::take(&term), Some(here()), 1_035);
    let cells = |term: &Terminal, r: u16| {
        term.row_text(usize::from(r))
            .unwrap_or_default()
            .trim_end()
            .chars()
            .count()
    };
    assert_eq!(
        cells(&term, ROWS - 1),
        usize::from(COLS) - 1,
        "the rule lost its last cell"
    );

    ledger.evaluate(EngineProbe::sample(&term), 1_035 + RISK_AFTER_MS + 5);
    let reply = ledger.reply(0, None);
    assert_eq!(ledger.render(), Render::DesyncRisk, "{reply}");
    assert!(reply.contains(" render=desync-risk "), "{reply}");
    let lines = epoch_lines(&ledger);
    assert_eq!(lines.len(), 1, "{reply}");
    assert!(
        lines[0].contains(" shift=+0 pushed=0 cut=1 ")
            && lines[0].contains(" verdict=desync-risk "),
        "{reply}"
    );
    assert!(ledger.nudge_view().is_some(), "the nudge is owed");

    term.process(b"\x1b[2J");
    ledger.evaluate(EngineProbe::sample(&term), 1_035 + RISK_AFTER_MS + 50);
    assert_eq!(ledger.render(), Render::Ok, "{}", ledger.reply(0, None));
    assert!(ledger.nudge_view().is_none());
}

#[test]
fn a_session_that_owes_nothing_has_no_epoch() {
    let mut ledger = ResizeLedger::default();
    ledger.set_agent(true);
    split_flap(&mut ledger, 1_000);
    let mut conpty = ResizeLedger::default();
    conpty.set_agent(true);
    let mut shrink = report(1, 24, 23, true, (10, 11));
    shrink.demoted = 1;
    let mut grow = report(2, 23, 24, true, (15, 16));
    grow.appended = 1;
    conpty.ingest(
        &copy_of(&[shrink, grow], probe(2, 16, 0)),
        Some(here()),
        1_000,
    );
    conpty.evaluate(probe(2, 16, 0), 1_000 + LATE_GROW + RISK_AFTER_MS);
    assert!(epoch_lines(&conpty).is_empty());
    assert_ne!(conpty.render(), Render::DesyncRisk);
}
