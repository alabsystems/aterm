// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests that empty CSI and DCS parameters reach handlers as `0` across chunks and carries.

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

const CASES: &[Case] = &[
    (b"\x1b[1;mX", &[(&[1, 0], b"", b'm', 0)]),
    (b"\x1b[38;mX", &[(&[38, 0], b"", b'm', 0)]),
    (b"\x1b[123;HX", &[(&[123, 0], b"", b'H', 0)]),
    (b"\x1b[>4;mX", &[(&[4, 0], b">", b'm', 0)]),
    (b"\x1b[5;rX", &[(&[5, 0], b"", b'r', 0)]),
    (b"\x1b[;mX", &[(&[0, 0], b"", b'm', 0)]),
    (b"\x1b[1; qX", &[(&[1, 0], b" ", b'q', 0)]),
    (b"\x1b[?1;$pX", &[(&[1, 0], b"?$", b'p', 0)]),
    (b"\x1b[;1mX", &[(&[0, 1], b"", b'm', 0)]),
    (b"\x1b[1;;4mX", &[(&[1, 0, 4], b"", b'm', 0)]),
    (b"\x1b[1;4:3;mX", &[(&[1, 4, 3, 0], b"", b'm', 0b100)]),
    (b"\x1b[4:3;mX", &[(&[4, 3, 0], b"", b'm', 0b10)]),
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

#[test]
fn an_empty_parameter_reaches_the_handler_as_the_default() {
    for case in CASES {
        let (bytes, _) = *case;
        let want = expected(case);
        let shown = String::from_utf8_lossy(bytes);
        for cuts in 0..1u32 << (bytes.len() - 1) {
            assert_eq!(feed_chunked(bytes, cuts), want, "{shown:?} cuts {cuts:#b}");
        }
        for split in 0..=bytes.len() {
            assert_eq!(
                feed_across_carry(bytes, split),
                want,
                "{shown:?} carried at {split}"
            );
        }
    }
}

#[test]
fn a_dcs_header_keeps_its_empty_last_parameter() {
    for (bytes, params) in [
        (&b"\x1bP0;1;q#0\x1b\\X"[..], &[0u16, 1, 0][..]),
        (&b"\x1bP;q\x1b\\X"[..], &[0, 0][..]),
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
