// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SecretBytes, read_to_end_wiping_on_error};
use zeroize::Zeroize;

#[test]
fn debug_never_shows_the_bytes() {
    let secret = SecretBytes::from(b"hunter2".as_slice());
    let shown = format!("{secret:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(shown.contains("7 bytes"), "{shown}");
}

#[test]
fn a_partial_read_error_wipes_the_accumulated_bytes() {
    use std::cell::Cell;
    use std::io::{self, Read};

    struct PartialThenError<'a> {
        remaining: &'static [u8],
        sent: &'a Cell<usize>,
    }
    impl Read for PartialThenError<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.remaining.is_empty() {
                return Err(io::Error::other("captured read failure"));
            }
            let count = out.len().min(self.remaining.len());
            out[..count].copy_from_slice(&self.remaining[..count]);
            self.remaining = &self.remaining[count..];
            self.sent.set(self.sent.get() + count);
            Ok(count)
        }
    }

    let sent = Cell::new(0);
    let mut bytes = b"existing".to_vec();
    let failed = read_to_end_wiping_on_error(
        PartialThenError {
            remaining: b"partial",
            sent: &sent,
        },
        &mut bytes,
    )
    .expect_err("the reader fails after supplying bytes");
    assert_eq!(sent.get(), b"partial".len());
    assert_eq!(failed.kind(), io::ErrorKind::Other);
    assert!(bytes.is_empty(), "no accumulated bytes survive the error");

    assert_eq!(
        read_to_end_wiping_on_error(b"complete".as_slice(), &mut bytes).expect("complete"),
        8
    );
    assert_eq!(bytes, b"complete");
    assert_eq!(
        read_to_end_wiping_on_error(b"".as_slice(), &mut bytes).expect("empty"),
        0
    );
    assert_eq!(
        bytes, b"complete",
        "a successful empty read leaves existing bytes"
    );
    bytes.zeroize();
}
