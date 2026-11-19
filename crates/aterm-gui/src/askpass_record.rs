// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The password sheet's unattended spacing on disk (`docs/DESIGN-sudo-askpass-2026-10-15.md`
//! §3.4 step 5): one line in the state dir, beside `askpass-off`, that carries the instance-wide
//! quiet window and a sheet standing across an update, a crash and a relaunch
//! (`App::persist_askpass_quiet`, `App::take_up_askpass_record`).
//!
//! The file is the whole line `askpass-quiet 1 <streak> <ended_ms|-> <standing_ms|->` and one
//! LF, at most [`MAX_BYTES`]: decimal with no leading zero (`0` alone allowed), a streak of `0`
//! only with no end and a streak of 1 or more only with one. Any other version, magic, shape,
//! non-UTF-8 or extra byte reads as none, never as a block.
//!
//! Compatibility: earlier builds never open this name; version 1 reads only version 1, so a
//! future version reads as none here; a later build must read version 1 by its meaning here or
//! as none, and must change the version for any change of meaning. The account binding is the
//! state dir (per `HOME`) and the reader's owner check; a writer of the same account, which can
//! already write `askpass-off`, buys at most one quiet window of at most `QUIET_MAX` at a
//! take-up ([`crate::askpass::settle_record`], [`crate::askpass::unattended_left`]).

#[cfg(unix)]
use std::path::Path;

/// The record's file name in the state dir.
pub(crate) const NAME: &str = "askpass-quiet";

/// The longest record: a longer file reads as none.
pub(crate) const MAX_BYTES: usize = 96;

/// The line's first two fields.
const HEAD: &str = "askpass-quiet 1";

/// The record's line for `streak`, the wall-clock milliseconds the unattended part `ended`, and
/// those a sheet has been `standing` since.
#[must_use]
pub(crate) fn line(streak: u32, ended: Option<u64>, standing: Option<u64>) -> String {
    let field = |ms: Option<u64>| ms.map_or_else(|| "-".to_owned(), |ms| ms.to_string());
    let ended = if streak == 0 { None } else { ended };
    let streak = if ended.is_none() { 0 } else { streak };
    format!("{HEAD} {streak} {} {}\n", field(ended), field(standing))
}

/// The record's streak, end and standing raise; `None` for anything but a well-formed version-1
/// line ([`line`]).
#[must_use]
pub(crate) fn parse(bytes: &[u8]) -> Option<(u32, Option<u64>, Option<u64>)> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
    let rest = text.strip_prefix(HEAD)?.strip_prefix(' ')?;
    let mut fields = rest.split(' ');
    let streak: u32 = decimal(fields.next()?)?;
    let ended = millis_or_none(fields.next()?)?;
    let standing = millis_or_none(fields.next()?)?;
    if fields.next().is_some() || (streak == 0) != ended.is_none() {
        return None;
    }
    Some((streak, ended, standing))
}

/// `-` as `Some(None)`, a decimal as `Some(Some(n))`, anything else `None`.
fn millis_or_none(field: &str) -> Option<Option<u64>> {
    if field == "-" {
        Some(None)
    } else {
        decimal(field).map(Some)
    }
}

/// ASCII decimal digits with no leading zero (`0` alone allowed) that fit `T`.
fn decimal<T: std::str::FromStr>(field: &str) -> Option<T> {
    let digits_only = !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit());
    let leading_zero = field.len() > 1 && field.starts_with('0');
    if !digits_only || leading_zero {
        return None;
    }
    field.parse().ok()
}

/// Whether this process has logged a record failure: one record per process, since every later
/// change writes the record again.
#[cfg(unix)]
static FAILURE_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
fn note_failure(what: &str, error: &dyn std::fmt::Display) {
    if !FAILURE_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        aterm_log::warn!(
            "askpass: the password sheet's spacing record could not be {what} ({error}); the \
             next change writes it again"
        );
    }
}

/// The streak, end and standing raise the record in `dir` holds ([`parse`]). The file must be
/// this user's own regular file with one link, no symlink, and no group or other write bit on it
/// or on `dir` (`aterm_uds::private_file::read_owned`); any failure, a missing file included,
/// reads as none. Nothing is written here.
#[cfg(unix)]
#[must_use]
pub(crate) fn read_in(dir: &Path) -> Option<(u32, Option<u64>, Option<u64>)> {
    let bytes = match aterm_uds::private_file::read_owned(dir, NAME, MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return None,
        Err(error) => {
            note_failure("read", &error);
            return None;
        }
    };
    parse(&bytes)
}

/// Write `line` as the record in `dir`: a new temporary beside it, mode 0600, renamed over the
/// name, so a link planted there is replaced and never written through. No flush: the record is
/// advisory (`aterm_uds::atomic_file::write_flushed`'s `None`).
/// # Errors
/// `dir` could not be made, or the write or its rename failed.
#[cfg(unix)]
pub(crate) fn write_in(dir: &Path, line: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    aterm_uds::atomic_file::write_flushed(&dir.join(NAME), line.as_bytes(), 0o600, None)
}

/// Remove the record in `dir`; one already gone is no failure.
/// # Errors
/// The name could not be removed (a folder there, or no permission).
#[cfg(unix)]
pub(crate) fn clear_in(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(dir.join(NAME)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Make the record in `dir` say `line` ([`write_in`]), or remove it for `None` ([`clear_in`]).
/// A failure is logged once per process; the next change writes it again.
#[cfg(unix)]
pub(crate) fn persist(dir: &Path, line: Option<&str>) {
    let done = match line {
        Some(line) => write_in(dir, line),
        None => clear_in(dir),
    };
    if let Err(error) = done {
        note_failure("written", &error);
    }
}

/// Remove the temporaries a writer that died between its create and its rename left beside the
/// record in `dir` ([`aterm_uds::atomic_file::sweep_temps_beside`]); it decides nothing.
#[cfg(unix)]
pub(crate) fn sweep(dir: &Path) {
    if let Err(error) = aterm_uds::atomic_file::sweep_temps_beside(
        &dir.join(NAME),
        aterm_uds::atomic_file::ABANDONED_AFTER,
    ) {
        note_failure("swept", &error);
    }
}

#[cfg(test)]
#[path = "askpass_record_tests.rs"]
mod tests;
