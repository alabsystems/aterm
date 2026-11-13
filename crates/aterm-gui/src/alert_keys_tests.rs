// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ConfirmKey, FieldEdit, confirm_key, field_edit};

const SHIFT: u64 = 1 << 17;
const CONTROL: u64 = 1 << 18;
const OPTION: u64 = 1 << 19;
const COMMAND: u64 = 1 << 20;
const FUNCTION: u64 = 1 << 23;

const RETURN: (Option<&str>, u16) = (Some("\r"), 0x24);
const KEYPAD_ENTER: (Option<&str>, u16) = (Some("\u{3}"), 0x4c);
const ESCAPE: (Option<&str>, u16) = (Some("\u{1b}"), 0x35);
const LETTER_A: (Option<&str>, u16) = (Some("a"), 0x00);

/// A POSTED KEY THAT COULD REACH A CONFIRMATION IS ASKED ABOUT, WHATEVER WINDOW IT NAMES:
/// while the confirmation is up, a key naming no window (`hwkey … shape=press`, which AppKit
/// hands to the key window, the paste sheet) and any key during an app-modal confirmation
/// count as reaching it, so a posted one is dropped; a key naming another window while
/// a sheet is up passes to it, and nothing counts once the confirmation is gone.
/// NEGATIVE CONTROL: with the rule `attached` alone (as `decide` read it at 3a3de060b1), the
/// window-less key and the app-modal alert's key do not count, and the first assertion fails.
#[test]
fn a_posted_key_that_could_reach_a_confirmation_counts_whatever_window_it_names() {
    use super::may_reach_confirmation as reaches;
    // (attached, outstanding, names_window, app_modal)
    assert!(
        reaches(false, true, false, false),
        "a window-less key, sheet up"
    );
    assert!(
        reaches(false, true, true, true),
        "any key, app-modal alert up"
    );
    assert!(
        reaches(false, true, false, true),
        "a window-less key, app-modal alert up"
    );
    assert!(
        reaches(true, true, true, false),
        "a key naming the sheet's window"
    );
    assert!(
        !reaches(false, true, true, false),
        "a key naming another window, sheet up"
    );
    assert!(
        !reaches(false, false, false, false),
        "a window-less key, nothing up"
    );
    assert!(
        !reaches(false, false, true, true),
        "any key, the alert gone"
    );
}

/// `hwkey` reads whether one of the person's confirmations is on screen before every key: a
/// [`super::ConfirmationUp`] counts one while it lives, nested ones included.
/// NEGATIVE CONTROL: with `hold` counting nothing, `confirmation_up` reads false under a
/// held guard and the first assertion fails.
/// THE PERSON'S EDITING KEYS EDIT A CONFIRMATION'S FIELD: ⌘X, ⌘C, ⌘V and ⌘A alone, in either
/// case, and nothing with another modifier or no Command.
#[test]
fn command_x_c_v_and_a_alone_edit_a_confirmation_field() {
    for (chars, want) in [
        ("x", FieldEdit::Cut),
        ("c", FieldEdit::Copy),
        ("v", FieldEdit::Paste),
        ("a", FieldEdit::SelectAll),
        ("V", FieldEdit::Paste),
    ] {
        assert_eq!(field_edit(COMMAND, Some(chars)), Some(want), "{chars}");
        assert_eq!(
            field_edit(COMMAND | FUNCTION, Some(chars)),
            Some(want),
            "{chars} with the function bit"
        );
    }
    for mods in [0, SHIFT, CONTROL, OPTION, COMMAND | SHIFT, COMMAND | OPTION] {
        assert_eq!(field_edit(mods, Some("v")), None, "{mods:#x}");
    }
    assert_eq!(field_edit(COMMAND | CONTROL, Some("v")), None);
    assert_eq!(field_edit(COMMAND, Some("z")), None);
    assert_eq!(field_edit(COMMAND, None), None);
}

#[test]
fn a_confirmation_on_screen_is_counted_until_it_goes() {
    assert!(!super::confirmation_up(), "none yet");
    let paste = super::ConfirmationUp::hold();
    assert!(super::confirmation_up(), "the paste sheet");
    let quit = super::ConfirmationUp::hold();
    drop(paste);
    assert!(super::confirmation_up(), "the Quit alert still stands");
    drop(quit);
    assert!(!super::confirmation_up(), "both gone");
}

fn action(attached: bool, mods: u64, key: (Option<&str>, u16)) -> ConfirmKey {
    confirm_key(attached, false, mods, key.0, key.1)
}

/// A key a control client posted (`aterm ctl hwkey`) never answers the person's confirmation,
/// a multi-line paste's included: Return, keypad Enter, Escape and any other key are
/// swallowed while one is up, and pass untouched when none is.
/// NEGATIVE CONTROL: without `confirm_key`'s `posted` arm, a posted Return answers Accept
/// and a posted Escape Cancel, and the first assertion fails.
#[test]
fn a_key_a_control_client_posted_never_answers_a_confirmation() {
    for key in [RETURN, KEYPAD_ENTER, ESCAPE, LETTER_A] {
        for mods in [0, COMMAND, SHIFT] {
            assert_eq!(
                confirm_key(true, true, mods, key.0, key.1),
                ConfirmKey::Drop,
                "a posted {key:?} with modifierFlags {mods:#x}"
            );
            assert_eq!(
                confirm_key(false, true, mods, key.0, key.1),
                ConfirmKey::PassThrough,
                "a posted {key:?} with no confirmation up"
            );
        }
    }
    assert_eq!(
        action(true, 0, RETURN),
        ConfirmKey::Accept,
        "CONTROL: the person's Return"
    );
}

#[test]
fn every_modifier_combination_still_accepts_return() {
    for mods in [
        0,
        SHIFT,
        CONTROL,
        OPTION,
        COMMAND,
        FUNCTION,
        COMMAND | SHIFT,
        COMMAND | OPTION,
        COMMAND | CONTROL | OPTION | SHIFT,
    ] {
        assert_eq!(
            action(true, mods, RETURN),
            ConfirmKey::Accept,
            "Return with modifierFlags {mods:#x} must accept",
        );
    }
}

#[test]
fn an_attached_confirmation_maps_enter_escape_and_a_character() {
    for (label, mods, key, expected) in [
        ("keypad Enter", FUNCTION, KEYPAD_ENTER, ConfirmKey::Accept),
        ("Escape", 0, ESCAPE, ConfirmKey::Cancel),
        ("⌘Escape", COMMAND, ESCAPE, ConfirmKey::Cancel),
        ("a", 0, LETTER_A, ConfirmKey::PassThrough),
        ("⌘a", COMMAND, LETTER_A, ConfirmKey::PassThrough),
    ] {
        assert_eq!(action(true, mods, key), expected, "{label}");
    }
}

#[test]
fn nothing_is_intercepted_when_no_confirmation_is_attached() {
    for key in [RETURN, KEYPAD_ENTER, ESCAPE, LETTER_A] {
        for mods in [0, COMMAND, SHIFT, COMMAND | SHIFT] {
            assert_eq!(
                action(false, mods, key),
                ConfirmKey::PassThrough,
                "{key:?} with modifierFlags {mods:#x} must reach the terminal when \
                 no confirmation is up",
            );
        }
    }
}

#[test]
fn missing_characters_fall_back_to_the_physical_key() {
    assert_eq!(action(true, COMMAND, (None, 0x24)), ConfirmKey::Accept);
    assert_eq!(action(true, 0, (None, 0x4c)), ConfirmKey::Accept);
    assert_eq!(action(true, 0, (None, 0x35)), ConfirmKey::Cancel);
    assert_eq!(action(true, 0, (None, 0x00)), ConfirmKey::PassThrough);
}
