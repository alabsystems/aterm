// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified bench -p aterm-gui --bench job_chunk_cost`.
//! Manual measurement, with no timing assertion and no live terminal state.

#[path = "../src/vitals_jobs/lines.rs"]
mod lines;
#[path = "../src/vitals_jobs/lines/reference.rs"]
mod reference;

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
    use std::hint::black_box;
    for (name, bytes) in [
        ("short", b"hello\r\n".to_vec()),
        (
            "plain_4k",
            b"0123456789abcdefghijklmnopqrstuvwxyz\r\n".repeat(108),
        ),
        (
            "plain_64k",
            b"0123456789abcdefghijklmnopqrstuvwxyz\r\n".repeat(1725),
        ),
        ("color_4k", b"\x1b[32mtest passed\x1b[0m\r\n".repeat(178)),
        (
            "draw_4k",
            b"\x1b[12;1Htests/test_api.py PASSED\x1b[K\r\n".repeat(117),
        ),
        ("crlf_4k", b"\r\n".repeat(2048)),
        ("no_lf_4k", vec![b'x'; 4096]),
        ("cr_only_4k", vec![b'\r'; 4096]),
        (
            "late_csi_4k",
            [vec![b'x'; 4090], b"\x1b[2H\r\n".to_vec()].concat(),
        ),
    ] {
        let rounds = (64 * 1024 * 1024 / bytes.len()).max(1024);
        for original in [true, false] {
            let before = instructions();
            let start = std::time::Instant::now();
            for _ in 0..rounds {
                let input = black_box(bytes.as_slice());
                black_box(if original {
                    reference::classify(input)
                } else {
                    let got = lines::Lines::of(input);
                    [got.ends, got.ended, got.tail, got.drawn]
                });
            }
            let ns = start.elapsed().as_nanos();
            let instructions = before.zip(instructions()).map(|(a, b)| b - a);
            println!(
                "case={name} original={original} bytes={} rounds={rounds} ns={ns} instructions={instructions:?}",
                bytes.len()
            );
        }
    }
}
