// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test binary and one test: count allocation traffic for real transcript
//! consumers without another parallel test polluting the global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_agent::harness::{codex_steps, footer, steps, upgrade, upgrade_models, usage};

static COUNTING: AtomicBool = AtomicBool::new(false);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);

struct Allocator;

fn record(bytes: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        BYTES.fetch_add(bytes, Ordering::Relaxed);
        CALLS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: every allocation and deallocation is forwarded unchanged to System.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: the caller supplies the valid allocation layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: the caller supplies the valid allocation layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        // SAFETY: the caller supplies System's allocation and its layout.
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller supplies System's allocation and its layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn measured<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    BYTES.store(0, Ordering::Relaxed);
    CALLS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let result = f();
    COUNTING.store(false, Ordering::Relaxed);
    (
        result,
        BYTES.load(Ordering::Relaxed),
        CALLS.load(Ordering::Relaxed),
    )
}

fn bounded<T>(name: &str, f: impl FnOnce() -> T) -> T {
    within(name, 16 * 1024, 100, f)
}

fn within<T>(name: &str, max_bytes: usize, max_calls: usize, f: impl FnOnce() -> T) -> T {
    let (result, bytes, calls) = measured(f);
    assert!(bytes < max_bytes, "{name} allocated {bytes} bytes");
    assert!(calls < max_calls, "{name} made {calls} allocations");
    result
}

/// The row's working directory: absolute on this host (a Windows one starts
/// with a drive).
const CWD: &str = if cfg!(windows) {
    "C:/work/project"
} else {
    "/work/project"
};

#[test]
fn transcript_readers_keep_allocation_proportional_to_input() {
    let model = "claude-opus-5-5";
    // A common coding-agent row: an assistant's tool calls, with escaped
    // multi-line commands and structured arguments, alongside tiny metadata.
    let command = "cat file.rs\\n日本語 ".repeat(200);
    let blocks: Vec<_> = (0..24)
        .map(|i| {
            format!(
                r#"{{"type":"tool_use","id":"tool-{i}","name":"exec_command","input":{{"command":"{command}","args":[1,2,3]}}}}"#
            )
        })
        .collect();
    for content in [format!("[{}]", blocks.join(",")), format!("\"{command}\"")] {
        let row = format!(
            r#"{{"type":"assistant","isSidechain":false,"timestamp":"2026-09-28T12:00:00Z","version":"2.1.283","cwd":"{CWD}","effort":"xhigh","message":{{"id":"msg-1","model":"{model}","content":{content},"usage":{{"input_tokens":42,"output_tokens":7}}}}}}"#
        );
        assert!(row.len() < 256 * 1024, "footer fixture must be admitted");
        let fold = bounded("usage", || {
            let mut fold = usage::TranscriptUsage::new();
            assert_eq!(fold.fold_line(&row), usage::Fold::Summed);
            fold
        });
        assert_eq!(fold.per_model()[model].input, 42);
        assert_eq!(fold.per_model()[model].output, 7);
        assert_eq!(
            fold.served_at(),
            upgrade_models::parse_utc("2026-09-28T12:00:00Z").and_then(|t| i64::try_from(t).ok()),
            "the allocation-saving projection still feeds the live wall's served time"
        );
        let facts = bounded("footer", || footer::tail_facts(row.as_bytes(), None));
        // The land's reader names the model as the footer shows it.
        assert_eq!(facts.model, footer::Said::Is(footer::model_display(model)));
        assert_eq!(facts.effort, footer::Said::Is("xhigh".to_string()));
        assert_eq!(
            bounded("cwd", || footer::last_cwd(row.as_bytes())).as_deref(),
            Some(std::path::Path::new(CWD))
        );
        assert_eq!(
            bounded("upgrade model", || upgrade::transcript_model(&row)).as_deref(),
            Some(model)
        );
        assert_eq!(
            bounded("first model", || upgrade::transcript_first_model(
                &row, "2.1.283"
            ))
            .as_deref(),
            Some(model)
        );
        assert_eq!(
            bounded("live model", || upgrade_models::live_model(&row, None))
                .expect("model")
                .id,
            model
        );
        assert_eq!(
            bounded("answer age", || upgrade_models::last_answer_at(&row)),
            upgrade_models::parse_utc("2026-09-28T12:00:00Z")
        );
        assert_eq!(
            bounded("resumed model", || upgrade_models::live_model_at(
                &row,
                None,
                Some(model),
                Some(1)
            ))
            .expect("model")
            .id,
            model
        );
        // The lifecycle readers need content block kinds and text, but no
        // tool input/result trees. Their budget includes those 24 small maps.
        assert!(!within("READY", 32 * 1024, 128, || {
            upgrade::transcript_has_ready(&row, "ATERM-UPGRADE-READY-0badf00d")
        }));
        assert!(
            within("notice", 32 * 1024, 128, || {
                upgrade::notice_scan(&row, "ATERM-UPGRADE-READY-0badf00d")
            })
            .is_none()
        );
        assert!(!within("login", 32 * 1024, 128, || {
            upgrade::transcript_login_wall(&row)
        }));
        assert!(
            within("limit", 32 * 1024, 128, || {
                upgrade::transcript_limit_until(&row)
            })
            .is_none()
        );
        assert!(!within("task scan", 32 * 1024, 128, || {
            upgrade::TaskScan::default().line(&row)
        }));
        // The session log's step fold (addendum A8, ruling S6): the calls'
        // ids and names, never their inputs (24 × 5 KB here); a plain-string
        // content is validated and never copied. Its budget holds the 24
        // small part maps and the 25 queued steps (about 190 B each), well
        // under the inputs a reader that kept them would copy.
        let mut fold = steps::StepFold::new(0, None);
        fold.read_at(1, None, 0, false);
        let line_end = row.len() as u64 + 1;
        // A plain-string content (4 KB here) is never copied: that row's
        // read keeps a few small values.
        let (bytes, calls) = if content.starts_with('[') {
            (64 * 1024, 256)
        } else {
            (4 * 1024, 64)
        };
        within("steps", bytes, calls, || {
            fold.push(row.as_bytes(), line_end)
        });
        // The row opens the conversation with no prompt before it: the
        // fold's opening hold keeps its steps through one take, for a
        // prompt row written after it (`steps::StepFold::take`).
        let calls = [fold.take(), fold.take()]
            .iter()
            .flat_map(|b| &b.steps)
            .filter(|s| matches!(s.ev, steps::StepEvent::ToolUse { .. }))
            .count();
        assert_eq!(calls, if content.starts_with('[') { 24 } else { 0 });
    }
    // The step fold's parts path (a `\u` in the row sends it there) on a
    // plain-string content with no escape, which the parser hands over
    // borrowed: still validated and never copied (4 KB here).
    let plain = "cat file.rs 日本語 ".repeat(200);
    let row = format!(
        r#"{{"type":"\u0061ssistant","isSidechain":false,"timestamp":"2026-09-28T12:00:00Z","message":{{"id":"msg-1","model":"{model}","content":"{plain}","usage":{{"input_tokens":42,"output_tokens":7}}}}}}"#
    );
    let mut fold = steps::StepFold::new(0, None);
    fold.read_at(1, None, 0, false);
    let line_end = row.len() as u64 + 1;
    within("steps, parts path", 4 * 1024, 64, || {
        fold.push(row.as_bytes(), line_end)
    });
    assert!(!fold.broken());
    codex_step_rows_materialize_no_text();
}

/// The session log's Codex step fold (5G-2, ruling S6), over a rollout's
/// rows as Codex 0.163.0 writes them, each carrying about 72 KB where Codex
/// writes text: the base instructions, a call's arguments, a script's
/// input, a result's output, a message, a compaction's history and a turn's
/// last message. Each row's read keeps its few keys and queues its steps,
/// well under the text a reader that built the row would copy. NEGATIVE
/// CONTROL: building each text-carrying row's tree allocates more than its
/// text.
fn codex_step_rows_materialize_no_text() {
    let big = "SENTINEL-TEXT \\n日本語 ".repeat(3 * 1024);
    let at = "2026-09-28T12:00:00.000Z";
    let thread = "01900000-0000-7000-8000-000000000001";
    let rows = [
        format!(
            r#"{{"timestamp":"{at}","type":"session_meta","payload":{{"id":"{thread}","session_id":"{thread}","cli_version":"0.163.0","history_mode":"paginated","base_instructions":{{"text":"{big}"}}}}}}"#
        ),
        format!(
            r#"{{"timestamp":"{at}","type":"turn_context","payload":{{"turn_id":"turn-1","cwd":"{CWD}","model":"gpt-6.1-sol","effort":"high"}}}}"#
        ),
        format!(
            r#"{{"timestamp":"{at}","type":"response_item","payload":{{"type":"function_call","name":"exec","namespace":"functions","arguments":"{big}","call_id":"call-1"}}}}"#
        ),
        format!(
            r#"{{"timestamp":"{at}","type":"response_item","payload":{{"type":"custom_tool_call","name":"exec","namespace":"functions","input":"{big}","call_id":"call-2"}}}}"#
        ),
        format!(
            r#"{{"timestamp":"{at}","type":"response_item","payload":{{"type":"function_call_output","call_id":"call-1","output":"{big}"}}}}"#
        ),
        format!(
            r#"{{"timestamp":"{at}","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{big}"}}]}}}}"#
        ),
        format!(
            r#"{{"timestamp":"{at}","type":"compacted","payload":{{"message":"{big}","replacement_history":[{{"type":"message","content":[{{"type":"input_text","text":"{big}"}}]}}]}}}}"#
        ),
        format!(
            r#"{{"timestamp":"{at}","type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-1","last_agent_message":"{big}","duration_ms":12}}}}"#
        ),
    ];
    let mut fold = codex_steps::CodexStepFold::new(0);
    fold.bind(1, None, 0);
    let mut end = 0u64;
    for row in &rows {
        end += row.len() as u64 + 1;
        within("codex steps", 4 * 1024, 64, || {
            fold.push(row.as_bytes(), end)
        });
        if row.contains("SENTINEL-TEXT") {
            // NEGATIVE CONTROL.
            let (tree, bytes, _) = measured(|| aterm_json::from_str::<aterm_json::Value>(row));
            assert!(tree.is_ok());
            assert!(bytes > big.len(), "a built row copies its text: {bytes}");
        }
    }
    assert!(!fold.broken());
    let batch = fold.take();
    assert_eq!((batch.lost, batch.overflow), (0, 0));
    // The bind, the prompt, the model, two calls, a result, the compaction
    // and the turn's end.
    assert_eq!(batch.steps.len(), 8, "{:?}", batch.steps);
    assert!(!format!("{:?}", batch.steps).contains("SENTINEL"));
}
