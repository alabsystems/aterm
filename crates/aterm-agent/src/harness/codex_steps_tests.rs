// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`]: the Codex step fold over the lab's committed Codex
//! 0.163.0 rollouts (reduced: keys, order, closed words and numbers kept,
//! ids remapped, every other string replaced) and over rows composed in the
//! shapes those rollouts and `super::super::codex_rollout`'s measured forks
//! show (values made up).

use super::*;
use crate::harness::codex_rollout::PAGINATED;
use crate::harness::codex_rollout::fixtures::{count, message, meta, meta_as, settings, stamp};
use crate::harness::usage::MAX_LINE_BYTES;
use aterm_phase::anchors::rollout_reduction_stem;

const FP: u64 = 0xc0de_0000_0000_0001;
const ID: Option<(u64, u64)> = Some((16_777_220, 78));

/// The lab's main rollout and its subagent's, reduced (the 5G lab run).
const MAIN: &str = include_str!("testdata/steps/codex-0.163.0.jsonl");
const SUBAGENT: &str = include_str!("testdata/steps/codex-0.163.0-subagent.jsonl");

/// Where the reductions are committed.
const STEPS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/harness/testdata/steps");

/// Thread ids in UUIDv7 form; a stand-in instant, unix ms.
const PARENT: &str = "01900000-0000-7000-8000-000000000001";
const FORK: &str = "01900000-0000-7000-8000-000000000002";
const OTHER: &str = "01900000-0000-7000-8000-000000000003";
const T0: i64 = 1_790_000_000_000;

/// A rollout's rows: the reducer's own header line is none.
fn rows(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| !l.starts_with('#'))
}

/// `text`'s rows pushed through a fold bound at `seed` with floor `floor`,
/// each ending past its newline, and what the fold queued.
fn fold_with(text: &str, seed: u64, floor: u64) -> (CodexStepFold, StepBatch) {
    let mut fold = CodexStepFold::new(floor);
    fold.bind(FP, ID, seed);
    let mut end = 0u64;
    for row in rows(text) {
        end += row.len() as u64 + 1;
        fold.push(row.as_bytes(), end);
    }
    let batch = fold.take();
    (fold, batch)
}

fn fold(text: &str) -> StepBatch {
    fold_with(text, 0, 0).1
}

/// A step as the tests read it: its kind and its fields, no position.
fn said(ev: &StepEvent) -> String {
    match ev {
        StepEvent::Bind { len } => format!("bind {len}"),
        StepEvent::Prompt { vref } => format!("prompt {}", vref.as_str()),
        StepEvent::Interrupt => "interrupt".into(),
        StepEvent::ToolUse {
            id,
            tool,
            cls,
            subagent,
        } => format!(
            "call {} {} {cls}{}",
            id.as_str(),
            tool.as_str(),
            if *subagent { " subagent" } else { "" }
        ),
        StepEvent::ToolResult { id, error } => {
            format!(
                "result {}{}",
                id.as_str(),
                if *error { " error" } else { "" }
            )
        }
        StepEvent::TurnEnd {
            vdur_ms,
            tok_in,
            tok_out,
        } => format!("end {vdur_ms} {tok_in} {tok_out}"),
        StepEvent::EndWall { kind } => format!("wall {kind:?}"),
        StepEvent::Compact => "compact".into(),
        StepEvent::Model { model, effort, ver } => format!(
            "model {} {} {}",
            model.as_str(),
            effort.as_ref().map_or("-", StepId::as_str),
            ver.as_ref().map_or("-", StepId::as_str)
        ),
        StepEvent::Off { why } => format!("off {why}"),
    }
}

fn saids(batch: &StepBatch) -> Vec<String> {
    batch.steps.iter().map(|s| said(&s.ev)).collect()
}

/// The reductions' turn ids: `00000000-0000-4000-8000-0000000000<n>`.
fn turn(n: &str) -> String {
    format!("prompt 00000000-0000-4000-8000-0000000000{n}")
}

/// A `turn_context` of turn `id` on `model`, as 0.163.0 writes one (its
/// policies and profiles stand-ins).
fn ctx(ms: i64, id: &str, model: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{id}\",\
         \"root_turn_id\":\"{id}\",\"cwd\":\"/stand-in/project\",\"approval_policy\":\
         \"on-request\",\"sandbox_policy\":{{\"type\":\"workspace-write\"}},\"model\":\
         \"{model}\",\"summary\":\"auto\"}}}}\n",
        stamp(ms)
    )
}

/// A `task_complete` of turn `id`, `extra` keys after its duration.
fn done(ms: i64, id: &str, dur: u64, extra: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\
         \"turn_id\":\"{id}\",\"last_agent_message\":\"SENTINEL-REPLY\",\"duration_ms\":{dur}\
         {extra}}}}}\n",
        stamp(ms)
    )
}

/// A `turn_aborted` of turn `id`.
fn aborted(ms: i64, id: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"turn_aborted\",\
         \"turn_id\":\"{id}\",\"reason\":\"interrupted\",\"duration_ms\":40}}}}\n",
        stamp(ms)
    )
}

/// A `function_call` of `name` in `namespace`, and its output.
fn call(ms: i64, id: &str, namespace: &str, name: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\
         \"function_call\",\"name\":\"{name}\",\"namespace\":\"{namespace}\",\"arguments\":\
         \"{{\\\"command\\\":\\\"SENTINEL-INPUT\\\"}}\",\"call_id\":\"{id}\"}}}}\n",
        stamp(ms)
    )
}

fn output(ms: i64, id: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\
         \"function_call_output\",\"call_id\":\"{id}\",\"output\":\"SENTINEL-OUTPUT\"}}}}\n",
        stamp(ms)
    )
}

/// A new paginated thread's first row.
fn opening() -> String {
    meta_as(T0, PARENT, PARENT, None, Some(PAGINATED), None)
}

/// THE LAB'S ROLLOUT, FOLDED (5G-2; T9). Each leg the lab ran on Codex
/// 0.163.0 is its turn: a plain turn, a tool call (`exec`), an approval
/// (`exec` again, its command escalated), a question
/// (`request_user_input`), Esc (`turn_aborted`), the usage wall
/// (`task_complete` with `usage_limit_exceeded`), a compaction (its count
/// and `compacted` before its `turn_context`, so its compaction is said
/// before its prompt and its tokens are its own) and a subagent
/// (`spawn_agent` in `collaboration`). Each turn opens once, at its
/// `turn_context`; its tokens are its growing counts' `last_token_usage`;
/// the model is said once. Every step is partial: no codex-alab build was
/// measured. Nothing is lost.
#[test]
fn the_labs_rollout_folds_to_its_legs() {
    let batch = fold(MAIN);
    let want = [
        "bind 0".to_string(),
        turn("03"),
        "model gpt-6.1-sol - 0.163.0".into(),
        "end 21 15048 12".into(),
        turn("0e"),
        "call call_r0019 exec other".into(),
        "result call_r0019".into(),
        "end 946 15390 22".into(),
        turn("18"),
        "call call_r0029 exec other".into(),
        "result call_r0029".into(),
        "end 8044 15645 22".into(),
        turn("23"),
        "call call_r0040 request_user_input other".into(),
        "result call_r0040".into(),
        "end 8945 15885 22".into(),
        turn("2d"),
        "interrupt".into(),
        turn("32"),
        "end 13 0 0".into(),
        "wall UsageSession".into(),
        "compact".into(),
        turn("36"),
        "end 17 9902 13".into(),
        turn("40"),
        "call call_r0069 spawn_agent agent subagent".into(),
        "result call_r0069".into(),
        "end 32 15611 23".into(),
    ];
    assert_eq!(saids(&batch), want);
    assert_eq!((batch.lost, batch.overflow), (0, 0));
    assert!(batch.steps.iter().all(|s| s.state == StepState::Partial));
    assert_eq!(batch.end, End::default());
    // Each step has its own place, in the rollout's order, and its row's
    // time (the reduction's, moved to 2000).
    let offs: Vec<u64> = batch.steps.iter().map(|s| s.pos.off).collect();
    assert!(offs.windows(2).all(|w| w[0] < w[1]), "{offs:?}");
    let y2000 = u64::try_from(stamp_ms("2000-01-01T00:00:00Z").expect("a stamp")).expect("ms");
    assert!(batch.steps[1..].iter().all(|s| s.pos.at_ms > y2000));
    assert!(batch.steps.iter().all(|s| s.pos.fp == FP && s.pos.id == ID));
}

/// A SUBAGENT'S COPY IS NO ONE'S TURN (measured on the lab's subagent): its
/// rollout copies its parent's `session_meta`, a compaction and two of its
/// parent's turns in one burst, then writes its own
/// `thread_settings_applied`, and only its own turn is read, on its own
/// model and effort. NEGATIVE CONTROLS: read with no fork's `session_meta`
/// first, the copy opens and ends the parent's turns (the phantoms the copy
/// rule keeps out); with its settings line naming another thread, the burst
/// holds its own turn too (a loss, never a phantom).
#[test]
fn a_subagents_copy_steps_nothing() {
    let batch = fold(SUBAGENT);
    assert_eq!(
        saids(&batch),
        [
            "bind 0".to_string(),
            turn("1a"),
            "model gpt-6.1-sol low 0.163.0".into(),
            "end 13 15211 13".into(),
        ]
    );
    assert_eq!((batch.lost, batch.overflow), (0, 0));
    // NEGATIVE CONTROLS.
    let headless: String = rows(SUBAGENT).skip(1).map(|r| format!("{r}\n")).collect();
    let phantoms = saids(&fold(&headless));
    for parent in [turn("10"), turn("17")] {
        assert!(phantoms.contains(&parent), "{phantoms:?}");
    }
    let own = "\"thread_id\":\"00000000-0000-4000-8000-000000000002\"";
    let settled = |r: &str| r.contains("\"thread_settings_applied\"") && r.contains(own);
    assert_eq!(rows(SUBAGENT).filter(|r| settled(r)).count(), 1);
    let unsettled: String = rows(SUBAGENT)
        .map(|r| {
            if settled(r) {
                r.replace(
                    own,
                    "\"thread_id\":\"00000000-0000-4000-8000-000000000009\"",
                )
            } else {
                r.to_string()
            }
        })
        .map(|r| format!("{r}\n"))
        .collect();
    assert_eq!(saids(&fold(&unsettled)), ["bind 0"]);
}

/// A LEGACY FORK'S COPY (`super::super::codex_rollout`'s header, measured on
/// 0.163.0): its parent's rows, counts among them, under one stamp, a
/// `turn_aborted` of its parent's open turn, a copied settings line naming
/// the parent, then its own in the same millisecond, and the prompt's turn
/// at once. Only that turn is read, its tokens its own count's (the copy's
/// total stands). NEGATIVE CONTROLS: with no fork's `session_meta` first the
/// parent's turns open, end and abort; with its own settings line naming
/// another thread the burst holds the prompt's turn.
#[test]
fn a_legacy_forks_copy_steps_nothing() {
    let forked = |own: &str| {
        [
            meta_as(T0, FORK, FORK, Some(PARENT), Some("legacy"), None),
            meta(T0, PARENT, PARENT, None),
            ctx(T0, "turn-p1", "gpt-6-luna"),
            message(T0),
            count(T0, (6_000, 0, 30), (3_000, 0, 10)),
            done(T0, "turn-p1", 900, ""),
            ctx(T0, "turn-p2", "gpt-6-luna"),
            message(T0),
            aborted(T0, "turn-p2"),
            settings(T0, PARENT),
            settings(T0, own),
            ctx(T0 + 24, "turn-f1", "gpt-6.1-sol"),
            message(T0 + 33),
            count(T0 + 37, (19_992, 0, 30), (13_992, 0, 0)),
            done(T0 + 40, "turn-f1", 16, ""),
        ]
        .concat()
    };
    assert_eq!(
        saids(&fold(&forked(FORK))),
        [
            "bind 0",
            "prompt turn-f1",
            "model gpt-6.1-sol - 0.163.0",
            "end 16 13992 0"
        ]
    );
    // NEGATIVE CONTROLS.
    let headless: String = forked(FORK)
        .lines()
        .skip(1)
        .map(|r| format!("{r}\n"))
        .collect();
    let phantoms = saids(&fold(&headless));
    for parent in [
        "prompt turn-p1",
        "end 900 3000 10",
        "prompt turn-p2",
        "interrupt",
    ] {
        assert!(phantoms.iter().any(|s| s == parent), "{phantoms:?}");
    }
    assert_eq!(saids(&fold(&forked(OTHER))), ["bind 0"]);
}

/// A PAGINATED `/fork` copies nothing (measured on 0.163.0): its
/// `session_meta` and its own settings line in one millisecond, then a
/// person's turn seconds on, read on its own counts. A turn right after the
/// settings line is read too: the line, not the stamps, ends the copy.
/// NEGATIVE CONTROL: with the settings line naming another thread, a turn
/// inside the burst is held as the copy's.
#[test]
fn a_paginated_fork_reads_its_own_turn() {
    let fork = |own: &str, after: i64| {
        [
            meta_as(T0, FORK, FORK, Some(PARENT), Some(PAGINATED), None),
            settings(T0, own),
            ctx(T0 + after, "turn-f1", "gpt-6.1-sol"),
            count(T0 + after + 10, (47_270, 0, 33), (15_860, 0, 10)),
            done(T0 + after + 20, "turn-f1", 20, ""),
        ]
        .concat()
    };
    let read = [
        "bind 0",
        "prompt turn-f1",
        "model gpt-6.1-sol - 0.163.0",
        "end 20 15860 10",
    ];
    assert_eq!(saids(&fold(&fork(FORK, 14_000))), read);
    assert_eq!(saids(&fold(&fork(FORK, 9))), read);
    assert_eq!(saids(&fold(&fork(OTHER, 14_000))), read, "the burst ended");
    // NEGATIVE CONTROL.
    assert_eq!(saids(&fold(&fork(OTHER, 9))), ["bind 0"]);
}

/// ONE TURN-OPEN KEY, ONCE PER TURN: a `turn_context` written again for a
/// running turn opens nothing, and `task_started` opens nothing — with no
/// `turn_context`, the lab's rollout opens, ends and interrupts no turn,
/// its calls still read. A key spelled with escapes is read. NEGATIVE
/// CONTROL: a second `turn_context` naming another turn opens it.
#[test]
fn a_turn_opens_once_at_its_turn_context() {
    let again: String = rows(MAIN)
        .flat_map(|r| {
            let twice = r.contains("\"type\":\"turn_context\"");
            std::iter::repeat_n(r, 1 + usize::from(twice))
        })
        .map(|r| format!("{r}\n"))
        .collect();
    assert_eq!(saids(&fold(&again)), saids(&fold(MAIN)));
    let unopened: String = rows(MAIN)
        .filter(|r| !r.contains("\"type\":\"turn_context\""))
        .map(|r| format!("{r}\n"))
        .collect();
    let read = saids(&fold(&unopened));
    assert!(MAIN.contains("\"task_started\""));
    assert!(
        !read
            .iter()
            .any(|s| s.starts_with("prompt") || s.starts_with("end") || s == "interrupt"),
        "{read:?}"
    );
    assert_eq!(read.iter().filter(|s| s.starts_with("call")).count(), 4);
    let escaped = [
        opening(),
        ctx(T0 + 1, "turn-1", "gpt-6.1-sol").replace("\"turn_context\"", "\"turn_\\u0063ontext\""),
    ]
    .concat();
    assert!(saids(&fold(&escaped)).contains(&"prompt turn-1".to_string()));
    // NEGATIVE CONTROL.
    let other = [
        opening(),
        ctx(T0 + 1, "turn-1", "gpt-6.1-sol"),
        ctx(T0 + 2, "turn-2", "gpt-6.1-sol"),
    ]
    .concat();
    let read = saids(&fold(&other));
    assert!(read.contains(&"prompt turn-2".to_string()), "{read:?}");
}

/// FAIL CLOSED: a key the fold reads with the wrong shape latches it off
/// for the rollout, and it steps nothing more until a bind lifts the latch;
/// a call id over the log's 40 bytes is counted lost. A bad row in the
/// history (at or below the seed) latches nothing. NEGATIVE CONTROL: the
/// well-shaped rows latch nothing.
#[test]
fn a_wrong_shape_latches_the_fold_off() {
    let base = [opening(), ctx(T0 + 1, "turn-1", "gpt-6.1-sol")].concat();
    let long = "t".repeat(StepId::MAX + 1);
    let bad = [
        ctx(T0 + 2, "turn-1", "gpt-6.1-sol").replace("\"turn_id\"", "\"turn\""),
        ctx(T0 + 2, &long, "gpt-6.1-sol"),
        done(T0 + 2, "turn-1", 5, "").replace("\"duration_ms\"", "\"duration\""),
        done(T0 + 2, "turn-1", 5, "").replace("\"turn_id\"", "\"turn\""),
        aborted(T0 + 2, "turn-1").replace("\"turn_id\"", "\"turn\""),
        call(T0 + 2, "call-1", "functions", "exec").replace("\"call_id\"", "\"call\""),
        output(T0 + 2, "call-1").replace("\"call_id\"", "\"call\""),
        call(T0 + 2, "call-1", "functions", "exec").replace(&stamp(T0 + 2), "never"),
    ];
    let after = done(T0 + 3, "turn-1", 5, "");
    for row in &bad {
        let text = [base.as_str(), row, &after].concat();
        let (fold, batch) = fold_with(&text, 0, 0);
        let read = saids(&batch);
        assert!(fold.broken(), "{row}");
        assert_eq!(
            read.last().map(String::as_str),
            Some("off unknown"),
            "{row}"
        );
        assert!(
            !read.iter().any(|s| s.starts_with("end")),
            "{row}: {read:?}"
        );
        // In the history, the same row latches nothing.
        let seed = (base.len() + row.len()) as u64;
        let (fold, batch) = fold_with(&text, seed, 0);
        assert!(!fold.broken(), "{row}");
        assert_eq!(saids(&batch), [format!("bind {seed}"), "end 5 0 0".into()]);
    }
    // A bind lifts the latch.
    let text = [base.as_str(), &bad[0]].concat();
    let (mut fold, _) = fold_with(&text, 0, 0);
    fold.bind(FP, ID, 0);
    for row in rows(&base) {
        fold.push(row.as_bytes(), 1);
    }
    assert!(!fold.broken());
    assert!(saids(&fold.take()).contains(&"prompt turn-1".to_string()));
    // A call id over 40 bytes is lost, never cut.
    let long_call = [base.as_str(), &call(T0 + 2, &long, "functions", "exec")].concat();
    let (fold, batch) = fold_with(&long_call, 0, 0);
    assert!(!fold.broken());
    assert_eq!(batch.lost, 1);
    assert!(!saids(&batch).iter().any(|s| s.starts_with("call")));
    // NEGATIVE CONTROL.
    let good = [
        base.as_str(),
        &call(T0 + 2, "call-1", "functions", "exec"),
        &output(T0 + 3, "call-1"),
        &done(T0 + 4, "turn-1", 5, ""),
    ]
    .concat();
    let (fold, batch) = fold_with(&good, 0, 0);
    assert!(!fold.broken());
    assert_eq!(
        saids(&batch),
        [
            "bind 0",
            "prompt turn-1",
            "model gpt-6.1-sol - 0.163.0",
            "call call-1 exec other",
            "result call-1",
            "end 5 0 0"
        ]
    );
}

/// THE SEED AND THE FLOOR: rows at or below the seed, and rows older than
/// the floor, fold into the open turn and its tokens but queue nothing, and
/// the model is said from the first row past them. NEGATIVE CONTROL: bound
/// at 0 with no floor, the same rows queue the turn's prompt.
#[test]
fn history_folds_and_queues_nothing() {
    let text: String = rows(MAIN).map(|r| format!("{r}\n")).collect();
    let row_end = |needle: &str| {
        let at = text.find(needle).expect("a row");
        (at + text[at..].find('\n').expect("a line")) as u64 + 1
    };
    let seed = row_end(
        "\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"00000000-0000-4000-8000-00000000000e\"",
    );
    let (_, batch) = fold_with(&text, seed, 0);
    let read = saids(&batch);
    assert_eq!(
        read[..5],
        [
            format!("bind {seed}"),
            "call call_r0019 exec other".into(),
            "result call_r0019".into(),
            "end 946 15390 22".into(),
            turn("18"),
        ]
    );
    assert_eq!(read[5], "model gpt-6.1-sol - 0.163.0");
    assert!(batch.steps[1..].iter().all(|s| s.pos.off > seed));
    // The floor: the third turn's start.
    let floor = u64::try_from(stamp_ms("2000-01-01T11:41:24.538Z").expect("a stamp")).expect("ms");
    let (_, batch) = fold_with(&text, 0, floor);
    let read = saids(&batch);
    assert_eq!(read[..2], ["bind 0".to_string(), turn("18")]);
    assert!(batch.steps[1..].iter().all(|s| s.pos.at_ms >= floor));
    // NEGATIVE CONTROL.
    assert!(saids(&fold(&text)).contains(&turn("0e")));
}

/// THE QUEUE IS BOUNDED: past [`STEP_QUEUE`] steps between takes the oldest
/// go and are counted; the bind stays. NEGATIVE CONTROL: a take between
/// keeps every step.
#[test]
fn the_queue_is_bounded() {
    let calls: String = (0..STEP_QUEUE + 10)
        .map(|i| call(T0 + 2, &format!("call-{i}"), "functions", "exec"))
        .collect();
    let text = [opening(), ctx(T0 + 1, "turn-1", "gpt-6.1-sol"), calls].concat();
    let (_, batch) = fold_with(&text, 0, 0);
    assert_eq!(batch.steps.len(), STEP_QUEUE);
    assert_eq!(batch.overflow, 13);
    assert_eq!(said(&batch.steps[0].ev), "bind 0");
    // NEGATIVE CONTROL.
    let mut fold = CodexStepFold::new(0);
    fold.bind(FP, ID, 0);
    let (mut end, mut n) = (0u64, 0usize);
    for (i, row) in rows(&text).enumerate() {
        end += row.len() as u64 + 1;
        fold.push(row.as_bytes(), end);
        if i % 100 == 0 {
            n += fold.take().steps.len();
        }
    }
    let last = fold.take();
    assert_eq!(n + last.steps.len(), STEP_QUEUE + 13);
    assert_eq!(last.overflow, 0);
}

/// A BOUNDED READ: [`fold_rollout`] reads at most its cap from the
/// rollout's first byte; a line over the line bound is skipped as it
/// streams and counted lost past the seed, as is a line that is not UTF-8,
/// and a torn last line is not read. NEGATIVE CONTROLS: a cap that stops
/// short says so; a seed past the long line counts it as history.
#[test]
fn a_rollout_is_read_within_its_bounds() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("codex-steps-read-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("rollout.jsonl");
    let head = [opening(), ctx(T0 + 1, "turn-1", "gpt-6.1-sol")].concat();
    let mut bytes = head.clone().into_bytes();
    bytes.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES + 1));
    bytes.push(b'\n');
    let long_end = bytes.len() as u64;
    bytes.extend_from_slice(b"{\"timestamp\":\"\xff\"}\n");
    bytes.extend_from_slice(done(T0 + 3, "turn-1", 5, "").as_bytes());
    let torn = done(T0 + 4, "turn-1", 6, "");
    bytes.extend_from_slice(&torn.as_bytes()[..torn.len() / 2]);
    std::fs::write(&path, &bytes).expect("write");
    let mut fold = CodexStepFold::new(0);
    fold.bind(FP, ID, 0);
    let read = fold_rollout(&path, &mut fold, u64::MAX).expect("read");
    assert!(read.complete);
    assert_eq!(read.long, 1);
    let batch = fold.take();
    assert_eq!(batch.lost, 2, "the long line and the one not UTF-8");
    assert_eq!(
        saids(&batch),
        [
            "bind 0",
            "prompt turn-1",
            "model gpt-6.1-sol - 0.163.0",
            "end 5 0 0"
        ]
    );
    // NEGATIVE CONTROLS.
    let mut fold = CodexStepFold::new(0);
    fold.bind(FP, ID, long_end);
    fold_rollout(&path, &mut fold, u64::MAX).expect("read");
    assert_eq!(fold.take().lost, 1, "only the line past the seed");
    let mut fold = CodexStepFold::new(0);
    fold.bind(FP, ID, 0);
    let short = fold_rollout(&path, &mut fold, head.len() as u64).expect("read");
    assert!(!short.complete);
    assert_eq!(short.read, head.len() as u64);
    assert!(fold_rollout(&dir.path().join("none"), &mut fold, 1).is_none());
}

/// The build a committed reduction is of: `<stem>.jsonl` or a leg of it,
/// `<stem>-<leg>.jsonl` (the stem `rollout_reduction_stem`'s: `codex-<build>`,
/// an ALab build's `codex-alab-<release>`), as its header (`# codex <build>
/// · …`) names it.
fn build_of<'a>(name: &str, header: &'a str) -> Option<&'a str> {
    let build = header.strip_prefix("# codex ")?.split(' ').next()?;
    let rest = name.strip_prefix(&rollout_reduction_stem(build))?;
    (rest == ".jsonl" || (rest.starts_with('-') && rest.ends_with(".jsonl"))).then_some(build)
}

/// The committed reductions of `build` ([`build_of`]), by name.
fn committed_of(build: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(STEPS_DIR)
        .expect("the testdata")
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(e.path()).ok()?;
            let header = text.lines().next().unwrap_or("");
            (build_of(&name, header) == Some(build)).then_some((name, text))
        })
        .collect();
    out.sort();
    out
}

/// EVERY COMMITTED CODEX REDUCTION FOLDS WHOLE (rule, for
/// Codex): each `codex-<build>[-<leg>].jsonl` is the build its header names,
/// every `session_meta` in it names that build, and the fold reads it with
/// no latch, no loss and no overflow, every step at the build's state —
/// partial while [`CODEX_MEASURED_BUILDS`] names none, and each build it
/// names has its reduction committed. NEGATIVE CONTROLS: a name and a
/// header of two builds name none, and a header naming no build names none.
#[test]
fn every_committed_codex_reduction_folds_whole() {
    let mut read = 0;
    for e in std::fs::read_dir(STEPS_DIR)
        .expect("the testdata")
        .filter_map(Result::ok)
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("codex-") {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).expect("a reduction");
        let header = text.lines().next().unwrap_or("");
        let build = build_of(&name, header).unwrap_or_else(|| panic!("{name}: {header}"));
        let metas = rows(&text).filter(|r| r.contains("\"type\":\"session_meta\""));
        for m in metas {
            assert!(
                m.contains(&format!("\"cli_version\":\"{build}\"")),
                "{name}"
            );
        }
        let batch = fold(&text);
        assert_eq!((batch.lost, batch.overflow), (0, 0), "{name}");
        let want = state_for(CODEX_MEASURED_BUILDS, Some(build));
        for s in &batch.steps[1..] {
            assert!(!matches!(s.ev, StepEvent::Off { .. }), "{name}");
            assert_eq!(s.state, want, "{name}");
        }
        assert!(
            batch
                .steps
                .iter()
                .any(|s| matches!(s.ev, StepEvent::TurnEnd { .. }))
        );
        read += 1;
    }
    assert!(read >= 2, "the lab's rollouts are committed");
    for b in CODEX_MEASURED_BUILDS {
        assert!(
            !committed_of(b).is_empty(),
            "{b} is named, and its reduction is not committed"
        );
    }
    // NEGATIVE CONTROLS.
    assert_eq!(
        build_of("codex-0.164.0.jsonl", "# codex 0.163.0 · lab reduce"),
        None
    );
    assert_eq!(
        build_of("codex-0.163.0.jsonl", "# 2.1.303 · lab reduce"),
        None
    );
    assert_eq!(
        build_of(
            "codex-0.163.0-subagent.jsonl",
            "# codex 0.163.0 · lab reduce"
        ),
        Some("0.163.0")
    );
    let alab = "# codex 0.164.0-dev.12521+g3d3eb12d8 · lab reduce";
    assert_eq!(
        build_of("codex-alab-0.164.0-dev.12521-first.jsonl", alab),
        Some("0.164.0-dev.12521+g3d3eb12d8")
    );
    // An ALab build is named by its release, never by its whole version.
    assert_eq!(
        build_of("codex-0.164.0-dev.12521+g3d3eb12d8-first.jsonl", alab),
        None
    );
}

/// THE CLASS TABLE holds only what a lab rollout showed: `spawn_agent` in
/// `collaboration` starts a subagent (0.163.0), and `exec_command` in no
/// namespace runs a shell command (codex-alab's approval leg). NEGATIVE
/// CONTROLS: either name in another namespace, or none, is `other`, as is
/// every other call (0.163.0's `exec`, either build's `request_user_input`).
#[test]
fn only_what_a_lab_rollout_showed_is_classed() {
    assert_eq!(
        tool_class(Some("collaboration"), "spawn_agent"),
        ("agent", true)
    );
    assert_eq!(tool_class(None, "exec_command"), ("shell", false));
    for (namespace, name) in [
        (Some("functions"), "spawn_agent"),
        (None, "spawn_agent"),
        (Some("functions"), "exec"),
        (None, "exec"),
        (Some("functions"), "exec_command"),
        (Some("functions"), "request_user_input"),
        (None, "request_user_input"),
        (Some("collaboration"), "wait_agent"),
    ] {
        assert_eq!(tool_class(namespace, name), ("other", false), "{name}");
    }
}

/// NO TEXT: no step of the lab's rollouts, or of composed rows
/// carrying sentinels where Codex writes text, holds any of that text.
#[test]
fn no_step_holds_text() {
    let text = [
        opening(),
        ctx(T0 + 1, "turn-1", "gpt-6.1-sol"),
        call(T0 + 2, "call-1", "functions", "exec"),
        output(T0 + 3, "call-1"),
        done(
            T0 + 4,
            "turn-1",
            5,
            ",\"error\":{\"message\":\"SENTINEL-ERROR\",\"codex_error_info\":\"usage_limit_exceeded\"}",
        ),
    ]
    .concat();
    let batch = fold(&text);
    assert!(saids(&batch).contains(&"wall UsageSession".to_string()));
    for b in [batch, fold(MAIN), fold(SUBAGENT)] {
        let all = format!("{:?}", b.steps);
        assert!(!all.contains("SENTINEL"), "{all}");
    }
}

/// The codex-alab builds the lab measured: the one
/// built from its source for the run, and the owner's managed one, read
/// from its store.
const ALAB_BUILDS: [&str; 2] = [ALAB, "0.164.0-dev.12479+g07a25d86a"];

/// The build the legs' steps are written out for ([`ALAB_LEGS`]).
const ALAB: &str = "0.164.0-dev.12521+g3d3eb12d8";

/// What each leg of `lab codex-alab-proof` did on [`ALAB`], as its rollout
/// folds (6D-3a): the leg, and its steps after the bind.
const ALAB_LEGS: [(&str, &[&str]); 6] = [
    // The instance's first session: one turn that ends on the done mark
    // (its two crash relaunches resume the thread and add only its own
    // settings lines).
    (
        "first",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12521",
            "end 64 1 1",
        ],
    ),
    // A turn without the done mark, then the one `keep going` continued.
    (
        "continue",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12521",
            "end 35 1 1",
            "prompt 00000000-0000-4000-8000-00000000000b",
            "end 8 1 1",
        ],
    ),
    // An escalated `exec_command`, approved once and run, in one turn.
    (
        "approval",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12521",
            "call call_r0010 exec_command shell",
            "result call_r0010",
            "end 5083 2 2",
        ],
    ),
    // A `request_user_input` answered with its recommended option.
    (
        "question",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12521",
            "call call_r0010 request_user_input other",
            "result call_r0010",
            "end 5048 2 2",
        ],
    ),
    // The model's 429 ends the turn on the usage wall (no tokens served);
    // waited out, the resumed turn.
    (
        "usage",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12521",
            "end 32 0 0",
            "wall UsageSession",
            "prompt 00000000-0000-4000-8000-000000000009",
            "end 8 1 1",
        ],
    ),
    // The turn `SIGKILL`ed mid-request writes no end (the rollout holds
    // none); the relaunched process carries it on in a turn of its own.
    (
        "carry",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12521",
            "prompt 00000000-0000-4000-8000-000000000009",
            "end 10 1 1",
        ],
    ),
];

/// The forever-port codex-alab build the lab measured (B5c): source
/// `089291d37`, codex-alab's `alab/forever-align`, under the lab's
/// Autopilot Cruise / LAND proof, so its Cruise legs take the defend prompt
/// after each done claim and its legs hold more turns than [`ALAB`]'s; its
/// steps are written out on their own ([`FOREVER_LEGS`]).
const FOREVER: &str = "0.164.0-dev.12672+g089291d37";

/// What each leg of `lab codex-alab-proof` did on [`FOREVER`], as its
/// rollout folds: the leg, and its steps after the bind.
const FOREVER_LEGS: [(&str, &[&str]); 6] = [
    // A turn ending on the done mark, then three turns of Cruise's defend
    // prompt and the defenses after it (its two crash relaunches resume the
    // thread and add only its own settings lines).
    (
        "first",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12672",
            "end 70 1 1",
            "prompt 00000000-0000-4000-8000-00000000000b",
            "end 9 1 1",
            "prompt 00000000-0000-4000-8000-000000000010",
            "end 10 1 1",
            "prompt 00000000-0000-4000-8000-000000000015",
            "end 8 1 1",
        ],
    ),
    // A turn without the done mark, the one `keep going` continued, and
    // the continued reply's defend prompt.
    (
        "continue",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12672",
            "end 38 1 1",
            "prompt 00000000-0000-4000-8000-00000000000b",
            "end 23 1 1",
            "prompt 00000000-0000-4000-8000-000000000010",
            "end 9 1 1",
        ],
    ),
    // An escalated `exec_command`, approved once and run, in one turn; then
    // two more acts' turns.
    (
        "approval",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12672",
            "call call_r0010 exec_command shell",
            "result call_r0010",
            "end 5115 2 2",
            "prompt 00000000-0000-4000-8000-00000000000f",
            "end 22 1 1",
            "prompt 00000000-0000-4000-8000-000000000014",
            "end 22 1 1",
        ],
    ),
    // A `request_user_input` answered with its recommended option; then
    // the question asked in prose and Land's answer to it.
    (
        "question",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12672",
            "call call_r0010 request_user_input other",
            "result call_r0010",
            "end 5063 2 2",
            "prompt 00000000-0000-4000-8000-00000000000f",
            "end 22 1 1",
            "prompt 00000000-0000-4000-8000-000000000014",
            "end 22 1 1",
        ],
    ),
    // The model's 429 ends the turn on the usage wall (no tokens served);
    // waited out, the resumed turn and the continuations after it.
    (
        "usage",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12672",
            "end 37 0 0",
            "wall UsageSession",
            "prompt 00000000-0000-4000-8000-000000000009",
            "end 23 1 1",
            "prompt 00000000-0000-4000-8000-00000000000e",
            "end 15 1 1",
            "prompt 00000000-0000-4000-8000-000000000013",
            "end 20 1 1",
            "prompt 00000000-0000-4000-8000-000000000018",
            "end 22 1 1",
        ],
    ),
    // The turn `SIGKILL`ed mid-request writes no end; the relaunched
    // process carries it on in a turn of its own, and the defend prompts
    // follow its done claim.
    (
        "carry",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12672",
            "prompt 00000000-0000-4000-8000-000000000009",
            "end 24 1 1",
            "prompt 00000000-0000-4000-8000-00000000000e",
            "end 8 1 1",
            "prompt 00000000-0000-4000-8000-000000000013",
            "end 9 1 1",
            "prompt 00000000-0000-4000-8000-000000000018",
            "end 8 1 1",
        ],
    ),
];

/// Each leg's committed reduction, on [`ALAB`].
fn alab_leg(leg: &str) -> String {
    alab_leg_of(ALAB, leg)
}

/// Each leg's committed reduction, on `build`.
fn alab_leg_of(build: &str, leg: &str) -> String {
    let path = format!("{STEPS_DIR}/{}-{leg}.jsonl", rollout_reduction_stem(build));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// What a step is, its numbers and ids aside: its kind, and a call's name.
fn shape(said: &str) -> String {
    let mut w = said.split(' ');
    match (w.next(), w.next(), w.next()) {
        (Some("call"), _, Some(name)) => format!("call {name}"),
        (Some(kind), ..) => kind.to_string(),
        _ => String::new(),
    }
}

/// What the rollout itself says of each turn, apart from the fold: the
/// turns its `turn_context`s open, each `task_complete`'s duration, the
/// tokens codex-alab's own per-turn record (`token_usage_record`'s
/// `turn_token_usage`, a count the fold never reads) gives the turn last,
/// and whether its error is the usage limit; and the calls and outputs by
/// id. The fold's steps, said as [`said`] says them (no bind, no model).
fn rollout_says(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut opened: Vec<String> = Vec::new();
    let mut tokens: std::collections::HashMap<String, (u64, u64)> = Default::default();
    for row in rows(text) {
        let v: Value = aterm_json::from_str(row).map_err(|e| format!("{e}: {row}"))?;
        let p = v.get("payload");
        let s = |k: &str| p.and_then(|p| p.get(k)).and_then(Value::as_str);
        match (v.get("type").and_then(Value::as_str), s("type")) {
            (Some("turn_context"), _) => {
                let t = s("turn_id").ok_or("a turn_context names its turn")?;
                if !opened.iter().any(|o| o == t) {
                    opened.push(t.to_string());
                    out.push(format!("prompt {t}"));
                }
            }
            (Some("token_usage_record"), _) => {
                let t = s("turn_id").ok_or("a record names its turn")?;
                let n = |k: &str| {
                    p.and_then(|p| p.get("turn_token_usage"))
                        .and_then(|u| u.get(k))
                        .and_then(Value::as_u64)
                };
                let (Some(i), Some(o)) = (n("input_tokens"), n("output_tokens")) else {
                    return Err(format!("a record with no turn tokens: {row}"));
                };
                tokens.insert(t.to_string(), (i, o));
            }
            (Some("event_msg"), Some("task_complete")) => {
                let t = s("turn_id").ok_or("a task_complete names its turn")?;
                let dur = p
                    .and_then(|p| p.get("duration_ms"))
                    .and_then(Value::as_u64)
                    .ok_or("a task_complete has its duration")?;
                let (i, o) = tokens.get(t).copied().unwrap_or((0, 0));
                out.push(format!("end {dur} {i} {o}"));
                let wall = p
                    .and_then(|p| p.get("error"))
                    .and_then(|e| e.get("codex_error_info"))
                    .and_then(Value::as_str)
                    == Some("usage_limit_exceeded");
                if wall {
                    out.push("wall UsageSession".into());
                }
            }
            (Some("response_item"), Some("function_call")) => {
                let id = s("call_id").ok_or("a call has its id")?;
                let name = s("name").ok_or("a call has its name")?;
                // The approval leg's call ran the shell command its model
                // asked for; the question leg's asked the person.
                let cls = match (s("namespace"), name) {
                    (None, "exec_command") => "shell",
                    _ => "other",
                };
                out.push(format!("call {id} {name} {cls}"));
            }
            (Some("response_item"), Some("function_call_output")) => {
                let id = s("call_id").ok_or("an output has its call")?;
                out.push(format!("result {id}"));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// THE MEASURED BUILDS, EXACT: each
/// leg of `lab codex-alab-proof` folds, on each codex-alab build the lab ran
/// it on ([`ALAB_BUILDS`] and [`FOREVER`]), to what the leg did
/// ([`ALAB_LEGS`], written out for [`ALAB`], whose managed sibling's legs
/// take the same steps, numbers and ids aside; [`FOREVER_LEGS`], written out
/// for the forever-port build, B5c): its turns open once each at their `turn_context`, end at
/// their `task_complete` with its duration, the usage wall on the 429's,
/// the calls and their outputs by id, the model once, the build said as its
/// release. Every step is the rollout's own account too ([`rollout_says`]):
/// each turn's tokens are the ones codex-alab's own per-turn record gives
/// it, which the fold never reads; and every step past the bind is exact,
/// each build being measured. The legs that showed no new step (`hold` is
/// `continue`'s shape) are not committed. NEGATIVE CONTROLS: a turn record
/// whose tokens are not the counts' is told apart; and read as an
/// unmeasured build (its `cli_version` one release on), every step is
/// partial.
#[test]
fn the_codex_alab_legs_fold_exact() {
    let mut kinds = std::collections::BTreeSet::new();
    // Each build, the legs its steps are written out by, and whether they
    // are written out for it (else they are another build's, its own taking
    // the same steps, numbers and ids aside).
    let builds = ALAB_BUILDS
        .iter()
        .map(|b| (*b, &ALAB_LEGS, *b == ALAB))
        .chain([(FOREVER, &FOREVER_LEGS, true)]);
    for (build, legs, own) in builds {
        assert!(CODEX_MEASURED_BUILDS.contains(&build), "{build}");
        let release = log_build(build).expect("an id");
        for (leg, want) in legs.iter().copied() {
            let text = alab_leg_of(build, leg);
            let batch = fold(&text);
            let read = saids(&batch);
            assert_eq!(read[0], "bind 0", "{build} {leg}");
            if own {
                assert_eq!(read[1..], *want, "{build} {leg}");
            }
            let wanted: Vec<String> = want.iter().map(|s| shape(s)).collect();
            let shapes: Vec<String> = read[1..].iter().map(|s| shape(s)).collect();
            assert_eq!(shapes, wanted, "{build} {leg}");
            assert!(
                read.contains(&format!("model gpt-5.5 - {}", release.as_str())),
                "{build} {leg}"
            );
            assert_eq!((batch.lost, batch.overflow), (0, 0), "{build} {leg}");
            assert!(
                batch.steps[1..].iter().all(|s| s.state == StepState::On),
                "{build} {leg}"
            );
            let own: Vec<String> = read[1..]
                .iter()
                .filter(|s| !s.starts_with("model"))
                .cloned()
                .collect();
            assert_eq!(rollout_says(&text).expect("read"), own, "{build} {leg}");
            for s in &read[1..] {
                kinds.insert(s.split(' ').next().unwrap_or("").to_string());
            }
        }
    }
    // What the build's measurement holds (T8's guard, for Codex): a turn
    // opened and ended, a call and its output, the usage wall, the model.
    assert_eq!(
        kinds.into_iter().collect::<Vec<_>>(),
        ["call", "end", "model", "prompt", "result", "wall"]
    );
    // NEGATIVE CONTROLS.
    let approval = alab_leg("approval");
    let off = approval.replacen(
        "\"turn_token_usage\":{\"input_tokens\":2",
        "\"turn_token_usage\":{\"input_tokens\":7",
        1,
    );
    assert_ne!(off, approval, "the plant took");
    let folded: Vec<String> = saids(&fold(&off))[1..]
        .iter()
        .filter(|s| !s.starts_with("model"))
        .cloned()
        .collect();
    assert_ne!(rollout_says(&off).expect("read"), folded);
    let next = alab_leg("first").replace(
        "\"cli_version\":\"0.164.0-dev.12521+g3d3eb12d8\"",
        "\"cli_version\":\"0.164.0-dev.12522+g3d3eb12d9\"",
    );
    let batch = fold(&next);
    assert!(
        batch.steps[1..]
            .iter()
            .all(|s| s.state == StepState::Partial)
    );
    assert!(saids(&batch).contains(&"model gpt-5.5 - 0.164.0-dev.12522".to_string()));
}

/// THE BUILD, SAID AND MATCHED (6D-3a): a build that is a log id is said
/// whole; an ALab build, whose `+` no log id holds, is said as its release
/// and matched whole; a bind handed the build its tap read says the bind at
/// that build's state. NEGATIVE CONTROLS: a build that is no id even before
/// a `+` is said as none, and a bind handed no build, or an unmeasured one,
/// is partial.
#[test]
fn a_build_is_said_as_its_release_and_matched_whole() {
    assert_eq!(
        log_build("0.163.0")
            .map(|b| b.as_str().to_owned())
            .as_deref(),
        Some("0.163.0")
    );
    assert_eq!(
        log_build(ALAB).map(|b| b.as_str().to_owned()).as_deref(),
        Some("0.164.0-dev.12521")
    );
    let mut fold = CodexStepFold::new(0);
    fold.bind_build(FP, ID, 0, Some(ALAB));
    assert_eq!(fold.take().steps[0].state, StepState::On);
    // NEGATIVE CONTROLS.
    assert!(log_build("0 163+x").is_none());
    for build in [None, Some("0.164.0-dev.12521")] {
        let mut fold = CodexStepFold::new(0);
        fold.bind_build(FP, ID, 0, build);
        assert_eq!(fold.take().steps[0].state, StepState::Partial, "{build:?}");
    }
}

/// The other codex-alab build the lab measured (the owner's managed one).
const ALAB_MANAGED: &str = "0.164.0-dev.12479+g07a25d86a";

/// THE WRITER HOLDS THE ROWS TO ITS BUILD: a thread's
/// `session_meta` names the build that started it, and a thread resumed or
/// re-exec'd onto another build appends without restating it (measured:
/// the `first` leg's rollout, written by three processes, holds one
/// `session_meta`), so where its reader knows the build writing the rows
/// now ([`CodexStepFold::set_writer`]) the fold holds them to it. The
/// `continue` leg, started on [`ALAB`]: written by [`ALAB`] it folds as its
/// golden; by the other measured build, exact still, the build said the
/// writer's; by a build nobody measured, every step partial; a writer that
/// moves between its turns says the model again, at its build; and a reader
/// that stops says `off` once ([`CodexStepFold::stop`]), folding nothing
/// more. NEGATIVE CONTROLS (scratch, the unit's notes): a state that
/// ignores the writer reads the unmeasured writer `On`; a model said at the
/// thread's build names 12521 for the 12479 writer; a move that keeps the
/// model said says it once.
#[test]
fn the_writer_holds_the_rows_to_its_build() {
    let text = alab_leg("continue");
    let second = rows(&text)
        .position(|r| r.contains("\"turn_context\"") && r.contains("8000-00000000000b"))
        .expect("the continued turn's context");
    // The leg's rows folded with `writer` writing them, the writer moving to
    // `moved` at the continued turn's context, or the reader stopping there.
    let fold_by = |writer: &str, moved: Option<&str>, stop: bool| {
        let mut fold = CodexStepFold::new(0);
        fold.set_writer(writer);
        fold.bind(FP, ID, 0);
        let mut end = 0u64;
        for (i, row) in rows(&text).enumerate() {
            if i == second {
                if let Some(build) = moved {
                    fold.set_writer(build);
                }
                if stop {
                    fold.stop(end);
                }
            }
            end += row.len() as u64 + 1;
            fold.push(row.as_bytes(), end);
        }
        fold.take()
    };
    let golden = ALAB_LEGS
        .iter()
        .find(|(leg, _)| *leg == "continue")
        .map(|(_, want)| *want)
        .expect("the continue leg");
    // (The bind is said before the thread's `session_meta` is read.)
    let batch = fold_by(ALAB, None, false);
    assert_eq!(saids(&batch)[1..], *golden);
    assert!(batch.steps[1..].iter().all(|s| s.state == StepState::On));
    let at = |release: &str| {
        golden
            .iter()
            .map(|s| s.replace("0.164.0-dev.12521", release))
            .collect::<Vec<_>>()
    };
    let batch = fold_by(ALAB_MANAGED, None, false);
    assert_eq!(
        saids(&batch)[1..],
        at("0.164.0-dev.12479"),
        "the writer's build"
    );
    assert!(batch.steps[1..].iter().all(|s| s.state == StepState::On));
    let batch = fold_by("0.164.0-dev.99999+g0123abcd", None, false);
    assert_eq!(saids(&batch)[1..], at("0.164.0-dev.99999"));
    assert!(
        batch.steps.iter().all(|s| s.state == StepState::Partial),
        "no row of a writer nobody measured is exact"
    );
    let moved = saids(&fold_by(ALAB, Some(ALAB_MANAGED), false));
    assert_eq!(
        moved[1..],
        [
            golden[0],
            golden[1],
            golden[2],
            golden[3],
            "model gpt-5.5 - 0.164.0-dev.12479",
            golden[4],
        ],
        "a writer that moved says the model again"
    );
    let stopped = saids(&fold_by(ALAB, None, true));
    assert_eq!(
        stopped[1..],
        [golden[0], golden[1], golden[2], "off unmeasured"],
        "a reader that stops says off once, and folds nothing more"
    );
}
