// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The step fold's prompt classifier against this module's own:
//! the marker prefixes are one set of consts both read,
//! and on every shape the step fold leaves out — a task notification, a
//! shell-mode command and its output, Esc's row, a meta row, a local
//! command — `asked_by` names no person's prompt either. NEGATIVE
//! CONTROLS: a person's prompt, and a peer's message (meta, or led by a
//! marker), are someone's to both. And a turn Claude Code begins of its
//! own (a `/loop`'s wake, a missed task's notice) is one to both: the fold
//! starts its vendor turn where `FinishedScan` numbers it anew, each over
//! its own projection of the row. Included from `upgrade.rs`, so
//! `asked_by`, `FinishedScan` and their answers are in scope unchanged.

use super::*;
use crate::harness::steps::{StepEvent, StepFold};

fn row(content: &str, extra: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"promptId":"v1","type":"user",{extra}"message":{{"role":"user","content":"{content}"}},"uuid":"u-1","timestamp":"2026-10-15T20:00:00.000Z"}}"#
    )
}

fn folds_to_a_prompt(line: &str) -> bool {
    let mut fold = StepFold::new(0, None);
    fold.read_at(1, None, 0, false);
    fold.push(line.as_bytes(), line.len() as u64 + 1);
    fold.take()
        .steps
        .iter()
        .any(|s| matches!(s.ev, StepEvent::Prompt { .. }))
}

fn asked(line: &str) -> Asked {
    let v = crate::harness::transcript::conversation(line).expect("a row");
    asked_by(&v, &[])
}

#[test]
fn the_step_folds_left_out_shapes_are_no_ones_prompt_here_either() {
    let left_out = [
        row(&format!("{TASK_NOTIFICATION}<task-id>b1</task-id>"), ""),
        row(&format!("{BASH_INPUT}ls</bash-input>"), ""),
        row(&format!("{BASH_STDOUT}x</bash-stdout>"), ""),
        row(&format!("{BASH_STDERR}boom</bash-stderr>"), ""),
        row(&format!("{INTERRUPTED}]"), ""),
        row("a caveat", r#""isMeta":true,"#),
        row("<command-name>/model</command-name>", ""),
        row("<local-command-stdout>Set model</local-command-stdout>", ""),
        row("woken", r#""origin":{"kind":"task-notification"},"#),
    ];
    for line in &left_out {
        assert!(!folds_to_a_prompt(line), "the fold: {line}");
        assert!(
            !matches!(asked(line), Asked::Theirs),
            "upgrade reads a person's prompt in {line}"
        );
    }
    // A person's prompt, and a peer's message (which Claude Code records
    // `isMeta`; or led by a marker), are someone's to both.
    for theirs in [
        row("list the files", ""),
        row("from a peer", r#""isMeta":true,"origin":{"kind":"peer"},"#),
        row(
            &format!("{TASK_NOTIFICATION}relayed by a peer"),
            r#""origin":{"kind":"peer"},"#,
        ),
    ] {
        assert!(folds_to_a_prompt(&theirs), "the fold: {theirs}");
        assert!(matches!(asked(&theirs), Asked::Theirs), "upgrade: {theirs}");
    }
}

/// A TURN CLAUDE CODE BEGINS OF ITS OWN IS ONE TO BOTH (`numbered_anew`, read
/// through each side's projection of the row): after a person's prompt
/// under `promptId` `v1` and its closed turn, every `claude/begins/` corpus
/// row moves `FinishedScan`'s running turn off `v1` and, once a model's row
/// says the turn ran, the step fold queues its `Prompt`. NEGATIVE CONTROLS:
/// every `claude/goes-on/` row (a Stop hook's feedback, `/goal`'s check-in)
/// leaves the running turn `v1` to both, and the fold queues no `Prompt`.
#[test]
fn a_turn_claude_code_begins_of_its_own_is_one_to_both() {
    use crate::harness::transcript::corpus_tests::TURN_ROWS;
    let typed = row("go", "");
    let reply = |t: &str| {
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"id":"m-{t}","model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"ok"}}]}},"uuid":"a-{t}","timestamp":"2026-10-15T20:{t}.000Z"}}"#
        )
    };
    let ended = r#"{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":900,"timestamp":"2026-10-15T20:00:02.000Z","uuid":"d-1"}"#;
    let (mut begins, mut goes_on) = (0, 0);
    for (name, line) in TURN_ROWS {
        let anew = name.starts_with("claude/begins/");
        if anew {
            begins += 1;
        } else {
            goes_on += 1;
        }
        let mut scan = FinishedScan::default();
        scan.line(&typed);
        assert_eq!(scan.turn.as_deref(), Some("v1"));
        scan.line(line);
        assert_eq!(scan.turn.as_deref() != Some("v1"), anew, "upgrade: {name}");
        let mut fold = StepFold::new(0, None);
        fold.read_at(1, None, 0, false);
        let mut off = 0;
        let mut feed = |fold: &mut StepFold, l: &str| {
            off += l.len() as u64 + 1;
            fold.push(l.as_bytes(), off);
        };
        for l in [typed.as_str(), &reply("00:01"), ended] {
            feed(&mut fold, l);
        }
        let _ = fold.take();
        feed(&mut fold, line);
        feed(&mut fold, &reply("40:01"));
        let prompted = fold
            .take()
            .steps
            .iter()
            .any(|s| matches!(s.ev, StepEvent::Prompt { .. }));
        assert_eq!(prompted, anew, "the fold: {name}");
    }
    assert!(begins >= 4 && goes_on >= 3, "{begins} {goes_on}");
}
