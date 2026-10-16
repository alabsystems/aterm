// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{WriteStep, classify_write_result, write_all};
use std::path::{Path, PathBuf};

/// The bounded buffer length the conformance trajectory uses (matches the cfg
/// `Size` we pin below). Small so the ty runs are cheap.
const SIZE: i64 = 5;

/// The committed `WriteAll.tla`, with `Init` PARAMETERIZED (`off = off_init /\
/// done = done_init`) so any predecessor state can be the strict first step. The
/// rewrite touches ONLY the `CONSTANTS` line (adds `off_init, done_init`) and the
/// `Init ==` line; every `Progress`/`Interrupted`/`Next`/invariant line is the
/// committed text verbatim, so the actions cannot drift from the checked spec.
fn parameterized_spec() -> String {
    let committed = std::fs::read_to_string(spec_path("WriteAll.tla")).expect("read WriteAll.tla");
    let mut out = String::new();
    for line in committed.lines() {
        let t = line.trim_start();
        if t.starts_with("CONSTANTS Size, Buggy") {
            out.push_str("CONSTANTS Size, Buggy, off_init, done_init\n");
        } else if t.starts_with("Init ==") {
            out.push_str("Init == off = off_init /\\ done = done_init\n");
        } else if t.starts_with("/\\ off = 0") || t.starts_with("/\\ done = FALSE") {
            // The two old Init conjunct lines — dropped (subsumed by the rewrite).
            continue;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn spec_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("aterm-spec-models/specs")
        .join(name)
}

fn transition_trace(prev: (i64, bool), next: (i64, bool), action: &str) -> String {
    let st = |off: i64, done: bool| {
        format!(
            "{{\"off\":{{\"type\":\"int\",\"value\":{off}}},\"done\":{{\"type\":\"bool\",\"value\":{done}}}}}"
        )
    };
    format!(
        "{{\"version\":\"1\",\"module\":\"WriteAll\",\"variables\":[\"off\",\"done\"],\"steps\":[\
         {{\"index\":0,\"state\":{}}},\
         {{\"index\":1,\"state\":{},\"action\":{{\"name\":\"{}\"}}}}\
         ]}}",
        st(prev.0, prev.1),
        st(next.0, next.1),
        action
    )
}

fn validate(
    ty: &Path,
    dir: &Path,
    spec: &str,
    prev: (i64, bool),
    next: (i64, bool),
    action: &str,
) -> (bool, String) {
    let spec_f = dir.join("WriteAll.tla");
    let cfg_f = dir.join("WriteAll.cfg");
    let trace_f = dir.join("t.json");
    std::fs::write(&spec_f, spec).expect("write spec");
    std::fs::write(
        &cfg_f,
        format!(
            "CONSTANT Size = {SIZE}\nCONSTANT Buggy = FALSE\n\
             CONSTANT off_init = {}\nCONSTANT done_init = {}\n\
             SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\n",
            prev.0,
            if prev.1 { "TRUE" } else { "FALSE" } // TLA+ booleans are UPPERCASE
        ),
    )
    .expect("write cfg");
    std::fs::write(&trace_f, transition_trace(prev, next, action)).expect("write trace");
    let out = aterm_spec::verify::TyCommand::trace_validate(ty, &trace_f, &spec_f, &cfg_f)
        .output()
        .expect("run ty trace validate");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Replay the REAL `write_all` loop's `off`/`done` trajectory over a scripted
/// sequence of `write(2)` returns, using the REAL [`classify_write_result`]
/// decision and the REAL `data = &data[n..]` advance. Returns the sequence of
/// `(off, done, action_label)` transitions the loop takes. `done` is set on the
/// step that exhausts the buffer (`data.is_empty()`), exactly as `write_all`'s
/// `while !data.is_empty()` exit reports completion.
fn replay(returns: &[(isize, bool)]) -> Vec<(i64, bool, &'static str)> {
    let total = SIZE;
    let mut off: i64 = 0;
    let mut steps = Vec::new();
    for &(r, is_eintr) in returns {
        if off >= total {
            break; // loop already exited (done)
        }
        match classify_write_result(r, is_eintr) {
            WriteStep::Retry => {
                // EINTR: off unchanged, not done — the spec's `Interrupted` with k=0.
                steps.push((off, false, "Interrupted"));
            }
            WriteStep::Stop => break, // peer closed / real error — loop ends, not "done"
            WriteStep::Advance(n) => {
                off += n as i64;
                let done = off >= total;
                // A step that finishes the buffer is `Progress`; a short write that
                // does NOT finish is `Interrupted` (advance-but-not-complete).
                let label = if done { "Progress" } else { "Interrupted" };
                steps.push((off, done, label));
            }
        }
    }
    steps
}

#[test]
fn real_write_all_drains_a_large_payload_over_a_pipe() {
    // END-TO-END: a payload far larger than a pipe buffer + a draining reader.
    // The real `write_all` MUST deliver every byte — `NoSilentDrop` end-to-end.
    let mut fds = [0i32; 2];
    // SAFETY: valid 2-element out-array for pipe(2).
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe() failed");
    let (rd, wr) = (fds[0], fds[1]);
    let payload: Vec<u8> = (0..(1 << 20)).map(|i| (i % 251) as u8).collect(); // 1 MiB > pipe buf
    let expect = payload.clone();
    let reader = std::thread::spawn(move || {
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: rd is a valid pipe read end; buf is a valid 4096-byte buffer.
            let n = unsafe { libc::read(rd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            got.extend_from_slice(&buf[..n as usize]);
        }
        // SAFETY: own pipe read end; close once drained.
        unsafe { libc::close(rd) };
        got
    });
    write_all(wr, &payload);
    // SAFETY: own pipe write end; closing signals EOF to the reader.
    unsafe { libc::close(wr) };
    let got = reader.join().expect("reader thread");
    assert_eq!(
        got.len(),
        expect.len(),
        "write_all dropped bytes — NoSilentDrop violated"
    );
    assert_eq!(got, expect, "write_all delivered corrupted/reordered bytes");
}

// Only `ty` reads the hand-written spec: `-- --ignored _under_ty` runs it.
#[test]
#[ignore = "needs Trust ty: `targo --unverified test -- --ignored _under_ty`"]
fn real_write_all_offset_trajectory_conforms_to_writeall_spec_under_ty() {
    let ty = aterm_spec::verify::required_ty();
    let dir = std::env::temp_dir().join(format!("aterm-writeall-conf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = parameterized_spec();

    // A scripted run of `write(2)` returns the real loop classifies+advances:
    //   short write (+2), EINTR retry (0 advance), short write (+1), full tail (+2).
    // Total = SIZE (5). This exercises BOTH spec actions: `Interrupted` (short
    // writes and the EINTR) and `Progress` (the final completing write).
    let returns = [(2isize, false), (-1, true), (1, false), (2, false)];
    let steps = replay(&returns);
    assert_eq!(
        steps.last().map(|s| (s.0, s.1)),
        Some((SIZE, true)),
        "loop must finish at off=Size, done"
    );

    // POSITIVE: each real transition strictly conforms to WriteAll's `Next`.
    let mut prev = (0i64, false);
    let mut validated = 0usize;
    for &(off, done, action) in &steps {
        let next = (off, done);
        let (ok, out) = validate(&ty, &dir, &spec, prev, next, action);
        assert!(
            ok,
            "real {action} transition {prev:?} -> {next:?} must conform to WriteAll\n--- ty ---\n{out}"
        );
        prev = next;
        validated += 1;
    }

    // NEGATIVE CONTROL — the dropped-tail bug: a `Progress` step that claims
    // `done` while `off < Size`. `NoSilentDrop` forbids it; ty MUST reject.
    let (bad_ok, o) = validate(&ty, &dir, &spec, (1, false), (3, true), "Progress");
    assert!(
        !bad_ok,
        "NEGATIVE CONTROL (done with off=3 < Size=5 — dropped tail) MUST be rejected\n--- ty ---\n{o}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "WriteAll Tier-1 conformance: {validated} real loop transitions (short writes + EINTR + \
         completing write) strictly validated against committed WriteAll.tla; dropped-tail \
         negative control rejected."
    );
}
