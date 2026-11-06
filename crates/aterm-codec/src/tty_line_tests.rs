// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::write_line;
use std::{fmt, io};

struct Pieces;
impl fmt::Display for Pieces {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("é\n")?;
        f.write_str("界\r\t\u{1b}[2J")?;
        f.write_str("\u{9b}\u{2028}\u{202e}\u{2066}\u{2069}")?;
        f.write_str("👩\u{200d}💻\\normal trust=literal")
    }
}

#[test]
fn formatted_pieces_escape_controls_and_keep_ordinary_text_without_a_cap() {
    let long = "名".repeat(4096);
    let mut out = Vec::new();
    write_line(&mut out, format_args!("prefix {Pieces} {long}")).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "prefix é\\n界\\r\\t\\x1b[2J\\u{{009b}}\\u{{2028}}\\u{{202e}}\\u{{2066}}\\u{{2069}}👩\u{200d}💻\\normal trust=literal {long}\n"
        )
    );
}

struct ShortWriter {
    bytes: Vec<u8>,
    remaining: usize,
    calls: usize,
}

impl io::Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.remaining == 0 {
            return Err(io::Error::from_raw_os_error(28));
        }
        let n = bytes.len().min(self.remaining).min(2);
        self.bytes.extend_from_slice(&bytes[..n]);
        self.remaining -= n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("the caller decides when to flush")
    }
}

#[test]
fn short_writes_and_errors_preserve_the_delivered_prefix_and_error_cause() {
    let expected = "é\\n界\\x1b\n".as_bytes();
    for remaining in 0..=expected.len() {
        let mut out = ShortWriter {
            bytes: Vec::new(),
            remaining,
            calls: 0,
        };
        let result = write_line(&mut out, format_args!("é\n界\u{1b}"));
        assert_eq!(out.bytes, expected[..remaining]);
        if remaining == expected.len() {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().raw_os_error(), Some(28));
        }
    }
}

#[test]
fn formatter_failure_returns_an_error_without_a_newline_or_panic() {
    struct Failed;
    impl fmt::Display for Failed {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("prefix\n")?;
            Err(fmt::Error)
        }
    }
    let mut out = Vec::new();
    let error = write_line(&mut out, format_args!("{Failed}")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(error.to_string(), "formatting failed");
    assert_eq!(out, b"prefix\\n");
}

#[test]
fn formatter_cannot_hide_an_output_error_or_resume_writing() {
    struct IgnoresError;
    impl fmt::Display for IgnoresError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let _ = f.write_str("first");
            let _ = f.write_str("second");
            Ok(())
        }
    }
    let mut out = ShortWriter {
        bytes: Vec::new(),
        remaining: 0,
        calls: 0,
    };
    let error = write_line(&mut out, format_args!("{IgnoresError}")).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(28));
    assert!(out.bytes.is_empty());
    assert_eq!(out.calls, 1);
}
