// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn refusal(code: u8, msg: &str) -> io::Error {
    astream_broker::BrokerRefusal {
        code,
        msg: msg.to_string(),
    }
    .into()
}

fn limited(ms: u64) -> io::Error {
    refusal(
        astream_broker::proto::ERR_RATE_LIMITED,
        &format!(
            "{}{ms} (record rate exceeded)",
            astream_broker::proto::RATE_LIMITED_PREFIX
        ),
    )
}

#[test]
fn only_a_rate_limited_refusal_names_a_wait() {
    assert_eq!(wait_of(&limited(7)), Some(Duration::from_millis(7)));
    assert_eq!(wait_of(&limited(0)), Some(Duration::from_millis(1)));
    assert_eq!(
        wait_of(&refusal(
            astream_broker::proto::ERR_RATE_LIMITED,
            "no number"
        )),
        Some(UNNAMED_WAIT)
    );
    assert_eq!(
        wait_of(&refusal(
            astream_broker::proto::ERR_REFUSED,
            "batch aborted"
        )),
        None
    );
    assert!(rate_limited(&held(Duration::from_millis(3))));
    assert!(!rate_limited(&io::Error::other("lost")));
}

#[test]
fn the_producer_waits_at_most_its_cap_and_the_sender_the_whole_wait() {
    let t0 = Instant::now();
    let mut hold = RateHold::default();
    let long = Duration::from_secs(30);
    hold.refused(t0, long);
    hold.hold_sender("s-a", t0, long);
    assert_eq!(hold.producer_left(t0), Some(PRODUCER_HOLD_MAX));
    assert!(
        hold.holds_sender("s-b", t0),
        "every sender while the producer waits"
    );
    let after_cap = t0 + PRODUCER_HOLD_MAX;
    assert!(hold.expire(after_cap), "the producer's hold ended");
    assert_eq!(hold.producer_left(after_cap), None);
    assert!(!hold.holds_sender("s-b", after_cap));
    assert!(
        hold.holds_sender("s-a", after_cap),
        "s-a waits its whole wait"
    );
    assert_eq!(hold.next_end(after_cap), Some(t0 + long));
    assert!(hold.expire(t0 + long));
    assert!(!hold.holds_sender("s-a", t0 + long));
    assert_eq!(hold.next_end(t0 + long), None);
    assert!(!hold.expire(t0 + long), "nothing left to end");
}

#[test]
fn a_later_end_stands() {
    let t0 = Instant::now();
    let mut hold = RateHold::default();
    hold.refused(t0, Duration::from_millis(500));
    hold.refused(t0, Duration::from_millis(100));
    assert_eq!(hold.producer_left(t0), Some(Duration::from_millis(500)));
    hold.hold_sender("s-a", t0, Duration::from_millis(500));
    hold.hold_sender("s-a", t0, Duration::from_millis(100));
    assert_eq!(hold.next_end(t0), Some(t0 + Duration::from_millis(500)));
}

#[test]
fn an_unrepresentable_wait_keeps_the_sender_held_without_panicking() {
    let t0 = Instant::now();
    for wait in [wait_of(&limited(u64::MAX)).unwrap(), Duration::MAX] {
        let mut hold = RateHold::default();
        hold.hold_sender("s-a", t0, wait);
        let later = t0 + Duration::from_secs(2);
        hold.hold_sender("s-a", later, Duration::from_millis(1));
        assert!(hold.holds_sender("s-a", later));
        assert!(!hold.holds_sender("s-b", later));
        assert!(!hold.expire(later));
        let wake = hold.next_end(later).expect("the held sender has a wake");
        assert!(wake > later, "a long wait never spins");
        if t0.checked_add(wait).is_none() {
            assert!(
                wake <= later + PRODUCER_HOLD_MAX,
                "revisit without shortening the hold"
            );
        }
        assert!(hold.holds_sender("s-a", later + Duration::from_secs(1)));
    }
}

#[test]
fn a_one_shot_client_waits_out_its_refusals_within_its_budget() {
    let mut answers = vec![limited(40), limited(30)].into_iter();
    let mut slept = Vec::new();
    let landed = waiting_out(
        || answers.next().map_or(Ok(9u64), Err),
        Duration::from_millis(100),
        |d| slept.push(d),
    );
    assert_eq!(landed.unwrap(), 9);
    assert_eq!(
        slept,
        [Duration::from_millis(40), Duration::from_millis(30)]
    );

    let mut tries = 0;
    let refused = waiting_out(
        || {
            tries += 1;
            Err::<u64, _>(limited(60))
        },
        Duration::from_millis(100),
        |_| {},
    );
    assert!(wait_of(&refused.unwrap_err()).is_some());
    assert_eq!(tries, 2, "the second wait would pass the budget");

    let mut tries = 0;
    let other = waiting_out(
        || {
            tries += 1;
            Err::<u64, _>(refusal(astream_broker::proto::ERR_REFUSED, "rolled back"))
        },
        Duration::from_secs(10),
        |_| {},
    );
    assert!(other.is_err());
    assert_eq!(tries, 1, "only a rate-limited refusal is waited out");
}
