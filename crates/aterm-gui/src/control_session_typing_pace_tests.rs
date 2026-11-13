// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use super::{TypingClock, TypingPace, press_by};

const SETTLE: Duration = Duration::from_millis(150);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn the_key_clock_charges_delivery_and_preserves_a_holds_returned_time() {
    let base = Instant::now();
    let deadline = base + ms(20_000);
    let mut clock = TypingClock::new(ms(400));
    for _ in 0..3 {
        clock.delivered();
    }
    assert_eq!(clock.at(base), base + ms(1_200));
    assert_eq!(clock.native(deadline), deadline - ms(1_200));
    assert_eq!(
        clock.at(clock.native(deadline) + ms(2_000)),
        deadline + ms(2_000)
    );
    let mut native = TypingClock::new(Duration::ZERO);
    native.delivered();
    assert_eq!(native.at(base), base);
    assert_eq!(native.native(deadline), deadline);
}

/// The pace is the keys' own: keys going every 1.5 s in a 20 s turn stop while the next
/// key, the paste and the reserve (2 x 1.5 s + 2.15 s) still fit, not when the cadence's
/// 2 x 70 ms would, and time a typing hold gave back to the deadline is no key's.
#[test]
fn the_pace_is_measured_from_the_keys_less_what_the_holds_gave_back() {
    let t0 = Instant::now();
    // Four keys pressed 1.5 s apart, under `deadline` all along.
    let paced = |deadline: Instant| {
        let mut pace = TypingPace::new(ms(70), ms(20_000), SETTLE, Some(ms(2_000)));
        for key in 0..4u32 {
            pace.pressed(t0 + ms(1_500) * key, deadline);
        }
        pace
    };
    // When the fifth key is due.
    let now = t0 + ms(6_000);
    let need = ms(1_500) * 2 + ms(2_150);
    let roomy = now + need + ms(500);
    assert_eq!(paced(roomy).per_key(4, now, roomy), ms(1_500));
    assert!(paced(roomy).fits(4, 1, now, roomy));
    let tight = now + need - ms(500);
    assert!(
        !paced(tight).fits(4, 1, now, tight),
        "keys at 1.5 s stop with {need:?} left, not at the cadence's 2.29 s"
    );
    // A hold gave 10 s back: the deadline moved, the keys' time did not.
    let deadline = t0 + ms(20_000);
    let at = t0 + ms(16_000);
    assert_eq!(
        paced(deadline).per_key(4, at, deadline + ms(10_000)),
        ms(1_500),
        "6 s of key time over 4 keys, the 10 s the hold gave back left out"
    );
    assert_eq!(
        paced(deadline).per_key(4, at, deadline),
        ms(4_000),
        "the control"
    );
}

/// The next key keeps room for as long a wait as a typing hold has needed so far, so a wait
/// like an earlier one fits before the paste instead of meeting the deadline part-way.
#[test]
fn the_pace_keeps_room_for_the_longest_wait_a_hold_has_needed() {
    let t0 = Instant::now();
    let deadline = t0 + ms(20_000);
    let mut pace = TypingPace::new(ms(70), ms(20_000), SETTLE, Some(ms(2_000)));
    pace.pressed(t0, deadline);
    // 200 keys in 16 s: 80 ms each.
    let now = deadline - ms(4_000);
    assert!(
        pace.fits(200, 1, now, deadline),
        "no hold yet: 4 s is room enough"
    );
    pace.held(ms(3_000));
    pace.held(ms(1_000));
    assert!(
        !pace.fits(200, 1, now, deadline),
        "a 3 s wait does not fit in 4 s"
    );
    assert!(pace.fits(200, 1, deadline - ms(5_500), deadline));
}

/// The reserve is the settle and the submit's window, at most half the turn; a turn whose
/// first key does not fit writes nothing, and huge numbers never overflow into a yes.
#[test]
fn the_reserve_is_at_most_half_the_turn_and_never_overflows() {
    let t0 = Instant::now();
    let pace = TypingPace::new(ms(1), ms(3_000), SETTLE, Some(ms(2_000)));
    let deadline = t0 + ms(3_000);
    assert!(
        pace.fits(0, 1, deadline - ms(1_503), deadline),
        "1.5 s, not 2.15 s"
    );
    assert!(!pace.fits(0, 1, deadline - ms(1_501), deadline));
    let none = TypingPace::new(ms(1), ms(3_000), SETTLE, None);
    assert!(
        none.fits(0, 1, deadline - ms(153), deadline),
        "submit=none keeps the settle"
    );
    let huge = TypingPace::new(Duration::MAX, ms(3_000), SETTLE, Some(ms(2_000)));
    assert!(!huge.fits(0, 1, t0, t0 + ms(3_000_000)));
    assert!(!pace.fits(0, usize::MAX, t0, t0 + ms(3_000_000)));
}

/// A rest that goes as several writes, a key for each Tab between its pastes, keeps room for
/// each of them at the keys' pace: keys at 1.5 s with three writes left need 4 x 1.5 s and
/// the reserve, not the 2 x 1.5 s of one paste. NEGATIVE CONTROL: budgeting one paste
/// whatever the rest, the three writes fit where they do not.
#[test]
fn the_pace_keeps_room_for_each_write_of_the_rest() {
    let t0 = Instant::now();
    let paced = |deadline: Instant| {
        let mut pace = TypingPace::new(ms(70), ms(20_000), SETTLE, Some(ms(2_000)));
        for key in 0..4u32 {
            pace.pressed(t0 + ms(1_500) * key, deadline);
        }
        pace
    };
    let now = t0 + ms(6_000);
    let one = now + ms(1_500) * 2 + ms(2_150) + ms(500);
    assert!(paced(one).fits(4, 1, now, one));
    assert!(
        paced(one).fits(4, 0, now, one),
        "an empty rest still budgets a paste"
    );
    assert!(
        !paced(one).fits(4, 3, now, one),
        "a paste, a Tab and a paste do not fit where one paste does"
    );
    let three = now + ms(1_500) * 4 + ms(2_150) + ms(500);
    assert!(paced(three).fits(4, 3, now, three));
}

/// The counts the pace budgets are the writes the rest makes, from every grapheme's offset.
#[test]
fn the_rest_write_counts_are_the_writes_of_each_rest() {
    use aterm_grapheme::GraphemeClusters;

    use super::{rest_write_counts, rest_writes};
    for text in [
        "",
        "abc",
        "a\tb",
        "\t\t",
        "a\u{1}\tb\u{1}c",
        "\u{1b}\u{1b}",
        "x\r\ny\nz\r",
        "e\u{301}\t\u{1f44d}\u{1f3fd} z\t",
    ] {
        let counts = rest_write_counts(text);
        let offsets: Vec<usize> = text
            .grapheme_indices()
            .map(|(at, _)| at)
            .chain([text.len()])
            .collect();
        assert_eq!(
            counts.iter().map(|&(at, _)| at).collect::<Vec<_>>(),
            offsets,
            "{text:?}"
        );
        for (at, writes) in counts {
            assert_eq!(writes, rest_writes(&text[at..]).len(), "{text:?} from {at}");
        }
    }
}

/// The waits after the last write end with the submit's window still to run, though the
/// program never stops drawing, but never before the echo's settle and never past the
/// deadline.
#[test]
fn the_waits_after_the_typing_leave_the_submit_its_window() {
    let t0 = Instant::now();
    let deadline = t0 + ms(20_000);
    assert_eq!(
        press_by(deadline, t0 + ms(17_000), SETTLE, ms(2_000)),
        t0 + ms(18_000)
    );
    assert_eq!(
        press_by(deadline, t0 + ms(19_000), SETTLE, ms(2_000)),
        t0 + ms(19_150),
        "the settle still runs"
    );
    assert_eq!(
        press_by(deadline, t0 + ms(19_950), SETTLE, ms(2_000)),
        deadline,
        "never past the deadline"
    );
    assert_eq!(
        press_by(deadline, t0 + ms(1_000), SETTLE, ms(0)),
        deadline,
        "submit=none: the deadline"
    );
}
