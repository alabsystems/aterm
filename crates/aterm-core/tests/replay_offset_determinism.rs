// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Checkpoint hydration and log-tail replay determinism test.

use aterm_core::terminal::{ClockReading, Terminal};

const ROWS: u16 = 12;
const COLS: u16 = 40;

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

/// One clock reading reused for every batch, so time-dependent paths (bell rate limit,
/// mode-2026 timeout) see a zero delta.
#[expect(
    clippy::disallowed_methods,
    reason = "test code: the one instant a test mints and feeds in"
)]
fn fixed_clock() -> ClockReading {
    ClockReading {
        monotonic: std::time::Instant::now(),
        wall_ms: Some(0),
    }
}

fn fresh() -> Terminal {
    Terminal::new(ROWS, COLS)
}

fn feed(term: &mut Terminal, chunks: &[&[u8]], clock: ClockReading) {
    for c in chunks {
        term.process_at(c, clock);
    }
}

#[test]
fn from_checkpoint_hydration_continues_the_mirror() {
    let clock = fixed_clock();
    const AT: usize = 8; // reattach after the alt-screen enter

    let mut live = fresh();
    feed(&mut live, SESSION, clock);

    let mut host_at = fresh();
    feed(&mut host_at, &SESSION[..AT], clock);
    let snapshot = host_at.checkpoint();

    let mut viewer = Terminal::from_checkpoint(&snapshot);
    feed(&mut viewer, &SESSION[AT..], clock);

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
