// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Allocation-traffic bounds for transcript consumers under a counting global allocator.

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

const CWD: &str = if cfg!(windows) {
    "C:/work/project"
} else {
    "/work/project"
};

#[test]
fn transcript_readers_keep_allocation_proportional_to_input() {
    let model = "claude-opus-5-5";
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
        let mut fold = steps::StepFold::new(0, None);
        fold.read_at(1, None, 0, false);
        let line_end = row.len() as u64 + 1;
        let (bytes, calls) = if content.starts_with('[') {
            (64 * 1024, 256)
        } else {
            (4 * 1024, 64)
        };
        within("steps", bytes, calls, || {
            fold.push(row.as_bytes(), line_end)
        });
        let calls = [fold.take(), fold.take()]
            .iter()
            .flat_map(|b| &b.steps)
            .filter(|s| matches!(s.ev, steps::StepEvent::ToolUse { .. }))
            .count();
        assert_eq!(calls, if content.starts_with('[') { 24 } else { 0 });
    }
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
    codex_rollout_scan_and_thread_sample_bound_allocations();
}

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
            let (tree, bytes, _) = measured(|| aterm_json::from_str::<aterm_json::Value>(row));
            assert!(tree.is_ok());
            assert!(bytes > big.len(), "a built row copies its text: {bytes}");
        }
    }
    assert!(!fold.broken());
    let batch = fold.take();
    assert_eq!((batch.lost, batch.overflow), (0, 0));
    assert_eq!(batch.steps.len(), 8, "{:?}", batch.steps);
    assert!(!format!("{:?}", batch.steps).contains("SENTINEL"));
}

fn codex_rollout_scan_and_thread_sample_bound_allocations() {
    use aterm_agent::ai::codex_limits::thread_sample;
    use aterm_agent::harness::codex_rollout::conversation_rollouts;

    let tmp = aterm_tempfile::Builder::new()
        .prefix("codex-alloc-bounds-")
        .tempdir()
        .expect("temp dir");
    let day = tmp.path().join("sessions/2026/10/26");
    std::fs::create_dir_all(&day).expect("sessions dir");

    let root_id = "01900000-0000-7000-8000-000000000001";
    let now_unix = 1_790_000_000i64;
    let started_unix = now_unix - 120;
    let reset = now_unix + 7200;
    let ts_open = usage::rfc3339_utc(now_unix - 110).replace('Z', ".000Z");
    let ts_live = usage::rfc3339_utc(now_unix - 20).replace('Z', ".000Z");
    let big_instructions = "You are Codex. Keep instructions unmaterialized. ".repeat(1400); // ~68 KB

    // Active root rollout + 12 pre-start subagent rollouts (> NEWEST_READ = 6) + 1 post-start
    // subagent rollout + 32 unrelated conversation rollouts (~2.9 MiB of `base_instructions`).
    std::fs::write(
        day.join(format!("rollout-2026-10-26T10-00-00-{root_id}.jsonl")),
        format!(
            r#"{{"timestamp":"{ts_open}","type":"session_meta","payload":{{"id":"{root_id}","session_id":"{root_id}","history_mode":"paginated","base_instructions":{{"text":"{big_instructions}"}}}}}}
{{"timestamp":"{ts_live}","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"codex","primary":{{"used_percent":41.2,"window_minutes":300,"resets_at":{reset}}},"secondary":null}}}}}}
"#
        ),
    )
    .expect("root rollout");

    for i in 0..12 {
        let sub_id = format!("01900000-0000-7000-8000-0000000001{i:02x}");
        std::fs::write(
            day.join(format!("rollout-2026-10-26T09-{i:02}-00-{sub_id}.jsonl")),
            format!(
                r#"{{"timestamp":"2026-09-21T10:00:00.000Z","type":"session_meta","payload":{{"id":"{sub_id}","session_id":"{root_id}","forked_from_id":"{root_id}","history_mode":"paginated","base_instructions":{{"text":"{big_instructions}"}}}}}}
{{"timestamp":"2026-09-21T10:05:00.000Z","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"codex","primary":{{"used_percent":99.0,"window_minutes":300,"resets_at":{reset}}},"secondary":null}}}}}}
"#
            ),
        )
        .expect("old sub rollout");
    }

    let live_sub = "01900000-0000-7000-8000-0000000002ff";
    std::fs::write(
        day.join(format!("rollout-2026-10-26T10-30-00-{live_sub}.jsonl")),
        format!(
            r#"{{"timestamp":"{ts_open}","type":"session_meta","payload":{{"id":"{live_sub}","session_id":"{root_id}","forked_from_id":"{root_id}","history_mode":"paginated","base_instructions":{{"text":"{big_instructions}"}}}}}}
{{"timestamp":"{ts_live}","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"codex","primary":{{"used_percent":62.4,"window_minutes":300,"resets_at":{reset}}},"secondary":null}}}}}}
"#
        ),
    )
    .expect("live sub rollout");

    for i in 0..32 {
        let other_id = format!("01900000-0000-7000-8000-000000003{i:03x}");
        std::fs::write(
            day.join(format!("rollout-2026-10-26T11-{i:02}-00-{other_id}.jsonl")),
            format!(
                r#"{{"timestamp":"{ts_open}","type":"session_meta","payload":{{"id":"{other_id}","session_id":"{other_id}","history_mode":"paginated","base_instructions":{{"text":"{big_instructions}"}}}}}}
{{"timestamp":"{ts_live}","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"codex","primary":{{"used_percent":5.0,"window_minutes":300,"resets_at":{reset}}},"secondary":null}}}}}}
"#
            ),
        )
        .expect("other rollout");
    }

    // 46 rollouts carry >3.1 MiB of `base_instructions`; `conversation_rollouts` reuses one
    // scratch buffer and borrows `SessionMetaLine` ids (< 384 KiB, < 500 allocations).
    let conv = within("conversation_rollouts", 384 * 1024, 500, || {
        conversation_rollouts(tmp.path(), root_id)
    });
    assert_eq!(conv.subagents, 13);

    // `thread_sample` reads `roots` + the 6 newest subagents without re-opening `session_meta`
    // for `opening_account` and picks the live subagent's 62.4% -> 63% share (< 768 KiB, < 1000 calls).
    let sample = within("thread_sample", 768 * 1024, 1000, || {
        thread_sample(tmp.path(), root_id, started_unix, now_unix)
    })
    .expect("post-start sample");
    assert_eq!(sample.session_pct, Some(63));
    #[cfg(target_os = "macos")]
    sqlite_goal_scan_bounds_allocations();
}

#[cfg(target_os = "macos")]
fn sqlite_goal_scan_bounds_allocations() {
    use aterm_agent::harness::goal::{Store, read_store};

    let tmp = aterm_tempfile::Builder::new()
        .prefix("sqlite-alloc-bounds-")
        .tempdir()
        .expect("temp dir");
    let dir = std::fs::canonicalize(tmp.path()).expect("canonical");
    let db = dir.join("goals.db");
    let target = "019a0000-0000-7000-8000-0000000001f4";
    let pad = "keep-rows-borrowed-".repeat(20);
    let mut sql = String::from(
        "PRAGMA page_size=4096; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;\n\
         CREATE TABLE goals (session_id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, \
         objective TEXT NOT NULL, status TEXT NOT NULL, percent_complete INTEGER NOT NULL);\n\
         BEGIN;\n",
    );
    for i in 0..=500u32 {
        let sid = format!("019a0000-0000-7000-8000-{i:012x}");
        let st = if sid == target { "active" } else { "complete" };
        sql.push_str(&format!(
            "INSERT INTO goals VALUES ('{sid}', 'g-{i}', 'goal {i} {pad}', '{st}', 50);\n"
        ));
    }
    sql.push_str("COMMIT;\n");
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .arg(&sql),
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .expect("sqlite3 seed");
    assert!(out.status.success(), "{out:?}");

    // Scanning 500 WAL-backed rows must reuse one row buffer and borrow inline B-tree cells
    // without per-row allocations (< 128 allocations total).
    let got = within("goal::read_store", 2 * 1024 * 1024, 128, || {
        read_store(Store::Muse, &db, target)
    })
    .expect("read_store ok")
    .expect("active goal");
    assert!(got.starts_with("goal 500 "));
}
