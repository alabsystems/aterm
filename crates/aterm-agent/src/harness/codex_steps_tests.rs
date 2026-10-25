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
fn rollout_says(text: &str, release: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut opened: Vec<String> = Vec::new();
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
                }
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
    let own = rollout_says(text, release.as_str())?;
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
    assert_eq!(
        kinds.into_iter().collect::<Vec<_>>(),
        ["call", "end", "model", "prompt", "result", "wall"]
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
        rollout_says(&off, "0.164.0-dev.12521").expect("read"),
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
        rollout_says(&moved, "0.164.0-dev.12521").expect("read"),
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

#[test]
fn the_writer_holds_the_rows_to_its_build() {
    let text = alab_leg("continue");
    let second = rows(&text)
        .position(|r| r.contains("\"turn_context\"") && r.contains("8000-00000000000b"))
        .expect("the continued turn's context");
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
