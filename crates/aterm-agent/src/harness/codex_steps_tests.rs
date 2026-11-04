// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`] over committed Codex rollout reductions and synthetic rows.

use super::*;
use crate::harness::codex_rollout::PAGINATED;
use crate::harness::codex_rollout::fixtures::{count, message, meta, meta_as, settings, stamp};
use crate::harness::usage::MAX_LINE_BYTES;
use aterm_phase::anchors::rollout_reduction_stem;

const FP: u64 = 0xc0de_0000_0000_0001;
const ID: Option<(u64, u64)> = Some((16_777_220, 78));

const MAIN: &str = include_str!("testdata/steps/codex-0.163.0.jsonl");
const SUBAGENT: &str = include_str!("testdata/steps/codex-0.163.0-subagent.jsonl");

const STEPS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/harness/testdata/steps");

const PARENT: &str = "01900000-0000-7000-8000-000000000001";
const FORK: &str = "01900000-0000-7000-8000-000000000002";
const OTHER: &str = "01900000-0000-7000-8000-000000000003";
const T0: i64 = 1_790_000_000_000;

fn rows(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| !l.starts_with('#'))
}

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

fn said(ev: &StepEvent) -> String {
    match ev {
        StepEvent::Bind { len } => format!("bind {len}"),
        StepEvent::Prompt { vref } => format!("prompt {}", vref.as_str()),
        StepEvent::HarnessTurn { vref } => format!("harness {}", vref.as_str()),
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

fn turn(n: &str) -> String {
    format!("prompt 00000000-0000-4000-8000-0000000000{n}")
}

fn ctx(ms: i64, id: &str, model: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"{id}\",\
         \"root_turn_id\":\"{id}\",\"cwd\":\"/stand-in/project\",\"approval_policy\":\
         \"on-request\",\"sandbox_policy\":{{\"type\":\"workspace-write\"}},\"model\":\
         \"{model}\",\"summary\":\"auto\"}}}}\n",
        stamp(ms)
    )
}

fn done(ms: i64, id: &str, dur: u64, extra: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\
         \"turn_id\":\"{id}\",\"last_agent_message\":\"SENTINEL-REPLY\",\"duration_ms\":{dur}\
         {extra}}}}}\n",
        stamp(ms)
    )
}

fn aborted(ms: i64, id: &str) -> String {
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"turn_aborted\",\
         \"turn_id\":\"{id}\",\"reason\":\"interrupted\",\"duration_ms\":40}}}}\n",
        stamp(ms)
    )
}

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

fn opening() -> String {
    meta_as(T0, PARENT, PARENT, None, Some(PAGINATED), None)
}

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
    let offs: Vec<u64> = batch.steps.iter().map(|s| s.pos.off).collect();
    assert!(offs.windows(2).all(|w| w[0] < w[1]), "{offs:?}");
    let y2000 = u64::try_from(stamp_ms("2000-01-01T00:00:00Z").expect("a stamp")).expect("ms");
    assert!(batch.steps[1..].iter().all(|s| s.pos.at_ms > y2000));
    assert!(batch.steps.iter().all(|s| s.pos.fp == FP && s.pos.id == ID));
}

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
    assert_eq!(saids(&fold(&fork(OTHER, 9))), ["bind 0"]);
}

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
    let other = [
        opening(),
        ctx(T0 + 1, "turn-1", "gpt-6.1-sol"),
        ctx(T0 + 2, "turn-2", "gpt-6.1-sol"),
    ]
    .concat();
    let read = saids(&fold(&other));
    assert!(read.contains(&"prompt turn-2".to_string()), "{read:?}");
}

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
        let seed = (base.len() + row.len()) as u64;
        let (fold, batch) = fold_with(&text, seed, 0);
        assert!(!fold.broken(), "{row}");
        assert_eq!(saids(&batch), [format!("bind {seed}"), "end 5 0 0".into()]);
    }
    let text = [base.as_str(), &bad[0]].concat();
    let (mut fold, _) = fold_with(&text, 0, 0);
    fold.bind(FP, ID, 0);
    for row in rows(&base) {
        fold.push(row.as_bytes(), 1);
    }
    assert!(!fold.broken());
    assert!(saids(&fold.take()).contains(&"prompt turn-1".to_string()));
    let long_call = [base.as_str(), &call(T0 + 2, &long, "functions", "exec")].concat();
    let (fold, batch) = fold_with(&long_call, 0, 0);
    assert!(!fold.broken());
    assert_eq!(batch.lost, 1);
    assert!(!saids(&batch).iter().any(|s| s.starts_with("call")));
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
    let floor = u64::try_from(stamp_ms("2000-01-01T11:41:24.538Z").expect("a stamp")).expect("ms");
    let (_, batch) = fold_with(&text, 0, floor);
    let read = saids(&batch);
    assert_eq!(read[..2], ["bind 0".to_string(), turn("18")]);
    assert!(batch.steps[1..].iter().all(|s| s.pos.at_ms >= floor));
    assert!(saids(&fold(&text)).contains(&turn("0e")));
}

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

fn build_of<'a>(name: &str, header: &'a str) -> Option<&'a str> {
    let build = header.strip_prefix("# codex ")?.split(' ').next()?;
    let rest = name.strip_prefix(&rollout_reduction_stem(build))?;
    (rest == ".jsonl" || (rest.starts_with('-') && rest.ends_with(".jsonl"))).then_some(build)
}

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
        assert_eq!(
            CODEX_MEASURED_BUILDS.contains(&build),
            rollout_reduction_stem(build).starts_with("codex-alab-"),
            "{name}: build.rs lists every codex-alab build a reduction names, and no other"
        );
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
    assert_eq!(
        build_of("codex-0.164.0-dev.12521+g3d3eb12d8-first.jsonl", alab),
        None
    );
}

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

const ALAB: &str = "0.164.0-dev.12521+g3d3eb12d8";

const ALAB_MANAGED: &str = "0.164.0-dev.12479+g07a25d86a";

/// A golden of a measured build's legs: the build, its legs' steps, and whether they are its own (the
/// managed 12479 is held to 12521's shapes).
type Golden = (
    &'static str,
    &'static [(&'static str, &'static [&'static str])],
    bool,
);

/// The goldens written for measured builds. A golden is an extra: every measured build is held to its
/// rollout's own account whether one is written or not.
const GOLDENS: [Golden; 5] = [
    (ALAB, &ALAB_LEGS, true),
    (ALAB_MANAGED, &ALAB_LEGS, false),
    (FOREVER, &FOREVER_LEGS, true),
    (EXCHANGE, &EXCHANGE_LEGS, true),
    (V165, &V165_LEGS, true),
];

const ALAB_LEGS: [(&str, &[&str]); 6] = [
    (
        "first",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.12521",
            "end 64 1 1",
        ],
    ),
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

const FOREVER: &str = "0.164.0-dev.12672+g089291d37";

const FOREVER_LEGS: [(&str, &[&str]); 6] = [
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

/// The build that records `left-to-person@v1`, which the proof's exchange legs need (codex-alab `32a73bca2`).
const EXCHANGE: &str = "0.164.0-dev.13172+g32a73bca2";

const EXCHANGE_LEGS: [(&str, &[&str]); 6] = [
    (
        "first",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.13172",
            "end 52 1 1",
            "prompt 00000000-0000-4000-8000-00000000000b",
            "end 12 1 1",
            "prompt 00000000-0000-4000-8000-000000000010",
            "end 20 1 1",
        ],
    ),
    (
        "continue",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.13172",
            "end 31 1 1",
            "prompt 00000000-0000-4000-8000-00000000000b",
            "end 23 1 1",
            "prompt 00000000-0000-4000-8000-000000000010",
            "end 11 1 1",
        ],
    ),
    (
        "approval",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.13172",
            "call call_r0010 exec_command shell",
            "result call_r0010",
            "end 5107 2 2",
            "prompt 00000000-0000-4000-8000-00000000000f",
            "end 25 1 1",
            "prompt 00000000-0000-4000-8000-000000000014",
            "end 27 1 1",
        ],
    ),
    (
        "question",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.13172",
            "call call_r0010 request_user_input other",
            "result call_r0010",
            "end 5060 2 2",
            "prompt 00000000-0000-4000-8000-00000000000f",
            "end 24 1 1",
            "prompt 00000000-0000-4000-8000-000000000014",
            "end 25 1 1",
        ],
    ),
    (
        "usage",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.13172",
            "end 29 0 0",
            "wall UsageSession",
            "prompt 00000000-0000-4000-8000-000000000009",
            "end 28 1 1",
            "prompt 00000000-0000-4000-8000-00000000000e",
            "end 20 1 1",
            "prompt 00000000-0000-4000-8000-000000000013",
            "end 20 1 1",
            "prompt 00000000-0000-4000-8000-000000000018",
            "end 24 1 1",
        ],
    ),
    (
        "carry",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.164.0-dev.13172",
            "prompt 00000000-0000-4000-8000-000000000009",
            "end 29 1 1",
            "prompt 00000000-0000-4000-8000-00000000000e",
            "end 13 1 1",
            "prompt 00000000-0000-4000-8000-000000000013",
            "end 14 1 1",
            "prompt 00000000-0000-4000-8000-000000000018",
            "end 14 1 1",
        ],
    ),
];

/// The first 0.165 build, the one this Mac's `codex` ran when it was measured (codex-alab `870dabaa6`).
const V165: &str = "0.165.0-dev.13594+g870dabaa6";

const V165_LEGS: [(&str, &[&str]); 6] = [
    (
        "first",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.165.0-dev.13594",
            "end 51 1 1",
            "prompt 00000000-0000-4000-8000-00000000000b",
            "end 14 1 1",
            "prompt 00000000-0000-4000-8000-000000000010",
            "end 27 1 1",
        ],
    ),
    (
        "continue",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.165.0-dev.13594",
            "end 43 1 1",
            "prompt 00000000-0000-4000-8000-00000000000b",
            "end 30 1 1",
            "prompt 00000000-0000-4000-8000-000000000010",
            "end 14 1 1",
        ],
    ),
    (
        "approval",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.165.0-dev.13594",
            "call call_r0010 exec_command shell",
            "result call_r0010",
            "end 5104 2 2",
            "prompt 00000000-0000-4000-8000-00000000000f",
            "end 41 1 1",
            "prompt 00000000-0000-4000-8000-000000000014",
            "end 12 1 1",
        ],
    ),
    (
        "question",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.165.0-dev.13594",
            "call call_r0010 request_user_input other",
            "result call_r0010",
            "end 5077 2 2",
            "prompt 00000000-0000-4000-8000-00000000000f",
            "end 33 1 1",
            "prompt 00000000-0000-4000-8000-000000000014",
            "end 15 1 1",
        ],
    ),
    (
        "usage",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.165.0-dev.13594",
            "end 32 0 0",
            "wall UsageSession",
            "prompt 00000000-0000-4000-8000-000000000009",
            "end 24 1 1",
            "prompt 00000000-0000-4000-8000-00000000000e",
            "end 24 1 1",
            "prompt 00000000-0000-4000-8000-000000000013",
            "end 14 1 1",
            "prompt 00000000-0000-4000-8000-000000000018",
            "end 18 1 1",
        ],
    ),
    (
        "carry",
        &[
            "prompt 00000000-0000-4000-8000-000000000003",
            "model gpt-5.5 - 0.165.0-dev.13594",
            "prompt 00000000-0000-4000-8000-000000000009",
            "end 37 1 1",
            "prompt 00000000-0000-4000-8000-00000000000e",
            "end 14 1 1",
            "prompt 00000000-0000-4000-8000-000000000013",
            "end 15 1 1",
            "prompt 00000000-0000-4000-8000-000000000018",
            "end 13 1 1",
        ],
    ),
];

fn alab_leg(leg: &str) -> String {
    alab_leg_of(ALAB, leg)
}

fn alab_leg_of(build: &str, leg: &str) -> String {
    let path = format!("{STEPS_DIR}/{}-{leg}.jsonl", rollout_reduction_stem(build));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn shape(said: &str) -> String {
    let mut w = said.split(' ');
    match (w.next(), w.next(), w.next()) {
        (Some("call"), _, Some(name)) => format!("call {name}"),
        (Some(kind), ..) => kind.to_string(),
        _ => String::new(),
    }
}

/// The steps the rollout `text` of a build said as `release` gives by its own rows, read apart from the fold.
fn rollout_says(text: &str, release: &str, shown: Gate) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut opened: Vec<String> = Vec::new();
    let mut open: Option<String> = None;
    let mut begun: Option<String> = None;
    let mut calls: Vec<(String, bool)> = Vec::new();
    let mut model: Option<(String, String)> = None;
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
                    if begun.take().as_deref() == Some(t) {
                        out.push(format!("harness {t}"));
                    }
                }
                open = Some(t.to_string());
                if let Some(m) = s("model") {
                    let now = (m.to_string(), s("effort").unwrap_or("-").to_string());
                    if model.as_ref() != Some(&now) {
                        out.push(format!("model {} {} {release}", now.0, now.1));
                        model = Some(now);
                    }
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
            (Some("event_msg"), Some("task_started")) if shown.autopilot => {
                let trigger = p
                    .and_then(|p| p.get("turn_attribution"))
                    .and_then(|a| a.get("turn_trigger"))
                    .and_then(Value::as_str);
                if let (Some("alab_autopilot"), Some(t)) = (trigger, s("turn_id")) {
                    if open.as_deref() == Some(t) {
                        out.push(format!("harness {t}"));
                    } else if !opened.iter().any(|o| o == t) {
                        begun = Some(t.to_string());
                    }
                }
            }
            (Some("event_msg"), Some("item_completed")) if shown.exit => {
                let item = p.and_then(|p| p.get("item"));
                let id = item.and_then(|i| i.get("id")).and_then(Value::as_str);
                let code = item.and_then(|i| i.get("exit_code"));
                if let (Some(id), Some(code)) = (id, code)
                    && let Some(call) = calls.iter_mut().find(|(c, _)| c == id)
                {
                    call.1 = code.as_i64().is_some_and(|x| x != 0);
                }
            }
            (Some("event_msg"), Some("turn_aborted")) => {
                open = None;
                calls.clear();
            }
            (Some("event_msg"), Some("task_complete")) => {
                open = None;
                calls.clear();
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
                let cls = match (s("namespace"), name) {
                    (None, "exec_command") => "shell",
                    _ => "other",
                };
                out.push(format!("call {id} {name} {cls}"));
                if shown.exit && !calls.iter().any(|(c, _)| c == id) {
                    calls.push((id.to_string(), false));
                }
            }
            (Some("response_item"), Some("function_call_output")) => {
                let id = s("call_id").ok_or("an output has its call")?;
                let failed = calls
                    .iter()
                    .position(|(c, _)| c == id)
                    .is_some_and(|i| calls.remove(i).1);
                out.push(format!("result {id}{}", if failed { " error" } else { "" }));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// T11, THE ACCOUNT'S GATED READS: the rollout's own account reads a failed command and an
/// Autopilot turn as the fold does where the build shows them, and neither where it does not.
#[test]
fn the_rollout_account_reads_the_gated_reads_as_the_fold() {
    let release = "0.166.0-dev.13766";
    let failed = exit_planted(&alab_leg_of(L13766, "approval"), 3);
    let read = saids(&fold(&failed));
    let said = rollout_says(&failed, release, Gate::from_words(&recounted(L13766))).expect("read");
    assert_eq!(read[1..], said);
    assert!(
        said.contains(&"result call_r0010 error".to_string()),
        "{said:?}"
    );
    let shut = rollout_says(&failed, release, Gate::NONE).expect("read");
    assert!(shut.contains(&"result call_r0010".to_string()), "{shut:?}");
    let planted = trigger_planted(AUTOPILOT_TRIGGER);
    let both = rollout_says(&planted, release, Gate::ALL).expect("read");
    assert_eq!(saids(&fold_ungated(&planted))[1..], both);
    assert!(both.contains(&format!("harness {CONTINUED}")), "{both:?}");
    let ahead = trigger_planted(AUTOPILOT_TRIGGER);
    let mut lines: Vec<&str> = ahead.lines().collect();
    let started = lines
        .iter()
        .position(|l| l.contains("\"task_started\"") && l.contains(CONTINUED))
        .expect("the continuation's start");
    let context = lines
        .iter()
        .position(|l| l.contains("\"turn_context\"") && l.contains(CONTINUED))
        .expect("the continuation's context");
    let row = lines.remove(started);
    lines.insert(context, row);
    let swapped: String = lines.iter().map(|l| format!("{l}\n")).collect();
    assert_eq!(
        saids(&fold_ungated(&swapped))[1..],
        rollout_says(&swapped, release, Gate::ALL).expect("read"),
        "in either row order"
    );
}

/// What the standing check holds the leg `name` (`text`) of the measured `build` to: the fold
/// whole and `On`, equal to the rollout's own account, and, where `build` has a golden, exactly
/// that golden (or, for a shape-only one, its shapes). The steps the fold read past its bind.
fn leg_holds(build: &str, name: &str, text: &str) -> Result<Vec<String>, String> {
    let release = log_build(build).ok_or_else(|| format!("{build}: no log id"))?;
    let batch = fold(text);
    let read = saids(&batch);
    if read.first().map(String::as_str) != Some("bind 0") {
        return Err(format!("{name}: no bind first: {read:?}"));
    }
    if (batch.lost, batch.overflow) != (0, 0) {
        return Err(format!(
            "{name}: lost {} and overflowed {}",
            batch.lost, batch.overflow
        ));
    }
    if !batch.steps[1..].iter().all(|s| s.state == StepState::On) {
        return Err(format!("{name}: a step of a measured build is not On"));
    }
    let own = rollout_says(text, release.as_str(), Gate::from_words(&recounted(build)))?;
    if !own.iter().any(|s| s.starts_with("model ")) {
        return Err(format!("{name}: the rollout names no model"));
    }
    if read[1..] != own {
        return Err(format!(
            "{name}: the fold is not the rollout's own account: {:?} where it says {own:?}",
            &read[1..]
        ));
    }
    if let Some((_, goldens, exact)) = GOLDENS.iter().find(|(b, ..)| *b == build) {
        let stem = format!("{}-", rollout_reduction_stem(build));
        let leg = name
            .strip_prefix(&stem)
            .and_then(|r| r.strip_suffix(".jsonl"))
            .ok_or_else(|| format!("{name} is no leg of {build}"))?;
        let want = goldens
            .iter()
            .find(|(l, _)| *l == leg)
            .map(|(_, w)| *w)
            .ok_or_else(|| format!("{name}: {build}'s golden has no leg {leg}"))?;
        if *exact && read[1..] != *want {
            return Err(format!(
                "{name}: the fold read {:?} where the exact golden says {want:?}",
                &read[1..]
            ));
        }
        let wanted: Vec<String> = want.iter().map(|s| shape(s)).collect();
        let shapes: Vec<String> = read[1..].iter().map(|s| shape(s)).collect();
        if shapes != wanted {
            return Err(format!(
                "{name}: the fold's shapes {shapes:?} where the golden's are {wanted:?}"
            ));
        }
    }
    Ok(read[1..].to_vec())
}

#[test]
fn the_codex_alab_legs_fold_exact() {
    let mut kinds = std::collections::BTreeSet::new();
    for build in CODEX_MEASURED_BUILDS.iter().copied() {
        let legs = committed_of(build);
        assert!(
            !legs.is_empty(),
            "{build} is named, and no reduction of it is"
        );
        let golden = GOLDENS.iter().find(|(b, ..)| *b == build);
        for (name, text) in &legs {
            let read = leg_holds(build, name, text).unwrap_or_else(|e| panic!("{e}"));
            for s in &read {
                kinds.insert(s.split(' ').next().unwrap_or("").to_string());
            }
        }
        if let Some((_, goldens, _)) = golden {
            assert_eq!(
                goldens.len(),
                legs.len(),
                "{build}: a golden leg is not committed"
            );
        }
    }
    for (build, ..) in GOLDENS {
        assert!(
            CODEX_MEASURED_BUILDS.contains(&build),
            "{build} has a golden and is not measured"
        );
    }
    let kinds: Vec<String> = kinds.into_iter().collect();
    let read = ["call", "end", "model", "prompt", "result", "wall"];
    assert!(
        read.iter().all(|k| kinds.iter().any(|s| s == k))
            && kinds
                .iter()
                .all(|k| read.contains(&k.as_str()) || k == "harness"),
        "{kinds:?}"
    );
    let approval = alab_leg("approval");
    let off = approval.replacen(
        "\"turn_token_usage\":{\"input_tokens\":2",
        "\"turn_token_usage\":{\"input_tokens\":7",
        1,
    );
    assert_ne!(off, approval, "the plant took");
    let folded = saids(&fold(&off));
    assert_eq!(
        saids(&fold(&approval)),
        folded,
        "a golden cannot see the plant: only the rollout's own account does"
    );
    assert_ne!(
        rollout_says(
            &off,
            "0.164.0-dev.12521",
            Gate::from_words(&recounted(ALAB))
        )
        .expect("read"),
        folded[1..]
    );
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

/// NC4, standing: a moved `duration_ms` is read by the fold and by the rollout's own account
/// alike (both read the number the row holds), and the reducer keeps numbers, so only an exact
/// golden sees it. The standing check ([`leg_holds`], what `the_codex_alab_legs_fold_exact`
/// holds every committed leg to) refuses it on 12521's exact golden, and a shape-only one could
/// not.
#[test]
fn a_moved_duration_fails_the_exact_golden() {
    let name = format!("{}-first.jsonl", rollout_reduction_stem(ALAB));
    let first = alab_leg("first");
    let moved = first.replacen("\"duration_ms\":64,", "\"duration_ms\":65,", 1);
    assert_ne!(moved, first, "the plant took");
    let golden = ALAB_LEGS
        .iter()
        .find(|(leg, _)| *leg == "first")
        .map(|(_, want)| *want)
        .expect("12521's first leg");
    assert_eq!(
        leg_holds(ALAB, &name, &first).expect("the committed leg holds"),
        golden
    );
    let read = saids(&fold(&moved));
    assert_eq!(
        read[1..],
        rollout_says(
            &moved,
            "0.164.0-dev.12521",
            Gate::from_words(&recounted(ALAB))
        )
        .expect("read"),
        "the rollout's own account reads the moved number too"
    );
    let e = leg_holds(ALAB, &name, &moved).expect_err("the exact golden sees the moved duration");
    assert!(e.contains("the exact golden says"), "{e}");
    assert!(read.contains(&"end 65 1 1".to_string()), "{read:?}");
    let shapes = |s: &[String]| s.iter().map(|s| shape(s)).collect::<Vec<_>>();
    let wanted: Vec<String> = golden.iter().map(|s| shape(s)).collect();
    assert_eq!(shapes(&read[1..]), wanted, "a shape-only golden cannot");
}

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
    assert!(log_build("0 163+x").is_none());
    for build in [None, Some("0.164.0-dev.12521")] {
        let mut fold = CodexStepFold::new(0);
        fold.bind_build(FP, ID, 0, build);
        assert_eq!(fold.take().steps[0].state, StepState::Partial, "{build:?}");
    }
}

/// THE WRITER HOLDS THE ROWS TO ITS BUILD: a rollout's rows are read `On` only while the process
/// writing them runs a measured build too; a writer that moves onto a build nobody measured reads
/// on, `Partial` from the move (Claude Code's unmeasured builds' rule), and says the model again
/// at its own build. NEGATIVE CONTROL: the same rows, the writer never moved, are `On` to the end.
#[test]
fn the_writer_holds_the_rows_to_its_build() {
    let text = alab_leg("continue");
    let second = rows(&text)
        .position(|r| r.contains("\"turn_context\"") && r.contains("8000-00000000000b"))
        .expect("the continued turn's context");
    let fold_by = |writer: &str, moved: Option<&str>| {
        let mut fold = CodexStepFold::new(0);
        fold.set_writer(Some(writer));
        fold.bind(FP, ID, 0);
        let mut end = 0u64;
        for (i, row) in rows(&text).enumerate() {
            if i == second
                && let Some(build) = moved
            {
                fold.set_writer(Some(build));
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
    let batch = fold_by(ALAB, None);
    assert_eq!(saids(&batch)[1..], *golden);
    assert!(batch.steps[1..].iter().all(|s| s.state == StepState::On));
    let at = |release: &str| {
        golden
            .iter()
            .map(|s| s.replace("0.164.0-dev.12521", release))
            .collect::<Vec<_>>()
    };
    let batch = fold_by(ALAB_MANAGED, None);
    assert_eq!(
        saids(&batch)[1..],
        at("0.164.0-dev.12479"),
        "the writer's build"
    );
    assert!(batch.steps[1..].iter().all(|s| s.state == StepState::On));
    let batch = fold_by("0.164.0-dev.99999+g0123abcd", None);
    assert_eq!(saids(&batch)[1..], at("0.164.0-dev.99999"));
    assert!(
        batch.steps.iter().all(|s| s.state == StepState::Partial),
        "no row of a writer nobody measured is exact"
    );
    let moved = saids(&fold_by(ALAB, Some(ALAB_MANAGED)));
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
    let unmeasured = fold_by(ALAB, Some("0.164.0-dev.99999+g0123abcd"));
    assert_eq!(
        saids(&unmeasured)[1..],
        [
            golden[0],
            golden[1],
            golden[2],
            golden[3],
            "model gpt-5.5 - 0.164.0-dev.99999",
            golden[4],
        ],
        "a writer that moved onto a build nobody measured is read on, and says the model again"
    );
    let states: Vec<StepState> = unmeasured.steps[1..].iter().map(|s| s.state).collect();
    assert_eq!(
        states,
        [
            StepState::On,
            StepState::On,
            StepState::On,
            StepState::Partial,
            StepState::Partial,
            StepState::Partial,
        ],
        "On up to the move, Partial from it"
    );
    let mut none = CodexStepFold::new(0);
    none.set_writer(None);
    none.bind_build(FP, ID, 0, Some(ALAB));
    assert_eq!(
        (none.reads_at(), none.take().steps[0].state),
        (StepState::Partial, StepState::Partial),
        "a writer its reader names no build of is no measured one"
    );
    let mut named = CodexStepFold::new(0);
    named.set_writer(Some(ALAB));
    named.bind_build(FP, ID, 0, Some(ALAB));
    assert_eq!(named.reads_at(), StepState::On, "NEGATIVE CONTROL");
}

/// The newest measured build: its legs carry gaps 20 and 25's rows (`item_completed`'s `exit_code`
/// under its call's id, `task_started`'s `turn_trigger`).
const L13766: &str = "0.166.0-dev.13766+gcd79a420c";

/// The committed command's completion row in a leg's text, `"exit_code":0` under `call_r0010`.
const EXITED: &str = "\"exit_code\":0";

/// `text` with its one command's `exit_code` planted to `code`.
fn exit_planted(text: &str, code: i64) -> String {
    assert_eq!(text.matches(EXITED).count(), 1, "the leg's one completion");
    text.replacen(EXITED, &format!("\"exit_code\":{code}"), 1)
}

/// `saids` with `result <id>` read as `result <id> error`.
fn failing(said: &[String], id: &str) -> Vec<String> {
    said.iter()
        .map(|s| {
            if *s == format!("result {id}") {
                format!("{s} error")
            } else {
                s.clone()
            }
        })
        .collect()
}

/// [`fold`] by the lab reducer's fold, whose gated reads read on every build.
fn fold_ungated(text: &str) -> StepBatch {
    let mut fold = CodexStepFold::ungated(0);
    fold.bind(FP, ID, 0);
    let mut end = 0u64;
    for row in rows(text) {
        end += row.len() as u64 + 1;
        fold.push(row.as_bytes(), end);
    }
    fold.take()
}

/// GAP 20: a command the build's completion row says exited non-zero is a failed step at its
/// output. NEGATIVE CONTROL: the committed exit 0 reads ok (a read of the key alone would fail it).
#[test]
fn a_failed_command_fails_its_step() {
    let approval = alab_leg_of(L13766, "approval");
    let plain = saids(&fold(&approval));
    assert!(
        plain.contains(&"result call_r0010".to_string()),
        "the committed exit 0 reads ok: {plain:?}"
    );
    let failed = exit_planted(&approval, 3);
    let batch = fold(&failed);
    assert_eq!(saids(&batch), failing(&plain, "call_r0010"));
    assert!(batch.steps[1..].iter().all(|s| s.state == StepState::On));
    assert_eq!((batch.lost, batch.overflow), (0, 0));
}

/// A declined command's completion says `-1` (codex-rs/core/src/tools/events.rs): a failed step.
#[test]
fn a_declined_command_fails_its_step() {
    let approval = alab_leg_of(L13766, "approval");
    let plain = saids(&fold(&approval));
    assert_eq!(
        saids(&fold(&exit_planted(&approval, -1))),
        failing(&plain, "call_r0010")
    );
}

/// The exit read is gated: a build nobody measured, a writer nobody measured, rows read short of
/// `On`, and a writer whose reductions show no `exit_code` each read the failed command `ok`.
#[test]
fn an_unmeasured_builds_failed_command_reads_ok() {
    let failed = exit_planted(&alab_leg_of(L13766, "approval"), 3);
    let unmeasured = failed.replace(
        "\"cli_version\":\"0.166.0-dev.13766+gcd79a420c\"",
        "\"cli_version\":\"0.166.0-dev.13767+g0000000\"",
    );
    assert_ne!(unmeasured, failed, "the plant took");
    let batch = fold(&unmeasured);
    let said = saids(&batch);
    assert!(said.contains(&"result call_r0010".to_string()), "{said:?}");
    assert!(batch.steps.iter().all(|s| s.state == StepState::Partial));
    let by = |prepare: &dyn Fn(&mut CodexStepFold, usize)| {
        let mut fold = CodexStepFold::new(0);
        fold.bind(FP, ID, 0);
        let mut end = 0u64;
        for (i, row) in rows(&failed).enumerate() {
            prepare(&mut fold, i);
            end += row.len() as u64 + 1;
            fold.push(row.as_bytes(), end);
        }
        saids(&fold.take())
    };
    let none = |_: &mut CodexStepFold, _: usize| {};
    assert!(
        by(&none).contains(&"result call_r0010 error".to_string()),
        "NEGATIVE CONTROL: measured, the plant fails its step"
    );
    let writer = |f: &mut CodexStepFold, i: usize| {
        if i == 0 {
            f.set_writer(Some("0.166.0-dev.13767+g0000000"));
        }
    };
    assert!(by(&writer).contains(&"result call_r0010".to_string()));
    let short_of_on = |f: &mut CodexStepFold, i: usize| {
        if i == 1 {
            assert_eq!(
                f.shown,
                Gate::of(Some(L13766)),
                "the meta row named the build"
            );
            f.state = StepState::Partial;
        }
    };
    assert!(
        by(&short_of_on).contains(&"result call_r0010".to_string()),
        "a build's gate opens only while its rows read On"
    );
    let unshown = |f: &mut CodexStepFold, i: usize| {
        if i == 0 {
            f.set_writer(Some(L13766));
            assert_eq!(
                f.reads_at(),
                StepState::Partial,
                "no rollout bound a build yet"
            );
            f.writer_shown = Gate::NONE;
        }
    };
    assert!(
        by(&unshown).contains(&"result call_r0010".to_string()),
        "a writer whose reductions show no exit_code reads none"
    );
}

/// A completion read after its call's output (a long-running exec's,
/// codex-rs/core/src/unified_exec/async_watcher.rs) changes nothing: the step is the call.
#[test]
fn a_completion_after_its_output_changes_nothing() {
    let approval = alab_leg_of(L13766, "approval");
    let plain = saids(&fold(&approval));
    let failed = exit_planted(&approval, 3);
    let mut lines: Vec<&str> = failed.lines().collect();
    let done = lines
        .iter()
        .position(|l| l.contains("\"exit_code\":3"))
        .expect("the completion");
    let output = lines
        .iter()
        .position(|l| l.contains("\"function_call_output\"") && l.contains("call_r0010"))
        .expect("its output");
    assert_eq!(
        done + 1,
        output,
        "the completion is read just before its output"
    );
    lines.swap(done, output);
    let late: String = lines.iter().map(|l| format!("{l}\n")).collect();
    assert_eq!(saids(&fold(&late)), plain);
    assert_eq!(saids(&fold_ungated(&late)), plain);
}

/// A completion pairs with its own call by id, never with the newest open one.
#[test]
fn a_completion_of_another_id_changes_nothing() {
    let approval = alab_leg_of(L13766, "approval");
    let plain = saids(&fold(&approval));
    let other: String = exit_planted(&approval, 3)
        .lines()
        .map(|l| {
            if l.contains("\"exit_code\":3") {
                l.replacen("\"id\":\"call_r0010\"", "\"id\":\"call_r9999\"", 1)
            } else {
                l.to_string()
            }
        })
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(other.contains("call_r9999"), "the plant took");
    assert_eq!(saids(&fold(&other)), plain);
}

/// The 13766 continue leg's continuation (its second turn), its `turn_trigger` planted to
/// `word`.
fn trigger_planted(word: &str) -> String {
    let text = alab_leg_of(L13766, "continue");
    let mut planted = 0;
    let out: String = text
        .lines()
        .map(|l| {
            if l.contains("\"task_started\"") && l.contains("8000-00000000000b\"") {
                planted += 1;
                l.replacen(
                    "\"turn_trigger\":\"\"",
                    &format!("\"turn_trigger\":\"{word}\""),
                    1,
                )
            } else {
                l.to_string()
            }
        })
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(planted, 1, "the continuation's one task_started");
    out
}

const CONTINUED: &str = "00000000-0000-4000-8000-00000000000b";

/// GAP 25: a turn codex-alab's Autopilot began says so in its `task_started`: a `harness` step
/// right after its prompt, where the build's reductions showed the word. 13766's show none (the
/// reducer blanked every trigger while nothing read one), so its gated fold reads the planted
/// rows exactly as the committed ones.
#[test]
fn an_autopilot_trigger_begins_a_harness_turn() {
    let committed = alab_leg_of(L13766, "continue");
    let plain = saids(&fold(&committed));
    let planted = trigger_planted(AUTOPILOT_TRIGGER);
    let read = saids(&fold_ungated(&planted));
    let at = plain
        .iter()
        .position(|s| *s == format!("prompt {CONTINUED}"))
        .expect("the continuation's prompt");
    let mut want = plain.clone();
    want.insert(at + 1, format!("harness {CONTINUED}"));
    assert_eq!(read, want);
    assert_eq!(saids(&fold_ungated(&committed)), plain);
    assert!(!Gate::of(Some(L13766)).autopilot);
    assert_eq!(
        saids(&fold(&planted)),
        plain,
        "13766 showed no alab_autopilot"
    );
    let batch = fold_ungated(&planted);
    let (prompt, harness) = (&batch.steps[at], &batch.steps[at + 1]);
    assert_eq!(
        prompt.pos.off + 1,
        harness.pos.off,
        "one row, the second after the first"
    );
}

/// A `task_started` row of turn `id` whose attribution is `attribution` (`None`: no
/// `turn_attribution` key).
fn started(ms: i64, id: &str, attribution: Option<&str>) -> String {
    let attribution =
        attribution.map_or_else(String::new, |a| format!(",\"turn_attribution\":{a}"));
    format!(
        "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\
         \"turn_id\":\"{id}\"{attribution},\"model_context_window\":258400}}}}\n",
        stamp(ms)
    )
}

fn trigger(word: &str) -> String {
    format!("{{\"turn_id\":\"turn-1\",\"turn_trigger\":\"{word}\",\"parent_turn_id\":null}}")
}

/// One synthetic turn begun by a `task_started` attributed `attribution`.
fn one_turn(attribution: Option<&str>) -> String {
    [
        opening(),
        started(T0 + 1, "turn-1", attribution),
        ctx(T0 + 2, "turn-1", "gpt-6.1-sol"),
        done(T0 + 3, "turn-1", 5, ""),
    ]
    .concat()
}

fn harnessed(batch: &StepBatch) -> usize {
    batch
        .steps
        .iter()
        .filter(|s| matches!(s.ev, StepEvent::HarnessTurn { .. }))
        .count()
}

/// A turn whose row names no trigger, a `null` one or a blanked one is nobody's: the fold reads
/// the trigger as saying nothing (codex_facts' survey rule, absent = a person's, is not this).
#[test]
fn a_turn_naming_no_trigger_is_nobodys() {
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
        assert_eq!(harnessed(&fold_ungated(&text)), 0, "{name}");
        read += 1;
    }
    assert!(read > 40, "every committed codex reduction ({read})");
    for attribution in [
        None,
        Some("{\"turn_id\":\"turn-1\",\"turn_trigger\":null}"),
        Some("{\"turn_id\":\"turn-1\"}"),
        Some("null"),
        Some("\"alab_autopilot\""),
        Some(&*trigger("")),
    ] {
        let batch = fold_ungated(&one_turn(attribution));
        assert_eq!(harnessed(&batch), 0, "{attribution:?}");
        assert_eq!(
            saids(&batch)[1..],
            ["prompt turn-1", "model gpt-6.1-sol - 0.163.0", "end 5 0 0"],
            "{attribution:?}"
        );
    }
    let control = fold_ungated(&one_turn(Some(&trigger(AUTOPILOT_TRIGGER))));
    assert_eq!(harnessed(&control), 1, "NEGATIVE CONTROL");
}

/// Only codex-alab's Autopilot's own word begins a harness turn: every other trigger codex
/// writes (a person's at the TUI, a queued message, `codex exec`, realtime, a goal, a daemon's
/// recovery, a memory consolidation, a guardian review) says nothing.
#[test]
fn only_the_autopilots_word_begins_a_harness_turn() {
    for word in [
        "user",
        "queue",
        "exec",
        "realtime",
        "safety_recourse_follow_up",
        "goal",
        "daemon_recovery",
        "memory_consolidation",
        "guardian_review",
        "alab_autopilot_v2",
        "ALAB_AUTOPILOT",
    ] {
        let batch = fold_ungated(&one_turn(Some(&trigger(word))));
        assert_eq!(harnessed(&batch), 0, "{word}");
    }
    let batch = fold_ungated(&one_turn(Some(&trigger(AUTOPILOT_TRIGGER))));
    assert_eq!(
        saids(&batch)[1..],
        [
            "prompt turn-1",
            "harness turn-1",
            "model gpt-6.1-sol - 0.163.0",
            "end 5 0 0"
        ]
    );
}

/// A trigger read after its turn's `turn_context` still names it, at its own row, after the
/// prompt; one read after its turn ended names nothing.
#[test]
fn a_trigger_read_after_its_prompt_follows_it() {
    let row = started(T0 + 3, "turn-1", Some(&trigger(AUTOPILOT_TRIGGER)));
    let text = [
        opening(),
        ctx(T0 + 2, "turn-1", "gpt-6.1-sol"),
        row.clone(),
        done(T0 + 4, "turn-1", 5, ""),
    ]
    .concat();
    let batch = fold_ungated(&text);
    assert_eq!(
        saids(&batch)[1..],
        [
            "prompt turn-1",
            "model gpt-6.1-sol - 0.163.0",
            "harness turn-1",
            "end 5 0 0"
        ]
    );
    let at = (opening().len() + ctx(T0 + 2, "turn-1", "gpt-6.1-sol").len() + row.len()) as u64;
    assert_eq!(batch.steps[3].pos.off, at, "at the task_started row");
    let ended = [
        opening(),
        ctx(T0 + 2, "turn-1", "gpt-6.1-sol"),
        done(T0 + 3, "turn-1", 5, ""),
        started(T0 + 4, "turn-1", Some(&trigger(AUTOPILOT_TRIGGER))),
        ctx(T0 + 5, "turn-2", "gpt-6.1-sol"),
        done(T0 + 6, "turn-2", 5, ""),
    ]
    .concat();
    let batch = fold_ungated(&ended);
    assert_eq!(harnessed(&batch), 0, "{:?}", saids(&batch));
}

/// What a reduction's rows show, recounted apart from `codex_shown::words`'s byte scanner: each
/// row parsed, every object walked; its snake-case keys, and `key=value` where the value is a
/// string closed word (snake case, no digit).
fn recount(texts: &[&str]) -> std::collections::BTreeSet<String> {
    fn snake(s: &str, digits: bool) -> bool {
        s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || (digits && c.is_ascii_digit()))
    }
    fn walk(v: &Value, out: &mut std::collections::BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    if snake(k, true) {
                        out.insert(k.clone());
                        if let Some(w) = v.as_str().filter(|w| snake(w, false)) {
                            out.insert(format!("{k}={w}"));
                        }
                    }
                    walk(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = std::collections::BTreeSet::new();
    for text in texts {
        for row in rows(text) {
            let v: Value = aterm_json::from_str(row).unwrap_or_else(|e| panic!("{e}: {row}"));
            walk(&v, &mut out);
        }
    }
    out
}

/// [`recount`] of the committed reductions of `build`, as `codex_shown::words` says them.
fn recounted(build: &str) -> String {
    let legs = committed_of(build);
    let texts: Vec<&str> = legs.iter().map(|(_, t)| t.as_str()).collect();
    let mut said = String::from("\n");
    for t in recount(&texts) {
        said.push_str(&t);
        said.push('\n');
    }
    said
}

/// CODEX_SHOWN IS WHAT EACH MEASURED BUILD'S REDUCTIONS HOLD: one entry per measured build, each
/// what its committed reductions show by an independent recount; every one shows the exit key
/// and the shell's name; no upstream build is named; and the scanner the lab reads the checkout
/// with says the same.
#[test]
fn codex_shown_is_what_each_measured_builds_reductions_hold() {
    let builds: Vec<&str> = CODEX_SHOWN.iter().map(|(b, _)| *b).collect();
    assert_eq!(
        builds, CODEX_MEASURED_BUILDS,
        "one entry per measured build, sorted"
    );
    assert!(!builds.contains(&"0.163.0"), "no upstream build");
    for (build, words) in CODEX_SHOWN {
        assert_eq!(*words, recounted(build), "{build}");
        let shown = |t: &str| words.contains(&format!("\n{t}\n"));
        assert!(shown("exit_code") && shown("name=exec_command"), "{build}");
        assert!(
            !shown("call_id=call_r0010"),
            "{build}: an id is no closed word"
        );
        assert_eq!(
            super::super::codex_shown::shown_in(std::path::Path::new(STEPS_DIR), build),
            *words,
            "{build}"
        );
        assert_eq!(Gate::of(Some(build)), Gate::from_words(words));
        assert!(Gate::of(Some(build)).exit, "{build}");
    }
    assert_eq!(Gate::of(Some("0.163.0")), Gate::NONE);
    assert_eq!(Gate::of(None), Gate::NONE);
    let upstream = super::super::codex_shown::shown_in(std::path::Path::new(STEPS_DIR), "0.163.0");
    assert_eq!(
        upstream, "\n",
        "codex_shown reads codex-alab reductions only"
    );
    let probe = super::super::codex_shown::words(&[
        "# codex 9.9.9 · lab reduce",
        "{\"type\":\"event_msg\",\"payload\":{\"turn_attribution\":{\"turn_trigger\":\"alab_autopilot\"},\"exit_code\":3,\"id\":\"call_r1\",\"s\":\"a\\\"b:\\\"c\"}}",
    ]);
    assert_eq!(
        probe,
        "\nexit_code\nid\npayload\ns\nturn_attribution\nturn_trigger\nturn_trigger=alab_autopilot\ntype\ntype=event_msg\n"
    );
    assert_eq!(
        Gate::from_words(&probe),
        Gate {
            exit: true,
            autopilot: true
        }
    );
}
