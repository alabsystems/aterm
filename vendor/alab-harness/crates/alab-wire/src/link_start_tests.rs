// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::fake::{FakeAterm, info_with_rows};

fn tracked_link(name: &str) -> (FakeAterm, Link) {
    let id = "example-alab";
    let config = Config::new(id, "example", &[Scope::Relaunch, Scope::Upgrade], "0.2.0")
        .unwrap()
        .with_contracts(&[Contract::Three])
        .unwrap()
        .with_c3_child_tracking();
    let fake = FakeAterm::for_harness(name, &config);
    fake.serve_contracts(&[3]);
    fake.set_info(&format!(
        "{} child-proof=1 child-start=1",
        info_with_rows(&[id])
    ));
    let link = Link::launch_at(
        config,
        std::process::id(),
        &fake.locator(),
        Child::default(),
        0,
    );
    assert!(link.judges());
    (fake, link)
}

fn begin_count(fake: &FakeAterm) -> usize {
    fake.verbs()
        .iter()
        .filter(|verb| {
            verb.split_ascii_whitespace()
                .any(|word| word.starts_with("child-start="))
        })
        .count()
}

#[test]
fn generations_sync_high_water_and_never_reuse_failed_attempt_numbers() {
    let counter = AtomicU64::new(0);
    assert_eq!(next_generation(&counter, 0).unwrap().get(), 1);
    assert_eq!(next_generation(&counter, 9).unwrap().get(), 10);
    assert_eq!(next_generation(&counter, 0).unwrap().get(), 11);
    assert_eq!(
        next_generation(&counter, u64::MAX),
        Err(Failure::Refused("child-start generation-exhausted".into()))
    );
    assert_eq!(
        next_generation(&counter, 0),
        Err(Failure::Refused("child-start generation-exhausted".into()))
    );
}

#[test]
fn all_begin_steps_retain_one_original_deadline() {
    let (fake, mut link) = tracked_link("c3-original-deadline");
    let before = fake.verbs().len();
    // Keep wall-clock scheduling out of the injected 500-ms logical budget.
    let start = Instant::now() + Duration::from_secs(60);
    let mut elapsed = [0, 100, 200, 300, 501].into_iter();
    let result = link.before_spawn_clock(None, 0, &AtomicU64::new(0), || {
        start + Duration::from_millis(elapsed.next().unwrap_or(501))
    });
    assert_eq!(
        result,
        ChildStart::Unclaimed(Failure::Gone("child-start deadline".into()))
    );
    let verbs = fake.verbs();
    assert_eq!(
        verbs[before..]
            .iter()
            .filter(|verb| verb.as_str() == "status")
            .count(),
        2,
        "high-water and pending proof were both read"
    );
    assert_eq!(begin_count(&fake), 1);
    assert!(link.is_lost() && link.event_fd().is_none());
}

#[test]
fn expired_original_deadline_refuses_a_second_begin() {
    let (fake, mut link) = tracked_link("c3-retry-deadline");
    fake.set_child_start_reply(Some("ERR beginning unavailable; retry"));
    let start = Instant::now() + Duration::from_secs(60);
    let mut elapsed = [0, 100, 200, 501].into_iter();
    let result = link.before_spawn_clock(None, 0, &AtomicU64::new(0), || {
        start + Duration::from_millis(elapsed.next().unwrap_or(501))
    });
    assert!(matches!(result, ChildStart::Unclaimed(_)));
    assert_eq!(
        begin_count(&fake),
        1,
        "retry did not acquire a new 500-ms budget"
    );
    assert!(link.is_lost() && link.event_fd().is_none());
}
