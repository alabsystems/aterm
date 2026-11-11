// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only `status.toml` stamps and scheduling rules for `atpkg` update passes.

use std::ffi::OsString;
use std::path::Path;

/// The exit code of an atpkg update pass that reached nothing: the link to every host failed.
pub const PASS_OFFLINE_EXIT: u8 = 69;

/// The fallback walk.
pub const FULL_PASS_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// No lane starts a scheduled full pass within this long of the last full pass attempted on the
/// machine ([`Stamps::last_attempt`]), nor while one is installing.
pub const PASS_SPACING_SECS: u64 = 5 * 60;

/// How a full `update` pass ended, as it records itself in `status.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassOutcome {
    /// The pass reached the signed index and ran to its end (a failed member is recorded in its own
    /// row), stamping `last_success_at`; or it had nothing to check, which stamps no success but
    /// counts as one ([`Stamps::last_success`]).
    Ok,
    /// No host answered ([`PASS_OFFLINE_EXIT`]): nothing was checked.
    Offline,
    /// Anything else: the index was not reached or nothing was installable here.
    Failed,
}

impl PassOutcome {
    /// The word `status.toml` carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Offline => "offline",
            Self::Failed => "failed",
        }
    }

    /// The outcome a recorded word names.
    #[must_use]
    pub fn from_word(word: &str) -> Self {
        match word.trim() {
            "ok" => Self::Ok,
            "offline" => Self::Offline,
            _ => Self::Failed,
        }
    }

    /// The outcome of a pass that exited `code`.
    #[must_use]
    pub const fn of_pass(stamped_success: bool, code: u8) -> Self {
        if stamped_success {
            Self::Ok
        } else if code == PASS_OFFLINE_EXIT {
            Self::Offline
        } else {
            Self::Failed
        }
    }
}

/// How long a scheduled pass queues at the store lock (`--wait-lock`) before it stands aside with
/// atpkg's contention exit.
pub const PASS_WAIT_LOCK_SECS: u64 = 30 * 60;

/// A detached update owns this total budget, including its store-lock wait.
/// Expiry requests cooperative cleanup inside the package process.
pub const PASS_OPERATION_LIMIT_SECS: u64 = PASS_WAIT_LOCK_SECS + 60 * 60;

/// The flags every scheduled pass carries after its verb.
#[must_use]
pub fn pass_flags(progress_file: Option<&Path>) -> Vec<OsString> {
    let mut flags: Vec<OsString> =
        vec!["--wait-lock".into(), PASS_WAIT_LOCK_SECS.to_string().into()];
    if let Some(path) = progress_file {
        flags.push("--progress-file".into());
        flags.push(path.into());
    }
    flags
}

/// The machine-wide stamps of `status.toml` a scheduler reads before it runs a pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stamps {
    /// Unix seconds of the last successful pass (`last_success_at`), or of a later full pass that
    /// recorded [`PassOutcome::Ok`] with nothing to check; the walk counts from either.
    pub last_success: Option<i64>,
    /// Unix seconds of the last full pass attempted, failed or not.
    pub last_attempt: Option<i64>,
    /// How the last full pass ended (`last_pass`); `None` when no pass recorded one.
    pub last_outcome: Option<PassOutcome>,
    /// The index build the last pass resolved (`last_index_build`; `0` for none).
    pub last_index_build: u64,
    /// The last full pass's requested or resolved index, bound to its end stamp in the same write.
    pub last_pass_target: Option<(i64, u64)>,
    /// Whether a pass is installing on the store now (its progress file names a live writer).
    pub in_flight: bool,
}

impl Stamps {
    /// The stamps of a `status.toml` text; an unparseable one reads as no stamps.
    #[must_use]
    pub fn parse(status_toml: &str) -> Self {
        let Ok(record) = status_toml.parse::<aterm_toml::Value>() else {
            return Self::default();
        };
        let word = |key: &str| {
            record
                .get(key)
                .and_then(aterm_toml::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let stamp = |key: &str| word(key).and_then(rfc3339_to_unix);
        let last_pass_at_word = word("last_pass_at");
        let last_pass_at = last_pass_at_word.and_then(rfc3339_to_unix);
        let last_outcome =
            last_pass_at.map(|_| PassOutcome::from_word(word("last_pass").unwrap_or_default()));
        let last_success = stamp("last_success_at")
            .max(last_pass_at.filter(|_| last_outcome == Some(PassOutcome::Ok)));
        let target_belongs_to_end = last_pass_at_word
            .filter(|end| end.ends_with(".000000000Z"))
            .is_some_and(|end| word("last_pass_attempted_at") == Some(end));
        let last_pass_target = last_pass_at.filter(|_| target_belongs_to_end).zip(
            record
                .get("last_pass_attempted_index_build")
                .and_then(aterm_toml::Value::as_integer)
                .and_then(|build| u64::try_from(build).ok())
                .filter(|build| *build > 0),
        );
        Self {
            last_success,
            last_attempt: last_pass_at.max(last_success),
            last_outcome,
            last_index_build: record
                .get("last_index_build")
                .and_then(aterm_toml::Value::as_integer)
                .and_then(|build| u64::try_from(build).ok())
                .unwrap_or(0),
            last_pass_target,
            in_flight: false,
        }
    }

    /// The stamps of the `status.toml` at `path`; absent or unreadable reads as none.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        read_text_bounded(path, 64 * 1024, false)
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    /// Whether a pass is installing now, or was attempted anywhere on the machine within
    /// [`PASS_SPACING_SECS`] of `now_unix`.
    #[must_use]
    pub fn attempted_recently(&self, now_unix: i64) -> bool {
        self.in_flight
            || self
                .last_attempt
                .is_some_and(|then| !elapsed(then, now_unix, PASS_SPACING_SECS))
    }

    /// Whether the latest completed full pass attempted an index older than `published`.
    #[must_use]
    pub fn completed_older_index_pass(&self, published: u64) -> bool {
        !self.in_flight
            && self.last_outcome.is_some()
            && self.last_pass_target.is_some_and(|(ended, attempted)| {
                self.last_attempt == Some(ended) && published > attempted
            })
    }

    /// Whether the machine's last full pass failed.
    #[must_use]
    pub fn last_failed(&self) -> bool {
        match (self.last_outcome, self.last_attempt) {
            (Some(outcome), Some(attempt)) => {
                outcome != PassOutcome::Ok && self.last_success.is_none_or(|ok| ok < attempt)
            }
            _ => false,
        }
    }
}

/// Whether a lane's own pass that failed at `failed_at` has been healed.
#[must_use]
pub fn healed(stamps: &Stamps, failed_at: i64) -> bool {
    stamps
        .last_success
        .is_some_and(|success| success > failed_at)
}

/// Seconds from `now_unix` until a launch owes its first `update` pass (`0`: now): at once when no
/// pass ever succeeded or the last one failed (the failure ladder's minutes-scale retry), else the
/// rest of [`FULL_PASS_INTERVAL_SECS`] since the last success.
#[must_use]
pub fn launch_due_in(stamps: &Stamps, now_unix: i64) -> u64 {
    match stamps.last_success {
        Some(success) if !stamps.last_failed() => {
            remaining(success, now_unix, FULL_PASS_INTERVAL_SECS)
        }
        _ => 0,
    }
}

/// Why the machine owes a full pass ([`full_pass_owed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owed {
    /// No update pass has ever succeeded here.
    NeverChecked,
    /// The last success is [`FULL_PASS_INTERVAL_SECS`] old: the fallback walk.
    Interval,
}

/// The full pass owed at `now_unix`, or `None` while [`full_pass_due_in`] holds it back.
#[must_use]
pub fn full_pass_owed(stamps: &Stamps, now_unix: i64) -> Option<Owed> {
    if full_pass_due_in(stamps, now_unix) > 0 {
        return None;
    }
    Some(match stamps.last_success {
        None => Owed::NeverChecked,
        Some(_) => Owed::Interval,
    })
}

/// Seconds from `now_unix` until [`full_pass_owed`] owes a pass (`0`: now): the latest of a pass
/// installing (the spacing), the spacing after the last attempt, the interval after a failed
/// attempt, and the interval after the last success.
#[must_use]
pub fn full_pass_due_in(stamps: &Stamps, now_unix: i64) -> u64 {
    let spacing = if stamps.in_flight {
        PASS_SPACING_SECS
    } else {
        stamps
            .last_attempt
            .map_or(0, |then| remaining(then, now_unix, PASS_SPACING_SECS))
    };
    let failed = match stamps.last_attempt {
        Some(then) if stamps.last_failed() => remaining(then, now_unix, FULL_PASS_INTERVAL_SECS),
        _ => 0,
    };
    let interval = stamps
        .last_success
        .map_or(0, |then| remaining(then, now_unix, FULL_PASS_INTERVAL_SECS));
    spacing.max(failed).max(interval)
}

fn elapsed(then: i64, now_unix: i64, every_secs: u64) -> bool {
    let every = i64::try_from(every_secs).unwrap_or(i64::MAX);
    let age = now_unix.saturating_sub(then);
    age >= every || age < every.saturating_neg()
}

fn remaining(then: i64, now_unix: i64, every_secs: u64) -> u64 {
    if elapsed(then, now_unix, every_secs) {
        return 0;
    }
    let every = i64::try_from(every_secs).unwrap_or(i64::MAX);
    let left = then.saturating_add(every).saturating_sub(now_unix);
    u64::try_from(left).unwrap_or(0).min(every_secs)
}

const CLAIM_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Claim a lane's once-per-`every_secs` slot in `stamp` (unix seconds, one line) at `now_unix`.
#[must_use]
pub fn claim(stamp: &Path, now_unix: i64, every_secs: u64) -> bool {
    let lock = stamp.with_extension("lock");
    let _held = match crate::FileLock::acquire_within(&lock, CLAIM_WAIT) {
        Ok(held) => held,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return false,
        Err(_) => return true,
    };
    let last = read_text_bounded(stamp, 256, true).and_then(|text| {
        let text = text.trim();
        (!text.starts_with('+'))
            .then(|| text.parse::<i64>().ok())
            .flatten()
    });
    if last.is_some_and(|then| !elapsed(then, now_unix, every_secs)) {
        return false;
    }
    write_stamp(stamp, now_unix);
    true
}

fn read_text_bounded(path: &Path, cap: u64, nofollow: bool) -> Option<String> {
    use std::io::Read as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let flags =
            libc::O_NONBLOCK | libc::O_CLOEXEC | if nofollow { libc::O_NOFOLLOW } else { 0 };
        opts.custom_flags(flags);
    }
    #[cfg(not(unix))]
    if nofollow
        && std::fs::symlink_metadata(path)
            .ok()
            .is_some_and(|m| m.file_type().is_symlink())
    {
        return None;
    }
    let file = opts.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > cap {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    file.take(cap.saturating_add(1))
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= cap).then_some(text)
}

fn write_stamp(stamp: &Path, now_unix: i64) {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(stamp)
        .ok()
        .is_some_and(|m| m.file_type().is_symlink())
    {
        return;
    }
    let Ok(mut file) = opts.open(stamp) else {
        return;
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return;
    }
    if file.set_len(0).is_ok() {
        let _ = writeln!(file, "{now_unix}");
    }
}

/// A package status stamp → unix seconds: strict about the zone, tolerant of a fraction.
#[must_use]
pub fn rfc3339_to_unix(s: &str) -> Option<i64> {
    aterm_types::rfc3339::parse_utc_fractional(s)
}

#[cfg(test)]
#[path = "pkg_check_tests.rs"]
mod tests;
