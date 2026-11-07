// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual scheduling work measurement. No live filesystem or timing assertion.

use aterm_uds::file_watch::{Debounce, QUIET};
use std::hint::black_box;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: this flavor writes the complete buffer, read only on success.
    let result = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: success initialized the selected rusage buffer.
    (result == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

fn main() {
    for files in [1u64, 64, 1024, 16384] {
        let cycles = (4096 / files).max(1);
        let start = Instant::now();
        let before = instructions();
        let clock = Instant::now();
        for _ in 0..cycles {
            let mut debounce = Debounce::default();
            for key in 0..files {
                debounce.note(black_box(key), black_box(start));
            }
            let due = debounce.due(start + Duration::from_secs(1));
            assert_eq!(due.len(), files as usize);
            assert_eq!(debounce.deadline(), None);
            black_box(due);
        }
        let elapsed = clock.elapsed();
        let count = before.zip(instructions()).map(|(a, b)| b - a);
        println!(
            "files={files} lifecycle=true cycles={cycles} ns={} instructions={count:?}",
            elapsed.as_nanos()
        );
        for update in [false, true] {
            let start = Instant::now();
            let mut debounce = Debounce::default();
            for key in 0..files {
                debounce.note(key, start);
            }
            let rounds = 4096u64;
            let at = start + QUIET / 2;
            let before = instructions();
            let clock = Instant::now();
            for turn in 0..rounds {
                if update {
                    debounce.note(
                        black_box(turn % files),
                        black_box(at + Duration::from_nanos(turn)),
                    );
                }
                black_box(debounce.deadline());
                assert!(debounce.due(black_box(at)).is_empty());
            }
            let elapsed = clock.elapsed();
            let count = before.zip(instructions()).map(|(a, b)| b - a);
            let due = debounce.due(start + Duration::from_secs(1));
            assert_eq!(due, (0..files).collect::<Vec<_>>());
            println!(
                "files={files} update={update} rounds={rounds} ns={} instructions={count:?}",
                elapsed.as_nanos()
            );
        }
    }
}
