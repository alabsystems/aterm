// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: u64 = 1_800_000_000_000;

fn away() -> Place {
    let mut place = crate::sample::hotspot(NOW, NOW - 1_000);
    place.since_before = false;
    place.since_ms = Some(NOW);
    place
}

fn check(place: &Place, now: u64, expected: Option<u64>) -> Option<AwayKey> {
    let projection = away_key_with_deadline(place, now);
    let key = away_key(place, now);
    assert_eq!(projection.key, key);
    assert_eq!(projection.next_change_ms, expected);
    let words = Words {
        now_ms: now,
        zone: &|_| Some(0),
    };
    assert_eq!(away_entry(place, &words).is_some(), key.is_some());
    if let Some(at) = expected {
        assert!(at > now);
        assert_eq!(away_key(place, at - 1), key);
        assert_ne!(away_key(place, at), key);
    }
    key
}

#[test]
fn the_away_dwell_enters_at_its_inclusive_deadline() {
    let place = away();
    let due = NOW + AWAY_ENTRY_AFTER_MS;
    assert_eq!(check(&place, due - 1, Some(due)), None);
    let key = check(&place, due, None).expect("eligible away key");
    assert_eq!(check(&place, due + 1, None), Some(key));
}

#[test]
fn fixed_ineligibility_has_no_presentation_deadline() {
    let original = away();
    let mut cases = vec![original.clone(); 5];
    cases[0].at = Where::Offline;
    cases[1].at = Where::Checking(Checking::Moved);
    cases[2].alab = Alab::Learning;
    cases[3].once_alab = true;
    cases[4].since_ms = None;
    for place in cases {
        assert_eq!(check(&place, NOW, None), None);
        assert_eq!(check(&place, u64::MAX, None), None);
    }
}

#[test]
fn a_stay_known_to_begin_before_the_record_needs_no_dwell() {
    let mut place = away();
    place.since_before = true;
    place.since_ms = None;
    let key = check(&place, 0, None).expect("known earlier stay");
    assert_eq!(
        key.since_ms,
        match place.alab {
            Alab::Away { since_ms } => since_ms,
            _ => unreachable!(),
        }
    );
}

#[test]
fn a_future_arrival_keeps_its_full_dwell() {
    let mut place = away();
    place.since_ms = Some(NOW + 5);
    let due = NOW + 5 + AWAY_ENTRY_AFTER_MS;
    assert_eq!(check(&place, NOW, Some(due)), None);
    assert!(check(&place, due, None).is_some());
}

#[test]
fn an_unrepresentable_dwell_is_not_wrapped_into_a_deadline() {
    let mut place = away();
    place.since_ms = Some(u64::MAX);
    assert_eq!(check(&place, u64::MAX - 1, None), None);
    assert_eq!(check(&place, u64::MAX, None), None);
}

#[test]
fn the_dwell_can_end_on_the_last_representable_millisecond() {
    let mut place = away();
    place.since_ms = Some(u64::MAX - AWAY_ENTRY_AFTER_MS);
    assert_eq!(check(&place, u64::MAX - 1, Some(u64::MAX)), None);
    assert!(check(&place, u64::MAX, None).is_some());
}

#[test]
fn hotspot_wording_variants_share_the_existing_fact_key() {
    let now = NOW + AWAY_ENTRY_AFTER_MS;
    let first = away();
    let mut by_range = first.clone();
    by_range.at = Where::Remote(Kind::Hotspot { by_range: true });
    by_range.kind = Some(Kind::Hotspot { by_range: true });
    assert_eq!(check(&first, now, None), check(&by_range, now, None));
    let words = Words {
        now_ms: now,
        zone: &|_| Some(0),
    };
    let before = away_entry(&first, &words).unwrap();
    let after = away_entry(&by_range, &words).unwrap();
    assert_eq!(before.log, after.log);
    assert_ne!(
        before.more, after.more,
        "detail retains the range qualification"
    );
}

#[test]
fn typed_key_equality_matches_the_formatted_follower_key() {
    let now = NOW + AWAY_ENTRY_AFTER_MS;
    let words = Words {
        now_ms: now,
        zone: &|_| Some(0),
    };
    let mut cases = vec![away(); 7];
    cases[1].internet.state = InternetState::Ok;
    cases[2].kind = Some(Kind::Hotspot { by_range: true });
    cases[3].kind = Some(Kind::Lan);
    cases[4].kind = None;
    cases[5].alab = Alab::Away { since_ms: None };
    cases[6].alab = Alab::Away {
        since_ms: Some(NOW - 2_000),
    };
    let old_key = |place: &Place| {
        away_entry(place, &words).map(|entry| {
            let since = match place.alab {
                Alab::Away { since_ms } => since_ms,
                _ => None,
            };
            (entry.log, since)
        })
    };
    for a in &cases {
        for b in &cases {
            assert_eq!(
                old_key(a) == old_key(b),
                away_key(a, now) == away_key(b, now)
            );
        }
    }
    assert_ne!(away_key(&cases[0], now), away_key(&cases[3], now));
    assert_ne!(away_key(&cases[0], now), away_key(&cases[4], now));
    assert_ne!(away_key(&cases[0], now), away_key(&cases[5], now));
    assert_ne!(away_key(&cases[0], now), away_key(&cases[6], now));
}
