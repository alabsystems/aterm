// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard, IME, mouse-modifier, and action dispatch methods on `App`.

use crate::keybinding::Action;
use crate::{App, WindowId};
use aterm_window::event::{ElementState, KeyEvent};
use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};

fn shift_l() -> KeyEvent {
    KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::KeyL),
        Key::Character(SmolStr::new("L")),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    )
}

#[test]
fn cmd_shift_l_composes_the_focused_sessions_ledger_command() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(sid));
    app.frontmost_window = Some(wid);
    let stable = app
        .pool
        .get(sid)
        .expect("stub pooled")
        .ctx
        .self_id
        .as_str()
        .to_string();
    assert!(app.last_ledger_plan.is_none());

    let fired = app.on_key_super_shift_chord(
        wid,
        ModifiersState::SUPER | ModifiersState::SHIFT,
        &shift_l(),
    );
    assert!(fired, "⇧⌘L is consumed, never typed into the PTY");
    let plan = app.last_ledger_plan.clone().expect("a plan was composed");
    assert_eq!(plan.sid, stable);
    assert_eq!(plan.argv[..2], ["drive", "ledger"]);
    assert_eq!(plan.argv[2], format!("@{stable}"));
    assert_eq!(plan.argv[3..6], ["--format", "html", "--out"]);
    assert_eq!(plan.argv[6], plan.out.to_string_lossy());

    app.last_ledger_plan = None;
    assert!(!app.on_key_super_shift_chord(wid, ModifiersState::SUPER, &shift_l()));
    assert!(app.last_ledger_plan.is_none());

    app.dispatch_action(wid, Action::OpenLedger);
    assert_eq!(
        app.last_ledger_plan.as_ref().map(|p| p.sid.as_str()),
        Some(stable.as_str())
    );
}
