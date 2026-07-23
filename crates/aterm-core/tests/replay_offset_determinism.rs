// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The replay / lash-mirror determinism seed.
//!
//! This is the falsifiable seed behind the astream "deterministic replay
//! substrate" thesis applied to a terminal: **the screen is a pure function of
//! the ordered output-byte log.** A session is recorded as an ordered list of
//! output records (the bytes a PTY emitted — exactly the `BYTES` frames an
//! aterm `subscribe` stream carries). We feed that log into the SHIPPING engine
//! via the injected-clock seam ([`Terminal::process_at`] + a FIXED
//! [`ClockReading`]) so the result cannot depend on real wall-clock pacing.
//!
//! Two things are proven, both bit-exact:
//!
//! 1. **Offset-addressed replay == live.** For every prefix offset N, a fresh
//!    engine fed the first N records reproduces the live engine's full captured
//!    projection ([`Terminal::checkpoint`]) and rendered text
//!    ([`Terminal::visible_content`]). The Nth state is a pure function of
//!    records[0..N] alone — nothing else leaks in. This is also the correctness
//!    of a *lashed* viewer: a viewer engine fed the host's output log mirrors it.
//!
//! 2. **Coalescing-independence.** The same bytes split at ANY boundaries —
//!    one shot, per original record, per single byte, or mid-escape-sequence —
//!    fold to the identical final state. A lash relay may re-chunk the host's
//!    output (coalesce or fragment `BYTES` frames) and the mirror is unchanged.
//!
//! A pinned FNV-1a hash of the final rendered text demonstrates drift detection
//! (machine-independent: text only), and a negative control (drop one record)
//! MUST diverge, so the determinism assertions are non-vacuous.

use aterm_core::terminal::{ClockReading, HostBindings, Terminal};

const ROWS: u16 = 12;
const COLS: u16 = 40;

/// A recorded output-byte log: the ordered records a PTY emitted. Deliberately
/// exercises the determinism hazards (SGR, cursor moves, scroll region, wide
/// CJK, a combining mark, alt-screen round-trip, autowrap toggle, OSC title,
/// tab stops). The concatenation ends at parser-ground (required by
/// `checkpoint()`); individual record boundaries may fall mid-sequence.
const SESSION: &[&[u8]] = &[
    b"\x1b[1;38;5;202mboot\x1b[0m\r\n",
    b"plain line one\r\nplain line two\r\n",
    b"\x1b[2;9r",                                           // DECSTBM scroll region
    b"\x1b[3;1Hwide: \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e", // CJK (full-width)
    b" combine: e\xcc\x81\r\n",                             // 'e' + U+0301 combining acute
    b"\x1b[7mreverse\x1b[0m\tafter-tab\r\n",
    b"row\r\nrow\r\nrow\r\nrow\r\n", // scroll content through the region
    b"\x1b[?1049h",                  // enter alt screen
    b"\x1b[2J\x1b[1;1Halt body",
    b"\x1b[?1049l",          // leave alt screen (main restored)
    b"\x1b]0;the-title\x07", // OSC 0 window title
    b"\x1b[?7l",             // autowrap off (a captured mode bit)
    b"\x1b[10;1Hlast",
];

/// A single fixed clock reading reused for every batch, so any time-dependent
/// path (bell rate-limit, mode-2026 timeout) observes a zero delta and the fold
/// is bit-deterministic — for both the live and the replay timelines.
fn fixed_clock() -> ClockReading {
    ClockReading {
        monotonic: std::time::Instant::now(), // CLOCK-EXEMPT: captured once; reused for all batches so deltas are zero (determinism)
        wall_ms: Some(0),
    }
}

fn fresh() -> Terminal {
    Terminal::new(ROWS, COLS)
}

/// Feed a sequence of byte chunks through the fixed-clock seam.
fn feed(term: &mut Terminal, chunks: &[&[u8]], clock: ClockReading) {
    for c in chunks {
        term.process_at(c, clock);
    }
}

/// Stable, version-independent FNV-1a-64 over bytes (so the golden pin does not
/// drift with the std hasher).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[test]
fn replay_from_zero_mirrors_live_at_every_offset() {
    let clock = fixed_clock();

    // The live host: one engine fed the records one at a time. Capture its full
    // projection after each record.
    let mut live = fresh();
    let mut live_states = Vec::new();
    for rec in SESSION {
        live.process_at(rec, clock);
        live_states.push(live.checkpoint());
    }

    // For every offset N, a FRESH replay engine fed records[0..N] must reproduce
    // the live host's state at N — exactly. The state at N is a pure function of
    // the first N records and nothing else.
    for n in 1..=SESSION.len() {
        let mut replay = fresh();
        feed(&mut replay, &SESSION[..n], clock);
        assert_eq!(
            replay.checkpoint(),
            live_states[n - 1],
            "replay of the first {n} records must mirror the live projection at offset {n}"
        );
    }

    // And the rendered text agrees at head (human-readable cross-check).
    let mut replay_full = fresh();
    feed(&mut replay_full, SESSION, clock);
    assert_eq!(replay_full.visible_content(), live.visible_content());
}

#[test]
fn coalescing_chunk_boundaries_do_not_change_state() {
    let clock = fixed_clock();

    // Reference: the original record boundaries.
    let mut by_record = fresh();
    feed(&mut by_record, SESSION, clock);
    let reference = by_record.checkpoint();

    let flat: Vec<u8> = SESSION.iter().flat_map(|r| r.iter().copied()).collect();

    // (a) one shot — the whole log in a single process call.
    let mut one_shot = fresh();
    one_shot.process_at(&flat, clock);
    assert_eq!(
        one_shot.checkpoint(),
        reference,
        "feeding the whole log at once must match the per-record fold"
    );

    // (b) one byte at a time — the finest possible fragmentation, splitting
    // many escape sequences mid-stream; the streaming parser must carry state
    // across calls and still land identically.
    let mut per_byte = fresh();
    for b in &flat {
        per_byte.process_at(std::slice::from_ref(b), clock);
    }
    assert_eq!(
        per_byte.checkpoint(),
        reference,
        "byte-at-a-time fragmentation must match the per-record fold"
    );

    // (c) an adversarial fixed split at offset 7 (lands inside the first SGR
    // sequence) then the remainder — a relay re-chunking output.
    let cut = 7.min(flat.len());
    let mut split = fresh();
    split.process_at(&flat[..cut], clock);
    split.process_at(&flat[cut..], clock);
    assert_eq!(
        split.checkpoint(),
        reference,
        "a mid-sequence re-chunk must match the per-record fold"
    );
}

#[test]
fn golden_visible_content_hash_is_pinned() {
    let clock = fixed_clock();
    let mut term = fresh();
    feed(&mut term, SESSION, clock);
    let hash = fnv1a_64(term.visible_content().as_bytes());
    // Pinned the day it was real; a change to the engine's rendered text for
    // this fixed log will trip this (drift detection, machine-independent).
    assert_eq!(
        hash, GOLDEN_VISIBLE_FNV1A,
        "rendered text drifted for the fixed session log; \
         if intentional, re-pin GOLDEN_VISIBLE_FNV1A to {hash:#018x}"
    );
}

/// Pinned FNV-1a-64 of `visible_content()` for `SESSION`. See the test above.
const GOLDEN_VISIBLE_FNV1A: u64 = 0x2981_8d48_0522_c513;

#[test]
fn negative_control_dropping_a_record_diverges() {
    let clock = fixed_clock();
    const DROP: usize = 6; // the "row\r\n..." chunk that scrolls the region

    let mut full = fresh();
    feed(&mut full, SESSION, clock);

    let mut dropped = fresh();
    for (i, rec) in SESSION.iter().enumerate() {
        if i == DROP {
            continue;
        }
        dropped.process_at(rec, clock);
    }

    assert_ne!(
        full.checkpoint(),
        dropped.checkpoint(),
        "dropping a recorded output record MUST diverge (negative control); \
         if this passes, the determinism assertions are vacuous"
    );
}

#[test]
fn from_checkpoint_hydration_continues_the_mirror() {
    // A lash viewer that reattaches mid-stream: snapshot the host at an offset,
    // hydrate a fresh engine from it, then feed the remaining records. It must
    // land where the live host did — reattach is snapshot + tail.
    let clock = fixed_clock();
    const AT: usize = 8; // reattach after the alt-screen enter

    let mut live = fresh();
    feed(&mut live, SESSION, clock);

    let mut host_at = fresh();
    feed(&mut host_at, &SESSION[..AT], clock);
    let snapshot = host_at.checkpoint();

    let mut viewer = Terminal::from_checkpoint(&snapshot, HostBindings::none());
    feed(&mut viewer, &SESSION[AT..], clock);

    assert_eq!(
        viewer.checkpoint(),
        live.checkpoint(),
        "reattach (hydrate at offset {AT} + tail the rest) must reach the live state"
    );
}
