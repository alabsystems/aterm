// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn read_file_and_read_text_roundtrip_and_refuse_oversized_or_symlinks() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sample.txt");
    write_atomic_durable(&path, b"hello release", 0o600).expect("write");

    assert_eq!(read_file(&path).expect("read_file"), b"hello release");
    assert_eq!(read_text(&path).expect("read_text"), "hello release");
    assert_eq!(
        read_regular_bytes(&path, 5).expect_err("over cap").kind(),
        std::io::ErrorKind::FileTooLarge
    );

    #[cfg(unix)]
    {
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&path, &link).expect("symlink");
        assert!(read_file(&link).is_err());
        assert!(read_text(&link).is_err());
        assert!(open_append(&link, true).is_err());
        assert!(open_create_new(&link).is_err());
    }

    let log = dir.path().join("append.log");
    assert!(open_append(&log, false).is_err());
    std::io::Write::write_all(&mut open_append(&log, true).expect("create"), b"a\n")
        .expect("write a");
    std::io::Write::write_all(&mut open_append(&log, false).expect("append"), b"b\n")
        .expect("write b");
    assert_eq!(read_text(&log).expect("read log"), "a\nb\n");
}

/// A reader whose every other `read` is interrupted (`EINTR`), as a signal without
/// `SA_RESTART` interrupts one on NFS or FUSE.
struct Interrupting<'a> {
    /// What is left to yield.
    rest: &'a [u8],
    /// Whether the next `read` is interrupted.
    interrupt: bool,
}

impl std::io::Read for Interrupting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.interrupt = !self.interrupt;
        if self.interrupt {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let n = buf.len().min(self.rest.len()).min(3);
        buf[..n].copy_from_slice(&self.rest[..n]);
        self.rest = &self.rest[n..];
        Ok(n)
    }
}

/// An interrupted read is retried, as `read_to_end` retries it, so the streamed hash and
/// comparison finish as the buffered ones do.
#[test]
fn streams_retry_an_interrupted_read() {
    let bytes = b"hello release";
    let reader = || Interrupting {
        rest: bytes,
        interrupt: false,
    };
    assert_eq!(
        sha256_stream(&mut reader()).expect("hash"),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
    );
    assert!(stream_equals(&mut reader(), bytes).expect("compare"));
    assert!(!stream_equals(&mut reader(), b"hello relaxed").expect("compare"));
}
