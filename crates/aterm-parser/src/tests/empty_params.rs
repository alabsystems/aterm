// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! An empty parameter is a parameter (ECMA-48 §5.4.2): `CSI 1 ; m` is `1`
//! and then the default, which xterm and vte read as bold, then reset. Every
//! empty parameter — leading, between two separators, or after the last `;` —
//! reaches the handler as 0, the value every handler reads as the default.

use super::*;

/// What one parser fed `bytes` dispatched: every CSI (with its subparameter
/// mask, 0 for a plain dispatch) and every DCS hook.
#[derive(Debug, PartialEq, Eq)]
struct Dispatched {
    csi: Vec<(Vec<u16>, Vec<u8>, u8, u32)>,
    dcs: Vec<(Vec<u16>, Vec<u8>, u8)>,
    prints: String,
}

fn dispatched(sink: &RecordingSink) -> Dispatched {
    // The two CSI lists interleave only across sequences; each case below is
    // one sequence (plus text), so concatenating them keeps the order.
    let mut csi: Vec<_> = sink
        .csi_dispatches
        .iter()
        .map(|(p, i, f)| (p.clone(), i.clone(), *f, 0))
        .collect();
    csi.extend(sink.csi_dispatches_with_subparams.iter().cloned());
    Dispatched {
        csi,
        dcs: sink.dcs_hooks.clone(),
        prints: sink.prints.iter().collect(),
    }
}

/// Feed `bytes` in the chunks `cuts` names (bit `k` set: a chunk ends after
/// byte `k`) through the fast path.
fn feed_chunked(bytes: &[u8], cuts: u32) -> Dispatched {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();
    let mut start = 0;
    for end in 1..=bytes.len() {
        if end == bytes.len() || cuts & (1 << (end - 1)) != 0 {
            parser.advance_fast(&bytes[start..end], &mut sink);
            start = end;
        }
    }
    dispatched(&sink)
}

/// `bytes` split at `split`, the head's parser state carried into a fresh
/// parser (the seamless-update handoff), the tail fed to that one.
fn feed_across_carry(bytes: &[u8], split: usize) -> Dispatched {
    let (head, tail) = bytes.split_at(split);
    let mut sink = RecordingSink::default();
    let mut outgoing = Parser::new();
    outgoing.advance_fast(head, &mut sink);
    let carry = outgoing.carry().expect("no hooked DCS in these cases");
    let mut successor = Parser::new();
    successor
        .restore_carry(&carry)
        .expect("a real carry restores");
    successor.advance_fast(tail, &mut sink);
    dispatched(&sink)
}

type Case = (
    &'static [u8],
    &'static [(&'static [u16], &'static [u8], u8, u32)],
);

/// Each sequence and the dispatch it must produce. The first byte after
/// `ESC [` picks the fast path's shape (one digit, two digits, a private
/// marker, three digits), so each shape is here with a trailing `;`.
const CASES: &[Case] = &[
    // The lead: bold, then the default (reset).
    (b"\x1b[1;mX", &[(&[1, 0], b"", b'm', 0)]),
    (b"\x1b[38;mX", &[(&[38, 0], b"", b'm', 0)]),
    (b"\x1b[123;HX", &[(&[123, 0], b"", b'H', 0)]),
    // vim's t_TE: XTMODKEYS with the value omitted, which resets.
    (b"\x1b[>4;mX", &[(&[4, 0], b">", b'm', 0)]),
    (b"\x1b[5;rX", &[(&[5, 0], b"", b'r', 0)]),
    (b"\x1b[;mX", &[(&[0, 0], b"", b'm', 0)]),
    // Before an intermediate the list closes the same way.
    (b"\x1b[1; qX", &[(&[1, 0], b" ", b'q', 0)]),
    (b"\x1b[?1;$pX", &[(&[1, 0], b"?$", b'p', 0)]),
    // Leading and middle empties already arrived as 0; they still do.
    (b"\x1b[;1mX", &[(&[0, 1], b"", b'm', 0)]),
    (b"\x1b[1;;4mX", &[(&[1, 0, 4], b"", b'm', 0)]),
    // After a subparameter group, a `;` opens an (empty) parameter too.
    (b"\x1b[1;4:3;mX", &[(&[1, 4, 3, 0], b"", b'm', 0b100)]),
    (b"\x1b[4:3;mX", &[(&[4, 3, 0], b"", b'm', 0b10)]),
    // Negative controls: no separator, no parameter; a group that ends in
    // digits, or a trailing `:`, opens none.
    (b"\x1b[mX", &[(&[], b"", b'm', 0)]),
    (b"\x1b[1mX", &[(&[1], b"", b'm', 0)]),
    (b"\x1b[1;4:3mX", &[(&[1, 4, 3], b"", b'm', 0b100)]),
    (b"\x1b[1;4:mX", &[(&[1, 4], b"", b'm', 0)]),
    (b"\x1b[4:mX", &[(&[4], b"", b'm', 0)]),
];

fn expected(case: &Case) -> Dispatched {
    Dispatched {
        csi: case
            .1
            .iter()
            .map(|(p, i, f, m)| (p.to_vec(), i.to_vec(), *f, *m))
            .collect(),
        dcs: Vec::new(),
        prints: "X".into(),
    }
}

/// Every case, in every chunking the fast path can be fed, through the byte
/// path, and across a carry at every byte.
#[test]
fn an_empty_parameter_reaches_the_handler_as_the_default() {
    for case in CASES {
        let (bytes, _) = *case;
        let want = expected(case);
        let shown = String::from_utf8_lossy(bytes);
        for cuts in 0..1u32 << (bytes.len() - 1) {
            assert_eq!(feed_chunked(bytes, cuts), want, "{shown:?} cuts {cuts:#b}");
        }
        let mut byte_path = RecordingSink::default();
        Parser::new().advance(bytes, &mut byte_path);
        assert_eq!(dispatched(&byte_path), want, "{shown:?} byte path");
        for split in 0..=bytes.len() {
            assert_eq!(
                feed_across_carry(bytes, split),
                want,
                "{shown:?} carried at {split}"
            );
        }
    }
}

/// A DCS header's parameters close the same way: `DCS 0 ; 1 ; q` (a sixel
/// header with its third parameter left empty) is `[0, 1, 0]`.
#[test]
fn a_dcs_header_keeps_its_empty_last_parameter() {
    for (bytes, params) in [
        (&b"\x1bP0;1;q#0\x1b\\X"[..], &[0u16, 1, 0][..]),
        (&b"\x1bP;q\x1b\\X"[..], &[0, 0][..]),
        // Negative control: no separator, no parameter.
        (&b"\x1bPq\x1b\\X"[..], &[][..]),
    ] {
        let want = Dispatched {
            csi: Vec::new(),
            dcs: vec![(params.to_vec(), Vec::new(), b'q')],
            prints: "X".into(),
        };
        let shown = String::from_utf8_lossy(bytes);
        for cuts in 0..1u32 << (bytes.len() - 1) {
            assert_eq!(feed_chunked(bytes, cuts), want, "{shown:?} cuts {cuts:#b}");
        }
    }
}
