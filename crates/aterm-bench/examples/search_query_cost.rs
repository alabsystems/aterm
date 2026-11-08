// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Manual cached literal-query work and allocation measurement, separate from timing floors.

use std::alloc::System;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_bench::allocator::{Observed, Observer};
use aterm_bench::{linkheavy_corpus, replog_corpus, rotating_corpus};
use aterm_core::search::SearchDirection;
use aterm_core::terminal::TerminalBuilder;

static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;

// SAFETY: the callback uses only nonallocating, nonpanicking atomic arithmetic.
unsafe impl Observer for Counting {
    fn requested(&self, bytes: usize) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOC: Observed<System, Counting> = Observed::new(System, Counting);

#[derive(Clone, Copy)]
struct Work {
    instructions: u64,
    cpu_time: u64,
    cycles: u64,
}

#[cfg(target_os = "macos")]
fn snapshot() -> Option<Work> {
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
    if rc != 0 {
        return None;
    }
    // SAFETY: zeroed at creation, so initialized whatever the call did.
    let info = unsafe { info.assume_init() };
    Some(Work {
        instructions: info.ri_instructions,
        cpu_time: info.ri_user_time.saturating_add(info.ri_system_time),
        cycles: info.ri_cycles,
    })
}

#[cfg(not(target_os = "macos"))]
fn snapshot() -> Option<Work> {
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
                    let before = snapshot();
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
                    let samples = before.zip(snapshot());
                    let work = samples.map(|(a, b)| b.instructions.saturating_sub(a.instructions));
                    let cpu_time = samples.map(|(a, b)| b.cpu_time.saturating_sub(a.cpu_time));
                    let cycles = samples
                        .filter(|(a, b)| a.cycles != 0 && b.cycles >= a.cycles)
                        .map(|(a, b)| b.cycles - a.cycles);
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
                        "search_query shape={shape} query={query_shape} direction={direction:?} access={access} repeats={REPEATS} matches={count} instructions={work:?} allocation_calls={calls} allocated_bytes={bytes} cpu_time={cpu_time:?} cycles={cycles:?}"
                    );
                }
            }
        }
    }
}
