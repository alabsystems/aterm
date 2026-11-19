// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Capture, collect_captures, drain_capture};
use std::io::{self, Cursor, Read};

struct Refused;

fn capture_bytes(bytes: &'static [u8]) -> Capture {
    drain_capture(Some(Box::new(Cursor::new(bytes))))
}

impl Read for Refused {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

#[test]
fn output_capture_preserves_complete_bytes_including_empty_output() {
    for (stdout, stderr) in [
        (b"".as_slice(), b"".as_slice()),
        (b"line\n\0".as_slice(), b"\xff".as_slice()),
    ] {
        let (out, err) = collect_captures(capture_bytes(stdout), capture_bytes(stderr)).unwrap();
        assert_eq!(out, stdout);
        assert_eq!(err, stderr);
    }
}

#[test]
fn an_output_prefix_followed_by_a_read_error_is_not_complete_capture() {
    for failed_stdout in [true, false] {
        let stream = Cursor::new(b"complete prefix\n").chain(Refused);
        let failed = drain_capture(Some(Box::new(stream)));
        let complete = capture_bytes(b"complete stream\n");
        let (stdout, stderr) = if failed_stdout {
            (failed, complete)
        } else {
            (complete, failed)
        };
        assert_eq!(
            collect_captures(stdout, stderr).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}

#[test]
fn a_missing_pipe_is_not_empty_output() {
    for missing_stdout in [true, false] {
        let missing = drain_capture(None);
        let complete = capture_bytes(b"complete stream\n");
        let (stdout, stderr) = if missing_stdout {
            (missing, complete)
        } else {
            (complete, missing)
        };
        assert_eq!(
            collect_captures(stdout, stderr).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}

#[test]
fn a_panicked_capture_reader_is_not_empty_output() {
    let reader = std::thread::spawn(|| -> io::Result<Vec<u8>> { panic!("capture fixture") });
    assert_eq!(
        collect_captures(capture_bytes(b"complete stdout\n"), reader)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Other
    );
}
