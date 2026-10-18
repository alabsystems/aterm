// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A verb's answer on stdout, where a reader that has gone away ends the answer, never the
//! process.
//!
//! std's `print!`/`println!` PANIC when stdout's reader has closed the pipe (`aterm drive
//! --help | head -1`, `| grep -q` after its match, `| true`): Rust leaves SIGPIPE ignored, so
//! the write is EPIPE and the panic is "failed printing to stdout: Broken pipe", exit 101.
//! Restoring SIGPIPE's default instead would kill a window-spawned `aterm pkg` pass the moment
//! its window quit (atpkg's orphan watch relies on the write failing, not killing), reach the
//! GUI and session lanes, and do nothing on Windows, where a closed pipe is `ERROR_NO_DATA` or
//! `ERROR_BROKEN_PIPE`, both `BrokenPipe` to std.
//!
//! So a verb prints through here, the rule `aterm ctl` already follows. A verb's answer (its
//! help, its data) goes through [`answer_exit`], or [`answer`] and [`answer_status`] where the
//! caller ends the process itself: a reader that went away ends the answer, and any other
//! failure (a full disk under a redirect) exits 1 saying so, because a script must not read an
//! empty file as the answer. [`print`] and [`println`] sit behind a crate's own
//! `print!`/`println!`, which every library the `aterm` executable runs a verb from in-process
//! defines over std's, for the incidental lines around an answer, which they drop rather than
//! panic on. [`write_all`] is for a caller whose next step depends on the reader having had the
//! text (`aterm fabric mint-for`'s cap, which it refuses to call minted otherwise).

use std::fmt;
use std::io::{self, Write as _};
use std::process::ExitCode;

/// Whether `e` is stdout's reader having gone away: `BrokenPipe`, which EPIPE and both Windows
/// pipe-closed errors (`ERROR_NO_DATA`, `ERROR_BROKEN_PIPE`) map to.
#[must_use]
pub fn reader_gone(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::BrokenPipe
}

/// `written`, with a reader that closed the pipe first counted as written: the answer ends
/// there, and that is no failure. Every other error stands.
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

/// Write `bytes` to stdout whole and flush them, for a caller whose next step depends on the
/// reader having had them.
///
/// # Errors
///
/// Every write error, a gone reader's ([`reader_gone`]) included.
pub fn write_all(bytes: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(bytes).and_then(|()| out.flush())
}

/// Write a verb's answer to stdout whole and flush it; a reader that went away has had all it
/// wanted ([`reader_gone_is_written`]).
///
/// # Errors
///
/// Any other write error (a full disk under a redirect).
pub fn answer(text: &str) -> io::Result<()> {
    reader_gone_is_written(write_all(text.as_bytes()))
}

/// The status a verb's answer ends with, given what writing it returned: the verb's own `code`
/// when it was written or its reader went away ([`reader_gone_is_written`]), else the stderr
/// line `<verb>: the answer was not written (<error>)`, which exits 1.
///
/// # Errors
///
/// That line, for any write error but a gone reader's.
pub fn answer_status<C>(verb: &str, written: io::Result<()>, code: C) -> Result<C, String> {
    match reader_gone_is_written(written) {
        Ok(()) => Ok(code),
        Err(e) => Err(format!("{verb}: the answer was not written ({e})")),
    }
}

/// Write `verb`'s whole answer to stdout and end with its `code`; an answer that was not
/// written for any reason but a gone reader ends with 1 and [`answer_status`]'s line on stderr.
#[must_use]
pub fn answer_exit(verb: &str, text: &str, code: u8) -> ExitCode {
    let status = answer_status(verb, write_all(text.as_bytes()), code).unwrap_or_else(|line| {
        let _ = writeln!(io::stderr(), "{line}");
        1
    });
    ExitCode::from(status)
}

/// `print!` as a verb's library spells it: `args` on stdout, flushed. Text that cannot be
/// written is dropped, where std's panicked, so it is for the lines around an answer, never the
/// answer itself ([`answer_exit`]).
pub fn print(args: fmt::Arguments<'_>) {
    let mut out = io::stdout().lock();
    let _ = out.write_fmt(args).and_then(|()| out.flush());
}

/// `println!` as a verb's library spells it: [`print`] with the newline.
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
