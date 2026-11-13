// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{Error, ErrorKind, Write as _};

/// Another process may write between calls even while this process holds stderr's lock.
#[test]
fn complete_diagnostic_lines_survive_a_shared_writers_intrusion() {
    struct SharedWriter {
        bytes: Vec<u8>,
        other: Option<&'static [u8]>,
        writes: usize,
    }

    impl std::io::Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(bytes);
            if let Some(other) = self.other.take() {
                self.bytes.extend_from_slice(other);
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let detail = std::hint::black_box(String::from("ready"));
    let mut fragmented = SharedWriter {
        bytes: Vec::new(),
        other: Some(b"child: count=2\n"),
        writes: 0,
    };
    writeln!(fragmented, "parent: {detail}").expect("fragmented diagnostic");
    let mut complete = SharedWriter {
        bytes: Vec::new(),
        other: Some(b"child: count=2\n"),
        writes: 0,
    };
    super::write_line_to(&mut complete, format_args!("parent: {detail}"))
        .expect("complete diagnostic");

    let records = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("child:"))
            .count()
    };
    assert!(fragmented.writes > 1);
    assert_eq!(records(&fragmented.bytes), 0);
    assert_eq!(complete.writes, 1);
    assert_eq!(records(&complete.bytes), 1);
    assert_eq!(complete.bytes, b"parent: ready\nchild: count=2\n");
}

/// A verb's answer whose reader closed the pipe ends with the verb's own code; one a full
/// disk refused (`aterm drive --help > /dev/full`) is a failure said on stderr, never the
/// verb's success over an empty file.
#[test]
fn a_gone_reader_keeps_the_verbs_code_and_a_full_disk_is_said() {
    assert_eq!(super::answer_status("aterm drive", Ok(()), 3), Ok(3));
    assert_eq!(
        super::answer_status("aterm drive", Err(Error::from(ErrorKind::BrokenPipe)), 3),
        Ok(3),
        "the reader had all it wanted"
    );
    assert_eq!(
        super::answer_status("aterm drive", Err(Error::from(ErrorKind::StorageFull)), 0),
        Err(format!(
            "aterm drive: the answer was not written ({})",
            Error::from(ErrorKind::StorageFull)
        )),
        "a full disk lost the answer"
    );
}

/// Only a gone reader counts as written; a full disk is still a failure to report.
#[test]
fn only_a_gone_reader_is_written() {
    assert!(super::reader_gone_is_written(Ok(())).is_ok());
    assert!(super::reader_gone_is_written(Err(Error::from(ErrorKind::BrokenPipe))).is_ok());
    let full = super::reader_gone_is_written(Err(Error::from(ErrorKind::StorageFull)));
    assert_eq!(
        full.map_err(|e| e.kind()),
        Err(ErrorKind::StorageFull),
        "any other error stands"
    );
}
