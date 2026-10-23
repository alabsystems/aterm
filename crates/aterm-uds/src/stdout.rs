// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-panicking stdout helpers that treat `BrokenPipe` as end-of-answer.

use std::fmt;
use std::io::{self, Write as _};
use std::process::ExitCode;

/// Returns whether `e` indicates that stdout's reader closed the pipe (`BrokenPipe`).
#[must_use]
pub fn reader_gone(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::BrokenPipe
}

/// Maps a [`reader_gone`] error in `written` to `Ok(())`, preserving any other error.
///
/// # Errors
///
/// `written`'s error unless it is [`reader_gone`].
pub fn reader_gone_is_written(written: io::Result<()>) -> io::Result<()> {
    match written {
        Err(e) if reader_gone(&e) => Ok(()),
        written => written,
    }
}

/// Writes `bytes` to stdout in full and flushes stdout.
///
/// # Errors
///
/// Every write error, a gone reader's ([`reader_gone`]) included.
pub fn write_all(bytes: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(bytes).and_then(|()| out.flush())
}

/// Writes a verb's `text` answer to stdout and flushes, treating [`reader_gone`] as `Ok(())`.
///
/// # Errors
///
/// Any other write error (a full disk under a redirect).
pub fn answer(text: &str) -> io::Result<()> {
    reader_gone_is_written(write_all(text.as_bytes()))
}

/// Returns `Ok(code)` when `written` succeeded or hit [`reader_gone`], else an error message.
///
/// # Errors
///
/// Returns `<verb>: the answer was not written (<error>)` for any other write error.
pub fn answer_status<C>(verb: &str, written: io::Result<()>, code: C) -> Result<C, String> {
    match reader_gone_is_written(written) {
        Ok(()) => Ok(code),
        Err(e) => Err(format!("{verb}: the answer was not written ({e})")),
    }
}

/// Writes `verb`'s `text` answer to stdout and returns `code`, or exits 1 on write failure.
#[must_use]
pub fn answer_exit(verb: &str, text: &str, code: u8) -> ExitCode {
    let status = answer_status(verb, write_all(text.as_bytes()), code).unwrap_or_else(|line| {
        let _ = writeln!(io::stderr(), "{line}");
        1
    });
    ExitCode::from(status)
}

/// Writes `args` to stdout and flushes, ignoring write errors.
pub fn print(args: fmt::Arguments<'_>) {
    let mut out = io::stdout().lock();
    let _ = out.write_fmt(args).and_then(|()| out.flush());
}

/// `println!` as a verb's library spells it: [`fn@print`] with the newline.
pub fn println(args: fmt::Arguments<'_>) {
    let mut out = io::stdout().lock();
    let _ = out
        .write_fmt(args)
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush());
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};

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
}
