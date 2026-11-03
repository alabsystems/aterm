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
    // A trailing `:` is an empty element, as a trailing `;` is an empty parameter.
    (b"\x1b[1;4:mX", &[(&[1, 4, 0], b"", b'm', 0b100)]),
    (b"\x1b[4:mX", &[(&[4, 0], b"", b'm', 0b10)]),
    (b"\x1b[4::mX", &[(&[4, 0, 0], b"", b'm', 0b110)]),
    (
        b"\x1b[38:2::1:2:mX",
        &[(&[38, 2, 0, 1, 2, 0], b"", b'm', 0b11_1110)],
    ),
    (b"\x1b[1;2: qX", &[(&[1, 2, 0], b" ", b'q', 0b100)]),
    (b"\x1b[5:HX", &[(&[5, 0], b"", b'H', 0b10)]),
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

/// The cap's mark: past `MAX_PARAMS` a dropped sub-parameter sets the mask's bit `MAX_PARAMS`, in
/// every way the bytes can arrive, so a handler can tell a colon group the cap cut from one that
/// ended there. A dropped plain parameter does not, and a sub-parameter that fits does not.
#[test]
fn a_sub_parameter_the_cap_drops_leaves_its_mark() {
    let group_bits = |from: u32, to: u32| (from..to).fold(0u32, |mask, bit| mask | (1 << bit));
    let cases = [
        // A group whose last value fell off the end: 38 is parameter 19, the blue parameter 24.
        (19, "38:2:3:10:20:9", 24, group_bits(20, 24) | (1 << 24)),
        // The same group one parameter earlier fits.
        (18, "38:2:3:10:20:9", 24, group_bits(19, 24)),
        // No id, ending exactly at the cap: nothing was dropped.
        (19, "38:2:10:20:9", 24, group_bits(20, 24)),
        // A trailing colon's empty element is a dropped sub-parameter like any other.
        (22, "4:3:", 24, (1 << 23) | (1 << 24)),
        // Plain parameters past the cap leave no mark and stay on the plain dispatch.
        (24, "1;2", 24, 0),
    ];
    for (lead, tail, len, mask) in cases {
        let bytes = format!("\x1b[{}{tail}mX", "7;".repeat(lead)).into_bytes();
        let mut whole = Parser::new();
        let mut sink = RecordingSink::default();
        whole.advance_fast(&bytes, &mut sink);
        let got = dispatched(&sink);
        assert_eq!(got.csi.len(), 1, "{tail} after {lead}");
        assert_eq!(got.csi[0].0.len(), len, "{tail} after {lead}");
        assert_eq!(
            got.csi[0].3, mask,
            "{tail} after {lead}: {:#x}",
            got.csi[0].3
        );
        let mut byte_at_a_time = Parser::new();
        let mut sink = RecordingSink::default();
        for byte in &bytes {
            byte_at_a_time.advance_fast(std::slice::from_ref(byte), &mut sink);
            byte_at_a_time.assert_invariants();
        }
        assert_eq!(
            dispatched(&sink),
            got,
            "{tail} after {lead}, a byte at a time"
        );
        for split in 0..=bytes.len() {
            assert_eq!(
                feed_across_carry(&bytes, split),
                got,
                "{tail} after {lead}, carried at {split}"
            );
        }
    }
}
