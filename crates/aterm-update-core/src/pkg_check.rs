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
    let last = std::fs::read_to_string(stamp)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    if last.is_some_and(|then| !elapsed(then, now_unix, every_secs)) {
        return false;
    }
    let _ = std::fs::write(stamp, format!("{now_unix}\n"));
    true
}

/// A package status stamp → unix seconds: strict about the zone, tolerant of a fraction.
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
        std::fs::write(
            &p,
            "last_success_at = \"2026-09-10T08:34:12Z\"\nlast_pass = \"failed\"\n\
             last_pass_at = \"2026-09-10T07:00:00Z\"\n",
        )
        .unwrap();
        let healed = Stamps::read(&p);
        assert_eq!(healed.last_attempt, healed.last_success);
        assert!(!healed.last_failed(), "a success since the failed pass");
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
        let cached = record(Some(now - 7200), "failed", now - 60);
        assert!(
            !healed(&cached, own_failure),
            "a cache-served pass reached nothing"
        );
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
        let walk_failed = at(Some(now - 4 * interval), Some(now - 3600));
        assert!(walk_failed.last_failed());
        assert_eq!(full_pass_owed(&walk_failed, now), None);
        assert_eq!(
            full_pass_owed(&walk_failed, now + interval - 3600),
            Some(Owed::Interval)
        );
        let walk_ok = at(Some(now - interval), Some(now - interval));
        assert!(!walk_ok.last_failed());
        assert_eq!(full_pass_owed(&walk_ok, now), Some(Owed::Interval));
        assert!(!Stamps::default().last_failed(), "never attempted");
    }

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
            "2026-09-10T06:40:53+05:30",
            "2026-09-10T06:40:53.123-08:00",
            "2026-09-10T06:40:53",
            "2026-09-10T06:40:53Zjunk",
        ] {
            assert_eq!(rfc3339_to_unix(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn completed_older_index_pass_requires_settled_matching_attempt_below_published() {
        let stamps = Stamps::parse(
            "last_pass_at = \"2026-09-10T06:40:53.000000000Z\"\n\
             last_pass = \"ok\"\n\
             last_pass_attempted_at = \"2026-09-10T06:40:53.000000000Z\"\n\
             last_pass_attempted_index_build = 100\n",
        );
        assert!(stamps.completed_older_index_pass(101));
        assert!(!stamps.completed_older_index_pass(100));
        assert!(!stamps.completed_older_index_pass(99));

        let mut in_flight = stamps;
        in_flight.in_flight = true;
        assert!(!in_flight.completed_older_index_pass(101));

        let mut no_outcome = stamps;
        no_outcome.last_outcome = None;
        assert!(!no_outcome.completed_older_index_pass(101));

        let mut superseded_attempt = stamps;
        superseded_attempt.last_attempt = stamps.last_attempt.map(|t| t + 10);
        assert!(!superseded_attempt.completed_older_index_pass(101));
    }
}
