// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Has the TOOLCHAIN package manager (atpkg) ever completed an update check on this
//! machine — read off its `status.toml` alone, so an edge that must not link atpkg's
//! internals (the `aterm` session lane, the window) can decide on it (R3, 2026-09-10).
//!
//! The contract is deliberately tiny and shared: atpkg stamps
//! `last_success_at = "<RFC3339>"` into `<prefix>/status.toml` ONLY on a successful
//! pass (every other write moves `updated_at`, which is a last-write stamp and says
//! nothing about success). "Never checked" is therefore: the file is absent, or it
//! carries no `last_success_at`, or that value is empty. DATA, never a line: until
//! Phase 2 (2026-09-22) every console edge printed it on stderr; now nothing prints into
//! a shell the user did not ask to update, and `aterm pkg doctor` reports it.
//!
//! THE MACHINE-WIDE PASS RULE lives here too (Phase 3, 2026-09-23), read by both lanes
//! that schedule a full pass on their own — the terminal session's one-shot
//! ([`full_pass_owed`]) and, for its interval, the window's walk ([`full_pass_due_in`]): a
//! full `update` pass is owed when none has ever succeeded or the last success is
//! [`FULL_PASS_INTERVAL_SECS`] old — never while a pass is installing, never within
//! [`PASS_SPACING_SECS`] of the last full pass ATTEMPTED anywhere on the machine, so
//! several aterm processes never run the same pass back to back, never within the interval
//! of one that FAILED, so a pass that keeps failing is retried once per interval by this
//! rule, not once per tab (the 2026-09-10 rule; the window retries its OWN failed pass on
//! its failure ladder). What was published reaches the machine sooner through the window's
//! hints — atpkg's next-index probe and the vendor head watch — never through this rule. It
//! replaced the two lanes' separate interval readers and their `ATPKG_UPDATE_INTERVAL_SECS`
//! knob, both deleted. A rate-limit hold ([`Stamps::metered_hold_in`]) is not this rule's:
//! a pass records one only with its end, which this rule already waits an interval past.
//!
//! THE PASS RECORDS HOW IT ENDED (`last_pass` / `last_pass_at`, [`PassOutcome`]), and the rule
//! reads that, never `updated_at`: every write moves `updated_at` — a vendor head-watch pass,
//! a typed verb — so "written after the last success" read as a FAILED pass and held the
//! session's pass back up to six hours (found in the reconcile of 2026-09-23). The rule is
//! the derived model `AtpkgFullPassRule` (aterm-spec), bound to the real writer and readers
//! by atpkg's and the window's conformance tests.

use std::ffi::OsString;
use std::path::Path;

/// The exit code of an atpkg update pass that reached NOTHING (Phase 3, 2026-09-22): no
/// host answered its index listing — the link itself failed (DNS, connect, timeout, TLS);
/// a listing refused with a status (a rate limit, a revoked token) is a failure, exit 1 —
/// no vendor's release channel answered, and every member it lost was lost at a fetch.
/// Not a success (it stamps no `last_success_at`), not a failure (nothing is wrong but the
/// network), not contention (75): a pass to retry quietly and soon. Sysexits
/// `EX_UNAVAILABLE`. Here, in the crate both atpkg and the window read, so the code the
/// pass exits with and the code the window classifies are one constant.
pub const PASS_OFFLINE_EXIT: u8 = 69;

/// THE FALLBACK WALK: a full pass is owed once the machine's last success is this old,
/// whatever the hints say — six hours. Not a day: the next-index probe answers only for the
/// shipped public index (a repointed or private index, an `ATPKG_REGISTRY` directory, a
/// network whose proxy answers the probe with anything but GitHub's own redirect all get
/// `Deferred`), and for those machines this walk is the only way a newly published index
/// is found. It is machine-wide, so N windows and every tab share one walk per interval.
pub const FULL_PASS_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// No lane starts a scheduled full pass within this long of the last full pass ATTEMPTED on
/// the machine by another process ([`Stamps::last_attempt`]), nor while one is installing:
/// the next look sees what that pass left.
pub const PASS_SPACING_SECS: u64 = 5 * 60;

/// The longest a rate-limited listing's reset holds the metered passes: GitHub's window is an
/// hour, so a recorded reset further ahead than this was written by a clock since set back,
/// and is ignored rather than trusted ([`Stamps::metered_hold_in`]).
pub const METERED_HOLD_MAX_SECS: u64 = 60 * 60;

/// How a full `update` pass ended, as the pass records it in `status.toml` (`last_pass`, with
/// `last_pass_at` the moment it ended). The one reading of a pass's outcome the schedulers
/// take ([`Stamps::last_failed`]); `updated_at` is every writer's and proves nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassOutcome {
    /// The pass stamped `last_success_at`: it resolved the signed index and ran to its end (a
    /// member that failed is recorded in its own row) — `last_success_at`'s own meaning. Or
    /// it had nothing to check (no program managed, the set not owed): no success stamp, and
    /// the schedule counts it as one ([`Stamps::last_success`]).
    Ok,
    /// No host answered ([`PASS_OFFLINE_EXIT`]): nothing was checked.
    Offline,
    /// Anything else: the index did not resolve, nothing was installable here, a refusal.
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

    /// The outcome of a pass that exited `code`: [`Self::Ok`] when it stamped a success
    /// (`stamped_success` — whatever the exit, as `last_success_at` reads), else offline or
    /// failed by the exit.
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

/// How long a scheduled pass queues behind another pass at the store lock (`--wait-lock`)
/// before it stands aside with atpkg's contention exit: bounded by the retry, not by the
/// download, and the same for every lane that schedules one.
pub const PASS_WAIT_LOCK_SECS: u64 = 30 * 60;

/// The flags every scheduled pass carries after its verb — ONE argv for the window's
/// passes and the session's: `--wait-lock` ([`PASS_WAIT_LOCK_SECS`]), so a contended pass
/// queues instead of refusing, and `--progress-file` under the store when there is one, so
/// a window can follow any lane's pass and every lane can see a pass in flight.
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
    /// Unix seconds of the last SUCCESSFUL update pass (`last_success_at`), or of a later
    /// full pass that recorded itself [`PassOutcome::Ok`] without one (it had nothing to
    /// check): the schedule's walk counts from either.
    pub last_success: Option<i64>,
    /// Unix seconds of the last full pass ATTEMPTED, failed or not: the end the pass recorded
    /// (`last_pass_at`), or the last success when that is later (a success any writer stamped
    /// is an attempt too). Never `updated_at`, which every writer moves.
    pub last_attempt: Option<i64>,
    /// How the last full pass ended, as it recorded itself (`last_pass`); `None` when no pass
    /// recorded one — a record from before the field reads by its success alone.
    pub last_outcome: Option<PassOutcome>,
    /// The index build the last pass resolved (`last_index_build`; `0` for none). A caller
    /// that can read the store's verified floor raises it to that.
    pub last_index_build: u64,
    /// The last full pass's requested or resolved index, bound to its end stamp in
    /// the same status write. `None` for older records or a pass that knew no
    /// index. A newer published hint may bypass sibling spacing only when this
    /// witness belongs to the latest attempt; same-build failures still space.
    pub last_pass_target: Option<(i64, u64)>,
    /// Unix seconds before which the metered GitHub API refuses this machine: the reset a
    /// rate-limited listing named (`metered_hold_until`) — read through
    /// [`Self::metered_hold_in`].
    pub metered_hold_until: Option<i64>,
    /// Whether a pass is installing on the store NOW (its progress file names a live
    /// writer) — never in `status.toml`; the caller that can read the progress file sets
    /// it, and it counts as an attempt this moment.
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
        // A prior binary can preserve the new target fields as unknown `extra`
        // while rewriting the known pass end. Its whole-second stamp cannot
        // match the target-bearing writer's fractional-zero format, including
        // when both passes end in the same second.
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
            metered_hold_until: stamp("metered_hold_until"),
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

    /// The latest completed full pass ATTEMPTED an older index. Only that
    /// evidence lets a strictly newer published hint bypass sibling spacing:
    /// an in-flight pass, current rate-limit hold, older schema, or target not
    /// bound to the latest attempt keeps the conservative wait. A failure at
    /// the same build is deduplicated; a new build may be tried after an old
    /// one failed if GitHub is not currently holding its metered listing.
    #[must_use]
    pub fn completed_older_index_pass(&self, published: u64, now_unix: i64) -> bool {
        !self.in_flight
            && self.metered_hold_in(now_unix) == 0
            && self.last_outcome.is_some()
            && self.last_pass_target.is_some_and(|(ended, attempted)| {
                self.last_attempt == Some(ended) && published > attempted
            })
    }

    /// Whether the machine's last full pass FAILED: it recorded an outcome other than
    /// [`PassOutcome::Ok`], and no success was stamped since. What the pass recorded, never
    /// an order of stamps: a write after the last success is no failed pass.
    #[must_use]
    pub fn last_failed(&self) -> bool {
        match (self.last_outcome, self.last_attempt) {
            (Some(outcome), Some(attempt)) => {
                outcome != PassOutcome::Ok && self.last_success.is_none_or(|ok| ok < attempt)
            }
            _ => false,
        }
    }

    /// Seconds left at `now_unix` of the hold a rate-limited listing recorded: `0` when there
    /// is none, when it has passed, or when it lies further ahead than
    /// [`METERED_HOLD_MAX_SECS`] (a clock since set back wrote it). The window's scheduled
    /// full passes wait it out, and park the next-index probe through it: the probe's
    /// Releases listing is metered too, and only its own hour-long cooldown after a refusal
    /// would stop it. The vendor head watch rides the vendors' hosts and is not held.
    #[must_use]
    pub fn metered_hold_in(&self, now_unix: i64) -> u64 {
        self.metered_hold_until
            .and_then(|until| u64::try_from(until.saturating_sub(now_unix)).ok())
            .filter(|&left| left <= METERED_HOLD_MAX_SECS)
            .unwrap_or(0)
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

/// THE MACHINE-WIDE RULE: the full pass owed at `now_unix` — none ever succeeded, or the
/// last success is [`FULL_PASS_INTERVAL_SECS`] old — or `None`, which is also the answer
/// while a pass is installing or was attempted within [`PASS_SPACING_SECS`] (the next look
/// sees what it left), and within the interval of one that failed. Pure.
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

/// Seconds from `now_unix` until [`full_pass_owed`] owes a pass, `0` when it owes one now:
/// the LATEST of what holds it back — a pass installing (look again after the spacing), the
/// spacing after the last attempt, the interval after a failed attempt, the interval after
/// the last success. Each is bounded by its own period, so a stamp a clock set back wrote
/// never holds a lane longer than that. No rate-limit hold: one is recorded only with a pass
/// end, and every pass end holds this rule an interval — longer than any hold
/// ([`METERED_HOLD_MAX_SECS`]). Pure: what a lane parks for when nothing is owed yet.
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

/// Whether `every_secs` have passed since the stamp `then` at `now_unix`. A stamp more
/// than a period in the FUTURE counts as passed: a clock since set back wrote it, and one
/// run rewrites it — trusting it would stall the lane until that clock came round again.
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
/// `now_unix`: `true`, with the stamp moved to now, when the last claim is that old, absent
/// or unreadable; `false` while it is younger, or while another process holds the claim.
/// Serialized across processes by an advisory lock on the sibling `<stamp>.lock`, held for
/// the read and the write only. A stamp that cannot be locked (no directory, a read-only
/// prefix) answers `true`: the lane runs as it would with no stamp at all.
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

/// The `last_success_at` value of a `status.toml` text, or `None` when the key is
/// absent or empty. A TOML parse failure reads as absent: a record this reader
/// cannot parse is a record that proves no success.
#[must_use]
pub fn last_success_at(status_toml: &str) -> Option<String> {
    let value: aterm_toml::Value = status_toml.parse().ok()?;
    value
        .get("last_success_at")
        .and_then(aterm_toml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Whether `status_toml_path` proves that NO atpkg pass has ever succeeded: the
/// file is absent, unreadable, unparseable, or carries no `last_success_at`.
#[must_use]
pub fn never_checked(status_toml_path: &Path) -> bool {
    match std::fs::read_to_string(status_toml_path) {
        Ok(text) => last_success_at(&text).is_none(),
        Err(_) => true,
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` (any trailing fraction/offset ignored) → unix seconds.
/// The same strict shape the roster's date parser reads; a stamp that does not fit
/// it is `None`.
#[must_use]
pub fn rfc3339_to_unix(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let days = aterm_types::rfc3339::days_from_civil(y, mo, d);
    Some(days * 86400 + h * 3600 + mi * 60 + se)
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

    /// The contract: absent file, no key, empty key ⇒ never checked; a stamped
    /// key ⇒ checked. `updated_at` alone proves nothing — it is a last-WRITE stamp,
    /// moved by failed passes and row rewrites alike (2026-09-10 audit).
    #[test]
    fn never_checked_reads_only_last_success_at() {
        let p = scratch("never");
        assert!(never_checked(&p), "absent file");
        std::fs::write(
            &p,
            "schema = 1\nupdated_at = \"2026-09-10T06:40:53Z\"\noutcome = \"up to date\"\n",
        )
        .unwrap();
        assert!(
            never_checked(&p),
            "updated_at is a last-write stamp, not a success"
        );
        std::fs::write(&p, "schema = 1\nlast_success_at = \"\"\n").unwrap();
        assert!(never_checked(&p), "an empty stamp is no stamp");
        std::fs::write(&p, "not = [valid toml").unwrap();
        assert!(never_checked(&p), "unparseable proves no success");
        std::fs::write(
            &p,
            "schema = 1\nupdated_at = \"2026-09-10T07:00:00Z\"\nlast_success_at = \"2026-09-10T06:40:53Z\"\n",
        )
        .unwrap();
        assert!(!never_checked(&p));
        assert_eq!(
            last_success_at(&std::fs::read_to_string(&p).unwrap()).as_deref(),
            Some("2026-09-10T06:40:53Z")
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// The stamps a scheduler reads: the success, the attempt (the end the last full pass
    /// recorded, or the success when that is later), the pass's own outcome, the index build
    /// and the metered hold — absent, empty and unparseable all read as none. `updated_at`
    /// is read by nothing here: until 2026-09-23 it was the attempt, and any write after the
    /// last success (a vendor head-watch pass, a typed verb) read as a failed pass.
    #[test]
    fn the_stamps_are_read_off_the_record() {
        let p = scratch("stamps");
        assert_eq!(Stamps::read(&p), Stamps::default(), "absent file");
        std::fs::write(
            &p,
            "schema = 1\nupdated_at = \"2026-09-10T09:00:00Z\"\n\
             last_success_at = \"2026-09-10T06:40:53Z\"\nlast_pass = \"failed\"\n\
             last_pass_at = \"2026-09-10T08:34:12Z\"\nlast_index_build = 43\n\
             metered_hold_until = \"2026-09-10T09:30:00Z\"\n",
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
                metered_hold_until: rfc3339_to_unix("2026-09-10T09:30:00Z"),
                in_flight: false,
            }
        );
        // Only a success stamp (a record from before `last_pass`): it is the attempt, and no
        // outcome is recorded — the write after it is not a failed pass.
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
        // A success stamped after the recorded pass (another writer's) is the later attempt.
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

    /// THE PASS'S OWN VERDICT: a pass that stamped a success is `ok` whatever it exited (a
    /// member that failed, a listing refused while the cache stood in), one that reached no
    /// host is `offline`, anything else `failed` — and each word reads back as itself.
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

    /// THE SESSION LANE'S MISREAD, FIXED (§3.3 of the 2026-09-22 design): a success seven
    /// hours old and a vendor head-watch write a minute ago. Read by the pass's recorded
    /// outcome the walk is owed now; the order of the stamps said "attempted after the last
    /// success", a failed pass, and held the session's pass until six hours after the WRITE.
    /// The negative control: the same record whose last pass did fail holds the interval.
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

    /// THE RATE-LIMIT HOLD (§3.2 of the 2026-09-22 design) is read off the record for the
    /// window's gate ([`Stamps::metered_hold_in`]), on the injected clock — never longer than
    /// [`METERED_HOLD_MAX_SECS`], so a reset a clock since set back recorded is ignored. The
    /// machine-wide rule reads none, and needs none: every record a pass writes with a hold
    /// — `ok` with the cache standing in, `failed`, `offline` keeping it — holds the rule an
    /// interval from that pass's end, past the reset.
    #[test]
    fn a_rate_limit_reset_is_read_for_the_window_and_outlasted_by_the_rule() {
        let now = 1_790_000_000_i64;
        let held = |until: i64| Stamps {
            metered_hold_until: Some(until),
            ..Stamps::default()
        };
        assert_eq!(held(now + 1200).metered_hold_in(now), 1200);
        assert_eq!(held(now - 1).metered_hold_in(now), 0, "passed");
        assert_eq!(
            held(now + 3601).metered_hold_in(now),
            0,
            "beyond an hour: ignored"
        );
        assert_eq!(held(now + 3600).metered_hold_in(now), METERED_HOLD_MAX_SECS);
        assert_eq!(Stamps::default().metered_hold_in(now), 0);
        let at = |unix: i64| aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs());
        let reset = now + 3600;
        for outcome in ["ok", "failed", "offline"] {
            // As `stamp_pass_end` writes it: the end is now, the hold beside it; a pass that
            // ended ok stamped its success in the same moment.
            let success = if outcome == "ok" { now } else { now - 86_400 };
            let stamps = Stamps::parse(&format!(
                "last_success_at = \"{}\"\nlast_pass = \"{outcome}\"\nlast_pass_at = \"{}\"\n\
                 metered_hold_until = \"{}\"\n",
                at(success),
                at(now),
                at(reset),
            ));
            assert_eq!(stamps.metered_hold_in(now), 3600, "{outcome}");
            assert_eq!(
                full_pass_due_in(&stamps, now),
                FULL_PASS_INTERVAL_SECS,
                "{outcome}: an interval from the pass's end"
            );
            assert_eq!(
                full_pass_owed(&stamps, reset),
                None,
                "{outcome}: not at the reset"
            );
        }
    }

    /// A FULL PASS WITH NOTHING TO CHECK (no program managed, the set not owed) records `ok`
    /// and stamps no success, and the walk counts from it all the same: the session lane
    /// used to read the record's last write as a failed attempt and wait the interval, and
    /// a pass that recorded nothing would be owed at every tab. A success LATER than such a
    /// pass is the walk's; a failed pass end is no success.
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

    /// THE MACHINE-WIDE RULE, which the session lane spawns on and the window's loop runs
    /// its fallback walk on: never succeeded ⇒ owed; a success six hours old ⇒ owed; a
    /// younger one ⇒ not, and [`full_pass_due_in`] names the rest of the interval — and
    /// nothing is owed while a pass is installing or within the spacing of the last attempt
    /// anywhere on the machine, so two processes never run the same pass back to back.
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
        // A clock set back: a stamp within a period ahead reads as fresh (and holds for at
        // most a period), one further ahead is a record from a clock no longer trusted.
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

    /// A PASS THAT KEEPS FAILING IS NOT RESPAWNED BY EVERY TAB (the 2026-09-10 rule, kept
    /// through Phase 3): once the last pass recorded a failure after the last success — or no
    /// pass ever succeeded — nothing is owed for the interval after that pass, as the session
    /// lane's old attempt-age rule answered: a record whose passes never succeed (an unserved
    /// triple exits 2 without a success) costs one pass per interval, not one per tab. The
    /// spacing alone would have respawned it from every tab opened five minutes on.
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
        // The walk after a failed one: the success is a day old, the last attempt (it
        // failed) an hour ago.
        let walk_failed = at(Some(now - 4 * interval), Some(now - 3600));
        assert!(walk_failed.last_failed());
        assert_eq!(full_pass_owed(&walk_failed, now), None);
        assert_eq!(
            full_pass_owed(&walk_failed, now + interval - 3600),
            Some(Owed::Interval)
        );
        // The negative control: a success six hours old whose attempt IS that success.
        let walk_ok = at(Some(now - interval), Some(now - interval));
        assert!(!walk_ok.last_failed());
        assert_eq!(full_pass_owed(&walk_ok, now), Some(Owed::Interval));
        assert!(!Stamps::default().last_failed(), "never attempted");
    }

    /// One argv for every lane that schedules a pass: the lock wait, and the progress
    /// file where there is a store.
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

    /// A lane's slot: the first claim wins and stamps; a second inside the period is
    /// refused; one a period later wins again; a garbage or far-future stamp is claimable;
    /// and a stamp with no directory to live in claims nothing and answers `true` — the
    /// lane runs as it did before it had a stamp.
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

    /// A sibling holding the claim lock makes the claim answer `false` — it is claiming
    /// this very slot — and never blocks past the claim's short wait.
    #[test]
    fn a_claim_held_elsewhere_is_not_taken_twice() {
        let dir = scratch("claim-held");
        let stamp = dir.parent().unwrap().join("session-pass.stamp");
        let held = crate::FileLock::acquire(&stamp.with_extension("lock")).unwrap();
        let started = std::time::Instant::now();
        assert!(!claim(&stamp, 1_790_000_000, PASS_SPACING_SECS));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        drop(held);
        assert!(claim(&stamp, 1_790_000_000, PASS_SPACING_SECS));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// The strict date shape. (Until Phase 2, 2026-09-22, this test also pinned the
    /// never-checked stderr line every console edge printed; no edge prints it now, and
    /// the constant is gone.)
    #[test]
    fn the_stamp_parser_is_pinned() {
        assert_eq!(rfc3339_to_unix("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(
            rfc3339_to_unix("2026-09-10T06:40:53.123Z"),
            rfc3339_to_unix("2026-09-10T06:40:53Z")
        );
        for bad in [
            "",
            "2026-09-10",
            "2026/09/10T06:40:53Z",
            "2026-13-10T06:40:53Z",
        ] {
            assert_eq!(rfc3339_to_unix(bad), None, "{bad:?}");
        }
    }
}
