// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn functional_capture_retains_real_memory_and_exclusion_without_timing_evidence() {
    use super::*;
    use aterm_capacity::{Fixed, Reading};

    let root = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(root.path().join("ledger"));
    let mut reading = Reading::of_gib(48, 40, 14);
    reading.load1 = Some(200.0);
    let mut claim = acquire_in(
        Some(&ledger),
        &Fixed::new(reading),
        Need::new("functional capture fixture")
            .mem(4 << 30)
            .margin(MARGIN)
            .quiet(f64::INFINITY),
        Admit::Wait,
        Pace::default(),
        &mut |line| panic!("unexpected wait: {line}"),
    )
    .unwrap();
    let records = ledger.list().unwrap();
    assert_eq!(records.len(), 1, "the original claim is actually held");
    assert_eq!(records[0].need.mem, 4 << 30);
    assert_eq!(records[0].need.margin, MARGIN);
    assert!(!records[0].held.unwrap().alone);
    assert_eq!(
        claim.timing_admission(),
        Err(aterm_capacity::TimingIssue::UnknownAdmission)
    );
    assert_eq!(
        claim.measurement_entry(),
        Err(aterm_capacity::TimingIssue::UnknownAdmission)
    );
    drop(claim);
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn unchecked_or_unknown_functional_admission_creates_no_claim() {
    use super::*;
    use aterm_capacity::{Fixed, Reading};

    fn need() -> Need {
        Need::new("functional capture fixture")
            .mem(4 << 30)
            .margin(MARGIN)
            .quiet(f64::INFINITY)
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(root.path().join("ledger"));
    let healthy = Fixed::new(Reading::of_gib(48, 40, 14));
    for (ledger, need, mode) in [
        (Some(&ledger), need(), Admit::Now),
        (
            Some(&ledger),
            Need::new("zero").margin(MARGIN).quiet(f64::INFINITY),
            Admit::Wait,
        ),
        (None, need(), Admit::Wait),
    ] {
        assert!(
            acquire_in(
                ledger,
                &healthy,
                need,
                mode,
                Pace::default(),
                &mut |line| panic!("unsafe admission must not wait: {line}")
            )
            .is_err()
        );
    }
    for reading in [
        Reading::default(),
        Reading {
            available: None,
            ..Reading::of_gib(48, 40, 14)
        },
        Reading {
            physical: None,
            ..Reading::of_gib(48, 40, 14)
        },
        Reading {
            available: Some(u64::MAX),
            ..Reading::of_gib(48, 40, 14)
        },
    ] {
        assert!(
            acquire_in(
                Some(&ledger),
                &Fixed::new(reading),
                need(),
                Admit::Wait,
                Pace::default(),
                &mut |line| panic!("unread memory must not wait: {line}")
            )
            .is_err()
        );
    }
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn losing_memory_evidence_while_waiting_cannot_start_functional_work() {
    use super::*;
    use aterm_capacity::Reading;

    struct Lost {
        reads: Cell<usize>,
    }
    impl Machine for Lost {
        fn read(&self, _: Option<&Path>) -> Reading {
            let n = self.reads.get();
            self.reads.set(n + 1);
            if n == 0 {
                Reading::of_gib(48, 0, 14)
            } else {
                Reading::default()
            }
        }
        fn procs(&self) -> Option<aterm_capacity::Procs> {
            None
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(root.path().join("ledger"));
    let machine = Lost {
        reads: Cell::new(0),
    };
    let error = acquire_in(
        Some(&ledger),
        &machine,
        Need::new("functional capture fixture")
            .mem(4 << 30)
            .margin(MARGIN)
            .quiet(f64::INFINITY),
        Admit::Wait,
        Pace {
            poll: std::time::Duration::ZERO,
            ..Pace::default()
        },
        &mut |_| {},
    )
    .unwrap_err();
    assert!(machine.reads.get() >= 2);
    assert!(error.contains("changed to unknown"), "{error}");
    assert!(ledger.list().unwrap().is_empty());
}
