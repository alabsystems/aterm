// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::terminal::{ClockReading, GONE_SETTLE, RowRange, Terminal};
use std::time::{Duration, Instant};

fn clock_at(base: Instant, off_ms: u64) -> ClockReading {
    ClockReading {
        monotonic: base + Duration::from_millis(off_ms),
        wall_ms: Some(off_ms),
    }
}

#[test]
fn row_matcher_latches_on_real_engine_output() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    let m = row_matcher(r"PROMPT-READY").expect("compile");
    let id = t.watch_rows(m, RowRange::All, base).expect("arm");
    assert!(t.watch_poll(id).is_none(), "pending before the row appears");

    t.process_at(b"working...\r\n", clock_at(base, 10));
    assert!(
        t.watch_poll(id).is_none(),
        "non-matching output does not latch"
    );

    t.process_at(b"PROMPT-READY\r\n", clock_at(base, 20));
    let sat = t
        .watch_poll(id)
        .expect("matching row latched on the real surface");
    assert!(sat.seq > 0);
}

#[test]
fn row_matcher_latches_immediately_if_already_matching() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    t.process_at(b"ALREADY-HERE\r\n", clock_at(base, 5));
    let m = row_matcher("ALREADY-HERE").expect("compile");
    let id = t.watch_rows(m, RowRange::All, base).expect("arm");
    assert!(
        t.watch_poll(id).is_some(),
        "an already-matching row latches at arm time"
    );
}

#[test]
fn row_gone_latches_immediately_if_nothing_matches() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    t.process_at(b"Done.\r\n", clock_at(base, 5));
    let drawn = base + Duration::from_millis(5);
    let m = row_matcher("esc to interrupt").expect("compile");
    let id = t
        .watch_rows_gone(m.clone(), RowRange::All, drawn + GONE_SETTLE)
        .expect("arm");
    assert!(
        t.watch_poll(id).is_some(),
        "no matching row at arm => latched at arm time"
    );
    let recent = t.watch_rows_gone(m, RowRange::All, drawn).expect("arm");
    assert!(t.watch_poll(recent).is_none(), "drawn just now: settling");
    assert!(t.watch_expire(drawn + GONE_SETTLE));
    assert!(t.watch_poll(recent).is_some());
}

#[test]
fn row_gone_waits_for_the_matching_row_to_leave() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 80);
    t.process_at(
        b"thinking\xe2\x80\xa6 (esc to interrupt)\r\n",
        clock_at(base, 5),
    );
    let m = row_matcher("esc to interrupt").expect("compile");
    let id = t.watch_rows_gone(m, RowRange::All, base).expect("arm");
    assert!(
        t.watch_poll(id).is_none(),
        "the footer is on screen => pending"
    );

    t.process_at(b"still thinking\r\n", clock_at(base, 10));
    assert!(
        t.watch_poll(id).is_none(),
        "content advanced but the row is still visible => still pending"
    );

    t.process_at(b"\x1b[H\x1b[2JDone.\r\n", clock_at(base, 20));
    assert!(t.watch_poll(id).is_none(), "settling");
    assert!(t.watch_expire(base + Duration::from_millis(20) + GONE_SETTLE));
    let sat = t
        .watch_poll(id)
        .expect("the row leaving the real surface latched `gone`");
    assert!(sat.seq > 0);
}

#[test]
fn row_gone_waits_while_a_visible_row_exhausts_the_scan_budget() {
    let base = Instant::now();
    let mut t = Terminal::new(24, 4096);
    let heavy = format!("{}z\r\n", "x".repeat(4000));
    t.process_at(heavy.as_bytes(), clock_at(base, 5));
    let m = row_matcher("(?:x?){1020}z").expect("compile");
    let shown = t
        .watch_rows(m.clone(), RowRange::All, base)
        .expect("arm match");
    let gone = t.watch_rows_gone(m, RowRange::All, base).expect("arm gone");
    assert!(
        t.watch_poll(shown).is_none(),
        "a row the budget refused is not a match"
    );
    assert!(
        t.watch_poll(gone).is_none(),
        "a row the budget refused is not gone: it is still on screen"
    );

    t.process_at(b"still working\r\n", clock_at(base, 10));
    assert!(
        t.watch_poll(gone).is_none(),
        "still unread => still pending"
    );

    t.process_at(b"\x1b[H\x1b[2Jxxxz\r\n", clock_at(base, 20));
    assert!(t.watch_poll(shown).is_some(), "a readable match latches");
    assert!(
        t.watch_poll(gone).is_none(),
        "a readable match holds `gone`"
    );

    t.process_at(b"\x1b[H\x1b[2JDone.\r\n", clock_at(base, 30));
    assert!(t.watch_expire(base + Duration::from_millis(30) + GONE_SETTLE));
    assert!(t.watch_poll(gone).is_some(), "the row left => `gone`");
}

#[test]
fn row_matcher_rejects_overlong_pattern() {
    let at_cap = "a".repeat(MAX_REGEX_PATTERN_LEN);
    assert_eq!(at_cap.len(), MAX_REGEX_PATTERN_LEN);
    assert!(
        row_matcher(&at_cap).is_ok(),
        "a pattern at the cap compiles"
    );

    let over_cap = "a".repeat(MAX_REGEX_PATTERN_LEN + 1);
    assert!(
        row_matcher(&over_cap).is_err(),
        "a pattern one byte over the cap is rejected"
    );
}

#[test]
fn row_matcher_bounds_compiled_size() {
    let amplifier = "(a{200}){200}";
    assert!(
        amplifier.len() <= MAX_REGEX_PATTERN_LEN,
        "under the length gate"
    );
    assert!(
        matches!(
            row_matcher(amplifier),
            Err(aterm_regex::Error::CompiledTooBig(REGEX_SIZE_LIMIT))
        ),
        "the bounded builder must reject a pattern that exceeds 128 KiB compiled",
    );
}

#[test]
fn ordinary_patterns_never_reach_the_scan_budget() {
    let matching = "PROMPT-READY 66390b5c8f user@example.com 192.168.0.1 ERROR ".repeat(70);
    let clear = ".".repeat(4096);
    for pattern in [
        r"PROMPT-READY",
        r"\b[0-9a-f]{7,40}\b",
        r"\S+@\S+",
        r"(?i)error|warn",
    ] {
        let re = bounded_regex(pattern).expect("compiles");
        assert_eq!(re.try_is_match(&matching), Ok(true), "{pattern:?}");
        assert_eq!(re.try_is_match(&clear), Ok(false), "{pattern:?}");
        assert!(
            !re.step_limit_exceeded(),
            "{pattern:?} exceeded the scan budget"
        );
        let m = row_matcher(pattern).expect("compiles");
        assert!(m.matches(&matching), "{pattern:?}");
        assert!(m.clears(&clear), "{pattern:?}");
    }
}

/// NEGATIVE CONTROL: wrong-length frames, corrupted `"OBSB"`/`"OBSV"` magic bytes, zero or
/// over-cap budget limits, and out-of-range verdict tags (`3`, `0xFF`) must all decode to
/// `None`.
#[test]
fn watcher_budget_and_row_verdict_wire_roundtrip() {
    for (mp, sz, st) in [(1, 1, 1), (1024, 128 * 1024, 1 << 22)] {
        let bw = encode_watcher_budget_wire(mp, sz, st);
        assert_eq!(decode_watcher_budget_wire(&bw), Some((mp, sz, st)));
    }
    let bw = encode_watcher_budget_wire(1024, 128 * 1024, 1 << 22);
    assert!(decode_watcher_budget_wire(&[]).is_none());
    assert!(decode_watcher_budget_wire(&bw[..19]).is_none());
    let mut bw_trailing = bw.to_vec();
    bw_trailing.push(0);
    assert!(
        decode_watcher_budget_wire(&bw_trailing).is_none(),
        "fixed 20-byte budget frames reject trailing bytes"
    );
    for idx in 0..4 {
        let mut bad_magic = bw;
        bad_magic[idx] ^= 0xFF;
        assert!(decode_watcher_budget_wire(&bad_magic).is_none());
    }
    for (mp, sz, st) in [
        (0, 128 * 1024, 1 << 22),
        (1024, 0, 1 << 22),
        (1024, 128 * 1024, 0),
        (1025, 128 * 1024, 1 << 22),
        (1024, 128 * 1024 + 1, 1 << 22),
        (1024, 128 * 1024, (1 << 22) + 1),
    ] {
        let bad = encode_watcher_budget_wire(mp, sz, st);
        assert!(
            decode_watcher_budget_wire(&bad).is_none(),
            "out-of-range budget ({mp}, {sz}, {st}) must be rejected"
        );
    }

    for verdict in [
        RowVerdict::NoMatch,
        RowVerdict::Match,
        RowVerdict::Undecided,
    ] {
        for (row_idx, plen, seq) in [(0, 0, 0), (12, 16, 99), (u16::MAX, u8::MAX, u64::MAX)] {
            let vw = encode_row_verdict_wire(verdict, row_idx, plen, seq);
            assert_eq!(
                decode_row_verdict_wire(&vw),
                Some((verdict, row_idx, plen, seq))
            );
        }
        let vw = encode_row_verdict_wire(verdict, 12, 16, 99);
        assert!(decode_row_verdict_wire(&[]).is_none());
        assert!(decode_row_verdict_wire(&vw[..15]).is_none());
        let mut vw_trailing = vw.to_vec();
        vw_trailing.push(0);
        assert!(
            decode_row_verdict_wire(&vw_trailing).is_none(),
            "fixed 16-byte verdict frames reject trailing bytes"
        );
        for idx in 0..4 {
            let mut bad_magic = vw;
            bad_magic[idx] ^= 0xFF;
            assert!(decode_row_verdict_wire(&bad_magic).is_none());
        }
    }
    let base_vw = encode_row_verdict_wire(RowVerdict::Match, 12, 16, 99);
    for bad_tag in [3u8, 0xFF] {
        let mut bad = base_vw;
        bad[4] = bad_tag;
        assert!(
            decode_row_verdict_wire(&bad).is_none(),
            "verdict tag {bad_tag} must be rejected"
        );
    }
}
