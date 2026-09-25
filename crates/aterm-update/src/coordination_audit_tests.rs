// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Multi-process coordination audit (2026-09-14): laws the shared `Updates/`
//! ledgers must obey when the window, N terminal-session checkers and a one-shot
//! `aterm update check` all read and write them. Every test here was RED against
//! the tree it was written on; the wrong behaviour each pins is named on the test.

use std::path::PathBuf;
use std::time::Duration;

use crate::checker_skip;
use crate::health::Health;
use crate::paths::Staging;

/// The check's one base interval — what every process is deduped against.
const BASE: Duration = Duration::from_secs(crate::cadence::INTERVAL_SECS);

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// A ledger exactly as the CHECK lane leaves it, `age_secs` ago.
fn write_check_ledger(s: &Staging, age_secs: u64, outcome: &str) {
    let stamp = aterm_types::rfc3339::format_rfc3339(unix_now().saturating_sub(age_secs));
    std::fs::write(
        crate::check_receipt::path(s),
        format!(
            "schema = 1
current_build = 42
source = \"fixture/channel\"
updated_at = \"{stamp}\"
checked_at = \"{stamp}\"
outcome = \"{outcome}\"
"
        ),
    )
    .expect("write ledger");
}

/// FINDING: an apply-lane write to `status.toml` reads as "another aterm process
/// completed this interval's update check".
///
/// `checker_skip_at` keys the machine-wide dedup window on `updated_at`, and
/// `updated_at` is stamped by EVERY writer of the one-line ledger — the apply
/// lane included (`record_apply_failure`, `record_apply_refusal`, and the boot
/// swap's `record_activating_status`). On the owner's machine (2026-09-14
/// `aterm.log`): the successful apply at 09:28:16 wrote the ledger, and every
/// process's checker then skipped ("another aterm process completed …") until
/// 09:49:17 — exactly 70 % of the 30-minute web interval later — although no
/// check had run since 09:23:54. The night before, three apply failures
/// (22:42, 22:52, 23:22) re-stamped it in turn and no check ran between 22:42:49
/// and 00:12:34 on a 30-minute cadence. An apply outcome is not a check: it must
/// neither spend nor renew the interval's network budget.
#[test]
fn an_apply_lane_ledger_write_is_not_a_completed_check() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::status::clear_check_note();
    let s = Staging::scratch("coord-apply-stamp");
    // The last CHECK completed 80 % of an interval ago — past the 70 % freshness
    // window — so this cycle owes the channel a check.
    write_check_ledger(
        &s,
        BASE.as_secs() * 8 / 10,
        "up to date (channel head v0.85.0)",
    );
    assert!(
        checker_skip(&s, BASE).is_none(),
        "precondition: a check stamp past the window does not defer"
    );

    // The apply lane records a failed attempt NOW — byte-for-byte the record
    // `crate::record_apply_failure` writes.
    crate::status::record(
        &s,
        1_789_276_245,
        "staged build did not apply: the staged update failed verification; the \
         terminal was left untouched",
    );
    assert!(
        checker_skip(&s, BASE).is_none(),
        "an apply FAILURE is not a completed check: the sibling checkers must not \
         hold off the channel for another window because of it"
    );

    // …and the boot swap's own post-apply record (`install::record_activating_status`),
    // written by the successor before it execs: the same rule.
    crate::status::record(
        &s,
        1_789_363_090,
        "installed 0.85.0 (build 1789363090); activating now",
    );
    assert!(
        checker_skip(&s, BASE).is_none(),
        "a successful apply is not a completed check either: the freshly execed \
         build's first check must not be deferred by its own activation record"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// FINDING: a check stamp AHEAD of the clock defers every checker on the machine
/// until the wall clock catches up.
///
/// `rfc3339_older_than` answers `false` for any future stamp, so a ledger written by a fast clock — a
/// VM restored from a snapshot, an RTC corrected by NTP after boot — is "fresh"
/// until real time passes it, and because every loop skips, nothing overwrites it.
#[test]
fn a_stamp_ahead_of_the_clock_does_not_defer_the_check() {
    let s = Staging::scratch("coord-future-stamp");
    let stamp = aterm_types::rfc3339::format_rfc3339(unix_now() + 6 * 3600);
    std::fs::write(
        crate::check_receipt::path(&s),
        format!(
            "schema = 1
current_build = 42
source = \"fixture/channel\"
updated_at = \"{stamp}\"
outcome = \"up to date\"
"
        ),
    )
    .expect("write ledger");
    assert!(
        checker_skip(&s, BASE).is_none(),
        "a stamp six hours in the future is a clock fault, not a fresh check — \
         honouring it parks every checker on this machine for six hours"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

fn health_path(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("aterm-coord-health-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("health.toml")
}

/// FINDING: a sibling process still running an OLDER build expires the apply
/// streak the window recorded on the build actually installed.
///
/// `expire_stale_apply_streak` runs at the top of EVERY process's check
/// (`check_and_stage_inner`), and zeroes the streak whenever the caller's build
/// differs from `last_apply_failure_build`. Its premise — "the machine moved" —
/// holds for a process on a NEWER build. It does not hold for a straggler: an
/// `aterm` session launched before a seamless self-update keeps running the old
/// image for days (`bundle::resolve` still names `/Applications/aterm.app`, so its
/// checker loop runs), and each of its checks erases the window's live streak.
/// `failing_applies` then never reaches `PERSISTENT_AFTER`, the "auto-update is
/// failing" notice never fires, `aterm-ctl update status` prints
/// `failing_applies=0` beside a lane that fails every attempt, and
/// `apply_failures_for_target` — the count the handoff's freeze budget reads —
/// restarts from zero.
#[test]
fn an_older_sibling_process_cannot_expire_the_apply_streak_a_newer_build_recorded() {
    let p = health_path("straggler");
    // The window, running the installed build 200, fails to reach staged 300 twice.
    Health::record_apply_failure(&p, 200, 300, "overlap handoff failed safely: ChildDied");
    let h = Health::record_apply_failure(&p, 200, 300, "overlap handoff failed safely: ChildDied");
    assert_eq!(h.apply_failures, 2, "precondition: two attempts recorded");
    assert_eq!(h.apply_failures_for_target, 2);

    // A session process still on build 100 (launched before the window moved to
    // 200) runs its half-hourly check.
    let h = Health::expire_stale_apply_streak(&p, 100);
    assert_eq!(
        h.apply_failures, 2,
        "a process on an OLDER build than the recorder proves nothing about the \
         machine having moved — the window's live streak must survive its check"
    );
    assert_eq!(
        h.apply_failures_for_target, 2,
        "the per-artifact attempt count survives too (the freeze budget reads it)"
    );
    assert_eq!(
        Health::read(&p).apply_failures,
        2,
        "and it survives on disk"
    );

    // A NEWER build still proves the move: the documented expiry is unchanged.
    let h = Health::expire_stale_apply_streak(&p, 250);
    assert_eq!(
        h.apply_failures, 0,
        "a newer running build expires the streak"
    );
    let _ = std::fs::remove_dir_all(p.parent().expect("dir"));
}
