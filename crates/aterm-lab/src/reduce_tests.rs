// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab reduce` over every `supervise/fixtures` transcript.

use std::collections::HashSet;

use aterm_agent::harness::steps::StepEvent;

use super::oracle::{Arg, Args, call_ids, check_no_text, check_tool_arguments};
use super::*;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-agent/src/supervise/fixtures"
);

fn fixtures() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(FIXTURES)
        .expect("the fixtures directory")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("claude-") && n.contains("-transcript-") && n.ends_with(".jsonl"))
        .map(|n| {
            let text = std::fs::read_to_string(format!("{FIXTURES}/{n}")).expect("a fixture");
            (n, text)
        })
        .collect();
    out.sort();
    assert!(out.len() >= 20, "the transcript fixtures: {}", out.len());
    out
}

fn check_folds(name: &str, source: &str, r: &Reduction) -> Result<(), String> {
    reads_as(
        &readings(source, Program::Claude),
        &readings(&r.text, Program::Claude),
        r.shift_days,
    )
    .map_err(|e| format!("{name}: {e}"))
}

fn failures(
    rules: &dyn Rules,
    check: &dyn Fn(&str, &str, &Reduction) -> Result<(), String>,
) -> Vec<String> {
    fixtures()
        .iter()
        .filter_map(|(n, text)| {
            let r = reduce_with(text, Program::Claude, rules).expect("a reduction");
            check(n, text, &r).err()
        })
        .collect()
}

fn folds(n: &str, t: &str, r: &Reduction) -> Result<(), String> {
    check_folds(n, t, r)
}

fn no_text(n: &str, t: &str, r: &Reduction) -> Result<(), String> {
    check_no_text(n, t, r, Program::Claude)
}

#[test]
fn a_reduction_folds_as_its_source() {
    let bad = failures(&P11, &folds);
    assert!(bad.is_empty(), "{bad:#?}");
    let (mut walls, mut notes, mut tools, mut ends) = (0, 0, 0, 0);
    for (_, text) in fixtures() {
        let f = readings(&text, Program::Claude);
        let count = |p: fn(&StepEvent) -> bool| f.steps.iter().filter(|s| p(&s.ev)).count();
        walls += count(|e| matches!(e, StepEvent::EndWall { .. }));
        tools += count(|e| matches!(e, StepEvent::ToolUse { .. }));
        ends += count(|e| matches!(e, StepEvent::TurnEnd { .. }));
        notes += f.notices.len();
    }
    assert!(
        walls >= 10 && notes >= 15 && tools >= 5 && ends >= 10,
        "{walls} {notes} {tools} {ends}"
    );
}

#[test]
fn a_reduction_keeps_no_text() {
    let bad = failures(&P11, &no_text);
    assert!(bad.is_empty(), "{bad:#?}");
}

const CLAUDE_PLANT: [&str; 14] = [
    r#"{"type":"user","timestamp":"2026-10-16T10:00:00.000Z","uuid":"11111111-1111-4111-8111-111111111111","message":{"role":"user","content":"hello"},"version":"2.1.303"}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:01.000Z","uuid":"21111111-1111-4111-8111-111111111111","message":{"id":"msg_01","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_01","name":"mcp__quokkadb__lookup","input":{"quokkaledgerInputKey":"x","customers":{"quokkaledgerCustomer":{"status":"open"}}}}]}}"#,
    r#"{"type":"user","timestamp":"2026-10-16T10:00:02.000Z","uuid":"31111111-1111-4111-8111-111111111111","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"x"}]},"toolUseResult":{"answers":{"quokkaledgerAnswer":"yes"}}}"#,
    r#"{"type":"system","timestamp":"2026-10-16T10:00:03.000Z quokkaledger tail","uuid":"41111111-1111-4111-8111-111111111111","content":"x"}"#,
    r#"{"type":"attachment","timestamp":"2026-10-16T10:00:04.000Z","uuid":"51111111-1111-4111-8111-111111111111","attachment":{"type":"todo","mode":"quokkaledgerMode","items":{"quokkaledgerTodo":{"status":"open"}}}}"#,
    r#"{"type":"attachment","timestamp":"2026-10-16T10:00:05.000Z","uuid":"61111111-1111-4111-8111-111111111111","attachment":{"type":"queued_command","prompt":"quokkaledger words","origin":{"kind":"human"}}}"#,
    r#"{"type":"api-request-shape","sessionId":"71111111-1111-4111-8111-111111111111","shapeHash":"x","timestamp":"2026-10-16T10:00:06.000Z","version":"2.1.303","shape":{"system":[{"type":"text","text":"x"}],"tools":[{"name":"mcp__quokkadb__lookup","description":"x","input_schema":{"type":"object","properties":{"quokkaledgerAccountKey":{"description":"y","type":"string"}},"required":["quokkaledgerAccountKey"]}}]}}"#,
    r#"{"type":"progress","timestamp":"2026-10-16T10:00:07.000Z","uuid":"81111111-1111-4111-8111-111111111111","data":{"type":"agent_progress","message":{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_02","content":"x"}]},"toolUseResult":{"answers":{"quokkaledgerNestedAnswer":"yes"}}}}}"#,
    r#"{"type":"user","timestamp":"2026-10-16T10:00:08.000Z","uuid":"91111111-1111-4111-8111-111111111111","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"x"}]},"mcpMeta":{"structuredContent":{"quokkaledgerStructKey":"v"}}}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:09.000Z","uuid":"a1111111-1111-4111-8111-111111111111","message":{"id":"msg_02","role":"assistant","model":"claude-opus-5-5","content":[{"type":"server_tool_use","id":"srvtoolu_01","name":"web_search","input":{"quokkaledgerQuery":"x"}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_01","content":{"quokkaledgerResultKey":"x"}}]}}"#,
    r#"{"type":"file-history-snapshot","messageId":"b1111111-1111-4111-8111-111111111111","snapshot":{"messageId":"b1111111-1111-4111-8111-111111111111","trackedFileBackups":{"quokkaledgerNotes":{"backupFileName":"x@v1","version":1,"backupTime":"2026-10-16T10:00:10.000Z"}},"timestamp":"2026-10-16T10:00:10.000Z"},"isSnapshotUpdate":false}"#,
    r##"{"type":"api-request-blob","sessionId":"c1111111-1111-4111-8111-111111111111","hash":"x","message":{"role":"system","content":[{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"mcp__quokkadb__lookup","description":"x","input_schema":{"type":"object","properties":{"quokkaledgerBlobArg":{"type":"string","description":"y"}},"required":["quokkaledgerBlobArg"]}}}},{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"mcp__quokkadb__find","description":"x","input_schema":{"$ref":"#/$defs/quokkaledgerDefName","$defs":{"quokkaledgerDefName":{"type":"object","properties":{"quokkaledgerDefArg":{"type":"string"}}}}}}}},{"type":"tool_removal","tool":{"type":"tool_reference","name":"mcp__quokkadb__lookup"}}]}}"##,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:12.000Z","uuid":"d1111111-1111-4111-8111-111111111111","wireIngestContext":{"toolu_01quokkaledgerCallId":{"cwd":"/private/tmp/x"}},"message":{"id":"msg_03","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_01quokkaledgerCallId","name":"mcp__quokkadb__lookup","input":{"quokkaledgerWireKey":"x","mode":"quokkaledgerModeWord"}}]},"wireToolInputs":{"toolu_01quokkaledgerCallId":{"quokkaledgerWireKey":"x","mode":"quokkaledgerModeWord"}}}"#,
    r#"{"type":"tool-list","timestamp":"2026-10-16T10:00:13.000Z","uuid":"e1111111-1111-4111-8111-111111111111","tools":[{"name":"x","parameters":{"type":"object","properties":{"quokkaledgerParamArg":{"type":"string"}}}}]}"#,
];

const CLAUDE_PLANT_KEPT: [(usize, &str); 12] = [
    (
        1,
        r#"{"type":"assistant","timestamp":"2000-01-01T10:00:01.000Z","uuid":"00000000-0000-4000-8000-000000000002","message":{"id":"msg_r0003","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_r0004","name":"mcp__tool1","input":{"quokkaledgerInputKey":"","customers":{"quokkaledgerCustomer":{"status":""}}}}]}}"#,
    ),
    (
        2,
        r#"{"type":"user","timestamp":"2000-01-01T10:00:02.000Z","uuid":"00000000-0000-4000-8000-000000000002","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_r0003","content":""}]},"toolUseResult":{"answers":{"quokkaledgerAnswer":""}}}"#,
    ),
    (
        3,
        r#"{"type":"system","timestamp":"2000-01-01T10:00:03.000Z quokkaledger tail","uuid":"00000000-0000-4000-8000-000000000002","content":""}"#,
    ),
    (
        4,
        r#"{"type":"attachment","timestamp":"2000-01-01T10:00:04.000Z","uuid":"00000000-0000-4000-8000-000000000002","attachment":{"type":"todo","mode":"quokkaledgerMode","items":{"quokkaledgerTodo":{"status":"open"}}}}"#,
    ),
    (
        6,
        r#"{"type":"api-request-shape","sessionId":"00000000-0000-4000-8000-000000000002","shapeHash":"","timestamp":"2000-01-01T10:00:06.000Z","version":"2.1.303","shape":{"system":[{"type":"text","text":""}],"tools":[{"name":"","description":"","input_schema":{"type":"object","properties":{"quokkaledgerAccountKey":{"description":"","type":"string"}},"required":[""]}}]}}"#,
    ),
    (
        7,
        r#"{"type":"progress","timestamp":"2000-01-01T10:00:07.000Z","uuid":"00000000-0000-4000-8000-000000000002","data":{"type":"agent_progress","message":{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_r0003","content":""}]},"toolUseResult":{"answers":{"quokkaledgerNestedAnswer":""}}}}}"#,
    ),
    (
        8,
        r#"{"type":"user","timestamp":"2000-01-01T10:00:08.000Z","uuid":"00000000-0000-4000-8000-000000000002","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_r0003","content":""}]},"mcpMeta":{"structuredContent":{"quokkaledgerStructKey":""}}}"#,
    ),
    (
        9,
        r#"{"type":"assistant","timestamp":"2000-01-01T10:00:09.000Z","uuid":"00000000-0000-4000-8000-000000000002","message":{"id":"msg_r0003","role":"assistant","model":"claude-opus-5-5","content":[{"type":"server_tool_use","id":"srvtoolu_r0004","name":"","input":{"quokkaledgerQuery":""}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_r0004","content":{"quokkaledgerResultKey":""}}]}}"#,
    ),
    (
        10,
        r#"{"type":"file-history-snapshot","messageId":"00000000-0000-4000-8000-000000000002","snapshot":{"messageId":"00000000-0000-4000-8000-000000000002","trackedFileBackups":{"quokkaledgerNotes":{"backupFileName":"","version":1,"backupTime":""}},"timestamp":"2000-01-01T10:00:10.000Z"},"isSnapshotUpdate":false}"#,
    ),
    (
        11,
        r#"{"type":"api-request-blob","sessionId":"00000000-0000-4000-8000-000000000002","hash":"","message":{"role":"system","content":[{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"","description":"","input_schema":{"type":"object","properties":{"quokkaledgerBlobArg":{"type":"string","description":""}},"required":[""]}}}},{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"","description":"","input_schema":{"k1":"","k2":{"quokkaledgerDefName":{"type":"object","properties":{"quokkaledgerDefArg":{"type":"string"}}}}}}}},{"type":"tool_removal","tool":{"type":"tool_reference","name":""}}]}}"#,
    ),
    (
        12,
        r#"{"type":"assistant","timestamp":"2000-01-01T10:00:12.000Z","uuid":"00000000-0000-4000-8000-000000000002","wireIngestContext":{"toolu_01quokkaledgerCallId":{"cwd":""}},"message":{"id":"msg_r0003","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_r0004","name":"mcp__tool1","input":{"k1":"","k2":""}}]},"wireToolInputs":{"toolu_01quokkaledgerCallId":{"quokkaledgerWireKey":"","mode":"quokkaledgerModeWord"}}}"#,
    ),
    (
        13,
        r#"{"type":"tool-list","timestamp":"2000-01-01T10:00:13.000Z","uuid":"00000000-0000-4000-8000-000000000002","tools":[{"name":"","parameters":{"type":"object","properties":{"quokkaledgerParamArg":{"type":"string"}}}}]}"#,
    ),
];

#[test]
fn a_reduction_keeps_no_key_or_stamp_tail_anyone_wrote() {
    let src: String = CLAUDE_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let r = reduce(&src, Program::Claude).expect("reduced");
    check_no_text("plant", &src, &r, Program::Claude).expect("no text");
    assert!(!r.text.contains("quokka"), "{}", r.text);
    let rows: Vec<&str> = r.text.lines().collect();
    assert!(
        rows[2].contains(r#""input":{"k1":"","k2":{"k3":{"k4":""}}}"#),
        "{}",
        rows[2]
    );
    assert!(
        rows[3].contains(r#""toolUseResult":{"k5":{"k6":""}}"#),
        "{}",
        rows[3]
    );
    assert!(
        rows[1].contains(r#""timestamp":"2000-01-01T10:00:00.000Z""#)
            && rows[4].contains(r#""timestamp":"""#),
        "{}",
        r.text
    );
    assert!(
        rows[5].contains(r#""attachment":{"type":"todo","mode":"","items":{"k7":{"k4":""}}}"#),
        "{}",
        rows[5]
    );
    assert!(
        rows[6].contains(
            r#""attachment":{"type":"queued_command","prompt":"","origin":{"kind":"human"}}"#
        ),
        "{}",
        rows[6]
    );
    assert!(
        rows[7].contains(
            r#""shape":{"k8":[{"k9":"","k10":""}],"k11":[{"k12":"","k13":"","k14":{"k9":"","k15":{"k16":{"k13":"","k9":""}},"k17":[""]}}]}"#
        ),
        "{}",
        rows[7]
    );
    assert!(
        rows[8].contains(r#""toolUseResult":{"k5":{"k18":""}}"#),
        "{}",
        rows[8]
    );
    assert!(
        rows[9].contains(r#""mcpMeta":{"k19":{"k20":""}}"#),
        "{}",
        rows[9]
    );
    assert!(
        rows[10].contains(r#""input":{"k21":""}"#) && rows[10].contains(r#""content":{"k22":""}"#),
        "{}",
        rows[10]
    );
    assert!(
        rows[11].contains(r#""trackedFileBackups":{"k23":{"k24":"","k25":1,"k26":""}},"timestamp":"2000-01-01T10:00:10.000Z""#),
        "{}",
        rows[11]
    );
    assert!(
        rows[12].contains(r#""definition":{"name":"","description":"","input_schema":{"k9":"","k15":{"k27":{"k9":"","k13":""}},"k17":[""]}}"#)
            && rows[12].contains(r#""input_schema":{"k28":"","k29":{"k30":{"k9":"","k15":{"k31":{"k9":""}}}}}"#),
        "{}",
        rows[12]
    );
    assert!(
        rows[13].contains(r#""wireIngestContext":{"toolu_r0019":{"k32":""}}"#)
            && rows[13]
                .contains(r#""id":"toolu_r0019","name":"mcp__tool1","input":{"k33":"","k34":""}"#)
            && rows[13].contains(r#""wireToolInputs":{"toolu_r0019":{"k33":"","k34":""}}"#),
        "{}",
        rows[13]
    );
    assert!(
        rows[14]
            .contains(r#""tools":[{"name":"","parameters":{"k9":"","k15":{"k35":{"k9":""}}}}]"#),
        "{}",
        rows[14]
    );
    for (n, kept) in CLAUDE_PLANT_KEPT {
        let src = format!("{}\n{}\n", CLAUDE_PLANT[0], CLAUDE_PLANT[n]);
        let r = reduce(&src, Program::Claude).expect("reduced");
        let mut lines = r.text.lines();
        let (head, prompt) = (lines.next().expect("head"), lines.next().expect("prompt"));
        let earlier = Reduction {
            text: format!("{head}\n{prompt}\n{kept}\n"),
            ..r.clone()
        };
        assert!(kept.contains("quokkaledger"), "{n}: {kept}");
        let e = check_no_text("plant", &src, &earlier, Program::Claude)
            .expect_err("the earlier rule's reduction keeps the planted words");
        assert!(
            e.contains("quokkale")
                || e.contains("okkaledg")
                || e.contains("a key in a free subtree")
                || e.contains("is kept as written"),
            "{n}: {e}"
        );
    }
}

const STEPS_2_1_303: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-agent/src/harness/testdata/steps/2.1.303.jsonl"
);

const STEPS_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-agent/src/harness/testdata/steps"
);

/// Every committed Claude Code transcript reduction (`<build>.jsonl`, every `.jsonl` in the step
/// testdata that is no Codex rollout's), by name: what `MEASURED_BUILDS` lists.
fn steps_claude() -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(STEPS_DIR)
        .expect("the step testdata")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.starts_with("codex-") && n.ends_with(".jsonl"))
        })
        .collect();
    paths.sort();
    assert!(
        paths
            .iter()
            .any(|p| p.as_path() == std::path::Path::new(STEPS_2_1_303)),
        "{paths:?}"
    );
    paths
}

fn steps_codex() -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(STEPS_DIR)
        .expect("the step testdata")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("codex-") && n.ends_with(".jsonl"))
        })
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 2
            && paths
                .iter()
                .any(|p| p.to_string_lossy().contains("codex-alab-")),
        "{paths:?}"
    );
    paths
}

fn rows_of(text: &str) -> Vec<Node> {
    text.lines()
        .filter(|l| is_row(l))
        .map(|l| parse(l.trim()).expect("a row"))
        .collect()
}

#[test]
fn a_tool_argument_name_is_never_kept_as_written() {
    let keys_only = |_: &str, _: &str| Ok(());
    for path in steps_claude() {
        let name = path.display().to_string();
        let committed = std::fs::read_to_string(&path).expect("a committed reduction");
        let src = rows_of(&committed);
        let mut calls = HashSet::new();
        for row in &src {
            call_ids(row, &mut calls);
        }
        let args = Args {
            calls: &calls,
            check: &keys_only,
        };
        let kept: Vec<String> = src
            .iter()
            .filter_map(|r| check_tool_arguments(r, r, Arg::ROW, &args).err())
            .collect();
        assert!(
            kept.is_empty(),
            "{name}: every argument name renamed: {kept:#?}"
        );
        assert!(
            committed.contains(r#""shape":{"#) && committed.contains(r#""type":"tool_use""#),
            "{name}: the tool list and a call are committed"
        );
        let again = reduce(&committed, Program::Claude).expect("reduced");
        assert!(again.again, "{name}: its own line says it is a reduction");
        assert!(again.text == committed, "{name} is reduced as it was");
        check_no_text(&name, &committed, &again, Program::Claude).expect("no text");
    }
    for path in steps_codex() {
        let text = std::fs::read_to_string(&path).expect("a committed reduction");
        let again = reduce(&text, Program::Codex).expect("reduced");
        let name = path.display().to_string();
        assert!(again.text == text, "{name} is reduced as it was");
        check_no_text(&name, &text, &again, Program::Codex).expect("no text");
    }
    let earlier = |n: usize, edit: Option<(&str, &str)>| {
        let (_, kept) = CLAUDE_PLANT_KEPT
            .iter()
            .find(|(m, _)| *m == n)
            .expect("an earlier reduction");
        let kept = edit.map_or_else(|| (*kept).to_string(), |(f, t)| kept.replace(f, t));
        (
            parse(CLAUDE_PLANT[n]).expect("a row"),
            parse(&kept).expect("a row"),
        )
    };
    let cwd = (r#""cwd":"""#, r#""k5":"""#);
    for (n, word, edit) in [
        (6, "quokkaledgerAccountKey", None),
        (11, "quokkaledgerBlobArg", None),
        (
            11,
            "quokkaledgerDefName",
            Some((r#""quokkaledgerBlobArg":{"#, r#""k27":{"#)),
        ),
        (12, "cwd", None),
        (12, "quokkaledgerWireKey", Some(cwd)),
        (13, "quokkaledgerParamArg", None),
    ] {
        let (a, b) = earlier(n, edit);
        let mut calls = HashSet::new();
        call_ids(&a, &mut calls);
        let args = Args {
            calls: &calls,
            check: &keys_only,
        };
        let e = check_tool_arguments(&a, &b, Arg::ROW, &args)
            .expect_err("the earlier rule kept the MCP tool's argument name");
        assert!(e.contains(&format!("{word:?}")), "{n}: {e}");
    }
    let src = format!("{}\n{}\n", CLAUDE_PLANT[0], CLAUDE_PLANT[12]);
    let r = reduce(&src, Program::Claude).expect("reduced");
    let (_, kept) = CLAUDE_PLANT_KEPT[10];
    let renamed_but_word = kept
        .replace(cwd.0, cwd.1)
        .replace(r#""quokkaledgerWireKey":"""#, r#""k1":"""#)
        .replace(
            r#""mode":"quokkaledgerModeWord""#,
            r#""k2":"quokkaledgerModeWord""#,
        );
    assert!(
        !renamed_but_word.contains("Key") && renamed_but_word.contains("ModeWord"),
        "{renamed_but_word}"
    );
    let mut lines = r.text.lines();
    let (head, prompt) = (lines.next().expect("head"), lines.next().expect("prompt"));
    let word = Reduction {
        text: format!("{head}\n{prompt}\n{renamed_but_word}\n"),
        ..r.clone()
    };
    let e = check_no_text("plant", &src, &word, Program::Claude)
        .expect_err("the argument check reads the word");
    assert!(
        e.contains("a tool's argument keeps") && e.contains("quokkale"),
        "{e}"
    );
    let renamed_and_empty = renamed_but_word.replace("quokkaledgerModeWord", "");
    let id = Reduction {
        text: format!("{head}\n{prompt}\n{renamed_and_empty}\n"),
        ..r.clone()
    };
    let e = check_no_text("plant", &src, &id, Program::Claude).expect_err("the id kept as a key");
    assert!(
        e.contains(
            r#""toolu_01quokkaledgerCallId" is remapped as a value but kept as written as a key"#
        ),
        "{e}"
    );
}

const CODEX_PLANT: [&str; 4] = [
    r#"{"timestamp":"2026-10-16T10:00:00.000Z","type":"session_meta","payload":{"id":"0199a0b1-0000-7000-8000-000000000abc","cwd":"/private/tmp/x/proj","cli_version":"0.163.0"}}"#,
    r#"{"timestamp":"2026-10-16T10:00:01.000Z","type":"event_msg","payload":{"type":"exec_command_begin","call_id":"call_abc","command":["bash","-lc","cat quokkaledger-plan.md"],"parsed_cmd":[{"type":"read","cmd":"cat quokkaledger-plan.md","name":"quokkaledger-plan.md","path":"/private/tmp/x/quokkaledger-plan.md"}]}}"#,
    r#"{"timestamp":"2026-10-16T10:00:02.000Z","type":"response_item","payload":{"type":"function_call","namespace":"collaboration","name":"spawn_agent","arguments":"{\"message\":\"say hi\"}","call_id":"call_x"}}"#,
    r#"{"timestamp":"2026-10-16T10:00:03.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","started_at_ms":1792144800123}}"#,
];

struct CodexNamesAnywhere;

impl Rules for CodexNamesAnywhere {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        if at.program == Program::Codex && !at.free && at.key == "name" {
            Treat::Tool
        } else {
            P11.string(at, s)
        }
    }
}

#[test]
fn a_codex_reduction_keeps_no_text() {
    let src: String = CODEX_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let r = reduce(&src, Program::Codex).expect("reduced");
    check_no_text("codex", &src, &r, Program::Codex).expect("no text");
    assert!(!r.text.contains("quokka"), "{}", r.text);
    let rows: Vec<&str> = r.text.lines().collect();
    assert!(
        rows[2].contains(r#""parsed_cmd":[{"type":"","cmd":"","name":"","path":""}]"#),
        "{}",
        rows[2]
    );
    assert!(
        rows[3].contains(r#""namespace":"collaboration","name":"spawn_agent","arguments":"""#),
        "{}",
        rows[3]
    );
    let kept = reduce_with(&src, Program::Codex, &CodexNamesAnywhere).expect("reduced");
    let e = check_no_text("codex", &src, &kept, Program::Codex)
        .expect_err("a file's name kept as a tool's");
    assert!(e.contains("quokkale") || e.contains("okkaledg"), "{e}");
}

struct KeepsContent;

impl Rules for KeepsContent {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        if matches!(at.key, "content" | "text") {
            Treat::Keep
        } else {
            P11.string(at, s)
        }
    }
}

struct ReplacesType;

impl Rules for ReplacesType {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        if at.key == "type" {
            Treat::Replace
        } else {
            P11.string(at, s)
        }
    }

    fn words(&self, c: &Candidates<'_>) -> Result<Vec<bool>, String> {
        Ok(c.every())
    }
}

struct BlanksNotices;

impl Rules for BlanksNotices {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        match P11.string(at, s) {
            Treat::Notice => Treat::Replace,
            t => t,
        }
    }

    fn words(&self, c: &Candidates<'_>) -> Result<Vec<bool>, String> {
        Ok(c.every())
    }
}

struct KeepsEveryCandidate;

impl Rules for KeepsEveryCandidate {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        P11.string(at, s)
    }

    fn words(&self, c: &Candidates<'_>) -> Result<Vec<bool>, String> {
        Ok(c.every())
    }
}

struct BlanksRead(&'static [&'static str]);

impl Rules for BlanksRead {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        P11.string(at, s)
    }

    fn words(&self, c: &Candidates<'_>) -> Result<Vec<bool>, String> {
        let mut keep = c.by_readings()?;
        for (k, (key, _)) in keep.iter_mut().zip(c.pairs()) {
            *k &= !self.0.contains(&key.as_str());
        }
        Ok(keep)
    }
}

struct ReplacesSubtype;

impl Rules for ReplacesSubtype {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        if at.key == "subtype" {
            Treat::Replace
        } else {
            P11.string(at, s)
        }
    }
}

#[test]
fn a_reducer_that_keeps_content_fails_the_text_check() {
    let bad = failures(&KeepsContent, &no_text);
    assert!(bad.len() >= 20, "{} of 24 failed: {bad:#?}", bad.len());
    assert!(
        failures(&KeepsContent, &folds).is_empty(),
        "it still folds as its source"
    );
}

#[test]
fn a_reducer_that_replaces_type_fails_the_fold_check() {
    let bad = failures(&ReplacesType, &folds);
    assert!(bad.len() >= 20, "{} of 24 failed: {bad:#?}", bad.len());
}

#[test]
fn a_reducer_that_blanks_notice_text_fails_the_wall_check() {
    let bad = failures(&BlanksNotices, &folds);
    let with = fixtures()
        .iter()
        .filter(|(_, t)| t.contains("\"isApiErrorMessage\":true"))
        .count();
    assert!(
        with >= 15 && bad.len() == with,
        "{} of {with}: {bad:#?}",
        bad.len()
    );
}

const WORDS_PLANT: [&str; 4] = [
    r#"{"type":"user","timestamp":"2026-10-16T10:00:00.000Z","uuid":"11111111-1111-4111-8111-111111111111","promptId":"12111111-1111-4111-8111-111111111111","message":{"role":"user","content":"hello"},"version":"2.1.303"}"#,
    r#"{"type":"mode","mode":"quokkaledger","sessionId":"71111111-1111-4111-8111-111111111111"}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:01.000Z","uuid":"21111111-1111-4111-8111-111111111111","parentUuid":"11111111-1111-4111-8111-111111111111","message":{"id":"msg_01","role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn"},"version":"2.1.303"}"#,
    r#"{"type":"system","subtype":"turn_duration","durationMs":1200,"timestamp":"2026-10-16T10:00:02.000Z","uuid":"31111111-1111-4111-8111-111111111111","parentUuid":"21111111-1111-4111-8111-111111111111","version":"2.1.303"}"#,
];

#[test]
fn a_closed_word_is_kept_only_where_a_reader_reads_it() {
    let src: String = WORDS_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let r = reduce(&src, Program::Claude).expect("reduced");
    let rows: Vec<&str> = r.text.lines().collect();
    assert_eq!(
        rows[2], r#"{"type":"mode","mode":"","sessionId":"00000000-0000-4000-8000-000000000003"}"#,
        "{}",
        r.text
    );
    assert!(
        rows[4].contains(r#""subtype":"turn_duration""#)
            && rows[3].contains(r#""stop_reason":"end_turn""#),
        "{}",
        r.text
    );
    assert!(!r.text.contains("quokka"), "{}", r.text);
    check_folds("plant", &src, &r).expect("it folds as its source");
    check_no_text("plant", &src, &r, Program::Claude).expect("no text");
    assert_eq!(unread_words(&r.text, Program::Claude), Ok(vec![]));
    assert!(r.words.1 < r.words.0, "{:?}", r.words);
    let every = reduce_with(&src, Program::Claude, &KeepsEveryCandidate).expect("reduced");
    assert!(
        every.text.contains(r#""mode":"quokkaledger""#),
        "{}",
        every.text
    );
    let unread = unread_words(&every.text, Program::Claude).expect("read");
    assert!(
        unread.contains(&("mode".to_string(), "quokkaledger".to_string())),
        "{unread:?}"
    );
    let blanked = reduce_with(&src, Program::Claude, &BlanksRead(&["subtype"])).expect("reduced");
    assert!(blanked.text.contains(r#""subtype":"""#), "{}", blanked.text);
    let e = check_folds("plant", &src, &blanked).expect_err("its turn end is gone");
    assert!(e.contains("steps became"), "{e}");
}

#[test]
fn every_closed_word_a_reduction_keeps_is_read() {
    let mut kept = 0;
    for (n, text) in fixtures() {
        let r = reduce(&text, Program::Claude).expect("reduced");
        assert_eq!(unread_words(&r.text, Program::Claude), Ok(vec![]), "{n}");
        kept += r.words.1;
    }
    let src: String = CODEX_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let r = reduce(&src, Program::Codex).expect("reduced");
    assert_eq!(unread_words(&r.text, Program::Codex), Ok(vec![]));
    assert!(kept >= 100, "{kept} words kept: the check is not vacuous");
    let unread = fixtures()
        .iter()
        .filter(|(_, text)| {
            let r = reduce_with(text, Program::Claude, &KeepsEveryCandidate).expect("reduced");
            !unread_words(&r.text, Program::Claude)
                .expect("read")
                .is_empty()
        })
        .count();
    assert_eq!(
        unread,
        fixtures().len(),
        "every fixture keeps an unread word"
    );
}

#[test]
fn a_reducer_that_blanks_a_read_word_fails_the_fold_check() {
    let with = fixtures()
        .iter()
        .filter(|(_, text)| {
            let r = reduce(text, Program::Claude).expect("reduced");
            r.text
                .match_indices(r#""subtype":""#)
                .any(|(i, m)| !r.text[i + m.len()..].starts_with('"'))
        })
        .count();
    let bad = failures(&BlanksRead(&["subtype"]), &folds);
    assert!(
        with >= 10 && bad.len() == with,
        "{} of {with}: {bad:#?}",
        bad.len()
    );
}

#[test]
fn a_reduction_nothing_restores_is_refused() {
    let src: String = WORDS_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let e = reduce_with(&src, Program::Claude, &ReplacesSubtype).expect_err("refused");
    assert!(e.contains("refused") && e.contains("steps became"), "{e}");
    assert!(reduce(&src, Program::Claude).is_ok());
}

#[test]
fn a_reduction_reduced_again_is_itself() {
    for (n, text) in fixtures() {
        let once = reduce(&text, Program::Claude).expect("reduced");
        let twice = reduce(&once.text, Program::Claude).expect("reduced again");
        let body = |t: &str| t.lines().skip(1).map(str::to_string).collect::<Vec<_>>();
        assert_eq!(body(&twice.text), body(&once.text), "{n}");
        assert!(twice.again && !once.again, "{n}");
        assert_eq!(
            (twice.shift_days, twice.moved_days),
            (0, once.moved_days),
            "{n}"
        );
        let thrice = reduce(&twice.text, Program::Claude).expect("reduced a third time");
        assert_eq!(thrice.text, twice.text, "{n}");
        assert!(
            !once
                .text
                .lines()
                .next()
                .unwrap_or_default()
                .contains("reduced again")
        );
    }
    let src: String = CODEX_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let once = reduce(&src, Program::Codex).expect("reduced");
    let twice = reduce(&once.text, Program::Codex).expect("reduced again");
    assert!(twice.again && twice.text.starts_with("# codex 0.163.0 "));
    assert!(twice.text.lines().skip(1).eq(once.text.lines().skip(1)));
    assert_eq!(
        reduce(&twice.text, Program::Codex).expect("again").text,
        twice.text
    );
}

#[test]
fn every_canonical_phrase_is_the_first_of_its_reading() {
    let ps = phrases();
    for (i, p) in ps.iter().enumerate() {
        let r = Reading::of(p);
        assert!(
            ps[..i].iter().all(|q| Reading::of(q) != r),
            "{p:?} reads as an earlier phrase ({r:?})"
        );
        assert_eq!(
            canonical(p, 0, 0, 2026).as_deref(),
            Ok(*p),
            "{p:?} is its own phrase"
        );
    }
    assert_eq!(
        canonical("API Error: 503 upstream connect error", 0, 0, 2026).as_deref(),
        Ok("API Error: 503")
    );
}

#[test]
fn a_reset_clause_is_rebased_by_whole_days() {
    let shift = days_from_civil(2000, 1, 1) - days_from_civil(2026, 10, 8);
    assert_eq!(
        reset_clause(
            "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
            shift,
            2026
        )
        .as_deref(),
        Some(" · resets 11:33am")
    );
    assert_eq!(
        reset_clause(
            "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
            shift,
            2026
        )
        .as_deref(),
        Some(" · resets Jan 4 at 9:35am")
    );
    assert_eq!(reset_clause("resets soon", shift, 2026), None);
    assert_eq!(
        canonical(
            "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
            0,
            shift,
            2026
        )
        .as_deref(),
        Ok("You've hit your weekly limit · resets Jan 4 at 9:35am")
    );
}

#[test]
fn the_tree_keeps_order_numbers_and_escapes() {
    let src = r#"{"z":1,"a":[1.50,-0,2e3,{"q":null,"b":true}],"s":"a\"b\\c\n\t\u0001😀é/"}"#;
    let v = parse(src).expect("json");
    assert_eq!(
        v.dump(),
        "{\"z\":1,\"a\":[1.50,-0,2e3,{\"q\":null,\"b\":true}],\"s\":\"a\\\"b\\\\c\\n\\t\\u0001\u{1f600}é/\"}"
    );
    for bad in [
        "{",
        "[1,]",
        "{\"a\" 1}",
        "01",
        "\"x",
        "1 2",
        "{\"a\":\"\u{1}\"}",
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn ids_are_remapped_one_to_one_their_validity_kept() {
    let src = concat!(
        r#"{"type":"assistant","uuid":"a1b2c3d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d","timestamp":"2026-10-08T16:34:58.921Z","message":{"id":"msg_01abc","content":[{"type":"tool_use","id":"toolu_lab1","name":"mcp__notes__find","input":{"q":"secretword"}},{"type":"tool_use","id":"an id with spaces","name":"Bash","input":{}}]}}"#,
        "\n",
        r#"{"type":"user","parentUuid":"a1b2c3d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d","timestamp":"2026-10-08T16:35:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_lab1","content":"secretword found"}]}}"#,
        "\n"
    );
    let r = reduce(src, Program::Claude).expect("reduced");
    let rows: Vec<&str> = r.text.lines().collect();
    assert_eq!(rows.len(), 3, "{}", r.text);
    assert!(
        rows[0].starts_with("# claude - · lab reduce"),
        "{}",
        rows[0]
    );
    assert_eq!(
        rows[1],
        r#"{"type":"assistant","uuid":"00000000-0000-4000-8000-000000000001","timestamp":"2000-01-01T16:34:58.921Z","message":{"id":"msg_r0002","content":[{"type":"tool_use","id":"toolu_r0003","name":"mcp__tool1","input":{"k1":""}},{"type":"tool_use","id":"r0004/","name":"Bash","input":{}}]}}"#
    );
    assert_eq!(
        rows[2],
        r#"{"type":"user","parentUuid":"00000000-0000-4000-8000-000000000001","timestamp":"2000-01-01T16:35:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_r0003","content":""}]}}"#
    );
    assert!(!r.text.contains("secretword"));
    assert_eq!(
        (r.rows, r.ids, r.shift_days),
        (
            2,
            4,
            days_from_civil(2000, 1, 1) - days_from_civil(2026, 10, 8)
        )
    );
}

#[test]
fn a_user_row_keeps_its_marker_and_nothing_past_it() {
    let row = |content: &str| {
        let src = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":{}}}}}\n",
            Node::Str(content.to_string()).dump()
        );
        let r = reduce(&src, Program::Claude).expect("reduced");
        let line = r.text.lines().nth(1).expect("a row").to_string();
        let v = parse(&line).expect("json");
        v.get("message")
            .and_then(|m| m.get("content"))
            .and_then(Node::as_str)
            .expect("content")
            .to_string()
    };
    assert_eq!(
        row("<task-notification>\n<task-id>b1</task-id>"),
        "<task-notification>"
    );
    assert_eq!(
        row("<bash-input>cat notes.txt</bash-input>"),
        "<bash-input>"
    );
    assert_eq!(
        row("[Request interrupted by user for tool use]"),
        "[Request interrupted by user"
    );
    assert_eq!(row("/compact keep the notes"), "/compact");
    assert_eq!(row("/compactor"), "");
    assert_eq!(row("seventh try"), "");
    let sys = "{\"type\":\"system\",\"subtype\":\"local_command\",\"content\":\"<local-command-stderr>Compaction failed · conversation could not be reduced below the context limit</local-command-stderr>\",\"files\":{\"/private/tmp/x/notes.txt\":1}}\n";
    let r = reduce(sys, Program::Claude).expect("reduced");
    assert_eq!(
        r.text.lines().nth(1),
        Some(
            "{\"type\":\"system\",\"subtype\":\"local_command\",\"content\":\"<local-command-stderr>Context limit reached</local-command-stderr>\",\"files\":{\"k1\":1}}"
        )
    );
}

#[test]
fn a_codex_rollout_keeps_words_ids_and_numbers_only() {
    let src = concat!(
        r#"{"timestamp":"2026-10-16T10:00:00.000Z","type":"session_meta","payload":{"id":"0199a0b1-0000-7000-8000-000000000abc","cwd":"/private/tmp/x/proj","cli_version":"0.163.0","instructions":"be brief please"}}"#,
        "\n",
        r#"{"timestamp":"2026-10-16T10:00:01.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"touch zebrafile\"}","call_id":"call_abc"}}"#,
        "\n",
        r#"{"timestamp":"2026-10-16T10:00:02.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120}},"rate_limits":{"primary":{"used_percent":40.0,"resets_at":1792144800}}}}"#,
        "\n"
    );
    let r = reduce(src, Program::Codex).expect("reduced");
    let shift = days_from_civil(2000, 1, 1) - days_from_civil(2026, 10, 16);
    let rows: Vec<&str> = r.text.lines().collect();
    assert!(
        rows[0].starts_with("# codex 0.163.0 · lab reduce"),
        "{}",
        rows[0]
    );
    assert_eq!(
        rows[1],
        r#"{"timestamp":"2000-01-01T10:00:00.000Z","type":"session_meta","payload":{"id":"00000000-0000-4000-8000-000000000001","cwd":"","cli_version":"0.163.0","instructions":""}}"#
    );
    assert_eq!(
        rows[2],
        r#"{"timestamp":"2000-01-01T10:00:01.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"","call_id":"call_r0002"}}"#
    );
    assert_eq!(
        rows[3],
        format!(
            r#"{{"timestamp":"2000-01-01T10:00:02.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":120}}}},"rate_limits":{{"primary":{{"used_percent":40.0,"resets_at":{}}}}}}}}}"#,
            1_792_144_800 + shift * 86_400
        )
    );
    for word in ["zebrafile", "be brief", "/private/tmp/x"] {
        assert!(!r.text.contains(word), "{word}");
    }
}

#[test]
fn a_codex_rollout_keeps_its_namespace_and_moves_its_millisecond_times() {
    let src = concat!(
        r#"{"timestamp":"2026-10-19T12:00:00.000Z","type":"session_meta","payload":{"id":"0199a0b1-0000-7000-8000-000000000abc","cli_version":"0.163.0"}}"#,
        "\n",
        r#"{"timestamp":"2026-10-19T12:00:00.010Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","arguments":"{\"message\":\"say hi pelicanhinge\"}","call_id":"call_lab0004"}}"#,
        "\n",
        r#"{"timestamp":"2026-10-19T12:00:00.020Z","type":"event_msg","payload":{"type":"item_completed","started_at_ms":1792411200015,"item":{"type":"AgentMessage","content":[{"type":"Text","text":"hello pelicanhinge"}]}}}"#,
        "\n",
        r#"{"timestamp":"2026-10-19T12:00:00.030Z","type":"compacted","payload":{"replacement_history":[{"type":"message","role":"user","internal_chat_message_metadata_passthrough":{"create_time":1792411200.025125}}]}}"#,
        "\n",
        r#"{"timestamp":"2026-10-19T12:00:01.000Z","type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted","duration_ms":4311}}"#,
        "\n"
    );
    let r = reduce(src, Program::Codex).expect("reduced");
    let day_s = (days_from_civil(2000, 1, 1) - days_from_civil(2026, 10, 19)) * 86_400;
    let rows: Vec<&str> = r.text.lines().collect();
    assert_eq!(
        rows[1],
        r#"{"timestamp":"2000-01-01T12:00:00.000Z","type":"session_meta","payload":{"id":"00000000-0000-4000-8000-000000000001","cli_version":"0.163.0"}}"#
    );
    assert_eq!(
        rows[2],
        r#"{"timestamp":"2000-01-01T12:00:00.010Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","arguments":"","call_id":"call_r0002"}}"#
    );
    assert_eq!(
        rows[3],
        format!(
            r#"{{"timestamp":"2000-01-01T12:00:00.020Z","type":"event_msg","payload":{{"type":"item_completed","started_at_ms":{},"item":{{"type":"","content":[{{"type":"","text":""}}]}}}}}}"#,
            1_792_411_200_015 + day_s * 1000
        )
    );
    assert_eq!(
        rows[4],
        format!(
            r#"{{"timestamp":"2000-01-01T12:00:00.030Z","type":"compacted","payload":{{"replacement_history":[{{"type":"","role":"","internal_chat_message_metadata_passthrough":{{"create_time":{}.025125}}}}]}}}}"#,
            1_792_411_200 + day_s
        )
    );
    assert_eq!(
        rows[5],
        r#"{"timestamp":"2000-01-01T12:00:01.000Z","type":"event_msg","payload":{"type":"turn_aborted","reason":"","duration_ms":4311}}"#
    );
    for epoch in ["1792411200", "pelicanhinge"] {
        assert!(!r.text.contains(epoch), "{epoch}");
    }
}

#[test]
fn a_line_that_is_no_json_refuses_the_file() {
    let e = reduce("{\"type\":\"user\"}\n{\"type\":", Program::Claude).expect_err("refused");
    assert!(e.starts_with("line 2: not one JSON value"), "{e}");
}

#[test]
fn the_verb_never_writes_over_a_file() {
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let src = dir.path().join("t.jsonl");
    let out = dir.path().join("r.jsonl");
    std::fs::write(
        &src,
        "{\"type\":\"user\",\"message\":{\"content\":\"hello there friend\"}}\n",
    )
    .expect("source");
    std::fs::write(&out, "kept\n").expect("an existing file");
    let args = |o: &std::path::Path| {
        vec![
            src.to_string_lossy().into_owned(),
            "--program".to_string(),
            "claude".to_string(),
            "--out".to_string(),
            o.to_string_lossy().into_owned(),
        ]
    };
    let e = cmd_reduce(&args(&out)).expect_err("refused");
    assert_eq!(e.code, 1);
    assert_eq!(std::fs::read_to_string(&out).expect("kept"), "kept\n");
    let fresh = dir.path().join("fresh.jsonl");
    assert_eq!(cmd_reduce(&args(&fresh)).expect("written"), 0);
    let text = std::fs::read_to_string(&fresh).expect("written");
    assert!(
        text.lines().nth(1) == Some("{\"type\":\"user\",\"message\":{\"content\":\"\"}}"),
        "{text}"
    );
    assert_eq!(cmd_reduce(&["x".to_string()]).expect_err("usage").code, 2);
}

fn harness_reads(jsonl: &str, build: &str, shift_s: i64) -> String {
    use aterm_agent::harness::upgrade as up;
    let back = |t: Option<u64>| t.map(|t| i64::try_from(t).unwrap_or(i64::MAX) - shift_s);
    let wall = up::transcript_standing_wall(jsonl).map(|w| {
        let credits = match w.credits {
            Some(CreditsWord::Off { reason, until }) => format!("off {reason:?} {:?}", back(until)),
            c => format!("{c:?}"),
        };
        (back(Some(w.at)), back(w.until), w.window, credits)
    });
    let limit = up::transcript_standing_limit(jsonl)
        .map(|l| (back(l.until), l.window, back(l.at), Reading::of(&l.text)));
    format!(
        "login {} {:?}; limit {:?} {:?} {}; wall {wall:?}; row {limit:?}; model {:?}; answer {:?}; warned {}; finished {}; tasked {}",
        up::transcript_login_wall(jsonl),
        back(up::login_lifted_at(jsonl)),
        back(up::transcript_limit_until(jsonl)),
        back(up::transcript_limit_reset(jsonl)),
        up::transcript_limit_spend(jsonl),
        up::transcript_model(jsonl),
        back(up::transcript_last_answer(jsonl, u64::MAX)),
        up::resumed_build_warned(jsonl, build, 0, u64::MAX),
        up::transcript_finished(jsonl, &[]),
        up::transcript_tasked(jsonl, &[]),
    )
}

fn harness_reads_as(source: &str, red: &Reduction) -> Result<(), String> {
    let a: Vec<&str> = source.lines().filter(|l| is_row(l)).collect();
    let b: Vec<&str> = red.text.lines().filter(|l| is_row(l)).collect();
    if a.len() != b.len() {
        return Err(format!("{} rows became {}", a.len(), b.len()));
    }
    let build = red.build.as_deref().unwrap_or_default();
    let shift_s = red.shift_days * 86_400;
    let notice = |l: &str| {
        notice_read(l).map(|mut n| {
            let r = Reading::of(&n.text);
            n.text.clear();
            (r, n)
        })
    };
    for n in 0..a.len() {
        let (x, y) = (notice(a[n]), notice(b[n]));
        if x != y {
            return Err(format!("row {n}'s notice {x:?} became {y:?}"));
        }
        let (x, y) = (
            harness_reads(&a[..=n].join("\n"), build, 0),
            harness_reads(&b[..=n].join("\n"), build, shift_s),
        );
        if x != y {
            return Err(format!("after row {n}: {x} became {y}"));
        }
    }
    Ok(())
}

#[test]
fn the_harness_reads_every_reduction_as_its_source() {
    let (mut login, mut walls, mut typed) = (0, 0, 0);
    let mut bad = Vec::new();
    for (n, text) in fixtures() {
        let r = reduce(&text, Program::Claude).expect("reduced");
        if let Err(e) = harness_reads_as(&text, &r) {
            bad.push(format!("{n}: {e}"));
        }
        login += usize::from(text.contains("authentication_failed"));
        walls += usize::from(text.contains(r#""status":"rejected""#));
        typed += usize::from(r.text.contains(r#""error":"rate_limit""#));
    }
    assert!(bad.is_empty(), "{bad:#?}");
    assert!(
        login >= 2 && walls >= 5 && typed >= 5,
        "{login} {walls} {typed}"
    );
    let keys: &[&str] = &["error", "status", "rateLimitType"];
    let with = fixtures()
        .iter()
        .filter(|(_, text)| {
            let r = reduce(text, Program::Claude).expect("reduced");
            keys.iter().any(|k| {
                r.text
                    .match_indices(&format!("\"{k}\":\""))
                    .any(|(i, m)| !r.text[i + m.len()..].starts_with('"'))
            })
        })
        .count();
    let blanks = BlanksRead(&["error", "status", "rateLimitType"]);
    let harness =
        |n: &str, t: &str, r: &Reduction| harness_reads_as(t, r).map_err(|e| format!("{n}: {e}"));
    let bad = failures(&blanks, &harness);
    assert!(
        with >= 7 && bad.len() == with,
        "{} of {with}: {bad:#?}",
        bad.len()
    );
    assert_eq!(failures(&blanks, &folds).len(), with);
}

const ENDS_PLANT: [&str; 10] = [
    r#"{"type":"user","timestamp":"2026-10-16T10:00:00.000Z","uuid":"11111111-1111-4111-8111-111111111111","promptId":"12111111-1111-4111-8111-111111111111","message":{"role":"user","content":"hello"},"version":"2.1.303"}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:01.000Z","uuid":"21111111-1111-4111-8111-111111111111","parentUuid":"11111111-1111-4111-8111-111111111111","message":{"id":"msg_01","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"ls"}}],"stop_reason":"tool_use"},"version":"2.1.303"}"#,
    r#"{"type":"user","timestamp":"2026-10-16T10:00:02.000Z","uuid":"31111111-1111-4111-8111-111111111111","parentUuid":"21111111-1111-4111-8111-111111111111","promptId":"12111111-1111-4111-8111-111111111111","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"x"}]},"version":"2.1.303"}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:03.000Z","uuid":"41111111-1111-4111-8111-111111111111","parentUuid":"31111111-1111-4111-8111-111111111111","message":{"id":"msg_02","role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn"},"version":"2.1.303"}"#,
    r#"{"type":"system","subtype":"turn_duration","durationMs":4000,"timestamp":"2026-10-16T10:00:04.000Z","uuid":"51111111-1111-4111-8111-111111111111","parentUuid":"41111111-1111-4111-8111-111111111111","version":"2.1.303"}"#,
    r#"{"type":"user","timestamp":"2026-10-16T10:00:05.000Z","uuid":"61111111-1111-4111-8111-111111111111","parentUuid":"51111111-1111-4111-8111-111111111111","promptId":"62111111-1111-4111-8111-111111111111","message":{"role":"user","content":"again"},"version":"2.1.303"}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:06.000Z","uuid":"71111111-1111-4111-8111-111111111111","parentUuid":"61111111-1111-4111-8111-111111111111","message":{"id":"msg_03","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_02","name":"Bash","input":{"command":"ls"}}],"stop_reason":"tool_use"},"version":"2.1.303"}"#,
    r#"{"type":"user","timestamp":"2026-10-16T10:00:07.000Z","uuid":"81111111-1111-4111-8111-111111111111","parentUuid":"71111111-1111-4111-8111-111111111111","promptId":"62111111-1111-4111-8111-111111111111","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_02","content":"x"}]},"version":"2.1.303"}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:08.000Z","uuid":"91111111-1111-4111-8111-111111111111","parentUuid":"81111111-1111-4111-8111-111111111111","message":{"id":"msg_04","role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn"},"version":"2.1.303"}"#,
    r#"{"type":"system","subtype":"turn_duration","durationMs":4000,"timestamp":"2026-10-16T10:00:09.000Z","uuid":"a1111111-1111-4111-8111-111111111111","parentUuid":"91111111-1111-4111-8111-111111111111","version":"2.1.303"}"#,
];

#[test]
fn an_end_after_a_row_is_read() {
    let src: String = ENDS_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let r = reduce(&src, Program::Claude).expect("reduced");
    check_folds("plant", &src, &r).expect("it folds as its source");
    let late = Reduction {
        text: r
            .text
            .replace("2000-01-01T10:00:03.000Z", "2000-01-01T11:00:03.000Z"),
        ..r.clone()
    };
    assert_ne!(late.text, r.text);
    let (a, b) = (
        readings(&src, Program::Claude),
        readings(&late.text, Program::Claude),
    );
    assert_eq!(a.steps.len(), b.steps.len());
    let e = check_folds("plant", &src, &late).expect_err("the end after row 3 moved");
    assert!(e.contains("the end after row 3"), "{e}");
}

#[test]
fn the_choice_puts_back_and_then_drops_what_is_needless() {
    let redundant = |k: &[bool]| (k[0] || k[1], k[2]);
    let chosen = choose(4, redundant);
    assert_eq!(chosen, vec![true, false, true, false]);
    assert_eq!(redundant(&chosen), redundant(&[true; 4]));
    let table = |k: &[bool]| match (k[0], k[1], k[2]) {
        (true, true, _) | (true, false, true) | (false, true, false) => 'B',
        _ => 'X',
    };
    let chosen = choose(3, table);
    assert_eq!(chosen, vec![false, true, false]);
    assert_eq!(table(&chosen), 'B');
    for (i, kept) in chosen.iter().enumerate() {
        if *kept {
            let mut fewer = chosen.clone();
            fewer[i] = false;
            assert_ne!(table(&fewer), 'B', "pair {i} is needless");
        }
    }
}

#[test]
fn the_verb_writes_only_what_reads_as_its_source() {
    let src: String = WORDS_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let r = reduce(&src, Program::Claude).expect("reduced");
    check_written(&src, &r, Program::Claude).expect("it passes");
    let every = reduce_with(&src, Program::Claude, &KeepsEveryCandidate).expect("reduced");
    let e = check_written(&src, &every, Program::Claude).expect_err("an unread word");
    assert!(
        e.contains("no reader reads") && e.contains("quokkaledger"),
        "{e}"
    );
    let blanked = reduce_with(&src, Program::Claude, &BlanksRead(&["subtype"])).expect("reduced");
    let e = check_written(&src, &blanked, Program::Claude).expect_err("not as its source");
    assert!(e.contains("does not read as its source"), "{e}");
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let file = dir.path().join("t.jsonl");
    std::fs::write(&file, &src).expect("source");
    let args = |out: &std::path::Path| {
        vec![
            file.to_string_lossy().into_owned(),
            "--program".to_string(),
            "claude".to_string(),
            "--out".to_string(),
            out.to_string_lossy().into_owned(),
        ]
    };
    let every: fn(&str, Program) -> Result<Reduction, String> =
        |s, p| reduce_with(s, p, &KeepsEveryCandidate);
    let blanks: fn(&str, Program) -> Result<Reduction, String> =
        |s, p| reduce_with(s, p, &BlanksRead(&["subtype"]));
    for (n, by) in [(1, every), (2, blanks)] {
        let out = dir.path().join(format!("r{n}.jsonl"));
        let e = reduce_verb(&args(&out), by).expect_err("refused");
        assert_eq!(e.code, 1, "{n}");
        assert!(!out.exists(), "{n}: nothing written");
    }
    let out = dir.path().join("r0.jsonl");
    assert_eq!(reduce_verb(&args(&out), reduce).expect("written"), 0);
    assert_eq!(std::fs::read_to_string(&out).expect("written"), r.text);
}

/// Every committed Claude Code reduction (each a lab run that hit the stub's five-hour limit on
/// the subscriber path) keeps its limit's words where the harness reads them.
#[test]
fn every_committed_claude_reduction_keeps_its_limit_words() {
    use aterm_agent::harness::upgrade as up;
    for path in steps_claude() {
        let name = path.display().to_string();
        let committed = std::fs::read_to_string(&path).expect("a committed reduction");
        let rows: Vec<&str> = committed.lines().filter(|l| is_row(l)).collect();
        let at = rows
            .iter()
            .position(|l| notice_read(l).is_some_and(|n| n.wall == Some(WallKind::UsageSession)))
            .unwrap_or_else(|| panic!("{name}: its usage limit's notice"));
        let cut = rows[..=at].join("\n");
        let wall = up::transcript_standing_wall(&cut).expect("the quota wall stands");
        assert_eq!(wall.window.as_deref(), Some("five_hour"), "{name}");
        assert!(wall.until.is_some(), "{name}: {wall:?}");
        let limit = up::transcript_standing_limit(&cut).expect("the limit row stands");
        assert_eq!(limit.window.as_deref(), Some("five_hour"), "{name}");
        let n = notice_read(rows[at]).expect("a notice");
        assert_eq!(
            (
                n.error.as_deref(),
                n.api_error.as_deref(),
                n.limit.as_deref()
            ),
            (
                Some("rate_limit"),
                Some("usage_limit_reached"),
                Some("five_hour")
            ),
            "{name}"
        );
    }
}
