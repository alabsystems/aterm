// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{VI_ACTIVE_TERMINALS, vi_any_active};
use crate::keybinding::Action;
use crate::{App, WindowId, term_lock};
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};

/// `VI_ACTIVE_TERMINALS` is process-wide (a cross-thread toggle must be
/// visible to the key path), so every test that toggles serializes here and
/// one test's transient +1 is never observed as another's count.
static VI_MIRROR_SERIAL: Mutex<()> = Mutex::new(());

/// A copy-mode tab elsewhere must not make both vi gates take this focused
/// terminal's mutex on every key (queueing typing behind its PTY reader).
/// Hold that mutex across both gates: the unrelated vi tab stays active,
/// yet the gates answer without waiting.
#[test]
fn another_sessions_vi_mode_does_not_lock_the_typing_terminal() {
    let _serial = VI_MIRROR_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut app = App::headless_for_test();
    let typing_wid = WindowId(0);
    let vi_wid = WindowId(7);
    app.install_window_state(vi_wid, crate::stub_session(7), 24, 80);
    app.toggle_vi_mode(vi_wid);
    assert!(vi_any_active());
    assert!(app.vi_target_active(vi_wid));

    let typing_term = app.front_terminal(typing_wid).unwrap().term.clone();
    // WHO RELEASED THE MUTEX IS THE VERDICT, NOT HOW LONG THE GATES TOOK (the
    // load-sensitive test audit of 2026-09-27). The gates used to be timed
    // against 500 ms while this holder let go on its own after 2 s: a test
    // thread kept off the CPU for half a second failed a correct tree, and
    // one stalled past 1.5 s before `started` let a gate that blocked on the
    // mutex pass. Now the holder keeps the mutex until this thread says so,
    // and reports whether it was told. A gate that waits for the mutex
    // cannot return until the holder gives up at `HOLD_AT_MOST` on its own,
    // and then the holder was never told — so that regression fails on
    // every run, and no stall short of the bound can fail a correct one.
    const HOLD_AT_MOST: Duration = aterm_spec::wait::MUST_HAPPEN;
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = term_lock(&typing_term);
        locked_tx.send(()).unwrap();
        release_rx.recv_timeout(HOLD_AT_MOST).is_ok()
    });
    locked_rx.recv_timeout(HOLD_AT_MOST).unwrap();

    let ev = KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::KeyL),
        Key::Character(SmolStr::new("l")),
        Some(SmolStr::new("l")),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    let mods = ModifiersState::empty();
    let started = Instant::now();
    let active = app.vi_target_active(typing_wid);
    let repeat = app.vi_repeat_action(typing_wid, mods, &ev, active);
    let swallowed = app.on_key_vi_mode(typing_wid, mods, &ev, active);
    let elapsed = started.elapsed();

    let _ = release_tx.send(());
    let held_throughout = holder.join().unwrap();
    assert!(!active);
    assert!(repeat.is_none());
    assert!(!swallowed, "an ordinary key still reaches its PTY");
    assert!(
        held_throughout,
        "the vi gates waited {elapsed:?} for the typing terminal's mutex: they \
         returned only once its holder gave it up unasked"
    );
    let vi_active = app.vi_target_active(vi_wid);
    assert!(app.vi_repeat_action(vi_wid, mods, &ev, vi_active).is_some());
    assert!(app.on_key_vi_mode(vi_wid, mods, &ev, vi_active));
    app.toggle_vi_mode(vi_wid);
}

/// Native focus cannot inherit terminal authority from a live hidden shell:
/// the negative half of
/// `vi_mirror_tracks_the_engine_so_the_key_path_can_skip_the_term_lock`.
/// The terminal stays in the pool, but no resolver, global handle, or
/// terminal-only action may select it implicitly.
#[test]
fn native_focus_has_no_terminal_capability_or_vi_fallback() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let hidden = app
        .front_terminal(wid)
        .expect("initial terminal")
        .term
        .clone();

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(app.front_terminal(wid).is_none());
    assert_eq!(app.focused_session_id(wid), None);
    assert!(app.windows[&wid].active_terminal.is_none());
    assert!(app.active_handle.lock().unwrap().is_none());

    app.dispatch_action(wid, Action::ToggleViMode);
    assert!(
        !term_lock(&hidden).vi_is_active(),
        "terminal-only command must not fall back to the hidden shell"
    );
}

/// The GUI-side vi mirror tracks the engine exactly, because the two
/// per-keystroke vi gates answer from it instead of taking the terminal
/// mutex twice per key. Off ⇒ `vi_any_active()` is false and neither gate
/// touches a terminal; toggling on raises it (or keys would leak to the PTY
/// in copy-mode), and toggling off lowers it (or every key pays both locks).
#[test]
fn vi_mirror_tracks_the_engine_so_the_key_path_can_skip_the_term_lock() {
    let _serial = VI_MIRROR_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut app = App::headless_for_test();
    let wid = WindowId(7);
    app.install_window_state(wid, crate::stub_session(7), 24, 80);
    let engine_active = |app: &App| {
        app.front_terminal(wid)
            .is_some_and(|terminal| term_lock(&terminal.term).vi_is_active())
    };
    let base = VI_ACTIVE_TERMINALS.load(Ordering::Relaxed);

    assert!(!engine_active(&app));
    app.dispatch_action(wid, Action::ToggleViMode);
    assert!(engine_active(&app));
    assert_eq!(
        VI_ACTIVE_TERMINALS.load(Ordering::Relaxed),
        base + 1,
        "entering copy-mode must open the gate"
    );
    assert!(vi_any_active());
    assert!(app.vi_target_active(wid));

    app.dispatch_action(wid, Action::ToggleViMode);
    assert!(!engine_active(&app));
    assert_eq!(
        VI_ACTIVE_TERMINALS.load(Ordering::Relaxed),
        base,
        "leaving copy-mode must close the gate again"
    );
    assert!(!app.vi_target_active(wid));
}

/// The mirror is a count (two windows can hold two terminals in copy-mode)
/// and never wraps: an extra "now inactive" report saturates at zero.
#[test]
fn vi_mirror_never_wraps_below_zero() {
    let _serial = VI_MIRROR_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let base = VI_ACTIVE_TERMINALS.load(Ordering::Relaxed);
    for _ in 0..(base + 2) {
        super::vi_note_toggled(false);
    }
    assert_eq!(VI_ACTIVE_TERMINALS.load(Ordering::Relaxed), 0);
    assert!(!vi_any_active());
    // Restore whatever the process-wide count was, so a leaked +1 from an
    // unrelated test is not silently swallowed by this one.
    for _ in 0..base {
        super::vi_note_toggled(true);
    }
}

/// A terminal that dies in copy-mode retires its contribution. `vi_toggle`
/// is the mirror's only other decrement, so otherwise closing a tab in
/// copy-mode would pin the gate "maybe active" for the rest of the process
/// and send both per-keystroke vi checks back to the terminal mutex.
#[test]
fn a_terminal_dying_in_copy_mode_does_not_strand_the_mirror() {
    let _serial = VI_MIRROR_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let base = VI_ACTIVE_TERMINALS.load(Ordering::Relaxed);

    // A terminal enters copy-mode, then is torn down while still in it.
    super::vi_note_toggled(true);
    assert_eq!(VI_ACTIVE_TERMINALS.load(Ordering::Relaxed), base + 1);
    super::vi_note_terminal_gone(true);

    assert_eq!(
        VI_ACTIVE_TERMINALS.load(Ordering::Relaxed),
        base,
        "the dead terminal's +1 must not outlive it"
    );

    // A terminal torn down while not in copy-mode contributed nothing and must
    // not over-decrement someone else's live copy-mode.
    super::vi_note_toggled(true);
    super::vi_note_terminal_gone(false);
    assert_eq!(
        VI_ACTIVE_TERMINALS.load(Ordering::Relaxed),
        base + 1,
        "a terminal that was not in vi must not decrement a live one"
    );
    assert!(
        vi_any_active(),
        "the still-live copy-mode terminal holds the gate"
    );
    super::vi_note_toggled(false);
    assert_eq!(VI_ACTIVE_TERMINALS.load(Ordering::Relaxed), base);
}
