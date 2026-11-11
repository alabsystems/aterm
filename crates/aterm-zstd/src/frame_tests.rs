// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{BLOCK_MAX, Checksum, WINDOW_MAX, write_raw};

struct Count(usize);

impl std::io::Write for Count {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn content_past_the_window_cap_is_refused() {
    let content = vec![0u8; WINDOW_MAX + 1];
    let mut out = Count(0);
    let err = write_raw(&mut out, &content, Checksum::Omitted).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(out.0, 0);
    write_raw(&mut out, &content[..WINDOW_MAX], Checksum::Omitted).unwrap();
    assert_eq!(out.0, 9 + 3 * (WINDOW_MAX / BLOCK_MAX) + WINDOW_MAX);
}
