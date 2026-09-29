// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test binary and one test: count allocation traffic for real transcript
//! consumers without another parallel test polluting the global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_agent::harness::{footer, upgrade, upgrade_models, usage};

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
fn metadata_readers_do_not_materialize_tool_payloads() {
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
}
