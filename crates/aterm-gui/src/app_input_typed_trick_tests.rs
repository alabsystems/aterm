// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{TrickLineFire, classify_press};
use crate::input::{InputEvent, PasteFraming, Source};
use crate::{App, WindowId, term_lock};
use aterm_effects::word_decorations::TrickFlashPhase;
use aterm_lexicon::Trick;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
use std::time::{Duration, Instant};

fn key(character: char) -> InputEvent {
    chord(character, Modifiers::empty())
}

fn chord(character: char, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(character),
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn named(named: NamedKey) -> InputEvent {
    InputEvent::Key {
        key: Key::Named(named),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn type_word(app: &mut App, wid: WindowId, word: &str) {
    for c in word.chars() {
        let ev = if c == ' ' {
            named(NamedKey::Space)
        } else {
            key(c)
        };
        app.input(wid, ev, Source::Human);
    }
}

/// Fixture: the resident pet on glass, the sparkle master on, Full motion,
/// and one capture so the word engine holds a caret for the front session
/// (the flash stores nothing for a pane it has never scanned).
fn pet_app() -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty pet".into());
    app.config.motion = Some("full".into());
    app.windows.get_mut(&wid).expect("window").focused = true;
    app.recompute_sparkle();
    assert!(app.sparkle.is_some(), "fixture: the sparkle master is on");
    assert!(app.trail_is_kitty_pet(), "fixture: the pet is the trail");
    let session = app.front_terminal(wid).expect("front terminal").session;
    app.capture_words(wid, Instant::now());
    (app, wid, session)
}

fn pending(app: &App, wid: WindowId) -> Option<Trick> {
    app.windows[&wid].fx.companion().brain().pending_trick()
}

/// Where the trick flash stands, read at `t0`: an instant the test took
/// before its first key. Every stamp a key or a present leaves is at or after
/// `t0`, and the phase reads an earlier clock as no time elapsed, so this is
/// the slot's state — none, unfound, found, revoked — with no clock in it.
/// Read at `Instant::now()` instead, a stall between a key and the read (a
/// loaded test process) ages a pending flash past its 750 ms give-up bound
/// and a found one past its 1.1 s window, and the read says `Idle`.
fn flash(app: &App, wid: WindowId, t0: Instant) -> TrickFlashPhase {
    app.windows[&wid].fx.decos().trick_flash_phase(t0)
}

/// Whether the pet grieves a failed command (12 ms long), judged at `typed`:
/// an instant the test took before the line's keys. The forgiveness window
/// (`TRICK_SUBMIT_FORGIVEN`, 2 s from the line's Enter) reads an earlier
/// clock as no time elapsed, so a stalled test process cannot age the line
/// out of it the way a read at `Instant::now()` could.
fn grieves_at(app: &mut App, wid: WindowId, typed: Instant) -> bool {
    crate::app_render::pet_fails_a_command_for_test(
        app.windows.get_mut(&wid).expect("window"),
        typed,
        Some(12),
    )
}

/// The line-abort set is exactly ⌃C ⌃Z ⌃\ ⌃J ⌃M with Ctrl and no Alt or
/// Super; Shift does not disqualify (⌃⇧C is 0x03 too). Neighbours (⌃A, ⌃K,
/// ⌃U, ⌃W, the bare letter, Alt and Super chords, Enter, Esc) are not, and
/// an abort chord is still a `brk` to feeds that only know that bit.
#[test]
fn classify_press_marks_the_ctrl_abort_chords_as_line_abort_and_nothing_else() {
    for c in ['c', 'z', '\\', 'j', 'm'] {
        let abort = chord(c, Modifiers::CTRL);
        let class = classify_press(&abort);
        assert!(class.line_abort, "⌃{c} aborts the line");
        assert!(class.brk, "⌃{c} is still a break to the other feeds");
        assert!(
            !class.kill_key && !class.enter_like,
            "⌃{c} is no kill and no Enter"
        );
        assert!(
            classify_press(&chord(c, Modifiers::CTRL | Modifiers::SHIFT)).line_abort,
            "⌃⇧{c} encodes the same byte"
        );
        assert!(
            !classify_press(&chord(c, Modifiers::CTRL | Modifiers::ALT)).line_abort,
            "⌥⌃{c} is a bound chord, not an interrupt"
        );
        assert!(
            !classify_press(&chord(c, Modifiers::CTRL | Modifiers::SUPER)).line_abort,
            "⌘⌃{c} is a bound chord, not an interrupt"
        );
        assert!(
            !classify_press(&key(c)).line_abort,
            "a bare {c} is a typed glyph"
        );
    }
    for c in ['a', 'e', 'k', 'u', 'w', 'l', 'r', 'd'] {
        assert!(
            !classify_press(&chord(c, Modifiers::CTRL)).line_abort,
            "⌃{c} is not an abort"
        );
    }
    assert!(!classify_press(&named(NamedKey::Enter)).line_abort);
    assert!(!classify_press(&named(NamedKey::Escape)).line_abort);
    assert!(!classify_press(&InputEvent::KeySequence(vec![0x03])).line_abort);
    // The job-control pair is exactly ⌃Z and ⌃\, always inside the abort
    // set; the screen clear is exactly ⌃L, and a chord to everyone else.
    for c in ['c', 'z', '\\', 'j', 'm', 'l', 'a', 'u', 'w'] {
        let ev = chord(c, Modifiers::CTRL);
        let class = classify_press(&ev);
        assert_eq!(class.job_chord, matches!(c, 'z' | '\\'), "⌃{c} job chord");
        assert!(
            !class.job_chord || class.line_abort,
            "⌃{c}: a job chord aborts"
        );
        assert_eq!(class.redraw_chord, c == 'l', "⌃{c} redraw chord");
    }
    let ctrl_l = chord('l', Modifiers::CTRL);
    let redraw = classify_press(&ctrl_l);
    assert!(
        redraw.brk && !redraw.kill_key && !redraw.line_abort,
        "⌃L is a plain chord to the other feeds"
    );
    assert!(!classify_press(&chord('l', Modifiers::CTRL | Modifiers::ALT)).redraw_chord);
    assert!(!classify_press(&chord('z', Modifiers::CTRL | Modifiers::SUPER)).job_chord);
    assert!(!classify_press(&key('l')).redraw_chord);
    assert!(!classify_press(&key('z')).job_chord);
}

/// `sit␣` through the real press path: the pet's latch holds `Sit` (a
/// request, not yet performed) and the flash is pending, noted before the
/// Space's echo could be scanned.
#[test]
fn typing_sit_and_a_space_latches_the_pet_and_notes_the_flash() {
    let t0 = Instant::now();
    let (mut app, wid, session) = pet_app();
    type_word(&mut app, wid, "sit");
    assert_eq!(
        pending(&app, wid),
        None,
        "an open token commands nothing yet"
    );
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle);
    type_word(&mut app, wid, " ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit), "the boundary fires");
    assert_eq!(
        flash(&app, wid, t0),
        TrickFlashPhase::Pending,
        "the flash is noted on the input path, waiting for the echo"
    );
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Admitted {
            session,
            addressed: false
        },
        "a plain word on an unnamed line is TENTATIVE"
    );
}

/// A committed IME run is typed text: `sit ` as one commit fires at the
/// space inside it, and a commit's end is not a boundary (`si` committed,
/// then `t` and Space typed, is still `sit␣`).
#[test]
fn an_ime_commit_is_typed_text_and_its_end_is_not_a_boundary() {
    let t0 = Instant::now();
    let (mut app, wid, _) = pet_app();
    app.input(wid, InputEvent::Text("sit ".into()), Source::Human);
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "the space inside the commit fires"
    );
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Pending);

    let (mut app, wid, _) = pet_app();
    app.input(wid, InputEvent::Text("si".into()), Source::Human);
    assert_eq!(pending(&app, wid), None, "a run left open stays open");
    type_word(&mut app, wid, "t ");
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "the run continued across the commit's end"
    );
}

/// A name glued to its command, as an IME commits an unspaced script, is a
/// guess: `猫咪坐下␣` latches and flashes nothing, and prose after it drops it
/// unheard. A name after it, or Enter on a line that stayed pet talk, sends
/// it final, the command first or last. A thing behind the name, or a verb
/// that glued spells another word, is no call.
#[test]
fn a_name_glued_to_its_command_by_ime_commit_waits_for_a_name_or_enter() {
    let t0 = Instant::now();
    let (mut app, wid, session) = pet_app();
    app.input(wid, InputEvent::Text("猫咪坐下 ".into()), Source::Human);
    assert_eq!(pending(&app, wid), None, "a guess says nothing yet");
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle);
    app.input(wid, InputEvent::Text("猫咪 ".into()), Source::Human);
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Pending);
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Admitted {
            session,
            addressed: true
        },
        "a name vouches for it"
    );

    let (mut app, wid, _) = pet_app();
    app.input(wid, InputEvent::Text("坐下猫咪".into()), Source::Human);
    app.input(wid, named(NamedKey::Enter), Source::Human);
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.fx.companion_mut().revoke_trick();
    assert_eq!(pending(&app, wid), Some(Trick::Sit), "sent: final");

    for prose in ["猫咪坐下 很可爱", "猫咪玩具", "喂猫咪"] {
        let (mut app, wid, _) = pet_app();
        app.input(wid, InputEvent::Text(prose.into()), Source::Human);
        app.input(wid, named(NamedKey::Enter), Source::Human);
        assert_eq!(pending(&app, wid), None, "{prose:?}");
        assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle, "{prose:?}");
    }
}

/// ⇧⏎ is an Enter to the listener (`enter_like` is any Enter press): the
/// line is judged and submitted exactly as the bare key does it.
#[test]
fn shift_enter_submits_the_line_like_enter() {
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sit");
    app.input(
        wid,
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::SHIFT,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        Source::Human,
    );
    assert_eq!(pending(&app, wid), Some(Trick::Sit), "⇧⏎ judged the token");
    assert_eq!(
        app.windows[&wid].trick_listener.line_chars(),
        0,
        "…and started a fresh line"
    );
}

/// Typed provenance only: program output full of `sit`, a paste of `sit `,
/// and a controller's raw `send` bytes reach the terminal, not the
/// listener; raw bytes ending in a newline after a typed `sit` reset the
/// line without judging it (the shell got `sitxyz`).
#[test]
fn program_output_a_paste_and_raw_controller_bytes_never_command_the_cat() {
    let t0 = Instant::now();
    let (mut app, wid, session) = pet_app();
    term_lock(&app.pool.get(session).expect("session").term).process(b"sit sit sit\r\n");
    assert_eq!(pending(&app, wid), None, "screen content is not typing");

    app.input(
        wid,
        InputEvent::Paste("sit ".into(), PasteFraming::AtDrain),
        Source::Human,
    );
    assert_eq!(pending(&app, wid), None, "a paste never fires");
    assert!(
        app.windows[&wid].trick_listener.is_poisoned(),
        "…and the line now holds text the listener did not see"
    );

    // A fresh line (Enter resets), then a controller's raw bytes.
    app.input(wid, named(NamedKey::Enter), Source::Human);
    assert!(!app.windows[&wid].trick_listener.is_poisoned());
    let ctl = Source::Controller;
    app.input(wid, InputEvent::KeySequence(b"sit ".to_vec()), ctl);
    assert_eq!(
        pending(&app, wid),
        None,
        "`send` bytes are not typed characters"
    );
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle);
    assert!(
        app.windows[&wid].trick_listener.is_poisoned(),
        "…and the line now holds bytes the listener did not see"
    );
    type_word(&mut app, wid, "sit ");
    assert_eq!(
        pending(&app, wid),
        None,
        "`send 'echo '` then a typed `sleep␣` must not nap the cat"
    );

    // `sit` typed, then `send 'xyz\n'`: the newline in the raw payload resets without
    // evaluating the open token.
    app.input(wid, named(NamedKey::Enter), Source::Human);
    type_word(&mut app, wid, "sit");
    app.input(wid, InputEvent::KeySequence(b"xyz\n".to_vec()), ctl);
    assert_eq!(pending(&app, wid), None, "the line that ran was `sitxyz`");
    assert!(
        !app.windows[&wid].trick_listener.is_poisoned(),
        "the raw newline ended the line: the next one starts fresh"
    );
    type_word(&mut app, wid, "sit ");
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "…and fresh means it can fire"
    );
}

/// A controller `key` is a keypress: `aterm ctl key` types `sit␣` exactly
/// like a hand (no `src` branch anywhere on this path).
#[test]
fn a_controller_key_commands_exactly_like_a_human_key() {
    let (mut app, wid, _) = pet_app();
    let ctl = Source::Controller;
    for c in "sit".chars() {
        app.input(wid, key(c), ctl);
    }
    app.input(wid, named(NamedKey::Space), ctl);
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
}

/// `sit⏎`: Enter judges the open token (one final fire) and reports a
/// pet-only line, which the host turns into exit-127 forgiveness: the
/// shell's fast `command not found` does not grieve the cat. The same fast
/// failure after an ordinary line sulks.
#[test]
fn enter_on_sit_submits_a_pet_only_line_the_cat_forgives() {
    let (mut app, wid, _) = pet_app();
    let t0 = Instant::now();
    type_word(&mut app, wid, "sit");
    app.input(wid, named(NamedKey::Enter), Source::Human);
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "Enter closes the token"
    );
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::None,
        "the line is over; nothing of it is left to confirm or revoke"
    );
    assert!(
        !grieves_at(&mut app, wid, t0),
        "`sit: command not found` is forgiven — the whole line was pet talk"
    );

    // Negative control: the same failure after an ordinary command grieves,
    // judged at the same early instant.
    let (mut plain, wid, _) = pet_app();
    let t0 = Instant::now();
    type_word(&mut plain, wid, "ls");
    plain.input(wid, named(NamedKey::Enter), Source::Human);
    assert_eq!(pending(&plain, wid), None);
    assert!(
        grieves_at(&mut plain, wid, t0),
        "an ordinary failed line still sulks"
    );
}

/// One row of the gate table: the gate's name and the config edit that
/// closes it.
type GateCase = (&'static str, fn(&mut App));

/// The four gates, each inert alone: the sparkle master off, the tricks
/// sub-table off, Serious Mode, and a word on `ignore_words`. The listener
/// is still fed (its line state stays coherent), but the pet hears nothing
/// and no flash is noted.
#[test]
fn the_master_the_tricks_bit_serious_mode_and_ignore_words_are_each_inert() {
    let t0 = Instant::now();
    let setups: [GateCase; 4] = [
        ("sparkle master off", |app| {
            app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
                enabled: Some(false),
                ..Default::default()
            });
        }),
        ("tricks off", |app| {
            app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
                tricks: Some(crate::app_config::SparkleTricksConfig {
                    enabled: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }),
        ("serious mode", |app| {
            app.set_serious_mode(true);
        }),
        ("ignore_words", |app| {
            app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
                tricks: Some(crate::app_config::SparkleTricksConfig {
                    ignore_words: Some(vec!["Sit".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }),
    ];
    for (label, setup) in setups {
        let (mut app, wid, session) = pet_app();
        setup(&mut app);
        // Model the worker-prepared generation handoff, as the summon tests do.
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.sparkle_dirty = true;
        app.recompute_sparkle();
        type_word(&mut app, wid, "sit ");
        assert_eq!(pending(&app, wid), None, "{label}: the pet hears nothing");
        assert_eq!(
            flash(&app, wid, t0),
            TrickFlashPhase::Idle,
            "{label}: no flash"
        );
        assert!(
            app.windows[&wid].trick_listener.is_pure()
                && app.windows[&wid].trick_listener.line_chars() == 4,
            "{label}: the listener was fed all the same"
        );
        assert_eq!(
            app.windows[&wid].trick_fire,
            TrickLineFire::Withheld { session },
            "{label}: the fire is remembered as withheld"
        );
        // Enter on the still-pure line confirms the tentative fire in the
        // listener; the host must not let a withheld fire into the pet that
        // way…
        app.input(wid, named(NamedKey::Enter), Source::Human);
        assert_eq!(
            pending(&app, wid),
            None,
            "{label}: the Enter's confirmation latches nothing"
        );
        assert_eq!(
            app.windows[&wid].trick_fire,
            TrickLineFire::None,
            "{label}: the line is over"
        );
        // …and the pet-only submit is not forgiven behind a closed gate:
        // with the feature off the cat grieves, and an ignored `sit` is a
        // real command whose failure is the user's.
        let grieving = crate::app_render::pet_fails_a_command_for_test(
            app.windows.get_mut(&wid).expect("window"),
            Instant::now(),
            Some(12),
        );
        assert!(grieving, "{label}: the fast failure grieves");
    }
}

/// A withheld fire is never confirmed, by the pet's name either: `sit` on
/// `ignore_words`, then `kitty␣`; the listener confirms, the host's memory
/// says withheld, the pet stays deaf, and the memory is not upgraded.
#[test]
fn a_withheld_fire_is_not_confirmed_by_naming_the_pet() {
    let t0 = Instant::now();
    let (mut app, wid, session) = pet_app();
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        tricks: Some(crate::app_config::SparkleTricksConfig {
            ignore_words: Some(vec!["sit".into()]),
            ..Default::default()
        }),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();
    type_word(&mut app, wid, "sit kitty ");
    assert_eq!(
        pending(&app, wid),
        None,
        "the name confirms nothing withheld"
    );
    assert!(
        !app.windows[&wid].fx.companion().brain().trick_engaged(),
        "the brain was never told"
    );
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Withheld { session },
        "the memory is not upgraded by the confirmation"
    );
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle);
}

/// The gate is read at every event: a fire admitted with tricks on, then a
/// config generation turning them off mid-line; the confirming `kitty␣`
/// reaches no pet.
#[test]
fn a_gate_closed_mid_line_blocks_the_confirmation_of_an_admitted_fire() {
    let (mut app, wid, session) = pet_app();
    type_word(&mut app, wid, "sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Admitted {
            session,
            addressed: false
        }
    );
    // The tentative latch is dropped the way a tick's TTL or a drain
    // would drop it; the host memory stays, as it does on a reload.
    app.windows
        .get_mut(&wid)
        .expect("window")
        .fx
        .companion_mut()
        .revoke_trick();
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        tricks: Some(crate::app_config::SparkleTricksConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();
    type_word(&mut app, wid, "kitty ");
    assert_eq!(pending(&app, wid), None, "tricks are off: no confirmation");
    assert!(!app.windows[&wid].fx.companion().brain().trick_engaged());
}

/// An erased line forgets its fire. `kitty sit␣` withheld (an ignored word,
/// addressed, so no tentative fire to revoke), then ten Backspaces (the
/// listener reports the line empty), then `kitty⏎`: a pet-only line the cat
/// forgives, because the remembered fire was on text that no longer
/// exists. An addressed withheld fire is the one kind no revoke ends.
#[test]
fn an_erased_line_forgets_its_withheld_fire() {
    let (mut app, wid, session) = pet_app();
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        tricks: Some(crate::app_config::SparkleTricksConfig {
            ignore_words: Some(vec!["sit".into()]),
            ..Default::default()
        }),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();
    type_word(&mut app, wid, "kitty sit ");
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Withheld { session }
    );
    for _ in 0..10 {
        app.input(wid, named(NamedKey::Backspace), Source::Human);
    }
    assert_eq!(
        app.windows[&wid].trick_listener.line_chars(),
        0,
        "fixture: the line is provably empty"
    );
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::None,
        "the memory went with the text"
    );
    let t0 = Instant::now();
    type_word(&mut app, wid, "kitty");
    app.input(wid, named(NamedKey::Enter), Source::Human);
    assert!(
        !grieves_at(&mut app, wid, t0),
        "`kitty: command not found` is forgiven — nothing withheld is left on the line"
    );
}

/// `ignore_words` reads a fire's word and its command. A name glued to its
/// command fires the whole run, so listing `坐下` holds back `坐下`,
/// `猫咪坐下` and `坐下猫咪` alike, and listing the run holds back the run.
/// A listed name holds back nothing, and a listed command holds back no other
/// command glued to the same name. Each line names the pet after the word.
#[test]
fn ignore_words_holds_back_a_glued_run_by_its_command_never_by_its_name() {
    let t0 = Instant::now();
    for (listed, typed, fires) in [
        ("坐下", "坐下 猫咪 ", None),
        ("坐下", "猫咪坐下 猫咪 ", None),
        ("坐下", "坐下猫咪 猫咪 ", None),
        ("猫咪坐下", "猫咪坐下 猫咪 ", None),
        ("猫咪", "猫咪坐下 猫咪 ", Some(Trick::Sit)),
        ("坐下", "猫咪跳 猫咪 ", Some(Trick::Jump)),
    ] {
        let (mut app, wid, session) = pet_app();
        app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
            tricks: Some(crate::app_config::SparkleTricksConfig {
                ignore_words: Some(vec![listed.into()]),
                ..Default::default()
            }),
            ..Default::default()
        });
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.sparkle_dirty = true;
        app.recompute_sparkle();
        app.input(wid, InputEvent::Text(typed.into()), Source::Human);
        assert_eq!(pending(&app, wid), fires, "{listed:?} vs {typed:?}");
        if fires.is_none() {
            assert_eq!(
                app.windows[&wid].trick_fire,
                TrickLineFire::Withheld { session },
                "{typed:?}"
            );
            assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle, "{typed:?}");
        }
    }
}

/// The memory of a fire is keyed to its session. `sit␣` withheld on tab A,
/// then `kitty⏎` on tab B's first, pure line: B's pet-only submit is
/// forgiven. Back on A, the half-typed line is poisoned, so nothing fires
/// there until a reset.
#[test]
fn a_fire_remembered_on_one_tab_says_nothing_about_another_tabs_line() {
    let (mut app, wid, session_a) = pet_app();
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        tricks: Some(crate::app_config::SparkleTricksConfig {
            ignore_words: Some(vec!["sit".into()]),
            ..Default::default()
        }),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();
    type_word(&mut app, wid, "sit ");
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Withheld { session: session_a }
    );

    let session_b = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(session_b));
    assert_eq!(
        app.front_terminal(wid).expect("front").session,
        session_b,
        "fixture: tab B is in front"
    );
    let t0 = Instant::now();
    type_word(&mut app, wid, "kitty");
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::None,
        "A's memory was re-keyed away on B's first press"
    );
    app.input(wid, named(NamedKey::Enter), Source::Human);
    {
        assert!(
            !grieves_at(&mut app, wid, t0),
            "B's pet-only line is forgiven: nothing on it was withheld"
        );
    }

    app.switch_tab_in(wid, 0);
    assert_eq!(app.front_terminal(wid).expect("front").session, session_a);
    type_word(&mut app, wid, "jump ");
    assert!(
        app.windows[&wid].trick_listener.is_poisoned(),
        "A was left mid-line: the listener no longer knows what is on it"
    );
    assert_eq!(pending(&app, wid), None, "…so nothing fires there");
}

/// The gates do not need the pet: with only the flying kitty (or no trail)
/// on glass, a typed command flashes and nothing is latched.
#[test]
fn with_no_pet_on_glass_a_typed_command_only_flashes() {
    let t0 = Instant::now();
    for style in ["rainbow kitty flying", "off"] {
        let (mut app, wid, _) = pet_app();
        app.config.cursor_trail_style = Some(style.into());
        assert!(!app.trail_is_kitty_pet(), "{style}: fixture — no pet");
        type_word(&mut app, wid, "sit ");
        assert_eq!(pending(&app, wid), None, "{style}: no pet, no latch");
        assert!(
            !app.windows[&wid].fx.companion().brain().trick_engaged(),
            "{style}: the brain was never told"
        );
        assert_eq!(
            flash(&app, wid, t0),
            TrickFlashPhase::Pending,
            "{style}: the word flashes"
        );
    }
}

/// Tentative, then prose: `sit␣` fires tentatively; `tight␣` makes the line
/// impure, and the revoke reaches both consumers: the pet's latch and the
/// unfound flash are dropped.
#[test]
fn a_tentative_command_followed_by_prose_is_revoked_on_both_consumers() {
    let t0 = Instant::now();
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Pending);
    type_word(&mut app, wid, "tight ");
    assert_eq!(pending(&app, wid), None, "`sit tight` is prose");
    assert_eq!(
        flash(&app, wid, t0),
        TrickFlashPhase::Idle,
        "the pending flash is dropped"
    );
    assert_eq!(app.windows[&wid].trick_fire, TrickLineFire::None);
}

/// Addressed, then confirmed: `kitty sit␣` is final at once, and a
/// tentative `sit␣` followed by the pet's name is confirmed, the host
/// upgrading its memory so a later revoke spares the flash.
#[test]
fn naming_the_pet_makes_the_command_final() {
    let (mut app, wid, session) = pet_app();
    type_word(&mut app, wid, "kitty sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Admitted {
            session,
            addressed: true
        }
    );

    let (mut app, wid, session) = pet_app();
    type_word(&mut app, wid, "sit kitty ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Admitted {
            session,
            addressed: true
        },
        "the name confirmed the tentative fire"
    );
    // …and the brain was told: `revoke_trick` drops only a tentative latch,
    // so a confirmed one survives it (the tentative `sit␣` alone is dropped
    // by the same call).
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.fx.companion_mut().revoke_trick();
    assert_eq!(
        ws.fx.companion_mut().brain().pending_trick(),
        Some(Trick::Sit),
        "the confirmation reached the pet as CONFIRMED"
    );
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sit ");
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.fx.companion_mut().revoke_trick();
    assert_eq!(
        ws.fx.companion_mut().brain().pending_trick(),
        None,
        "negative control"
    );
}

/// Look never wins a phrase: `hey kitty sit` leaves the pet holding a
/// confirmed sit, and `pspsps kitty jump` a confirmed jump. The negative
/// control is the verb-first order, where the phrase law absorbs the
/// greeting.
#[test]
fn the_verb_after_a_greeting_is_the_request() {
    for (line, want) in [
        ("hey kitty sit ", Trick::Sit),
        ("pspsps kitty jump ", Trick::Jump),
    ] {
        let (mut app, wid, session) = pet_app();
        type_word(&mut app, wid, line);
        assert_eq!(pending(&app, wid), Some(want), "{line:?}");
        assert_eq!(
            app.windows[&wid].trick_fire,
            TrickLineFire::Admitted {
                session,
                addressed: true
            },
            "{line:?}"
        );
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.fx.companion_mut().revoke_trick();
        assert_eq!(
            ws.fx.companion_mut().brain().pending_trick(),
            Some(want),
            "{line:?} is final"
        );
    }
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sit hey ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit), "negative control");
}

/// A present the motion policy demotes ends a pending kitty flash: its tick
/// is unpaintable, and an unpaintable tick drops the slot
/// (`TrickFlash::emit`). The same frames at Full motion find the word.
///
/// The present runs on a pinned frame clock (`App::redraw_window_at`): the
/// flash gives up 750 ms after its key and ends 1.1 s after the frame that
/// found it, and on the wall clock a stalled test thread (a loaded
/// one-process suite) pushed the Full frame past one or the other, so it
/// read `Idle`. The negative control is that clock: the same Full present,
/// its frame seconds after the keys, finds a flash that already gave up. A
/// present that ignored the pin would find the word there.
#[test]
fn a_demoted_present_ends_a_pending_kitty_flash() {
    for (label, motion, late, fate) in [
        ("full", "full", false, TrickFlashPhase::Live),
        ("reduced", "reduced", false, TrickFlashPhase::Idle),
        ("full, a late frame", "full", true, TrickFlashPhase::Idle),
    ] {
        let t0 = Instant::now();
        let (mut app, wid, _) = pet_app();
        app.windows.get_mut(&wid).expect("window").present = Some(crate::PresentTarget::Cpu {
            surface: crate::present::CpuSurface::Scripted(Default::default()),
        });
        type_word(&mut app, wid, "sit ");
        let typed = Instant::now();
        assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Pending, "{label}");
        app.config.motion = Some(motion.into());
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(b"sit ");
        let epoch = term_lock(&term).damage_epoch();
        // Every key stamp is at or after `t0`, so `t0 + 300 ms` is inside the
        // give-up bound of each however long the keys took; the late frame is
        // past it however quickly they ran (a key queued behind a paste is
        // stamped at most 1.5 s ahead, `ORDERED_EGRESS_WITNESS_GRACE`).
        let frame = if late {
            typed + Duration::from_secs(10)
        } else {
            t0 + Duration::from_millis(300)
        };
        app.redraw_window_at(wid, frame);
        assert!(
            !app.windows[&wid].fx.decos().needs_rescan(epoch),
            "{label}: the present rescanned the damaged grid"
        );
        assert_eq!(flash(&app, wid, t0), fate, "{label}");
    }
}

/// The flash of an addressed fire is never taken back by a revoke: the
/// pet's `revoke_trick` is forwarded (a no-op on a confirmed latch), the
/// flash finishes, and the memory ends. A tentative fire's revoke reaches
/// both.
#[test]
fn a_revoke_spares_the_flash_of_an_addressed_fire() {
    let t0 = Instant::now();
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "kitty sit ");
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Pending);
    let now = Instant::now();
    let ws = app.windows.get_mut(&wid).expect("window");
    super::revoke_trick_line(ws, now);
    assert_eq!(
        ws.fx.decos().trick_flash_phase(t0),
        TrickFlashPhase::Pending,
        "an addressed word's flash finishes"
    );
    assert_eq!(
        ws.fx.companion_mut().brain().pending_trick(),
        Some(Trick::Sit),
        "a confirmed latch is a request the user finished making"
    );
    assert_eq!(ws.trick_fire, TrickLineFire::None, "the memory ends");

    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sit ");
    let now = Instant::now();
    let ws = app.windows.get_mut(&wid).expect("window");
    super::revoke_trick_line(ws, now);
    assert_eq!(
        ws.fx.decos().trick_flash_phase(t0),
        TrickFlashPhase::Idle,
        "a tentative word's pending flash is dropped"
    );
    assert_eq!(ws.fx.companion_mut().brain().pending_trick(), None);
}

/// ⌃C and ⌃U reset the line (fresh and pure), so an abandoned typo does not
/// block the next command; Esc and an arrow poison it until a reset.
#[test]
fn ctrl_c_and_ctrl_u_reset_the_line_where_esc_only_poisons_it() {
    let t0 = Instant::now();
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sot");
    app.input(wid, chord('c', Modifiers::CTRL), Source::Human);
    assert!(!app.windows[&wid].trick_listener.is_poisoned());
    assert_eq!(
        app.windows[&wid].trick_listener.line_chars(),
        0,
        "⌃C: a fresh line"
    );
    type_word(&mut app, wid, "sit ");
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "…on which `sit␣` fires"
    );

    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "npm sot");
    app.input(wid, chord('u', Modifiers::CTRL), Source::Human);
    assert!(app.windows[&wid].trick_listener.is_pure(), "⌃U: pure again");
    type_word(&mut app, wid, "sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));

    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sot");
    app.input(wid, named(NamedKey::Escape), Source::Human);
    assert!(
        app.windows[&wid].trick_listener.is_poisoned(),
        "Esc poisons"
    );
    type_word(&mut app, wid, " sit ");
    assert_eq!(pending(&app, wid), None, "nothing fires on a poisoned line");
    app.input(wid, chord('c', Modifiers::CTRL), Source::Human);
    type_word(&mut app, wid, "sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit), "…until the reset");

    // A tentative fire aborted by ⌃C is taken back.
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
    app.input(wid, chord('c', Modifiers::CTRL), Source::Human);
    assert_eq!(pending(&app, wid), None, "⌃C revokes the tentative latch");
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle);
}

/// A word-kill on the open token clears just the token (`sot` ⌃W `sit␣`
/// fires); ⌃K and forward Delete do nothing to the line at all.
#[test]
fn a_word_kill_clears_the_token_and_forward_kills_are_no_ops() {
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sot");
    app.input(wid, chord('w', Modifiers::CTRL), Source::Human);
    assert!(app.windows[&wid].trick_listener.is_pure());
    type_word(&mut app, wid, "sit ");
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "⌃W took the typo, not the line"
    );

    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "si");
    app.input(wid, chord('k', Modifiers::CTRL), Source::Human);
    app.input(wid, named(NamedKey::Delete), Source::Human);
    assert!(
        app.windows[&wid].trick_listener.is_pure()
            && !app.windows[&wid].trick_listener.is_poisoned(),
        "forward kills at the end of the line remove nothing"
    );
    type_word(&mut app, wid, "t ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
}

/// ⌃Z and ⌃\ reset only a line the listener can prove is empty. At a prompt
/// shells ignore them and keep the half-typed line, so `sot` ⌃Z `sit␣` is
/// `sotsit ` and must not fire (the line is poisoned until ⌃C), and a
/// tentative fire they interrupt is taken back. A poisoned line is never
/// proved empty, whatever its count reads: Esc then `sot`, ↑ and a paste each
/// leave text the count never saw. A clean empty line stays clean.
#[test]
fn a_job_chord_resets_only_a_provably_empty_line_and_breaks_any_other() {
    let t0 = Instant::now();
    for c in ['z', '\\'] {
        let (mut app, wid, _) = pet_app();
        type_word(&mut app, wid, "sot");
        app.input(wid, chord(c, Modifiers::CTRL), Source::Human);
        assert!(
            app.windows[&wid].trick_listener.is_poisoned(),
            "⌃{c}: the shell kept `sot`; the listener no longer knows the line"
        );
        type_word(&mut app, wid, "sit ");
        assert_eq!(pending(&app, wid), None, "⌃{c}: `sotsit ` never fires");
        app.input(wid, chord('c', Modifiers::CTRL), Source::Human);
        type_word(&mut app, wid, "sit ");
        assert_eq!(pending(&app, wid), Some(Trick::Sit), "⌃{c}: …until ⌃C");

        type PutText = fn(&mut App, WindowId);
        let unseen: [(&str, PutText); 3] = [
            ("Esc, then `sot`", |app, wid| {
                app.input(wid, named(NamedKey::Escape), Source::Human);
                type_word(app, wid, "sot");
            }),
            ("↑", |app, wid| {
                app.input(wid, named(NamedKey::ArrowUp), Source::Human);
            }),
            ("a paste", |app, wid| {
                let paste = InputEvent::Paste("sot".into(), PasteFraming::AtDrain);
                app.input(wid, paste, Source::Human);
            }),
        ];
        for (what, put_text) in unseen {
            let (mut app, wid, _) = pet_app();
            put_text(&mut app, wid);
            let listener = &app.windows[&wid].trick_listener;
            assert!(
                listener.is_poisoned() && listener.line_chars() == 0,
                "fixture: {what} poisons a line whose count reads 0"
            );
            app.input(wid, chord(c, Modifiers::CTRL), Source::Human);
            type_word(&mut app, wid, "sit ");
            assert_eq!(pending(&app, wid), None, "⌃{c} after {what}: never fires");
        }

        let (mut app, wid, _) = pet_app();
        app.input(wid, chord(c, Modifiers::CTRL), Source::Human);
        assert!(
            !app.windows[&wid].trick_listener.is_poisoned(),
            "⌃{c} on a clean empty line: nothing to break"
        );
        type_word(&mut app, wid, "sit ");
        assert_eq!(pending(&app, wid), Some(Trick::Sit), "⌃{c}: heard at once");

        let (mut app, wid, _) = pet_app();
        type_word(&mut app, wid, "sit ");
        assert_eq!(pending(&app, wid), Some(Trick::Sit));
        app.input(wid, chord(c, Modifiers::CTRL), Source::Human);
        assert_eq!(pending(&app, wid), None, "⌃{c} revokes the tentative latch");
        assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle);
        assert_eq!(app.windows[&wid].trick_fire, TrickLineFire::None);
    }
}

/// ⌃L is nothing to the line: the screen clears and the line is redrawn, so
/// an interrupted command still completes, a fire it follows is neither
/// revoked nor confirmed, and the line after a clear is heard.
#[test]
fn a_screen_clear_keeps_the_line() {
    let t0 = Instant::now();
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "si");
    app.input(wid, chord('l', Modifiers::CTRL), Source::Human);
    assert!(
        app.windows[&wid].trick_listener.is_pure()
            && !app.windows[&wid].trick_listener.is_poisoned()
            && app.windows[&wid].trick_listener.line_chars() == 2,
        "the line is exactly what it was"
    );
    type_word(&mut app, wid, "t ");
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "`si` ⌃L `t␣` is `sit␣`"
    );
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Pending);
    app.input(wid, chord('l', Modifiers::CTRL), Source::Human);
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "a clear revokes nothing"
    );
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Pending);

    let (mut app, wid, _) = pet_app();
    app.input(wid, chord('l', Modifiers::CTRL), Source::Human);
    type_word(&mut app, wid, "sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit), "cleared, then `sit␣`");
}

/// The pet obeys without the word engine: with every sparkle family off
/// (the master on) nothing can flash, and `sit␣` still latches the pet, as
/// `aterm help kitty` promises. The flash is best-effort; the command is not.
#[test]
fn with_every_sparkle_family_off_a_typed_command_moves_the_pet_and_cannot_flash() {
    let t0 = Instant::now();
    let (mut app, wid, session) = pet_app();
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        profanity: Some(crate::app_config::SparkleProfanityConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        feline: Some(crate::app_config::SparkleFelineConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();
    assert!(app.sparkle.is_none(), "fixture: no word engine");
    assert!(
        app.config.kitty_tricks_enabled(),
        "fixture: the gate is open"
    );
    type_word(&mut app, wid, "sit ");
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "the pet obeys either way"
    );
    assert_eq!(
        app.windows[&wid].trick_fire,
        TrickLineFire::Admitted {
            session,
            addressed: false
        }
    );
    assert_eq!(
        flash(&app, wid, t0),
        TrickFlashPhase::Idle,
        "nothing to flash with"
    );
}

/// A password prompt (canonical no-echo tty): every swallowed press poisons
/// the line (a passphrase starting `sleep ` must not nap the cat), and the
/// Enter that ends the secret resets it, so the next prompt starts clean.
#[cfg(unix)]
#[test]
fn a_no_echo_prompt_poisons_the_line_and_its_enter_resets_it() {
    use aterm_session::sink::SinkWriter;
    use std::sync::Arc;

    let (master, slave) = super::typed_kitty_summon_tests::pty_pair_with_lflag(|l| {
        *l &= !libc::ECHO;
    });
    let sink = Arc::new(SinkWriter::new(master));
    assert!(
        sink.tty_echo()
            .expect("a pty master answers")
            .swallows_input(),
        "fixture: the tty swallows input"
    );
    let mut app = App::headless_for_test_with_sink(sink.clone());
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty pet".into());
    app.recompute_sparkle();
    type_word(&mut app, wid, "sleep ");
    assert_eq!(pending(&app, wid), None, "a secret never moves the cat");
    assert!(
        app.windows[&wid].trick_listener.is_poisoned(),
        "the swallowed presses poisoned the line"
    );
    app.input(wid, named(NamedKey::Enter), Source::Human);
    assert!(
        !app.windows[&wid].trick_listener.is_poisoned(),
        "the Enter that ends the secret starts a clean line"
    );
    assert_eq!(
        pending(&app, wid),
        None,
        "…and judged nothing on its way out"
    );
    drop(app);
    drop(sink);
    // SAFETY: both fds are ours and open; the borrowed-fd sink did not
    // close the master.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// Serious Mode's drain resets the pet and ends the memory of the fire, but
/// leaves the line to the listener (fed on every press, it still knows the
/// line). So: prose typed before the drain keeps `sit␣` typed after it from
/// firing; a fresh line is not deaf once Serious Mode lifts; and a
/// pre-drain fire's confirmation reaches no pet.
#[test]
fn the_serious_drain_drops_the_hosts_memory_and_leaves_the_line_to_the_listener() {
    let t0 = Instant::now();
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
    app.set_serious_mode(true);
    {
        let ws = &app.windows[&wid];
        assert_eq!(
            ws.fx.companion().brain().pending_trick(),
            None,
            "the pet was reset"
        );
        assert_eq!(ws.trick_fire, TrickLineFire::None, "the memory ended");
        assert!(
            ws.trick_listener.is_pure()
                && !ws.trick_listener.is_poisoned()
                && ws.trick_listener.line_chars() == 4,
            "the listener still knows the line"
        );
    }
    // The pre-drain fire's confirmation, once Serious Mode lifts: the
    // listener confirms, the host remembers no admitted fire, no pet.
    app.set_serious_mode(false);
    assert!(app.trail_is_kitty_pet(), "fixture: the pet is back");
    type_word(&mut app, wid, "kitty ");
    assert_eq!(
        pending(&app, wid),
        None,
        "a fire drained with the pet is not confirmed into the new one"
    );

    // Prose typed before the drain, a command word typed after Serious Mode
    // lifts, on the same line: no fire.
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "npm run ");
    app.set_serious_mode(true);
    app.set_serious_mode(false);
    type_word(&mut app, wid, "sit ");
    assert_eq!(
        pending(&app, wid),
        None,
        "`npm run sit` is not a command to the cat, drain or no drain"
    );
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Idle);
    // The hand ends the line; the next one is judged as ever.
    app.input(wid, chord('c', Modifiers::CTRL), Source::Human);
    type_word(&mut app, wid, "sit ");
    assert_eq!(pending(&app, wid), Some(Trick::Sit));

    // A line completed (so the listener is bound to this session and its
    // next line is truly fresh), Serious Mode toggled on that fresh line,
    // then `sit⏎` once it lifts: the cat sits, and the shell's `command
    // not found` is forgiven.
    let (mut app, wid, _) = pet_app();
    type_word(&mut app, wid, "ls");
    app.input(wid, named(NamedKey::Enter), Source::Human);
    app.set_serious_mode(true);
    app.set_serious_mode(false);
    let t0 = Instant::now();
    type_word(&mut app, wid, "sit");
    app.input(wid, named(NamedKey::Enter), Source::Human);
    assert_eq!(
        pending(&app, wid),
        Some(Trick::Sit),
        "the first line after Serious Mode lifts is heard"
    );
    assert!(
        !grieves_at(&mut app, wid, t0),
        "…and its `command not found` is forgiven"
    );
}

/// The introspection renderer declares the snapshot's row origin. `sit⏎` on the bottom row, at a
/// prompt width that puts the shell's `command not found: sit` in the typed word's columns one
/// row nearer the caret: with `set_scan_base_y` called before the rescan, a headless capture's
/// flash lands on the typed line two rows up, not the error line.
#[test]
fn a_headless_capture_declares_base_y_so_the_scrolled_flash_finds_the_typed_line() {
    let (mut app, wid, session) = pet_app();
    let t0 = Instant::now();
    let rows = usize::from(app.windows[&wid].rows);
    let prompt = format!("{}$ ", "~".repeat(22));
    {
        let term = app.pool.get(session).expect("session").term.clone();
        let mut t = term_lock(&term);
        // Walk the caret to the bottom row, then echo the prompt and the
        // typed word as a shell would.
        for _ in 0..rows.saturating_sub(1) {
            t.process(b"\r\n");
        }
        t.process(format!("{prompt}sit").as_bytes());
        let cursor = t.cursor();
        assert_eq!(
            (usize::from(cursor.row), cursor.col),
            (rows - 1, 27),
            "fixture: typed on the bottom row"
        );
    }
    // The capture that records the caret the flash will anchor on.
    app.capture_words(wid, t0);
    // The word was typed: the listener's line must know it.
    type_word(&mut app, wid, "sit");
    // Enter is the boundary key: noted on the input path first…
    app.input(wid, named(NamedKey::Enter), Source::Human);
    assert_eq!(pending(&app, wid), Some(Trick::Sit));
    assert_eq!(flash(&app, wid, t0), TrickFlashPhase::Pending);
    // …then the shell answers, scrolling the typed line two rows up.
    {
        let term = app.pool.get(session).expect("session").term.clone();
        term_lock(&term).process(format!("\r\nzsh: command not found: sit\r\n{prompt}").as_bytes());
    }
    app.capture_words(wid, t0 + Duration::from_millis(300));
    let ws = &app.windows[&wid];
    assert_eq!(
        ws.fx
            .decos()
            .trick_flash_phase(t0 + Duration::from_millis(300)),
        TrickFlashPhase::Live,
        "the capture's rescan located the word"
    );
    let mut cells: Vec<(u16, u16)> = ws
        .input_scratch
        .ink
        .iter()
        .map(|cell| (cell.row, cell.col))
        .collect();
    cells.sort_unstable();
    let typed_row = u16::try_from(rows - 3).expect("row");
    assert_eq!(
        cells,
        [(typed_row, 24), (typed_row, 25), (typed_row, 26)],
        "the TYPED line two rows up — never the error line one row up"
    );
}
