// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original classifier at 6df3e212bb, retained only as a test/measurement oracle.

pub(super) fn classify(chunk: &[u8]) -> [bool; 4] {
    let drawn = (0..chunk.len()).any(|i| {
        let Some(rest) = chunk.get(i..).and_then(|c| c.strip_prefix(b"\x1b[")) else {
            return false;
        };
        rest.iter()
            .find(|b| (0x40..=0x7e).contains(*b))
            .is_some_and(|b| matches!(b, b'H' | b'f' | b'd' | b'r' | b'L' | b'M' | b'S' | b'T'))
    });
    let (mut ends, mut ended, mut tail) = (false, false, false);
    for &b in chunk {
        match b {
            b'\n' => {
                ends = true;
                ended |= tail;
                tail = false;
            }
            b'\r' => {}
            _ => tail = true,
        }
    }
    [ends, ended, tail, drawn]
}
