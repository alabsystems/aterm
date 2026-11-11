// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// NEGATIVE CONTROL: `Default::default()`, `new()`, `reset()`, and `#[serde(default)]`
/// all produce `(Some(0), 0)` (`\x1b[>4;0m`), while `disable_modify_other_keys()` and an
/// explicit `"modify_other_keys":null` payload remain distinct at `(None, 0)` (`\x1b[>4m`).
#[test]
fn test_default_matches_new_and_reset() {
    let default = XtermKeyboardState::default();
    let new = XtermKeyboardState::new();
    let mut reset = XtermKeyboardState::new();
    reset.disable_modify_other_keys();
    assert_eq!(reset.modify_other_keys(), None);
    assert_eq!(reset.query_modify_other_keys_response(), "\x1b[>4m");

    reset.reset();
    assert_eq!(default, new);
    assert_eq!(default, reset);
    assert_eq!(default.modify_other_keys(), Some(0));
    assert_eq!(default.format_other_keys(), 0);
    assert_eq!(default.query_modify_other_keys_response(), "\x1b[>4;0m");
    assert_eq!(new.query_modify_other_keys_response(), "\x1b[>4;0m");

    #[cfg(feature = "serde")]
    {
        #[derive(serde::Deserialize)]
        struct Holder {
            #[serde(default)]
            xterm_keyboard: XtermKeyboardState,
        }
        let omitted: Holder = aterm_json::from_str("{}").expect("deserializes omitted field");
        assert_eq!(omitted.xterm_keyboard, XtermKeyboardState::new());
    }
}

#[test]
fn test_set_modify_other_keys() {
    let mut state = XtermKeyboardState::new();

    state.set_modify_other_keys(1);
    assert_eq!(state.modify_other_keys(), Some(1));

    state.set_modify_other_keys(2);
    assert_eq!(state.modify_other_keys(), Some(2));

    state.set_modify_other_keys(10);
    assert_eq!(state.modify_other_keys(), Some(2));

    state.set_modify_other_keys(0);
    assert_eq!(state.modify_other_keys(), Some(0));
}

#[test]
fn test_reset_modify_other_keys() {
    let mut state = XtermKeyboardState::new();
    state.set_modify_other_keys(2);

    state.reset_modify_other_keys();
    assert_eq!(state.modify_other_keys(), Some(0));
}

#[test]
fn test_disable_modify_other_keys() {
    let mut state = XtermKeyboardState::new();
    state.set_modify_other_keys(2);

    state.disable_modify_other_keys();
    assert_eq!(state.modify_other_keys(), None);
}

#[test]
fn test_format_other_keys() {
    let mut state = XtermKeyboardState::new();

    state.set_format_other_keys(1);
    assert_eq!(state.format_other_keys(), 1);

    state.set_format_other_keys(5);
    assert_eq!(state.format_other_keys(), 1);

    state.reset_format_other_keys();
    assert_eq!(state.format_other_keys(), 0);
}

#[test]
fn test_query_responses() {
    let mut state = XtermKeyboardState::new();

    assert_eq!(state.query_modify_other_keys_response(), "\x1b[>4;0m");
    assert_eq!(state.query_format_other_keys_response(), "\x1b[>4;0f");

    state.set_modify_other_keys(2);
    state.set_format_other_keys(1);
    assert_eq!(state.query_modify_other_keys_response(), "\x1b[>4;2m");
    assert_eq!(state.query_format_other_keys_response(), "\x1b[>4;1f");
}

#[test]
fn test_reset_all() {
    let mut state = XtermKeyboardState::new();
    state.set_modify_other_keys(2);
    state.set_format_other_keys(1);

    state.reset();
    assert_eq!(state.modify_other_keys(), Some(0));
    assert_eq!(state.format_other_keys(), 0);
}

#[cfg(feature = "serde")]
#[test]
fn test_serde_deserialize_clamps_invariants() {
    let clamped: XtermKeyboardState =
        aterm_json::from_str(r#"{"modify_other_keys":99,"format_other_keys":7}"#)
            .expect("deserializes");
    assert_eq!(clamped.modify_other_keys(), Some(2));
    assert_eq!(clamped.format_other_keys(), 1);

    let disabled: XtermKeyboardState =
        aterm_json::from_str(r#"{"modify_other_keys":null,"format_other_keys":0}"#)
            .expect("deserializes");
    assert_eq!(disabled.modify_other_keys(), None);
    assert_eq!(disabled.format_other_keys(), 0);
}
