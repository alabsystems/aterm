// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct ShortSink {
    bytes: Vec<u8>,
    interrupt: bool,
    limit: usize,
}

impl Write for ShortSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if std::mem::take(&mut self.interrupt) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = self.limit.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn exact_append_writer_bounds_every_underlying_write() {
    let mut sink = ShortSink {
        bytes: Vec::new(),
        interrupt: true,
        limit: 2,
    };
    {
        let mut exact = ExactWriter {
            writer: &mut sink,
            remaining: 6,
        };
        exact.write_all(b"abcdef").unwrap();
        exact.finish().unwrap();
    }
    assert_eq!(sink.bytes, b"abcdef");
    sink.bytes.clear();
    {
        let mut exact = ExactWriter {
            writer: &mut sink,
            remaining: 3,
        };
        assert_eq!(
            exact.write_all(b"excess").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            exact.finish().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
    assert!(
        sink.bytes.is_empty(),
        "the over-budget write reached no sink"
    );
    sink.limit = 0;
    let mut exact = ExactWriter {
        writer: &mut sink,
        remaining: 1,
    };
    assert_eq!(
        exact.write_all(b"x").unwrap_err().kind(),
        io::ErrorKind::WriteZero
    );
    assert_eq!(
        exact.finish().unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn incomplete_or_excess_append_never_gets_a_durable_success() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    for scenario in 0..3 {
        write_atomic(&path, b"old", 0o600).unwrap();
        let expected = stamp(&path).unwrap();
        let flushes = aterm_durable::seam::Recording::start();
        let result = append_at_with(&path, &expected, 4, |writer| {
            writer.write_all(b"ab")?;
            match scenario {
                0 => Ok(()),
                1 => writer.write_all(b"over"),
                _ => Err(io::Error::new(io::ErrorKind::BrokenPipe, "producer failed")),
            }
        });
        assert!(
            flushes.take().is_empty(),
            "a failed producer was made durable"
        );
        assert!(result.is_err());
        drop(flushes);
        assert_eq!(std::fs::read(&path).unwrap(), b"oldab");
    }
}

#[test]
fn append_flushes_buffer_before_counting_and_accepts_zero_length() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    write_atomic(&path, b"old", 0o600).unwrap();
    let expected = stamp(&path).unwrap();
    let flushes = aterm_durable::seam::Recording::start();
    let written = append_at_with(&path, &expected, 2, |writer| {
        let mut buffered = io::BufWriter::with_capacity(64 * 1024, writer);
        buffered.write_all(b"++")?;
        buffered.flush()
    })
    .unwrap();
    assert_eq!(flushes.take(), [aterm_durable::Flush::Durable]);
    assert_eq!(written.len, expected.len + 2);
    assert_eq!(std::fs::read(&path).unwrap(), b"old++");
    let empty = append_at_with(&path, &written, 0, |_| Ok(())).unwrap();
    assert_eq!(empty.len, written.len);
    assert_eq!(flushes.take(), [aterm_durable::Flush::Durable]);
}

#[test]
fn failed_final_buffer_flush_or_length_overflow_writes_no_bytes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    write_atomic(&path, b"old", 0o600).unwrap();
    let expected = stamp(&path).unwrap();
    let flushes = aterm_durable::seam::Recording::start();
    let result = append_at_with(&path, &expected, 1, |writer| {
        let mut buffered = io::BufWriter::with_capacity(64 * 1024, writer);
        buffered.write_all(b"two")?;
        buffered.flush()
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert!(flushes.take().is_empty());
    let overflow = FileStamp {
        len: u64::MAX,
        ..expected
    };
    let result = append_at_with(&path, &overflow, 1, |_| panic!("overflow invoked producer"));
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert!(flushes.take().is_empty());
}

#[test]
fn stamped_producer_identifies_only_the_file_it_renamed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    let placed = write_atomic_stamped_with(&path, 0o600, |writer| writer.write_all(b"ours"))
        .unwrap()
        .unwrap();
    assert_eq!(stamp(&path), Some(placed));
    let replaced = write_atomic_stamped_after(
        &path,
        0o600,
        |writer| writer.write_all(b"next"),
        || {
            write_atomic(&path, b"peer", 0o600).unwrap();
        },
    )
    .unwrap();
    assert_eq!(replaced, None);
    assert_eq!(std::fs::read(&path).unwrap(), b"peer");
    let removed = write_atomic_stamped_after(
        &path,
        0o600,
        |writer| writer.write_all(b"next"),
        || {
            std::fs::remove_file(&path).unwrap();
        },
    )
    .unwrap();
    assert_eq!(removed, None);
}
