// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn projected(
    record: &Record,
    facts: &Facts,
    evidence: &Evidence,
    book: Option<&Book>,
    now: u64,
    keeper_start: Option<u64>,
    expected: Option<u64>,
) -> (Record, Place) {
    let current = step(record, facts, evidence, book, now, keeper_start);
    let projection = step_with_deadline(record, facts, evidence, book, now, keeper_start);
    assert_eq!(
        (&projection.record, &projection.place),
        (&current.0, &current.1)
    );
    assert_eq!(projection.next_change_ms, expected);
    if let Some(at) = expected.filter(|at| *at > now) {
        assert_eq!(
            step(
                &projection.record,
                facts,
                evidence,
                book,
                at - 1,
                keeper_start
            ),
            current
        );
        assert_ne!(
            step(&projection.record, facts, evidence, book, at, keeper_start),
            current
        );
    }
    current
}

#[test]
fn time_only_comparisons_start_from_the_updated_record() {
    let facts = on(cafe());
    let evidence = Evidence::default();
    let book = book(4);
    let first = step(&Record::default(), &facts, &evidence, Some(&book), T0, None);
    let replay = step(
        &Record::default(),
        &facts,
        &evidence,
        Some(&book),
        T0 + 1,
        None,
    );
    assert_ne!(
        first.0, replay.0,
        "replaying an empty record opens a later stay"
    );
    for keeper_start in [None, Some(T0)] {
        let first = projected(
            &Record::default(),
            &facts,
            &evidence,
            Some(&book),
            T0,
            keeper_start,
            None,
        );
        assert_eq!(
            step(
                &first.0,
                &facts,
                &evidence,
                Some(&book),
                T0 + 1,
                keeper_start
            ),
            first,
            "a normalized unchanged stay has no millisecond recheck"
        );
    }
}

#[test]
fn settling_changes_strictly_after_the_inclusive_settle_period() {
    let facts = on(route(ip(192, 168, 4, 31), 22, ip(192, 168, 4, 1), None));
    let evidence = Evidence::default();
    let book = book(4);
    let first = projected(
        &learned(&home()),
        &facts,
        &evidence,
        Some(&book),
        T0,
        Some(T0),
        Some(T0 + SETTLE_MS + 1),
    );
    assert_eq!(first.1.at, Where::Checking(Checking::Moved));
    let last_unchanged = T0 + SETTLE_MS;
    assert_eq!(
        step(
            &first.0,
            &facts,
            &evidence,
            Some(&book),
            last_unchanged,
            Some(T0)
        ),
        first
    );
    let next = step(
        &first.0,
        &facts,
        &evidence,
        Some(&book),
        last_unchanged + 1,
        Some(T0),
    );
    assert_eq!(next.1.at, Where::Checking(Checking::Unidentified));
    assert!(next.1.network.as_ref().is_some_and(|id| !id.is_strong()));
    assert_ne!(next.0, first.0);
}

#[test]
fn a_future_member_answer_enters_at_the_ahead_allowance() {
    let facts = on(home());
    let mut answer = answer_on(&home(), "m1", T0);
    answer.at_ms = T0 + AHEAD_MAX_MS + 5;
    let evidence = Evidence {
        answers: vec![answer],
        ..Evidence::default()
    };
    let book = book(4);
    let first = projected(
        &Record::default(),
        &facts,
        &evidence,
        Some(&book),
        T0,
        None,
        Some(T0 + 5),
    );
    assert_eq!((first.1.heard, first.1.last_answer), (0, None));
    assert_eq!(
        step(&first.0, &facts, &evidence, Some(&book), T0 + 4, None),
        first
    );
    let next = step(&first.0, &facts, &evidence, Some(&book), T0 + 5, None);
    assert_eq!(next.0, first.0, "admission can change only Place");
    assert_eq!(next.1.heard, 1);
    assert_eq!(
        next.1.last_answer,
        Some((T0 + AHEAD_MAX_MS + 5, Lane::Fleet))
    );
}

#[test]
fn future_silent_rounds_enter_at_the_ahead_allowance() {
    let facts = on(home());
    let evidence = Evidence {
        rounds: (0..SILENT_ROUNDS)
            .map(|_| Round {
                lane: Lane::Fleet,
                answered: 0,
                blocked: 0,
                began_ms: T0,
                at_ms: T0 + AHEAD_MAX_MS + 5,
            })
            .collect(),
        ..Evidence::default()
    };
    let first = projected(
        &learned(&home()),
        &facts,
        &evidence,
        None,
        T0,
        None,
        Some(T0 + 5),
    );
    assert_eq!(first.1.at, Where::AlabLan);
    assert_eq!(step(&first.0, &facts, &evidence, None, T0 + 4, None), first);
    let next = step(&first.0, &facts, &evidence, None, T0 + 5, None);
    assert_eq!(next.1.at, Where::Remote(Kind::Lan));
    assert!(next.1.once_alab);
}

#[test]
fn internet_note_freshness_includes_its_last_millisecond() {
    let facts = on(home());
    for ok in [false, true] {
        let evidence = Evidence {
            internet: vec![InternetNote {
                ok,
                source: InternetSource::ApiProbe,
                at_ms: T0,
                endpoint: "contract.invalid".to_owned(),
            }],
            ..Evidence::default()
        };
        let first = projected(
            &learned(&home()),
            &facts,
            &evidence,
            None,
            T0,
            None,
            Some(T0 + INTERNET_FRESH_MS + 1),
        );
        assert_eq!(first.1.internet.source, InternetSource::ApiProbe);
        assert_eq!(
            step(
                &first.0,
                &facts,
                &evidence,
                None,
                T0 + INTERNET_FRESH_MS,
                None
            ),
            first
        );
        let next = step(
            &first.0,
            &facts,
            &evidence,
            None,
            T0 + INTERNET_FRESH_MS + 1,
            None,
        );
        assert_eq!(next.0, first.0);
        assert_eq!(next.1.internet.state, InternetState::PathUp);
        assert_eq!(next.1.internet.source, InternetSource::Path);
        assert_eq!(
            (next.1.internet.at_ms, next.1.internet.endpoint),
            (None, None)
        );
    }
}

#[test]
fn a_future_internet_note_enters_before_its_timestamp() {
    let facts = on(home());
    let evidence = Evidence {
        internet: vec![InternetNote {
            ok: false,
            source: InternetSource::ApiProbe,
            at_ms: T0 + AHEAD_MAX_MS + 5,
            endpoint: "contract.invalid".to_owned(),
        }],
        ..Evidence::default()
    };
    let first = projected(
        &learned(&home()),
        &facts,
        &evidence,
        None,
        T0,
        None,
        Some(T0 + 5),
    );
    assert_eq!(first.1.internet.state, InternetState::PathUp);
    assert_eq!(step(&first.0, &facts, &evidence, None, T0 + 4, None), first);
    let next = step(&first.0, &facts, &evidence, None, T0 + 5, None);
    assert_eq!(next.1.internet.state, InternetState::Down);
    assert_eq!(next.1.internet.at_ms, Some(T0 + AHEAD_MAX_MS + 5));
}

fn seed_book(at: u64) -> Book {
    let mut book = book(LEARN_AT);
    for member in &mut book.members {
        member.addr = Some(IpAddr::V4(home().address));
        member.network = NetworkId::of(&home());
        member.answered_ms = Some(at);
    }
    book
}

#[test]
fn a_book_seed_expires_after_its_inclusive_freshness_period() {
    let facts = on(home());
    let evidence = Evidence::default();
    let book = seed_book(T0);
    let first = projected(
        &Record::default(),
        &facts,
        &evidence,
        Some(&book),
        T0,
        None,
        Some(T0 + SEED_WITHIN_MS + 1),
    );
    assert_eq!(first.1.recognised, Some(Recognised::Seeded));
    assert!(first.0.remembered.is_empty());
    assert_eq!(
        step(
            &first.0,
            &facts,
            &evidence,
            Some(&book),
            T0 + SEED_WITHIN_MS,
            None
        ),
        first
    );
    let next = step(
        &first.0,
        &facts,
        &evidence,
        Some(&book),
        T0 + SEED_WITHIN_MS + 1,
        None,
    );
    assert_eq!(next.1.at, Where::Checking(Checking::Learning));
    assert_eq!(next.1.recognised, None);
    assert!(next.0.remembered.is_empty());
}

#[test]
fn a_future_book_seed_enters_at_the_ahead_allowance() {
    let facts = on(home());
    let evidence = Evidence::default();
    let book = seed_book(T0 + AHEAD_MAX_MS + 5);
    let first = projected(
        &Record::default(),
        &facts,
        &evidence,
        Some(&book),
        T0,
        None,
        Some(T0 + 5),
    );
    assert_eq!(first.1.at, Where::Checking(Checking::Learning));
    assert_eq!(
        step(&first.0, &facts, &evidence, Some(&book), T0 + 4, None),
        first
    );
    let next = step(&first.0, &facts, &evidence, Some(&book), T0 + 5, None);
    assert_eq!(next.1.recognised, Some(Recognised::Seeded));
    assert_eq!(next.1.at, Where::AlabLan);
    assert!(next.0.remembered.is_empty());
}

#[test]
fn confirmation_changes_the_record_at_its_inclusive_deadline() {
    let facts = on(home());
    let mut answer = answer_on(&home(), "m1", T0);
    answer.at_ms = T0;
    let evidence = Evidence {
        answers: vec![answer],
        ..Evidence::default()
    };
    let mut record = learned(&home());
    record.remembered[0].confirmed_ms = T0;
    let first = projected(
        &record,
        &facts,
        &evidence,
        None,
        T0,
        Some(T0),
        Some(T0 + CONFIRM_EVERY_MS),
    );
    let due = T0 + CONFIRM_EVERY_MS;
    assert_eq!(
        step(&first.0, &facts, &evidence, None, due - 1, Some(T0)),
        first
    );
    let next = step(&first.0, &facts, &evidence, None, due, Some(T0));
    assert_eq!(next.1, first.1, "confirmation can change only Record");
    assert_eq!(next.0.remembered[0].confirmed_ms, due);
    assert_eq!(
        step(&next.0, &facts, &evidence, None, due + 1, Some(T0)),
        next
    );
}

#[test]
fn evidence_admission_saturates_at_the_clock_limit() {
    let now = u64::MAX - AHEAD_MAX_MS - 1;
    let facts = on(home());
    let mut answer = answer_on(&home(), "m1", now);
    answer.at_ms = u64::MAX;
    let evidence = Evidence {
        answers: vec![answer],
        ..Evidence::default()
    };
    let first = projected(
        &Record::default(),
        &facts,
        &evidence,
        None,
        now,
        None,
        Some(now + 1),
    );
    assert_eq!(first.1.heard, 0);
    let next = step(&first.0, &facts, &evidence, None, now + 1, None);
    assert_eq!(next.1.heard, 1);
    assert_eq!(step(&next.0, &facts, &evidence, None, u64::MAX, None), next);
}

#[test]
fn the_away_entry_has_a_deadline_even_when_step_does_not_change() {
    use crate::words::{AWAY_ENTRY_AFTER_MS, Words, away_entry};

    let evidence = Evidence::default();
    let home = step(
        &learned(&home()),
        &on(home()),
        &evidence,
        None,
        T0,
        Some(T0),
    );
    let facts = on(cafe());
    let first = projected(&home.0, &facts, &evidence, None, T0 + MIN, Some(T0), None);
    assert!(!first.1.since_before);
    let due = first.1.since_ms.unwrap() + AWAY_ENTRY_AFTER_MS;
    assert_eq!(
        step(&first.0, &facts, &evidence, None, due, Some(T0)),
        first
    );
    assert!(
        away_entry(
            &first.1,
            &Words {
                now_ms: due - 1,
                zone: &|_| Some(0)
            }
        )
        .is_none()
    );
    assert!(
        away_entry(
            &first.1,
            &Words {
                now_ms: due,
                zone: &|_| Some(0)
            }
        )
        .is_some()
    );
}

#[test]
fn a_silent_round_that_changes_no_output_is_skipped() {
    let facts = on(home());
    let evidence = Evidence {
        rounds: (1..=SILENT_ROUNDS)
            .map(|index| Round {
                lane: Lane::Fleet,
                answered: 0,
                blocked: 0,
                began_ms: T0,
                at_ms: T0 + AHEAD_MAX_MS + 5 * u64::try_from(index).unwrap(),
            })
            .collect(),
        ..Evidence::default()
    };
    let due = T0 + 5 * u64::try_from(SILENT_ROUNDS).unwrap();
    let first = projected(
        &learned(&home()),
        &facts,
        &evidence,
        None,
        T0,
        None,
        Some(due),
    );
    assert_eq!(
        step(&first.0, &facts, &evidence, None, due - 1, None),
        first
    );
}

#[test]
fn competing_deadlines_are_recomputed_from_each_updated_record() {
    let facts = on(home());
    let mut answer = answer_on(&home(), "m1", T0);
    answer.at_ms = T0;
    let evidence = Evidence {
        answers: vec![answer],
        internet: vec![InternetNote {
            ok: true,
            source: InternetSource::ApiProbe,
            at_ms: T0,
            endpoint: "contract.invalid".to_owned(),
        }],
        ..Evidence::default()
    };
    let mut record = learned(&home());
    record.remembered[0].confirmed_ms = T0;
    let expiry = T0 + INTERNET_FRESH_MS + 1;
    let first = projected(&record, &facts, &evidence, None, T0, Some(T0), Some(expiry));
    let due = T0 + CONFIRM_EVERY_MS;
    let after_expiry = projected(
        &first.0,
        &facts,
        &evidence,
        None,
        expiry,
        Some(T0),
        Some(due),
    );
    let confirmed = projected(
        &after_expiry.0,
        &facts,
        &evidence,
        None,
        due,
        Some(T0),
        Some(due + CONFIRM_EVERY_MS),
    );
    assert_eq!(confirmed.0.remembered[0].confirmed_ms, due);
}

#[test]
fn a_shadowed_seed_has_no_time_only_deadline() {
    let facts = on(home());
    let evidence = Evidence::default();
    let book = seed_book(T0 + AHEAD_MAX_MS + 5);
    let first = projected(
        &learned(&home()),
        &facts,
        &evidence,
        Some(&book),
        T0,
        None,
        None,
    );
    assert_eq!(
        step(&first.0, &facts, &evidence, Some(&book), u64::MAX, None),
        first
    );
}

#[test]
fn an_expiry_beyond_the_integer_limit_is_no_deadline() {
    let now = u64::MAX - 1;
    let facts = on(home());
    let evidence = Evidence {
        internet: vec![InternetNote {
            ok: true,
            source: InternetSource::ApiProbe,
            at_ms: u64::MAX - INTERNET_FRESH_MS + 1,
            endpoint: "contract.invalid".to_owned(),
        }],
        ..Evidence::default()
    };
    let record = step(
        &Record::default(),
        &facts,
        &Evidence::default(),
        None,
        u64::MAX - INTERNET_FRESH_MS,
        None,
    )
    .0;
    let first = projected(&record, &facts, &evidence, None, now, None, None);
    assert_eq!(first.1.internet.source, InternetSource::ApiProbe);
    assert_eq!(
        step(&first.0, &facts, &evidence, None, u64::MAX, None),
        first
    );
}

#[test]
fn a_confirmation_can_be_due_on_the_last_representable_millisecond() {
    let now = u64::MAX - 1;
    let facts = on(home());
    let mut answer = answer_on(&home(), "m1", now - 1_000);
    answer.at_ms = now;
    let evidence = Evidence {
        answers: vec![answer],
        ..Evidence::default()
    };
    let mut record = step(
        &learned(&home()),
        &facts,
        &Evidence::default(),
        None,
        now - 1_000,
        None,
    )
    .0;
    record.remembered[0].confirmed_ms = u64::MAX - CONFIRM_EVERY_MS;
    let first = projected(&record, &facts, &evidence, None, now, None, Some(u64::MAX));
    let last = projected(&first.0, &facts, &evidence, None, u64::MAX, None, None);
    assert_eq!(last.0.remembered[0].confirmed_ms, u64::MAX);
}

#[test]
fn a_discarded_learning_timestamp_can_overtake_an_eviction_key() {
    let facts = on(home());
    let mut evidence = Evidence::default();
    for index in 0..LEARN_AT {
        let mut answer = answer_on(&home(), &format!("m{index}"), T0);
        answer.at_ms = T0;
        evidence.answers.push(answer);
    }
    let book = book(LEARN_AT);
    let record = Record {
        remembered: (0..REMEMBERED_MAX)
            .map(|index| {
                let mut route = cafe();
                route.gateway_hw = Some(vec![u8::try_from(index).unwrap(); 6]);
                Known {
                    network: NetworkId::of(&route).unwrap(),
                    learned_ms: T0 - 1,
                    confirmed_ms: T0 + 5,
                }
            })
            .collect(),
        ..Record::default()
    };
    let first = projected(
        &record,
        &facts,
        &evidence,
        Some(&book),
        T0,
        None,
        Some(T0 + 6),
    );
    assert!(!first.0.remembers(&NetworkId::of(&home()).unwrap()));
    assert_eq!(
        step(&first.0, &facts, &evidence, Some(&book), T0 + 5, None),
        first
    );
    let next = step(&first.0, &facts, &evidence, Some(&book), T0 + 6, None);
    assert!(next.0.remembers(&NetworkId::of(&home()).unwrap()));
}
