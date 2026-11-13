// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Cursor, Read, Write};

struct Duplex {
    incoming: Cursor<&'static [u8]>,
    written: Vec<u8>,
}

impl Duplex {
    fn reading(bytes: &'static [u8]) -> Self {
        Self {
            incoming: Cursor::new(bytes),
            written: Vec::new(),
        }
    }
}

impl Read for Duplex {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.incoming.read(bytes)
    }
}

impl Write for Duplex {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.written.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_truncated_authority_frame_forwards_no_possession() {
    let mut member =
        Duplex::reading(b"ALAB member=1 authority=abcdef\nOK authority=4 authoritysig=00\nab");
    let mut joiner = Duplex::reading(b"prove authority=00\npossess response=must-not-send\n");
    assert!(relay(&mut joiner, &mut member).is_err());
    assert_eq!(member.written, b"prove authority=00\n");
    assert_eq!(
        joiner.incoming.position(),
        b"prove authority=00\n".len() as u64
    );
}
