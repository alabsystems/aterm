// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-path tests for the echo proof (ATERM-KI-1025-1): a recording keeps a key a program reads
//! raw only once the live screen shows its own echo, visibly and in place, and logs it at that
//! frame; every other outcome stops the recording before anything of the key is logged.
//!
//! The fixture's tty mode is unreadable and no prompt of aterm's shell is open, so every typed key
//! is a raw one; the test feeds the program's echo, or none, itself.

use std::collections::BTreeSet;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use aterm_effects::aos_momentum::{MomentumCode, MomentumEvent, MomentumSchema, MomentumState};
use aterm_models_aos::aos_key_hold_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

use super::aos_repair_tests::{
    WID, detached_for, exported_log, glyph, leave_prompt, momentum_app, press, status, type_keys,
};
use crate::App;
use crate::aos_host::Worked;
use crate::aos_host::echo::{ECHO_HOLD_OFF, ECHO_PROOF_BOUND, RELEASE_ON_PENDING, held_keys};
use crate::input::{InputEvent, PasteFraming};
use crate::scripted_redraw_fixture::feed;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// `d` in nanoseconds, a log instant's unit.
fn ns(d: Duration) -> i128 {
    i128::try_from(d.as_nanos()).expect("small")
}

/// The fixture at a program that reads raw (no prompt in front, its tty mode unreadable), with a
/// recording attached at `t0`.
fn raw_app(t0: Instant) -> App {
    raw_app_after(t0, b"")
}

/// [`raw_app`] with the program on the alternate screen, where Shift-Enter types a new line into
/// a composer.
fn raw_alt_app(t0: Instant) -> App {
    raw_app_after(t0, b"\x1b[?1049h")
}

/// [`raw_app`], the program having printed `output` before the attach.
fn raw_app_after(t0: Instant, output: &[u8]) -> App {
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    leave_prompt(&app);
    if !output.is_empty() {
        feed(&app, output);
    }
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    app
}

/// A press of `key` with `mods`, as the keyboard sends it.
fn pressed(key: Key, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

/// Shift-Enter: on the alternate screen, a new line typed into a composer, which carries no text.
fn shift_enter() -> InputEvent {
    pressed(Key::Named(NamedKey::Enter), Modifiers::SHIFT)
}

/// The person types `c` at `at`.
fn key(app: &mut App, c: char, at: Instant) {
    press(app, glyph(c), at);
}

/// A pasted `text`, unbracketed.
fn paste(text: &str) -> InputEvent {
    InputEvent::Paste(text.to_owned(), PasteFraming::Gesture { bracketed: false })
}

/// The `Key` instants the recording logged since `epoch`, in nanoseconds after it.
fn key_times(app: &App, epoch: Instant) -> Vec<i128> {
    exported_log(app, epoch)
        .iter()
        .filter_map(|e| match *e {
            MomentumEvent::Key { at, .. } => Some(at),
            _ => None,
        })
        .collect()
}

/// The glow's typing-rate stamp in `app`'s attach checkpoint: `None` when it started from rest.
fn checkpoint_rate_at(app: &App, epoch: Instant) -> Option<i128> {
    let trace = app.windows[&WID]
        .fx
        .cursor()
        .momentum
        .aos_trace()
        .expect("attached");
    let state: MomentumState = aterm_aos::checkpoint::Checkpoint::decode(&trace.checkpoint)
        .expect("decodes")
        .restore(&MomentumSchema::new(epoch), &trace.identity)
        .expect("restores");
    state.glow().snapshot(epoch).expect("a snapshot").rate_at
}

/// A program's echo of `c`: `c` itself.
fn same(c: char) -> String {
    c.to_string()
}

/// Types `text` raw from `at`, `gap` apart, feeding each key's `echo` and drawing a frame `lag`
/// after each press; returns the last frame's instant.
fn type_echoed(
    app: &mut App,
    text: &str,
    echo: impl Fn(char) -> String,
    at: Instant,
    gap: Duration,
    lag: Duration,
) -> Instant {
    let mut frame = at;
    for (i, c) in text.chars().enumerate() {
        let pressed = at + gap * u32::try_from(i).expect("short");
        key(app, c, pressed);
        feed(app, echo(c).as_bytes());
        frame = pressed + lag;
        app.redraw_window_at(WID, frame);
    }
    frame
}

#[test]
fn a_raw_key_the_program_echoes_in_place_is_recorded_at_its_echo() {
    let t0 = Instant::now();
    for masked in [false, true] {
        let mut app = raw_app(t0);
        let t = t0 + ms(100);
        let echo = |c: char| {
            if masked {
                "*".to_owned()
            } else {
                c.to_string()
            }
        };
        let _ = type_echoed(&mut app, "ls", echo, t, ms(60), ms(7));
        if masked {
            assert!(
                !status(&app).attached,
                "negative control: '*' feedback stopped it"
            );
            assert!(detached_for(&app, "raw_key"), "{:?}", app.aos_ls_now());
            continue;
        }
        assert!(status(&app).attached, "still recording");
        assert_eq!(
            key_times(&app, t0),
            [ns(ms(167)), ns(ms(167))],
            "both keys at the frame that proved the pair, never at their presses"
        );
    }
}

#[test]
fn nothing_is_logged_while_a_key_waits_for_its_echo() {
    let t0 = Instant::now();
    // A hidden field answers each key without showing it: by moving its cursor, by advancing it
    // over blanks, or by a redraw of another row. None of them is the key's own echo.
    for (what, off, output) in [
        ("no echo", false, ""),
        ("'*' feedback", false, "*"),
        (
            "the cursor advanced past the press over blanks",
            false,
            "  ",
        ),
        (
            "a status row redrawn, the cursor parked on it",
            false,
            "\x1b[5;1Hstatus 12:00",
        ),
        (
            "an unrelated row redrawn, the cursor back at the press",
            false,
            "\x1b7\x1b[3;1Hlog line\x1b8",
        ),
        ("negative control: the hold off", true, ""),
        (
            "negative control: the program echoes 'pw' in place",
            false,
            "pw",
        ),
    ] {
        ECHO_HOLD_OFF.with(|o| o.set(off));
        let mut app = raw_app(t0);
        app.redraw_window_at(WID, t0 + ms(50));
        let before = status(&app).log_len;
        let t = t0 + ms(100);
        key(&mut app, 'p', t);
        key(&mut app, 'w', t + ms(1));
        if !output.is_empty() {
            feed(&app, output.as_bytes());
        }
        let mut lens = Vec::new();
        for d in [5, 10, 20] {
            app.redraw_window_at(WID, t + ms(d));
            lens.push(status(&app).log_len);
        }
        ECHO_HOLD_OFF.with(|o| o.set(false));
        if off {
            assert!(lens[0] > before, "{what}: logged at the press");
            assert_eq!(key_times(&app, t0), [ns(ms(100)), ns(ms(101))], "{what}");
            continue;
        }
        if output == "pw" {
            assert_eq!(
                key_times(&app, t0),
                [ns(ms(105)), ns(ms(105))],
                "{what}: released at the first frame"
            );
            continue;
        }
        if output == "*" {
            assert!(!status(&app).attached, "{what}: refuted at the first frame");
            assert!(detached_for(&app, "raw_key"), "{what}");
            continue;
        }
        assert_eq!(
            lens, [before; 3],
            "{what}: no key, tick or query while held"
        );
        assert!(status(&app).attached, "{what}: the recording waits");
        app.redraw_window_at(WID, t + ECHO_PROOF_BOUND - ms(1));
        assert!(status(&app).attached, "{what}: just under the bound");
        assert_eq!(status(&app).log_len, before, "{what}");
        app.redraw_window_at(WID, t + ECHO_PROOF_BOUND);
        assert!(!status(&app).attached, "{what}: refuted at the bound");
        assert!(detached_for(&app, "raw_key"), "{what}");
    }
}

/// A frame whose read of the live terminal finds its lock busy (the egress writer's, say) settles
/// nothing, the key still held and nothing of it logged; the watcher refutes the hold once its
/// bound and a frame have passed, with no read of the terminal.
#[test]
fn a_frame_that_cannot_read_the_terminal_leaves_a_held_key_to_the_watcher() {
    let t0 = Instant::now();
    let mut app = raw_app(t0);
    let t = t0 + ms(100);
    key(&mut app, 'x', t);
    let before = status(&app).log_len;
    assert_eq!(held_keys(&app.windows[&WID].aos), 1, "held for its echo");
    let bound = t + ECHO_PROOF_BOUND;
    let term = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    let (locked, busy) = channel();
    let (done, release) = channel::<()>();
    std::thread::scope(|s| {
        s.spawn(move || {
            let _busy = crate::term_lock(&term);
            locked.send(()).expect("the frame waits for the lock");
            // A frame that waited for the lock reads it after this hang detector.
            let _ = release.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        });
        busy.recv().expect("the other thread holds the lock");
        app.aos_settle_hold(WID, bound);
        done.send(()).expect("the holder waits for the frame");
    });
    assert!(status(&app).attached, "the busy frame settled nothing");
    assert_eq!(
        held_keys(&app.windows[&WID].aos),
        1,
        "the key is still held"
    );
    assert_eq!(status(&app).log_len, before, "nothing of it is logged");
    let frame = app.windows[&WID].effect_present_interval();
    app.aos_watch_via(None::<fn(Worked)>, bound);
    assert!(
        status(&app).attached,
        "the watcher leaves the frame its turn"
    );
    app.aos_watch_via(None::<fn(Worked)>, bound + frame);
    assert!(!status(&app).attached, "the watcher refuted the hold");
    assert!(detached_for(&app, "raw_key"), "{:?}", app.aos_ls_now());
}

#[test]
fn a_key_that_carries_no_text_never_stops_a_raw_recording() {
    let t0 = Instant::now();
    let named = |key| pressed(Key::Named(key), Modifiers::empty());
    let chorded = |c, mods| pressed(Key::Character(c), mods);
    for (what, keys) in [
        (
            "arrows, Tab, Escape, F5, Ctrl-F and Alt-b (Option as Meta)",
            vec![
                named(NamedKey::ArrowDown),
                named(NamedKey::ArrowUp),
                named(NamedKey::Tab),
                named(NamedKey::Escape),
                named(NamedKey::F5),
                chorded('f', Modifiers::CTRL),
                chorded('b', Modifiers::ALT),
            ],
        ),
        ("negative control: an unechoed 'j'", vec![glyph('j')]),
    ] {
        let mut app = raw_app(t0);
        let t = t0 + ms(100);
        for (i, ev) in (0u32..).zip(keys) {
            press(&mut app, ev, t + ms(20) * i);
        }
        let held = held_keys(&app.windows[&WID].aos);
        app.redraw_window_at(WID, t + ECHO_PROOF_BOUND + ms(200));
        if held > 0 {
            assert!(what.starts_with("negative control"), "{what}: held");
            assert!(!status(&app).attached, "{what}: refuted at the bound");
            assert!(detached_for(&app, "raw_key"), "{what}");
            continue;
        }
        assert!(!what.starts_with("negative control"), "{what}: never held");
        assert!(status(&app).attached, "{what}: still recording");
        assert!(
            key_times(&app, t0).is_empty(),
            "{what}: never logged as keys"
        );
    }
}

#[test]
fn an_alternate_screen_shift_enter_is_logged_only_with_nothing_held() {
    let t0 = Instant::now();
    let mut app = raw_alt_app(t0);
    let at = type_echoed(&mut app, "ab", same, t0 + ms(100), ms(30), ms(5));
    assert_eq!(key_times(&app, t0).len(), 2, "'ab' released");
    let enter = at + ms(30);
    press(&mut app, shift_enter(), enter);
    assert!(
        status(&app).attached,
        "it carries no text, so it is not private"
    );
    assert_eq!(
        key_times(&app, t0).last(),
        Some(&ns(enter - t0)),
        "logged at its press"
    );
    assert_eq!(key_times(&app, t0).len(), 3);

    let x = enter + ms(30);
    key(&mut app, 'x', x);
    assert_eq!(
        held_keys(&app.windows[&WID].aos),
        1,
        "an unechoed 'x' waits"
    );
    press(&mut app, shift_enter(), x + ms(20));
    assert!(
        !status(&app).attached,
        "with 'x' held it joins nothing and stops the recording"
    );
    assert!(detached_for(&app, "raw_key"));

    let mut app = raw_app(t0);
    let at = type_echoed(&mut app, "ab", same, t0 + ms(100), ms(30), ms(5));
    press(&mut app, shift_enter(), at + ms(30));
    assert_eq!(
        key_times(&app, t0).len(),
        2,
        "contrast: on the primary screen Shift-Enter never reaches the verdict and logs nothing"
    );
    assert!(status(&app).attached);
}

#[test]
fn an_invisible_echo_never_proves() {
    let t0 = Instant::now();
    for (what, sgr) in [
        ("negative control: shown", ""),
        ("SGR 8", "\x1b[8m"),
        ("blue on blue", "\x1b[34;44m"),
    ] {
        let mut app = raw_app(t0);
        feed(&app, sgr.as_bytes());
        let last = type_echoed(&mut app, "ab", same, t0 + ms(100), ms(30), ms(5));
        if sgr.is_empty() {
            assert_eq!(key_times(&app, t0).len(), 2, "{what}: recorded");
            continue;
        }
        assert!(status(&app).attached, "{what}: waits");
        app.redraw_window_at(WID, last + ECHO_PROOF_BOUND);
        assert!(!status(&app).attached, "{what}: refuted at the bound");
        assert!(detached_for(&app, "raw_key"), "{what}");
    }
}

#[test]
fn a_scrolled_back_viewport_never_proves() {
    // The scrolled arm differs from the control in the scroll alone.
    let t0 = Instant::now();
    for scrolled in [false, true] {
        let mut app = raw_app(t0);
        let rows = app.windows[&WID].rows;
        for _ in 0..rows {
            feed(&app, b"\r\n");
        }
        feed(&app, b"ab\r\n");
        let t = t0 + ms(100);
        key(&mut app, 'a', t);
        key(&mut app, 'b', t + ms(30));
        let term = app.front_terminal(WID).expect("front").term.clone();
        if scrolled {
            let mut g = crate::term_lock(&term);
            g.scroll_display(1);
            assert!(
                g.grid().display_offset() > 0,
                "history shows 'ab' at the hold's row"
            );
        }
        crate::term_lock(&term).process(b"ab");
        app.redraw_window_at(WID, t + ms(40));
        if !scrolled {
            assert_eq!(
                key_times(&app, t0).len(),
                2,
                "control: recorded at its echo"
            );
            continue;
        }
        assert!(
            status(&app).attached && held_keys(&app.windows[&WID].aos) == 2,
            "the live row shows the echo, but no proof is read under a scrolled viewport"
        );
        assert_eq!(status(&app).log_len, 0, "and nothing is logged");
        app.redraw_window_at(WID, t + ECHO_PROOF_BOUND);
        assert!(!status(&app).attached, "never proven: stopped by the bound");
        assert!(detached_for(&app, "raw_key"));
    }
}

#[test]
fn identical_echoes_never_open_a_run() {
    let t0 = Instant::now();
    let mut app = raw_app(t0);
    let last = type_keys(&mut app, t0 + ms(60), 3);
    assert!(status(&app).attached, "'aaa' echoed 'aaa' waits");
    assert_eq!(status(&app).log_len, 0, "and logged nothing");
    app.redraw_window_at(WID, t0 + ms(60) + ECHO_PROOF_BOUND);
    assert!(!status(&app).attached, "refuted at the first key's bound");
    assert!(last < t0 + ms(60) + ECHO_PROOF_BOUND);

    let mut app = raw_app(t0);
    let _ = type_echoed(&mut app, "ab", |_| "*".into(), t0 + ms(60), ms(30), ms(5));
    assert!(!status(&app).attached, "pwfeedback's '**'");

    let mut app = raw_app(t0);
    let _ = type_echoed(&mut app, "ab", same, t0 + ms(60), ms(30), ms(5));
    assert_eq!(
        key_times(&app, t0).len(),
        2,
        "negative control: 'ab' echoed 'ab'"
    );
}

#[test]
fn a_run_continues_only_where_the_last_echo_ended() {
    let t0 = Instant::now();
    let mut app = raw_app(t0);
    let echo = same;
    let at = type_echoed(&mut app, "ab", echo, t0 + ms(100), ms(30), ms(5));
    assert_eq!(key_times(&app, t0).len(), 2, "'ab' released");
    let at = type_echoed(&mut app, "c", echo, at + ms(30), ms(30), ms(5));
    assert_eq!(
        key_times(&app, t0).len(),
        3,
        "'c' at the run's end released alone"
    );

    feed(&app, b"\r\n");
    let at = type_echoed(&mut app, "d", echo, at + ms(30), ms(30), ms(5));
    assert_eq!(
        key_times(&app, t0).len(),
        3,
        "'d' on a new row opens a new run: it waits"
    );
    let at = type_echoed(&mut app, "e", echo, at + ms(30), ms(30), ms(5));
    assert_eq!(key_times(&app, t0).len(), 5, "'de' released");

    let sink = app.pool.get(0).expect("session").ctx.sink.clone();
    let before = sink.input_epoch();
    let _ = sink.write_reply(b"\x1b[I");
    assert_ne!(
        sink.input_epoch(),
        before,
        "fixture: a reply is input to the session"
    );
    let at = type_echoed(&mut app, "f", echo, at + ms(60), ms(30), ms(5));
    assert_eq!(
        key_times(&app, t0).len(),
        5,
        "input between the runs: 'f' at the run's end continues nothing"
    );
    app.redraw_window_at(WID, at + ECHO_PROOF_BOUND);
    assert!(!status(&app).attached);

    let mut app = raw_app(t0);
    let at = type_echoed(&mut app, "ab", echo, t0 + ms(100), ms(30), ms(5));
    let _ = type_echoed(&mut app, "c", |_| "*".into(), at + ms(30), ms(30), ms(5));
    assert!(
        !status(&app).attached,
        "a '*' where 'c' should show refutes at once"
    );
}

#[test]
fn a_space_no_typed_glyph_follows_stops_the_recording_at_its_bound() {
    // A blank never corroborates a run, so an echoed space waits for a typed glyph after it. A
    // key that sends no text never reaches the hold, so after Tab (completion printed past the
    // space), Enter or an arrow the space stays unproven. Each arm differs from the control in
    // the key typed after the space.
    let t0 = Instant::now();
    let named = |k| Some(pressed(Key::Named(k), Modifiers::empty()));
    for (arm, next, shown) in [
        ("control: 'c'", None, &b"c"[..]),
        ("Tab, completion printed", named(NamedKey::Tab), b"cd/"),
        ("Enter", named(NamedKey::Enter), b"\r\n"),
        ("an arrow", named(NamedKey::ArrowLeft), b"\x1b[D"),
    ] {
        let mut app = raw_app(t0);
        let at = type_echoed(&mut app, "ab", same, t0 + ms(100), ms(30), ms(5));
        assert_eq!(key_times(&app, t0).len(), 2, "{arm}: 'ab' released");
        let space = at + ms(30);
        press(
            &mut app,
            pressed(Key::Named(NamedKey::Space), Modifiers::empty()),
            space,
        );
        feed(&app, b" ");
        app.redraw_window_at(WID, space + ms(5));
        assert_eq!(
            held_keys(&app.windows[&WID].aos),
            1,
            "{arm}: the echoed space waits"
        );
        let then = space + ms(30);
        match next {
            Some(ev) => press(&mut app, ev, then),
            None => key(&mut app, 'c', then),
        }
        feed(&app, shown);
        app.redraw_window_at(WID, then + ms(5));
        if arm.starts_with("control") {
            assert_eq!(
                key_times(&app, t0).len(),
                4,
                "{arm}: the glyph proves the space before it"
            );
            assert!(status(&app).attached, "{arm}");
            continue;
        }
        assert!(status(&app).attached, "{arm}: still waiting");
        assert_eq!(
            key_times(&app, t0).len(),
            2,
            "{arm}: the space is not logged"
        );
        app.redraw_window_at(WID, space + ECHO_PROOF_BOUND);
        assert!(
            !status(&app).attached,
            "{arm}: refuted at the space's bound"
        );
        assert!(detached_for(&app, "raw_key"), "{arm}");
    }
}

#[test]
fn continuous_typing_over_a_slow_link_stays_attached() {
    const TEXT: &str = "abcdefghijklmnopqrstuvwxyz";
    let t0 = Instant::now();
    for (what, rtt) in [
        ("150 ms", ms(150)),
        (
            "negative control: past the bound",
            ECHO_PROOF_BOUND + ms(100),
        ),
    ] {
        let mut app = raw_app(t0);
        let start = t0 + ms(100);
        let gap = ms(50);
        let chars: Vec<char> = TEXT.chars().collect();
        let n = u32::try_from(chars.len()).expect("short");
        let mut events: Vec<(Instant, bool, char)> = Vec::new();
        for (i, &c) in chars.iter().enumerate() {
            let pressed = start + gap * u32::try_from(i).expect("short");
            events.push((pressed, true, c));
            events.push((pressed + rtt, false, c));
        }
        events.sort_by_key(|&(at, is_press, _)| (at, !is_press));
        for (at, is_press, c) in events {
            if is_press {
                key(&mut app, c, at);
            } else {
                feed(&app, c.to_string().as_bytes());
            }
            app.redraw_window_at(WID, at + ms(1));
        }
        if rtt > ECHO_PROOF_BOUND {
            assert!(!status(&app).attached, "{what}: detached");
            continue;
        }
        let burst = gap * n + rtt;
        assert!(
            burst > ECHO_PROOF_BOUND,
            "the burst outlasts one key's bound: {burst:?}"
        );
        assert!(
            status(&app).attached,
            "{what}: every key echoed in time stays recorded"
        );
        assert_eq!(
            key_times(&app, t0).len(),
            chars.len(),
            "{what}: each key logged once"
        );
    }
}

#[test]
fn a_paste_while_a_key_waits_detaches() {
    let t0 = Instant::now();
    for held in [true, false] {
        let mut app = raw_app(t0);
        let t = t0 + ms(100);
        if held {
            key(&mut app, 'a', t);
        }
        press(&mut app, paste("b"), t + ms(10));
        assert_eq!(status(&app).attached, !held, "held={held}");
        assert_eq!(detached_for(&app, "raw_key"), held, "held={held}");
    }
}

/// A `[key_sequences]` chord's bytes, as `forward_literal_press` hands them to the press path.
fn chord(bytes: &[u8]) -> InputEvent {
    InputEvent::KeySequence(bytes.to_vec())
}

#[test]
fn a_chord_that_types_text_is_held_for_its_echo_like_a_typed_key() {
    let t0 = Instant::now();
    for (what, sent, echo) in [
        ("'*' feedback", "#", Some("*")),
        ("no echo", "#", None),
        ("echoed with the next key", "#", Some("#S")),
        (
            "negative control: a Meta chord (ESC, then '#') sends a command",
            "\x1b#",
            Some("*"),
        ),
    ] {
        let mut app = raw_app(t0);
        app.redraw_window_at(WID, t0 + ms(50));
        let before = status(&app).log_len;
        let t = t0 + ms(100);
        press(&mut app, chord(sent.as_bytes()), t);
        if echo == Some("#S") {
            key(&mut app, 'S', t + ms(40));
        }
        assert_eq!(status(&app).log_len, before, "{what}: nothing at the press");
        if let Some(echo) = echo {
            feed(&app, echo.as_bytes());
        }
        let frame = t + ms(48);
        app.redraw_window_at(WID, frame);
        if sent.starts_with('\x1b') {
            assert!(status(&app).attached, "{what}: stops nothing");
            assert!(
                status(&app).log_len > before,
                "{what}: the next frame's tick logs its instant, as it did a text chord's"
            );
            continue;
        }
        match echo {
            Some("*") => {
                assert!(!status(&app).attached, "{what}: refuted at the first frame");
                assert!(detached_for(&app, "raw_key"), "{what}");
            }
            Some(_) => {
                assert!(status(&app).attached, "{what}");
                assert_eq!(
                    key_times(&app, t0),
                    [ns(frame - t0), ns(frame - t0)],
                    "{what}: both at the frame that proved '#S', never at their presses"
                );
            }
            None => {
                assert!(status(&app).attached, "{what}: the recording waits");
                assert_eq!(status(&app).log_len, before, "{what}: no key or tick");
                app.redraw_window_at(WID, t + ECHO_PROOF_BOUND);
                assert!(!status(&app).attached, "{what}: refuted at the bound");
                assert!(detached_for(&app, "raw_key"), "{what}");
            }
        }
    }
}

#[test]
fn aterms_own_predicted_echo_never_proves_a_key() {
    let t0 = Instant::now();
    for echoed in [false, true] {
        let mut app = raw_app(t0);
        app.config.predictive_echo = Some("always".into());
        app.predict_mode_cache = None;
        let t = t0 + ms(100);
        key(&mut app, 'x', t);
        key(&mut app, 'y', t + ms(30));
        if echoed {
            feed(&app, b"xy");
        }
        app.redraw_window_at(WID, t + ms(40));
        if echoed {
            assert_eq!(
                key_times(&app, t0).len(),
                2,
                "negative control: the program's echo"
            );
            continue;
        }
        assert!(
            app.windows[&WID].pred_shown,
            "the frame drew aterm's prediction of 'xy'"
        );
        assert!(status(&app).attached, "and proved nothing by it");
        assert_eq!(status(&app).log_len, 0);
        app.redraw_window_at(WID, t + ECHO_PROOF_BOUND);
        assert!(!status(&app).attached, "refuted at the bound");
    }
}

#[test]
fn an_attach_or_detach_while_a_key_waits_starts_the_next_glow_from_rest() {
    let t0 = Instant::now();
    let mut app = raw_app(t0);
    key(&mut app, 'a', t0 + ms(100));
    let epoch = t0 + ms(110);
    app.aos_attach_at(MomentumCode::Shipped, epoch)
        .expect("re-attaches");
    assert!(
        detached_for(&app, "raw_key"),
        "the held key stopped the old recording"
    );
    assert_eq!(
        checkpoint_rate_at(&app, epoch),
        None,
        "the new one starts from rest"
    );

    for held in [true, false] {
        let mut app = raw_app(t0);
        let _ = type_echoed(&mut app, "ab", same, t0 + ms(60), ms(30), ms(5));
        if held {
            key(&mut app, 'c', t0 + ms(200));
        }
        let _ = app.aos_detach_now(t0 + ms(210)).expect("detaches");
        assert!(!detached_for(&app, "raw_key"), "the person's own detach");
        let epoch = t0 + ms(220);
        app.aos_attach_at(MomentumCode::Shipped, epoch)
            .expect("attaches");
        if held {
            assert_eq!(
                checkpoint_rate_at(&app, epoch),
                None,
                "held keys left it tainted"
            );
        } else {
            assert!(
                checkpoint_rate_at(&app, epoch).is_some(),
                "negative control: logged keys carry their momentum"
            );
        }
    }

    // A run proven under one recording corroborates nothing in the next: `c` typed at its end
    // with no input since waits for a distinct second key once the recording restarted.
    for (what, restart) in [
        ("re-attached", Some(false)),
        ("detached, then attached", Some(true)),
        ("negative control: the same recording", None),
    ] {
        let mut app = raw_app(t0);
        let at = type_echoed(&mut app, "ab", same, t0 + ms(60), ms(30), ms(5));
        if let Some(detach_first) = restart {
            if detach_first {
                let _ = app.aos_detach_now(at + ms(5)).expect("detaches");
            }
            app.aos_attach_at(MomentumCode::Shipped, at + ms(10))
                .expect("attaches");
        }
        let _ = type_echoed(&mut app, "c", same, at + ms(30), ms(30), ms(5));
        assert_eq!(
            held_keys(&app.windows[&WID].aos),
            usize::from(restart.is_some()),
            "{what}"
        );
    }
}

/// The claim-2 binding (`OnlyEchoedKeysLogged`) and the person's grant (D12's
/// `UnseenOnlyWithOptIn`, `MaskedKeyNeverLogged`, `OptInEndsWithItsAttach`): random GUI moves, each
/// followed by the model admitting the projected step in the actions the harness names. The harness
/// computes `echoed` and `masked` from the bytes it fed, never from the code; `unproven` is a key
/// logged as seen that the harness never echoed, `unseen` keys logged unseen while the window held
/// no grant, `exposed` keys logged from a hold the harness masked. The fixture is on the alternate
/// screen, where Shift-Enter (`Bare`) carries no text: a press the log gains one key at, with
/// nothing held, is the model's `Key`, and one typed while keys wait under the grant is held blind.
mod claim2 {
    use super::*;
    use crate::aos_host::FRONT_GROUP;
    use crate::aos_host::echo::{UNSEEN_OVER_MASK, UNSEEN_WITHOUT_GRANT};
    use aterm_effects::aos_momentum::KeyEcho;

    /// The foreground group the fixture's session reports, and another program's.
    const GROUP: i32 = 100;
    const OTHER_GROUP: i32 = 200;

    /// One move of the walk.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Move {
        Type,
        Echo,
        Mask,
        Frame,
        Late,
        Newline,
        Attach,
        Detach,
        Bare,
        /// The person's yes for the live attach.
        OptIn,
        /// The grant's end before its attach's: the person's stop, or a raw key while another
        /// program leads the tab, in turn.
        Lapse,
    }

    /// The moves, weighted so a walk often gets as far as a proven pair.
    const MOVES: [Move; 18] = [
        Move::Type,
        Move::Type,
        Move::Type,
        Move::Echo,
        Move::Echo,
        Move::Echo,
        Move::Frame,
        Move::Frame,
        Move::Frame,
        Move::Mask,
        Move::Late,
        Move::Newline,
        Move::Attach,
        Move::Attach,
        Move::Detach,
        Move::Bare,
        Move::OptIn,
        Move::Lapse,
    ];

    /// A key the harness saw held: its press, whether the harness fed its echo in place, and
    /// whether it was held blind (no cells).
    struct HeldKey {
        at: Instant,
        echoed: bool,
        blind: bool,
    }

    struct Gui {
        app: App,
        now: Instant,
        epoch: Instant,
        queue: Vec<HeldKey>,
        pending: Vec<char>,
        /// A mask or newline was fed since the hold opened: later echoes are out of place.
        spoiled: bool,
        /// A mask the harness fed shows where an unechoed held key should.
        masked: i64,
        unproven: i64,
        unseen: i64,
        exposed: i64,
        lapses: u64,
        rng: u64,
        next_char: usize,
    }

    impl Gui {
        fn new(seed: u64) -> Self {
            let t0 = Instant::now();
            FRONT_GROUP.with(|g| g.set(Some(GROUP)));
            Self {
                app: raw_alt_app(t0),
                now: t0,
                epoch: t0,
                queue: Vec::new(),
                pending: Vec::new(),
                spoiled: false,
                masked: 0,
                unproven: 0,
                unseen: 0,
                exposed: 0,
                lapses: 0,
                rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
                next_char: 0,
            }
        }

        fn below(&mut self, n: usize) -> usize {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            usize::try_from(self.rng % u64::try_from(n).expect("small")).expect("small")
        }

        fn attached(&self) -> bool {
            status(&self.app).attached
        }

        fn optin(&self) -> bool {
            self.app.aos_unseen_on(WID)
        }

        /// The keys the live recording logged, and how many of them unseen.
        fn keys_logged(&self) -> (usize, usize) {
            if !self.attached() {
                return (0, 0);
            }
            let log = exported_log(&self.app, self.epoch);
            let keys = log
                .iter()
                .filter(|e| matches!(e, MomentumEvent::Key { .. }))
                .count();
            let unseen = log
                .iter()
                .filter(|e| {
                    matches!(
                        e,
                        MomentumEvent::Key {
                            echo: KeyEcho::Unseen,
                            ..
                        }
                    )
                })
                .count();
            (keys, unseen)
        }

        fn project(&self) -> State {
            let ws = &self.app.windows[&WID];
            let held = held_keys(&ws.aos);
            assert_eq!(
                ws.fx.cursor().momentum.aos_holding(),
                held > 0,
                "the host holds exactly while the window does"
            );
            assert_eq!(held, self.queue.len(), "the harness saw every hold");
            let echoed = self.echoed_prefix();
            let mut s = State::new();
            for (name, v) in [
                ("att", i64::from(self.attached())),
                ("held", i64::try_from(held).expect("small")),
                ("echoed", i64::try_from(echoed).expect("small")),
                ("glow", i64::from(ws.fx.cursor().momentum.aos_tainted())),
                ("leak", 0),
                ("unproven", self.unproven),
                ("gap", 0),
                ("dirty", 0),
                ("optin", i64::from(self.optin())),
                ("masked", self.masked),
                ("unseen", self.unseen),
                ("exposed", self.exposed),
            ] {
                s.insert(name, v);
            }
            s
        }

        /// The next char the walk types.
        fn next(&mut self) -> char {
            let c = ['a', 'b', 'c', 'a', 'd'][self.next_char % 5];
            self.next_char += 1;
            c
        }

        /// Makes `m`'s GUI calls, reconciles the harness's queue with what the window did, and
        /// names the model's actions for what the harness saw: a key held or refused, an echo or
        /// a mask it fed in place, keys the log gained (seen or unseen), a grant given or ended, a
        /// recording that stopped or started.
        fn act(&mut self, m: Move) -> Vec<&'static str> {
            self.now += ms(7);
            let now = self.now;
            let was_attached = self.attached();
            let optin_before = self.optin();
            let (logged_before, unseen_before) = self.keys_logged();
            let held_before = held_keys(&self.app.windows[&WID].aos);
            let echoed_before = self.echoed_prefix();
            let blinded = self.queue.iter().any(|k| k.blind);
            let mut seen = Vec::new();
            let mut lapse_key = false;
            let mut bare = false;
            match m {
                Move::Type if was_attached && self.queue.len() >= 2 => {}
                Move::Type => {
                    let c = self.next();
                    key(&mut self.app, c, now);
                    self.pending.push(c);
                }
                Move::Echo if blinded => {}
                Move::Echo => {
                    if let Some(&c) = self.pending.get(echoed_before) {
                        feed(&self.app, c.to_string().as_bytes());
                        if !self.spoiled {
                            self.queue[echoed_before].echoed = true;
                        }
                    }
                }
                Move::Mask if blinded => {}
                Move::Mask => {
                    if !self.queue.is_empty() {
                        feed(&self.app, b"*");
                        // In place, the '*' lands on the first unechoed key's cell.
                        if !self.spoiled && self.masked == 0 && echoed_before < self.queue.len() {
                            self.masked = 1;
                            seen.push("Mask");
                        }
                        self.spoiled = true;
                    }
                }
                Move::Frame => self.app.redraw_window_at(WID, now),
                Move::Late => {
                    if let Some(first) = self.queue.first() {
                        self.now = self.now.max(first.at + ECHO_PROOF_BOUND);
                    }
                    self.app.redraw_window_at(WID, self.now);
                }
                Move::Newline => {
                    feed(&self.app, b"\r\n");
                    self.spoiled = true;
                }
                Move::Attach => {
                    if !was_attached {
                        self.app
                            .aos_attach_at(MomentumCode::Shipped, now)
                            .expect("attaches");
                        self.epoch = now;
                    }
                }
                Move::Detach => {
                    if was_attached {
                        let _ = self.app.aos_detach_now(now).expect("detaches");
                    }
                }
                // Under the grant a held run takes blind keys too, up to the model's cap.
                Move::Bare if optin_before && self.queue.len() >= 2 => {}
                Move::Bare => {
                    press(&mut self.app, shift_enter(), now);
                    bare = true;
                }
                Move::OptIn => {
                    if !optin_before && self.app.aos_grant_for_test(WID).is_ok() {
                        seen.push("OptIn");
                    }
                }
                Move::Lapse if !optin_before => {}
                Move::Lapse => {
                    self.lapses += 1;
                    seen.push(if held_before > 0 { "Refute" } else { "Lapse" });
                    if self.lapses % 2 == 1 {
                        self.app.aos_unseen_stop(WID, now);
                    } else {
                        FRONT_GROUP.with(|g| g.set(Some(OTHER_GROUP)));
                        let c = self.next();
                        key(&mut self.app, c, now);
                        self.pending.push(c);
                        FRONT_GROUP.with(|g| g.set(Some(GROUP)));
                        lapse_key = true;
                    }
                }
            }
            let held = held_keys(&self.app.windows[&WID].aos);
            let attached = self.attached();
            let (logged, unseen) = self.keys_logged();
            let typed =
                (matches!(m, Move::Type) || lapse_key) && self.pending.len() > self.queue.len();
            // Logged at its own press: the log gained one key, with nothing held before or after.
            let at_press = (typed || bare)
                && was_attached
                && attached
                && held_before == 0
                && held == 0
                && logged == logged_before + 1;
            if (typed || bare) && held == held_before + 1 {
                if bare {
                    self.pending.push('\0');
                }
                self.queue.push(HeldKey {
                    at: now,
                    echoed: false,
                    blind: bare,
                });
                seen.push("Hold");
            } else if at_press {
                if typed {
                    // The harness feeds an echo only after a press: this key had none.
                    self.pending.pop();
                    self.unproven = 1;
                }
                seen.push("Key");
            } else if typed || (bare && was_attached) {
                if typed {
                    self.pending.pop();
                }
                seen.push("Private");
            } else if bare {
                seen.push("Key");
            }
            if matches!(m, Move::Echo) && echoed_before < self.echoed_prefix() {
                seen.push("Echo");
            }
            if was_attached && attached && !matches!(m, Move::Attach) {
                let gained_unseen = unseen.saturating_sub(unseen_before);
                if gained_unseen > 0 {
                    assert_eq!(
                        gained_unseen,
                        self.queue.len(),
                        "an unseen release logs the whole hold"
                    );
                    if !optin_before {
                        self.unseen = 1;
                    }
                    if self.masked == 1 {
                        self.exposed = 1;
                    }
                    self.queue.clear();
                    self.pending.clear();
                    seen.push("Unproven");
                }
                let released =
                    logged.saturating_sub(logged_before + usize::from(at_press) + gained_unseen);
                for k in self.queue.drain(..released.min(self.queue.len())) {
                    if !k.echoed {
                        self.unproven = 1;
                    }
                    seen.push("Release");
                }
                self.pending.drain(..released.min(self.pending.len()));
            }
            match m {
                Move::Frame | Move::Late if was_attached && !attached => seen.push("Refute"),
                Move::Attach if !was_attached => seen.push("Attach"),
                Move::Detach if was_attached => seen.push("Detach"),
                _ => {}
            }
            if held == 0 {
                self.queue.clear();
                self.pending.clear();
                self.spoiled = false;
                self.masked = 0;
            }
            seen
        }

        /// How many of the held keys, oldest first, the harness fed in place.
        fn echoed_prefix(&self) -> usize {
            self.queue.iter().take_while(|k| k.echoed).count()
        }
    }

    /// The state `model` reaches from `prev` by taking `steps` in order, or `None` where one of them
    /// is not enabled.
    fn take(model: &Model, prev: &State, steps: &[&'static str]) -> Option<State> {
        let mut st = prev.clone();
        steps.iter().all(|a| model.fire(a, &mut st)).then_some(st)
    }

    /// Walks `seed`'s moves drawn from `moves`, checking that `model` takes each step the harness
    /// saw, by name, to the window's projection; returns the actions taken.
    fn walk(model: &Model, seed: u64, steps: usize, moves: &[Move]) -> BTreeSet<&'static str> {
        let mut gui = Gui::new(seed);
        let mut st = gui.project();
        let mut fired = BTreeSet::new();
        for i in 0..steps {
            let m = moves[gui.below(moves.len())];
            let seen = gui.act(m);
            let got = gui.project();
            assert_eq!(
                take(model, &st, &seen).as_ref(),
                Some(&got),
                "seed {seed} step {i} {m:?} saw {seen:?}: {st:?}"
            );
            fired.extend(seen);
            st = got;
        }
        fired
    }

    #[test]
    fn the_window_logs_only_keys_the_harness_echoed_in_lock_step_with_the_model() {
        let model = aos_key_hold_model();
        let mut fired = BTreeSet::new();
        for seed in 1..=64 {
            fired.extend(walk(&model, seed, 40, &MOVES));
        }
        for action in [
            "Attach", "Detach", "Key", "Private", "Hold", "Echo", "Release", "Refute", "OptIn",
            "Lapse", "Mask", "Unproven",
        ] {
            assert!(
                fired.contains(action),
                "the walk took `{action}`: {fired:?}"
            );
        }
    }

    /// Rule 4's binding: with no grant ever given, the walk never logs a key unseen and keeps the
    /// old laws, every stop where it was.
    #[test]
    fn without_a_grant_the_window_keeps_the_proof_it_had() {
        let model = aos_key_hold_model();
        let ungranted: Vec<Move> = MOVES.into_iter().filter(|&m| m != Move::OptIn).collect();
        let mut fired = BTreeSet::new();
        for seed in 1..=64 {
            fired.extend(walk(&model, seed, 40, &ungranted));
        }
        for action in ["OptIn", "Lapse", "Unproven"] {
            assert!(!fired.contains(action), "{action}: {fired:?}");
        }
        for action in ["Hold", "Release", "Refute", "Mask"] {
            assert!(fired.contains(action), "{action}: {fired:?}");
        }
    }

    #[test]
    fn negative_control_a_release_on_a_frame_that_proves_nothing() {
        let model = aos_key_hold_model();
        let mutant = interp::with_buggy(&model, 2);
        let mut gui = Gui::new(1);
        assert_eq!(gui.act(Move::Type), ["Hold"]);
        let prev = gui.project();
        RELEASE_ON_PENDING.with(|on| on.set(true));
        let seen = gui.act(Move::Frame);
        RELEASE_ON_PENDING.with(|on| on.set(false));
        let got = gui.project();
        assert_eq!(seen, ["Release"], "the log gained the key no frame proved");
        assert_eq!(got["unproven"], 1, "the harness never fed its echo");
        assert_eq!(
            take(&model, &prev, &seen),
            None,
            "the healthy model refuses it"
        );
        assert_eq!(
            take(&mutant, &prev, &seen),
            Some(got.clone()),
            "Buggy 2 takes it"
        );
        assert!(!mutant.check_invariant("OnlyEchoedKeysLogged", &got));
    }

    #[test]
    fn negative_control_a_text_key_logged_at_its_press() {
        let model = aos_key_hold_model();
        let mutant = interp::with_buggy(&model, 5);
        let mut gui = Gui::new(1);
        let prev = gui.project();
        ECHO_HOLD_OFF.with(|off| off.set(true));
        let seen = gui.act(Move::Type);
        ECHO_HOLD_OFF.with(|off| off.set(false));
        let got = gui.project();
        assert_eq!(seen, ["Key"], "the log gained the key at its own press");
        assert_eq!(got["unproven"], 1, "the harness never fed its echo");
        assert_ne!(
            take(&model, &prev, &seen),
            Some(got.clone()),
            "the healthy model's Key logs only a key that needs no echo"
        );
        assert_eq!(
            take(&mutant, &prev, &seen),
            Some(got.clone()),
            "Buggy 5 takes it"
        );
        assert!(!mutant.check_invariant("OnlyEchoedKeysLogged", &got));
    }

    /// The healthy model refuses `steps` from `prev` to `got`, dial `dial`'s mutant takes them, and
    /// its state breaks `law`.
    fn only_the_mutant_takes(
        dial: i64,
        law: &str,
        prev: &State,
        steps: &[&'static str],
        got: &State,
    ) {
        let model = aos_key_hold_model();
        let mutant = interp::with_buggy(&model, dial);
        assert_ne!(
            take(&model, prev, steps).as_ref(),
            Some(got),
            "the healthy model refuses {steps:?} to {got:?}"
        );
        assert_eq!(
            take(&mutant, prev, steps).as_ref(),
            Some(got),
            "Buggy {dial} takes {steps:?}"
        );
        assert!(!mutant.check_invariant(law, got), "Buggy {dial}: {law}");
    }

    #[test]
    fn negative_control_an_unseen_release_without_a_grant() {
        let mut gui = Gui::new(1);
        UNSEEN_WITHOUT_GRANT.with(|on| on.set(true));
        let held = gui.act(Move::Type);
        let prev = gui.project();
        let seen = gui.act(Move::Late);
        UNSEEN_WITHOUT_GRANT.with(|on| on.set(false));
        let got = gui.project();
        assert_eq!(held, ["Hold"]);
        assert_eq!(seen, ["Unproven"], "the log gained the key unseen");
        assert_eq!((got["unseen"], got["optin"]), (1, 0), "with no grant");
        only_the_mutant_takes(6, "UnseenOnlyWithOptIn", &prev, &seen, &got);

        let mut gui = Gui::new(1);
        assert_eq!(gui.act(Move::Type), ["Hold"]);
        assert_eq!(
            gui.act(Move::Late),
            ["Refute"],
            "negative control: the same keys without the mutant stop the recording"
        );
    }

    #[test]
    fn negative_control_an_unseen_release_over_a_mask() {
        let mut gui = Gui::new(1);
        assert_eq!(gui.act(Move::OptIn), ["OptIn"]);
        assert_eq!(gui.act(Move::Type), ["Hold"]);
        assert_eq!(gui.act(Move::Mask), ["Mask"]);
        let prev = gui.project();
        UNSEEN_OVER_MASK.with(|on| on.set(true));
        let seen = gui.act(Move::Frame);
        UNSEEN_OVER_MASK.with(|on| on.set(false));
        let got = gui.project();
        assert_eq!(seen, ["Unproven"], "the masked key was logged unseen");
        assert_eq!(got["exposed"], 1);
        only_the_mutant_takes(7, "MaskedKeyNeverLogged", &prev, &seen, &got);

        let mut gui = Gui::new(1);
        for (m, want) in [
            (Move::OptIn, "OptIn"),
            (Move::Type, "Hold"),
            (Move::Mask, "Mask"),
        ] {
            assert_eq!(gui.act(m), [want]);
        }
        assert_eq!(
            gui.act(Move::Frame),
            ["Refute"],
            "negative control: the healthy grant stops at the mask"
        );
    }

    #[test]
    fn negative_control_a_grant_that_outlives_its_attach() {
        let mut gui = Gui::new(1);
        assert_eq!(gui.act(Move::OptIn), ["OptIn"]);
        let prev = gui.project();
        crate::aos_host::GRANT_OUTLIVES_ATTACH.with(|on| on.set(true));
        let seen = gui.act(Move::Detach);
        let got = gui.project();
        crate::aos_host::GRANT_OUTLIVES_ATTACH.with(|on| on.set(false));
        assert_eq!(seen, ["Detach"]);
        assert_eq!((got["att"], got["optin"]), (0, 1), "the grant outlived it");
        only_the_mutant_takes(8, "OptInEndsWithItsAttach", &prev, &seen, &got);

        let mut gui = Gui::new(1);
        assert_eq!(gui.act(Move::OptIn), ["OptIn"]);
        assert_eq!(gui.act(Move::Detach), ["Detach"]);
        assert_eq!(
            gui.project()["optin"],
            0,
            "negative control: a healthy detach ends the grant"
        );
    }
}
