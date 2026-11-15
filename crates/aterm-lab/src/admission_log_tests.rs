// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The admission log's reader ([`super`]) over fixture rows, with exact numbers: E1's
//! bytes-weighted ratio over its population, a pooled claim in it only where its width's record
//! sized it, E2's avoidable waits and their causes, judged only at looks a minute before their
//! end, `room*` over every held claim of memory, a holder with no true peak counted by its claim,
//! the starved share summed over rows, the heavy land judgments' overlaps, and how far back the
//! log reaches.

use std::collections::BTreeMap;

use aterm_capacity::admission_log::{Reading, Row};
use aterm_json::Value;

use super::{Cause, Frame, Sum, Verdict, duration_ms, judge, room};

const GIB: u64 = 1 << 30;
const HOUR_MS: u64 = 3_600_000;

fn frame(cores: u64) -> Frame {
    Frame {
        cores,
        since_ms: 0,
        now_ms: HOUR_MS,
    }
}

fn reading(rows: Vec<Row>) -> Reading {
    Reading {
        rows,
        ..Reading::default()
    }
}

/// The `stage` row of claim `id`: `claimed` bytes sized by `basis`, the stage's peak `peak`, its
/// record's outcome `kept`; an unpooled compile with no ceiling, so at the host's width.
fn stage(id: &str, basis: &str, claimed: u64, peak: u64, kept: &str) -> Row {
    Row::new("stage", 1_000)
        .with("run", "100-1")
        .with("stage", "TestCompile")
        .with("claim", id)
        .with("claimed", claimed)
        .with("basis", basis)
        .with("width", 14_u64)
        .with("pooled", false)
        .with("ceiling", Value::Null)
        .with("peak", peak)
        .with("kept", kept)
        .with("cpu_ms", 1_000_u64)
        .with("wall_ms", 1_000_u64)
}

/// A look row's holder.
fn holder(id: &str, held: bool, claimed: u64, reserved: u64) -> Value {
    let mut fields = aterm_json::Map::new();
    fields.insert("id".to_owned(), id.into());
    fields.insert("pid".to_owned(), 9_u64.into());
    fields.insert("held".to_owned(), held.into());
    fields.insert("claimed".to_owned(), claimed.into());
    fields.insert("reserved".to_owned(), reserved.into());
    fields.insert("alone".to_owned(), false.into());
    Value::Object(fields)
}

/// A waiting `look` row of claim `id` asking `mem` with no margin, short of memory with
/// `available` free and `held` ahead of it.
fn look(id: &str, at_ms: u64, mem: u64, available: u64, held: &[(&str, u64, u64)]) -> Row {
    let reserved = held.iter().map(|(_, _, r)| r).sum::<u64>();
    let holders: Vec<Value> = held
        .iter()
        .map(|(h, claimed, reserved)| holder(h, true, *claimed, *reserved))
        .collect();
    Row::new("look", at_ms)
        .with("id", id)
        .with("pid", 7_u64)
        .with("what", "verify tippy lint")
        .with("mem", mem)
        .with("margin", 0_u64)
        .with("admitted", false)
        .with("available", available)
        .with("shorts", vec![Value::from("memory")])
        .with("reserved", reserved)
        .with("holders", holders)
}

/// The look that admits claim `id`.
fn admitting(id: &str, at_ms: u64) -> Row {
    Row::new("look", at_ms)
        .with("id", id)
        .with("admitted", true)
        .with("shorts", Vec::<Value>::new())
}

/// E1 is bytes-weighted over its population: 20 own claims of 12 GiB on 10 GiB peaks and 10 of
/// 6 GiB on 4 GiB read 300/240 = 1.25 (a mean of the per-claim ratios would say 1.3), which passes.
/// A cold claim, a claim under a jobs ceiling and a peak under 4 GiB count only in every claim's
/// figure. NEGATIVE CONTROLS: 30 claims of 15 GiB on 10 GiB read 1.5 and fail, and 29 of them
/// are too few to say; a light run moves every claim's figure and the light share, never the
/// gated one. G-bin reads the mismatched binaries once there are 50.
#[test]
fn e1_reads_bytes_weighted_over_matched_non_light_claims() {
    let mut rows: Vec<Row> = (0..20)
        .map(|i| stage(&format!("a{i}"), "learned", 12 * GIB, 10 * GIB, "kept"))
        .collect();
    rows.extend((0..10).map(|i| stage(&format!("b{i}"), "elsewhere", 6 * GIB, 4 * GIB, "kept")));
    rows.push(stage("cold", "cold", 16 * GIB, 5 * GIB, "kept"));
    rows.push(stage("ceiling", "learned", 20 * GIB, 5 * GIB, "kept").with("ceiling", 4_u64));
    rows.push(stage("small", "learned", 3 * GIB, 2 * GIB, "kept"));
    let e1 = judge(&reading(rows.clone()), frame(14)).e1;
    assert_eq!(
        e1.matched,
        Sum {
            claim: 300 * GIB,
            peak: 240 * GIB,
            n: 30
        }
    );
    assert_eq!(e1.matched.ratio(), Some(1.25));
    assert_eq!(e1.verdict, Verdict::Pass);
    assert_eq!(e1.under, 0);
    assert_eq!(e1.p90, Some(1.5), "the 27th of 30 sorted ratios");
    assert_eq!(
        e1.all,
        Sum {
            claim: 339 * GIB,
            peak: 252 * GIB,
            n: 33
        }
    );
    assert_eq!(e1.light, 0);
    assert_eq!(
        e1.table
            .get(&("cold".to_owned(), "TestCompile".to_owned()))
            .copied(),
        Some(Sum {
            claim: 16 * GIB,
            peak: 5 * GIB,
            n: 1
        })
    );

    let mut lit = rows.clone();
    lit.push(stage("light", "learned", 40 * GIB, 8 * GIB, "light"));
    let lit = judge(&reading(lit), frame(14)).e1;
    assert_eq!(
        lit.matched, e1.matched,
        "a light run is out of the gated population"
    );
    assert_eq!(lit.verdict, Verdict::Pass);
    assert_eq!(lit.all.n, 34);
    assert_eq!(lit.all.claim, 379 * GIB);
    assert_eq!(lit.light, 1);

    let over: Vec<Row> = (0..30)
        .map(|i| stage(&format!("o{i}"), "learned", 15 * GIB, 10 * GIB, "kept"))
        .collect();
    let failed = judge(&reading(over.clone()), frame(14)).e1;
    assert_eq!(failed.matched.ratio(), Some(1.5));
    assert_eq!(failed.verdict, Verdict::Fail);
    let few = judge(&reading(over[..29].to_vec()), frame(14)).e1;
    assert_eq!(few.verdict, Verdict::Vacuous);

    let tests = |count: u64| {
        let mut class = aterm_json::Map::new();
        class.insert("claim".to_owned(), (70 * GIB).into());
        class.insert("peak".to_owned(), (50 * GIB).into());
        class.insert("count".to_owned(), count.into());
        let mut classes = aterm_json::Map::new();
        classes.insert("mismatched".to_owned(), Value::Object(class));
        Row::new("tests", 2_000).with("classes", Value::Object(classes))
    };
    assert_eq!(
        judge(&reading(vec![tests(50)]), frame(14)).e1.g_bin,
        Verdict::Pass,
        "1.4 over 50 mismatched binaries"
    );
    assert_eq!(
        judge(&reading(vec![tests(49)]), frame(14)).e1.g_bin,
        Verdict::Vacuous
    );
}

/// A 12 GiB claim whose own true peak is 10 GiB (so its true peak claims 12) waits two minutes
/// behind a held 16 GiB claim that still reserves all of it, with 20 GiB available: the holder's
/// true peak of 5 GiB leaves 15 GiB beside it, so the wait was avoidable, and since the 4 GiB
/// beside the held reservations could not hold its claim, the cause is `holders`. A claim of
/// 16 GiB in the same room is `own`. Behind a held 4 GiB claim with no row and a 10 GiB claim
/// waiting ahead, the 16 GiB beside the held reservation holds its claim and the 6 GiB beside
/// every reservation does not: `ahead`, a wait for its turn, not for the holders. NEGATIVE
/// CONTROLS: a holder whose true peak is 12 GiB leaves 8 GiB, and the wait is not avoidable; a
/// wait admitted within the minute is no wait.
#[test]
fn e2_tags_an_avoidable_wait_by_cause() {
    let rows = |holder_peak: u64, mem: u64| {
        vec![
            stage("h", "learned", 16 * GIB, holder_peak, "kept"),
            stage("w", "learned", mem, 10 * GIB, "kept"),
            look("w", 0, mem, 20 * GIB, &[("h", 16 * GIB, 16 * GIB)]),
            look("w", 60_000, mem, 20 * GIB, &[("h", 16 * GIB, 16 * GIB)]),
            admitting("w", 120_000),
        ]
    };
    let e2 = judge(&reading(rows(5 * GIB, 12 * GIB)), frame(14)).e2;
    assert_eq!(e2.waits.len(), 1, "{e2:?}");
    let wait = &e2.waits[0];
    assert_eq!(wait.id, "w");
    assert_eq!(wait.end_ms - wait.first_ms, 120_000);
    assert!(wait.ended);
    assert_eq!(wait.needs, 12 * GIB, "peak_claim of its 10 GiB true peak");
    assert_eq!(wait.room, Some(15 * GIB));
    assert_eq!(wait.cause, Some(Cause::Holders));
    assert_eq!(e2.avoidable(Cause::Holders), 1);
    assert_eq!(
        e2.verdict,
        Verdict::Fail,
        "one avoidable wait fails the gate"
    );

    let own = judge(&reading(rows(5 * GIB, 16 * GIB)), frame(14)).e2;
    assert_eq!(own.waits[0].cause, Some(Cause::Own), "{own:?}");

    let queued = |at_ms: u64| {
        look("w", at_ms, 12 * GIB, 20 * GIB, &[])
            .with(
                "holders",
                vec![
                    holder("h", true, 4 * GIB, 4 * GIB),
                    holder("q", false, 10 * GIB, 10 * GIB),
                ],
            )
            .with("reserved", 14 * GIB)
    };
    let ahead = judge(
        &reading(vec![
            stage("w", "learned", 12 * GIB, 10 * GIB, "kept"),
            queued(0),
            queued(60_000),
            admitting("w", 120_000),
        ]),
        frame(14),
    )
    .e2;
    assert_eq!(ahead.waits[0].room, Some(16 * GIB), "{ahead:?}");
    assert_eq!(ahead.waits[0].cause, Some(Cause::Ahead), "{ahead:?}");
    assert_eq!(ahead.avoidable(Cause::Holders), 0);

    let tight = judge(&reading(rows(12 * GIB, 12 * GIB)), frame(14)).e2;
    assert_eq!(tight.waits[0].room, Some(8 * GIB));
    assert_eq!(tight.waits[0].cause, None);
    assert_eq!(tight.verdict, Verdict::Vacuous, "one wait, none avoidable");

    let brief = judge(
        &reading(vec![
            look("w", 0, 12 * GIB, 20 * GIB, &[("h", 16 * GIB, 16 * GIB)]),
            admitting("w", 59_999),
        ]),
        frame(14),
    )
    .e2;
    assert!(brief.waits.is_empty(), "{brief:?}");
}

/// A held claim with no `stage` row counts its claim as its true peak: one claiming 16 GiB that
/// uses 10 (it reserves 6) adds its 6 GiB, so 20 GiB available leaves `room*` 14 GiB, and the
/// coverage counts all of its bytes by claim. With a row whose peak is 11 GiB, it adds 1 GiB.
#[test]
fn a_holder_without_a_stage_row_counts_its_claim() {
    let at = look("w", 0, 12 * GIB, 20 * GIB, &[("h", 16 * GIB, 6 * GIB)]);
    assert_eq!(room(&at, &BTreeMap::new()), Some(14 * GIB));
    let rows = vec![at.clone(), admitting("w", 120_000)];
    let report = judge(&reading(rows.clone()), frame(14));
    assert_eq!(report.e2.waits[0].room, Some(14 * GIB));
    assert_eq!(report.coverage.holders, 1);
    assert_eq!(report.coverage.known, 0);
    assert_eq!(report.coverage.held_bytes, 16 * GIB);
    assert_eq!(report.coverage.by_claim_bytes, 16 * GIB);

    let known = BTreeMap::from([("h".to_owned(), 11 * GIB)]);
    assert_eq!(room(&at, &known), Some(19 * GIB));
    let mut with_row = rows;
    with_row.push(stage("h", "learned", 16 * GIB, 11 * GIB, "kept"));
    let report = judge(&reading(with_row), frame(14));
    assert_eq!(report.e2.waits[0].room, Some(19 * GIB));
    assert_eq!(report.coverage.known, 1);
    assert_eq!(report.coverage.by_claim_bytes, 0);
}

/// The test binaries' starved share is Σ starved / Σ(starved + CPU) over the `tests` rows:
/// 200 ms starved beside 600 of CPU and none beside 1,200 is 200/2,000 (the rows' mean share
/// would be 0.125). The compiles' share sums only the stage rows whose account read starved time.
#[test]
fn starved_share_sums_the_tests_rows() {
    let tests = |cpu: u64, starved: u64| {
        Row::new("tests", 2_000)
            .with("cpu_ms", cpu)
            .with("starved_ms", starved)
    };
    let rows = vec![
        tests(600, 200),
        tests(1_200, 0),
        stage("c", "learned", 8 * GIB, 6 * GIB, "kept")
            .with("cpu_ms", 700_u64)
            .with("starved_ms", 300_u64),
        stage("d", "learned", 8 * GIB, 6 * GIB, "kept"),
    ];
    let e4 = judge(&reading(rows), frame(14)).e4;
    assert_eq!(e4.tests.n, 2);
    assert_eq!(e4.tests.share(), Some(0.1));
    assert_eq!(
        e4.compiles.n, 1,
        "a stage that read no starved time is not summed"
    );
    assert_eq!(e4.compiles.share(), Some(0.3));
    assert_eq!(e4.all().share(), Some(500.0 / 3_000.0));
}

/// On two cores H is 600 s of CPU. Three heavy land judgments (one heavy by its test binaries'
/// CPU) overlap in three pairs, two or more at once for 70 s and three at most; a land judgment
/// a millisecond under H and a heavy run that is no land judgment count in none. Their mean wall
/// and the CPU of the judgments that reached a verdict make §4 row 2's figures.
#[test]
fn heavy_land_judgments_count_their_overlaps() {
    let run = |id: &str, land: bool, start: u64, end: u64, stage: u64, test: u64, outcome: &str| {
        Row::new("run", end)
            .with("run", id)
            .with("land", land)
            .with("start_ms", start)
            .with("end_ms", end)
            .with("stage_cpu_ms", stage)
            .with("test_cpu_ms", test)
            .with("outcome", outcome)
    };
    let rows = vec![
        run("a", true, 0, 100_000, 700_000, 0, "pass"),
        run("b", true, 50_000, 150_000, 1, 600_000, "fail"),
        run("c", true, 200_000, 300_000, 599_999, 0, "pass"),
        run("d", false, 0, 300_000, 900_000, 0, "pass"),
        run("e", true, 90_000, 120_000, 650_000, 0, "could-not-run"),
    ];
    let report = judge(&reading(rows), frame(2));
    let e3 = &report.e3;
    assert_eq!(e3.heavy_ms, 600_000);
    assert_eq!(e3.land, 4);
    let heavy: Vec<&str> = e3.heavy.iter().map(|j| j.run.as_str()).collect();
    assert_eq!(heavy, ["a", "b", "e"]);
    assert_eq!(e3.pairs, 3);
    assert_eq!(e3.overlap_ms, 70_000);
    assert_eq!(e3.most, 3);
    assert_eq!(
        e3.verdict,
        Verdict::Vacuous,
        "fewer than 20 heavy land judgments"
    );
    assert_eq!(report.row2.mean_wall_ms, Some(230_000 / 3));
    assert_eq!(report.row2.completed_cpu_ms, 700_000 + 600_000 + 599_999);
    assert_eq!(report.row2.span_ms, HOUR_MS);
}

/// A pooled stage claim is in E1's population only where the record keyed by its own compiler
/// jobs sized it: six jobs' claim sized by six jobs' record counts, and its exact figures are the
/// population's. NEGATIVE CONTROLS: a claim pooled at six jobs that the host-width record sized
/// (no `keyed_jobs`), and one another width's record sized, count only in every claim's figure.
#[test]
fn e1_counts_a_pooled_claim_only_where_its_width_sized_it() {
    let pooled = |id: &str, claimed: u64, keyed: Option<u64>| {
        stage(id, "learned", claimed, 10 * GIB, "kept")
            .with("pooled", true)
            .with("width", 6_u64)
            .with("keyed_jobs", keyed)
    };
    let rows = vec![
        pooled("keyed", 12 * GIB, Some(6)),
        pooled("host", 20 * GIB, None),
        pooled("other", 16 * GIB, Some(8)),
    ];
    let e1 = judge(&reading(rows), frame(14)).e1;
    assert_eq!(
        e1.matched,
        Sum {
            claim: 12 * GIB,
            peak: 10 * GIB,
            n: 1
        }
    );
    assert_eq!(
        e1.all,
        Sum {
            claim: 48 * GIB,
            peak: 30 * GIB,
            n: 3
        }
    );
}

/// A wait is judged only at its looks a minute or more before its end: a two-minute wait whose
/// look at 0 left `room*` of 5 GiB and whose look 20 s before its admission left 15 GiB is not
/// avoidable, since the holders may have just let their memory go. NEGATIVE CONTROL: that same
/// look a minute before the admission makes the wait avoidable, for the held reservations.
#[test]
fn e2_judges_a_wait_only_at_looks_a_minute_before_its_end() {
    let rows = |late_ms: u64| {
        vec![
            stage("h", "learned", 16 * GIB, 5 * GIB, "kept"),
            stage("w", "learned", 12 * GIB, 10 * GIB, "kept"),
            look("w", 0, 12 * GIB, 10 * GIB, &[("h", 16 * GIB, 16 * GIB)]),
            look(
                "w",
                late_ms,
                12 * GIB,
                20 * GIB,
                &[("h", 16 * GIB, 16 * GIB)],
            ),
            admitting("w", 120_000),
        ]
    };
    let late = judge(&reading(rows(100_000)), frame(14)).e2;
    assert_eq!(late.waits.len(), 1, "{late:?}");
    assert_eq!(
        late.waits[0].room,
        Some(5 * GIB),
        "only the look at 0 judges it"
    );
    assert_eq!(late.waits[0].cause, None);

    let early = judge(&reading(rows(60_000)), frame(14)).e2;
    assert_eq!(early.waits[0].room, Some(15 * GIB));
    assert_eq!(early.waits[0].cause, Some(Cause::Holders));
}

/// `room*` counts every held claim of memory a look was held to, its `held_mem`, though its
/// shortfall (the kernel's pressure alone) names no holder: a 10 GiB claim with no row adds what
/// it reserves, a 6 GiB one already in use whose true peak is 8 GiB adds 2, so 20 GiB available
/// leaves 8. A wait whose looks were made by fresh claims is one wait under its first id, and its
/// own true peak is the admitted claim's. NEGATIVE CONTROL: without `held_mem`, the same look
/// would count no held claim and read 20.
#[test]
fn room_counts_every_held_claim_of_memory() {
    let held_mem = |id: &str, claimed: u64, reserved: u64| {
        let mut fields = aterm_json::Map::new();
        fields.insert("id".to_owned(), id.into());
        fields.insert("claimed".to_owned(), claimed.into());
        fields.insert("reserved".to_owned(), reserved.into());
        fields.insert("alone".to_owned(), false.into());
        Value::Object(fields)
    };
    let pressure = |at_ms: u64| {
        look("w", at_ms, 4 * GIB, 20 * GIB, &[])
            .with("shorts", vec![Value::from("pressure")])
            .with(
                "held_mem",
                vec![held_mem("a", 10 * GIB, 10 * GIB), held_mem("b", 6 * GIB, 0)],
            )
    };
    let peaks = BTreeMap::from([("b".to_owned(), 8 * GIB)]);
    assert_eq!(room(&pressure(0), &peaks), Some(8 * GIB));
    let bare = look("w", 0, 4 * GIB, 20 * GIB, &[]).with("shorts", vec![Value::from("pressure")]);
    assert_eq!(room(&bare, &peaks), Some(20 * GIB), "no held claim listed");

    let rows = vec![
        stage("b", "learned", 6 * GIB, 8 * GIB, "kept"),
        stage("w2", "learned", 4 * GIB, 3 * GIB, "kept"),
        pressure(0),
        pressure(60_000).with("claim", "w1"),
        admitting("w", 120_000).with("claim", "w2"),
    ];
    let report = judge(&reading(rows), frame(14));
    assert_eq!(report.e2.waits.len(), 1, "{:?}", report.e2);
    let wait = &report.e2.waits[0];
    assert_eq!(wait.id, "w");
    assert_eq!(wait.room, Some(8 * GIB));
    assert_eq!(
        wait.needs,
        aterm_capacity::peak_claim(3 * GIB),
        "the admitted claim's own true peak"
    );
    assert_eq!(report.coverage.holders, 2);
    assert_eq!(report.coverage.known, 1);
}

/// Coverage says how far back the log reaches whatever the window: its oldest line and its oldest
/// run row, read across every file, beside the oldest run row in the window.
#[test]
fn coverage_says_how_far_back_the_log_reaches() {
    let reading = Reading {
        rows: vec![Row::new("run", 5_000).with("run", "r")],
        oldest_ms: Some(100),
        oldest_by_kind: BTreeMap::from([("run".to_owned(), 1_000)]),
        ..Reading::default()
    };
    let report = judge(&reading, frame(14));
    assert_eq!(report.coverage.oldest_ms, Some(100));
    assert_eq!(report.coverage.oldest_run_ms, Some(1_000));
    assert_eq!(report.coverage.window_run_ms, Some(5_000));
    let text = report.text(std::path::Path::new("/log"));
    assert!(
        text.contains("reaches back to at_ms 100 (its run rows to 1000; in the window, 5000)"),
        "{text}"
    );
}

/// `--since` takes a count and its unit.
#[test]
fn since_reads_a_count_and_its_unit() {
    assert_eq!(duration_ms("24h"), Some(24 * HOUR_MS));
    assert_eq!(duration_ms("90m"), Some(90 * 60_000));
    assert_eq!(duration_ms("2d"), Some(48 * HOUR_MS));
    assert_eq!(duration_ms("30s"), Some(30_000));
    for bad in ["24", "h", "0h", "1w", "-1h", "1.5h"] {
        assert_eq!(duration_ms(bad), None, "{bad}");
    }
}
