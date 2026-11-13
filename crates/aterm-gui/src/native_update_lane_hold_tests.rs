// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every hold has a token of its own and a bound none of which is a person's event; a hold
/// that holds on a person is that person's own state now, by its own [`PersonHold`].
#[test]
fn every_hold_is_bounded_by_time_the_program_or_a_persons_state_now() {
    let mut tokens = std::collections::BTreeSet::new();
    for &hold in LaneHold::ALL {
        assert!(tokens.insert(hold.token()), "{hold:?}: a token of its own");
        assert!(!hold.lifts().is_empty(), "{hold:?}");
        match hold.bound() {
            HoldBound::Within(bound) => assert!(
                bound > Duration::ZERO && bound <= Duration::from_secs(60 * 60),
                "{hold:?}: {bound:?}"
            ),
            HoldBound::Program => {}
            HoldBound::PersonNow(person) => assert_eq!(
                person.hold(),
                hold,
                "{hold:?} holds on a person's state that is not its own"
            ),
        }
    }
}

/// A hold is said once when it begins, never per poll, again when it changes, and its end
/// once; each line carries the build, the hold's token, its start, what lifts it and its bound.
#[test]
fn a_hold_is_said_once_per_change_and_once_when_it_ends() {
    let start = Instant::now();
    let at = |s: u64| start + Duration::from_secs(s);
    let since_unix = 1_793_338_376;
    let mut named = NamedHold::default();
    let line = named
        .hold(7, LaneHold::SetupDraft, "a draft", at(0), since_unix)
        .expect("a new hold is said");
    for part in [
        "build 7",
        "a draft",
        "(setup-draft)",
        &aterm_types::rfc3339::format_rfc3339(since_unix),
        LaneHold::SetupDraft.lifts(),
        &LaneHold::SetupDraft.bound_words(),
    ] {
        assert!(line.contains(part), "{line:?} says {part:?}");
    }
    for s in 1..=600 {
        assert_eq!(
            named.hold(7, LaneHold::SetupDraft, "other words", at(s), 1),
            None,
            "poll {s}: the same hold is not said again"
        );
    }
    assert_eq!(named.current().map(|held| held.since), Some(at(0)));
    assert!(
        named
            .hold(7, LaneHold::SystemState, "an Apply", at(601), 2)
            .is_some_and(|line| line.contains("(system-state)")),
        "a change of hold is said"
    );
    let end = named.release(7, at(700)).expect("the end is said");
    assert_eq!(
        end,
        released_line(7, LaneHold::SystemState, Duration::from_secs(99))
    );
    assert_eq!(named.release(7, at(701)), None, "an end is said once");
    assert!(named.current().is_none());
}

/// A hold another line already names is kept for the status and says nothing of itself.
#[test]
fn a_hold_said_elsewhere_is_kept_but_not_said_twice() {
    let now = Instant::now();
    for hold in [
        LaneHold::RestoredAgents,
        LaneHold::RosterConfirmation,
        LaneHold::ActivityRevoked,
    ] {
        let mut named = NamedHold::default();
        assert_eq!(named.hold(7, hold, "words", now, 1), None, "{hold:?}");
        assert_eq!(named.current().map(|held| held.hold), Some(hold));
        assert_eq!(named.release(7, now), None, "{hold:?}");
        assert_eq!(named.current(), None);
    }
}

/// A hold's clock runs from the first attempt that saw its condition while every later attempt
/// sees it again, stops at the first that does not, and starts a new episode after a gap no
/// attempt looked across: an hour with nothing attempting is not an hour of holding.
#[test]
fn a_hold_clock_counts_one_episode_of_attempts_in_a_row() {
    let start = Instant::now();
    let at = |secs: u64| start + Duration::from_secs(secs);
    let bound = Duration::from_secs(90);
    let mut clock = HoldClock::default();
    assert!(
        !clock.overdue(Duration::ZERO, at(0)),
        "never seen, never due"
    );
    clock.observe(true, at(0));
    clock.observe(true, at(60));
    assert!(!clock.overdue(bound, at(89)));
    assert!(clock.overdue(bound, at(90)), "since the first look");
    clock.observe(false, at(91));
    assert!(
        !clock.overdue(bound, at(200)),
        "the condition went, and the clock with it"
    );
    clock.observe(true, at(300));
    assert!(
        !clock.overdue(bound, at(389)),
        "a new episode counts from its own first look"
    );
    clock.observe(
        true,
        at(300) + HoldClock::SAMPLE_GAP + Duration::from_secs(1),
    );
    assert!(
        !clock.overdue(
            bound,
            at(300) + HoldClock::SAMPLE_GAP + Duration::from_secs(2)
        ),
        "a look past the sampling gap starts the episode again"
    );
    let mut stale = HoldClock::default();
    stale.observe(true, at(0));
    stale.observe(true, at(200));
    assert!(stale.overdue(bound, at(200)));
    assert!(
        !stale.overdue(
            bound,
            at(200) + HoldClock::SAMPLE_GAP + Duration::from_secs(1)
        ),
        "a reader long after the last look cannot tell that episode from a new one"
    );
}

/// Each backstop the lane keeps itself is a hold's `Within` bound, so the line that names the
/// hold says the bound the code enforces.
#[test]
fn the_lanes_own_backstops_are_the_holds_stated_bounds() {
    for (hold, bound) in [
        (
            LaneHold::SetupAnswer,
            crate::workstation_setup::SETUP_ANSWER_BACKSTOP,
        ),
        (
            LaneHold::SystemState,
            crate::system_state_host::UNPRESSED_APPLY_SETTLE,
        ),
        (
            LaneHold::PaneExited,
            crate::app_update_handoff::PANE_EXITED_BACKSTOP,
        ),
        (LaneHold::PasswordPrompt, crate::askpass::ASKPASS_DEADLINE),
    ] {
        assert_eq!(hold.bound(), HoldBound::Within(bound), "{hold:?}");
    }
    assert_eq!(
        LaneHold::SetupChoice.bound(),
        HoldBound::PersonNow(PersonHold::SetupChoice),
        "a choice someone made is their unsaved work"
    );
}
