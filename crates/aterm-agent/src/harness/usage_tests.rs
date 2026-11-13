// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PANEL_WIDE: &str = "   Settings  Status   Config   Usage   Stats

   Session

   Total cost:            $0.0000

   Current session
   ███▌                                               7% used
   Resets 1:20pm (America/Los_Angeles)

   Current week (all models)
   ███████████████████████████████                    62% used
   Resets Sep 23 at 12pm (America/Los_Angeles)

   Current week (Fable)
   ██████████████████████████████████████████████████ 100% used
   Resets Sep 23 at 11:59am (America/Los_Angeles)

   What's contributing to your limits usage?
   Approximate, based on local sessions on this machine

   100% of your usage came from subagent-heavy sessions
";

const PANEL_NARROW: &str = "   Current session · Resets 1:20pm
   (America/Los_Angeles)
   ███▊

   7% used

   Current week (all models) · Resets Sep 23 at 12pm
   (America/Los_Angeles)
   █████████████████████████████████▍

   62% used

   Current week (Fable) · Resets Sep 23 at 11:59am
   (America/Los_Angeles)
   ██████████████████████████████████████████████████
   ████
   100% used

   What's contributing to your limits usage?
";

fn rows(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

fn assistant_row(id: &str, model: &str, input: u64, output: u64) -> String {
    format!(
        r#"{{"type":"assistant","isSidechain":false,"uuid":"u-{id}","message":{{"id":"{id}","model":"{model}","role":"assistant","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":10,"cache_read_input_tokens":1000,"service_tier":"standard"}}}}}}"#
    )
}

fn fold_all(t: &mut TranscriptUsage, lines: &[&str]) -> Vec<Fold> {
    lines.iter().map(|l| t.fold_line(l)).collect()
}

fn view_with(acct: AccountView) -> UsageView {
    let mut v = UsageView::new(1_789_650_000);
    v.accounts.push(acct);
    v
}

fn live_account() -> AccountView {
    let mut a = AccountView::new("work", true);
    a.model = Some("claude-fable-5-1".to_owned());
    a
}

#[test]
fn numbers_are_read_as_integer_or_float_and_junk_is_absent() {
    let v: Value = aterm_json::from_str(
        r#"{"i":62,"f":62.7,"neg":-1,"big":1e30,"s":"62","n":null,"nan_like":"NaN"}"#,
    )
    .expect("parses");
    assert_eq!(num_f64(v.get("i")), Some(62.0));
    assert_eq!(num_f64(v.get("f")), Some(62.7));
    assert_eq!(num_u64(v.get("f")), Some(62));
    assert_eq!(num_u64(v.get("neg")), None);
    assert_eq!(num_u64(v.get("big")), Some(u64::MAX), "saturates");
    assert_eq!(num_f64(v.get("s")), None);
    assert_eq!(num_f64(v.get("n")), None);
    assert_eq!(num_f64(v.get("missing")), None);
}

#[test]
fn transcript_duplicate_message_ids_are_summed_once() {
    let mut t = TranscriptUsage::new();
    let a = assistant_row("msg_a", "claude-fable-5-1", 100, 10);
    let folds = fold_all(&mut t, &[&a, &a, &a]);
    assert_eq!(folds, [Fold::Summed, Fold::Duplicate, Fold::Duplicate]);
    let s = t.per_model()["claude-fable-5-1"];
    assert_eq!(
        (s.input, s.output, s.cache_write, s.cache_read),
        (100, 10, 10, 1000)
    );
    assert_eq!(s.messages, 1);
    assert_eq!(t.duplicates, 2);
    assert_eq!(t.conflicting_repeats, 0);
    assert_eq!(t.assistant_rows, 3);
}

#[test]
fn transcript_distinct_ids_sum_per_model() {
    let mut t = TranscriptUsage::new();
    fold_all(
        &mut t,
        &[
            &assistant_row("msg_a", "claude-fable-5-1", 100, 10),
            &assistant_row("msg_b", "claude-fable-5-1", 50, 5),
            &assistant_row("msg_c", "claude-opus-5", 7, 3),
        ],
    );
    assert_eq!(t.per_model()["claude-fable-5-1"].input, 150);
    assert_eq!(t.per_model()["claude-fable-5-1"].messages, 2);
    assert_eq!(t.per_model()["claude-opus-5"].output, 3);
    let total = t.total();
    assert_eq!((total.input, total.output, total.messages), (157, 18, 3));
}

#[test]
fn transcript_rows_without_usage_are_counted_not_summed() {
    let mut t = TranscriptUsage::new();
    let folds = fold_all(
        &mut t,
        &[
            r#"{"type":"assistant","message":{"id":"msg_x","model":"m","content":[]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_y","model":"m","usage":"none"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_z","model":"m","usage":{"service_tier":"standard"}}}"#,
            r#"{"type":"assistant"}"#,
        ],
    );
    assert!(folds.iter().all(|f| *f == Fold::NoUsage), "{folds:?}");
    assert_eq!(t.rows_without_usage, 4);
    assert!(t.per_model().is_empty());
}

#[test]
fn transcript_non_assistant_rows_are_ignored() {
    let mut t = TranscriptUsage::new();
    let folds = fold_all(
        &mut t,
        &[
            r#"{"type":"user","message":{"role":"user","content":"hi","usage":{"input_tokens":999}}}"#,
            r#"{"type":"progress","data":{}}"#,
            r#"{"type":"summary","summary":"x"}"#,
            r#"{"message":{"usage":{"input_tokens":5}}}"#,
            "   ",
        ],
    );
    assert_eq!(
        folds,
        [
            Fold::NotAssistant,
            Fold::NotAssistant,
            Fold::NotAssistant,
            Fold::NotAssistant,
            Fold::Blank
        ]
    );
    assert!(t.per_model().is_empty());
    assert_eq!(t.rows, 5);
    assert_eq!(t.assistant_rows, 0);
}

#[test]
fn the_byte_prefilter_never_drops_a_row_the_parser_would_have_summed() {
    let mut t = TranscriptUsage::new();
    let escaped = r#"{"type":"\u0061ssistant","message":{"id":"msg_esc","model":"m","usage":{"input_tokens":7}}}"#;
    assert!(!cannot_be_assistant(escaped), "a `\\u` line must be parsed");
    assert_eq!(t.fold_line(escaped), Fold::Summed);
    assert_eq!(t.total().input, 7);
    assert_eq!(t.assistant_rows, 1);

    for skipped in [
        r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        r#"{"type":"attachment","data":{"path":"/tmp/x"}}"#,
        r#"{"type":"user","message":{"content":"a \"quoted\" \\ tab\there"}}"#,
    ] {
        assert!(cannot_be_assistant(skipped), "{skipped}");
        assert_eq!(t.fold_line(skipped), Fold::NotAssistant, "{skipped}");
    }

    for parsed in [
        r#"{"type":"assistant","message":{}}"#,
        r#"{"type":"user","text":"\u00e9"}"#,
        r#"["type","user"]"#,
        r#"{"type":"user""#,
    ] {
        assert!(!cannot_be_assistant(parsed), "{parsed}");
    }

    let mut u = TranscriptUsage::new();
    assert_eq!(
        u.fold_line(r#"{"type":"assistant","message":{"usage":}"#),
        Fold::Unparsed,
        "the corruption that loses spend is still seen"
    );
    assert_eq!(u.rows_unparsed, 1);
    assert_eq!(
        u.fold_line(r#"{"type":"user","message":{"content":}"#),
        Fold::NotAssistant
    );
    assert_eq!(u.rows_unparsed, 1, "unchanged: it was never parsed");
}

#[test]
fn transcript_sidechain_row_is_summed_and_counted() {
    let mut t = TranscriptUsage::new();
    let side = assistant_row("msg_side", "claude-haiku-4-5", 20, 2)
        .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert!(side.contains(r#""isSidechain":true"#));
    assert_eq!(t.fold_line(&side), Fold::Summed);
    assert_eq!(t.sidechain_rows, 1);
    assert_eq!(t.per_model()["claude-haiku-4-5"].input, 20);
    assert_eq!(t.fold_line(&side), Fold::Duplicate);
    assert_eq!(t.sidechain_rows, 2, "every sidechain row is counted");
    assert_eq!(t.per_model()["claude-haiku-4-5"].messages, 1);
}

#[test]
fn transcript_very_long_line_is_skipped_and_counted() {
    let mut t = TranscriptUsage::new();
    let long = format!(
        r#"{{"type":"assistant","message":{{"id":"msg_l","model":"m","usage":{{"input_tokens":1}},"pad":"{}"}}}}"#,
        "x".repeat(MAX_LINE_BYTES)
    );
    assert!(long.len() > MAX_LINE_BYTES);
    assert_eq!(t.fold_line(&long), Fold::TooLong);
    assert_eq!(t.rows_skipped_long, 1);
    assert!(t.per_model().is_empty());
    let real_sized = format!(
        r#"{{"type":"assistant","message":{{"id":"msg_r","model":"m","usage":{{"input_tokens":1}},"pad":"{}"}}}}"#,
        "x".repeat(1_125_606)
    );
    assert_eq!(t.fold_line(&real_sized), Fold::Summed);
}

#[test]
fn transcript_malformed_lines_are_counted_unparsed() {
    let mut t = TranscriptUsage::new();
    let folds = fold_all(
        &mut t,
        &[
            "{\"type\":\"assistant\",",
            "[1,2]",
            "\"assistant\"",
            "{\"type\":\"assistant\"} x",
        ],
    );
    assert!(folds.iter().all(|f| *f == Fold::Unparsed), "{folds:?}");
    assert_eq!(t.rows_unparsed, 4);
    assert_eq!(t.rows, 4);
}

#[test]
fn transcript_differing_repeat_raises_to_the_largest_and_counts_once() {
    let mut t = TranscriptUsage::new();
    let first = assistant_row("msg_c", "m", 100, 10);
    let grown = assistant_row("msg_c", "m", 100, 999);
    let lower = assistant_row("msg_c", "m", 50, 20);
    assert_eq!(t.fold_line(&first), Fold::Summed);
    assert_eq!(t.fold_line(&grown), Fold::Conflict);
    assert_eq!(t.fold_line(&lower), Fold::Conflict);
    let m = t.per_model()["m"];
    assert_eq!(m.output, 999, "the largest output, not the first");
    assert_eq!(m.input, 100, "a lower count never lowers what is held");
    assert_eq!(m.messages, 1);
    assert_eq!(t.conflicting_repeats, 2);
    assert_eq!(t.duplicates, 2);
}

fn stamped_row(id: &str, input: u64, output: u64, at: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{at}","isSidechain":false,"message":{{"id":"{id}","model":"m","usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
    )
}

fn unix(at: &str) -> i64 {
    i64::try_from(super::super::unix_time::parse_utc(at).expect("a stamp")).unwrap()
}

fn totals(t: &TranscriptUsage) -> (u64, u64) {
    t.per_model().values().fold((0, 0), |(i, o), spend| {
        let tokens = spend.tokens();
        (i + tokens.prompt(), o + tokens.output)
    })
}

#[test]
fn the_series_buckets_tokens_by_row_minute() {
    let mut t = TranscriptUsage::new();
    fold_all(
        &mut t,
        &[
            &stamped_row("a", 100, 1, "2026-10-19T12:00:05Z"),
            &stamped_row("b", 200, 2, "2026-10-19T12:00:59.900Z"),
            &stamped_row("c", 400, 4, "2026-10-19T12:01:00Z"),
            &stamped_row("d", 800, 8, "2026-10-19T12:03:10Z"),
            &assistant_row("e", "m", 1, 16).replacen(
                '{',
                r#"{"timestamp":"2026-10-19T12:03:59Z","#,
                1,
            ),
        ],
    );
    let series = t.series();
    assert_eq!(series.width_s(), SERIES_BASE_S);
    assert_eq!(series.start_s(), unix("2026-10-19T12:00:00Z"));
    assert_eq!(
        series.buckets().iter().copied().collect::<Vec<_>>(),
        [(300, 3), (400, 4), (0, 0), (800 + 1 + 10 + 1000, 8 + 16)]
    );
    assert_eq!(series.sum(), totals(&t), "every row graphed");
    assert_eq!(t.unstamped(), 0);
}

#[test]
fn the_series_coarsens_and_keeps_the_sum() {
    let base = unix("2026-10-19T00:00:00Z");
    let mut series = TokenSeries::default();
    let mut sum = (0u64, 0u64);
    for minute in (0..300).chain((-120..0).rev()) {
        let (i, o) = (u64::try_from(minute + 1000).unwrap(), 1);
        series.note(base + minute * 60 + 7, i, o);
        sum = (sum.0 + i, sum.1 + o);
        assert!(series.buckets().len() <= SERIES_BUCKETS);
        assert_eq!(series.sum(), sum, "nothing dropped at minute {minute}");
    }
    assert_eq!(series.width_s(), 4 * SERIES_BASE_S);
    assert_eq!(series.start_s().rem_euclid(series.width_s()), 0);
    assert!(series.start_s() <= base - 120 * 60);
    assert!(series.start_s() + series.width_s() * 120 > base + 299 * 60);
    let held = series.clone();
    series.note(base + 86_400 * 365, 0, 0);
    assert_eq!(series, held);
    for at in [i64::MIN, i64::MAX, 0] {
        series.note(at, 5, 5);
        sum = (sum.0 + 5, sum.1 + 5);
    }
    assert!(series.buckets().len() <= SERIES_BUCKETS);
    assert_eq!(series.sum(), sum);
}

#[test]
fn absorb_aligns_two_widths() {
    let base = unix("2026-10-19T09:00:00Z");
    let fine: Vec<(i64, u64, u64)> = (0..10).map(|m| (base + m * 60, 10, 1)).collect();
    let coarse: Vec<(i64, u64, u64)> = (0..180)
        .step_by(7)
        .map(|m| (base - 3600 + m * 60 + 30, 100, 3))
        .collect();
    let of = |rows: &[(i64, u64, u64)]| {
        let mut series = TokenSeries::default();
        for &(at, i, o) in rows {
            series.note(at, i, o);
        }
        series
    };
    let (a, b) = (of(&fine), of(&coarse));
    assert_eq!(
        (a.width_s(), b.width_s()),
        (SERIES_BASE_S, 2 * SERIES_BASE_S)
    );
    let mut ab = a.clone();
    ab.absorb(&b);
    let mut ba = b.clone();
    ba.absorb(&a);
    let mut all = fine.clone();
    all.extend(&coarse);
    assert_eq!(ab, of(&all), "one fold of every row");
    assert_eq!(ba, ab);
    assert_eq!(ab.sum(), (a.sum().0 + b.sum().0, a.sum().1 + b.sum().1));
    let mut held = ab.clone();
    held.absorb(&TokenSeries::default());
    assert_eq!(held, ab);
    let mut empty = TokenSeries::default();
    empty.absorb(&b);
    assert_eq!(empty, b);
    let mut naive = b.clone();
    for (k, &(i, o)) in a.buckets().iter().enumerate() {
        let slot = &mut naive.buckets[k];
        *slot = (slot.0 + i, slot.1 + o);
    }
    assert_ne!(naive, ab);
}

#[test]
fn a_grown_repeat_adds_only_its_growth() {
    let mut t = TranscriptUsage::new();
    let first = stamped_row("msg_c", 100, 10, "2026-10-19T12:00:10Z");
    let grown = stamped_row("msg_c", 100, 999, "2026-10-19T12:01:10Z");
    let lower = stamped_row("msg_c", 50, 20, "2026-10-19T12:02:10Z");
    assert_eq!(
        fold_all(&mut t, &[&first, &grown, &lower]),
        [Fold::Summed, Fold::Conflict, Fold::Conflict]
    );
    assert_eq!(
        t.series().buckets().iter().copied().collect::<Vec<_>>(),
        [(100, 10), (0, 989)],
        "the growth alone, in its own row's minute; a lower repeat adds nothing"
    );
    assert_eq!(t.series().sum(), totals(&t));
    assert_eq!(totals(&t), (100, 999));
}

#[test]
fn a_row_without_a_timestamp_counts_in_the_totals_not_the_series() {
    let mut t = TranscriptUsage::new();
    let bare = assistant_row("n1", "m", 100, 10);
    assert_eq!(t.fold_line(&bare), Fold::Summed);
    assert_eq!(totals(&t), (100 + 10 + 1000, 10));
    assert!(t.series().is_empty());
    assert_eq!(t.unstamped(), 1);
    let generation = t.generation();
    assert_ne!(generation, 0);
    assert_eq!(t.fold_line(&bare), Fold::Duplicate);
    assert_eq!(t.generation(), generation, "a duplicate sums nothing");
    let stamped = assistant_row("n2", "m", 100, 10).replacen(
        '{',
        r#"{"timestamp":"2026-10-19T12:00:00Z","#,
        1,
    );
    assert_eq!(t.fold_line(&stamped), Fold::Summed);
    assert_eq!(t.series().sum(), (100 + 10 + 1000, 10));
    assert_eq!(t.unstamped(), 1);
    assert_ne!(t.generation(), generation);
    let mut whole = TranscriptUsage::new();
    whole.absorb(&t);
    whole.absorb(&t);
    assert_eq!((whole.unstamped(), whole.series().sum()), (2, (2220, 20)));
}

#[test]
fn columns_spread_and_keep_the_sum() {
    let start = unix("2026-10-19T12:00:00Z");
    let mut series = TokenSeries::default();
    series.note(start + 5, 60, 6);
    series.note(start + 65, 120, 0);
    series.note(start + 185, 1, 7);
    assert_eq!(series.columns(start, start + 60, 3), [(20, 2); 3]);
    assert_eq!(series.columns(start, start + 30, 1), [(60, 6)]);
    assert_eq!(
        series.columns(start, start + 90, 3),
        [(30, 3), (30, 3), (120, 0)],
        "a window ending mid-minute holds the minute"
    );
    let whole = series.sum();
    for n in 1..=97 {
        let columns = series.columns(start - 17, start + 240 + 13, n);
        let sum = columns
            .iter()
            .fold((0, 0), |(i, o), &(ci, co)| (i + ci, o + co));
        assert_eq!(sum, whole, "{n} columns");
    }
    let lone = series.columns(start + 180, start + 240, 3);
    assert_eq!(lone.iter().map(|c| c.0).sum::<u64>(), 1);
    assert_eq!(lone.iter().map(|c| c.1).sum::<u64>(), 7);
    assert!(series.columns(start, start + 60, 0).is_empty());
    assert_eq!(series.columns(start + 60, start, 2), [(0, 0); 2]);
    assert_eq!(
        TokenSeries::default().columns(start, start + 60, 2),
        [(0, 0); 2]
    );
    let view = crate::harness::session_usage::TrafficView {
        start_s: series.start_s(),
        width_s: series.width_s(),
        buckets: series.buckets().iter().copied().collect(),
        ..Default::default()
    };
    for n in [1, 3, 7, 60] {
        assert_eq!(
            view.columns(start - 17, start + 253, n),
            series.columns(start - 17, start + 253, n),
            "{n} columns"
        );
    }
    assert_eq!(
        crate::harness::session_usage::TrafficView::default().columns(start, start + 60, 2),
        [(0, 0); 2]
    );
}

#[test]
fn working_time_is_the_union_of_the_main_chains_turns() {
    let turn = |at: &str, ms: u64, side: bool| {
        format!(
            r#"{{"parentUuid":"p","isSidechain":{side},"type":"system","subtype":"turn_duration","durationMs":{ms},"messageCount":28,"timestamp":"2026-09-29T15:{at}Z","uuid":"u","isMeta":false,"version":"2.1.284"}}"#
        )
    };
    let mut t = TranscriptUsage::new();
    assert_eq!(t.fold_line(&turn("00:10.500", 10_500, false)), Fold::Turn);
    assert_eq!(t.busy_ms(), 10_500);
    fold_all(
        &mut t,
        &[
            &turn("00:30.000", 10_000, false),
            &turn("01:00.000", 55_000, false),
            &turn("02:00.000", 30_000, true),
        ],
    );
    assert_eq!(t.busy_ms(), 60_000, "the union, not the 75.5 s sum");
    assert_eq!(t.assistant_rows, 0, "no turn row is an answer");
}

#[test]
fn a_streamed_subagent_message_counts_its_final_output_once() {
    let row = |stop: &str, output: u64, block: &str| {
        format!(
            r#"{{"type":"assistant","isSidechain":true,"message":{{"id":"msg_011Cej","model":"claude-opus-5","role":"assistant","stop_reason":{stop},"content":[{{"type":"{block}"}}],"usage":{{"input_tokens":3,"output_tokens":{output},"cache_creation_input_tokens":1200,"cache_read_input_tokens":20000}}}}}}"#
        )
    };
    let mut t = TranscriptUsage::new();
    fold_all(
        &mut t,
        &[
            &row("null", 8, "thinking"),
            &row("null", 8, "tool_use"),
            &row("\"tool_use\"", 349, "tool_use"),
        ],
    );
    let m = t.per_model()["claude-opus-5"];
    assert_eq!(m.output, 349);
    assert_eq!(m.messages, 1);
    assert_eq!((m.input, m.cache_write, m.cache_read), (3, 1200, 20_000));
}

#[test]
fn a_limit_notice_row_is_kept_with_its_time() {
    let notice = |text: &str, ts: &str, api_error: bool| {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","isApiErrorMessage":{api_error},"error":"rate_limit","message":{{"id":"x-{ts}","model":"<synthetic>","role":"assistant","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":0,"output_tokens":0}}}}}}"#
        )
    };
    let mut t = TranscriptUsage::new();
    fold_all(
        &mut t,
        &[
            &notice(
                "You've hit your session limit · resets 3pm (America/Los_Angeles)",
                "2026-09-24T21:00:00.000Z",
                true,
            ),
            &notice(
                "You've hit your session limit · resets 1pm (America/Los_Angeles)",
                "2026-09-23T19:00:00Z",
                true,
            ),
            &notice(
                "You've hit your weekly limit · resets Oct 1 at 9am (UTC)",
                "2026-09-24T20:30:00Z",
                true,
            ),
            &notice(
                "You've hit your limit · resets 4pm",
                "2026-09-25T00:00:00Z",
                true,
            ),
            &notice(
                "You've hit your session limit · resets 5pm",
                "2026-09-25T00:00:00Z",
                false,
            ),
        ],
    );
    let rows = t.limit_notices();
    assert_eq!(
        rows.get("five_hour"),
        Some(&LimitNoticeRow {
            reset_text: Some("3pm (America/Los_Angeles)".into()),
            at: 1_790_283_600,
        }),
        "the newest, not the later-written older one; not the quoted words"
    );
    assert_eq!(
        rows.get("seven_day")
            .map(|r| (r.reset_text.as_deref(), r.at)),
        Some((Some("Oct 1 at 9am (UTC)"), 1_790_281_800)),
    );
    assert_eq!(rows.len(), 2, "a notice naming no window is not kept");
    assert_eq!(t.served_at(), None, "a notice is no served response");
    let mut summed = TranscriptUsage::new();
    summed.absorb(&t);
    assert!(
        summed.limit_notices().is_empty(),
        "a sum carries no wall: the footer reads the main file's own fold"
    );
}

#[test]
fn projected_usage_preserves_the_live_limit_wall_and_its_lift() {
    let at = |minute: u32| format!("2026-09-28T12:{minute:02}:00Z");
    let unix = |minute: u32| {
        i64::try_from(super::super::unix_time::parse_utc(&at(minute)).unwrap()).unwrap()
    };
    let answer = |output: u64, minute: u32, api_error: bool| {
        format!(
            r#"{{"type":"assistant","message":{{"id":"m1","model":"claude-opus-5-5","usage":{{"input_tokens":1,"output_tokens":{output}}}}},"timestamp":"{}","isApiErrorMessage":{api_error}}}"#,
            at(minute)
        )
    };
    for content in [
        r#""You've hit your session limit · resets 3pm (UTC)""#,
        r#"[{"type":"tool_result","text":"not the notice","content":{"output":"ignored"}},null,["nested"],{"type":"text","text":"You've hit your session limit · resets 3pm (UTC)"}]"#,
    ] {
        let mut t = TranscriptUsage::new();
        assert_eq!(t.fold_line(&answer(5, 0, false)), Fold::Summed);
        let notice = format!(
            r#"{{"type":"assistant","isApiErrorMessage":false,"timestamp":"2026-09-27T12:00:00Z","message":{{"id":"notice","model":"<synthetic>","content":{content}}},"isApiErrorMess\u0061ge":true,"tim\u0065stamp":"{}"}}"#,
            at(1)
        );
        assert_eq!(t.fold_line(&notice), Fold::NoUsage);
        assert_eq!(
            t.limit_notices().get("five_hour"),
            Some(&LimitNoticeRow {
                reset_text: Some("3pm (UTC)".into()),
                at: unix(1),
            })
        );
        let wall = |t: &TranscriptUsage| {
            super::super::session_usage::wall_of(
                t.limit_notices(),
                t.served_at(),
                unix(4),
                &|_, _| Some(0),
            )
        };
        assert!(wall(&t).is_some(), "the new notice reaches the live wall");
        assert_eq!(t.fold_line(&answer(5, 2, false)), Fold::Duplicate);
        assert_eq!(t.fold_line(&answer(9, 3, true)), Fold::Conflict);
        assert_eq!(t.served_at(), Some(unix(0)));
        assert!(
            wall(&t).is_some(),
            "a repeated response or API-error row cannot lift the wall"
        );
        assert_eq!(t.fold_line(&answer(12, 4, false)), Fold::Conflict);
        assert_eq!(t.served_at(), Some(unix(4)));
        assert!(wall(&t).is_none(), "new served output lifts the wall");
        let counts = t.per_model()["claude-opus-5-5"];
        assert_eq!((counts.output, counts.messages), (12, 1));
    }
}

#[test]
fn the_served_time_is_the_newest_served_response() {
    let at = |minute: u32| format!("2026-09-24T21:{minute:02}:00.000Z");
    let row = |id: Option<&str>, model: &str, out: u64, minute: u32, api_error: bool| {
        let id = id.map_or(String::new(), |id| format!("\"id\":\"{id}\","));
        format!(
            r#"{{"type":"assistant","timestamp":"{}","isApiErrorMessage":{api_error},"message":{{{id}"model":"{model}","usage":{{"input_tokens":1,"output_tokens":{out}}}}}}}"#,
            at(minute)
        )
    };
    let minute = |m: i64| 1_790_283_600 + m * 60;
    let mut t = TranscriptUsage::new();
    t.fold_line(&row(Some("n"), "<synthetic>", 0, 1, true));
    assert_eq!(t.served_at(), None, "a limit notice");
    t.fold_line(&row(Some("m1"), "claude-opus-5-5", 5, 2, false));
    assert_eq!(t.served_at(), Some(minute(2)));
    t.fold_line(&row(Some("m1"), "claude-opus-5-5", 5, 3, false));
    assert_eq!(t.served_at(), Some(minute(2)), "the same figures again");
    t.fold_line(&row(Some("m1"), "claude-opus-5-5", 9, 4, false));
    assert_eq!(t.served_at(), Some(minute(4)), "the message grew");
    t.fold_line(&row(Some("m1"), "claude-opus-5-5", 2, 30, false));
    assert_eq!(
        t.served_at(),
        Some(minute(4)),
        "a repeat BELOW the figures held grew nothing: not served"
    );
    t.fold_line(&row(None, "claude-opus-5-5", 1, 5, false));
    assert_eq!(t.served_at(), Some(minute(5)), "a row with no id");
    t.fold_line(&row(Some("s"), "<synthetic>", 3, 6, false));
    t.fold_line(&row(Some("e"), "claude-opus-5-5", 3, 7, true));
    t.fold_line(&format!(
        r#"{{"type":"assistant","timestamp":"{}","message":{{"id":"u","model":"claude-opus-5-5"}}}}"#,
        at(8)
    ));
    t.fold_line(&format!(
        r#"{{"type":"assistant","timestamp":"{}","message":{{"id":"v","usage":{{"input_tokens":1}}}}}}"#,
        at(9)
    ));
    assert_eq!(
        t.served_at(),
        Some(minute(5)),
        "synthetic, an API error, no usage, no model: none served"
    );
    t.fold_line(&row(Some("m2"), "claude-opus-5-5", 1, 1, false));
    assert_eq!(t.served_at(), Some(minute(5)), "the NEWEST time is kept");
}

#[test]
fn a_notice_names_its_window_or_none() {
    for (text, want) in [
        (
            "You've hit your session limit · resets 3pm",
            Some("five_hour"),
        ),
        (
            "You\u{2019}ve hit your weekly limit · resets Oct 1 at 9am",
            Some("seven_day"),
        ),
        ("You've hit your limit · resets 4pm", None),
        ("Approaching usage limit · resets 3pm", None),
        ("Login expired · Please run /login", None),
        ("the session limit is a thing", None),
    ] {
        assert_eq!(notice_window(text), want, "{text}");
    }
}

#[test]
fn transcript_rows_without_id_or_with_a_huge_id_are_summed_and_counted() {
    let mut t = TranscriptUsage::new();
    let no_id = r#"{"type":"assistant","message":{"model":"m","usage":{"input_tokens":1}}}"#;
    let empty_id =
        r#"{"type":"assistant","message":{"id":"","model":"m","usage":{"input_tokens":1}}}"#;
    let huge_id = assistant_row(&"i".repeat(MAX_ID_BYTES + 1), "m", 1, 0);
    let folds = fold_all(&mut t, &[no_id, no_id, empty_id, &huge_id, &huge_id]);
    assert!(folds.iter().all(|f| *f == Fold::Summed), "{folds:?}");
    assert_eq!(t.rows_without_id, 5);
    assert_eq!(
        t.per_model()["m"].input,
        5,
        "cannot dedupe, so every row counts"
    );
    assert_eq!(t.duplicates, 0);
}

#[test]
fn transcript_missing_model_files_under_unknown_and_missing_fields_read_zero() {
    let mut t = TranscriptUsage::new();
    let row = r#"{"type":"assistant","message":{"id":"msg_u","usage":{"output_tokens":4}}}"#;
    assert_eq!(t.fold_line(row), Fold::Summed);
    let s = t.per_model()[UNKNOWN_MODEL];
    assert_eq!(
        (s.input, s.output, s.cache_write, s.cache_read),
        (0, 4, 0, 0)
    );
    let row = r#"{"type":"assistant","message":{"id":"msg_f","model":"m","usage":{"input_tokens":12.9,"output_tokens":-5}}}"#;
    assert_eq!(t.fold_line(row), Fold::Summed);
    assert_eq!(
        (t.per_model()["m"].input, t.per_model()["m"].output),
        (12, 0)
    );
}

#[test]
fn transcript_seen_set_is_bounded_and_the_bound_is_what_it_says() {
    let mut t = TranscriptUsage::new();
    let first = assistant_row("msg_first", "m", 1, 0);
    assert_eq!(t.fold_line(&first), Fold::Summed);
    for i in 0..(SEEN_IDS_MAX - 1) {
        assert_eq!(
            t.fold_line(&assistant_row(&format!("msg_{i}"), "m", 1, 0)),
            Fold::Summed
        );
    }
    assert_eq!(t.fold_line(&first), Fold::Duplicate);
    assert_eq!(
        t.fold_line(&assistant_row("msg_evictor", "m", 1, 0)),
        Fold::Summed
    );
    assert_eq!(t.fold_line(&first), Fold::Summed);
    assert_eq!(t.per_model()["m"].messages as usize, SEEN_IDS_MAX + 2);
}

#[test]
fn transcript_model_count_is_bounded_into_other() {
    let mut t = TranscriptUsage::new();
    for i in 0..(MAX_MODELS + 3) {
        t.fold_line(&assistant_row(
            &format!("msg_{i}"),
            &format!("model-{i:03}"),
            1,
            0,
        ));
    }
    assert_eq!(t.per_model().len(), MAX_MODELS + 1);
    assert_eq!(t.per_model()[OTHER_MODEL].messages, 3);
    t.fold_line(&assistant_row("msg_again", "model-000", 1, 0));
    assert_eq!(t.per_model()["model-000"].messages, 2);
}

#[test]
fn one_hour_cache_writes_are_kept_apart() {
    let split = r#"{"type":"assistant","message":{"id":"msg_s","model":"m","usage":{"input_tokens":10,"cache_creation_input_tokens":1000,"cache_read_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":200,"ephemeral_1h_input_tokens":800},"output_tokens":0}}}"#;
    let over = r#"{"type":"assistant","message":{"id":"msg_o","model":"o","usage":{"cache_creation_input_tokens":5,"cache_creation":{"ephemeral_1h_input_tokens":9}}}}"#;
    let only_split = r#"{"type":"assistant","message":{"id":"msg_p","model":"p","usage":{"cache_creation":{"ephemeral_5m_input_tokens":2,"ephemeral_1h_input_tokens":3}}}}"#;
    let bare = r#"{"type":"assistant","message":{"id":"msg_b","model":"b","usage":{"cache_creation_input_tokens":1000}}}"#;
    let mut t = TranscriptUsage::new();
    for line in [split, over, only_split, bare] {
        assert_eq!(t.fold_line(line), Fold::Summed);
    }
    let held = |t: &TranscriptUsage, m: &str| {
        let s = t.per_model()[m];
        (s.cache_write, s.cache_write_1h)
    };
    assert_eq!(held(&t, "m"), (1000, 800));
    assert_eq!(held(&t, "o"), (5, 5));
    assert_eq!(held(&t, "p"), (5, 3));
    assert_eq!(held(&t, "b"), (1000, 0));
    t.fold_line(&split.replace("800}", "900}").replace(":1000,", ":1100,"));
    assert_eq!(held(&t, "m"), (1100, 900));
}

#[test]
fn the_account_model_is_the_newest_main_chain_row_of_its_transcript() {
    let mut t = TranscriptUsage::new();
    assert_eq!(t.last_model(), None);
    let side = r#"{"type":"assistant","isSidechain":true,"message":{"id":"msg_s","model":"claude-haiku-4-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
    fold_all(
        &mut t,
        &[
            &assistant_row("msg_a", "claude-opus-5", 10, 1),
            &assistant_row("msg_b", "claude-fable-5-1", 10, 1),
            side,
        ],
    );
    assert_eq!(t.last_model(), Some("claude-fable-5-1"));
    let mut a = AccountView::new("work", true);
    a.add_transcript(&t);
    assert_eq!(a.model.as_deref(), Some("claude-fable-5-1"));
    let mut none = TranscriptUsage::new();
    fold_all(
        &mut none,
        &[r#"{"type":"assistant","message":{"id":"msg_n","usage":{"input_tokens":1}}}"#],
    );
    assert_eq!(none.last_model(), None);
    let mut a = AccountView::new("work", true);
    a.add_transcript(&none);
    assert_eq!(a.model, None);
}

#[test]
fn the_session_sum_names_the_main_transcripts_model() {
    let mut main = TranscriptUsage::new();
    fold_all(
        &mut main,
        &[&assistant_row("msg_a", "claude-fable-5-1", 10, 1)],
    );
    let side = assistant_row("msg_s", "claude-haiku-4-5", 1, 1)
        .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    let mut sub = TranscriptUsage::new();
    fold_all(&mut sub, &[&side]);
    assert_eq!(sub.last_model(), None, "a sidechain row names no model");
    let mut all = TranscriptUsage::new();
    all.absorb(&main);
    all.absorb(&sub);
    assert_eq!(all.last_model(), Some("claude-fable-5-1"));
    assert_eq!(all.per_model().len(), 2, "both files' spend is summed");
    let mut a = AccountView::new("account", true);
    a.add_transcript(&all);
    assert_eq!(a.model.as_deref(), Some("claude-fable-5-1"));
    let mut none = TranscriptUsage::new();
    none.absorb(&sub);
    none.absorb(&TranscriptUsage::new());
    assert_eq!(none.last_model(), None);
}

#[test]
fn a_synthetic_row_never_names_the_sessions_model() {
    let synthetic = r#"{"type":"assistant","isSidechain":false,"message":{"id":"msg_x","model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your limit"}],"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
    let mut t = TranscriptUsage::new();
    fold_all(
        &mut t,
        &[
            &assistant_row("msg_a", "claude-fable-5-1", 10, 1),
            synthetic,
        ],
    );
    assert_eq!(t.last_model(), Some("claude-fable-5-1"));
    let mut a = AccountView::new("account", true);
    a.add_transcript(&t);
    assert_eq!(a.model.as_deref(), Some("claude-fable-5-1"));
    let mut only = TranscriptUsage::new();
    fold_all(&mut only, &[synthetic]);
    assert_eq!(only.last_model(), None);
    fold_all(&mut t, &[&assistant_row("msg_b", "claude-opus-5", 10, 1)]);
    assert_eq!(t.last_model(), Some("claude-opus-5"));
}

#[test]
fn view_shown_is_the_first_active_account_else_the_first() {
    let mut v = UsageView::new(0);
    assert!(v.shown().is_none());
    v.accounts.push(AccountView::new("alt-1", false));
    v.accounts.push(AccountView::new("work", true));
    v.accounts.push(AccountView::new("work-2", true));
    assert_eq!(v.shown().map(|a| a.label.as_str()), Some("work"));
    v.accounts.iter_mut().for_each(|a| a.active = false);
    assert_eq!(v.shown().map(|a| a.label.as_str()), Some("alt-1"));
}

#[test]
fn a_tap_reading_fills_the_windows_of_the_json() {
    let reading = crate::harness::tap::Reading {
        at: 1_000,
        sid: "s-1".to_string(),
        pid: 7,
        session: None,
        login: None,
        home: None,
        api_ms: 5,
        five_hour: Some(crate::harness::tap::Window {
            pct: 42,
            resets_at: 1_000 + 2 * 3_600 + 30 * 60,
            pct_x10: None,
        }),
        seven_day: None,
        warm: None,
        cache_expires_at: None,
        version: None,
        model: None,
        ctx_left: None,
        dur_us: 0,
        row: None,
    };
    let mut a = live_account();
    a.add_reading(&reading, 1_012);
    let mut view = UsageView::new(1_012);
    view.accounts.push(a);
    let json: Value = aterm_json::from_str(&usage_json(&view)).expect("json");
    let five = &json["accounts"][0]["windows"]["five_hour"];
    assert_eq!(five["used_pct"], Value::from(42u64));
    assert_eq!(five["source"], Value::from("tap"));
    assert_eq!(five["age_s"], Value::from(12u64));
    assert!(json["accounts"][0]["windows"].get("seven_day").is_none());
    let bare: Value = aterm_json::from_str(&usage_json(&view_with(live_account()))).expect("json");
    assert_eq!(bare["accounts"][0]["windows"], Value::Object(Map::new()));
    assert!(bare["accounts"][0].get("missing").is_none(), "no gap noted");
}

/// The reading Claude Code 2.1.316 leaves a login whose replies carry the week and no
/// five-hour window (`tap::tests`' `claude-2.1.316-statusline-week-only.json`).
fn week_only_reading() -> crate::harness::tap::Reading {
    crate::harness::tap::Reading {
        at: 1_793_473_703,
        sid: "s-1".to_string(),
        pid: 7,
        session: None,
        login: None,
        home: None,
        api_ms: 219_320_877,
        five_hour: None,
        seven_day: Some(crate::harness::tap::Window {
            pct: 1,
            resets_at: 1_794_078_000,
            pct_x10: Some(10),
        }),
        warm: None,
        cache_expires_at: None,
        version: Some("2.1.316".to_string()),
        model: Some("claude-opus-5-5".to_string()),
        ctx_left: None,
        dur_us: 0,
        row: None,
    }
}

/// A WINDOW THE VENDOR DOES NOT REPORT READS `–` WITH ITS REASON, NEVER A BARE `?`: on
/// 2.1.316's week-only reading the HUD says `–/5h`, one line says why, and `--json` carries
/// `missing.five_hour = "not-reported"`; a window nobody explained still reads `?%`.
#[test]
fn a_window_the_vendor_does_not_report_reads_a_dash_with_its_reason() {
    let reading = week_only_reading();
    let now = reading.at + 5;
    let mut a = AccountView::new("m21-2", true);
    a.model = Some("claude-opus-5-5".to_owned());
    a.add_reading(&reading, now);
    let mut unexplained = UsageView::new(now);
    unexplained.accounts.push(a.clone());
    assert!(
        hud_line(&unexplained).starts_with("opus ?%/5h \u{b7} 1%/7d"),
        "{}",
        hud_line(&unexplained)
    );
    assert!(gap_lines(&unexplained).is_empty());
    let both = Consulted {
        store: true,
        usage_cache: CacheRead::Read,
    };
    a.note_gaps(Some(&reading), both, |_| false);
    let mut view = UsageView::new(now);
    view.accounts.push(a);
    assert_eq!(
        hud_line(&view),
        "opus \u{2013}/5h \u{b7} 1%/7d \u{b7} acct m21-2 \u{b7} resets in 6d23h \u{b7} tap 5s"
    );
    let why = gap_lines(&view);
    assert_eq!(
        why,
        [
            "5h \u{2013}: not reported: Claude Code reports this login's week but no 5-hour \
          window (it hands a window only while the API reports one); the accounts store and \
          the /usage cache hold none either"
        ]
    );
    let json: Value = aterm_json::from_str(&usage_json(&view)).expect("json");
    let acct = &json["accounts"][0];
    assert_eq!(acct["missing"]["five_hour"], Value::from("not-reported"));
    assert_eq!(
        acct["consulted"],
        Value::Array(vec![Value::from("store"), Value::from("usage-cache")])
    );
    assert!(acct["missing"].get("seven_day").is_none());
    assert_eq!(acct["windows"]["seven_day"]["used_pct"], Value::from(1u64));
    assert!(
        acct["windows"].get("five_hour").is_none(),
        "no figure is invented"
    );
}

/// EACH GAP IS DECIDED BY WHAT WAS READ ([`WindowGap::of`]): a reading with any figure means
/// the vendor reports none of the missing window; the store's word stands in where the
/// process left no reading; a reading with no figure, or none at all, says that instead; and
/// two windows empty for one reason share one line.
#[test]
fn each_gap_is_decided_by_what_was_read() {
    let figures = week_only_reading();
    let bare = crate::harness::tap::Reading {
        seven_day: None,
        ..week_only_reading()
    };
    for (reading, unreported, gap) in [
        (Some(&figures), false, WindowGap::NotReported),
        (Some(&figures), true, WindowGap::NotReported),
        (Some(&bare), false, WindowGap::NoFigures),
        (Some(&bare), true, WindowGap::NotReported),
        (None, false, WindowGap::NoReading),
        (None, true, WindowGap::NotReported),
    ] {
        assert_eq!(
            WindowGap::of(reading, unreported),
            gap,
            "{:?} {unreported}",
            reading.map(crate::harness::tap::Reading::has_figures)
        );
    }
    assert_eq!(
        hud_line(&view_with(live_account())),
        "fable ?%/5h \u{b7} ?%/7d \u{b7} acct work \u{b7} resets ? \u{b7} none",
        "two windows nobody explained keep `?`"
    );
    let mut a = live_account();
    a.note_gaps(None, Consulted::default(), |_| false);
    let view = view_with(a);
    assert_eq!(
        hud_line(&view),
        "fable \u{2013}/5h \u{b7} \u{2013}/7d \u{b7} acct work \u{b7} resets \u{2013} \u{b7} none"
    );
    assert_eq!(
        gap_lines(&view),
        [
            "5h, 7d \u{2013}: no usage-tap reading of this Claude Code process (it runs without \
          aterm's tap, or wrote none since it started); the accounts store and the /usage \
          cache were not read: aterm judged no account for this process"
        ]
    );
    let json: Value = aterm_json::from_str(&usage_json(&view)).expect("json");
    assert_eq!(json["accounts"][0]["consulted"], Value::Array(Vec::new()));
    let mut a = live_account();
    let store = Consulted {
        store: true,
        usage_cache: CacheRead::OtherLogin,
    };
    a.note_gaps(Some(&bare), store, |_| false);
    assert_eq!(
        gap_lines(&view_with(a)),
        [
            "5h, 7d \u{2013}: Claude Code's status line for this process carries no rate-limit \
          window: none comes before the first reply, and none for a login that is no Claude \
          subscription; the accounts store holds none either, and the /usage cache was not \
          used: it is the default home's, and this process is not shown to run on its account"
        ]
    );
}

/// NO FIGURE LOOKS FRESHER THAN IT IS: where the two windows come from different sources or
/// ages (the tap's five-hour figure beside a week the store held for days), the HUD names each
/// one's; where they share one reading it names it once.
#[test]
fn each_window_keeps_its_own_source_and_age() {
    let reading = crate::harness::tap::Reading {
        five_hour: Some(crate::harness::tap::Window {
            pct: 42,
            resets_at: 1_793_473_703 + 5 + 3_600,
            pct_x10: Some(420),
        }),
        seven_day: None,
        ..week_only_reading()
    };
    let now = reading.at + 5;
    let mut a = AccountView::new("work", true);
    a.model = Some("claude-opus-5-5".to_owned());
    a.add_reading(&reading, now);
    let week = crate::harness::accounts::WindowFig {
        pct_x10: 300,
        reset: now + 86_400,
        at: now - 5 * 86_400,
        by: "here".to_owned(),
        source: "tap".to_owned(),
        epoch_at: None,
    };
    a.add_held("seven_day", &week, now);
    let mut view = UsageView::new(now);
    view.accounts.push(a);
    assert_eq!(
        hud_line(&view),
        "opus 42%/5h \u{b7} 30%/7d \u{b7} acct work \u{b7} resets in 1h \u{b7} 5h tap 5s \u{b7} \
         7d store 5d"
    );
    let mut one = AccountView::new("work", true);
    one.model = Some("claude-opus-5-5".to_owned());
    one.add_reading(
        &crate::harness::tap::Reading {
            seven_day: week_only_reading().seven_day,
            ..reading
        },
        now,
    );
    let mut view = UsageView::new(now);
    view.accounts.push(one);
    assert!(
        hud_line(&view).ends_with(" \u{b7} resets in 1h \u{b7} tap 5s"),
        "{}",
        hud_line(&view)
    );
}

/// WHAT aterm HOLDS OF THE ACCOUNT FILLS ONLY AN EMPTY WINDOW ([`AccountView::add_held`]): a
/// current store figure fills the five-hour window the reading lacked, named `store` and aged
/// from its own observation; the session's own week is never replaced; a void figure or one a
/// limit reset cut fills nothing; a cached `/usage` figure is named as such.
#[test]
fn a_held_figure_fills_only_an_empty_window() {
    let reading = week_only_reading();
    let now = reading.at + 5;
    let fig = |pct_x10: u32, reset: i64, source: &str| crate::harness::accounts::WindowFig {
        pct_x10,
        reset,
        at: now - 60,
        by: "here".to_owned(),
        source: source.to_owned(),
        epoch_at: None,
    };
    let mut a = AccountView::new("work", true);
    a.add_reading(&reading, now);
    a.add_held("seven_day", &fig(500, reading.at + 3_600, "tap"), now);
    assert_eq!(
        a.windows["seven_day"].used_pct, 1,
        "the session's own week wins"
    );
    a.add_held("five_hour", &fig(420, now, "tap"), now);
    assert!(
        !a.windows.contains_key("five_hour"),
        "a void figure fills nothing"
    );
    let cut = crate::harness::accounts::WindowFig {
        epoch_at: Some(now - 30),
        ..fig(420, now + 3_600, "tap")
    };
    a.add_held("five_hour", &cut, now);
    assert!(
        !a.windows.contains_key("five_hour"),
        "a cut figure fills nothing"
    );
    a.add_held("five_hour", &fig(425, now + 3_600, "sample"), now);
    let five = a.windows["five_hour"];
    assert_eq!(
        (five.used_pct, five.resets_at, five.age_s, five.source),
        (43, now + 3_600, 60, SOURCE_STORE)
    );
    a.note_gaps(Some(&reading), Consulted::default(), |_| true);
    assert!(a.gaps.is_empty(), "no window is empty");
    let mut cached = AccountView::new("work", true);
    cached.add_held(
        "five_hour",
        &fig(
            70,
            now + 3_600,
            crate::harness::account_stats::SOURCE_USAGE_CACHE,
        ),
        now,
    );
    assert_eq!(cached.windows["five_hour"].source, "usage-cache");
}

/// THE USAGE LINE SAYS 100% ONLY FOR A READING OF 100
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2.1, as the glass says it): a tap
/// reading of 99.96 and a held figure of 999 tenths are each `99%` on the line, half up
/// below that (62.96 is `63%`), and a reading of 100 is `100%`. NEGATIVE CONTROL: rounding
/// the tap's whole field and `WindowFig::whole` plainly half up from the tenths
/// (`(x10 + 5) / 10`) prints `100%/5h` and `100%/7d`; trusting the whole field persisted by
/// an earlier format-v1 tap does the same even though its `pct_x10` is 999 (run once, restored).
#[test]
fn the_usage_line_says_100_only_for_a_reading_of_100() {
    let vars = crate::harness::tap::TapVars {
        sid: Some("s-60776b6e2ed1b73bd013".to_owned()),
        pid: Some("68537".to_owned()),
        login: None,
        home: None,
    };
    let now = 1_793_473_703;
    let line_of = |reading: &crate::harness::tap::Reading, week_x10: u32| {
        let mut a = AccountView::new("work", true);
        a.model = Some("claude-opus-5-5".to_owned());
        a.add_reading(reading, now);
        let week = crate::harness::accounts::WindowFig {
            pct_x10: week_x10,
            reset: now + 86_400,
            at: now - 60,
            by: "here".to_owned(),
            source: "tap".to_owned(),
            epoch_at: None,
        };
        a.add_held("seven_day", &week, now);
        let mut view = UsageView::new(now);
        view.accounts.push(a);
        let line = hud_line(&view);
        let figures = line.split(" \u{b7} acct").next().unwrap_or_default();
        figures.to_owned()
    };
    let reading = |five: &str| {
        let text = format!(
            r#"{{"model":{{"id":"claude-opus-5-5"}},"rate_limits":{{"five_hour":{{"used_percentage":{five},"resets_at":{}}}}}}}"#,
            now + 3_600
        );
        crate::harness::tap::Reading::of_stdin(&text, &vars, now).expect("a reading")
    };
    let line = |five: &str, week_x10: u32| line_of(&reading(five), week_x10);
    assert_eq!(line("99.96", 999), "opus 99%/5h \u{b7} 99%/7d");
    assert_eq!(line("99.5", 995), "opus 99%/5h \u{b7} 99%/7d");
    assert_eq!(line("62.96", 629), "opus 63%/5h \u{b7} 63%/7d");
    assert_eq!(
        line("100", 1000),
        "opus 100%/5h \u{b7} 100%/7d",
        "CONTROL: a reading of 100 is 100%"
    );
    let mut old_v1 = reading("99.96");
    old_v1.five_hour.as_mut().expect("the window").pct = 100;
    let old_v1 = crate::harness::tap::Reading::of_json(&old_v1.to_json())
        .expect("the persisted format-v1 reading");
    assert_eq!(
        line_of(&old_v1, 999),
        "opus 99%/5h \u{b7} 99%/7d",
        "the exact tenths override an older writer's rounded whole"
    );
}

#[test]
fn hud_line_is_capped_at_1024_bytes_on_a_char_boundary() {
    let mut a = AccountView::new("é".repeat(700), true);
    a.model = Some("claude-fable-5-1".to_owned());
    let line = hud_line(&view_with(a));
    assert!(line.len() <= HUD_MAX_BYTES, "{}", line.len());
    assert!(
        line.len() > HUD_MAX_BYTES - 4,
        "cut near the cap, not far below"
    );
    assert!(line.is_char_boundary(line.len()));
    assert!(line.chars().all(|c| c == 'é' || c == '·' || c.is_ascii()));
    for label in ["日".repeat(400), "🙂".repeat(300)] {
        let mut a = AccountView::new(label, true);
        a.model = Some("m".to_owned());
        let line = hud_line(&view_with(a));
        assert!(line.len() <= HUD_MAX_BYTES);
        assert!(line.len() > HUD_MAX_BYTES - 5);
        assert!(std::str::from_utf8(line.as_bytes()).is_ok());
    }
    assert_eq!(truncate_to("abc".to_owned(), 3), "abc");
    assert_eq!(truncate_to("aé".to_owned(), 2), "a");
}

#[test]
fn hud_line_flattens_control_characters_in_labels_and_model_ids() {
    let mut a = AccountView::new("wo\nrk\x1b[31m", true);
    a.model = Some("claude-fa\tble-5-1".to_owned());
    let line = hud_line(&view_with(a));
    assert!(!line.contains('\n') && !line.contains('\x1b') && !line.contains('\t'));
    assert!(line.starts_with("fa ble ?%/5h"));

    let split = AccountView::new("wo\u{2028}rk", true);
    let line = hud_line(&view_with(split));
    assert!(!line.contains('\u{2028}'), "{line:?}");
    assert!(line.contains("wo rk"), "{line:?}");
}

#[test]
fn model_short_names_the_family() {
    assert_eq!(model_short("claude-fable-5-1"), "fable");
    assert_eq!(model_short("claude-opus-4-1"), "opus");
    assert_eq!(model_short("claude-sonnet-4-5-20250929"), "sonnet");
    assert_eq!(model_short("claude-3-5-haiku-20241022"), "haiku");
    assert_eq!(model_short("gpt-5"), "gpt");
    assert_eq!(
        model_short("claude-3-5"),
        "3-5",
        "no family word: the id itself"
    );
    assert_eq!(model_short(""), "");
}

#[test]
fn usage_json_is_schema_1_and_round_trips_through_the_parser() {
    let mut work = live_account();
    let mut t = TranscriptUsage::new();
    fold_all(
        &mut t,
        &[
            &assistant_row("msg_a", "claude-fable-5-1", 1_000_000, 0),
            &assistant_row("msg_b", "claude-opus-5", 10, 0),
        ],
    );
    work.add_transcript(&t);
    let alt = AccountView::new("alt-1", false);
    let mut v = UsageView::new(1_789_650_000);
    v.accounts.push(work);
    v.accounts.push(alt);

    let doc = usage_json(&v);
    let j: Value = aterm_json::from_str(&doc).expect("the document is JSON");
    assert_eq!(j["schema"].as_u64(), Some(1));
    assert_eq!(j["as_of"].as_str(), Some("2026-09-17T13:00:00Z"));
    let accounts = j["accounts"].as_array().expect("accounts");
    assert_eq!(accounts.len(), 2);
    let w = &accounts[0];
    assert_eq!(w["label"].as_str(), Some("work"));
    assert_eq!(w["active"].as_bool(), Some(true));
    assert_eq!(w["model"].as_str(), Some("claude-opus-5"));
    assert!(w["windows"].as_object().is_some_and(Map::is_empty));
    let fable = &w["spend"]["claude-fable-5-1"];
    assert_eq!(fable["in"].as_u64(), Some(1_000_000));
    assert_eq!(fable["out"].as_u64(), Some(0));
    assert_eq!(fable["cache_write"].as_u64(), Some(10));
    assert_eq!(fable["cache_read"].as_u64(), Some(1000));
    assert!(fable["usd"].is_null());
    let a = &accounts[1];
    assert_eq!(a["active"].as_bool(), Some(false));
    assert!(a.get("model").is_none());
    assert!(a["windows"].as_object().is_some_and(Map::is_empty));
    assert!(a["spend"].as_object().is_some_and(Map::is_empty));
    assert_eq!(j["sheet"]["enabled"].as_bool(), Some(false));
    assert!(j["sheet"]["last_sync"].is_null());
    assert!(j["sheet"]["last_error"].is_null());
    assert!(!doc.contains('\n'), "one line");
}

#[test]
fn rfc3339_utc_matches_known_instants() {
    assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339_utc(-1), "1969-12-31T23:59:59Z");
    assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(rfc3339_utc(1_789_669_500), "2026-09-17T18:25:00Z");
    assert_eq!(rfc3339_utc(4_102_444_799), "2099-12-31T23:59:59Z");
    assert_eq!(rfc3339_utc(-62_162_035_200), "0000-03-01T00:00:00Z");
    let _ = rfc3339_utc(i64::MAX);
    let _ = rfc3339_utc(i64::MIN);
}

#[test]
fn kimi_usage_keeps_its_optional_week_and_month_apart() {
    let screen = rows(
        "   ╭ Usage ─────────────────────────────────────────╮\n\
         \x20  │ Plan usage                                      │\n\
         \x20  │   5h limit    ███████░░░  36% used  resets in 25m │\n\
         \x20  │   Monthly limit  ███░░░░  13% used  resets in 26d 20h 27m │\n\
         \x20  ╰─────────────────────────────────────────────────╯",
    );
    assert_eq!(
        kimi_usage_windows(&screen),
        vec![
            PanelWindow {
                name: "five_hour".to_owned(),
                used_pct: 36,
                reset_text: Some("in 25m".to_owned()),
            },
            PanelWindow {
                name: "monthly".to_owned(),
                used_pct: 13,
                reset_text: Some("in 26d 20h 27m".to_owned()),
            },
        ]
    );
    let legacy = screen
        .iter()
        .map(|row| row.replace("Monthly limit", "Weekly limit "))
        .collect::<Vec<_>>();
    assert_eq!(kimi_usage_windows(&legacy)[1].name, "seven_day");
    let newer = screen
        .iter()
        .map(|row| row.replace("36% used", "41% used"))
        .collect::<Vec<_>>();
    let repeated = [screen.clone(), newer].concat();
    assert_eq!(kimi_usage_windows(&repeated)[0].used_pct, 41);
    assert!(kimi_usage_windows(&screen[..screen.len() - 1]).is_empty());
    assert!(kimi_usage_windows(&rows("5h limit 36% used resets in 25m")).is_empty());
}

#[test]
fn panel_reader_takes_both_measured_layouts_and_the_fable_window() {
    for (what, text) in [("wide", PANEL_WIDE), ("narrow", PANEL_NARROW)] {
        let got = usage_panel_windows(&rows(text));
        let want = vec![
            PanelWindow {
                name: String::from("five_hour"),
                used_pct: 7,
                reset_text: Some(String::from("1:20pm (America/Los_Angeles)")),
            },
            PanelWindow {
                name: String::from("seven_day"),
                used_pct: 62,
                reset_text: Some(String::from("Sep 23 at 12pm (America/Los_Angeles)")),
            },
            PanelWindow {
                name: String::from("seven_day_overage_included"),
                used_pct: 100,
                reset_text: Some(String::from("Sep 23 at 11:59am (America/Los_Angeles)")),
            },
        ];
        assert_eq!(got, want, "{what}");
    }
}

#[test]
fn panel_reader_takes_the_copied_text_form() {
    let got = usage_panel_windows(&rows(
        "Current session: 7% used · resets 1:20pm (America/Los_Angeles)\n\
         Current week (all models): 62% used · resets Sep 23 at 12pm\n",
    ));
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].name, "five_hour");
    assert_eq!(got[0].used_pct, 7);
    assert_eq!(got[1].used_pct, 62);
    assert_eq!(got[1].reset_text.as_deref(), Some("Sep 23 at 12pm"));
}

#[test]
fn panel_reader_refuses_what_is_not_a_panel() {
    assert!(
        usage_panel_windows(&rows(
            "⏺ my five-hour window says Current session: 99% used\n"
        ))
        .is_empty()
    );
    assert!(
        usage_panel_windows(&rows(
            "   Current session\n   Resets 1:20pm\n\n   Current week (all models)\n   Resets Sep 23\n"
        ))
        .is_empty()
    );
    assert!(
        usage_panel_windows(&rows(
            "                                              7% until auto-compact\n\
             ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents\n"
        ))
        .is_empty()
    );
    assert!(
        usage_panel_windows(&rows(
            "   100% of your usage came from subagent-heavy sessions\n\
                81% of your usage was at >150k context\n"
        ))
        .is_empty()
    );
    assert!(usage_panel_windows(&[]).is_empty());
}

use aterm_phase::prompt::fixtures as fx;

const SCREEN_2_1_278: &str = include_str!("../../tests/testdata/vendor/2.1.278/screen/usage.txt");

fn hint() -> String {
    format!("   {}", aterm_phase::anchor!("prompt.cancel"))
}

fn retry_hints() -> String {
    format!("   r to retry · {}", aterm_phase::anchor!("prompt.cancel"))
}

fn settled() -> Vec<String> {
    fx::screen(fx::USAGE_SETTLED)
}

fn last_row(rows: &[String]) -> usize {
    rows.iter()
        .rposition(|r| !r.trim().is_empty())
        .expect("a drawn row")
}

fn composed(between: &[&str], tail: &str) -> Vec<String> {
    let mut out = settled();
    let at = last_row(&out);
    assert!(aterm_phase::panel_tail(&out[at]), "{:?}", out[at]);
    out[at] = tail.to_string();
    for row in between.iter().rev() {
        out.insert(at, String::new());
        out.insert(at, (*row).to_string());
    }
    out
}

fn header_of(rows: &[String]) -> usize {
    aterm_phase::settings_panel(rows).expect("the panel").header
}

fn read(rows: &[String]) -> PanelRead {
    usage_panel_read(rows, header_of(rows))
}

fn figures(read: &PanelRead) -> Vec<(String, u32)> {
    let PanelRead::Settled(windows) = read else {
        panic!("not settled: {read:?}")
    };
    windows
        .iter()
        .map(|w| (w.name.clone(), w.used_pct))
        .collect()
}

fn three(session: u32, week: u32, fable: u32) -> Vec<(String, u32)> {
    vec![
        ("five_hour".to_string(), session),
        ("seven_day".to_string(), week),
        ("seven_day_overage_included".to_string(), fable),
    ]
}

#[test]
fn the_measured_screens_with_the_tail_under_the_fold_are_no_sample() {
    let screen = rows(SCREEN_2_1_278);
    assert_eq!(screen.len(), 46);
    let fullscreen = Some(aterm_phase::SettingsPanel {
        rule: 2,
        header: 3,
        renderer: aterm_phase::PanelRenderer::Fullscreen,
    });
    for (what, screen, seen) in [
        ("2.1.278", screen.clone(), three(18, 64, 100)),
        (
            "2.1.287 150x50",
            fx::screen(fx::USAGE_FOLD),
            three(1, 10, 0),
        ),
        (
            "2.1.287 110x32",
            fx::screen(fx::USAGE_SHORT),
            three(2, 10, 0),
        ),
    ] {
        assert_eq!(aterm_phase::settings_panel(&screen), fullscreen, "{what}");
        assert!(
            screen[last_row(&screen)].ends_with('↓'),
            "{what}: {:?}",
            screen[last_row(&screen)]
        );
        assert_eq!(usage_panel_read(&screen, 3), PanelRead::Fold, "{what}");
        let on_the_grid: Vec<(String, u32)> = usage_panel_windows(&screen[4..])
            .into_iter()
            .map(|w| (w.name, w.used_pct))
            .collect();
        assert_eq!(on_the_grid, seen, "{what}");
    }

    assert_eq!(aterm_phase::settings_panel(&rows(PANEL_WIDE)), None);
    assert_eq!(aterm_phase::settings_panel(&rows(PANEL_NARROW)), None);
    let rule = "─".repeat(56);
    let mut framed = screen.clone();
    framed.extend([rule.clone(), "❯ ".to_string(), rule]);
    assert_eq!(aterm_phase::settings_panel(&framed), None);
    let mut shifted = screen.clone();
    shifted[3] = shifted[3][1..].to_string();
    assert_eq!(aterm_phase::settings_panel(&shifted), None);
    let mut shifted = screen.clone();
    shifted[3] = format!(" {}", shifted[3]);
    assert_eq!(aterm_phase::settings_panel(&shifted), None);
}

#[test]
fn the_header_guard_matches_the_measured_header_and_no_copy_of_it() {
    for (what, screen) in [
        ("2.1.278", rows(SCREEN_2_1_278)),
        ("refreshing", fx::screen(fx::USAGE_REFRESHING)),
        ("settled", fx::screen(fx::USAGE_SETTLED)),
        ("fold", fx::screen(fx::USAGE_FOLD)),
        ("whole", fx::screen(fx::USAGE_WHOLE)),
        ("narrow", fx::screen(fx::USAGE_NARROW)),
        ("short", fx::screen(fx::USAGE_SHORT)),
        ("inline", fx::screen(fx::USAGE_INLINE)),
    ] {
        let panel = aterm_phase::settings_panel(&screen).expect("the panel");
        let guard = panel.header_guard(&screen);
        let matcher = aterm_observe::row_matcher(&guard).expect("the guard compiles");
        let hits: Vec<usize> = (0..screen.len())
            .filter(|&i| matcher.matches(&screen[i]))
            .collect();
        assert_eq!(hits, [panel.header], "{what}: {guard}");
    }
    let screen = fx::screen(fx::USAGE_SETTLED);
    let guard = aterm_phase::settings_panel(&screen)
        .expect("the panel")
        .header_guard(&screen);
    let matcher = aterm_observe::row_matcher(&guard).expect("the guard compiles");
    let help = format!("aterm ctl key 'if={guard}' esc");
    for copy in [
        "  ⎿     Settings  Status   Config   Usage   Stats",
        "    Settings  Status   Config   Usage   Stats",
        "⏺ Settings  Status   Config   Usage   Stats",
        "Settings  Status   Config   Usage   Stats",
        help.as_str(),
    ] {
        assert!(!matcher.matches(copy), "{copy}");
    }
}

#[test]
fn a_whole_panel_with_nothing_unsettled_on_it_is_settled() {
    assert_eq!(figures(&read(&settled())), three(1, 10, 0));
    assert_eq!(
        figures(&read(&fx::screen(fx::USAGE_WHOLE))),
        three(2, 10, 0)
    );
    assert_eq!(
        figures(&read(&fx::screen(fx::USAGE_NARROW))),
        three(2, 10, 0)
    );
    let PanelRead::Settled(windows) = read(&settled()) else {
        panic!("not settled")
    };
    assert_eq!(
        (
            windows[0].reset_text.as_deref(),
            windows[1].reset_text.as_deref()
        ),
        (
            Some("2am (America/Los_Angeles)"),
            Some("Oct 8 at 9am (America/Los_Angeles)")
        )
    );

    let refreshing = fx::screen(fx::USAGE_REFRESHING);
    assert_eq!(read(&refreshing), PanelRead::Unsettled);
    let stale: Vec<(String, u32)> = usage_panel_windows(&refreshing[4..])
        .into_iter()
        .map(|w| (w.name, w.used_pct))
        .collect();
    assert_eq!(
        stale,
        [("five_hour".to_string(), 1), ("seven_day".to_string(), 9)]
    );
    assert_eq!(
        read(&composed(&["   Refreshing…"], &hint())),
        PanelRead::Unsettled
    );
    assert_eq!(read(&composed(&[], &hint())), read(&settled()));
}

#[test]
fn an_enrich_error_under_the_windows_is_unsettled() {
    let hints = retry_hints();
    for line in [
        "   Showing last-known usage as of 2m ago (rate limited — try again in a moment)",
        "   Showing last-known usage (could not refresh)",
        "   Could not refresh usage data",
        "   Partial usage data (rate limited — try again in a moment)",
        "   Per-model breakdown unavailable (rate limited — try again in a moment)",
    ] {
        assert_eq!(
            read(&composed(&[line], &hints)),
            PanelRead::Unsettled,
            "{line}"
        );
        assert_eq!(
            read(&composed(&[line], &hint())),
            PanelRead::Unsettled,
            "{line}"
        );
    }
    assert_eq!(read(&composed(&[], &hints)), PanelRead::Fold);
}

#[test]
fn a_loading_or_failed_panel_is_unsettled() {
    let measured = settled();
    let first_window = measured
        .iter()
        .position(|r| r.trim() == "Current session")
        .expect("the first window");
    let head = |rest: &[&str]| {
        let mut out = measured[..first_window].to_vec();
        for row in rest {
            out.push((*row).to_string());
            out.push(String::new());
        }
        out
    };
    let cancel = hint();
    let retry = retry_hints();
    let loading = head(&["   Loading usage data…", &cancel]);
    assert!(loading.iter().any(|r| r.trim().starts_with("Total cost:")));
    assert_eq!(read(&loading), PanelRead::Unsettled);
    for why in [
        "Failed to load usage data",
        "Failed to load usage data: upstream timed out",
        "Usage endpoint is rate limited. Please try again in a moment.",
    ] {
        let failed = head(&[&format!("   Error: {why}"), &retry]);
        assert_eq!(read(&failed), PanelRead::Unsettled, "{why}");
    }
    let mut bare = measured[..=header_of(&measured)].to_vec();
    bare.extend([
        String::new(),
        "   Loading usage data…".to_string(),
        String::new(),
        cancel.clone(),
    ]);
    assert_eq!(read(&bare), PanelRead::Unsettled);

    assert_eq!(read(&head(&[&cancel])), PanelRead::Unsettled);
    assert!(usage_panel_windows(&head(&[&cancel])).is_empty());
}

#[test]
fn a_panel_whose_tail_is_under_the_fold_is_no_sample() {
    let whole = settled();
    let tail = last_row(&whole);
    let mut arrow = whole.clone();
    arrow[tail] = format!("{:<149}↓", hint());
    assert_eq!(read(&arrow), PanelRead::Fold);
    let mark = whole
        .iter()
        .position(|r| r.trim().starts_with("Total cost:"))
        .expect("the session block");
    for cut in mark + 1..tail {
        assert_eq!(read(&whole[..cut]), PanelRead::Fold, "cut at {cut}");
    }
    assert!(matches!(read(&whole), PanelRead::Settled(_)));
    assert!(matches!(read(&whole[..=tail]), PanelRead::Settled(_)));
    let mut after = whole[..=tail].to_vec();
    after.push("   What's contributing to your limits usage?".to_string());
    assert_eq!(read(&after), PanelRead::Fold);
}

#[test]
fn a_whole_panel_with_the_session_or_the_week_is_settled_and_with_neither_is_not() {
    let whole = settled();
    let title = |name: &str| {
        whole
            .iter()
            .position(|r| r.trim() == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    let (session, week, fable, rest) = (
        title("Current session"),
        title("Current week (all models)"),
        title("Current week (Fable)"),
        title("What's contributing to your limits usage?"),
    );
    let without = |from: usize, to: usize| {
        let mut out = whole[..from].to_vec();
        out.extend_from_slice(&whole[to..]);
        out
    };
    let week_only = {
        let mut out = whole[..session].to_vec();
        out.extend_from_slice(&whole[week..fable]);
        out.extend_from_slice(&whole[rest..]);
        out
    };
    // Claude Code draws a window only for a figure the account has: each whole tab with one
    // of the two is a read, its other windows with it.
    let only_session = without(week, rest);
    assert!(only_session.iter().any(|r| r.contains("1% used")));
    assert_eq!(
        figures(&read(&only_session)),
        [("five_hour".to_string(), 1)]
    );
    assert_eq!(
        figures(&read(&without(week, fable))),
        [
            ("five_hour".to_string(), 1),
            ("seven_day_overage_included".to_string(), 0)
        ]
    );
    assert_eq!(
        figures(&read(&without(session, week))),
        [
            ("seven_day".to_string(), 10),
            ("seven_day_overage_included".to_string(), 0)
        ]
    );
    assert_eq!(figures(&read(&week_only)), [("seven_day".to_string(), 10)]);
    assert_eq!(
        figures(&read(&without(fable, rest))),
        [("five_hour".to_string(), 1), ("seven_day".to_string(), 10)]
    );
    // A whole tab with neither is no read, whatever other window it shows; and one window off
    // any panel is still no window set.
    let fable_only = without(session, fable);
    assert!(
        fable_only
            .iter()
            .any(|r| r.trim() == "Current week (Fable)")
    );
    assert_eq!(read(&without(session, rest)), PanelRead::Unsettled);
    assert_eq!(read(&fable_only), PanelRead::Unsettled);
    assert!(usage_panel_windows(&fable_only).is_empty());
}

/// 2.1.318'S PANEL WITH NO SESSION WINDOW IS READ WHOLE (HP1). Measured in the lab on both
/// renderers (`claude-2.1.318-usage-week-only-*.txt`): an account whose answers carry no
/// five-hour figure gets the week's window alone, no `Current session` (and on the fullscreen
/// renderer the weekly warning in the panel's rule). As measured the tab also carries the lab's
/// enrich error (its usage body's read refused by the black hole), so it is not settled; the
/// same tab with its body read (that row gone, and the hint row without the retry the error
/// alone adds, §0 of docs/SPEC-claude-accounts-probe-2026-10-01.md) is settled on the week with
/// its reset, and samples the week with no session. Negative control: the reader before HP1 took the panel's
/// windows through the two-window guard ([`usage_panel_windows`]) and settled only on the
/// session's and the week's together; it reads nothing off this tab.
#[test]
fn the_2_1_318_panel_with_no_session_window_is_read_whole() {
    const READ_AT: i64 = 1_793_980_920; // 2026-11-06T16:02:00Z, 08:02 in Los Angeles
    const RESET: i64 = 1_794_255_720; // 2026-11-09T20:22:00Z, Nov 9 at 12:22pm there
    const PST: i64 = -8 * 3600;
    let zones = |zone: &str, _: i64| (zone == "America/Los_Angeles").then_some(PST);
    let refused = aterm_phase::anchor!("usage.could_not_refresh");
    for (what, text, reset_text, reset) in [
        (
            "fullscreen",
            fx::USAGE_WEEK_ONLY_FULLSCREEN_2_1_318,
            "Nov 9 at 12:22pm (America/Los_Angeles)",
            RESET,
        ),
        (
            "inline",
            fx::USAGE_WEEK_ONLY_INLINE_2_1_318,
            "Nov 9 at 12:21pm (America/Los_Angeles)",
            RESET - 60,
        ),
    ] {
        let measured = fx::screen(text);
        assert_eq!(
            read(&measured),
            PanelRead::Unsettled,
            "{what}: the lab's enrich error"
        );
        let retry = format!("r to retry · {}", aterm_phase::anchor!("prompt.cancel"));
        let whole: Vec<String> = measured
            .iter()
            .filter(|r| r.trim() != refused)
            .map(|r| r.replacen(&retry, aterm_phase::anchor!("prompt.cancel"), 1))
            .collect();
        assert_eq!(whole.len() + 1, measured.len(), "{what}: one row cut");
        assert!(
            !measured.iter().any(|r| aterm_phase::panel_tail(r))
                && whole.iter().any(|r| aterm_phase::panel_tail(r)),
            "{what}: the hint without its retry"
        );
        let got = read(&whole);
        assert_eq!(figures(&got), [("seven_day".to_string(), 98)], "{what}");
        let PanelRead::Settled(windows) = got else {
            unreachable!()
        };
        assert_eq!(windows[0].reset_text.as_deref(), Some(reset_text), "{what}");
        let sample = panel_sample(&windows, READ_AT, PST, &zones).expect("a sample");
        assert_eq!(
            (sample.session_pct, sample.week_pct, sample.week_reset),
            (None, Some(98), Some(reset)),
            "{what}"
        );

        // CONTROL: the reader before HP1 reads no window off it, so no read at all.
        let under = &whole[header_of(&whole) + 1..];
        assert!(usage_panel_windows(under).is_empty(), "{what}");
        let has = |name: &str| windows.iter().any(|w| w.name == name);
        assert!(!(has(SESSION_WINDOW) && has(WEEK_WINDOW)), "{what}");
    }
    let fullscreen = fx::screen(fx::USAGE_WEEK_ONLY_FULLSCREEN_2_1_318);
    let rule = aterm_phase::settings_panel(&fullscreen)
        .expect("the panel")
        .rule;
    assert!(
        fullscreen[rule].contains("You've used 98% of your weekly limit"),
        "{:?}",
        fullscreen[rule]
    );
}

#[test]
fn a_settings_panel_on_another_tab_is_not_the_usage_tab() {
    let measured = settled();
    let other = |body: &[&str]| {
        let mut out = measured[..=header_of(&measured)].to_vec();
        out.push(String::new());
        out.extend(body.iter().map(|r| (*r).to_string()));
        out.extend([String::new(), hint()]);
        out
    };
    for body in [
        &[
            "   Version: 0.0.0",
            "   Session name: (none)",
            "   cwd: /work",
        ][..],
        &[
            "   Auto-compact                          true",
            "   Theme                                 dark",
        ][..],
        &[][..],
    ] {
        let panel = other(body);
        assert!(aterm_phase::settings_panel(&panel).is_some());
        assert_eq!(read(&panel), PanelRead::NotUsage, "{body:?}");
    }
    assert_eq!(
        read(&other(&["   Total cost:            $0.0000"])),
        PanelRead::Unsettled
    );
    assert_eq!(usage_panel_read(&settled(), 10_000), PanelRead::NotUsage);
}

#[test]
fn the_inline_renderers_panel_is_told_from_the_fullscreen_one() {
    let inline = fx::screen(fx::USAGE_INLINE);
    let panel = aterm_phase::settings_panel(&inline).expect("the inline panel");
    assert_eq!(panel.renderer, aterm_phase::PanelRenderer::Inline);
    assert_eq!(
        figures(&usage_panel_read(&inline, panel.header)),
        three(2, 10, 0)
    );
    assert_eq!(
        aterm_phase::settings_panel(&fx::screen(fx::USAGE_INLINE_SCROLLED)),
        None
    );
    assert_eq!(
        aterm_phase::settings_panel(&settled()).map(|p| p.renderer),
        Some(aterm_phase::PanelRenderer::Fullscreen)
    );
}

const READ_AT: i64 = 1_790_914_800;

fn zones(zone: &str, _at: i64) -> Option<i64> {
    (zone == "America/Los_Angeles").then_some(-7 * 3600)
}

fn window(name: &str, pct: u32, reset: Option<&str>) -> PanelWindow {
    PanelWindow {
        name: name.to_string(),
        used_pct: pct,
        reset_text: reset.map(str::to_string),
    }
}

#[test]
fn a_settled_panels_sample_places_each_reset_in_the_panels_zone() {
    let PanelRead::Settled(windows) = read(&settled()) else {
        panic!("not settled")
    };
    let east = 9 * 3600;
    let sample = panel_sample(&windows, READ_AT, east, &zones).expect("a sample");
    assert_eq!(
        sample,
        crate::harness::accounts::Sample {
            session_pct: Some(1),
            session_reset: Some(1_790_931_600),
            week_pct: Some(10),
            week_reset: Some(1_791_475_200),
            session_read_at: Some(READ_AT),
            week_read_at: Some(READ_AT),
            at: READ_AT,
            by: String::new(),
        }
    );
    assert_eq!(
        (
            rfc3339_utc(1_790_931_600).as_str(),
            rfc3339_utc(1_791_475_200).as_str()
        ),
        ("2026-10-02T09:00:00Z", "2026-10-08T16:00:00Z")
    );
    assert_eq!(sample.session(READ_AT), Some((1, 1_790_931_600)));
    assert_eq!(sample.week(READ_AT), Some((10, 1_791_475_200)));

    let nowhere = panel_sample(&windows, READ_AT, east, &|_, _| None).expect("a sample");
    assert_eq!((nowhere.session_pct, nowhere.week_pct), (Some(1), Some(10)));
    assert_ne!(nowhere.session_reset, sample.session_reset);
    assert_ne!(nowhere.week_reset, sample.week_reset);
}

#[test]
fn a_reset_beyond_its_windows_bound_is_dropped_and_its_figure_kept() {
    let sample = |session: Option<&str>, week: Option<&str>| {
        panel_sample(
            &[
                window("five_hour", 40, session),
                window("seven_day", 50, week),
            ],
            READ_AT,
            0,
            &zones,
        )
        .expect("a sample")
    };
    let ahead = |at: Option<i64>| at.map(|at| at - READ_AT);
    let zone = "(America/Los_Angeles)";
    let kept = sample(Some(&format!("1:20am {zone}")), None);
    assert_eq!(ahead(kept.session_reset), Some(4 * 3600));
    let dropped = sample(Some(&format!("Oct 2 at 9:20pm {zone}")), None);
    assert_eq!(
        (dropped.session_pct, dropped.session_reset),
        (Some(40), None)
    );
    let kept = sample(None, Some(&format!("Oct 8 at 9:20pm {zone}")));
    assert_eq!(ahead(kept.week_reset), Some(7 * 24 * 3600));
    let dropped = sample(None, Some(&format!("Oct 10 at 9:20pm {zone}")));
    assert_eq!((dropped.week_pct, dropped.week_reset), (Some(50), None));
    let both = sample(
        Some(&format!("Oct 2 at 9:20pm {zone}")),
        Some(&format!("Oct 2 at 9:20pm {zone}")),
    );
    assert_eq!(
        (both.session_reset, ahead(both.week_reset)),
        (None, Some(24 * 3600))
    );
    let unread = sample(Some("when the moon is full"), None);
    assert_eq!(
        (unread.session_pct, unread.session_reset, unread.week_reset),
        (Some(40), None, None)
    );
}

#[test]
fn a_window_that_was_not_read_gives_no_figure_and_none_gives_no_sample() {
    let week = window("seven_day", 10, Some("Oct 8 at 9am (America/Los_Angeles)"));
    let session = window("five_hour", 1, Some("2am (America/Los_Angeles)"));
    let fable = window(
        "seven_day_overage_included",
        100,
        Some("Oct 8 at 9am (America/Los_Angeles)"),
    );
    let of = |windows: &[PanelWindow]| panel_sample(windows, READ_AT, 0, &zones);

    let lone = of(std::slice::from_ref(&week)).expect("the week alone");
    assert_eq!(
        (lone.session_pct, lone.session_reset, lone.week_pct),
        (None, None, Some(10))
    );
    assert_eq!(lone.session(READ_AT), None);
    let lone = of(std::slice::from_ref(&session)).expect("the session alone");
    assert_eq!(
        (lone.session_pct, lone.week_pct, lone.week_reset),
        (Some(1), None, None)
    );
    assert_eq!(of(&[]), None);
    assert_eq!(of(std::slice::from_ref(&fable)), None);
    assert_eq!(of(&[fable.clone(), window("spend_limit", 12, None)]), None);
    let both = of(&[fable, week, session]).expect("both");
    assert_eq!((both.session_pct, both.week_pct), (Some(1), Some(10)));
}

#[test]
fn panel_reader_never_guesses_a_measured_key_for_an_unmeasured_title() {
    let got = usage_panel_windows(&rows(
        "   Current session\n   ███ 5% used\n   Resets 1pm\n\n\
         \x20  Current week (Haiku 9 Super)\n   ███ 11% used\n   Resets Sep 23\n",
    ));
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].name, "seven_day_haiku_9_super");
    for (_, key) in PANEL_TITLES {
        assert_ne!(got[1].name, key);
    }
    let junk = usage_panel_windows(&rows(
        "   Current session\n   ███ 5% used\n\n\
         \x20  Current week (···)\n   ███ 11% used\n\n\
         \x20  Current week (all models)\n   ███ 22% used\n",
    ));
    assert_eq!(
        junk.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
        vec!["five_hour", "seven_day"],
        "a keyless title is dropped, not guessed"
    );
}

#[test]
fn panel_sample_local_places_zoneless_resets_across_a_daylight_change() {
    const PDT: i64 = -7 * 3_600;
    const PST: i64 = -8 * 3_600;
    const NOW: i64 = 1_793_520_000; // 2026-11-01 01:00 PDT (08:00Z), 1h before fall-back
    const CHANGE: i64 = 1_793_523_600; // 2026-11-01T09:00:00Z
    let windows = [
        window("five_hour", 25, Some("3am")),
        window("seven_day", 60, Some("Nov 3 at 11am")),
    ];
    let local_at = |unix: i64| Some(if unix < CHANGE { PDT } else { PST });
    let dst = panel_sample_local(&windows, NOW, &local_at, &|_, _| None).expect("sample");
    let flat = panel_sample(&windows, NOW, PDT, &|_, _| None).expect("sample");
    assert_eq!(
        (dst.session_reset, dst.week_reset),
        (Some(1_793_530_800), Some(1_793_732_400)),
        "3am PST (11:00Z, 3h ahead) and Nov 3 11am PST (19:00Z)"
    );
    assert_eq!(
        (flat.session_reset, flat.week_reset),
        (Some(1_793_527_200), Some(1_793_728_800)),
        "CONTROL: flat PDT places both an hour early"
    );
}
