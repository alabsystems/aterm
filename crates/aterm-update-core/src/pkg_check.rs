// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Has the TOOLCHAIN package manager (atpkg) ever completed an update check on this
//! machine — read off its `status.toml` alone, so a CLI edge that must not link
//! atpkg's internals (the `aterm` session lane, the window's headless path) can say
//! so on stderr before the first pass has run (R3, 2026-09-10).
//!
//! The contract is deliberately tiny and shared: atpkg stamps
//! `last_success_at = "<RFC3339>"` into `<prefix>/status.toml` ONLY on a successful
//! pass (every other write moves `updated_at`, which is a last-write stamp and says
//! nothing about success). "Never checked" is therefore: the file is absent, or it
//! carries no `last_success_at`, or that value is empty. One reader here, every edge
//! prints [`NEVER_CHECKED_STDERR_LINE`] verbatim, and a driver can grep for it.
//!
//! The SPAWN decision reads the other stamp on purpose (2026-09-10 review): a session
//! launch spawns its one-shot pass when the last ATTEMPT (`updated_at`) is older than
//! the interval — not the last success — so a machine whose pass persistently fails
//! runs one pass per interval, not one per terminal tab. `last_success_at` stays the
//! input of the stderr line: the words say whether a pass ever completed.

use std::path::Path;

/// THE line every console edge prints when no atpkg pass has ever succeeded here.
/// Exactly this text, on stderr, once per launch — the session lane
/// (`crates/aterm/src/main.rs`) and the window's no-loop path both use it.
pub const NEVER_CHECKED_STDERR_LINE: &str = "atpkg: no update check has run yet on this machine \u{2014} packages cannot be updated until the first pass completes (run: aterm pkg update)";

/// The cadence the window's update loop parks on between passes, and the age past
/// which a SESSION launch spawns a one-shot pass of its own (R4): six hours, or
/// `ATPKG_UPDATE_INTERVAL_SECS` when set (`0` = once per window launch, never from
/// a session — a once-pass that timed out queued behind another aterm's install is
/// retried on the window's short backoff until it actually runs, or given up on for
/// that launch once the holder looks wedged: three waits with nothing moving in its
/// progress file, 2026-09-10). One reader for both edges so they cannot drift.
#[must_use]
pub fn update_interval_secs() -> u64 {
    std::env::var("ATPKG_UPDATE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(6 * 60 * 60)
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

/// Seconds since the last SUCCESSFUL pass recorded in `status_toml_path`, at
/// `now_unix` — `None` when there has never been one (or the stamp does not parse).
/// A stamp in the future reads as age 0 (a clock that moved backwards must not
/// spawn a pass every launch).
#[must_use]
pub fn last_success_age_secs(status_toml_path: &Path, now_unix: i64) -> Option<u64> {
    let text = std::fs::read_to_string(status_toml_path).ok()?;
    let stamp = last_success_at(&text)?;
    age_of(&stamp, now_unix)
}

/// The `updated_at` value of a `status.toml` text — atpkg's last-WRITE stamp, moved
/// by every pass whether it succeeded or failed — or `None` when the key is absent
/// or empty. A record without one falls back to `last_success_at`: a ledger that
/// only ever stamped its success still names the one attempt it knows.
#[must_use]
pub fn last_attempt_at(status_toml: &str) -> Option<String> {
    let value: aterm_toml::Value = status_toml.parse().ok()?;
    value
        .get("updated_at")
        .and_then(aterm_toml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| last_success_at(status_toml))
}

/// Seconds since the last ATTEMPTED pass recorded in `status_toml_path` (its
/// `updated_at`), at `now_unix` — `None` when no pass has ever run (or the stamp
/// does not parse). THE input of the session-lane spawn decision
/// ([`session_pass_due`]): a persistently failing pass moves this stamp on every
/// attempt, so the lane spawns one pass per interval and never one per launch.
/// A stamp in the future reads as age 0, as for the success age.
#[must_use]
pub fn last_attempt_age_secs(status_toml_path: &Path, now_unix: i64) -> Option<u64> {
    let text = std::fs::read_to_string(status_toml_path).ok()?;
    let stamp = last_attempt_at(&text)?;
    age_of(&stamp, now_unix)
}

fn age_of(stamp: &str, now_unix: i64) -> Option<u64> {
    let then = rfc3339_to_unix(stamp)?;
    Some(u64::try_from(now_unix.saturating_sub(then)).unwrap_or(0))
}

/// Whether a SESSION launch should spawn a one-shot `atpkg update` of its own
/// (R4): never when the interval is `0` (the "once per window launch" setting),
/// otherwise when no pass has ever been ATTEMPTED or the last attempt is older
/// than the interval — `age_secs` is [`last_attempt_age_secs`], so a pass that
/// keeps failing is retried once per interval, not once per tab. Pure, so the
/// decision is testable without a file.
#[must_use]
pub fn session_pass_due(age_secs: Option<u64>, interval_secs: u64) -> bool {
    if interval_secs == 0 {
        return false;
    }
    age_secs.is_none_or(|age| age >= interval_secs)
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

    /// The age is measured from the SUCCESS stamp; a future stamp reads as fresh;
    /// the session-pass decision follows the interval, and `0` disarms it.
    #[test]
    fn the_session_pass_is_due_by_age_against_the_interval() {
        let p = scratch("age");
        assert_eq!(last_success_age_secs(&p, 1_789_022_453), None);
        std::fs::write(&p, "last_success_at = \"2026-09-10T06:40:53Z\"\n").unwrap();
        let stamp = rfc3339_to_unix("2026-09-10T06:40:53Z").unwrap();
        assert_eq!(last_success_age_secs(&p, stamp + 7200), Some(7200));
        assert_eq!(
            last_success_age_secs(&p, stamp - 100),
            Some(0),
            "a clock that moved backwards is not a stale pass"
        );
        let six_hours = 6 * 60 * 60;
        assert!(session_pass_due(None, six_hours), "never checked ⇒ due");
        assert!(!session_pass_due(Some(7200), six_hours));
        assert!(session_pass_due(Some(six_hours), six_hours));
        assert!(
            !session_pass_due(None, 0),
            "interval 0 means once per window launch, never from a session"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// THE SPAWN READS THE ATTEMPT, THE WORDS READ THE SUCCESS (2026-09-10
    /// review): a ledger whose pass keeps failing moves `updated_at` on every
    /// attempt and never stamps `last_success_at`. The stderr line stays (never
    /// succeeded), but a fresh attempt makes the session pass NOT due — one pass
    /// per interval, not one per terminal tab. A ledger with only a success stamp
    /// counts it as the attempt; an absent ledger is due.
    #[test]
    fn the_spawn_decision_reads_the_last_attempt_not_the_last_success() {
        let p = scratch("attempt");
        let six_hours: u64 = 6 * 60 * 60;
        let stamp = rfc3339_to_unix("2026-09-10T08:34:12Z").unwrap();
        let later = |secs: u64| stamp + i64::try_from(secs).unwrap();
        assert_eq!(last_attempt_age_secs(&p, stamp), None, "absent ledger");
        assert!(session_pass_due(
            last_attempt_age_secs(&p, stamp),
            six_hours
        ));
        // A failing pass, ten minutes ago: never succeeded, but recently attempted.
        std::fs::write(
            &p,
            "schema = 1\nupdated_at = \"2026-09-10T08:34:12Z\"\noutcome = \"index fetch failed\"\n",
        )
        .unwrap();
        assert!(never_checked(&p), "the words still say: never succeeded");
        assert_eq!(last_success_age_secs(&p, stamp + 600), None);
        assert_eq!(last_attempt_age_secs(&p, stamp + 600), Some(600));
        assert!(
            !session_pass_due(last_attempt_age_secs(&p, stamp + 600), six_hours),
            "a pass that just failed is not respawned by the next tab"
        );
        assert!(
            session_pass_due(last_attempt_age_secs(&p, later(six_hours)), six_hours),
            "…but it is retried once the interval has passed"
        );
        // Only a success stamp: it is the attempt too.
        std::fs::write(&p, "last_success_at = \"2026-09-10T08:34:12Z\"\n").unwrap();
        assert_eq!(
            last_attempt_at(&std::fs::read_to_string(&p).unwrap()).as_deref(),
            Some("2026-09-10T08:34:12Z")
        );
        assert_eq!(last_attempt_age_secs(&p, stamp + 5), Some(5));
        // An empty updated_at is no attempt; the success stamp answers.
        std::fs::write(
            &p,
            "updated_at = \"\"\nlast_success_at = \"2026-09-10T08:34:12Z\"\n",
        )
        .unwrap();
        assert_eq!(last_attempt_age_secs(&p, stamp + 5), Some(5));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// The strict date shape, and the exact stderr line every edge prints.
    #[test]
    fn the_stamp_parser_and_the_stderr_line_are_pinned() {
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
        assert_eq!(
            NEVER_CHECKED_STDERR_LINE,
            "atpkg: no update check has run yet on this machine \u{2014} packages cannot be \
             updated until the first pass completes (run: aterm pkg update)"
        );
        assert!(!NEVER_CHECKED_STDERR_LINE.contains('\n'));
    }
}
