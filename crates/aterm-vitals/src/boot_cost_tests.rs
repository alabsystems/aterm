// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::published_cpu::{CpuBasis, CpuExecutionWindow};

const SECOND: u64 = 1_000_000_000;

fn observation(seconds: u64) -> Observation {
    let at_ns = seconds * SECOND;
    Observation {
        at_ns,
        before_ns: Some(at_ns.saturating_sub(10)),
        after_ns: Some(at_ns),
        process: Some(ProcessKey {
            pid: 17,
            start: 4,
            version: Some(2),
        }),
        counter_epoch: 1,
        process_cpu: RawCounters {
            own_ns: Some(31),
            inherited_ns: Some(47),
        },
        coalition_id: Some(5),
        coalition_cpu: RawCounters {
            own_ns: Some(101),
            inherited_ns: Some(211),
        },
        historical_reported: HistoricalReported {
            own_ns: 13,
            tabs_ns: 29,
            incomplete: false,
        },
        wakes: Some(WakeCounts {
            epoch: 3,
            variants: vec![WakeCount {
                kind: "Paint".into(),
                count: 7,
            }],
        }),
        coverage: Coverage::default(),
    }
}

fn captured(seconds: &[u64]) -> Book {
    let mut collector = Collector::new(Book::default());
    for &second in seconds {
        collector
            .observe(Some("boot-a"), observation(second))
            .unwrap();
    }
    collector.book().clone()
}

#[test]
fn exact_targets_retain_two_real_independent_cuts() {
    let mut collector = Collector::new(Book::default());
    assert_eq!(
        collector.book().next_target_ns("boot-a"),
        Some(TARGETS_NS[0])
    );
    collector.observe(Some("boot-a"), observation(0)).unwrap();
    collector.observe(Some("boot-a"), observation(179)).unwrap();
    collector.observe(Some("boot-a"), observation(180)).unwrap();
    assert_eq!(
        collector.book().next_target_ns("boot-a"),
        Some(TARGETS_NS[1])
    );
    collector.observe(Some("boot-a"), observation(900)).unwrap();
    let boot = collector.book().boot("boot-a").unwrap();
    assert_eq!(boot.targets, [Some(0), Some(1)]);
    assert_eq!(
        boot.observations
            .iter()
            .map(|o| o.at_ns)
            .collect::<Vec<_>>(),
        TARGETS_NS
    );
    assert_eq!(collector.book().next_target_ns("boot-a"), None);
    assert_eq!(boot.coverage, Coverage::default());
    let o = boot.observation(1).unwrap();
    assert_eq!(
        o.process_cpu,
        RawCounters {
            own_ns: Some(31),
            inherited_ns: Some(47)
        }
    );
    assert_eq!(
        o.coalition_cpu,
        RawCounters {
            own_ns: Some(101),
            inherited_ns: Some(211)
        }
    );
    assert_eq!(
        o.historical_reported,
        HistoricalReported {
            own_ns: 13,
            tabs_ns: 29,
            incomplete: false
        }
    );
    assert_eq!(o.accounting().basis, CpuBasis::PublishedCounters);
    assert_eq!(
        o.accounting().execution_window,
        CpuExecutionWindow::Unavailable
    );
}

#[test]
fn late_and_sleep_overshoots_share_one_actual_cut() {
    for seconds in [&[179, 901][..], &[901][..], &[173, 950][..]] {
        let book = captured(seconds);
        let boot = book.boot("boot-a").unwrap();
        assert_eq!(boot.observations.len(), 1);
        assert_eq!(boot.targets, [Some(0), Some(0)]);
        assert_eq!(boot.observations[0].at_ns, seconds.last().unwrap() * SECOND);
        assert!(
            boot.coverage_for(0).contains(
                Coverage::LATE_START
                    .with(Coverage::LATE_CUT)
                    .with(Coverage::PARTIAL)
            )
        );
    }
}

#[test]
fn missing_identity_counters_and_wakes_remain_unavailable() {
    let mut collector = Collector::new(Book::default());
    assert!(
        !collector
            .observe(
                None,
                Observation {
                    at_ns: TARGETS_NS[1],
                    ..Observation::default()
                }
            )
            .unwrap()
    );
    assert!(collector.book().boots.is_empty());
    assert_eq!(collector.book().next_target_ns(""), None);
    collector
        .observe(
            Some("boot-a"),
            Observation {
                at_ns: TARGETS_NS[0],
                ..Observation::default()
            },
        )
        .unwrap();
    let boot = collector.book().boot("boot-a").unwrap();
    let o = boot.observation(0).unwrap();
    assert_eq!(o.process_cpu.own_ns, None);
    assert_eq!(o.coalition_cpu.own_ns, None);
    assert_eq!(o.wakes, None);
    assert!(
        boot.coverage.contains(
            Coverage::PARTIAL
                .with(Coverage::IDENTITY_MISSING)
                .with(Coverage::COUNTERS_MISSING)
                .with(Coverage::WAKE_MISSING)
        )
    );
    assert_eq!(
        decode(&encode(collector.book()).unwrap()).unwrap(),
        *collector.book()
    );
}

#[test]
fn source_counter_and_wake_epochs_qualify_without_combining_channels() {
    let mut collector = Collector::new(Book::default());
    collector.observe(Some("boot-a"), observation(179)).unwrap();
    let mut changed = observation(180);
    changed.process = Some(ProcessKey {
        pid: 17,
        start: 9,
        version: None,
    });
    changed.counter_epoch = 2;
    changed.coalition_id = Some(7);
    changed.historical_reported = HistoricalReported {
        own_ns: 1,
        tabs_ns: 2,
        incomplete: true,
    };
    changed.wakes.as_mut().unwrap().epoch = 4;
    let expected = changed.clone();
    collector.observe(Some("boot-a"), changed).unwrap();
    let boot = collector.book().boot("boot-a").unwrap();
    assert!(
        boot.coverage.contains(
            Coverage::SOURCE_CHANGED
                .with(Coverage::COUNTER_RESET)
                .with(Coverage::WAKE_RESET)
                .with(Coverage::IDENTITY_MISSING)
        )
    );
    let cut = boot.observation(0).unwrap();
    assert_eq!(cut.process_cpu, expected.process_cpu);
    assert_eq!(cut.coalition_cpu, expected.coalition_cpu);
    assert_eq!(cut.historical_reported, expected.historical_reported);
    let frozen = cut.clone();
    collector.observe(Some("boot-a"), observation(900)).unwrap();
    assert_eq!(
        collector.book().boot("boot-a").unwrap().observation(0),
        Some(&frozen)
    );
}

#[test]
fn comparable_decreases_and_wake_saturation_stay_partial() {
    let mut collector = Collector::new(Book::default());
    collector.observe(Some("boot-a"), observation(179)).unwrap();
    let mut decreased = observation(180);
    decreased.process_cpu.own_ns = Some(0);
    decreased.coalition_cpu.inherited_ns = Some(0);
    decreased.wakes.as_mut().unwrap().variants[0].count = 0;
    collector.observe(Some("boot-a"), decreased).unwrap();
    assert!(
        collector
            .book()
            .boot("boot-a")
            .unwrap()
            .coverage
            .contains(Coverage::COUNTER_RESET.with(Coverage::WAKE_RESET))
    );
    let mut saturated = observation(900);
    saturated.wakes.as_mut().unwrap().variants[0].count = u64::MAX;
    collector.observe(Some("boot-a"), saturated).unwrap();
    assert!(
        collector
            .book()
            .boot("boot-a")
            .unwrap()
            .observation(1)
            .unwrap()
            .coverage
            .contains(Coverage::PARTIAL)
    );
}

#[test]
fn rising_wake_counts_in_a_new_process_do_not_continue_a_reused_local_epoch() {
    let mut collector = Collector::new(Book::default());
    let mut original = observation(179);
    original.wakes.as_mut().unwrap().epoch = 1;
    collector.observe(Some("boot-a"), original).unwrap();
    let mut relaunched = observation(180);
    relaunched.process = Some(ProcessKey {
        pid: 23,
        start: 12,
        version: Some(1),
    });
    relaunched.process_cpu.own_ns = Some(101);
    relaunched.wakes.as_mut().unwrap().epoch = 1;
    relaunched.wakes.as_mut().unwrap().variants[0].count = 77;
    collector.observe(Some("boot-a"), relaunched).unwrap();
    let boot = collector.book().boot("boot-a").unwrap();
    assert!(
        boot.coverage_for(0).contains(
            Coverage::SOURCE_CHANGED
                .with(Coverage::WAKE_RESET)
                .with(Coverage::PARTIAL)
        )
    );
    let cut = boot.observation(0).unwrap();
    assert_eq!(cut.counter_epoch, 1);
    assert_eq!(cut.wakes.as_ref().unwrap().epoch, 1);
    assert_eq!(cut.wakes.as_ref().unwrap().variants[0].count, 77);
    assert_eq!(cut.process_cpu.own_ns, Some(101));
}

#[test]
fn restored_and_completed_cuts_are_immutable_and_pending_cuts_survive_restoration() {
    let original = captured(&[180]);
    let frozen = original.boots[0].observations[0].clone();
    let mut collector = Collector::new(decode(&encode(&original).unwrap()).unwrap());
    collector.observe(Some("boot-a"), observation(900)).unwrap();
    let boot = collector.book().boot("boot-a").unwrap();
    assert_eq!(boot.observation(0), Some(&frozen));
    assert!(
        boot.coverage_for(1).contains(
            Coverage::RESTORED_GAP
                .with(Coverage::COLLECTION_GAP)
                .with(Coverage::PARTIAL)
        )
    );
    let pending = collector.book().clone();
    assert!(!collector.restore(&original).unwrap());
    assert_eq!(collector.book(), &pending);
    let mut later = observation(1000);
    later.process_cpu.own_ns = Some(999);
    assert!(!collector.observe(Some("boot-a"), later).unwrap());
    assert_eq!(collector.book(), &pending);
}

#[test]
fn first_persisted_target_wins_conflicting_sources_and_conflict_is_sticky() {
    let original = captured(&[180]);
    let mut replacement = observation(181);
    replacement.process = Some(ProcessKey {
        pid: 23,
        start: 12,
        version: Some(1),
    });
    replacement.counter_epoch = 2;
    replacement.process_cpu.own_ns = Some(777);
    let mut incoming = Collector::new(Book::default());
    incoming.observe(Some("boot-a"), replacement).unwrap();
    incoming.observe(Some("boot-a"), observation(900)).unwrap();
    let mut merged = original.clone();
    assert!(merged.merge(incoming.book()).unwrap());
    let boot = merged.boot("boot-a").unwrap();
    assert_eq!(boot.observation(0), original.boots[0].observation(0));
    assert_eq!(boot.observation(0).unwrap().process_cpu.own_ns, Some(31));
    assert_eq!(boot.observations.len(), 2);
    assert!(
        boot.coverage_for(0)
            .contains(Coverage::CONFLICT.with(Coverage::PARTIAL))
    );
    assert_eq!(decode(&encode(&merged).unwrap()).unwrap(), merged);
    assert!(!merged.merge(&original).unwrap());
    assert!(merged.boots[0].coverage_for(0).contains(Coverage::CONFLICT));
}

#[test]
fn merging_a_new_cut_compares_counter_and_wake_epochs_without_rewriting_it() {
    let mut persisted = captured(&[180]);
    let mut incoming = Collector::new(Book::default());
    let mut reset = observation(900);
    reset.counter_epoch = 8;
    reset.wakes.as_mut().unwrap().epoch = 9;
    incoming.observe(Some("boot-a"), reset).unwrap();
    let immutable = incoming.book().boots[0].observations[0].clone();
    persisted.merge(incoming.book()).unwrap();
    let boot = &persisted.boots[0];
    assert_eq!(boot.observation(1), Some(&immutable));
    assert!(
        boot.coverage_for(1).contains(
            Coverage::COUNTER_RESET
                .with(Coverage::WAKE_RESET)
                .with(Coverage::CONFLICT)
        )
    );
}

#[test]
fn rejection_is_atomic_and_reboot_uses_a_distinct_record() {
    let mut collector = Collector::new(captured(&[180]));
    let before = collector.book().clone();
    assert!(collector.observe(Some("boot-a"), observation(179)).is_err());
    assert_eq!(collector.book(), &before);
    collector.observe(Some("boot-b"), observation(180)).unwrap();
    assert_eq!(collector.book().boots.len(), 2);
    let before = collector.book().clone();
    let mut invalid = before.clone();
    invalid.boots[0].targets[0] = Some(9);
    let mut book = before.clone();
    assert!(book.merge(&invalid).is_err());
    assert_eq!(book, before);
}

#[test]
fn retention_bounds_boots_bytes_and_wake_variants() {
    let mut collector = Collector::new(Book::default());
    for n in 0..MAX_BOOTS + 3 {
        collector
            .observe(Some(&format!("boot-{n}")), observation(900))
            .unwrap();
    }
    assert_eq!(collector.book().boots.len(), MAX_BOOTS);
    assert_eq!(collector.book().boots[0].boot_id, "boot-3");
    let mut collector = Collector::new(Book::default());
    for n in 0..MAX_BOOTS {
        let mut large = observation(900);
        large.wakes.as_mut().unwrap().variants = (0..MAX_VARIANTS)
            .map(|i| WakeCount {
                kind: format!("{i:03}{}", "x".repeat(MAX_WAKE_KIND_BYTES - 3)),
                count: i as u64,
            })
            .collect();
        collector
            .observe(Some(&format!("boot-{n}")), large)
            .unwrap();
    }
    assert!(
        collector.book().boots.len() < MAX_BOOTS,
        "the byte cap also evicts oldest boots"
    );
    let bytes = encode(collector.book()).unwrap();
    assert!(bytes.len() <= MAX_BYTES);
    assert_eq!(decode(&bytes).unwrap(), *collector.book());
    assert!(
        collector.book().boots.iter().all(|b| b.observations[0]
            .wakes
            .as_ref()
            .unwrap()
            .variants
            .len()
            == MAX_VARIANTS)
    );
    let before = collector.book().clone();
    let mut too_many = observation(900);
    too_many.wakes.as_mut().unwrap().variants = (0..=MAX_VARIANTS)
        .map(|i| WakeCount {
            kind: format!("Wake{i}"),
            count: 1,
        })
        .collect();
    assert_eq!(
        collector.observe(Some("too-many"), too_many),
        Err(Error::Bounds)
    );
    assert_eq!(collector.book(), &before);
}

#[test]
fn strict_codec_rejects_truncation_bad_tags_bounds_and_extra_documents() {
    let book = captured(&[180, 900]);
    let bytes = encode(&book).unwrap();
    assert_eq!(decode(&bytes).unwrap(), book);
    for end in 0..bytes.len() {
        assert!(decode(&bytes[..end]).is_err(), "prefix {end}");
    }
    assert_eq!(decode(&vec![0; MAX_BYTES + 1]), Err(Error::TooLarge));
    let mut bad = bytes.clone();
    bad[7] = 2;
    assert_eq!(decode(&bad), Err(Error::Format));
    bad = bytes.clone();
    bad[8] = MAX_BOOTS as u8 + 1;
    assert_eq!(decode(&bad), Err(Error::Bounds));
    bad = bytes.clone();
    bad[11] = 255;
    assert_eq!(decode(&bad), Err(Error::Utf8));
    let before_tag = 9 + 2 + "boot-a".len() + 8 + 2 + 1 + 2 + 8;
    bad = bytes.clone();
    bad[before_tag] = 2;
    assert_eq!(decode(&bad), Err(Error::Invalid("tag")));
    bad = bytes.clone();
    bad.extend_from_slice(&bytes);
    assert_eq!(decode(&bad), Err(Error::Trailing));
}

/// One named change to an observation.
type Edit = (&'static str, fn(&mut Observation));

#[test]
fn restored_transitions_require_every_source_counter_and_wake_qualification() {
    let cases: [Edit; 12] = [
        ("process generation", |o| {
            o.process.as_mut().unwrap().start += 1
        }),
        ("coalition identity", |o| o.coalition_id = Some(8)),
        ("counter epoch", |o| o.counter_epoch += 1),
        ("process own", |o| o.process_cpu.own_ns = Some(0)),
        ("process inherited", |o| {
            o.process_cpu.inherited_ns = Some(0)
        }),
        ("coalition own", |o| o.coalition_cpu.own_ns = Some(0)),
        ("coalition inherited", |o| {
            o.coalition_cpu.inherited_ns = Some(0)
        }),
        ("historical own", |o| o.historical_reported.own_ns = 0),
        ("historical tabs", |o| o.historical_reported.tabs_ns = 0),
        ("wake epoch", |o| o.wakes.as_mut().unwrap().epoch += 1),
        ("wake count", |o| {
            o.wakes.as_mut().unwrap().variants[0].count = 0
        }),
        ("missing wake kind", |o| {
            o.wakes.as_mut().unwrap().variants.clear()
        }),
    ];
    for (name, change) in cases {
        let mut forged = captured(&[180, 900]);
        change(&mut forged.boots[0].observations[1]);
        assert_eq!(
            encode(&forged),
            Err(Error::Invalid("unqualified observation transition")),
            "{name}"
        );
        let observations = forged.boots[0].observations.clone();
        let required = observed_coverage(&observations[1], Some(&observations[0]));
        forged.boots[0].coverage = forged.boots[0].coverage.with(required);
        let bytes = encode(&forged).unwrap();
        assert_eq!(decode(&bytes).unwrap(), forged, "qualified {name}");
        assert_eq!(
            forged.boots[0].observations, observations,
            "qualifying {name} changes no frozen payload"
        );
        let coverage_at = 9 + 2 + forged.boots[0].boot_id.len() + 8;
        for flag in [
            Coverage::SOURCE_CHANGED,
            Coverage::COUNTER_RESET,
            Coverage::WAKE_RESET,
        ] {
            if !required.contains(flag) {
                continue;
            }
            let mut unqualified = bytes.clone();
            let missing = Coverage(forged.boots[0].coverage.0 & !flag.0);
            unqualified[coverage_at..coverage_at + 2].copy_from_slice(&missing.0.to_le_bytes());
            assert_eq!(
                decode(&unqualified),
                Err(Error::Invalid("unqualified observation transition")),
                "{name} missing {flag:?}"
            );
        }
        let mut collector = Collector::new(Book::default());
        collector.observe(Some("boot-a"), observation(180)).unwrap();
        let mut next = observation(900);
        change(&mut next);
        collector.observe(Some("boot-a"), next).unwrap();
        assert_eq!(
            decode(&encode(collector.book()).unwrap()).unwrap(),
            *collector.book(),
            "collector {name}"
        );
    }
}

#[test]
fn strict_documents_reject_contradictory_or_invented_coverage() {
    let original = captured(&[180, 900]);
    let reject = |mutate: fn(&mut Book)| {
        let mut bad = original.clone();
        mutate(&mut bad);
        assert!(encode(&bad).is_err(), "accepted {bad:?}");
    };
    reject(|b| b.boots.push(b.boots[0].clone()));
    reject(|b| b.boots[0].targets[0] = None);
    reject(|b| b.boots[0].targets[1] = Some(2));
    reject(|b| b.boots[0].observations[0].at_ns = 179 * SECOND);
    reject(|b| b.boots[0].observations[0].after_ns = Some(181 * SECOND));
    reject(|b| b.boots[0].observations[0].coverage = Coverage(1 << 15));
    reject(|b| b.boots[0].observations[0].process_cpu.own_ns = None);
    reject(|b| {
        let wakes = b.boots[0].observations[0].wakes.as_mut().unwrap();
        wakes.variants.push(wakes.variants[0].clone());
    });
    reject(|b| b.boots[0].coverage = Coverage::default());
    let mut fabricated = captured(&[901]);
    fabricated.boots[0].observations.push(observation(902));
    fabricated.boots[0].targets[1] = Some(1);
    assert!(encode(&fabricated).is_err());
}
