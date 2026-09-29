// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test binary and one test (as `transcript_allocations.rs`, so no other
//! test pollutes the global allocator): the Claude Code footer's reader
//! allocates each [`footer::TAIL_BYTES`] window of a transcript it walks
//! back ONCE.
//!
//! Both of its window walks — the tail read afresh (`TailCache`'s fresh
//! read) and the walk back to a resumed conversation's answer (`rows_back`,
//! under `conversation_model`) — join each window to the head of the row the
//! newer window cut. Until 2026-09-29 the window's buffer was sized to the
//! window alone and that head appended after the read, so every window but
//! the newest was allocated and then grown: a second block of twice the
//! window, and the first copied into it (measured read-only on 2026-09-29
//! over a real 12 MB transcript: `rows_back` over the whole file allocated
//! 31.5 MB for 11.8 MB of transcript).

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_agent::harness::footer::{self, TailCache};

static COUNTING: AtomicBool = AtomicBool::new(false);
/// Blocks of at least half a window allocated fresh.
static WINDOWS: AtomicUsize = AtomicUsize::new(0);
/// Blocks of at least half a window GROWN after they were filled: a
/// window's buffer reallocated.
static GROWN: AtomicUsize = AtomicUsize::new(0);
/// Every byte asked for, fresh or grown.
static BYTES: AtomicUsize = AtomicUsize::new(0);

/// Half a window: no row of this test, no parse of one and no other read of
/// the reader (the head's 64 KiB, the registry's few hundred bytes) comes
/// near it — only a window's buffer does.
const WINDOWISH: usize = (footer::TAIL_BYTES / 2) as usize;

struct Allocator;

// SAFETY: every allocation and deallocation is forwarded unchanged to System.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            if layout.size() >= WINDOWISH {
                WINDOWS.fetch_add(1, Ordering::Relaxed);
            }
        }
        // SAFETY: the caller supplies the valid allocation layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            if layout.size() >= WINDOWISH {
                WINDOWS.fetch_add(1, Ordering::Relaxed);
            }
        }
        // SAFETY: the caller supplies the valid allocation layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            BYTES.fetch_add(size, Ordering::Relaxed);
            if layout.size() >= WINDOWISH {
                GROWN.fetch_add(1, Ordering::Relaxed);
            }
        }
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

/// The process's start (the owner's session, 2026-09-28): the floor.
const START: u64 = 1_790_607_848;
const PID: u32 = 4242;
const SESSION: &str = "00000000-0000-4000-8000-000000000001";

fn stamp(at: u64) -> String {
    aterm_agent::harness::usage::rfc3339_utc(i64::try_from(at).expect("a stamp"))
}

/// A person's message of about a KiB, stamped `at`: it decides neither the
/// model nor the effort, so a walk back goes on past it.
fn user_row(n: usize, at: u64) -> String {
    let words = format!("row {n}: carry on with the refactor. ").repeat(24);
    format!(
        r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"{words}"}}}}"#,
        stamp(at)
    )
}

/// A Claude directory holding process [`PID`]'s registry entry and its
/// session's transcript: a conversation RESUMED from before the process's
/// start — its first rows, a question and an answer by `claude-opus-5-5`,
/// stamped an hour before [`START`] — then about 3.75 windows of the
/// process's own messages. The process was launched with `--resume` onto
/// it, so the footer names the model Claude restored: the answer at the
/// file's very head, which both walks reach only through every window.
fn resumed_conversation(root: &Path) -> (std::path::PathBuf, u64) {
    let claude = root.join("claude");
    let project = claude.join("projects").join("-w-repo");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        claude.join(format!("sessions/{PID}.json")),
        format!(r#"{{"pid":{PID},"sessionId":"{SESSION}","cwd":"/w/repo"}}"#),
    )
    .unwrap();
    let before = START - 3_600;
    let mut body = format!(
        "{}\n{}\n",
        user_row(0, before),
        format_args!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"id":"a0","role":"assistant","model":"claude-opus-5-5","content":[]}}}}"#,
            stamp(before + 5)
        )
    );
    let want = footer::TAIL_BYTES * 15 / 4;
    let mut n = 1;
    while (body.len() as u64) < want {
        body.push_str(&user_row(n, START + n as u64));
        body.push('\n');
        n += 1;
    }
    let mut file = std::fs::File::create(project.join(format!("{SESSION}.jsonl"))).unwrap();
    file.write_all(body.as_bytes()).unwrap();
    (claude, body.len() as u64)
}

#[test]
fn each_window_the_footer_walks_back_is_allocated_once() {
    let root = std::env::temp_dir().join(format!("aterm-footer-windows-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (claude, len) = resumed_conversation(&root);
    let windows = len.div_ceil(footer::TAIL_BYTES) as usize;
    assert_eq!(windows, 4, "the transcript spans four windows");
    assert!(
        len - (windows as u64 - 1) * footer::TAIL_BYTES >= WINDOWISH as u64,
        "its oldest window is counted too"
    );
    let entry = footer::session_of_pid(&claude, PID, Some(START)).expect("the registry");
    let launch = footer::launch_facts(&[
        "claude".to_string(),
        "--resume".to_string(),
        SESSION.to_string(),
    ]);

    COUNTING.store(true, Ordering::Relaxed);
    let facts = footer::facts_for_entry_cached(
        &claude,
        PID,
        Some(START),
        &entry,
        &launch,
        &mut TailCache::default(),
    );
    COUNTING.store(false, Ordering::Relaxed);
    let (fresh, grown, bytes) = (
        WINDOWS.load(Ordering::Relaxed),
        GROWN.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    );
    eprintln!(
        "footer windows: {len} bytes of transcript, {windows} windows a walk: \
         {fresh} window blocks allocated, {grown} grown, {bytes} bytes asked for"
    );

    // Both walks reached the head: the restored answer is named.
    assert_eq!(facts.model.as_deref(), Some("Opus 5.5"), "{facts:?}");
    // The tail read afresh and the walk back to the answer: every window of
    // each allocated, once.
    assert_eq!(fresh, 2 * windows, "one block per window per walk");
    assert_eq!(grown, 0, "no window's buffer grown after its read");
    std::fs::remove_dir_all(&root).unwrap();
}
