// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Robustness / fuzz lane (the audit's #1 cheap assurance win, on stock Rust).
// A terminal MUST NOT panic, hang, or go out-of-bounds on adversarial output —
// hostile programs emit malformed CSI/OSC/DCS/UTF-8 on purpose. proptest throws
// thousands of crafted byte streams at `Terminal::process`; any panic is a real
// bug. (cargo-fuzz/libFuzzer is a later nightly addition; this runs in CI today.)

use aterm_core::terminal::Terminal;
use proptest::prelude::*;

/// Bytes biased toward escape-sequence machinery, to actually stress the parser
/// (uniform random bytes are almost all printable and barely exercise it).
fn escapey_byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        2 => Just(0x1b_u8),                            // ESC
        2 => prop::sample::select(b"[]P;:?".to_vec()), // CSI/OSC/DCS intro + params
        2 => prop::sample::select(b"0123456789".to_vec()),
        2 => prop::sample::select(b"mHJKABCDfdghlnpqrstu".to_vec()), // final bytes
        1 => prop::sample::select(vec![0x07_u8, 0x08, 0x09, 0x0a, 0x0d, 0x9b, 0x90, 0x9c]),
        1 => any::<u8>(),                              // anything, incl. invalid UTF-8
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// No panic / no hang on arbitrary bytes.
    #[test]
    fn never_panics_on_arbitrary_bytes(input in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut t = Terminal::new(24, 80);
        t.process(&input);
        let _ = t.visible_content(); // reading the model must also not panic
    }

    /// No panic on escape-sequence-dense streams (the real parser stress).
    #[test]
    fn never_panics_on_escape_dense(input in prop::collection::vec(escapey_byte(), 0..4096)) {
        let mut t = Terminal::new(24, 80);
        t.process(&input);
        let _ = t.visible_content();
    }

    /// Splitting input across process() calls must not panic — escape sequences
    /// straddling a chunk boundary are a classic bug class.
    #[test]
    fn chunked_feed_never_panics(
        input in prop::collection::vec(escapey_byte(), 0..2048),
        splits in prop::collection::vec(1usize..64, 0..64),
    ) {
        let mut t = Terminal::new(24, 80);
        let mut rest = &input[..];
        let mut si = 0;
        while !rest.is_empty() {
            let n = splits.get(si).copied().unwrap_or(7).min(rest.len());
            let (head, tail) = rest.split_at(n);
            t.process(head);
            rest = tail;
            si += 1;
        }
        let _ = t.visible_content();
    }

    /// Tiny grids + heavy escapes (boundary arithmetic on cursor/scroll).
    #[test]
    fn tiny_grid_never_panics(
        input in prop::collection::vec(escapey_byte(), 0..1024),
        rows in 1u16..4, cols in 1u16..4,
    ) {
        let mut t = Terminal::new(rows, cols);
        t.process(&input);
        let _ = t.visible_content();
    }
}

/// A corpus of hand-picked nasties that must each be survived.
#[test]
fn known_adversarial_corpus_is_survived() {
    let corpus: &[&[u8]] = &[
        b"\x1b[",                      // incomplete CSI
        b"\x1b[999999999999999999m",   // overflowing param
        b"\x1b[;;;;;;;;;;;;;;;;;;;;m", // many empty params
        b"\x1b]0;",                    // unterminated OSC
        b"\x1b]8;;\x07",               // empty hyperlink
        b"\x1bP+q\x1b\\",              // DCS
        b"\xff\xfe\xfd",               // invalid UTF-8
        b"\xf0\x28\x8c\x28",           // invalid UTF-8 sequence
        b"\x1b[1;1H\x1b[999;999H",     // out-of-range cursor moves
        b"\x1b[r\x1b[?1049h\x1b[2J",   // reset region + alt + clear
        b"\x08\x08\x08\x08",           // backspaces at col 0
        b"\x1b[100S\x1b[100T",         // huge scroll up/down
    ];
    for bytes in corpus {
        let mut t = Terminal::new(24, 80);
        t.process(bytes);
        let _ = t.visible_content();
    }
}
