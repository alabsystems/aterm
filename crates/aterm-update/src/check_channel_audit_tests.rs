// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CHECK-CHANNEL AUDIT, 2026-09-14 — failing tests for the cross-process checker
//! gate (`crate::checker_skip_at`, the "another aterm process completed this
//! interval's update check" skip).
//!
//! Every test here names a wrong behaviour observed on the owner's machine that day
//! and fails on the tree it was written against. They pin the CONTRACT the gate
//! promises in its own doc — "whether the shared ledger records a CHECK completed
//! within the freshness window" — against what it actually reads, which is
//! `status.toml`'s `updated_at`: a stamp every writer of that one-line file
//! refreshes, the apply lane included.
//!
//! Evidence (~/Library/Logs/aterm/aterm.log, epoch seconds): the in-session apply
//! at 1789403296 wrote `installed 0.85.0 (build 1789363090); activating now`; the
//! freshly exec'd process then logged "another aterm process completed this
//! interval's update check" seventeen times, ~75 s apart, from 1789403300 to
//! 1789404476, and made its first real check at 1789404557 — exactly 21 min (70 %
//! of the 30-min web base) after a status write that no check lane made.

use std::time::Duration;

use crate::checker_skip_at;
use crate::paths::Staging;

/// The check's one base interval.
const BASE: Duration = Duration::from_secs(crate::cadence::INTERVAL_SECS);

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// A completed-check receipt at `updated_at`, carrying the check result.
/// Apply writers cannot modify this file; the fixture tests result classification.
fn write_stamped(s: &Staging, updated_at: u64, outcome: &str) {
    let stamp = aterm_types::rfc3339::format_rfc3339(updated_at);
    std::fs::write(
        crate::check_receipt::path(s),
        format!(
            "schema = 1\ncurrent_build = 42\nsource = \"fixture/channel\"\nupdated_at = \"{stamp}\"\nchecked_at = \"{stamp}\"\noutcome = \
             \"{outcome}\"\n"
        ),
    )
    .expect("write ledger");
}

/// THE 21-MINUTE POST-APPLY STALL. Every sentence below is one the APPLY lane
/// writes into `status.toml` (`install.rs` `record_activating_status`,
/// `apply_staged_if_ready`'s refusal/deferral arms; `lib.rs`
/// `record_apply_refusal` / `record_apply_failure`), through the real writer
/// (`status::record`), so the stamp is exactly what a sibling reads. None of them
/// is a completed check, so none may make a sibling — or the writer's own
/// successor process — skip this interval's network check.
///
/// Why it matters beyond the wasted 21 minutes: the apply lane retries a failing
/// stage on its own clock (600 s, 1800 s stand-downs on 2026-09-14), and each
/// verdict rewrites this file. While those writes land inside the freshness
/// window, EVERY checker on the machine skips, so the newer release that would fix
/// the failing apply is never fetched — the lane that is broken silences the lane
/// that could route around it.
#[test]
fn check_channel_audit_an_apply_lane_status_write_is_not_a_completed_check() {
    let s = Staging::scratch("audit-apply-write");
    let now = now_secs();
    for outcome in [
        "installed 0.85.0 (build 1789363090); activating now",
        "boot apply refused: the terminal is busy",
        "staged 0.85.0 (build 1789363090) — NOT applied: pre-park verification refused",
        "staged build did not apply: PreparationFailed",
        "reverted crash-looping update to the previous build",
    ] {
        crate::status::record(&s, 1789276245, outcome);
        assert_eq!(
            checker_skip_at(&s, BASE, now),
            None,
            "{outcome:?}: the apply lane completed no check, so its stamp must not make \
             a sibling skip this interval's check"
        );
    }
    // The positive control, and the contract a fix has to keep: a stamp the CHECK
    // lane wrote inside the window still skips. The completed-check receipt
    // owns its timestamp independently of the any-writer status record.
    let stamp = aterm_types::rfc3339::format_rfc3339(now - 60);
    std::fs::write(
        crate::check_receipt::path(&s),
        format!(
            "schema = 1\ncurrent_build = 42\nsource = \"fixture/channel\"\nupdated_at = \"{stamp}\"\nchecked_at = \"{stamp}\"\noutcome = \"up to \
             date (channel head v0.85.0)\"\n"
        ),
    )
    .expect("write ledger");
    assert!(
        checker_skip_at(&s, BASE, now).is_some(),
        "a check completed a minute ago still dedups the siblings"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// THE WIDENED WINDOW BELONGS TO THE HOST'S BACKOFF, NOT THE APPLY LANE'S. The gate
/// doubles the freshness window when the check receipt records a deferral, which is
/// how a machine that was just told to slow down keeps every sibling off the host.
/// But `install.rs` writes `deferred: install location not writable` / `deferred:
/// staged bundle failed re-verification (discarded)` / `deferred: staged bundle
/// build-number rebind mismatch (discarded)` for APPLY deferrals that never touched
/// the network: an admin-owned `/Applications` cost every launch a 42-minute check
/// holiday. The widening keys on the check receipt's own `outcome = "deferred"`.
#[test]
fn check_channel_audit_an_apply_lane_deferral_does_not_widen_the_check_window() {
    let s = Staging::scratch("audit-apply-deferral");
    let now = now_secs();
    for outcome in [
        "deferred: install location not writable",
        "deferred: staged bundle failed re-verification (discarded)",
        "deferred: staged bundle build-number rebind mismatch (discarded)",
    ] {
        // 80 % of an interval: past the base window (70 %), inside the widened one
        // (70 % of two intervals).
        write_stamped(&s, now - BASE.as_secs() * 8 / 10, outcome);
        assert_eq!(
            checker_skip_at(&s, BASE, now),
            None,
            "{outcome:?}: an apply-lane deferral is not a GitHub backoff and must not \
             hold the machine off its channel for a second interval"
        );
    }
    // The check lane's own deferral still widens, exactly as before.
    write_stamped(&s, now - BASE.as_secs() * 8 / 10, "deferred");
    assert!(
        checker_skip_at(&s, BASE, now).is_some_and(|(reason, _)| reason.contains("deferred")),
        "a recorded host deferral keeps the machine-wide retreat"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A STAMP FROM THE FUTURE HOLDS EVERY CHECKER UNTIL THE CLOCK CATCHES UP.
/// `rfc3339_older_than` answers
/// "not older" for any stamp ahead of now, so after a clock step backwards (a wrong
/// clock corrected by NTP, a VM restored from a snapshot) every background loop on
/// the machine skips — logging "another aterm process completed this interval's
/// update check" — until wall time passes the stamp plus the window, and nothing
/// overwrites the stamp because every loop skipped. A stamp no honest writer could
/// have produced at this instant is not evidence of a completed check.
#[test]
fn check_channel_audit_a_ledger_stamped_in_the_future_does_not_hold_every_checker() {
    let s = Staging::scratch("audit-future-stamp");
    let now = now_secs();
    write_stamped(&s, now + 2 * 3600, "up to date (channel head v0.85.0)");
    assert_eq!(
        checker_skip_at(&s, BASE, now),
        None,
        "a stamp two hours ahead of now is not a check that completed inside the window"
    );
    // Sanity: a stamp a minute in the past is the ordinary fresh skip.
    write_stamped(&s, now - 60, "up to date (channel head v0.85.0)");
    assert!(checker_skip_at(&s, BASE, now).is_some());
    let _ = std::fs::remove_dir_all(&s.root);
}
