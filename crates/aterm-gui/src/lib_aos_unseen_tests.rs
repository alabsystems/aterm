// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-path tests for the person's grant to record keys aterm cannot see echoed
//! (ATERM-KI-1025-1's D12): only a person's yes in aterm's alert grants it, for one attach; under it
//! a held key no frame proves is logged unseen at the frame that settles it, and the recording says
//! so wherever it goes; a mask in a held key's cells, a no-echo key and Secure Keyboard Entry still
//! stop the recording.
//!
//! The fixture's tty mode is unreadable and no prompt of aterm's shell is open, so every typed key
//! is a raw one; the test's `FRONT_GROUP` stands in for the process group that leads the tab.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_effects::aos_momentum::{KeyEcho, MomentumCode, MomentumEvent, RAW_KEY};
use aterm_session::sink::SinkWriter;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

use super::aos_repair_tests::{
    WID, detached_for, exported_log, glyph, leave_prompt, momentum_app, momentum_app_on, press,
    status,
};
use crate::App;
use crate::aos_host::echo::{ECHO_PROOF_BOUND, held_keys, hold_is_granted};
use crate::aos_host::{
    FRONT_GROUP, GRANT_OUTLIVES_ATTACH, SECURE_ENTRY, UNSEEN_DECLINED, UNSEEN_HELD,
    UNSEEN_NO_GROUP, UNSEEN_NOT_RECORDING, UNSEEN_PERSON_ONLY,
};
use crate::input::InputEvent;
use crate::native_settings::{ConsentGestures, OwnerAnswer};
use crate::scripted_redraw_fixture::{feed, scripted_app_with_sink};

/// The process group the fixture's program runs in, and another program's.
const GROUP: i32 = 100;
const OTHER: i32 = 200;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// `d` in nanoseconds, a log instant's unit.
fn ns(d: Duration) -> i128 {
    i128::try_from(d.as_nanos()).expect("small")
}

/// `app` at a program that reads raw (no prompt in front, its tty mode unreadable), in front of
/// its tab as [`GROUP`], with a recording attached at `t0`; on the alternate screen when `alt`.
fn raw_on(app: App, t0: Instant, alt: bool) -> App {
    FRONT_GROUP.with(|g| g.set(Some(GROUP)));
    let mut app = app;
    app.redraw_window_at(WID, t0);
    leave_prompt(&app);
    if alt {
        feed(&app, b"\x1b[?1049h");
    }
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    app
}

/// [`raw_on`] over the momentum fixture, no grant given.
fn ungranted(t0: Instant) -> App {
    raw_on(momentum_app(false), t0, false)
}

/// [`ungranted`] with the person's grant for its attach.
fn granted(t0: Instant) -> App {
    let mut app = ungranted(t0);
    app.aos_grant_for_test(WID).expect("granted");
    assert!(app.aos_unseen_on(WID), "fixture: the grant is live");
    app
}

/// The person types `c` at `at`.
fn key(app: &mut App, c: char, at: Instant) {
    press(app, glyph(c), at);
}

/// Shift-Enter: on the alternate screen, a new line typed into a composer, which carries no text.
fn shift_enter() -> InputEvent {
    InputEvent::Key {
        key: Key::Named(NamedKey::Enter),
        mods: Modifiers::SHIFT,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

/// The keys the live recording logged since `epoch`: each one's instant, in nanoseconds after it,
/// and whether it was seen.
fn logged(app: &App, epoch: Instant) -> Vec<(i128, KeyEcho)> {
    exported_log(app, epoch)
        .iter()
        .filter_map(|e| match *e {
            MomentumEvent::Key { at, echo, .. } => Some((at, echo)),
            _ => None,
        })
        .collect()
}

/// Whether `app`'s timeline lists a row whose words are `words`.
fn row(app: &App, words: &str) -> bool {
    app.aos_ls_now()
        .iter()
        .any(|l| l.starts_with("slot=0 row=") && l.ends_with(&format!(" {words}")))
}

/// Whether the recording stopped for an unproven run, the grant with it.
fn stopped_raw(app: &App) -> bool {
    !status(app).attached
        && detached_for(app, &RAW_KEY.replace(' ', "_"))
        && !app.aos_holds_grant_for_test(WID)
}

fn held(app: &App) -> usize {
    held_keys(&app.windows[&WID].aos)
}

fn granted_hold(app: &App) -> bool {
    hold_is_granted(&app.windows[&WID].aos)
}

#[test]
fn a_granted_recording_logs_an_unechoed_run_unseen_at_the_frame_past_its_bound() {
    let t0 = Instant::now();
    let mut app = granted(t0);
    key(&mut app, 'p', t0 + ms(10));
    key(&mut app, 'w', t0 + ms(20));
    app.redraw_window_at(WID, t0 + ms(500));
    assert_eq!(held(&app), 2, "nothing proven, the bound not passed");
    assert!(granted_hold(&app));
    assert!(logged(&app, t0).is_empty(), "nothing is logged while held");
    let settled = t0 + ms(10) + ECHO_PROOF_BOUND;
    app.redraw_window_at(WID, settled);
    assert_eq!(
        logged(&app, t0),
        [(ns(settled - t0), KeyEcho::Unseen); 2],
        "both keys, unseen, at the one frame that settled them"
    );
    assert!(status(&app).attached, "the recording runs on");
    assert!(app.aos_unseen_on(WID), "and so does the grant");
    assert!(
        app.windows[&WID].fx.cursor().momentum.aos_tainted(),
        "the next attach starts the glow from rest"
    );

    let mut app = ungranted(t0);
    key(&mut app, 'p', t0 + ms(10));
    key(&mut app, 'w', t0 + ms(20));
    app.redraw_window_at(WID, settled);
    assert!(
        stopped_raw(&app),
        "negative control: without the grant the same keys stop the recording"
    );
}

#[test]
fn a_mask_in_a_held_keys_cells_stops_a_granted_recording_wherever_the_cursor_is() {
    let t0 = Instant::now();
    for (what, shown, stops) in [
        ("masks in place", &b"**"[..], true),
        ("masks, the cursor parked elsewhere", b"**\x1b[6;1H", true),
        ("a bullet for the second key", "p\u{2022}".as_bytes(), true),
        ("the cursor parked elsewhere, no mask", b"\x1b[6;1H", false),
    ] {
        let mut app = granted(t0);
        key(&mut app, 'p', t0 + ms(10));
        key(&mut app, 'w', t0 + ms(20));
        feed(&app, shown);
        app.redraw_window_at(WID, t0 + ms(30));
        if stops {
            assert!(stopped_raw(&app), "{what}");
            continue;
        }
        assert_eq!(held(&app), 2, "negative control, {what}: still held");
        let settled = t0 + ms(10) + ECHO_PROOF_BOUND;
        app.redraw_window_at(WID, settled);
        assert_eq!(
            logged(&app, t0),
            [(ns(settled - t0), KeyEcho::Unseen); 2],
            "negative control, {what}: logged unseen at the bound"
        );
    }
}

#[test]
fn a_masked_cell_bearing_key_that_cannot_be_placed_still_stops_under_the_grant() {
    let t0 = Instant::now();
    // A deferred wrap: the field's masks fill the row and the cursor waits at the margin.
    let mut app = granted(t0);
    let cols = app
        .front_terminal(WID)
        .map(|f| crate::term_lock(&f.term).cols())
        .expect("a terminal");
    feed(&app, &vec![b'*'; usize::from(cols)]);
    key(&mut app, 'p', t0 + ms(10));
    assert!(stopped_raw(&app), "a deferred wrap");

    // One key past the hold's columns: every key before it waits, masked, and it stops the
    // recording at its press.
    let mut app = granted(t0);
    for i in 0..32u64 {
        key(&mut app, 'p', t0 + ms(10 + i));
        feed(&app, b"*");
    }
    assert_eq!(held(&app), 32, "the hold's columns are full");
    key(&mut app, 'p', t0 + ms(50));
    assert!(stopped_raw(&app), "one key past the cap");

    // Over its own glyph: held with its cells, so a mask there still refutes it.
    let mut app = granted(t0);
    feed(&app, b"p\x1b[D");
    key(&mut app, 'p', t0 + ms(10));
    assert!(
        granted_hold(&app),
        "a key over its own glyph is held with its cells"
    );
    feed(&app, b"*");
    app.redraw_window_at(WID, t0 + ms(20));
    assert!(stopped_raw(&app), "and the mask over it refutes it");

    // Negative controls: the same presses with no mask. Where a key can be placed it is held with
    // its cells and logged unseen at the bound; where it cannot, it stops the recording all the
    // same, so the stop is the placement's.
    let mut app = granted(t0);
    feed(&app, b"p\x1b[D");
    key(&mut app, 'p', t0 + ms(10));
    let settled = t0 + ms(10) + ECHO_PROOF_BOUND;
    app.redraw_window_at(WID, settled);
    assert_eq!(
        logged(&app, t0),
        [(ns(settled - t0), KeyEcho::Unseen)],
        "negative control: over its own glyph, no mask"
    );
    let mut app = granted(t0);
    feed(&app, &vec![b'x'; usize::from(cols)]);
    key(&mut app, 'p', t0 + ms(10));
    assert!(
        stopped_raw(&app),
        "negative control: a deferred wrap with no mask"
    );
}

#[test]
fn keys_typed_over_their_own_glyphs_are_never_logged_seen_under_the_grant() {
    let t0 = Instant::now();
    // vim's normal mode on "el": the row shows both keys' glyphs before anything is printed, then
    // the program prints elsewhere and moves the cursor past them, which would pass for their echo.
    let mut app = granted(t0);
    feed(&app, b"el\x1b[2D");
    key(&mut app, 'e', t0 + ms(10));
    key(&mut app, 'l', t0 + ms(20));
    assert_eq!(held(&app), 2, "both held with their cells");
    feed(&app, b"\x1b7\x1b[6;1Hx\x1b8\x1b[2C");
    app.redraw_window_at(WID, t0 + ms(30));
    assert_eq!(
        held(&app),
        2,
        "a frame showing the glyphs they were typed over proves nothing"
    );
    assert!(logged(&app, t0).is_empty(), "nothing is logged while held");
    let settled = t0 + ms(10) + ECHO_PROOF_BOUND;
    app.redraw_window_at(WID, settled);
    assert_eq!(
        logged(&app, t0),
        [(ns(settled - t0), KeyEcho::Unseen); 2],
        "both logged unseen at the bound"
    );
    assert!(status(&app).attached, "the recording runs on");

    let mut app = ungranted(t0);
    feed(&app, b"el\x1b[2D");
    key(&mut app, 'e', t0 + ms(10));
    assert!(
        stopped_raw(&app),
        "negative control: without the grant the first key stops the recording at its press"
    );
}

#[test]
fn a_no_echo_key_and_secure_keyboard_entry_still_stop_a_granted_recording() {
    let t0 = Instant::now();
    for holding in [false, true] {
        let sink = Arc::new(SinkWriter::new(-1));
        let mut app = raw_on(
            momentum_app_on(scripted_app_with_sink(sink.clone()), false),
            t0,
            false,
        );
        app.aos_grant_for_test(WID).expect("granted");
        if holding {
            key(&mut app, 'a', t0 + ms(10));
            assert!(granted_hold(&app), "fixture: a key held under the grant");
        }
        // Another program leads the tab now: a grant consulted here would lapse first.
        FRONT_GROUP.with(|g| g.set(Some(OTHER)));
        sink.set_tty_swallows_hint(true);
        key(&mut app, 'p', t0 + ms(20));
        sink.set_tty_swallows_hint(false);
        assert!(
            !status(&app).attached && detached_for(&app, "no-echo_key"),
            "holding: {holding}"
        );
        assert!(
            !row(&app, "unseen=off why=program_left"),
            "holding: {holding}: the grant was never consulted"
        );
        assert!(!app.aos_holds_grant_for_test(WID), "holding: {holding}");
    }

    let mut app = granted(t0);
    app.config.secure_keyboard_entry = Some(true);
    key(&mut app, 'p', t0 + ms(10));
    assert!(!status(&app).attached && detached_for(&app, "secure_keyboard_entry"));
    assert!(!app.aos_holds_grant_for_test(WID));

    let mut app = granted(t0);
    key(&mut app, 'p', t0 + ms(10));
    assert!(
        status(&app).attached && granted_hold(&app),
        "negative control: the same press with neither is held under the grant"
    );
}

#[test]
fn the_grant_ends_with_its_attach() {
    let t0 = Instant::now();
    let mut app = granted(t0);
    let first = app.aos_attaches_for_test(WID);
    let _ = app.aos_detach_now(t0 + ms(5)).expect("detaches");
    assert!(
        !app.aos_unseen_on(WID) && !app.aos_holds_grant_for_test(WID),
        "a detach"
    );
    app.aos_attach_at(MomentumCode::Shipped, t0 + ms(10))
        .expect("re-attaches");
    assert!(app.aos_attaches_for_test(WID) > first);
    assert!(!app.aos_unseen_on(WID), "the next attach has none");
    key(&mut app, 'p', t0 + ms(20));
    assert!(held(&app) == 1 && !granted_hold(&app));
    app.redraw_window_at(WID, t0 + ms(20) + ECHO_PROOF_BOUND);
    assert!(stopped_raw(&app), "its unproven key stops the recording");

    let mut app = granted(t0);
    app.aos_attach_at(MomentumCode::Shipped, t0 + ms(10))
        .expect("re-attaches over the live one");
    assert!(
        !app.aos_holds_grant_for_test(WID),
        "an attach over a live one"
    );

    // A grant left past its attach (the mutant) is cleared by a privacy stop of a recording that
    // is no longer attached: both of `privacy_detach`'s branches clear it.
    let mut app = granted(t0);
    GRANT_OUTLIVES_ATTACH.with(|on| on.set(true));
    let _ = app.aos_detach_now(t0 + ms(5)).expect("detaches");
    let outlived = (app.aos_holds_grant_for_test(WID), app.aos_unseen_on(WID));
    GRANT_OUTLIVES_ATTACH.with(|on| on.set(false));
    assert_eq!(
        outlived,
        (true, true),
        "negative control: the mutant's grant outlives its attach"
    );
    assert!(!app.aos_unseen_on(WID), "a stale grant is never live");
    app.aos_privacy_detach_all(SECURE_ENTRY);
    assert!(
        !app.aos_holds_grant_for_test(WID),
        "a privacy stop while detached"
    );
}

#[test]
fn the_grant_lapses_when_its_program_leaves_the_front() {
    let t0 = Instant::now();
    let mut app = granted(t0);
    FRONT_GROUP.with(|g| g.set(Some(OTHER)));
    key(&mut app, 'p', t0 + ms(10));
    assert!(!app.aos_unseen_on(WID), "the grant lapsed");
    assert!(row(&app, "unseen=off why=program_left"));
    assert!(
        status(&app).attached,
        "with nothing held the recording runs on"
    );
    assert!(
        held(&app) == 1 && !granted_hold(&app),
        "the key is held as with no grant"
    );
    FRONT_GROUP.with(|g| g.set(Some(GROUP)));
    key(&mut app, 'w', t0 + ms(20));
    assert!(
        !app.aos_unseen_on(WID),
        "its program's return brings no grant back"
    );

    let mut app = granted(t0);
    key(&mut app, 'p', t0 + ms(10));
    FRONT_GROUP.with(|g| g.set(Some(OTHER)));
    key(&mut app, 'w', t0 + ms(20));
    assert!(
        stopped_raw(&app) && row(&app, "unseen=off why=program_left"),
        "with keys held under it, the lapse stops the recording"
    );

    let mut app = granted(t0);
    key(&mut app, 'p', t0 + ms(10));
    key(&mut app, 'w', t0 + ms(20));
    assert!(
        held(&app) == 2 && granted_hold(&app) && app.aos_unseen_on(WID),
        "negative control: with its program still in front the grant holds"
    );
}

#[test]
fn a_key_with_no_cells_waits_blind_under_the_grant() {
    let t0 = Instant::now();
    let mut app = raw_on(momentum_app(false), t0, true);
    app.aos_grant_for_test(WID).expect("granted");
    key(&mut app, 'p', t0 + ms(10));
    press(&mut app, shift_enter(), t0 + ms(20));
    assert_eq!(held(&app), 2, "Shift-Enter joins the held key, blind");
    let settled = t0 + ms(10) + ECHO_PROOF_BOUND;
    app.redraw_window_at(WID, settled);
    assert_eq!(
        logged(&app, t0),
        [(ns(settled - t0), KeyEcho::Unseen); 2],
        "both logged unseen at the bound"
    );

    let mut app = raw_on(momentum_app(false), t0, true);
    key(&mut app, 'p', t0 + ms(10));
    press(&mut app, shift_enter(), t0 + ms(20));
    assert!(
        stopped_raw(&app),
        "negative control: without the grant it stops the recording"
    );
}

#[test]
fn only_a_persons_yes_in_aterms_alert_grants() {
    let t0 = Instant::now();
    let refusal = |app: &mut App| app.pending_action_refusal.take();

    let mut app = ungranted(t0);
    app.toggle_unseen_keys();
    assert_eq!(
        refusal(&mut app).as_deref(),
        Some(OwnerAnswer::NotAsked.refusal(UNSEEN_DECLINED)),
        "a headless instance's alert is nobody's"
    );
    assert!(!app.aos_holds_grant_for_test(WID));

    app.consent_gestures = ConsentGestures::confirming();
    app.with_controller_mark(true, App::toggle_unseen_keys);
    assert_eq!(
        refusal(&mut app).as_deref(),
        Some(UNSEEN_PERSON_ONLY),
        "a control client"
    );
    app.with_assistive_source(App::toggle_unseen_keys);
    assert_eq!(
        refusal(&mut app).as_deref(),
        Some(UNSEEN_PERSON_ONLY),
        "an accessibility client"
    );
    app.with_origin_mark(
        aterm_window::event::InputOrigin::Relayed,
        App::toggle_unseen_keys,
    );
    assert_eq!(
        refusal(&mut app).as_deref(),
        Some(UNSEEN_PERSON_ONLY),
        "a key or click another process posted"
    );
    assert!(!app.aos_holds_grant_for_test(WID));
    assert!(
        crate::control::control_media::invoke_refusal("RecordUnseenKeys")
            .is_some_and(|r| r.starts_with("ERR denied: ")),
        "`aterm ctl invoke` is refused"
    );
    for form in ["unseen", "unseen on", "record-unseen", "grant"] {
        assert!(
            crate::aos_verb::parse(form, true).is_err(),
            "the verb has no `{form}`"
        );
    }

    key(&mut app, 'p', t0 + ms(10));
    app.toggle_unseen_keys();
    assert_eq!(refusal(&mut app).as_deref(), Some(UNSEEN_HELD));
    assert!(!app.aos_holds_grant_for_test(WID));

    let mut app = ungranted(t0);
    app.consent_gestures = ConsentGestures::confirming();
    FRONT_GROUP.with(|g| g.set(None));
    app.toggle_unseen_keys();
    assert_eq!(refusal(&mut app).as_deref(), Some(UNSEEN_NO_GROUP));
    let _ = app.aos_detach_now(t0 + ms(5)).expect("detaches");
    app.toggle_unseen_keys();
    assert_eq!(refusal(&mut app).as_deref(), Some(UNSEEN_NOT_RECORDING));

    let mut app = ungranted(t0);
    app.consent_gestures = ConsentGestures::declining();
    app.toggle_unseen_keys();
    assert_eq!(
        refusal(&mut app).as_deref(),
        Some(UNSEEN_DECLINED),
        "the person said no"
    );
    app.consent_gestures = ConsentGestures::confirming();
    app.toggle_unseen_keys();
    assert_eq!(
        refusal(&mut app).as_deref(),
        Some(OwnerAnswer::Quiet.refusal(UNSEEN_DECLINED)),
        "a no quiets the next ask"
    );
    assert!(!app.aos_holds_grant_for_test(WID));

    let mut app = ungranted(t0);
    app.consent_gestures = ConsentGestures::confirming();
    app.toggle_unseen_keys();
    assert_eq!(refusal(&mut app), None, "negative control: a person's yes");
    assert!(app.aos_unseen_on(WID) && row(&app, "unseen=on why=person"));
    app.toggle_unseen_keys();
    assert_eq!(refusal(&mut app), None);
    assert!(
        !app.aos_unseen_on(WID) && row(&app, "unseen=off why=person"),
        "their next press stops it"
    );
}

#[test]
fn the_recording_says_it_holds_unseen_keys_wherever_it_goes() {
    use crate::aos_verb::{Asked, check_file, write_export};

    let t0 = Instant::now();
    let mut app = granted(t0);
    key(&mut app, 'p', t0 + ms(10));
    key(&mut app, 'w', t0 + ms(20));
    let settled = t0 + ms(10) + ECHO_PROOF_BOUND;
    app.redraw_window_at(WID, settled);
    let line = app.control_aos(Asked::Status, settled).expect("status");
    assert!(line.contains(" unseen=on"), "{line}");
    let trace = app.aos_trace_now().expect("a trace");
    assert_eq!(
        (trace.unseen, trace.counts.unseen, trace.counts.keys),
        (2, 2, 2)
    );
    let exported = app.aos_export_now(None).expect("exports");
    assert_eq!(exported.unseen, 2);
    let root = aterm_tempfile::tempdir().expect("a scratch state dir");
    let path = write_export(root.path(), "unseen.trace", &exported.bytes).expect("writes");
    let checked = check_file(&path, &aterm_stamp::build_id()).expect("checks");
    assert!(checked.contains(" unseen=2 "), "{checked}");

    let later = settled + ms(100);
    app.aos_repair_at(MomentumCode::Shipped, later)
        .expect("repairs");
    if status(&app).paused {
        app.aos_resume_now().expect("resumes");
    }
    assert_eq!(
        app.aos_trace_now().expect("a trace").unseen,
        2,
        "a repair keeps the count"
    );

    let _ = app.aos_detach_now(later + ms(10)).expect("detaches");
    app.aos_attach_at(MomentumCode::Shipped, later + ms(20))
        .expect("re-attaches");
    assert_eq!(
        app.aos_trace_now().expect("a trace").unseen,
        0,
        "the next recording starts its count at zero"
    );
    let line = app
        .control_aos(Asked::Status, later + ms(20))
        .expect("status");
    assert!(line.contains(" unseen=off"), "{line}");

    let mut app = granted(t0);
    key(&mut app, 'p', t0 + ms(10));
    feed(&app, b"p");
    key(&mut app, 'w', t0 + ms(20));
    feed(&app, b"w");
    app.redraw_window_at(WID, t0 + ms(30));
    let trace = app.aos_trace_now().expect("a trace");
    assert_eq!(
        (trace.unseen, trace.counts.keys),
        (0, 2),
        "negative control: keys proven under the grant are logged seen"
    );
    assert_eq!(logged(&app, t0), [(ns(ms(30)), KeyEcho::Seen); 2]);
}
