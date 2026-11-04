// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::write_record_buffered;
use std::io::{self, Write};

#[derive(Default)]
struct Sink {
    writes: Vec<usize>,
    bytes: Vec<u8>,
    refuse: bool,
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.refuse {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "disk failed"));
        }
        self.writes.push(bytes.len());
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn record_buffer_coalesces_small_writes_and_flushes_the_final_partial_chunk() {
    let mut sink = Sink::default();
    let count = 3 * 64 * 1024 + 17;
    write_record_buffered(&mut sink, |writer| {
        for _ in 0..count {
            writer.write_all(b"x")?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(sink.bytes, vec![b'x'; count]);
    assert_eq!(sink.writes, [64 * 1024, 64 * 1024, 64 * 1024, 17]);
}

#[test]
fn record_buffer_reports_the_final_write_failure_before_commit() {
    let mut sink = Sink {
        refuse: true,
        ..Sink::default()
    };
    let mut produced = false;
    let result = write_record_buffered(&mut sink, |writer| {
        writer.write_all(b"fits entirely in the buffer")?;
        produced = true;
        Ok(())
    });
    assert!(produced, "failure comes from the explicit final flush");
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert!(sink.bytes.is_empty());
}
