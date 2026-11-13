// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-panicking stdout helpers that treat `BrokenPipe` as end-of-answer, and complete stderr diagnostic lines.

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

/// Renders `args` and its newline before writing the complete diagnostic to stderr.
///
/// This avoids separate writes for formatting fragments when a child shares stderr.
/// It uses one `write_all`; partial writes and oversized records still carry no general
/// cross-process atomicity guarantee.
///
/// # Errors
///
/// Every stderr write error, including a closed reader.
pub fn stderr_line(args: fmt::Arguments<'_>) -> io::Result<()> {
    write_line_to(&mut io::stderr().lock(), args)
}

fn write_line_to(out: &mut impl io::Write, args: fmt::Arguments<'_>) -> io::Result<()> {
    let mut line = fmt::format(args);
    line.push('\n');
    out.write_all(line.as_bytes())
}

#[cfg(test)]
#[path = "stdout_tests.rs"]
mod tests;
