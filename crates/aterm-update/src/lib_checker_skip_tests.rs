// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use super::{SKIP_JITTER_SECS, checker_skip, checker_skip_at, paths::Staging, skip_timer_target};

fn staging(name: &str) -> crate::paths::ScratchStaging {
    Staging::scratch(&format!("checker-{name}"))
}

fn now() -> u64 {
    aterm_time::unix_secs().expect("clock")
}

fn write_receipt(s: &Staging, age_secs: u64, outcome: &str) {
    let stamp = aterm_types::rfc3339::format_rfc3339(now().saturating_sub(age_secs));
    std::fs::write(
        super::check_receipt::path(s),
        format!(
            "schema = 1\ncurrent_build = 42\nsource = \"fixture/channel\"\n\
             updated_at = \"{stamp}\"\noutcome = \"{outcome}\"\n"
        ),
    )
    .expect("write receipt");
}

const BASE: Duration = Duration::from_secs(30 * 60);

#[test]
fn a_skipping_process_points_its_timer_at_the_windows_end() {
    let s = staging("window-end");
    write_receipt(&s, 60, "completed");
    let now = now();
    let (_, expiry) = checker_skip_at(&s, BASE, now).expect("a fresh stamp skips");
    let fresh = BASE.as_secs() * 7 / 10;
    assert!(
        ((now - 60 + fresh - 1)..=(now - 60 + fresh + 1)).contains(&expiry),
        "checked_at + 0.7 × base: {expiry} vs now={now}"
    );
    assert_eq!(
        skip_timer_target(expiry, 0),
        expiry,
        "no entropy, no scatter"
    );
    let scattered = skip_timer_target(expiry, 255);
    assert!(
        (expiry..expiry + SKIP_JITTER_SECS).contains(&scattered),
        "the scatter is under a minute: {scattered} vs {expiry}"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_stamp_ahead_of_the_clock_defers_no_check() {
    let s = staging("future");
    write_receipt(&s, 0, "completed");
    assert!(checker_skip_at(&s, BASE, now() - 2 * 3600).is_none());
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_recorded_deferral_holds_off_every_process_not_just_the_one_that_saw_it() {
    let s = staging("deferred");
    write_receipt(&s, 25 * 60, "deferred");
    let reason = checker_skip(&s, BASE).expect("the deferral widens the window");
    assert!(reason.contains("deferred"), "{reason}");
    write_receipt(&s, 50 * 60, "deferred");
    assert!(checker_skip(&s, BASE).is_none(), "the retreat self-heals");
    write_receipt(&s, 25 * 60, "completed");
    assert!(
        checker_skip(&s, BASE).is_none(),
        "a healthy check returns the window to the base interval"
    );
    write_receipt(&s, 25 * 60, "deferred: install location not writable");
    assert!(checker_skip(&s, BASE).is_none());
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn the_deferring_process_wakes_past_its_own_deferred_window() {
    use super::cadence::Cadence;
    let s = staging("own-deferral");
    write_receipt(&s, 0, "deferred");
    let stamped = now();
    let (_, expiry) = checker_skip_at(&s, BASE, stamped).expect("the deferral's window");
    let at = std::time::Instant::now();
    for entropy in [0u8, 128, 255] {
        let until = skip_timer_target(expiry, entropy);
        let window_end = at + Duration::from_secs(until - stamped);
        let mut schedule = Cadence::new(BASE);
        schedule.deferred(Some(window_end), at, entropy);
        let wake = stamped + schedule.delay_at(at, entropy).as_secs();
        assert!(
            checker_skip_at(&s, BASE, wake).is_none(),
            "entropy {entropy}: woke at +{}s, inside its own window",
            wake - stamped
        );
        let mut ladder = Cadence::new(BASE);
        ladder.failed();
        let early = stamped + ladder.delay_at(at, entropy).as_secs();
        assert!(
            checker_skip_at(&s, BASE, early).is_some_and(|(reason, _)| reason.contains("deferred")),
            "entropy {entropy}: the ladder alone woke at +{}s",
            early - stamped
        );
    }
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_deferral_holds_the_machine_off_for_minutes_however_fast_the_base() {
    use super::cadence::{Cadence, INTERVAL_SECS};
    let base = Duration::from_secs(INTERVAL_SECS);
    let s = staging("deferral-floor");
    write_receipt(&s, 13 * 60, "deferred");
    assert!(
        checker_skip(&s, base).is_some_and(|reason| reason.contains("deferred")),
        "13 minutes into the retreat, every process still holds off"
    );
    write_receipt(&s, 15 * 60, "deferred");
    assert!(checker_skip(&s, base).is_none(), "and past it, they check");
    write_receipt(&s, 13 * 60, "completed");
    assert!(checker_skip(&s, base).is_none());

    write_receipt(&s, 0, "deferred");
    let stamped = now();
    let (_, expiry) = checker_skip_at(&s, base, stamped).expect("the deferral's window");
    assert!(expiry >= stamped + 10 * 60, "{}s", expiry - stamped);
    let at = std::time::Instant::now();
    for entropy in [0u8, 128, 255] {
        let until = skip_timer_target(expiry, entropy);
        let mut schedule = Cadence::new(base);
        schedule.deferred(Some(at + Duration::from_secs(until - stamped)), at, entropy);
        let wake = stamped + schedule.delay_at(at, entropy).as_secs();
        assert!(
            checker_skip_at(&s, base, wake).is_none(),
            "entropy {entropy}: woke at +{}s, inside its own window",
            wake - stamped
        );
    }
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_missing_or_empty_ledger_never_defers_a_check() {
    let s = staging("missing");
    assert!(checker_skip(&s, BASE).is_none(), "no ledger: check");
    std::fs::write(
        super::check_receipt::path(&s),
        "schema = 1\ncurrent_build = 42\nsource = \"fixture/channel\"\nupdated_at = \"\"\n",
    )
    .expect("write");
    assert!(checker_skip(&s, BASE).is_none(), "empty stamp: check");
    std::fs::write(super::check_receipt::path(&s), "not toml at all {{{").expect("write");
    assert!(checker_skip(&s, BASE).is_none(), "unparseable: check");
    std::fs::write(
        super::check_receipt::path(&s),
        format!(
            "schema = 1\nupdated_at = \"{}\"\n",
            aterm_types::rfc3339::format_rfc3339(now())
        ),
    )
    .expect("write");
    assert!(
        checker_skip(&s, BASE).is_none(),
        "legacy, no provenance: check"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn the_status_line_carries_delivery_only_when_recorded() {
    use super::Delivery;
    assert_eq!(
        Delivery::from_ledger_text("schema = 1\noutcome = \"up to date\"\n").status_line_suffix(),
        ""
    );
    assert_eq!(
        Delivery::from_ledger_text("not toml {{{").status_line_suffix(),
        ""
    );
    for note in ["deferred", "blocked"] {
        assert_eq!(
            Delivery::from_ledger_text(&format!("schema = 1\ndelivery = \"{note}\"\n"))
                .status_line_suffix(),
            format!(" delivery={note}")
        );
    }
}
