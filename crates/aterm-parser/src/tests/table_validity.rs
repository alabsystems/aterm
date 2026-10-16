// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! State naming: `State::name` agrees with `Debug` for every state.

use super::super::*;

/// `State::name` is the state's `Debug` spelling for every state, so a log
/// line naming an abandoned partial sequence reads exactly like a `{:?}` dump.
/// The list is checked against `State::COUNT` so a new state cannot slip past.
#[test]
fn state_name_matches_debug_for_every_state() {
    let all = [
        State::Ground,
        State::Escape,
        State::EscapeIntermediate,
        State::CsiEntry,
        State::CsiParam,
        State::CsiIntermediate,
        State::CsiIgnore,
        State::DcsEntry,
        State::DcsParam,
        State::DcsIntermediate,
        State::DcsPassthrough,
        State::DcsIgnore,
        State::OscString,
        State::SosPmApcString,
    ];
    assert_eq!(all.len(), State::COUNT, "every state is listed");
    for state in all {
        assert_eq!(state.name(), format!("{state:?}"));
    }
}
