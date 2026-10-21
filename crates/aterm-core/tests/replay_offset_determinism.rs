// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Replay / lash-mirror reattach: a viewer that hydrates from a host's
//! checkpoint mid-stream and is fed the rest of the ordered output-byte log
//! lands on the live host's state, bit-exact. The log is fed through the
//! injected-clock seam ([`Terminal::process_at`] + a fixed [`ClockReading`]) so
//! wall-clock pacing cannot matter. (Chunk-boundary independence and fold
//! stability are pinned over the adversarial corpora in
//! `replay_corpus_probe.rs`.)

use aterm_core::terminal::{ClockReading, Terminal};

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
#[expect(
    clippy::disallowed_methods,
    reason = "test code: the one instant a test mints and feeds in"
)]
fn fixed_clock() -> ClockReading {
    ClockReading {
        monotonic: std::time::Instant::now(), // captured once; reused for all batches so deltas are zero (determinism)
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

    let mut viewer = Terminal::from_checkpoint(&snapshot);
    feed(&mut viewer, &SESSION[AT..], clock);

    // The viewer is a restore the tail reached before any host adopted it: its
    // checkpoint carries that lineage's seed uncertainty
    // (`HolderRepr::seed_untrusted`), which the live host never has.
    let reattached = viewer.checkpoint();
    assert!(
        reattached.holder.seed_untrusted,
        "a restore output reached before adoption carries its lineage"
    );
    assert_eq!(
        reattached.without_seed_lineage(),
        live.checkpoint(),
        "reattach (hydrate at offset {AT} + tail the rest) must reach the live state"
    );
}
