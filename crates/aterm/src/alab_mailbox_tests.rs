// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::mailbox_seen::Address;

/// A pair's list is taken up once a day in a process once a pass settled, whatever the
/// spelling; another sign-in address on the same mailbox is its own pair, a running pass
/// holds its pair, and one that did not settle leaves it due at once. The registry reads the
/// list as the harness noted it.
/// NEGATIVE CONTROL: with `begin` keyed by the login alone (target ignored in `key`), the
/// day's second address waits: `` assertion failed: rests.begin(LOGIN, Some("b@example.test"),
/// now) ``; with `end` resting an unsettled pass, the retry waits: `` assertion failed:
/// rests.begin(OTHER, None, now) ``.
#[test]
fn a_pair_rests_once_settled_and_a_failed_pass_is_due_again() {
    const LOGIN: &str = "owner@proton.example";
    const OTHER: &str = "other@proton.example";
    let rests = Rests::new();
    let now = Instant::now();
    assert!(rests.begin(LOGIN, Some("a@example.test"), now));
    assert!(!rests.begin(LOGIN, Some("a@example.test"), now), "running");
    rests.end(LOGIN, Some("a@example.test"), true, now);
    assert!(!rests.begin("Owner@proton.example ", Some("A@example.test"), now));
    assert!(rests.begin(LOGIN, Some("b@example.test"), now));
    rests.end(LOGIN, Some("b@example.test"), true, now);
    assert!(rests.begin(LOGIN, Some("a@example.test"), now + REST));
    rests.end(LOGIN, Some("a@example.test"), true, now + REST);
    assert!(rests.begin(OTHER, None, now));
    rests.end(OTHER, None, false, now);
    assert!(rests.begin(OTHER, None, now));
    rests.end(OTHER, None, true, now);
    // Nothing runs: settling returns at once.
    rests.settle(Duration::ZERO);
    assert_eq!(*rests.running.lock().unwrap(), 0);
    assert!(settled(&Ok(Discovered {
        outcome: aterm_db::alab::discover::Outcome::Unchanged,
        findings: Vec::new(),
        records: 0,
    })));
    assert!(!speaks(&Ok(Discovered {
        outcome: aterm_db::alab::discover::Outcome::Unchanged,
        findings: Vec::new(),
        records: 0,
    })));
    assert!(speaks(&Ok(Discovered {
        outcome: aterm_db::alab::discover::Outcome::QuotaSpent,
        findings: Vec::new(),
        records: 0,
    })));
    assert!(speaks(&Ok(Discovered {
        outcome: aterm_db::alab::discover::Outcome::Held,
        findings: Vec::new(),
        records: 0,
    })));
    assert!(speaks(&Err("no valid clock on this Mac".into())));
    assert!(!settled(&Ok(Discovered {
        outcome: aterm_db::alab::discover::Outcome::Incomplete,
        findings: Vec::new(),
        records: 0,
    })));
    assert!(!settled(&Err("no vault state on this Mac".into())));
    let seen = Seen {
        login: "owner@proton.example".into(),
        target: Some("ai@example.test".into()),
        addresses: vec![Address {
            email: "ai@example.test".into(),
            own: false,
            receives: true,
        }],
        complete: true,
    };
    let read = listing(&seen);
    assert_eq!(read.login, seen.login);
    assert_eq!(read.target, seen.target);
    assert!(read.complete);
    assert_eq!(read.listed.len(), 1);
    assert!(read.listed[0].receives);
}

/// The hook's take-up ends each pair by its own pass: one that did not settle is due again at
/// once, one that settled rests, and where no thread starts the pair is due again and no pass
/// is left counted as running, so an ending process does not wait out [`SETTLE`] for it.
/// NEGATIVE CONTROL: with the thread ending its pair settled whatever its pass said, the
/// incomplete pair waits: `` an incomplete pass leaves its pair due ``; with the failed
/// spawn's `end` dropped, `` no pass is left running `` fails.
#[test]
fn a_take_up_ends_its_pair_by_its_pass_and_frees_it_when_no_thread_starts() {
    use aterm_db::alab::discover::Outcome;
    const LOGIN: &str = "owner@proton.example";
    fn incomplete(_: &Listing) -> Result<Discovered, String> {
        Ok(Discovered {
            outcome: Outcome::Incomplete,
            findings: Vec::new(),
            records: 0,
        })
    }
    fn unchanged(_: &Listing) -> Result<Discovered, String> {
        Ok(Discovered {
            outcome: Outcome::Unchanged,
            findings: Vec::new(),
            records: 0,
        })
    }
    fn unreached(_: &Listing) -> Result<Discovered, String> {
        panic!("no pass runs where no thread started")
    }
    let rests: &'static Rests = Box::leak(Box::new(Rests::new()));
    let seen = |target: &str| Seen {
        login: LOGIN.into(),
        target: Some(target.into()),
        addresses: Vec::new(),
        complete: true,
    };
    let inline = |body: Box<dyn FnOnce() + Send>| {
        body();
        Ok(())
    };
    take_up(rests, &seen("a@example.test"), inline, incomplete);
    take_up(rests, &seen("b@example.test"), inline, unchanged);
    assert!(
        rests.begin(LOGIN, Some("a@example.test"), Instant::now()),
        "an incomplete pass leaves its pair due"
    );
    rests.end(LOGIN, Some("a@example.test"), false, Instant::now());
    assert!(
        !rests.begin(LOGIN, Some("b@example.test"), Instant::now()),
        "a settled pass rests its pair"
    );
    take_up(
        rests,
        &seen("c@example.test"),
        |_| Err(std::io::Error::other("no thread")),
        unreached,
    );
    assert_eq!(*rests.running.lock().unwrap(), 0, "no pass is left running");
    assert!(
        rests.begin(LOGIN, Some("c@example.test"), Instant::now()),
        "a pair whose thread never started is due again"
    );
}
