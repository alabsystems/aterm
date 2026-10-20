// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE SESSION LOG'S STEP TAP COSTS THE FOOTER THREAD (Phase 3B, plan
//! §9 M1) — a benchmark, not a gate: it writes a 64 MiB synthetic
//! transcript (Claude Code's row shapes, a tool mix, values made up) into a
//! scratch directory and prints how long the usage fold takes to read it
//! from the first byte (`SessionUsage::refresh`), without the step tap and
//! with it, two ways: bound to the file as it is (every row history: the
//! first read of a transcript that already exists) and seeded at offset 0
//! (every row past the watermark, queued and shed past the queue's bound:
//! the worst case). Run it by hand, release:
//!
//! ```text
//! targo --unverified bench -p aterm-agent --bench step_tap_cost
//! ```

use std::io::Write as _;
use std::time::{Duration, Instant};

use aterm_agent::harness::session_usage::SessionUsage;
use aterm_agent::harness::steps::{StepFold, StepMark};

/// The transcript's size.
const BYTES: usize = 64 * 1024 * 1024;
/// Reads timed per lane (the best is reported).
const RUNS: usize = 5;

fn stamp(i: usize) -> String {
    format!(
        "2026-10-15T{:02}:{:02}:{:02}.{:03}Z",
        (i / 3600) % 24,
        (i / 60) % 60,
        i % 60,
        i % 1000
    )
}

/// One vendor turn's rows: a prompt, a call with its input, its result (the
/// bulk of a real transcript's bytes), a reply, the turn's end.
fn turn(i: usize, out: &mut Vec<u8>) {
    let input = "cat src/lib.rs | head -n 40 && ".repeat(8 + i % 24);
    let result = r#"fn main() { println!(\"hello\"); }\n"#.repeat(60 + (i * 7) % 400);
    let tool = ["Bash", "Read", "Edit", "Grep", "Task", "mcp__x__y"][i % 6];
    let t = stamp(i);
    let rows = [
        format!(
            r#"{{"parentUuid":"p{i}","isSidechain":false,"promptId":"pr-{i}","type":"user","message":{{"role":"user","content":"please look at the parser and fix the bug number {i}"}},"uuid":"u-{i}","timestamp":"{t}","version":"2.1.300","cwd":"/work"}}"#
        ),
        format!(
            r#"{{"parentUuid":"u-{i}","isSidechain":false,"type":"assistant","message":{{"id":"msg-{i}a","model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"Looking."}},{{"type":"tool_use","id":"toolu_{i}","name":"{tool}","input":{{"command":"{input}"}}}}],"usage":{{"input_tokens":3,"output_tokens":120,"cache_creation_input_tokens":2048,"cache_read_input_tokens":40960}}}},"uuid":"a-{i}","timestamp":"{t}","effort":"high","version":"2.1.300"}}"#
        ),
        format!(
            r#"{{"parentUuid":"a-{i}","isSidechain":false,"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_{i}","is_error":false,"content":"{result}"}}]}},"uuid":"r-{i}","timestamp":"{t}","toolUseResult":{{"stdout":"{result}"}}}}"#
        ),
        format!(
            r#"{{"parentUuid":"r-{i}","isSidechain":false,"type":"assistant","message":{{"id":"msg-{i}b","model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"Fixed: the loop now stops at the end of input, and the test covers it."}}],"usage":{{"input_tokens":3,"output_tokens":300,"cache_creation_input_tokens":512,"cache_read_input_tokens":43008}}}},"uuid":"b-{i}","timestamp":"{t}","effort":"high"}}"#
        ),
        format!(
            r#"{{"parentUuid":"b-{i}","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":{},"timestamp":"{t}","uuid":"d-{i}","isMeta":false}}"#,
            4000 + i % 9000
        ),
    ];
    for r in rows {
        out.extend_from_slice(r.as_bytes());
        out.push(b'\n');
    }
}

fn best(runs: &mut [Duration]) -> Duration {
    runs.sort_unstable();
    runs[0]
}

fn main() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-step-tap-cost-")
        .tempdir()
        .expect("a scratch dir");
    let path = dir.path().join("session.jsonl");
    let mut body = Vec::with_capacity(BYTES + 64 * 1024);
    let mut turns = 0;
    while body.len() < BYTES {
        turn(turns, &mut body);
        turns += 1;
    }
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&body))
        .expect("write the transcript");
    let lines = body.iter().filter(|&&b| b == b'\n').count();
    let mb = body.len() as f64 / (1024.0 * 1024.0);
    drop(body);

    // The file's key, as the cursor binds a fold to it.
    let key = {
        let mut usage = SessionUsage::new(&path);
        let mut tap = StepFold::new(0, None);
        usage.refresh_tapped(0, Some(&mut tap));
        let bind = tap.take().steps.pop().expect("a bind");
        StepMark {
            fp: bind.pos.fp,
            id: bind.pos.id,
            off: 0,
        }
    };
    let mut plain = Vec::new();
    let mut history = Vec::new();
    let mut fresh = Vec::new();
    let mut queued = (0, 0);
    for _ in 0..RUNS {
        let mut usage = SessionUsage::new(&path);
        let t = Instant::now();
        let r = usage.refresh(u64::MAX);
        plain.push(t.elapsed());
        assert!(r.caught_up);

        for (given, lane) in [(None, &mut history), (Some(key), &mut fresh)] {
            let mut usage = SessionUsage::new(&path);
            let mut tap = StepFold::new(0, given);
            let t = Instant::now();
            let r = usage.refresh_tapped(u64::MAX, Some(&mut tap));
            lane.push(t.elapsed());
            assert!(r.caught_up);
            let batch = tap.take();
            assert_eq!(batch.lost, 0, "every synthetic row is one the fold reads");
            let n = batch.steps.len() as u64 + batch.overflow;
            if given.is_some() {
                queued.1 = n;
            } else {
                queued.0 = n;
            }
        }
    }
    let p = best(&mut plain);
    println!("M1 step tap: {mb:.1} MiB, {lines} lines, {turns} turns; best of {RUNS}");
    let rate = |d: Duration| (d.as_secs_f64() * 1e3, mb / d.as_secs_f64());
    let (ms, rt) = rate(p);
    println!("  usage fold alone        : {ms:8.1} ms  {rt:7.1} MiB/s");
    for (name, lane, n) in [
        ("tapped, all history     ", &mut history, queued.0),
        ("tapped, every row fresh ", &mut fresh, queued.1),
    ] {
        let t = best(lane);
        let (ms, rt) = rate(t);
        let share = (t.as_secs_f64() - p.as_secs_f64()) / t.as_secs_f64();
        println!(
            "  {name}: {ms:8.1} ms  {rt:7.1} MiB/s  {n} steps  tap share {:.1}%",
            share * 100.0
        );
    }
    // Per turn a prompt, a call, its result and the turn's end; once, the
    // bind and the model.
    assert_eq!(queued, (1, 4 * turns as u64 + 2), "the steps the rows hold");
}
