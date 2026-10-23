// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`] step folding, wall judgment, cursor tap, and S6 text-leak guards.

use super::*;
use crate::supervise::run::wall_confirm::{confirms, transcript_ends_on, transcript_wall};

const FP: u64 = 0x5eed_0000_0000_0001;
const ID: Option<(u64, u64)> = Some((16_777_220, 77));
/// 2026-10-15T20:00:00Z, wall ms.
const T0: u64 = 1_792_094_400_000;
/// A session id's fingerprint ([`StepFold::note_vacant`]).
const SID: u64 = 0x5e55_0000_0000_0001;

fn at(ms: u64) -> String {
    let mut s = aterm_types::rfc3339::format_rfc3339((T0 + ms) / 1000);
    s.pop();
    format!("{s}.{:03}Z", (T0 + ms) % 1000)
}

fn prompt(id: &str, ms: u64, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"promptId":"{id}","type":"user","message":{{"role":"user","content":"{text}"}},"uuid":"u-{id}","timestamp":"{}","version":"2.1.300"}}"#,
        at(ms)
    )
}

fn calls(msg: &str, ms: u64, tools: &[(&str, &str)]) -> String {
    let parts: Vec<String> = tools
        .iter()
        .map(|(id, name)| {
            format!(
                r#"{{"type":"tool_use","id":"{id}","name":"{name}","input":{{"command":"SENTINEL-INPUT"}}}}"#
            )
        })
        .collect();
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"id":"{msg}","model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"SENTINEL-REPLY"}},{}],"usage":{{"input_tokens":3,"output_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}},"uuid":"a-{msg}","timestamp":"{}","effort":"high","version":"2.1.300"}}"#,
        parts.join(","),
        at(ms)
    )
}

fn reply(msg: &str, ms: u64, output: u64) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"id":"{msg}","model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"SENTINEL-REPLY"}}],"usage":{{"input_tokens":3,"output_tokens":{output},"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}},"uuid":"a-{msg}-{output}","timestamp":"{}","effort":"high"}}"#,
        at(ms)
    )
}

fn results(ms: u64, ids: &[(&str, bool)]) -> String {
    let parts: Vec<String> = ids
        .iter()
        .map(|(id, error)| {
            format!(
                r#"{{"type":"tool_result","tool_use_id":"{id}","is_error":{error},"content":[{{"type":"text","text":"SENTINEL-RESULT"}}]}}"#
            )
        })
        .collect();
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":[{}]}},"uuid":"r-{ms}","timestamp":"{}","toolUseResult":{{"stdout":"SENTINEL-RESULT"}}}}"#,
        parts.join(","),
        at(ms)
    )
}

fn turn_end(ms: u64, dur: u64) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":{dur},"timestamp":"{}","uuid":"d-{ms}","isMeta":false}}"#,
        at(ms)
    )
}

fn notice(ms: u64, text: &str, error: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"id":"n-{ms}","model":"<synthetic>","role":"assistant","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":0,"output_tokens":0}}}},"isApiErrorMessage":true,"error":"{error}","uuid":"n-{ms}","timestamp":"{}"}}"#,
        at(ms)
    )
}

/// A fold bound to an empty file (everything after is fresh) at floor 0,
struct Fed {
    fold: StepFold,
    off: u64,
    ends: Vec<u64>,
}

impl Fed {
    fn new() -> Fed {
        let mut fold = StepFold::new(0, None);
        fold.read_at(FP, ID, 0, false);
        Fed {
            fold,
            off: 0,
            ends: Vec::new(),
        }
    }

    fn push(&mut self, row: &str) -> &mut Fed {
        self.off += row.len() as u64 + 1;
        self.ends.push(self.off);
        self.fold.push(row.as_bytes(), self.off);
        self
    }

    fn events(&mut self) -> Vec<StepEvent> {
        self.fold
            .take()
            .steps
            .into_iter()
            .map(|s| s.ev)
            .filter(|e| !matches!(e, StepEvent::Bind { .. }))
            .collect()
    }
}

fn id(s: &str) -> StepId {
    StepId::new(s).expect("an id")
}

/// Plan §2.4: a turn's rows fold to its steps in order — the prompt, the
#[test]
fn a_turn_folds_to_its_steps_in_order() {
    let mut f = Fed::new();
    f.push(&prompt("v1", 0, "list the files"))
        .push(&calls(
            "m1",
            100,
            &[("toolu_a", "Bash"), ("toolu_b", "Task")],
        ))
        .push(&results(200, &[("toolu_a", false), ("toolu_b", true)]))
        .push(&reply("m2", 300, 40))
        .push(&turn_end(400, 4_000));
    let batch = f.fold.take();
    let evs: Vec<StepEvent> = batch.steps.iter().map(|s| s.ev).collect();
    let model = StepEvent::Model {
        model: id("claude-opus-5-5"),
        effort: Some(id("high")),
        ver: Some(id("2.1.300")),
    };
    assert_eq!(
        evs,
        vec![
            StepEvent::Bind { len: 0 },
            StepEvent::Prompt { vref: id("v1") },
            model,
            StepEvent::ToolUse {
                id: id("toolu_a"),
                tool: ToolName::new("Bash"),
                cls: "shell",
                subagent: false
            },
            StepEvent::ToolUse {
                id: id("toolu_b"),
                tool: ToolName::new("Task"),
                cls: "agent",
                subagent: true
            },
            StepEvent::ToolResult {
                id: id("toolu_a"),
                error: false
            },
            StepEvent::ToolResult {
                id: id("toolu_b"),
                error: true
            },
            StepEvent::TurnEnd {
                vdur_ms: 4_000,
                tok_in: 2 * (3 + 100 + 1000),
                tok_out: 20 + 40
            },
        ]
    );
    let offs: Vec<u64> = batch.steps.iter().skip(1).map(|s| s.pos.off).collect();
    assert!(offs.windows(2).all(|w| w[0] < w[1]), "{offs:?}");
    assert_eq!(batch.steps[4].pos.off, f.ends[1]);
    assert!(batch.steps[2].pos.off > f.ends[0]);
    assert_eq!(
        batch.steps[1].pos,
        StepPos {
            fp: FP,
            id: ID,
            off: f.ends[0],
            at_ms: T0
        }
    );
    assert!(batch.steps.iter().all(|s| s.state == StepState::Partial));
}

/// A11: a turn's tokens count each message id once, at its largest (a
#[test]
fn a_turns_tokens_count_each_message_once_at_its_largest() {
    let mut f = Fed::new();
    f.push(&reply("m0", 0, 999))
        .push(&prompt("v1", 10, "go"))
        .push(&reply("m1", 20, 10))
        .push(&reply("m1", 30, 10))
        .push(&reply("m1", 40, 25))
        .push(&turn_end(50, 40));
    let end = f
        .events()
        .into_iter()
        .find(|e| matches!(e, StepEvent::TurnEnd { .. }));
    assert_eq!(
        end,
        Some(StepEvent::TurnEnd {
            vdur_ms: 40,
            tok_in: 1103,
            tok_out: 25
        })
    );
    let no_id = reply("x", 60, 7).replace(r#""id":"x","#, "");
    let mut g = Fed::new();
    g.push(&prompt("v2", 0, "go"))
        .push(&no_id)
        .push(&no_id)
        .push(&turn_end(70, 1));
    let end = g
        .events()
        .into_iter()
        .find(|e| matches!(e, StepEvent::TurnEnd { .. }));
    assert!(
        matches!(end, Some(StepEvent::TurnEnd { tok_out: 14, .. })),
        "{end:?}"
    );
}

/// S3, A10: the user rows that are no prompt — a task notification (by its
#[test]
fn the_user_rows_that_ask_nothing_emit_no_prompt() {
    let quiet = [
        prompt("a", 0, "<task-notification><task-id>b1</task-id>"),
        prompt("b", 0, "<bash-input>ls</bash-input>"),
        prompt(
            "c",
            0,
            "<bash-stdout>x</bash-stdout><bash-stderr></bash-stderr>",
        ),
        prompt("d", 0, "<bash-stderr>boom</bash-stderr>"),
        prompt("e", 0, "<command-name>/model</command-name>"),
        prompt(
            "f",
            0,
            "<local-command-stdout>Set model</local-command-stdout>",
        ),
        prompt("g", 0, "/compact"),
        prompt("h", 0, "\\n  <command-message>doctor</command-message>"),
        prompt("i", 0, "anything").replace(r#""type":"user","#, r#""type":"user","isMeta":true,"#),
        prompt("j", 0, "summary").replace(
            r#""type":"user","#,
            r#""type":"user","isCompactSummary":true,"#,
        ),
        prompt("j2", 0, "orphaned workflow notification").replace(
            r#""type":"user","#,
            r#""type":"user","queueTranscriptOnly":true,"#,
        ),
        prompt("k", 0, "a wake").replace(
            r#""type":"user","#,
            r#""type":"user","origin":{"kind":"task-notification"},"#,
        ),
        prompt("l", 0, "\\u003cbash-input>ls</bash-input>"),
        prompt("m", 0, "a subagent's ask")
            .replace(r#""isSidechain":false"#, r#""isSidechain":true"#),
    ];
    for row in &quiet {
        let mut f = Fed::new();
        assert!(f.push(row).events().is_empty(), "{row}");
    }
    let mut f = Fed::new();
    assert_eq!(
        f.push(&prompt(
            "z",
            0,
            "[Request interrupted by user for tool use]"
        ))
        .events(),
        vec![StepEvent::Interrupt]
    );
    for (row, vref) in [
        (prompt("v1", 0, "run <bash-input> for me"), "v1"),
        (prompt("v2", 0, "\\u0020 compact the notes"), "v2"),
        (
            prompt("v3", 0, "x").replace(
                r#""content":"x""#,
                r#""content":[{"type":"image","source":{"type":"base64","data":"AAAA"}},{"type":"text","text":"look at this"}]"#,
            ),
            "v3",
        ),
    ] {
        let mut f = Fed::new();
        assert_eq!(f.push(&row).events(), vec![StepEvent::Prompt { vref: id(vref) }], "{row}");
    }
    let peer = prompt("v5", 0, "from a peer").replace(
        r#""type":"user","#,
        r#""type":"user","isMeta":true,"origin":{"kind":"peer"},"#,
    );
    let mut f = Fed::new();
    assert_eq!(
        f.push(&peer).events(),
        vec![StepEvent::Prompt { vref: id("v5") }]
    );
    assert_eq!(f.fold.end().last, EndClass::Nothing);
    let part = prompt("v4", 0, "x").replace(
        r#""content":"x""#,
        r#""content":[{"type":"text","text":"<task-notification>done"}]"#,
    );
    let mut f = Fed::new();
    assert!(f.push(&part).events().is_empty());
}

/// A TURN WITH NO PROMPT ROW (S3 as `upgrade.rs`'s `TaskScan` reads it):
#[test]
fn a_prompt_command_or_a_wake_starts_the_turn_its_reply_says_ran() {
    let command = prompt(
        "p-5",
        0,
        "<command-message>init is analyzing</command-message>\\n<command-name>/init</command-name>",
    );
    let expansion = prompt("p-5", 10, "Please analyze this codebase")
        .replace(r#""type":"user","#, r#""type":"user","isMeta":true,"#);
    let turn = [
        calls("m1", 1_000, &[("toolu_a", "Read")]),
        results(1_100, &[("toolu_a", false)]),
        reply("m2", 1_200, 5),
        turn_end(1_300, 1_300),
    ];
    let mut typed = Fed::new();
    typed.push(&prompt("p-5", 0, "analyze this codebase"));
    for r in &turn {
        typed.push(r);
    }
    let typed = typed.events();
    let mut f = Fed::new();
    f.push(&command).push(&expansion);
    for r in &turn {
        f.push(r);
    }
    let steps: Vec<Step> = f
        .fold
        .take()
        .steps
        .into_iter()
        .filter(|s| !matches!(s.ev, StepEvent::Bind { .. }))
        .collect();
    assert_eq!(steps.iter().map(|s| s.ev).collect::<Vec<_>>(), typed);
    assert_eq!(steps[0].ev, StepEvent::Prompt { vref: id("p-5") });
    assert_eq!(steps[0].pos.at_ms, T0, "dated at the command row");
    assert!(steps[0].pos.off > f.ends[1] && steps[0].pos.off < steps[1].pos.off);
    for wake in [
        prompt("w-1", 5_000, "<task-notification><task-id>b1</task-id>"),
        prompt("w-1", 5_000, "a wake").replace(
            r#""type":"user","#,
            r#""type":"user","origin":{"kind":"task-notification"},"#,
        ),
    ] {
        let mut f = Fed::new();
        f.push(&prompt("v1", 0, "go"))
            .push(&reply("m1", 100, 5))
            .push(&turn_end(200, 200));
        let _ = f.events();
        f.push(&wake)
            .push(&calls("m3", 6_000, &[("toolu_b", "Bash")]));
        assert_eq!(
            f.events(),
            vec![
                StepEvent::Prompt { vref: id("w-1") },
                StepEvent::ToolUse {
                    id: id("toolu_b"),
                    tool: ToolName::new("Bash"),
                    cls: tool_class("Bash").0,
                    subagent: false,
                },
            ],
            "{wake}"
        );
    }
    let mut f = Fed::new();
    f.push(&prompt("s-1", 0, "<bash-input>cat notes.txt</bash-input>"))
        .push(&prompt(
            "s-1",
            20,
            "<bash-stdout>Not logged in</bash-stdout><bash-stderr></bash-stderr>",
        ))
        .push(&reply("m1", 40, 1))
        .push(&turn_end(60, 60));
    let evs = no_model(f.events());
    assert_eq!(evs.first(), Some(&StepEvent::Prompt { vref: id("s-1") }));
    assert!(matches!(evs.last(), Some(StepEvent::TurnEnd { .. })));
    let mut f = Fed::new();
    f.push(&prompt("s-1", 0, "<bash-input>ls</bash-input>"))
        .push(&prompt("s-1", 20, "<bash-stdout>a</bash-stdout>"))
        .push(&prompt("v2", 1_000, "go"))
        .push(&reply("m1", 1_100, 1));
    assert_eq!(
        no_model(f.events()),
        vec![StepEvent::Prompt { vref: id("v2") }]
    );
    let mut f = Fed::new();
    f.push(&prompt("c-1", 0, "<command-name>/model</command-name>"))
        .push(&prompt(
            "c-1",
            5,
            "<local-command-stdout>Set model</local-command-stdout>",
        ))
        .push(&reply("m1", 1_000, 5));
    assert!(
        !f.events()
            .iter()
            .any(|e| matches!(e, StepEvent::Prompt { .. })),
        "a local command runs no turn"
    );
    let mut f = Fed::new();
    f.push(&command)
        .push(&notice(1_000, "API Error", "unknown"));
    assert!(f.events().is_empty());
    let mut f = Fed::new();
    f.push(&prompt("v1", 0, "go"))
        .push(&calls("m1", 100, &[("toolu_a", "Bash")]))
        .push(&prompt(
            "w-1",
            150,
            "<task-notification><task-id>b1</task-id>",
        ))
        .push(&reply("m2", 200, 5));
    let prompts = f
        .events()
        .into_iter()
        .filter(|e| matches!(e, StepEvent::Prompt { .. }))
        .count();
    assert_eq!(prompts, 1, "the wake joined the running turn");
}

/// A8: a row whose words are spelt with `\u` escapes is read as the words
#[test]
fn an_escaped_key_is_read_as_its_word() {
    let ask = prompt("v1", 0, "go").replace(r#""user""#, r#""\u0075ser""#);
    let call = calls("m1", 5, &[("toolu_a", "Bash")])
        .replace(r#""type":"tool_use""#, r#""type":"tool_\u0075se""#);
    let result = results(10, &[("toolu_a", false)]).replace("tool_result", "tool_\\u0072esult");
    let mut f = Fed::new();
    let evs = f.push(&ask).push(&call).push(&result).events();
    assert_eq!(
        evs.iter()
            .filter(|e| !matches!(e, StepEvent::Model { .. }))
            .copied()
            .collect::<Vec<_>>(),
        vec![
            StepEvent::Prompt { vref: id("v1") },
            StepEvent::ToolUse {
                id: id("toolu_a"),
                tool: ToolName::new("Bash"),
                cls: "shell",
                subagent: false
            },
            StepEvent::ToolResult {
                id: id("toolu_a"),
                error: false
            }
        ]
    );
}

/// The relaunched first prompt written after newer rows (`parentUuid:
#[test]
fn a_relaunched_first_prompt_after_newer_rows_starts_no_turn() {
    let first = prompt("v0", 0, "hello").replace(r#""parentUuid":"p""#, r#""parentUuid":null"#);
    let mut f = Fed::new();
    assert_eq!(
        f.push(&first).events(),
        vec![StepEvent::Prompt { vref: id("v0") }]
    );
    f.push(&reply("m1", 10, 1)).push(&turn_end(20, 5));
    let _ = f.events();
    let late =
        prompt("v9", 30, "hello again").replace(r#""parentUuid":"p""#, r#""parentUuid":null"#);
    assert!(f.push(&late).events().is_empty());
    assert_eq!(f.fold.end().last, EndClass::Reply);
    assert!(f.fold.end().closed);
}

/// A row Claude Code writes `isMeta` (no prompt by its words): under
fn meta(id: Option<&str>, uuid: &str, ms: u64, extra: &str, text: &str) -> String {
    let row = prompt("x", ms, text)
        .replace(
            r#""type":"user","#,
            &format!(r#""type":"user","isMeta":true{extra},"#),
        )
        .replace(r#""uuid":"u-x""#, &format!(r#""uuid":"{uuid}""#));
    match id {
        Some(id) => row.replace(r#""promptId":"x""#, &format!(r#""promptId":"{id}""#)),
        None => row.replace(r#""promptId":"x","#, ""),
    }
}

/// The events of `f` but the model's.
fn no_model(evs: Vec<StepEvent>) -> Vec<StepEvent> {
    evs.into_iter()
        .filter(|e| !matches!(e, StepEvent::Model { .. }))
        .collect()
}

/// A TURN CLAUDE CODE NUMBERS OF ITS OWN starts the vendor turn its model's
#[test]
fn a_turn_claude_code_numbers_of_its_own_starts_its_vendor_turn() {
    let loop_words = "keep going: check the land log";
    for (what, wake, vref) in [
        (
            "a /loop wake under a promptId of its own",
            meta(
                Some("p-2"),
                "w-1",
                5_000,
                r#","promptSource":"system","scheduledTaskId":"t1","turnOrigin":"scheduled""#,
                loop_words,
            ),
            "p-2",
        ),
        (
            "a /loop wake under no promptId, by its scheduledTaskId",
            meta(
                None,
                "w-2",
                5_000,
                r#","promptSource":"system","scheduledTaskId":"t1""#,
                loop_words,
            ),
            "w-2",
        ),
        (
            "a row under no promptId, by its turnOrigin",
            meta(
                None,
                "w-3",
                5_000,
                r#","turnOrigin":"scheduled""#,
                loop_words,
            ),
            "w-3",
        ),
        (
            "a missed one-shot task's notice under no promptId",
            meta(
                None,
                "w-4",
                5_000,
                "",
                "The following one-shot scheduled task was missed while Claude was not running.",
            ),
            "w-4",
        ),
    ] {
        let mut f = Fed::new();
        f.push(&prompt("v1", 0, "go"))
            .push(&reply("m1", 100, 5))
            .push(&turn_end(200, 200));
        let _ = f.events();
        f.push(&wake)
            .push(&calls("m3", 6_000, &[("toolu_b", "Bash")]))
            .push(&results(6_100, &[("toolu_b", false)]))
            .push(&turn_end(6_200, 1_200));
        let steps: Vec<Step> = f
            .fold
            .take()
            .steps
            .into_iter()
            .filter(|s| !matches!(s.ev, StepEvent::Model { .. }))
            .collect();
        assert_eq!(
            steps.iter().map(|s| s.ev).collect::<Vec<_>>(),
            vec![
                StepEvent::Prompt { vref: id(vref) },
                StepEvent::ToolUse {
                    id: id("toolu_b"),
                    tool: ToolName::new("Bash"),
                    cls: "shell",
                    subagent: false,
                },
                StepEvent::ToolResult {
                    id: id("toolu_b"),
                    error: false
                },
                StepEvent::TurnEnd {
                    vdur_ms: 1_200,
                    tok_in: 3 + 100 + 1000,
                    tok_out: 20
                },
            ],
            "{what}"
        );
        assert_eq!(steps[0].pos.at_ms, T0 + 5_000, "{what}: dated at the wake");
    }
    for (what, kept) in [
        (
            "a Stop hook's feedback under the turn's promptId",
            meta(
                Some("v1"),
                "h-1",
                150,
                "",
                "Stop hook feedback: the goal is not met",
            ),
        ),
        (
            "/goal's check-in under the turn's promptId",
            meta(Some("v1"), "h-2", 150, "", "Goal check-in: not met yet"),
        ),
        (
            "a Stop hook's feedback under no promptId",
            meta(
                None,
                "h-3",
                150,
                "",
                "Stop hook feedback: the goal is not met",
            ),
        ),
        (
            "the notice's words quoted in a hook's",
            meta(
                None,
                "h-4",
                150,
                "",
                "Stop hook feedback: the log says The following one-shot scheduled task was missed",
            ),
        ),
    ] {
        let mut f = Fed::new();
        f.push(&prompt("v1", 0, "go"))
            .push(&reply("m1", 100, 5))
            .push(&kept)
            .push(&calls("m2", 1_000, &[("toolu_c", "Bash")]))
            .push(&turn_end(1_100, 1_100));
        let evs = no_model(f.events());
        assert_eq!(
            evs.iter()
                .filter(|e| matches!(e, StepEvent::Prompt { .. }))
                .count(),
            1,
            "{what}: {evs:?}"
        );
        assert_eq!(evs[0], StepEvent::Prompt { vref: id("v1") }, "{what}");
        assert!(
            evs.iter()
                .any(|e| matches!(e, StepEvent::ToolUse { id: t, .. } if *t == id("toolu_c"))),
            "{what}"
        );
    }
}

/// A RUNNING TURN THE BIND DID NOT READ (module docs). A tap armed after
#[test]
fn a_running_turn_the_bind_did_not_read_is_no_new_one() {
    use super::super::transcript::corpus_tests::TURN_ROWS;
    let hook = meta(
        Some("v1"),
        "h-1",
        150,
        "",
        "Stop hook feedback: the goal is not met",
    );
    let wake = prompt("w-1", 150, "<task-notification><task-id>b1</task-id>");
    let rest = [
        calls("m2", 1_000, &[("toolu_c", "Bash")]),
        results(1_050, &[("toolu_c", false)]),
        turn_end(1_100, 1_100),
    ];
    let one = 3 + 100 + 1000;
    for (what, first, tok_in, tok_out) in [
        (
            "a Stop hook's feedback after the reply",
            vec![reply("m1", 100, 5), hook.clone()],
            2 * one,
            25,
        ),
        ("a Stop hook's feedback read first", vec![hook], one, 20),
        (
            "a wake after the reply",
            vec![reply("m1", 100, 5), wake],
            2 * one,
            25,
        ),
    ] {
        let mut f = Fed::new();
        for r in first.iter().chain(&rest) {
            f.push(r);
        }
        let evs = no_model(f.events());
        assert!(
            !evs.iter().any(|e| matches!(e, StepEvent::Prompt { .. })),
            "{what}: {evs:?}"
        );
        assert_eq!(
            evs.last(),
            Some(&StepEvent::TurnEnd {
                vdur_ms: 1_100,
                tok_in,
                tok_out
            }),
            "{what}: {evs:?}"
        );
    }
    let (mut begins, mut goes_on) = (0, 0);
    for (name, row) in TURN_ROWS {
        let anew = name.starts_with("claude/begins/");
        if anew {
            begins += 1;
        } else {
            goes_on += 1;
        }
        let mut f = Fed::new();
        f.push(row);
        for r in &rest {
            f.push(r);
        }
        let evs = no_model(f.events());
        assert_eq!(
            matches!(evs.first(), Some(StepEvent::Prompt { .. })),
            anew,
            "{name}: {evs:?}"
        );
        assert!(
            evs.iter()
                .skip(1)
                .all(|e| !matches!(e, StepEvent::Prompt { .. })),
            "{name}: {evs:?}"
        );
    }
    assert!(begins >= 4 && goes_on >= 3, "{begins} {goes_on}");
    let mut f = Fed::new();
    f.push(&calls("m1", 100, &[("toolu_a", "Bash")]))
        .push(&results(150, &[("toolu_a", false)]))
        .push(&turn_end(200, 200));
    let _ = f.events();
    f.push(&meta(
        Some("p-2"),
        "w-9",
        5_000,
        "",
        "keep going: check the land log",
    ))
    .push(&calls("m3", 6_000, &[("toolu_b", "Bash")]));
    assert_eq!(
        no_model(f.events()),
        vec![
            StepEvent::Prompt { vref: id("p-2") },
            StepEvent::ToolUse {
                id: id("toolu_b"),
                tool: ToolName::new("Bash"),
                cls: "shell",
                subagent: false,
            },
        ]
    );
}

/// The steps of fixture `name` read by a fold bound after its first `from`
fn fold_fixture_from(name: &str, from: usize) -> Vec<Step> {
    let (_, jsonl) = fixtures()
        .into_iter()
        .find(|(n, _)| n == name)
        .expect("the fixture");
    let lines: Vec<&str> = jsonl.lines().collect();
    let mut off: u64 = lines[..from].iter().map(|l| l.len() as u64 + 1).sum();
    let mut fold = StepFold::new(0, None);
    fold.read_at(FP, ID, off, false);
    for line in &lines[from..] {
        off += line.len() as u64 + 1;
        fold.push(line.as_bytes(), off);
    }
    let mut steps = fold.take().steps;
    steps.extend(fold.take().steps);
    steps
        .into_iter()
        .filter(|s| !matches!(s.ev, StepEvent::Bind { .. } | StepEvent::Model { .. }))
        .collect()
}

/// The measured 2.1.294 turn, bound after its call's result (a tap armed
#[test]
fn a_measured_nudge_in_a_turn_the_bind_did_not_read_goes_on_it() {
    let steps = fold_fixture_from("claude-2.1.294-transcript-bash-output-wall-phrase.jsonl", 4);
    let evs: Vec<StepEvent> = steps.iter().map(|s| s.ev).collect();
    assert_eq!(
        evs,
        vec![StepEvent::TurnEnd {
            vdur_ms: 3_088,
            tok_in: 21_392 + 21_423,
            tok_out: 2
        }]
    );
}

/// THE CLASS over every `supervise/fixtures` transcript: a fold bound
#[test]
fn a_bind_that_read_no_history_invents_no_prompt() {
    let prompts = |steps: Vec<Step>| -> Vec<(StepId, u64)> {
        steps
            .into_iter()
            .filter_map(|s| match s.ev {
                StepEvent::Prompt { vref } => Some((vref, s.pos.at_ms)),
                _ => None,
            })
            .collect()
    };
    let mut checked = 0;
    for (name, jsonl) in fixtures() {
        let full = prompts(fold_fixture_from(&name, 0));
        for from in 1..=jsonl.lines().count() {
            for p in prompts(fold_fixture_from(&name, from)) {
                checked += 1;
                assert!(
                    full.contains(&p),
                    "{name} bound after line {from}: {p:?} not in {full:?}"
                );
            }
        }
    }
    assert!(checked >= 40, "{checked}");
}

/// The fixture `name` folded from its first byte at floor 0, taking after
fn fold_fixture(name: &str, split: &[usize]) -> Vec<Vec<Step>> {
    let (_, jsonl) = fixtures()
        .into_iter()
        .find(|(n, _)| n == name)
        .expect("the fixture");
    let mut fold = StepFold::new(0, None);
    fold.read_at(FP, ID, 0, false);
    let mut takes = Vec::new();
    let mut off = 0;
    let keep = |b: StepBatch| -> Vec<Step> {
        b.steps
            .into_iter()
            .filter(|s| !matches!(s.ev, StepEvent::Bind { .. } | StepEvent::Model { .. }))
            .collect()
    };
    for (i, line) in jsonl.lines().enumerate() {
        off += line.len() as u64 + 1;
        fold.push(line.as_bytes(), off);
        if split.contains(&i) {
            takes.push(keep(fold.take()));
        }
    }
    takes.push(keep(fold.take()));
    takes
}

/// A FIRST PROMPT WRITTEN AFTER ITS TURN'S FIRST ROWS heads them (the
#[test]
fn a_first_prompt_written_after_its_turns_first_rows_heads_them() {
    let edit = "claude-2.1.300-transcript-box-edit-star-hook-pending.jsonl";
    let ask = id("4edaa7ab-dac9-4cb1-9aa7-98ca2e1ec950");
    let first_call = id("toolu_labd4d52d26afb8cbdf");
    let second_call = id("toolu_lab9a787d75af12c706");
    let asked_ms = stamp_ms("2026-10-15T20:09:56.653Z").expect("a time");
    for split in [&[][..], &[4][..], &[5][..]] {
        let takes = fold_fixture(edit, split);
        let steps: Vec<Step> = takes.into_iter().flatten().collect();
        let evs: Vec<StepEvent> = steps.iter().map(|s| s.ev).collect();
        assert_eq!(
            evs,
            vec![
                StepEvent::Prompt { vref: ask },
                StepEvent::ToolUse {
                    id: first_call,
                    tool: ToolName::new("Read"),
                    cls: "read",
                    subagent: false,
                },
                StepEvent::ToolResult {
                    id: first_call,
                    error: false
                },
                StepEvent::ToolUse {
                    id: second_call,
                    tool: ToolName::new("Edit"),
                    cls: "edit",
                    subagent: false,
                },
            ],
            "taken after rows {split:?}"
        );
        assert_eq!(steps[0].pos.at_ms, asked_ms, "dated at the prompt's row");
        let offs: Vec<u64> = steps.iter().map(|s| s.pos.off).collect();
        assert!(offs.windows(2).all(|w| w[0] < w[1]), "{offs:?}");
    }
    let limit = "claude-2.1.294-transcript-session-limit-first-turn.jsonl";
    let evs: Vec<StepEvent> = fold_fixture(limit, &[])
        .into_iter()
        .flatten()
        .map(|s| s.ev)
        .collect();
    assert_eq!(
        evs.first(),
        Some(&StepEvent::Prompt {
            vref: id("1d13d31b-c2b3-4378-a89f-a61c905a4291")
        }),
        "{evs:?}"
    );
    assert_eq!(
        evs.iter()
            .filter(|e| matches!(e, StepEvent::Prompt { .. }))
            .count(),
        1
    );
    assert!(evs.iter().any(|e| matches!(e, StepEvent::TurnEnd { .. })));
    assert!(evs.iter().any(|e| matches!(e, StepEvent::EndWall { .. })));
    let parentless = |ms: u64| {
        prompt("v9", ms, "a new ask").replace(r#""parentUuid":"p""#, r#""parentUuid":null"#)
    };
    let call = calls("m1", 100, &[("toolu_a", "Bash")]);
    let mut f = Fed::new();
    f.push(&call).push(&parentless(500));
    let evs = no_model(f.events());
    assert_eq!(
        evs,
        vec![StepEvent::ToolUse {
            id: id("toolu_a"),
            tool: ToolName::new("Bash"),
            cls: "shell",
            subagent: false,
        }]
    );
    let mut f = Fed::new();
    f.push(&call).push(&parentless(50));
    assert_eq!(
        no_model(f.events()).first(),
        Some(&StepEvent::Prompt { vref: id("v9") })
    );
    let mut f = Fed::new();
    f.push(&call);
    assert!(no_model(f.events()).is_empty(), "held through one take");
    assert_eq!(no_model(f.events()).len(), 1, "handed on at the next");
}

/// THE OPENING HOLD WAITS IN THE QUEUE ITSELF: its steps count toward
#[test]
fn the_opening_hold_waits_in_the_bounded_queue() {
    let boundary = format!(
        r#"{{"parentUuid":null,"type":"system","subtype":"compact_boundary","content":"Conversation compacted","isMeta":false,"compactMetadata":{{"trigger":"auto","preTokens":160000}},"uuid":"c-1","timestamp":"{}"}}"#,
        at(0)
    );
    for burst in [1, STEP_QUEUE] {
        let mut f = Fed::new();
        f.push(&boundary);
        for i in 0..burst {
            let call = format!("toolu_{i}");
            f.push(&calls(&format!("m{i}"), 100 + i as u64, &[(&call, "Bash")]));
        }
        let first = f.fold.take();
        let evs: Vec<StepEvent> = first.steps.iter().map(|s| s.ev).collect();
        let before = if burst == 1 {
            vec![StepEvent::Bind { len: 0 }, StepEvent::Compact]
        } else {
            vec![StepEvent::Bind { len: 0 }]
        };
        assert_eq!(evs, before, "a burst of {burst}: only what came before");
        let second = f.fold.take();
        assert!(first.steps.len() + second.steps.len() <= STEP_QUEUE);
        assert!(
            second
                .steps
                .iter()
                .all(|s| matches!(s.ev, StepEvent::ToolUse { .. } | StepEvent::Model { .. })),
            "a burst of {burst}"
        );
        let last = format!("toolu_{}", burst - 1);
        assert_eq!(
            second.steps.last().map(|s| s.ev),
            Some(StepEvent::ToolUse {
                id: id(&last),
                tool: ToolName::new("Bash"),
                cls: "shell",
                subagent: false,
            }),
            "the newest held step is kept"
        );
    }
}

/// THE CLASS over every `supervise/fixtures` transcript (each one added there
#[test]
fn every_fixtures_turn_steps_follow_a_prompt() {
    let (mut checked, mut prompts) = (0, 0);
    for (name, _) in fixtures() {
        let mut prompted = false;
        for s in fold_fixture(&name, &[]).into_iter().flatten() {
            match s.ev {
                StepEvent::Prompt { .. } => {
                    prompted = true;
                    prompts += 1;
                }
                StepEvent::ToolUse { .. }
                | StepEvent::ToolResult { .. }
                | StepEvent::TurnEnd { .. }
                | StepEvent::EndWall { .. } => {
                    checked += 1;
                    assert!(prompted, "{name}: {:?} before any prompt", s.ev);
                }
                _ => {}
            }
        }
    }
    assert!(checked >= 40 && prompts >= 20, "{checked} {prompts}");
}

/// S24: a compaction's boundary is an instant step; its summary row is no
#[test]
fn a_compact_boundary_is_a_compact_step() {
    let boundary = format!(
        r#"{{"parentUuid":null,"logicalParentUuid":"u-5","type":"system","subtype":"compact_boundary","content":"Conversation compacted","isMeta":false,"compactMetadata":{{"trigger":"auto","preTokens":160000}},"uuid":"c-1","timestamp":"{}"}}"#,
        at(0)
    );
    let mut f = Fed::new();
    assert_eq!(f.push(&boundary).events(), vec![StepEvent::Compact]);
}

/// §2.6: a limit notice closed by its turn's end is the turn's wall
#[test]
fn a_notice_closed_by_the_turn_end_is_the_turns_wall() {
    let mut f = Fed::new();
    f.push(&prompt("v1", 0, "go")).push(&notice(
        10,
        "You've hit your session limit · resets 3pm (Europe/London)",
        "rate_limit",
    ));
    assert!(
        !f.events()
            .iter()
            .any(|e| matches!(e, StepEvent::EndWall { .. }))
    );
    let end = f.fold.end();
    assert_eq!(
        (end.last, end.closed, end.wall()),
        (EndClass::Notice, false, None)
    );
    assert!(end.confirms(WallKind::UsageSession) && end.confirms(WallKind::UsageWeekly));
    assert!(!end.confirms(WallKind::Context) && !end.confirms(WallKind::Auth));
    f.push(&turn_end(20, 10));
    let evs = f.events();
    assert!(
        matches!(
            evs[..],
            [
                StepEvent::TurnEnd { .. },
                StepEvent::EndWall {
                    kind: WallKind::UsageSession
                }
            ]
        ),
        "{evs:?}"
    );
}

/// Plan §2.8: each discriminant of the wrong shape latches the fold off,
#[test]
fn a_wrong_shaped_discriminant_latches_the_fold_off() {
    let bad = [
        calls("m1", 0, &[("toolu_a", "Bash")]).replace(r#""id":"toolu_a""#, r#""id":7"#),
        results(0, &[("toolu_a", false)])
            .replace(r#""tool_use_id":"toolu_a""#, r#""tool_use_id":null"#),
        turn_end(0, 5).replace(r#""durationMs":5"#, r#""durationMs":"5""#),
        prompt("v1", 0, "go").replace(&at(0), "yesterday"),
    ];
    for row in &bad {
        let mut f = Fed::new();
        f.push(row).push(&prompt("v2", 10, "next"));
        assert_eq!(f.events(), vec![StepEvent::Off { why: "unknown" }], "{row}");
        assert!(f.fold.broken());
    }
    let mut f = Fed::new();
    f.push(&format!(
        r#"{{"type":"attachment","attachment":{{"type":"new_kind"}},"timestamp":"{}"}}"#,
        at(0)
    ))
    .push(r#"{"type":"mode-change","mode":"plan"}"#)
    .push(&reply("m1", 0, 1).replace(r#""effort":"high""#, r#""effort":["high"]"#))
    .push(&prompt("v1", 10, "go"));
    let evs = f.events();
    assert!(!f.fold.broken());
    assert_eq!(
        evs.last(),
        Some(&StepEvent::Prompt { vref: id("v1") }),
        "{evs:?}"
    );
}

/// THE LATCH IS THE BOUND FILE'S: a fold latched off on one file reads
#[test]
fn a_latch_lifts_on_a_bind_of_another_file() {
    let mut f = Fed::new();
    f.push(&turn_end(0, 5).replace(r#""durationMs":5"#, r#""durationMs":"5""#));
    assert!(f.fold.broken());
    assert_eq!(f.events(), vec![StepEvent::Off { why: "unknown" }]);
    f.fold.read_at(FP, ID, 0, true);
    let row = prompt("v2", 10, "next");
    f.fold.push(row.as_bytes(), row.len() as u64 + 1);
    assert!(f.fold.broken());
    assert!(f.fold.take().steps.is_empty());
    f.fold.read_at(FP + 1, ID, 0, false);
    let row = prompt("v3", 20, "fresh");
    f.fold.push(row.as_bytes(), row.len() as u64 + 1);
    assert!(!f.fold.broken());
    let evs: Vec<StepEvent> = f.fold.take().steps.into_iter().map(|s| s.ev).collect();
    assert_eq!(
        evs,
        vec![
            StepEvent::Bind { len: 0 },
            StepEvent::Prompt { vref: id("v3") }
        ]
    );
}

/// S1, S23: rows older than the floor, and rows ending at or below the
#[test]
fn history_below_the_floor_or_the_seed_queues_nothing() {
    let rows = [
        prompt("v1", 0, "old"),
        reply("m1", 10, 1),
        turn_end(20, 5),
        prompt("v2", 5_000, "new"),
    ];
    let line_ends: Vec<u64> = rows
        .iter()
        .scan(0, |o, r| {
            *o += r.len() as u64 + 1;
            Some(*o)
        })
        .collect();
    let mut fold = StepFold::new(T0 + 1_000, None);
    fold.read_at(FP, ID, 0, false);
    for (r, e) in rows.iter().zip(&line_ends) {
        fold.push(r.as_bytes(), *e);
    }
    let evs: Vec<StepEvent> = fold.take().steps.into_iter().map(|s| s.ev).collect();
    assert_eq!(
        evs,
        vec![
            StepEvent::Bind { len: 0 },
            StepEvent::Prompt { vref: id("v2") }
        ]
    );
    let mut fold = StepFold::new(0, None);
    fold.read_at(FP, ID, line_ends[2], false);
    for (r, e) in rows.iter().zip(&line_ends) {
        fold.push(r.as_bytes(), *e);
    }
    let batch = fold.take();
    assert_eq!(batch.steps.len(), 2, "{:?}", batch.steps);
    assert_eq!(fold.end().last, EndClass::Input);
    let newer = prompt("v3", 6_000, "newer");
    let newer_end = line_ends[3] + newer.len() as u64 + 1;
    fold.read_at(FP, ID, newer_end, true);
    for (r, e) in rows.iter().zip(&line_ends) {
        fold.push(r.as_bytes(), *e);
    }
    fold.push(newer.as_bytes(), newer_end);
    let again: Vec<StepEvent> = fold.take().steps.into_iter().map(|s| s.ev).collect();
    assert_eq!(
        again,
        vec![
            StepEvent::Bind { len: newer_end },
            StepEvent::Prompt { vref: id("v3") }
        ]
    );
    for (given, want) in [
        (
            StepMark {
                fp: FP,
                id: ID,
                off: line_ends[0],
            },
            3,
        ),
        (
            StepMark {
                fp: FP + 1,
                id: ID,
                off: line_ends[0],
            },
            0,
        ),
    ] {
        let mut fold = StepFold::new(0, Some(given));
        fold.read_at(FP, ID, line_ends[3], false);
        for (r, e) in rows.iter().zip(&line_ends) {
            fold.push(r.as_bytes(), *e);
        }
        let queued = fold.take().steps.len() - 1;
        assert_eq!(queued, want, "{given:?}");
    }
}

/// A11: the model is said from the first row past the seed, even when the
#[test]
fn a_fresh_row_names_the_model_the_history_had() {
    let old = reply("m0", 0, 1);
    let mut fold = StepFold::new(0, None);
    fold.read_at(FP, ID, old.len() as u64 + 1, false);
    let mut off = 0;
    let mut evs = Vec::new();
    for row in [old, reply("m1", 10, 1), reply("m2", 20, 1)] {
        off += row.len() as u64 + 1;
        fold.push(row.as_bytes(), off);
        evs.push(
            fold.take()
                .steps
                .into_iter()
                .map(|s| s.ev)
                .filter(|e| !matches!(e, StepEvent::Bind { .. }))
                .collect::<Vec<_>>(),
        );
    }
    let model = StepEvent::Model {
        model: id("claude-opus-5-5"),
        effort: Some(id("high")),
        ver: None,
    };
    assert_eq!(evs, vec![vec![], vec![model], vec![]]);
}

/// A11 AT THE FLOOR: a row past the seed but older than the floor (the
#[test]
fn a_row_below_the_floor_names_no_model() {
    let mut fold = StepFold::new(T0 + 15, None);
    fold.read_at(FP, ID, 0, false);
    let mut off = 0;
    let mut evs = Vec::new();
    for row in [reply("m0", 0, 1), reply("m1", 20, 1)] {
        off += row.len() as u64 + 1;
        fold.push(row.as_bytes(), off);
        evs.push(
            fold.take()
                .steps
                .into_iter()
                .map(|s| s.ev)
                .filter(|e| !matches!(e, StepEvent::Bind { .. }))
                .collect::<Vec<_>>(),
        );
    }
    let model = StepEvent::Model {
        model: id("claude-opus-5-5"),
        effort: Some(id("high")),
        ver: None,
    };
    assert_eq!(evs, vec![vec![], vec![model]]);
}

/// The queue keeps its bind and the newest steps after it, [`STEP_QUEUE`]
#[test]
fn a_full_queue_drops_its_oldest_and_unread_rows_are_counted() {
    let mut f = Fed::new();
    for i in 0..(STEP_QUEUE as u64 + 4) {
        f.push(&prompt(&format!("v{i}"), i, "go"));
    }
    let batch = f.fold.take();
    assert_eq!((batch.steps.len(), batch.overflow), (STEP_QUEUE, 5));
    assert_eq!(
        batch.steps.first().map(|s| s.ev),
        Some(StepEvent::Bind { len: 0 }),
        "the bind is never dropped"
    );
    assert_eq!(
        batch.steps.get(1).map(|s| s.ev),
        Some(StepEvent::Prompt { vref: id("v5") })
    );
    assert_eq!(
        batch.steps.last().map(|s| s.ev),
        Some(StepEvent::Prompt { vref: id("v1027") })
    );
    f.push("{\"type\":\"user\",").push("not json \"user\"");
    f.fold.long_line(f.off + 10);
    assert_eq!(f.fold.take().lost, 3);
    let mut fold = StepFold::new(0, None);
    fold.read_at(FP, ID, 1_000, false);
    fold.push(b"{\"type\":\"user\",", 20);
    fold.long_line(40);
    assert_eq!(fold.take().lost, 0);
}

/// S9, A19, T8: the lab measured 2.1.303 alone, so its steps are exact and
#[test]
fn every_build_is_partial_until_one_is_measured() {
    assert_eq!(MEASURED_BUILDS, ["2.1.303"]);
    assert_eq!(state_for(MEASURED_BUILDS, Some("2.1.303")), StepState::On);
    for other in ["2.1.300", "2.1.302", "2.1.304", "2.1.3030", "2.1.30"] {
        assert_eq!(
            state_for(MEASURED_BUILDS, Some(other)),
            StepState::Partial,
            "{other}"
        );
    }
    assert_eq!(state_for(MEASURED_BUILDS, None), StepState::Partial);
    assert_eq!(state_for(&["2.1.300"], Some("2.1.300")), StepState::On);
    assert_eq!(state_for(&["2.1.300"], Some("2.1.299")), StepState::Partial);
    assert_eq!(state_for(&["2.1.300"], Some("2.1.301")), StepState::Partial);
    assert_eq!(
        state_for(&["2.1.299", "2.1.301"], Some("2.1.301")),
        StepState::On
    );
    assert_eq!(state_for(&["2.1.300"], None), StepState::Partial);
}

/// Where a measured build's reduction is committed (T8).
const MEASURED_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/harness/testdata/steps");

/// How a Codex rollout's reduction is named beside them (session log 5G).
const CODEX_REDUCTION: &str = "codex-";

/// THE GUARD (T8): every build `builds` names has its reduction in `dir`
fn measured_builds_hold(dir: &std::path::Path, builds: &[&str]) -> Result<(), String> {
    for b in builds {
        let path = dir.join(format!("{b}.jsonl"));
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("{b} is named, and its reduction {}: {e}", path.display()))?;
        let mut fold = StepFold::new(0, None);
        fold.read_at(FP, ID, 0, false);
        let mut end = 0u64;
        for line in text.lines() {
            end += line.len() as u64 + 1;
            fold.push(line.as_bytes(), end);
        }
        let (first, second) = (fold.take(), fold.take());
        let steps: Vec<Step> = first.steps.into_iter().chain(second.steps).collect();
        if first.lost + second.lost + first.overflow + second.overflow != 0 {
            return Err(format!("{b}: the reduction folds with loss"));
        }
        let want = state_for(builds, Some(b));
        for s in &steps {
            if matches!(s.ev, StepEvent::Off { .. })
                || (!matches!(s.ev, StepEvent::Bind { .. }) && s.state != want)
            {
                return Err(format!("{b}: {:?} at {:?}", s.ev, s.state));
            }
        }
        let has = |p: &dyn Fn(&StepEvent) -> bool| steps.iter().any(|s| p(&s.ev));
        let expected = has(&|e| matches!(e, StepEvent::Prompt { .. }))
            && has(&|e| matches!(e, StepEvent::ToolUse { cls: "shell", .. }))
            && has(&|e| matches!(e, StepEvent::ToolResult { error: false, .. }))
            && has(&|e| matches!(e, StepEvent::TurnEnd { .. }))
            && has(&|e| matches!(e, StepEvent::EndWall { kind } if kind.reads_limited()));
        if !expected {
            let evs: Vec<_> = steps.iter().map(|s| s.ev).collect();
            return Err(format!(
                "{b}: the reduction lacks a lab run's events: {evs:?}"
            ));
        }
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for e in entries.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(CODEX_REDUCTION) {
            continue;
        }
        let build = name.strip_suffix(".jsonl").unwrap_or(&name);
        if !builds.contains(&build) {
            return Err(format!(
                "{name} is committed, and no measured build names it"
            ));
        }
    }
    Ok(())
}

/// T8: a build is exact exactly when its reduction is committed.
#[test]
fn a_build_is_exact_iff_its_reduction_is_committed() {
    measured_builds_hold(std::path::Path::new(MEASURED_DIR), MEASURED_BUILDS)
        .expect("the measured builds and their reductions agree");
}

/// NEGATIVE CONTROL (T8): a listed build with no reduction fails the guard,
#[test]
fn a_listed_build_with_no_reduction_fails_the_guard() {
    let dir = std::path::Path::new(MEASURED_DIR);
    let e = measured_builds_hold(dir, &["9.9.999"]).expect_err("no file");
    assert!(e.starts_with("9.9.999 is named"), "{e}");
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    std::fs::write(tmp.path().join("9.9.998.jsonl"), "# a reduction\n").expect("written");
    let e = measured_builds_hold(tmp.path(), &[]).expect_err("an unnamed file");
    assert!(e.contains("no measured build names it"), "{e}");
    let e = measured_builds_hold(tmp.path(), &["9.9.998"]).expect_err("no lab run's events");
    assert!(e.contains("lacks a lab run's events"), "{e}");
}

/// A11: the class table by name. The names the approval policy knows
#[test]
fn tools_are_classed_by_name() {
    for (name, want) in [
        ("Bash", ("shell", false)),
        ("BashOutput", ("shell", false)),
        ("Edit", ("edit", false)),
        ("MultiEdit", ("edit", false)),
        ("NotebookEdit", ("edit", false)),
        ("Write", ("write", false)),
        ("Read", ("read", false)),
        ("Grep", ("search", false)),
        ("Glob", ("search", false)),
        ("WebFetch", ("web", false)),
        ("Task", ("agent", true)),
        ("Agent", ("agent", true)),
        ("TodoWrite", ("plan", false)),
        ("mcp__claude-in-chrome__navigate", ("mcp", false)),
        ("Monitor", ("other", false)),
        ("", ("other", false)),
    ] {
        assert_eq!(tool_class(name), want, "{name}");
    }
    let known = [
        "Write",
        "Read",
        "Glob",
        "Grep",
        "Edit",
        "Bash",
        "NotebookEdit",
    ];
    for name in crate::supervise::policy::box_call::PROVABLE
        .iter()
        .chain(&known)
    {
        let (cls, _) = tool_class(name);
        assert_ne!(cls, "other", "{name}");
        assert_ne!(
            cls == "shell",
            *name != "Bash",
            "{name}: only Bash is a shell"
        );
    }
    assert_eq!(ToolName::new(&"x".repeat(70)).as_str().len(), ToolName::MAX);
    assert_eq!(ToolName::new("é\u{7}é").as_str(), "éé");
    assert!(StepId::new(&"a".repeat(41)).is_none() && StepId::new("a b").is_none());
}

/// S6, plan §2.7: no text of a prompt, a reply, a tool's input or result,
#[test]
fn a_transcripts_text_never_reaches_a_step() {
    let local = format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout>SENTINEL-LOCAL</local-command-stdout>","uuid":"l-1","timestamp":"{}"}}"#,
        at(60)
    );
    let mut f = Fed::new();
    f.push(&prompt("v1", 0, "SENTINEL-PROMPT"))
        .push(&calls(
            "m1",
            10,
            &[("toolu_a", "Bash"), ("toolu_b", "SENTINEL_NAME")],
        ))
        .push(&results(20, &[("toolu_a", false)]))
        .push(&notice(
            30,
            "API Error: 500 SENTINEL-NOTICE",
            "server_error",
        ))
        .push(&turn_end(40, 5))
        .push(&local);
    let batch = f.fold.take();
    let seen = format!("{batch:?}");
    for s in [
        "SENTINEL-PROMPT",
        "SENTINEL-REPLY",
        "SENTINEL-INPUT",
        "SENTINEL-RESULT",
        "SENTINEL-NOTICE",
        "SENTINEL-LOCAL",
    ] {
        assert!(!seen.contains(s), "{s} leaked: {seen}");
    }
    assert!(seen.contains("SENTINEL_NAME"), "the tool's name is kept");
    assert_eq!(batch.end.last, EndClass::Opaque);
}

/// Every transcript under `supervise/fixtures` (A9: all of them, so one
fn fixtures() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/supervise/fixtures");
    let mut all: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .map(|p| {
            let name = p
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read_to_string(&p).expect("a fixture"))
        })
        .collect();
    all.sort();
    all
}

/// One screen wall of every kind the screen reads that the transcript can
const SCREENS: &[WallKind] = &[
    WallKind::UsageSession,
    WallKind::UsageWeekly,
    WallKind::ModelBucket { consent: false },
    WallKind::Spend,
    WallKind::Context,
    WallKind::Auth,
    WallKind::Overloaded,
    WallKind::ApiError {
        code: Some(500),
        retryable: true,
        cause: aterm_phase::ApiCause::Server,
    },
    WallKind::ApiError {
        code: Some(429),
        retryable: true,
        cause: aterm_phase::ApiCause::Server,
    },
    WallKind::Memory,
];

fn class_of(e: &Entry) -> EndClass {
    match e {
        Entry::Nothing => EndClass::Nothing,
        Entry::Notice { .. } => EndClass::Notice,
        Entry::ToolResult => EndClass::ToolResult,
        Entry::Input => EndClass::Input,
        Entry::Reply => EndClass::Reply,
    }
}

/// THE AGREEMENT (A9, S7): after every line of every `supervise/fixtures`
#[test]
fn the_end_agrees_with_the_harness_on_every_fixture_and_corpus_row() {
    let corpus: String = super::super::transcript::corpus_tests::ROWS
        .iter()
        .chain(super::super::transcript::corpus_tests::TURN_ROWS)
        .filter(|(id, _)| id.starts_with("claude/"))
        .map(|(_, row)| format!("{row}\n"))
        .collect();
    let mut opaque_seen = 0;
    let mut confirmed = 0;
    let mut refuted = 0;
    let fixtures = fixtures();
    assert!(fixtures.len() >= 24, "{} fixtures", fixtures.len());
    for (name, jsonl) in fixtures
        .iter()
        .map(|(n, j)| (n.as_str(), j.as_str()))
        .chain([("corpus", corpus.as_str())])
    {
        let mut fold = StepFold::new(0, None);
        fold.read_at(FP, ID, 0, false);
        let mut so_far = String::new();
        let mut off = 0;
        for line in jsonl.lines() {
            so_far.push_str(line);
            so_far.push('\n');
            off += line.len() as u64 + 1;
            fold.push(line.as_bytes(), off);
            let mine = fold.end();
            let theirs = transcript_ends_on(&so_far);
            let at = || format!("{name} after {:?}", &line[..line.len().min(120)]);
            if mine.last == EndClass::Opaque {
                opaque_seen += 1;
                for &k in SCREENS {
                    assert_eq!(judge_wall(k, &mine, 0), Judge::Wait, "{}", at());
                }
                continue;
            }
            assert_eq!(mine.last, class_of(&theirs.last), "{}", at());
            assert_eq!(mine.closed, theirs.closed, "{}", at());
            assert_eq!(mine.at_ms.map(|m| (m / 1000) as i64), theirs.at, "{}", at());
            assert_eq!(mine.wall(), transcript_wall(&theirs), "{}", at());
            assert_eq!(mine.lagging(), theirs.lagging(), "{}", at());
            for &k in SCREENS {
                if k == WallKind::Memory {
                    continue;
                }
                assert_eq!(mine.confirms(k), confirms(k, &theirs), "{} for {k:?}", at());
                match judge_wall(k, &mine, 0) {
                    Judge::Confirm => {
                        confirmed += 1;
                        assert!(confirms(k, &theirs), "{}", at());
                    }
                    Judge::Refute => {
                        refuted += 1;
                        assert!(!confirms(k, &theirs) && !theirs.lagging(), "{}", at());
                    }
                    Judge::Wait => {}
                }
            }
        }
    }
    assert!(
        opaque_seen > 0 && confirmed > 0 && refuted > 0,
        "{opaque_seen} {confirmed} {refuted}"
    );
    let (_, limit) = fixtures
        .iter()
        .find(|(n, _)| n == "claude-2.1.294-transcript-session-limit.jsonl")
        .expect("the session-limit fixture");
    let theirs = transcript_ends_on(limit);
    let mut fold = StepFold::new(0, None);
    fold.read_at(FP, ID, 0, false);
    let mut off = 0;
    for line in limit
        .lines()
        .filter(|l| !l.contains("isApiErrorMessage\":true"))
    {
        off += line.len() as u64 + 1;
        fold.push(line.as_bytes(), off);
    }
    assert_eq!(class_of(&theirs.last), EndClass::Notice);
    assert_ne!(fold.end().last, class_of(&theirs.last));
}

/// §2.6: the judge confirms the end's own class, waits while the end lags,
#[test]
fn the_wall_judge_confirms_waits_and_refutes() {
    let mut f = Fed::new();
    f.push(&prompt("v1", 0, "go")).push(&notice(
        1_000,
        "Not logged in · Please run /login",
        "authentication_failed",
    ));
    let end = f.fold.end();
    assert_eq!(judge_wall(WallKind::Auth, &end, 0), Judge::Confirm);
    assert_eq!(judge_wall(WallKind::UsageSession, &end, 0), Judge::Refute);
    assert_eq!(
        judge_wall(WallKind::UsageSession, &end, T0 + 2_000),
        Judge::Wait,
        "older than the turn"
    );
    assert_eq!(judge_wall(WallKind::Memory, &end, 0), Judge::Wait);
    let mut g = Fed::new();
    g.push(&prompt("v1", 0, "go"))
        .push(&calls("m1", 10, &[("toolu_a", "Bash")]));
    assert_eq!(
        judge_wall(WallKind::Context, &g.fold.end(), 0),
        Judge::Wait,
        "lagging"
    );
    g.push(&results(20, &[("toolu_a", false)]))
        .push(&reply("m2", 30, 1))
        .push(&turn_end(40, 5));
    assert_eq!(
        judge_wall(WallKind::Context, &g.fold.end(), 0),
        Judge::Refute,
        "closed on a reply"
    );
    g.push(&format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"local_command","content":"x","uuid":"l-1","timestamp":"{}"}}"#,
        at(50)
    ))
    .push(&turn_end(60, 5));
    let end = g.fold.end();
    assert_eq!((end.last, end.closed), (EndClass::Opaque, true));
    assert_eq!(
        judge_wall(WallKind::Context, &end, 0),
        Judge::Wait,
        "opaque"
    );
}

struct Dir(aterm_tempfile::TempDir);

impl Dir {
    fn new() -> Dir {
        Dir(aterm_tempfile::Builder::new()
            .prefix("aterm-steps-")
            .tempdir()
            .expect("a scratch dir"))
    }
}

fn append(path: &std::path::Path, rows: &[String]) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open");
    for r in rows {
        writeln!(f, "{r}").expect("append");
    }
}

/// A20: the cursor hands the tap each completed main-chain line once, with
#[test]
fn the_cursor_taps_each_line_once_with_its_end_offset() {
    use super::super::session_usage::TranscriptCursor;
    let dir = Dir::new();
    let path = dir.0.path().join("s.jsonl");
    append(&path, &[prompt("v0", 0, "old"), reply("m0", 10, 5)]);
    let len0 = std::fs::metadata(&path).expect("stat").len();
    let mut cursor = TranscriptCursor::new(&path);
    let mut plain = TranscriptCursor::new(&path);
    let mut tap = StepFold::new(0, None);
    cursor
        .advance_tapped(u64::MAX, Some(&mut tap))
        .expect("read");
    assert_eq!(
        tap.take().steps.iter().map(|s| s.ev).collect::<Vec<_>>(),
        vec![StepEvent::Bind { len: len0 }]
    );
    append(&path, &[prompt("v1", 20, "new")]);
    let late = prompt("v2", 30, "late");
    let (head, tail) = late.split_at(40);
    let write = |bytes: &[u8]| {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(bytes))
            .expect("append");
    };
    write(head.as_bytes());
    cursor
        .advance_tapped(u64::MAX, Some(&mut tap))
        .expect("read");
    let steps = tap.take().steps;
    let len1 = len0 + prompt("v1", 20, "new").len() as u64 + 1;
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(
        (steps[0].ev, steps[0].pos.off),
        (StepEvent::Prompt { vref: id("v1") }, len1)
    );
    write(format!("{tail}\n").as_bytes());
    cursor
        .advance_tapped(u64::MAX, Some(&mut tap))
        .expect("read");
    let steps = tap.take().steps;
    assert_eq!(
        steps.iter().map(|s| (s.ev, s.pos.off)).collect::<Vec<_>>(),
        vec![(
            StepEvent::Prompt { vref: id("v2") },
            len1 + late.len() as u64 + 1
        )]
    );
    plain.advance(u64::MAX).expect("read");
    assert_eq!(cursor.fold().rows, plain.fold().rows);
    assert_eq!(cursor.fold().assistant_rows, plain.fold().assistant_rows);
    let rewritten = format!("{}\n{}\n", prompt("v0", 0, "old"), prompt("v1", 20, "new"));
    std::fs::write(&path, &rewritten).expect("rewrite");
    let a = cursor
        .advance_tapped(u64::MAX, Some(&mut tap))
        .expect("read");
    assert!(a.restarted);
    let evs: Vec<StepEvent> = tap.take().steps.into_iter().map(|s| s.ev).collect();
    assert_eq!(
        evs,
        vec![StepEvent::Bind {
            len: rewritten.len() as u64
        }]
    );
}

/// A FILE THAT DID NOT EXIST HELD NO HISTORY (S1 as built): a fold told
#[test]
fn a_file_the_fold_saw_absent_is_bound_from_its_first_byte() {
    let rows = [
        prompt("v0", 0, "older than the floor"),
        prompt("v1", 2_000, "new"),
    ];
    let mut ends = Vec::new();
    let mut end = 0;
    for r in &rows {
        end += r.len() as u64 + 1;
        ends.push(end);
    }
    let fold_of = |vacant: bool| {
        let mut fold = StepFold::new(T0 + 1_000, None);
        if vacant {
            fold.note_vacant(SID);
            fold.note_found(SID, FP);
        }
        fold.read_at(FP, ID, end, false);
        for (r, e) in rows.iter().zip(&ends) {
            fold.push(r.as_bytes(), *e);
        }
        fold
    };
    let mut vacant = fold_of(true);
    assert!(vacant.is_bound());
    let evs: Vec<StepEvent> = vacant.take().steps.into_iter().map(|s| s.ev).collect();
    assert_eq!(
        evs,
        vec![
            StepEvent::Bind { len: 0 },
            StepEvent::Prompt { vref: id("v1") }
        ]
    );
    let evs: Vec<StepEvent> = fold_of(false)
        .take()
        .steps
        .into_iter()
        .map(|s| s.ev)
        .collect();
    assert_eq!(evs, vec![StepEvent::Bind { len: end }]);
    vacant.read_at(FP + 1, ID, 7, false);
    let evs: Vec<StepEvent> = vacant.take().steps.into_iter().map(|s| s.ev).collect();
    assert_eq!(evs, vec![StepEvent::Bind { len: 7 }]);
}

/// THE VACANCY IS THE ABSENT SESSION'S: a fold bound to one file, told
#[test]
fn a_vacancy_seeds_only_the_absent_sessions_file() {
    let rows = [prompt("v1", 2_000, "an older conversation's")];
    let end = rows[0].len() as u64 + 1;
    let fold_of = |found: u64| {
        let mut fold = StepFold::new(T0, None);
        fold.read_at(FP, ID, 10, false);
        fold.note_vacant(SID);
        fold.note_found(found, FP + 1);
        fold.read_at(FP + 1, ID, end, false);
        fold.push(rows[0].as_bytes(), end);
        fold.take()
            .steps
            .into_iter()
            .map(|s| s.ev)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        fold_of(SID + 1),
        vec![StepEvent::Bind { len: 10 }, StepEvent::Bind { len: end }],
        "another session's file is history"
    );
    assert_eq!(
        fold_of(SID),
        vec![
            StepEvent::Bind { len: 10 },
            StepEvent::Bind { len: 0 },
            StepEvent::Prompt { vref: id("v1") }
        ]
    );
}

/// The same through the footer's cache, as `read_pid` tells it of each
#[test]
fn the_footer_cache_keys_a_vacancy_to_its_session() {
    use super::super::footer::FooterCache;
    let dir = Dir::new();
    let a = dir.0.path().join("a.jsonl");
    let d = dir.0.path().join("d.jsonl");
    let x = dir.0.path().join("x.jsonl");
    append(&a, &[prompt("v0", 0, "a")]);
    append(&d, &[prompt("v1", 100, "d's history")]);
    let none = |_: Option<&str>, _: i64| -> Option<i64> { None };
    let mut cache = FooterCache::default();
    cache.set_floor(0, None);
    cache.note_entry(None, "a", Some((&a, true)));
    let _ = cache.usage(Some(a.clone()), 0, &none);
    cache.note_entry(None, "x", None);
    cache.note_entry(None, "d", Some((&d, true)));
    let _ = cache.usage(Some(d.clone()), 0, &none);
    let evs: Vec<StepEvent> = cache.take_steps().steps.into_iter().map(|s| s.ev).collect();
    let len = |p: &std::path::Path| std::fs::metadata(p).expect("len").len();
    assert_eq!(
        evs,
        vec![
            StepEvent::Bind { len: len(&a) },
            StepEvent::Bind { len: len(&d) }
        ]
    );
    cache.note_entry(None, "x", None);
    append(&x, &[prompt("v2", 200, "x's first")]);
    cache.note_entry(None, "x", Some((&x, true)));
    let _ = cache.usage(Some(x.clone()), 0, &none);
    let evs: Vec<StepEvent> = cache.take_steps().steps.into_iter().map(|s| s.ev).collect();
    assert_eq!(
        evs,
        vec![
            StepEvent::Bind { len: 0 },
            StepEvent::Prompt { vref: id("v2") }
        ]
    );
}

/// The footer's cache taps nothing until the glue arms it; armed on a
#[test]
fn the_footer_cache_taps_only_once_armed() {
    use super::super::footer::FooterCache;
    let dir = Dir::new();
    let path = dir.0.path().join("s.jsonl");
    append(&path, &[prompt("v0", 0, "old")]);
    let mut cache = FooterCache::default();
    let none = |_: Option<&str>, _: i64| -> Option<i64> { None };
    let _ = cache.usage(Some(path.clone()), 0, &none);
    assert!(cache.take_steps().steps.is_empty(), "not armed");
    cache.set_floor(0, None);
    append(&path, &[prompt("v1", 10, "new")]);
    let _ = cache.usage(Some(path.clone()), 0, &none);
    let evs: Vec<StepEvent> = cache.take_steps().steps.into_iter().map(|s| s.ev).collect();
    let len0 = prompt("v0", 0, "old").len() as u64 + 1;
    assert_eq!(
        evs,
        vec![
            StepEvent::Bind { len: len0 },
            StepEvent::Prompt { vref: id("v1") }
        ]
    );
}
