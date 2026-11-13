// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::time::Duration;

fn source(owner: &str) -> Source {
    Source {
        owner: owner.into(),
        repo: "channel".into(),
    }
}

#[test]
fn only_a_completed_check_for_this_source_and_build_can_defer_discovery() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::status::clear_check_note();
    let staging = Staging::scratch("check-receipt-provenance");
    let model = aterm_models_update::native_update_check_receipt_model();
    let now = crate::unix_now_secs();
    let base = Duration::from_secs(3); // 70% freshness = two seconds.
    for age in 0..=2 {
        for same_source in [false, true] {
            for same_build in [false, true] {
                record_at(&staging, 42, &source("one"), false, None, now - age);
                let bytes = std::fs::read(path(&staging)).unwrap();
                crate::status::record(&staging, 42, "staged build did not apply: terminal busy");
                assert_eq!(std::fs::read(path(&staging)).unwrap(), bytes);
                let mut state = model.init_state();
                state.insert("actual_age", i64::try_from(age).unwrap());
                state.insert("stamped_age", i64::try_from(age).unwrap());
                state.insert("same_source", i64::from(same_source));
                state.insert("same_build", i64::from(same_build));
                state.insert("written", 1);
                let skipped = crate::checker_skip_for(
                    &staging,
                    if same_build { 42 } else { 43 },
                    &source(if same_source { "ONE" } else { "two" }),
                    base,
                    now,
                )
                .is_some();
                assert_eq!(skipped, model.action_enabled("Skip", &state), "{state:?}");
                if age == 2 && same_source && same_build {
                    let generic: aterm_toml::Value = std::fs::read_to_string(&staging.status)
                        .unwrap()
                        .parse()
                        .unwrap();
                    let generic_stamp = generic.get("updated_at").unwrap().as_str().unwrap();
                    assert!(
                        !crate::rfc3339_older_than(generic_stamp, 2),
                        "negative control: the historical any-writer timestamp is fresh"
                    );
                    state.insert("skipped", 1);
                    assert!(!model.check_invariant("OnlyCompletedCheckDefers", &state));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn a_head_in_flight_shrinks_the_dedup_window_to_its_quick_rung() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::status::clear_check_note();
    let staging = Staging::scratch("check-receipt-in-flight");
    let source = source("one");
    let base = Duration::from_secs(crate::cadence::INTERVAL_SECS);
    let now = crate::unix_now_secs();
    let skip = || crate::checker_skip_for(&staging, 42, &source, base, now);

    record_at(&staging, 42, &source, false, None, now - 60);
    assert!(skip().is_some(), "no head in flight: the base window skips");

    let head = Some("v0.92.0");
    assert_eq!(record_at(&staging, 42, &source, false, None, now - 60), 0);
    assert_eq!(record_at(&staging, 42, &source, false, head, now - 60), 1);
    assert!(skip().is_none(), "check 1: a 42 s window is over at 60 s");
    assert_eq!(record_at(&staging, 42, &source, false, head, now - 30), 2);
    assert_eq!(
        skip().map(|(_, until)| until),
        Some(now - 30 + 42),
        "check 2: the 42 s window still holds at 30 s"
    );
    let spent = crate::cadence::IN_FLIGHT_CHECKS;
    let mut count = 2;
    while count < spent {
        count = record_at(&staging, 42, &source, false, head, now - 60);
    }
    assert!(skip().is_none(), "check {count}: still the 42 s window");
    assert_eq!(
        record_at(&staging, 42, &source, false, head, now - 60),
        spent + 1
    );
    assert!(
        skip().is_some(),
        "the quick waits are spent: the base window again"
    );

    std::fs::write(
        path(&staging),
        format!(
            "schema = 1\nupdated_at = \"{}\"\ncurrent_build = 42\nsource = \"one/channel\"\n\
             outcome = \"deferred\"\nhead_in_flight = \"v0.92.0\"\nhead_in_flight_checks = 1\n",
            aterm_types::rfc3339::format_rfc3339(now - 100)
        ),
    )
    .unwrap();
    assert!(skip().is_some(), "a deferral holds its own window");
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn two_builds_checking_one_head_in_flight_share_one_ladder() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("check-receipt-two-builds");
    let now = crate::unix_now_secs();
    let head = Some("v0.93.0");
    let spent = crate::cadence::IN_FLIGHT_CHECKS;
    let counts: Vec<u32> = [42, 41]
        .into_iter()
        .cycle()
        .take(spent as usize + 1)
        .map(|build| record_at(&staging, build, &source("one"), false, head, now))
        .collect();
    assert_eq!(counts, (1..=spent + 1).collect::<Vec<_>>());
    assert!(crate::cadence::in_flight_retry(spent).is_some());
    assert!(
        crate::cadence::in_flight_retry(spent + 1).is_none(),
        "the quick waits are spent"
    );
    assert_eq!(
        record_at(&staging, 41, &source("two"), false, head, now),
        1,
        "another source"
    );
    assert_eq!(
        record_at(&staging, 42, &source("two"), false, Some("v0.94.0"), now),
        1,
        "another head"
    );
    assert_eq!(record_at(&staging, 42, &source("two"), false, None, now), 0);
    assert_eq!(
        record_at(&staging, 41, &source("two"), false, Some("v0.94.0"), now),
        1,
        "a check that found none ended the run"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}
