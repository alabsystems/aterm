// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: Clocks = Clocks {
    since_boot: 300_000,
    wall: 1_790_000_000,
};
const DAY: u64 = 24 * HOUR;
const TIMED: [Choice; 3] = [Choice::Hours1, Choice::Hours8, Choice::Hours24];

#[test]
fn a_click_turns_it_on_forever_and_the_next_one_off() {
    let on = Mode::Off.toggled(NOW);
    assert_eq!(on, Mode::Forever);
    assert_eq!(on.toggled(NOW + 5), Mode::Off);
    let timed = Mode::chosen(Choice::Hours8, NOW);
    assert_eq!(timed.toggled(NOW + 1), Mode::Off);
    // A span past its deadline, before the mug's tick ends it, still shows the mug lit, so a
    // click then turns it off, as it does any lit mug.
    assert_eq!(timed.toggled(NOW + 8 * HOUR), Mode::Off);
    assert_eq!(timed.toggled(NOW + 9 * HOUR), Mode::Off);
    // A hand-edited span of nothing has ended as it starts.
    let nothing = Mode::Until {
        deadline: NOW.since_boot + HOUR,
        span: 0,
    };
    assert_eq!(nothing.settled(NOW), Mode::Off);
    assert_eq!(nothing.toggled(NOW), Mode::Off);
}

#[test]
fn a_choice_sets_a_deadline_its_span_away() {
    for choice in Choice::ALL {
        let mode = Mode::chosen(choice, NOW);
        assert!(mode.is_on(NOW), "{choice:?}");
        assert_eq!(mode.choice(NOW), Some(choice));
        match choice.span() {
            None => assert_eq!(mode, Mode::Forever),
            Some(span) => {
                assert_eq!(
                    mode,
                    Mode::Until {
                        deadline: NOW.since_boot + span,
                        span
                    }
                );
                assert_eq!(mode.remaining(NOW), Some(span));
                assert!(mode.is_on(NOW + (span - 1)));
                assert!(!mode.is_on(NOW + span));
                assert_eq!(mode.settled(NOW + span), Mode::Off);
                assert_eq!(mode.choice(NOW + span), None);
            }
        }
    }
}

#[test]
fn off_and_forever_have_no_time_left() {
    assert_eq!(Mode::Off.remaining(NOW), None);
    assert_eq!(Mode::Forever.remaining(NOW), None);
    let end = Clocks {
        since_boot: u64::MAX,
        wall: u64::MAX,
    };
    assert_eq!(Mode::Forever.settled(end), Mode::Forever);
    assert_eq!(Mode::activate(Some(0), NOW), Mode::Off);
    assert_eq!(
        Mode::activate(Some(HOUR), end),
        Mode::Until {
            deadline: u64::MAX,
            span: HOUR
        }
    );
}

/// A wall clock set back a day mid-span keeps the span to the time it was set for, and its end
/// on the wall clock moves back with the date.
#[test]
fn a_wall_clock_set_back_keeps_no_span_longer_than_it_was_set_for() {
    for choice in TIMED {
        let span = choice.span().expect("timed");
        let mode = Mode::chosen(choice, NOW);
        let back = Clocks {
            since_boot: NOW.since_boot + 600,
            wall: NOW.wall - DAY + 600,
        };
        assert_eq!(mode.remaining(back), Some(span - 600), "{choice:?}");
        assert_eq!(mode.choice(back), Some(choice), "{choice:?}");
        assert_eq!(
            back.wall_at(NOW.since_boot + span),
            NOW.wall - DAY + span,
            "{choice:?}"
        );
        assert!(mode.is_on(back + (span - 601)), "{choice:?}");
        assert!(
            !mode.is_on(back + (span - 600)),
            "{choice:?}: ended on time"
        );
        assert_eq!(mode.settled(back + (span - 600)), Mode::Off, "{choice:?}");
    }
}

/// The wall clock set forward a day ten minutes into a span ends nothing early.
#[test]
fn a_wall_clock_set_forward_ends_no_span_early() {
    for choice in TIMED {
        let span = choice.span().expect("timed");
        let mode = Mode::chosen(choice, NOW);
        let ahead = Clocks {
            since_boot: NOW.since_boot + 600,
            wall: NOW.wall + DAY + 600,
        };
        assert!(mode.is_on(ahead), "{choice:?}");
        assert_eq!(mode.remaining(ahead), Some(span - 600), "{choice:?}");
        assert_eq!(mode.choice(ahead), Some(choice), "{choice:?}");
        assert_eq!(
            mode.toggled(ahead),
            Mode::Off,
            "{choice:?}: a click turns it off"
        );
    }
}

/// Time asleep counts, as the boot clock counts it: a span a sleep outlasts has ended at the
/// wake, and one that outlasts the sleep has that much less left.
#[test]
fn time_asleep_counts_toward_a_span() {
    let woke = NOW + 2 * HOUR;
    assert_eq!(Mode::chosen(Choice::Hours1, NOW).settled(woke), Mode::Off);
    assert!(!Mode::chosen(Choice::Hours1, NOW).is_on(woke));
    assert_eq!(
        Mode::chosen(Choice::Hours8, NOW).remaining(woke),
        Some(6 * HOUR)
    );
}

/// However its deadline came to be, a timed mode never has more than its span left, and
/// settles to that; settled, it ends a span later.
#[test]
fn no_timed_mode_has_more_than_its_span_left() {
    let far = Mode::Until {
        deadline: NOW.since_boot + DAY,
        span: HOUR,
    };
    assert!(far.is_on(NOW));
    assert_eq!(far.remaining(NOW), Some(HOUR));
    assert_eq!(far.choice(NOW), Some(Choice::Hours1));
    let settled = far.settled(NOW);
    assert_eq!(settled, Mode::chosen(Choice::Hours1, NOW));
    assert_eq!(settled.settled(NOW + HOUR), Mode::Off);
    let near = Mode::chosen(Choice::Hours8, NOW);
    assert_eq!(
        near.settled(NOW + 60),
        near,
        "a deadline its span away is kept"
    );
}

#[test]
fn a_span_no_menu_item_names_checks_none() {
    let mode = Mode::activate(Some(90 * 60), NOW);
    assert!(mode.is_on(NOW));
    assert_eq!(mode.choice(NOW), None);
}

#[test]
fn span_words_parse() {
    assert_eq!(parse_span("forever"), Ok(None));
    for choice in Choice::ALL {
        assert_eq!(parse_span(choice.word()), Ok(choice.span()), "{choice:?}");
    }
    assert_eq!(parse_span("90m"), Ok(Some(90 * 60)));
    assert_eq!(parse_span("2h"), Ok(Some(2 * HOUR)));
    assert_eq!(parse_span("8760h"), Ok(Some(MAX_SPAN)));
    for bad in [
        "",
        "h",
        "m",
        "1",
        "1s",
        "-1h",
        "+1h",
        "1.5h",
        "0h",
        "0m",
        "8761h",
        "1 h",
        "forever!",
        "99999999999999999999h",
    ] {
        assert!(parse_span(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn time_left_reads_as_hours_and_minutes() {
    assert_eq!(describe_left(0), "less than a minute");
    assert_eq!(describe_left(59), "less than a minute");
    assert_eq!(describe_left(60), "1 min");
    assert_eq!(describe_left(52 * 60 + 30), "52 min");
    assert_eq!(describe_left(HOUR), "1 h");
    assert_eq!(describe_left(7 * HOUR + 12 * 60 + 59), "7 h 12 min");
    assert_eq!(describe_left(24 * HOUR), "24 h");
}

/// Every reading of the machine, with and without the lid option, and the hold each one
/// gets for a mode that is on; an off mode holds nothing on any.
fn every_machine() -> impl Iterator<Item = Machine> {
    (0..8u8).map(|bits| Machine {
        lid_closed: bits & 1 != 0,
        dark_wake: bits & 2 != 0,
        going_to_sleep: bits & 4 != 0,
    })
}

/// By default a closed lid holds nothing, the mode staying on, and an open lid in a full
/// wake holds the activity alone: no reading holds the system assertion.
#[test]
fn by_default_a_closed_lid_holds_nothing() {
    for machine in every_machine() {
        let held = hold(true, false, machine);
        assert!(!held.system(), "{machine:?}");
        let want = if machine.going_to_sleep || machine.dark_wake {
            Hold::Paused(Pause::Asleep)
        } else if machine.lid_closed {
            Hold::Paused(Pause::LidClosed)
        } else {
            Hold::Idle
        };
        assert_eq!(held, want, "{machine:?}");
        assert_eq!(held.activity(), want == Hold::Idle, "{machine:?}");
    }
}

/// The opt-in holds both assertions, lid open or closed, and lets go of both once the
/// system announces its sleep or the Mac is in a dark wake: a lid-closed Mac on battery,
/// which macOS slept despite `PreventSystemSleep`, is not held through its dark wakes.
/// NEGATIVE CONTROL: `hold` with its `machine.dark_wake` term removed answers
/// `LidClosedToo` for a dark wake, which the first assertion refuses.
#[test]
fn the_opt_in_holds_both_until_macos_sleeps_anyway() {
    for machine in every_machine() {
        let held = hold(true, true, machine);
        if machine.going_to_sleep || machine.dark_wake {
            assert_eq!(held, Hold::Paused(Pause::Asleep), "{machine:?}");
            assert!(!held.activity() && !held.system());
        } else {
            assert_eq!(held, Hold::LidClosedToo, "{machine:?}");
            assert!(held.activity() && held.system());
        }
    }
}

/// An off mode holds nothing, whatever the lid and the option.
#[test]
fn an_off_mode_holds_nothing_anywhere() {
    for machine in every_machine() {
        for lid_awake in [false, true] {
            assert_eq!(hold(false, lid_awake, machine), Hold::Off, "{machine:?}");
        }
    }
}

/// The lid's close pauses the hold and its open resumes it while the mode is still on: the
/// pause is no change of the mode, and a span that ended during it holds nothing after.
/// NEGATIVE CONTROL: `hold` with its `machine.lid_closed` arm removed answers `Idle` for
/// the closed lid, which the first assertion refuses.
#[test]
fn a_closed_lid_pauses_the_mode_and_an_open_one_resumes_it() {
    let closed = Machine {
        lid_closed: true,
        ..Machine::default()
    };
    let open = Machine::default();
    let mode = Mode::chosen(Choice::Hours8, NOW);
    let during = NOW + HOUR;
    assert_eq!(
        hold(mode.is_on(during), false, closed),
        Hold::Paused(Pause::LidClosed)
    );
    assert_eq!(
        mode.settled(during),
        mode,
        "the pause leaves the mode as it was"
    );
    assert_eq!(hold(mode.is_on(during + 60), false, open), Hold::Idle);
    let after = NOW + 9 * HOUR;
    assert_eq!(hold(mode.is_on(after), false, open), Hold::Off);
    assert_eq!(hold(Mode::Forever.is_on(after), false, open), Hold::Idle);
}

/// A pause within [`SETTLE_SECS`] of a wake is settling, whichever pause it is; a hold, no
/// wake, a wake that long ago, and a wake read after a clock that reads before it are not.
/// NEGATIVE CONTROL: `settling` answering `false` for every pause leaves the lid's open
/// unheld until the next tick, which the first assertion refuses.
#[test]
fn a_pause_just_after_a_wake_is_settling() {
    let woke = NOW.since_boot;
    for pause in [Pause::Asleep, Pause::LidClosed] {
        let paused = Hold::Paused(pause);
        assert!(settling(paused, Some(woke), woke), "{pause:?}");
        assert!(settling(paused, Some(woke), woke + SETTLE_SECS - 1));
        assert!(!settling(paused, Some(woke), woke + SETTLE_SECS));
        assert!(!settling(paused, None, woke));
        assert!(!settling(paused, Some(woke), woke - 1));
    }
    for held in [Hold::Off, Hold::Idle, Hold::LidClosedToo] {
        assert!(!settling(held, Some(woke), woke), "{held:?}");
    }
}

#[test]
fn every_choice_has_its_own_label_word_and_span() {
    for (i, a) in Choice::ALL.iter().enumerate() {
        assert_eq!(Choice::from_span(a.span()), Some(*a));
        for b in &Choice::ALL[i + 1..] {
            assert_ne!(a.label(), b.label());
            assert_ne!(a.word(), b.word());
            assert_ne!(a.span(), b.span());
        }
    }
}
