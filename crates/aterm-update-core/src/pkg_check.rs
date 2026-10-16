// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Whether the toolchain package manager (atpkg) has completed an update check on this
//! machine, whether its last one reached the signed index, and how it ended, read off its
//! `status.toml` alone so edges that must not link atpkg (the `aterm` session lane, the
//! window) can decide on it.
//!
//! A full `update` pass writes the record at its end, and nothing else writes these keys (the
//! derived models `AtpkgPassStamps` and `AtpkgFullPassRule` state the contract):
//!
//! * `last_success_at`: a pass that reached the signed index (verified an index the channel
//!   served this pass, not the cache's) and ran to its end. A cache-served, offline or
//!   failed-resolve pass stamps none, or a sibling's failure ladder would read it as healed.
//!   Absent or empty is "never checked".
//! * `last_pass` / `last_pass_at`: how the pass ended ([`PassOutcome`]). The schedulers read
//!   these, never `updated_at`: every write moves `updated_at`, so "written after the last
//!   success" would read as a failed pass.
//! * `last_pass_attempted_index_build` / `last_pass_attempted_at`: the index the pass was
//!   launched for or resolved, bound to its end in the same write, so a newer published index
//!   is told apart from a sibling's attempt at the same one.
//!
//! It is data, never printed into a shell the user did not ask to update; `aterm pkg doctor`
//! reports it.
//!
//! The machine-wide pass rule lives here too ([`full_pass_owed`], [`full_pass_due_in`]), read
//! by the session's one-shot and the window's walk, so aterm processes never run the same pass
//! back to back and a failing pass is retried once per interval, not once per tab (the window
//! retries its own failure on its ladder until [`healed`]). New publications arrive sooner
//! through the window's hints. There is no rate-limit hold: the pass makes no metered request,
//! and a 429 is an ordinary failure.

use std::ffi::OsString;
use std::path::Path;

/// The exit code (sysexits `EX_UNAVAILABLE`) of an atpkg update pass that reached nothing:
/// the link to every host failed (DNS, connect, timeout, TLS). A host that answered with
/// anything else (a rate limit, a 5xx, a portal page) is a failure, exit 1. Offline stamps no
/// `last_success_at`, records `offline`, and is retried quietly and soon. Shared by atpkg and
/// the window so the exit and its classification are one constant.
pub const PASS_OFFLINE_EXIT: u8 = 69;

/// The fallback walk: a full pass is owed once the machine's last success is this old,
/// whatever the hints say. Not a day: the next-index probe answers only for the shipped public
/// index (a repointed or private index, an `ATPKG_REGISTRY` directory or a proxy that rewrites
/// GitHub's redirect get `Deferred`), and there this walk is the only way a new index is
/// found. Machine-wide, so every window and tab share one walk per interval.
pub const FULL_PASS_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// No lane starts a scheduled full pass within this long of the last full pass attempted on
/// the machine ([`Stamps::last_attempt`]), nor while one is installing.
pub const PASS_SPACING_SECS: u64 = 5 * 60;

/// How a full `update` pass ended, as it records itself in `status.toml` (`last_pass`, with
/// `last_pass_at` its end). The schedulers read this ([`Stamps::last_failed`]), never
/// `updated_at`, which every writer moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassOutcome {
    /// The pass reached the signed index and ran to its end (a failed member is recorded in
    /// its own row), stamping `last_success_at`; or it had nothing to check, which stamps no
    /// success but counts as one ([`Stamps::last_success`]).
    Ok,
    /// No host answered ([`PASS_OFFLINE_EXIT`]): nothing was checked.
    Offline,
    /// Anything else: the index was not reached (a refusal the cache stood in for, a resolve
    /// that failed) or nothing was installable here.
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

    /// The outcome a recorded word names; a word this build does not know is not a success,
    /// so it reads as [`Self::Failed`].
    #[must_use]
    pub fn from_word(word: &str) -> Self {
        match word.trim() {
            "ok" => Self::Ok,
            "offline" => Self::Offline,
            _ => Self::Failed,
        }
    }

    /// The outcome of a pass that exited `code`: [`Self::Ok`] when it stamped a success,
    /// whatever the exit; else offline or failed by the exit.
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

/// How long a scheduled pass queues at the store lock (`--wait-lock`) before it stands aside
/// with atpkg's contention exit.
pub const PASS_WAIT_LOCK_SECS: u64 = 30 * 60;

/// The flags every scheduled pass carries after its verb: `--wait-lock`
/// ([`PASS_WAIT_LOCK_SECS`]), so a contended pass queues, and `--progress-file` under the
/// store when there is one, so every lane can see a pass in flight.
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
    /// Unix seconds of the last successful pass (`last_success_at`), or of a later full pass
    /// that recorded [`PassOutcome::Ok`] with nothing to check; the walk counts from either.
    pub last_success: Option<i64>,
    /// Unix seconds of the last full pass attempted, failed or not: `last_pass_at`, or the last
    /// success when that is later. Never `updated_at`, which every writer moves.
    pub last_attempt: Option<i64>,
    /// How the last full pass ended (`last_pass`); `None` when no pass recorded one.
    pub last_outcome: Option<PassOutcome>,
    /// The index build the last pass resolved (`last_index_build`; `0` for none). A caller
    /// that can read the store's verified floor raises it to that.
    pub last_index_build: u64,
    /// The last full pass's requested or resolved index, bound to its end stamp in the same
    /// write; `None` for older records or a pass that knew no index.
    pub last_pass_target: Option<(i64, u64)>,
    /// Whether a pass is installing on the store now (its progress file names a live writer).
    /// Not in `status.toml`: the caller that reads the progress file sets it, and it counts as
    /// an attempt this moment.
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
        // An older writer can carry the target fields along as unknown keys while rewriting
        // the pass end in whole seconds, so the target counts only beside the target-bearing
        // writer's `.000000000Z` end.
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
        std::fs::read_to_string(path).map_or_else(|_| Self::default(), |text| Self::parse(&text))
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

    /// Whether the latest completed full pass attempted an index older than `published`. Only
    /// that lets a newer published hint bypass sibling spacing: an in-flight pass, an older
    /// record, or a target not bound to the latest attempt keeps the wait, and a failure at the
    /// same build stays deduplicated.
    #[must_use]
    pub fn completed_older_index_pass(&self, published: u64) -> bool {
        !self.in_flight
            && self.last_outcome.is_some()
            && self.last_pass_target.is_some_and(|(ended, attempted)| {
                self.last_attempt == Some(ended) && published > attempted
            })
    }

    /// Whether the machine's last full pass failed: it recorded an outcome other than
    /// [`PassOutcome::Ok`] and no success was stamped since. A write after the last success is no
    /// failed pass.
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

/// Whether a lane's own pass that failed at `failed_at` has been healed: a pass that ended
/// `ok` ended after it, anywhere on the machine. A cache-served pass after a refusal records
/// `failed` and heals nothing (`AtpkgPassStamps`' `HealedOnlyByAReachedPass`).
#[must_use]
pub fn healed(stamps: &Stamps, failed_at: i64) -> bool {
    stamps
        .last_success
        .is_some_and(|success| success > failed_at)
}

/// Seconds from `now_unix` until a launch owes its first `update` pass (`0`: now): at once
/// when no pass ever succeeded or the last one failed (the failure ladder's minutes-scale
/// retry), else the rest of [`FULL_PASS_INTERVAL_SECS`] since the last success.
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

/// The full pass owed at `now_unix` (none ever succeeded, or the last success is
/// [`FULL_PASS_INTERVAL_SECS`] old), or `None` while [`full_pass_due_in`] holds it back.
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

/// Seconds from `now_unix` until [`full_pass_owed`] owes a pass (`0`: now): the latest of a
/// pass installing (the spacing), the spacing after the last attempt, the interval after a
/// failed attempt, and the interval after the last success. Each is bounded by its own period,
/// so a stamp from a clock set back holds a lane no longer than that.
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

/// Whether `every_secs` have passed since `then` at `now_unix`. A stamp more than a period in
/// the future counts as passed: a clock since set back wrote it, and trusting it would stall
/// the lane until that clock came round again.
fn elapsed(then: i64, now_unix: i64, every_secs: u64) -> bool {
    let every = i64::try_from(every_secs).unwrap_or(i64::MAX);
    let age = now_unix.saturating_sub(then);
    age >= every || age < every.saturating_neg()
}

/// Seconds left until [`elapsed`] answers true for `then`, never more than one period.
fn remaining(then: i64, now_unix: i64, every_secs: u64) -> u64 {
    if elapsed(then, now_unix, every_secs) {
        return 0;
    }
    let every = i64::try_from(every_secs).unwrap_or(i64::MAX);
    let left = then.saturating_add(every).saturating_sub(now_unix);
    u64::try_from(left).unwrap_or(0).min(every_secs)
}

/// How long [`claim`] waits for a sibling that is claiming the same slot this instant.
const CLAIM_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Claim a lane's once-per-`every_secs` slot in `stamp` (unix seconds, one line) at
/// `now_unix`: `true`, with the stamp moved to now, when the last claim is that old, absent or
/// unreadable; `false` while it is younger or another process holds the claim. Serialized by
/// an advisory lock on `<stamp>.lock`, held for the read and write only. A stamp that cannot
/// be locked (no directory, a read-only prefix) answers `true`, as if there were no stamp.
#[must_use]
pub fn claim(stamp: &Path, now_unix: i64, every_secs: u64) -> bool {
    let lock = stamp.with_extension("lock");
    let _held = match crate::FileLock::acquire_within(&lock, CLAIM_WAIT) {
        Ok(held) => held,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return false,
        Err(_) => return true,
    };
    let last = std::fs::read_to_string(stamp)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    if last.is_some_and(|then| !elapsed(then, now_unix, every_secs)) {
        return false;
    }
    let _ = std::fs::write(stamp, format!("{now_unix}\n"));
    true
}

/// A package status stamp → unix seconds: strict about the zone, tolerant of a fraction
/// (`aterm_types::rfc3339::parse_utc_fractional`). The pass writer stamps `last_pass_at` as
/// `….000000000Z`; an offset is refused, since reading it as UTC would move every freshness
/// verdict drawn from it by up to 14 h.
#[must_use]
pub fn rfc3339_to_unix(s: &str) -> Option<i64> {
    aterm_types::rfc3339::parse_utc_fractional(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aterm-pkg-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("status.toml")
    }

    /// The success, the attempt, the outcome and the index build are read off the record;
    /// absent, empty and unparseable read as none, and `updated_at` is read by nothing.
    #[test]
    fn the_stamps_are_read_off_the_record() {
        let p = scratch("stamps");
        assert_eq!(Stamps::read(&p), Stamps::default(), "absent file");
        std::fs::write(
            &p,
            "schema = 1\nupdated_at = \"2026-09-10T09:00:00Z\"\n\
             last_success_at = \"2026-09-10T06:40:53Z\"\nlast_pass = \"failed\"\n\
             last_pass_at = \"2026-09-10T08:34:12Z\"\nlast_index_build = 43\n",
        )
        .unwrap();
        assert_eq!(
            Stamps::read(&p),
            Stamps {
                last_success: rfc3339_to_unix("2026-09-10T06:40:53Z"),
                last_attempt: rfc3339_to_unix("2026-09-10T08:34:12Z"),
                last_outcome: Some(PassOutcome::Failed),
                last_index_build: 43,
                last_pass_target: None,
                in_flight: false,
            }
        );
        // Only a success stamp (an older record): it is the attempt, and no outcome is read.
        std::fs::write(
            &p,
            "updated_at = \"2026-09-10T09:00:00Z\"\nlast_success_at = \"2026-09-10T08:34:12Z\"\n",
        )
        .unwrap();
        let only = Stamps::read(&p);
        assert_eq!(only.last_attempt, only.last_success);
        assert_eq!(only.last_outcome, None);
        assert!(!only.last_failed());
        assert_eq!(only.last_index_build, 0);
        // A success stamped after the recorded pass is the later attempt.
        std::fs::write(
            &p,
            "last_success_at = \"2026-09-10T08:34:12Z\"\nlast_pass = \"failed\"\n\
             last_pass_at = \"2026-09-10T07:00:00Z\"\n",
        )
        .unwrap();
        let healed = Stamps::read(&p);
        assert_eq!(healed.last_attempt, healed.last_success);
        assert!(!healed.last_failed(), "a success since the failed pass");
        // A word this build does not know is no success; a word with no time is no pass.
        std::fs::write(
            &p,
            "last_pass = \"settled\"\nlast_pass_at = \"2026-09-10T07:00:00Z\"\n",
        )
        .unwrap();
        assert_eq!(Stamps::read(&p).last_outcome, Some(PassOutcome::Failed));
        std::fs::write(&p, "last_pass = \"ok\"\nlast_pass_at = \"\"\n").unwrap();
        assert_eq!(Stamps::read(&p).last_outcome, None);
        std::fs::write(&p, "not = [valid toml").unwrap();
        assert_eq!(Stamps::read(&p), Stamps::default());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A pass that stamped a success is `ok` whatever it exited, one that reached no host is
    /// `offline`, anything else `failed`; each word reads back as itself.
    #[test]
    fn a_pass_records_its_outcome_by_what_it_stamped() {
        assert_eq!(PassOutcome::of_pass(true, 0), PassOutcome::Ok);
        assert_eq!(PassOutcome::of_pass(true, 1), PassOutcome::Ok);
        assert_eq!(
            PassOutcome::of_pass(false, PASS_OFFLINE_EXIT),
            PassOutcome::Offline
        );
        for code in [0, 1, 2, 75] {
            assert_eq!(
                PassOutcome::of_pass(false, code),
                PassOutcome::Failed,
                "{code}"
            );
        }
        for outcome in [PassOutcome::Ok, PassOutcome::Offline, PassOutcome::Failed] {
            assert_eq!(PassOutcome::from_word(outcome.word()), outcome);
        }
    }

    /// A seven-hour-old success and a write a minute ago: the walk is owed now, since the
    /// write is no failed pass. Negative control: the same record whose last pass failed holds
    /// the interval from that pass.
    #[test]
    fn a_write_after_the_last_success_is_no_failed_pass() {
        let now = 1_790_000_000_i64;
        let hours = |n: i64| n * 3600;
        let success = now - hours(7);
        let text = |outcome: &str, pass_at: i64| {
            format!(
                "updated_at = \"{}\"\nlast_success_at = \"{}\"\nlast_pass = \"{outcome}\"\n\
                 last_pass_at = \"{}\"\n",
                aterm_types::rfc3339::format_rfc3339((now - 60).unsigned_abs()),
                aterm_types::rfc3339::format_rfc3339(success.unsigned_abs()),
                aterm_types::rfc3339::format_rfc3339(pass_at.unsigned_abs()),
            )
        };
        let ok = Stamps::parse(&text("ok", success));
        assert!(!ok.last_failed());
        assert_eq!(full_pass_owed(&ok, now), Some(Owed::Interval));
        let failed = Stamps::parse(&text("failed", now - hours(1)));
        assert!(failed.last_failed());
        assert_eq!(full_pass_owed(&failed, now), None);
        assert_eq!(
            full_pass_due_in(&failed, now),
            FULL_PASS_INTERVAL_SECS - 3600,
            "six hours after the failed pass, not after the write"
        );
    }

    /// Only a pass that ended `ok` strictly after the lane's own failure heals it (a
    /// cache-served pass records `failed`). A launch owes a pass at once when none ever
    /// succeeded or the last failed, else the rest of the interval since the last success.
    #[test]
    fn a_failure_is_healed_only_by_a_later_ok_pass_and_a_launch_retries_it_at_once() {
        let now = 1_790_000_000_i64;
        let at = |unix: i64| aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs());
        let record = |success: Option<i64>, outcome: &str, pass_at: i64| {
            Stamps::parse(&format!(
                "{}last_pass = \"{outcome}\"\nlast_pass_at = \"{}\"\n",
                success.map_or(String::new(), |s| format!(
                    "last_success_at = \"{}\"\n",
                    at(s)
                )),
                at(pass_at),
            ))
        };
        let own_failure = now - 600;
        assert!(!healed(&Stamps::default(), own_failure));
        // A sibling's cache-served pass after a refusal: `failed`, no success, no heal.
        let cached = record(Some(now - 7200), "failed", now - 60);
        assert!(
            !healed(&cached, own_failure),
            "a cache-served pass reached nothing"
        );
        // A sibling's pass that reached the index after the lane's failure heals it.
        let reached = record(Some(now - 60), "ok", now - 60);
        assert!(healed(&reached, own_failure));
        assert!(
            !healed(&record(Some(own_failure), "ok", own_failure), own_failure),
            "same second: not after"
        );
        let interval = i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap();
        assert_eq!(launch_due_in(&Stamps::default(), now), 0, "never checked");
        assert_eq!(launch_due_in(&cached, now), 0, "the last pass failed");
        assert_eq!(
            launch_due_in(&record(Some(now - 60), "ok", now - 60), now),
            FULL_PASS_INTERVAL_SECS - 60
        );
        assert_eq!(
            launch_due_in(&record(Some(now - interval), "ok", now - interval), now),
            0
        );
    }

    /// A full pass with nothing to check records `ok` without a success stamp, and the walk
    /// counts from it; a later success is the walk's, and a failed pass end is no success.
    #[test]
    fn a_pass_with_nothing_to_check_counts_as_the_last_success() {
        let now = 1_790_000_000_i64;
        let at = |unix: i64| aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs());
        let empty = Stamps::parse(&format!(
            "last_pass = \"ok\"\nlast_pass_at = \"{}\"\n",
            at(now - 600)
        ));
        assert_eq!(empty.last_success, Some(now - 600));
        assert!(!empty.last_failed());
        assert_eq!(full_pass_owed(&empty, now), None);
        assert_eq!(
            full_pass_owed(
                &empty,
                now - 600 + i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap()
            ),
            Some(Owed::Interval)
        );
        let later = Stamps::parse(&format!(
            "last_success_at = \"{}\"\nlast_pass = \"ok\"\nlast_pass_at = \"{}\"\n",
            at(now - 60),
            at(now - 600)
        ));
        assert_eq!(later.last_success, Some(now - 60));
        let failed = Stamps::parse(&format!(
            "last_pass = \"failed\"\nlast_pass_at = \"{}\"\n",
            at(now - 600)
        ));
        assert_eq!(failed.last_success, None, "the negative control");
        assert!(failed.last_failed());
    }

    /// Never succeeded or a six-hour-old success ⇒ owed; a younger one ⇒ not, with
    /// [`full_pass_due_in`] naming the rest; nothing while a pass installs or within the
    /// spacing of the last attempt anywhere on the machine.
    #[test]
    fn a_full_pass_is_owed_on_the_interval_or_never_checked_and_never_back_to_back() {
        let now = 1_790_000_000_i64;
        let interval = i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap();
        let spacing = i64::try_from(PASS_SPACING_SECS).unwrap();
        let at = |success: Option<i64>, attempt: Option<i64>| Stamps {
            last_success: success,
            last_attempt: attempt,
            ..Stamps::default()
        };
        assert_eq!(FULL_PASS_INTERVAL_SECS, 6 * 60 * 60);
        assert_eq!(
            full_pass_owed(&Stamps::default(), now),
            Some(Owed::NeverChecked),
            "no record at all"
        );
        let installing = Stamps {
            in_flight: true,
            ..Stamps::default()
        };
        assert_eq!(
            full_pass_owed(&installing, now),
            None,
            "a pass is installing now: the next look sees what it left"
        );
        assert_eq!(full_pass_due_in(&installing, now), PASS_SPACING_SECS);
        let checked = now - interval + 60;
        assert_eq!(full_pass_owed(&at(Some(checked), Some(checked)), now), None);
        assert_eq!(
            full_pass_due_in(&at(Some(checked), Some(checked)), now),
            60,
            "the rest of the interval"
        );
        assert_eq!(
            full_pass_owed(&at(Some(now - interval), Some(now - interval)), now),
            Some(Owed::Interval),
            "six hours to the second"
        );
        assert_eq!(
            full_pass_owed(&at(Some(now - 2 * interval), Some(now - 60)), now),
            None,
            "owed, but a sibling attempted a minute ago"
        );
        // A clock set back: a stamp within a period ahead reads as fresh (for at most a
        // period); one further ahead is not trusted.
        assert_eq!(
            full_pass_owed(&at(Some(now + 3600), Some(now - spacing)), now),
            None
        );
        assert_eq!(
            full_pass_due_in(&at(Some(now + 3600), Some(now - spacing)), now),
            FULL_PASS_INTERVAL_SECS,
            "never longer than one interval"
        );
        assert_eq!(
            full_pass_owed(&at(Some(now + 2 * interval), Some(now - spacing)), now),
            Some(Owed::Interval)
        );
        assert!(!at(None, Some(now + 2 * spacing)).attempted_recently(now));
        assert!(at(None, Some(now + spacing - 1)).attempted_recently(now));
    }

    /// Once the last pass failed after the last success (or none ever succeeded), nothing is
    /// owed for the interval after it: a record whose passes never succeed costs one pass per
    /// interval, not one per tab.
    #[test]
    fn a_failing_pass_is_owed_again_only_after_the_interval() {
        let now = 1_790_000_000_i64;
        let interval = i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap();
        let spacing = i64::try_from(PASS_SPACING_SECS).unwrap();
        let at = |success: Option<i64>, attempt: Option<i64>| Stamps {
            last_success: success,
            last_attempt: attempt,
            last_outcome: attempt.map(|attempt| {
                if success == Some(attempt) {
                    PassOutcome::Ok
                } else {
                    PassOutcome::Failed
                }
            }),
            ..Stamps::default()
        };
        for never in [
            at(None, Some(now - spacing)),
            at(None, Some(now - 2 * 3600)),
            at(None, Some(now - interval + 1)),
        ] {
            assert!(never.last_failed());
            assert_eq!(full_pass_owed(&never, now), None, "{never:?}");
        }
        assert_eq!(
            full_pass_due_in(&at(None, Some(now - spacing)), now),
            FULL_PASS_INTERVAL_SECS - PASS_SPACING_SECS
        );
        assert_eq!(
            full_pass_owed(&at(None, Some(now - interval)), now),
            Some(Owed::NeverChecked),
            "an interval on: owed again"
        );
        // A failed walk: the success is a day old, the failed attempt an hour ago.
        let walk_failed = at(Some(now - 4 * interval), Some(now - 3600));
        assert!(walk_failed.last_failed());
        assert_eq!(full_pass_owed(&walk_failed, now), None);
        assert_eq!(
            full_pass_owed(&walk_failed, now + interval - 3600),
            Some(Owed::Interval)
        );
        // Negative control: a six-hour-old success whose attempt is that success.
        let walk_ok = at(Some(now - interval), Some(now - interval));
        assert!(!walk_ok.last_failed());
        assert_eq!(full_pass_owed(&walk_ok, now), Some(Owed::Interval));
        assert!(!Stamps::default().last_failed(), "never attempted");
    }

    /// One argv for every lane: the lock wait, and the progress file where there is a store.
    #[test]
    fn every_scheduled_pass_carries_the_same_flags() {
        let words = |flags: Vec<OsString>| {
            flags
                .into_iter()
                .map(|f| f.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(words(pass_flags(None)), ["--wait-lock", "1800"]);
        assert_eq!(
            words(pass_flags(Some(Path::new("/p/progress.json")))),
            ["--wait-lock", "1800", "--progress-file", "/p/progress.json"]
        );
        assert_eq!(PASS_WAIT_LOCK_SECS, 30 * 60);
    }

    /// The first claim wins and stamps, a second inside the period is refused, one a period
    /// later wins; a garbage or far-future stamp is claimable, and a stamp with no directory
    /// answers `true`.
    #[test]
    fn a_lane_slot_is_claimed_once_per_period() {
        let dir = scratch("claim");
        let stamp = dir.parent().unwrap().join("machine-apply.stamp");
        let now = 1_790_000_000_i64;
        let day: u64 = 24 * 60 * 60;
        let day_secs = i64::try_from(day).unwrap();
        assert!(claim(&stamp, now, day), "first claim");
        assert_eq!(
            std::fs::read_to_string(&stamp).unwrap().trim(),
            now.to_string()
        );
        assert!(!claim(&stamp, now + 60, day), "inside the day");
        assert!(!claim(&stamp, now + day_secs - 1, day));
        assert!(claim(&stamp, now + day_secs, day), "a day later");
        assert!(!claim(&stamp, now + day_secs + 1, day), "and stamped again");
        std::fs::write(&stamp, "garbage").unwrap();
        assert!(claim(&stamp, now, day), "an unreadable stamp is claimable");
        std::fs::write(&stamp, format!("{}\n", now + 3 * day_secs)).unwrap();
        assert!(claim(&stamp, now, day), "a far-future stamp is claimable");
        let nowhere = dir.parent().unwrap().join("absent/dir/x.stamp");
        assert!(
            claim(&nowhere, now, day),
            "no directory: nothing to hold it"
        );
        assert!(claim(&nowhere, now, day), "…and nothing refuses either");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// A sibling holding the claim lock makes the claim answer `false` within its short wait.
    #[test]
    fn a_claim_held_elsewhere_is_not_taken_twice() {
        let dir = scratch("claim-held");
        let stamp = dir.parent().unwrap().join("session-pass.stamp");
        let held = crate::FileLock::acquire(&stamp.with_extension("lock")).unwrap();
        let started = std::time::Instant::now();
        assert!(!claim(&stamp, 1_790_000_000, PASS_SPACING_SECS));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the claim blocked past its wait"
        );
        drop(held);
        assert!(claim(&stamp, 1_790_000_000, PASS_SPACING_SECS));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// Strict about the zone, tolerant of a fraction.
    #[test]
    fn the_stamp_parser_is_pinned() {
        assert_eq!(rfc3339_to_unix("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(
            rfc3339_to_unix("2026-09-10T06:40:53.123Z"),
            rfc3339_to_unix("2026-09-10T06:40:53Z")
        );
        assert_eq!(
            rfc3339_to_unix("2026-09-10T06:40:53.000000000Z"),
            rfc3339_to_unix("2026-09-10T06:40:53Z"),
            "the pass writer's own shape"
        );
        for bad in [
            "",
            "2026-09-10",
            "2026/09/10T06:40:53Z",
            "2026-13-10T06:40:53Z",
            // An offset is a different instant, not a decoration to ignore.
            "2026-09-10T06:40:53+05:30",
            "2026-09-10T06:40:53.123-08:00",
            "2026-09-10T06:40:53",
            "2026-09-10T06:40:53Zjunk",
        ] {
            assert_eq!(rfc3339_to_unix(bad), None, "{bad:?}");
        }
    }
}
