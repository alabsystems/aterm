// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Multi-process coordination audit (2026-09-14): laws the shared `Updates/`
//! ledgers must obey when the window, N terminal-session checkers and a one-shot
//! `aterm update check` all read and write them. Every test here was RED against
//! the tree it was written on; the wrong behaviour each pins is named on the test.

use std::path::PathBuf;

use crate::health::Health;

// The two ledger laws this audit also found — an apply-lane status write is not a
// completed check, and a stamp ahead of the clock defers nothing — are pinned against
// the check receipt in `check_channel_audit_tests.rs`.

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
