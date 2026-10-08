// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test binary and one test: count allocation traffic for real transcript
//! consumers without another parallel test polluting the global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_agent::harness::{footer, upgrade, upgrade_codex, upgrade_models, usage};
use aterm_agent::supervise::codex_usage;

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
    eprintln!("transcript allocations: {name}: {bytes} bytes, {calls} allocations");
    assert!(bytes < max_bytes, "{name} allocated {bytes} bytes");
    assert!(calls < max_calls, "{name} made {calls} allocations");
    result
}

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
            r#"{{"type":"assistant","isSidechain":false,"timestamp":"2026-09-28T12:00:00Z","version":"2.1.283","cwd":"/work/project","effort":"xhigh","message":{{"id":"msg-1","model":"{model}","content":{content},"usage":{{"input_tokens":42,"output_tokens":7}}}}}}"#
        );
        assert!(row.len() < 256 * 1024, "footer fixture must be admitted");
        let (full, full_bytes, full_calls) = measured(|| {
            aterm_json::from_str::<aterm_json::Value>(&row).expect("full Value control")
        });
        eprintln!(
            "transcript allocations: full Value: {full_bytes} bytes, {full_calls} allocations ({} input bytes)",
            row.len()
        );
        assert_eq!(full["message"]["model"].as_str(), Some(model));
        assert!(
            full_bytes > content.len(),
            "control must materialize content"
        );

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
            Some(std::path::Path::new("/work/project"))
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
    }
    codex_metadata_does_not_materialize_payloads();
    backward_context_reads_allocate_linearly();
}

/// Codex carries large instructions and world state in head/context rows; the
/// head and sandbox decisions need their discriminants, never those payloads.
fn codex_metadata_does_not_materialize_payloads() {
    let padding = "agent instructions\\n日本語 ".repeat(16_384);
    let row = format!(
        r#"{{"type":"turn_context","timestamp":"2026-10-06T12:00:00Z","payload":{{"cwd":"/work/project","model":"gpt-6-astra","sandbox_policy":{{"type":"danger-full-access"}},"developer_instructions":"{padding}"}}}}"#
    );
    let (full, full_bytes, _) =
        measured(|| aterm_json::from_str::<aterm_json::Value>(&row).expect("full Value control"));
    assert!(full_bytes > padding.len());
    assert_eq!(full["payload"]["model"].as_str(), Some("gpt-6-astra"));
    eprintln!("transcript allocations: Codex full Value: {full_bytes} bytes");
    assert!(bounded("Codex turn head", || upgrade_codex::head_part(
        &row
    )));
    let context = bounded("Codex context", || {
        codex_usage::rollout_turn_context(&row).expect("context")
    });
    assert_eq!(context.cwd, "/work/project");
    assert_eq!(context.sandbox, "danger-full-access");
    assert_eq!(context.model.as_deref(), Some("gpt-6-astra"));
    // A tool result can quote event words; it must not become an event or a
    // head, nor allocate its output while checking those words.
    let decoy = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call_output","output":"event_msg task_complete {padding}"}}}}"#
    );
    assert!(!bounded("Codex tool head", || upgrade_codex::head_part(
        &decoy
    )));
    let mut cursor = std::io::Cursor::new(decoy.as_bytes());
    // The reader's input buffer is expected; the payload's Value tree is not.
    let state = within("Codex quoted turn event", 3 * decoy.len(), 100, || {
        upgrade_codex::rollout_turn_back(&mut cursor, decoy.len() as u64, u64::MAX, u64::MAX)
    });
    assert_eq!(state, upgrade_codex::TurnState::Unknown);
}

/// The historical reader recopied the entire accumulated partial line at each
/// backward step. Keep it as the allocation negative control, with the same
/// real context parser, so the threshold rejects the original quadratic path.
fn copied_context_read(path: &std::path::Path) -> Option<codex_usage::TurnContext> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut end = file.metadata().ok()?.len();
    let mut carry = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(codex_usage::CONTEXT_CHUNK);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity(usize::try_from(end - start).ok()?);
        (&mut file).take(end - start).read_to_end(&mut buf).ok()?;
        buf.extend_from_slice(&carry);
        let (head, whole) = if start > 0 {
            match buf.iter().position(|&b| b == b'\n') {
                Some(nl) => (buf[..nl].to_vec(), &buf[nl + 1..]),
                None => (buf.clone(), &buf[buf.len()..]),
            }
        } else {
            (Vec::new(), &buf[..])
        };
        if let Some(found) = codex_usage::rollout_turn_context(&String::from_utf8_lossy(whole)) {
            return Some(found);
        }
        carry = head;
        end = start;
    }
    None
}

fn backward_context_reads_allocate_linearly() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("codex-context-allocation-")
        .tempdir()
        .expect("scratch");
    let path = scratch.path().join("rollout.jsonl");
    let context = r#"{"type":"turn_context","payload":{"cwd":"/work/project","model":"gpt-6-astra","sandbox_policy":{"type":"danger-full-access"}}}"#;
    let mut previous = 0;
    for mib in [8, 16] {
        let rollout = format!(
            "{context}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"output\":\"{}\"}}}}\n",
            "x".repeat(mib * 1024 * 1024)
        );
        std::fs::write(&path, &rollout).expect("write rollout");
        let (found, bytes, calls) = measured(|| {
            codex_usage::last_turn_context(&path, codex_usage::CONTEXT_REACH)
                .expect("context behind tool output")
        });
        let (old, old_bytes, _) = measured(|| copied_context_read(&path).expect("old context"));
        eprintln!(
            "transcript allocations: Codex {mib}MiB backward read: {bytes} bytes, {calls} allocations; copied-carry control: {old_bytes} bytes"
        );
        assert_eq!(found, old);
        assert_eq!(found.sandbox, "danger-full-access");
        assert!(bytes < 3 * rollout.len(), "linear reader allocated {bytes}");
        assert!(
            old_bytes > 8 * rollout.len(),
            "control must expose repeated copying"
        );
        if previous > 0 {
            assert!(
                bytes < 3 * previous,
                "doubling input must not square allocation"
            );
        }
        previous = bytes;
        assert!(codex_usage::last_turn_context(&path, codex_usage::CONTEXT_CHUNK).is_none());
    }
}
