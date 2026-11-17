// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One refill or raise opens one barrier whatever order the `/usage` panel's figures and the
//! usage tap's arrive in ([`note_panel_sample`], [`note_reading_held`]).

use super::*;

const NOW: i64 = 1_790_000_000;
const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
/// The week every figure here is of.
const RESET: i64 = NOW + 3 * DAY;

fn state() -> State {
    State {
        machine: "0a0b".to_owned(),
        ..State::default()
    }
}

/// A tap reading of the week, served at `at` by a process begun at `born`.
fn tap(pct_x10: u32, refilled: Option<u32>, at: i64, born: i64, seen_before: bool) -> TapRead {
    TapRead {
        seven_day: Some(WindowRead {
            pct_x10,
            reset: RESET,
            refilled,
        }),
        at,
        seen_before,
        born: Some(born),
        ..TapRead::default()
    }
}

/// A panel's figure of the week, recorded at `at`.
fn panel(week_pct: u32, reset: i64, at: i64) -> Sample {
    Sample {
        week_pct: Some(week_pct),
        week_reset: Some(reset),
        at,
        ..Sample::default()
    }
}

fn resets(events: &[Event]) -> usize {
    events.iter().filter(|e| e.kind == Kind::Reset).count()
}

/// The week refilled or raised: a process begun two hours ago read it 99% an hour ago, and one
/// begun after that reads it 69% now, which opens its barrier at `NOW`. Returns the state and how
/// many barriers the readings opened.
fn refilled() -> (State, usize) {
    let mut s = state();
    let before = note_reading(
        &mut s,
        "main",
        &tap(990, None, NOW - HOUR, NOW - 2 * HOUR, true),
        NOW - HOUR,
    );
    let after = note_reading(
        &mut s,
        "main",
        &tap(690, None, NOW, NOW - 30 * MIN, false),
        NOW,
    );
    (s, resets(&before) + resets(&after))
}

/// A limit reset spent at `NOW` cut the week's 99%, which a tap read an hour before
/// ([`reset_outcome`]'s barrier, dated after its figure).
fn spent() -> State {
    let mut s = state();
    note_reading(
        &mut s,
        "main",
        &tap(990, None, NOW - HOUR, NOW - 2 * HOUR, true),
        NOW - HOUR,
    );
    let fig = s
        .accounts
        .get_mut("main")
        .and_then(|a| a.windows.get_mut(WEEK_LIMIT))
        .expect("the held figure");
    fig.epoch_at = Some(NOW);
    assert!(fig.cut());
    s
}

fn week_of(s: &State) -> (Option<u32>, Option<i64>, Option<i64>) {
    let sample = s.accounts["main"].sample.as_ref().expect("a sample");
    (sample.week_pct, sample.week_reset, sample.week_read_at)
}

/// A PANEL NOT SHOWN SERVED AFTER A REFILL IS HELD BACK. After the barrier at `NOW`, a screen
/// read (no served bound) records a panel at +10 min that says 99% of the same week: it may be one
/// Claude Code drew before the refill, so the sample keeps the barrier's 69% and the ledger row
/// says the panel's figure was held back; the row stays refilled. An older process that read 99%
/// before the refill is served again, and a process begun after the panel reads the week: neither
/// opens a second barrier. The panel's reset may be the minute before the tap's, as Claude Code
/// draws it ([`PANEL_RESET_SLACK_S`]).
/// CONTROLS: a panel a probe claimed after the barrier is the refilled window's, and its 99% is
/// taken (the row is low on it); one claimed before the barrier and read after it is held back;
/// and a panel naming a reset past the slack is another window's, which no barrier holds back.
#[test]
fn a_panel_not_shown_served_after_a_refill_is_held_back_and_opens_no_second_barrier() {
    for (why, reset) in [
        ("the tap's reset", RESET),
        (
            "the minute before it, as the panel draws it",
            RESET - PANEL_RESET_SLACK_S,
        ),
    ] {
        let (mut s, opened) = refilled();
        assert_eq!(opened, 1, "{why}");
        let recorded = NOW + 10 * MIN;
        let events = note_panel_sample(&mut s, "main", panel(99, reset, recorded), None, recorded);
        assert_eq!(events.len(), 1, "{why}: {events:?}");
        assert!(
            events[0]
                .why
                .contains("week 69% (the panel's 99% held back: it may predate"),
            "{why}: {}",
            events[0].why
        );
        assert_eq!(week_of(&s), (Some(69), Some(RESET), Some(NOW)), "{why}");
        assert_eq!(
            s.accounts["main"].low_until(recorded),
            None,
            "{why}: the refilled row is not low again"
        );
        let older = NOW + 20 * MIN;
        let again = note_reading(
            &mut s,
            "main",
            &tap(700, Some(990), older, NOW - 2 * HOUR, true),
            older,
        );
        let newer = NOW + 30 * MIN;
        let later = note_reading(
            &mut s,
            "main",
            &tap(705, None, newer, older + MIN, false),
            newer,
        );
        assert_eq!(
            resets(&again) + resets(&later),
            0,
            "{why}: one refill, one barrier: {again:?} {later:?}"
        );
        let fig = &s.accounts["main"].windows[WEEK_LIMIT];
        assert_eq!((fig.pct_x10, fig.epoch_at), (705, Some(NOW)), "{why}");
        assert_eq!(s.accounts["main"].low_until(newer), None, "{why}");
    }

    let recorded = NOW + 10 * MIN;
    let (mut s, _) = refilled();
    note_panel_read(
        &mut s,
        "main",
        &panel(99, RESET, recorded),
        NOW + MIN,
        recorded,
    );
    assert_eq!(
        week_of(&s),
        (Some(99), Some(RESET), Some(recorded)),
        "CONTROL: a probe claimed after the barrier"
    );
    assert!(
        s.accounts["main"].low_until(recorded).is_some(),
        "CONTROL: use grew after the refill"
    );

    let (mut s, _) = refilled();
    note_panel_read(
        &mut s,
        "main",
        &panel(99, RESET, recorded),
        NOW - MIN,
        recorded,
    );
    assert_eq!(
        week_of(&s),
        (Some(69), Some(RESET), Some(NOW)),
        "CONTROL: a probe claimed before the barrier"
    );

    let (mut s, _) = refilled();
    let beyond = RESET - PANEL_RESET_SLACK_S - 1;
    note_panel_sample(&mut s, "main", panel(99, beyond, recorded), None, recorded);
    assert_eq!(
        week_of(&s),
        (Some(99), Some(beyond), Some(recorded)),
        "CONTROL: a reset past the slack names another window"
    );
}

/// A LIMIT RESET SPENT, THEN A PANEL OF THE SPENT WINDOW. The week's held 99% was cut by a limit
/// reset spent at `NOW` ([`reset_outcome`]'s barrier, dated after its figure). A screen read at
/// +10 min records a panel that says 100%: it is held back, and with no live figure the sample
/// holds none of the week, so the row is not low; a process begun after the panel reads the week
/// half used and opens no second barrier on top of the spent reset.
/// CONTROL: the same panel served after the reset (a probe claimed after it) is taken.
#[test]
fn a_panel_of_a_spent_reset_s_window_is_held_back() {
    let recorded = NOW + 10 * MIN;
    let mut s = spent();
    let events = note_panel_sample(&mut s, "main", panel(100, RESET, recorded), None, recorded);
    assert!(
        events[0].why.contains("week – (the panel's 100% held back"),
        "{}",
        events[0].why
    );
    assert_eq!(week_of(&s), (None, None, None));
    assert_eq!(s.accounts["main"].low_until(recorded), None);
    let read = NOW + 20 * MIN;
    let events = note_reading(
        &mut s,
        "main",
        &tap(500, None, read, recorded + MIN, false),
        read,
    );
    assert_eq!(resets(&events), 0, "{events:?}");
    assert_eq!(
        s.accounts["main"].windows[WEEK_LIMIT].epoch_at,
        Some(NOW),
        "the spent reset's barrier stands alone"
    );

    let mut s = spent();
    note_panel_read(
        &mut s,
        "main",
        &panel(100, RESET, recorded),
        NOW + MIN,
        recorded,
    );
    assert_eq!(
        week_of(&s),
        (Some(100), Some(RESET), Some(recorded)),
        "CONTROL: a probe claimed after the reset"
    );
}

/// THE ORDERS THAT WERE ALREADY RIGHT, PINNED: a panel a probe read after the barrier opens no
/// second one, and a panel first, then a process begun after it reading the week clearly less
/// used, opens exactly one. (Two processes and no panel: `every_session_that_sees_one_refill_
/// records_it_once`; a held figure another machine dated opens none:
/// `a_new_process_reading_clearly_less_of_the_same_week_opens_its_epoch`.)
#[test]
fn a_panel_before_or_after_the_tap_opens_one_barrier() {
    let (mut s, opened) = refilled();
    let probed = NOW + 10 * MIN;
    note_panel_read(&mut s, "main", &panel(70, RESET, probed), NOW + MIN, probed);
    let read = NOW + 20 * MIN;
    let events = note_reading(
        &mut s,
        "main",
        &tap(710, None, read, probed + MIN, false),
        read,
    );
    assert_eq!(opened + resets(&events), 1, "{events:?}");
    assert_eq!(
        s.accounts["main"].windows[WEEK_LIMIT].epoch_at,
        Some(NOW),
        "the panel after the barrier"
    );

    let mut s = state();
    let shown = NOW - HOUR;
    note_panel_sample(&mut s, "main", panel(99, RESET, shown), None, shown);
    let events = note_reading(
        &mut s,
        "main",
        &tap(690, None, NOW, NOW - 30 * MIN, false),
        NOW,
    );
    assert_eq!(resets(&events), 1, "{events:?}");
    let fig = &s.accounts["main"].windows[WEEK_LIMIT];
    assert_eq!(
        (fig.pct_x10, fig.epoch_at),
        (690, Some(NOW)),
        "the panel first"
    );
    assert_eq!(s.accounts["main"].low_until(NOW), None);
}

/// A PANEL SERVED AFTER THE HELD FIGURE THAT READS THE WINDOW CLEARLY LESS USED OPENS ITS
/// BARRIER. A tap read the week 99% an hour ago; the accounts probe, claimed half an hour ago,
/// reads it 69%: the panel was served after the held figure, so the week was refilled or raised
/// between them, and the probe opens the barrier at its record time (one `Reset`), its figure the
/// held one. A tab whose process began before the 99% then reads the week 69.5% for the first
/// time (no order to show: held back, as before the barrier), and a reading that carries no week
/// rebuilds the sample: the row stays refilled, not low, and no second barrier opens.
/// CONTROLS: a screen read of the same panel (no served bound) opens none, and a probe's panel
/// less than [`REFILL_UNSEEN_MIN_X10`] lower opens none.
/// Negative control: without the panel's barrier (no `opens`) the probe books no `Reset`, and the
/// older process's reading would merge against the held 99% and rebuild the sample at 99: the
/// first assertion fails.
#[test]
fn a_panel_served_after_the_held_figure_reading_it_clearly_lower_opens_the_barrier() {
    let base = || {
        let mut s = state();
        note_reading(
            &mut s,
            "main",
            &tap(990, None, NOW - HOUR, NOW - 2 * HOUR, true),
            NOW - HOUR,
        );
        s
    };
    let claimed = NOW - 30 * MIN;
    let probed = NOW - 25 * MIN;
    let mut s = base();
    let mut opened = resets(&note_panel_read(
        &mut s,
        "main",
        &panel(69, RESET, probed),
        claimed,
        probed,
    ));
    assert_eq!(opened, 1);
    let fig = &s.accounts["main"].windows[WEEK_LIMIT];
    assert_eq!(
        (fig.pct_x10, fig.reset, fig.epoch_at),
        (690, RESET, Some(probed))
    );
    assert_eq!(s.accounts["main"].low_until(probed), None);
    // A tab begun before the 99%, read for the first time.
    opened += resets(&note_reading(
        &mut s,
        "main",
        &tap(695, None, NOW, NOW - 3 * HOUR, false),
        NOW,
    ));
    assert_eq!(week_of(&s), (Some(69), Some(RESET), Some(probed)));
    assert_eq!(s.accounts["main"].low_until(NOW), None, "not low again");
    // A reading that carries no week (2.1.316 leaves out a window the API did not report).
    let later = NOW + 20 * MIN;
    opened += resets(&note_reading(
        &mut s,
        "main",
        &TapRead {
            five_hour: Some(WindowRead {
                pct_x10: 100,
                reset: NOW + 2 * HOUR,
                refilled: None,
            }),
            at: later,
            seen_before: true,
            born: Some(NOW - 3 * HOUR),
            ..TapRead::default()
        },
        later,
    ));
    assert_eq!(week_of(&s).0, Some(69));
    assert_eq!(s.accounts["main"].low_until(later), None);
    assert_eq!(opened, 1, "one refill, one barrier");

    let mut s = base();
    let events = note_panel_sample(&mut s, "main", panel(69, RESET, probed), None, probed);
    assert_eq!(resets(&events), 0, "CONTROL: a screen read shows no order");
    assert_eq!(s.accounts["main"].windows[WEEK_LIMIT].pct_x10, 990);

    let mut s = base();
    let events = note_panel_read(&mut s, "main", &panel(95, RESET, probed), claimed, probed);
    assert_eq!(
        resets(&events),
        0,
        "CONTROL: four points lower is no refill"
    );
    assert_eq!(s.accounts["main"].windows[WEEK_LIMIT].epoch_at, None);
}

/// A SCREEN READ OF THE HELD FIGURE'S OWN WHOLE DOES NOT DATE IT LATER. A tap read the week 99%
/// two hours ago; half an hour after it, a screen read records a panel of the same week at 99%,
/// which may have been drawn at any time before, so the sample keeps the tap's instant for it; a
/// process begun ten minutes after the tap's reading then reads the week 69%: it began after the
/// 99% was served, so it opens the barrier (once), and the row is not low.
/// CONTROL: a probe's panel of the same whole is dated by its record time, as before.
/// Negative control: dating the screen read's 99% at its record time (which makes the held figure
/// newer than the process, so it shows no order and opens no barrier) fails the first assertion.
#[test]
fn a_screen_read_of_the_held_whole_keeps_the_held_figure_s_date() {
    let read = NOW - 2 * HOUR;
    let base = || {
        let mut s = state();
        note_reading(
            &mut s,
            "main",
            &tap(990, None, read, read - HOUR, true),
            read,
        );
        s
    };
    let shown = read + 30 * MIN;
    let mut s = base();
    note_panel_sample(&mut s, "main", panel(99, RESET, shown), None, shown);
    assert_eq!(week_of(&s), (Some(99), Some(RESET), Some(read)));
    let events = note_reading(
        &mut s,
        "main",
        &tap(690, None, NOW, read + 10 * MIN, false),
        NOW,
    );
    assert_eq!(resets(&events), 1, "{events:?}");
    assert_eq!(s.accounts["main"].low_until(NOW), None);

    let mut s = base();
    note_panel_read(&mut s, "main", &panel(99, RESET, shown), shown - MIN, shown);
    assert_eq!(
        week_of(&s),
        (Some(99), Some(RESET), Some(shown)),
        "CONTROL: a probe's panel"
    );
}

/// A SCREEN READ LONG AFTER A REFILL IS TAKEN AS READ. A screen read's panel is held back for
/// [`SCREEN_PANEL_HOLD_S`] after the barrier and no longer: one recorded three days after the
/// refill that says 99% of the same week is the week used up since (on other Macs, say), and it
/// is the sample, so the row turns low; one recorded a second inside the bound is held back.
/// Negative control: holding a screen read back to the window's reset keeps 69% three days on,
/// and the first assertion fails.
#[test]
fn a_screen_read_long_after_a_refill_is_taken() {
    let (mut s, _) = refilled();
    let days = NOW + 2 * DAY + 20 * HOUR;
    note_panel_sample(&mut s, "main", panel(99, RESET, days), None, days);
    assert_eq!(week_of(&s), (Some(99), Some(RESET), Some(days)));
    assert!(s.accounts["main"].low_until(days).is_some());

    let (mut s, _) = refilled();
    let inside = NOW + SCREEN_PANEL_HOLD_S - 1;
    note_panel_sample(&mut s, "main", panel(99, RESET, inside), None, inside);
    assert_eq!(week_of(&s), (Some(69), Some(RESET), Some(NOW)));
    let (mut s, _) = refilled();
    let at = NOW + SCREEN_PANEL_HOLD_S;
    note_panel_sample(&mut s, "main", panel(99, RESET, at), None, at);
    assert_eq!(week_of(&s).0, Some(99), "the bound itself is past the hold");
}

/// A PROVIDER'S READ IS SERVED AT ITS OWN INSTANT ([`note_sample`]). After a limit reset spent at
/// `NOW` cut the week's 99%, a provider's answer served a minute before the reset (and recorded
/// after it) says 100%: it is held back, and the row is not low; one served a minute after the
/// reset is the spent window's own, and taken. After a tap's refill barrier, a provider's 99%
/// served a minute after it is taken at once, where a screen read's would be held back.
/// Negative controls: recording a provider's read as a screen read (no served bound) holds back
/// the reads served after the barrier; taking every provider read as read, whatever its serve
/// time, takes the first.
#[test]
fn a_provider_s_read_is_served_at_its_own_instant() {
    let after = NOW + MIN;
    let mut s = spent();
    let events = note_sample(&mut s, "main", panel(100, RESET, NOW - MIN), after);
    assert!(
        events[0].why.contains("week – (the panel's 100% held back"),
        "{}",
        events[0].why
    );
    assert_eq!(week_of(&s), (None, None, None));
    assert_eq!(s.accounts["main"].low_until(after), None);
    let mut s = spent();
    note_sample(&mut s, "main", panel(100, RESET, after), after);
    assert_eq!(week_of(&s), (Some(100), Some(RESET), Some(after)));

    let (mut s, _) = refilled();
    note_sample(&mut s, "main", panel(99, RESET, after), after);
    assert_eq!(week_of(&s), (Some(99), Some(RESET), Some(after)));
    assert!(s.accounts["main"].low_until(after).is_some());
}

/// A PANEL ROW NAMING NO RESET IS HELD BACK UNDER A BARRIER: a reset Claude Code printed that does
/// not parse, or one past [`RESET_BELIEVED_S`], leaves the row's reset unnamed, and such a row
/// can be of the refilled window. After the tap's barrier, a screen read of the week at 99% with
/// no reset is held back with the held figure's reset in its place, and the row stays refilled.
/// CONTROL: the same panel with no barrier on the held figure is taken as read, its reset
/// unnamed. Negative control: holding back only a row that names its window's reset takes the
/// 99%, and the first assertion fails.
#[test]
fn a_panel_row_naming_no_reset_is_held_back_under_a_barrier() {
    let unnamed = |at| Sample {
        week_pct: Some(99),
        week_reset: None,
        at,
        ..Sample::default()
    };
    let recorded = NOW + 10 * MIN;
    let (mut s, _) = refilled();
    note_panel_sample(&mut s, "main", unnamed(recorded), None, recorded);
    assert_eq!(week_of(&s), (Some(69), Some(RESET), Some(NOW)));
    assert_eq!(s.accounts["main"].low_until(recorded), None);

    let mut s = state();
    note_reading(&mut s, "main", &tap(690, None, NOW, NOW - HOUR, true), NOW);
    note_panel_sample(&mut s, "main", unnamed(recorded), None, recorded);
    assert_eq!(
        week_of(&s),
        (Some(99), None, Some(recorded)),
        "CONTROL: no barrier"
    );
}
