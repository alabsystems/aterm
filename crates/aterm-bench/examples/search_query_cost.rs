// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Manual cached literal-query work and allocation measurement, separate from timing floors.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_bench::{linkheavy_corpus, replog_corpus, rotating_corpus};
use aterm_core::search::SearchDirection;
use aterm_core::terminal::TerminalBuilder;

static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;

fn note(bytes: usize) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

// SAFETY: every operation forwards the caller's pointer and layout to System unchanged;
// the counters use only nonallocating atomic arithmetic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        // SAFETY: forwarding the caller's layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        note(size);
        // SAFETY: ptr and layout came from System through this allocator.
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr and layout came from System through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    let pid = libc::c_int::try_from(std::process::id()).ok()?;
    // SAFETY: info is writable for the requested rusage_info_v4 flavor.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    // SAFETY: zeroed at creation, so initialized whatever the call did.
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

fn main() {
    const REPEATS: usize = 20;
    for (shape, corpus, common, short) in [
        ("rotating", rotating_corpus(50_000), "jkl", "j"),
        ("replog", replog_corpus(50_000), "ERROR", "E"),
        ("linkheavy", linkheavy_corpus(25_000), "tgz", "t"),
        (
            "unicode",
            "日E\u{301} E\u{301} 日E\u{301}\r\n"
                .repeat(10_000)
                .into_bytes(),
            "E\u{301}",
            "日",
        ),
    ] {
        let mut term = TerminalBuilder::new()
            .size(24, 80)
            .ring_buffer_size(60_000)
            .build();
        term.process(&corpus);
        let _ = term.indexed_search();
        let rebuilds = term.search_index_rebuilds();
        for (query_shape, query) in [
            ("common", common),
            ("short", short),
            ("absent", "@!absent!@"),
        ] {
            for direction in [SearchDirection::Forward, SearchDirection::Backward] {
                for access in ["terminal", "index"] {
                    let expected = term
                        .indexed_search()
                        .search_results_opts_direction(query, true, false, direction)
                        .expect("literal query");
                    let count = expected.matches.len();
                    if query_shape == "absent" {
                        assert_eq!(count, 0);
                    } else {
                        assert!(count > 0);
                    }
                    let before_calls = CALLS.load(Ordering::Relaxed);
                    let before_bytes = BYTES.load(Ordering::Relaxed);
                    let before = instructions();
                    if access == "terminal" {
                        for _ in 0..REPEATS {
                            let result = term
                                .indexed_search()
                                .search_results_opts_direction(
                                    black_box(query),
                                    true,
                                    false,
                                    direction,
                                )
                                .expect("literal query");
                            black_box(result);
                        }
                    } else {
                        let index = term.indexed_search();
                        for _ in 0..REPEATS {
                            let result = index
                                .search_results_opts_direction(
                                    black_box(query),
                                    true,
                                    false,
                                    direction,
                                )
                                .expect("literal query");
                            black_box(result);
                        }
                    }
                    let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                    let calls = CALLS.load(Ordering::Relaxed) - before_calls;
                    let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
                    assert_eq!(
                        term.search_index_rebuilds(),
                        rebuilds,
                        "queries must reuse the index"
                    );
                    let actual = term
                        .indexed_search()
                        .search_results_opts_direction(query, true, false, direction)
                        .expect("literal query");
                    assert_eq!(actual, expected, "measurement must not mutate results");
                    println!(
                        "search_query shape={shape} query={query_shape} direction={direction:?} access={access} repeats={REPEATS} matches={count} instructions={work:?} allocation_calls={calls} allocated_bytes={bytes}"
                    );
                }
            }
        }
    }
}
