// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The transcript projections over rows shaped like Claude Code's and Codex's,
//! and over the vendor drift and malformed input they must survive, recorded
//! while the projection drove registry serde's `Visitor`, `MapAccess`,
//! `SeqAccess` and `IgnoredAny` (docs/AUDIT-third-party-2026-10-15.md, SP.3),
//! and held since SP.6 against aterm-serde's.
//! Each row is read by all five projections; an `Ok` is the kept value written
//! back out by `aterm_json`, an `Err` its `Display`.

#[path = "../../../aterm-json/test_support/serde_corpus.rs"]
mod answers;

use answers::Corpus;

/// Rows with the keys the vendors write, values made up (the step fold's
/// agreement test reads the `claude/` ones too).
pub(crate) const ROWS: &[(&str, &str)] = &[
    (
        "claude/assistant",
        r#"{"parentUuid":"p-1","isSidechain":false,"type":"assistant","uuid":"u-2","timestamp":"2026-10-15T20:36:32.123Z","message":{"id":"msg_1","model":"claude-opus-5-5","role":"assistant","stop_reason":"end_turn","type":"message","usage":{"input_tokens":3,"output_tokens":120,"cache_creation_input_tokens":2048,"cache_read_input_tokens":40960,"server_tool_use":{"web_search_requests":0},"service_tier":"standard","iterations":[{"type":"message","input_tokens":3,"output_tokens":120,"cache_creation_input_tokens":2048,"cache_read_input_tokens":40960}]},"content":[{"type":"text","text":"Done."},{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}]},"requestId":"r","version":"2.1.301","cwd":"/home/dev/src","effort":"high"}"#,
    ),
    (
        "claude/user-prompt",
        r#"{"parentUuid":null,"isSidechain":false,"promptId":"x","type":"user","message":{"role":"user","content":"/model opus"},"isMeta":false,"uuid":"u-1","timestamp":"2026-10-15T20:36:30Z","userType":"external","cwd":"/home/dev/src","sessionId":"s","version":"2.1.301","gitBranch":"main"}"#,
    ),
    (
        "claude/user-tool-result",
        r#"{"parentUuid":"u-2","type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"a\nb"}]},{"type":"text","text":"follow-up"}]},"uuid":"u-3","timestamp":"2026-10-15T20:36:40Z"}"#,
    ),
    (
        "claude/turn-duration",
        r#"{"parentUuid":"u-3","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":5123,"messageCount":4,"timestamp":"2026-10-15T20:36:41Z","uuid":"u-4","isMeta":true}"#,
    ),
    (
        "claude/limit",
        r#"{"type":"system","subtype":"api_error","isApiErrorMessage":true,"error":"rate_limit","content":"You've hit your limit","quotaLimits":{"status":"rejected","resetsAt":1790870400,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"five_hour","overageStatus":"rejected","isUsingOverage":false},"uuid":"u-5","timestamp":"2026-10-15T20:37:00Z"}"#,
    ),
    (
        "claude/compact-boundary",
        r#"{"parentUuid":null,"logicalParentUuid":"u-5","type":"system","subtype":"compact_boundary","content":"Conversation compacted","isMeta":false,"isCompactSummary":true,"compactMetadata":{"trigger":"auto","preTokens":160000},"uuid":"u-6","timestamp":"2026-10-15T20:40:00Z"}"#,
    ),
    (
        "claude/bookkeeping",
        r#"{"type":"cost-state","sessionId":"s","totalCostUSD":0,"totalAPIDuration":100,"modelUsage":{},"hasUnknownModelCost":false}"#,
    ),
    (
        "codex/session-meta",
        r#"{"timestamp":"2026-10-15T20:00:00.000Z","type":"session_meta","payload":{"id":"c-1","cwd":"/home/dev/src","originator":"codex_cli_rs","cli_version":"0.163.0","instructions":"long text","git":{"branch":"main"}}}"#,
    ),
    (
        "codex/turn-context",
        r#"{"timestamp":"2026-10-15T20:00:01.000Z","type":"turn_context","payload":{"cwd":"/home/dev/src","approval_policy":"on-request","sandbox_policy":{"type":"workspace-write","network_access":false},"model":"gpt-5-codex","effort":"high","summary":"auto"}}"#,
    ),
    (
        "codex/response-item",
        r#"{"timestamp":"2026-10-15T20:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
    ),
    // Vendor drift: a kept key of the wrong shape reads as null, never an error.
    (
        "drift/usage-strings",
        r#"{"type":"assistant","message":{"model":["opus"],"usage":{"input_tokens":"3","output_tokens":{"n":1},"cache_read_input_tokens":null}}}"#,
    ),
    (
        "drift/message-array",
        r#"{"type":"assistant","message":[{"model":"opus"}],"timestamp":7}"#,
    ),
    (
        "drift/content-scalars",
        r#"{"type":"user","message":{"content":[null,12,"scalar",["nested"],{"type":"text","text":"kept","extra":1},{"text":{"deep":true}}]}}"#,
    ),
    (
        "drift/quota-shapes",
        r#"{"type":"system","quotaLimits":{"status":{"is":"rejected"},"resetsAt":"soon","rateLimitType":["seven_day"]}}"#,
    ),
    // Duplicate keys: the last one wins, an escaped spelling of a key included.
    (
        "duplicate/last-wins",
        r#"{"type":"user","type":"assistant","message":{"model":"a","model":"b"},"type":"system"}"#,
    ),
    (
        "duplicate/escaped-spelling",
        r#"{"type":"assistant","\u0074ype":"user","message":{"model":"a","mod\u0065l":"b"}}"#,
    ),
    // Numbers at the edges of every width the JSON reader keeps.
    (
        "number/u64-max",
        r#"{"type":"assistant","message":{"usage":{"input_tokens":18446744073709551615}}}"#,
    ),
    (
        "number/u64-overflow",
        r#"{"type":"assistant","message":{"usage":{"input_tokens":18446744073709551616}}}"#,
    ),
    (
        "number/i64-min",
        r#"{"type":"assistant","message":{"usage":{"input_tokens":-9223372036854775808}}}"#,
    ),
    (
        "number/negative-zero",
        r#"{"type":"assistant","durationMs":-0,"message":{"usage":{"input_tokens":-0.0}}}"#,
    ),
    (
        "number/exponent",
        r#"{"type":"system","durationMs":1e3,"resetsAt":1.5E-3}"#,
    ),
    (
        "number/infinite",
        r#"{"type":"system","durationMs":1e9999}"#,
    ),
    (
        "number/nan-literal",
        r#"{"type":"system","durationMs":NaN}"#,
    ),
    (
        "number/infinity-literal",
        r#"{"type":"system","durationMs":Infinity}"#,
    ),
    (
        "number/leading-zero",
        r#"{"type":"system","durationMs":01}"#,
    ),
    // Not a row at all.
    ("shape/array", r#"[{"type":"assistant"}]"#),
    ("shape/string", r#""assistant""#),
    ("shape/null", "null"),
    ("shape/empty-object", "{}"),
    ("shape/empty", ""),
    ("shape/trailing", r#"{"type":"user"} {"type":"user"}"#),
    ("shape/trailing-comma", r#"{"type":"user",}"#),
    (
        "shape/unterminated",
        r#"{"type":"user","message":{"content":"x"#,
    ),
    (
        "shape/lone-surrogate",
        r#"{"type":"user","message":{"content":"\uD800"}}"#,
    ),
    (
        "shape/bad-escape-discarded",
        r#"{"type":"user","unknown":"\x"}"#,
    ),
    ("shape/control-char", "{\"type\":\"us\ner\"}"),
    ("shape/whitespace", " \t\r\n{ \"type\" : \"user\" }\n "),
];

/// Rows of a turn Claude Code begins of its own (`claude/begins/`) and of
/// rows that go on the running turn (`claude/goes-on/`), the running turn
/// being `promptId` `v1` (values made up; the shapes `upgrade_tests.rs`
/// holds `upgrade::FinishedScan` to, and Claude Code 2.1.302's `/loop` wake):
/// read by the step projections (`steps.txt`) and by the step fold's
/// agreements with `upgrade.rs` and with the harness's end, never by
/// `transcript.txt`, which stays as SP.6 recorded it.
pub(crate) const TURN_ROWS: &[(&str, &str)] = &[
    (
        "claude/begins/loop-wake",
        r#"{"parentUuid":"a-1","isSidechain":false,"promptId":"p-2","type":"user","message":{"role":"user","content":"keep going: check the land log"},"isMeta":true,"uuid":"w-1","timestamp":"2026-10-15T20:40:00.000Z","permissionMode":"bypassPermissions","promptSource":"system","scheduledTaskId":"00000001","scheduledFireId":"f-1","turnOrigin":"scheduled","queuePriority":"later","queueSkipAttachments":true,"version":"2.1.302"}"#,
    ),
    (
        "claude/begins/loop-wake-unnumbered",
        r#"{"parentUuid":"a-1","isSidechain":false,"type":"user","message":{"role":"user","content":"keep going: check the land log"},"isMeta":true,"uuid":"w-2","timestamp":"2026-10-15T20:40:00.000Z","promptSource":"system","scheduledTaskId":"00000001","version":"2.1.267"}"#,
    ),
    (
        "claude/begins/turn-origin-unnumbered",
        r#"{"parentUuid":"a-1","isSidechain":false,"type":"user","message":{"role":"user","content":"keep going: check the land log"},"isMeta":true,"uuid":"w-3","timestamp":"2026-10-15T20:40:00.000Z","turnOrigin":"scheduled","version":"2.1.302"}"#,
    ),
    (
        "claude/begins/missed-task-notice",
        r#"{"parentUuid":"a-1","isSidechain":false,"type":"user","message":{"role":"user","content":"The following one-shot scheduled task was missed while Claude was not running. It has already been removed from .claude/scheduled_tasks.json.\n\n[at 09:00]\ncheck the land log"},"isMeta":true,"uuid":"w-4","timestamp":"2026-10-15T20:40:00.000Z","version":"2.1.302"}"#,
    ),
    (
        "claude/goes-on/stop-hook",
        r#"{"parentUuid":"a-1","isSidechain":false,"promptId":"v1","type":"user","message":{"role":"user","content":"Stop hook feedback:\n[The compiler builds itself]: The condition has not been satisfied: stage 2 fails."},"isMeta":true,"uuid":"h-1","timestamp":"2026-10-15T20:40:00.000Z","version":"2.1.302"}"#,
    ),
    (
        "claude/goes-on/goal-check-in",
        r#"{"parentUuid":"a-1","isSidechain":false,"promptId":"v1","type":"user","message":{"role":"user","content":"Goal check-in: the compiler builds itself is not met yet."},"isMeta":true,"uuid":"h-2","timestamp":"2026-10-15T20:40:00.000Z","version":"2.1.302"}"#,
    ),
    (
        "claude/goes-on/stop-hook-unnumbered",
        r#"{"parentUuid":"a-1","isSidechain":false,"type":"user","message":{"role":"user","content":"Stop hook feedback: the log says \"The following one-shot scheduled task was missed\"; the goal is not met."},"isMeta":true,"uuid":"h-3","timestamp":"2026-10-15T20:40:00.000Z","version":"2.1.267"}"#,
    ),
];

#[test]
fn transcript_projections_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "transcript");
    let written = |read: aterm_json::Result<aterm_json::Value>| {
        read.map(|value| aterm_json::to_string(&value).expect("a kept value writes"))
    };
    let deep = format!("{{\"unknown\":{}0{}}}", "[".repeat(130), "]".repeat(130));
    let rows = ROWS
        .iter()
        .map(|&(id, row)| (id, row.to_owned()))
        .chain([("shape/deep-discarded", deep)]);
    for (id, row) in rows {
        corpus.record(format!("{id}/usage"), written(super::usage(&row)));
        corpus.record(format!("{id}/metadata"), written(super::metadata(&row)));
        corpus.record(format!("{id}/command"), written(super::command(&row)));
        corpus.record(
            format!("{id}/conversation"),
            written(super::conversation(&row)),
        );
    }
    corpus.finish();
}

/// The step fold's two projections (addendum A8) over the same rows, held to
/// their own recorded answers (`steps.txt`; `transcript.txt` is untouched):
/// what each keeps, and that neither keeps any text — a string content, and
/// a bare string among the parts, read as null; a text part keeps only its
/// `type`.
#[test]
fn steps_projections_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "steps");
    let written = |read: aterm_json::Result<aterm_json::Value>| {
        read.map(|value| aterm_json::to_string(&value).expect("a kept value writes"))
    };
    let deep = format!("{{\"unknown\":{}0{}}}", "[".repeat(130), "]".repeat(130));
    let rows = ROWS
        .iter()
        .chain(TURN_ROWS)
        .map(|&(id, row)| (id, row.to_owned()))
        .chain([("shape/deep-discarded", deep)]);
    for (id, row) in rows {
        corpus.record(format!("{id}/steps"), written(super::steps(&row)));
        corpus.record(format!("{id}/steps-bare"), written(super::steps_bare(&row)));
    }
    corpus.finish();
}
