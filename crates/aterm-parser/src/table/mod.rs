// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-time generated transition table.
//!
//! Based on the vt100.net DEC ANSI parser state machine.
//! Reference: <https://vt100.net/emu/dec_ansi_parser>

mod dcs_osc;
mod types;

pub use types::{ActionType, Transition};

use crate::state::State;

/// Helper to set transitions for a range of bytes in a state.
const fn set_range(
    table: &mut [[Transition; 256]; State::COUNT],
    state: State,
    start: u8,
    end: u8,
    transition: Transition,
) {
    let mut byte = start;
    while byte <= end {
        table[state as usize][byte as usize] = transition;
        if byte == 255 {
            break;
        }
        byte += 1;
    }
}

const fn apply_anywhere_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    let mut state_idx = 0;
    while state_idx < State::COUNT {
        table[state_idx][0x18] = Transition::new(State::Ground, ActionType::Execute);
        table[state_idx][0x1A] = Transition::new(State::Ground, ActionType::Execute);
        table[state_idx][0x1B] = Transition::new(State::Escape, ActionType::Clear);

        let mut c1 = 0x80u8;
        while c1 <= 0x8F {
            table[state_idx][c1 as usize] = Transition::new(State::Ground, ActionType::Execute);
            c1 += 1;
        }
        table[state_idx][0x90] = Transition::new(State::DcsEntry, ActionType::Clear);
        c1 = 0x91;
        while c1 <= 0x97 {
            table[state_idx][c1 as usize] = Transition::new(State::Ground, ActionType::Execute);
            c1 += 1;
        }
        table[state_idx][0x98] = Transition::new(State::SosPmApcString, ActionType::None);
        table[state_idx][0x99] = Transition::new(State::Ground, ActionType::Execute);
        table[state_idx][0x9A] = Transition::new(State::Ground, ActionType::Execute);
        table[state_idx][0x9B] = Transition::new(State::CsiEntry, ActionType::Clear);
        table[state_idx][0x9C] = Transition::new(State::Ground, ActionType::None);
        table[state_idx][0x9D] = Transition::new(State::OscString, ActionType::OscStart);
        table[state_idx][0x9E] = Transition::new(State::SosPmApcString, ActionType::None);
        table[state_idx][0x9F] = Transition::new(State::SosPmApcString, ActionType::ApcStart);

        state_idx += 1;
    }
}

const fn apply_ground_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::Ground,
        0x00,
        0x17,
        Transition::new(State::Ground, ActionType::Execute),
    );
    table[State::Ground as usize][0x19] = Transition::new(State::Ground, ActionType::Execute);
    set_range(
        table,
        State::Ground,
        0x1C,
        0x1F,
        Transition::new(State::Ground, ActionType::Execute),
    );
    set_range(
        table,
        State::Ground,
        0x20,
        0x7E,
        Transition::new(State::Ground, ActionType::Print),
    );
    // DEL (0x7F) is ignored per VT100 spec, consistent with all other states.
    table[State::Ground as usize][0x7F] = Transition::new(State::Ground, ActionType::Ignore);
}

const fn apply_escape_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::Escape,
        0x00,
        0x17,
        Transition::new(State::Escape, ActionType::Execute),
    );
    table[State::Escape as usize][0x19] = Transition::new(State::Escape, ActionType::Execute);
    set_range(
        table,
        State::Escape,
        0x1C,
        0x1F,
        Transition::new(State::Escape, ActionType::Execute),
    );
    set_range(
        table,
        State::Escape,
        0x20,
        0x2F,
        Transition::new(State::EscapeIntermediate, ActionType::Collect),
    );
    set_range(
        table,
        State::Escape,
        0x30,
        0x4F,
        Transition::new(State::Ground, ActionType::EscDispatch),
    );
    table[State::Escape as usize][0x50] = Transition::new(State::DcsEntry, ActionType::Clear);
    set_range(
        table,
        State::Escape,
        0x51,
        0x57,
        Transition::new(State::Ground, ActionType::EscDispatch),
    );
    table[State::Escape as usize][0x58] = Transition::new(State::SosPmApcString, ActionType::None);
    table[State::Escape as usize][0x59] = Transition::new(State::Ground, ActionType::EscDispatch);
    table[State::Escape as usize][0x5A] = Transition::new(State::Ground, ActionType::EscDispatch);
    table[State::Escape as usize][0x5B] = Transition::new(State::CsiEntry, ActionType::Clear);
    table[State::Escape as usize][0x5C] = Transition::new(State::Ground, ActionType::EscDispatch);
    table[State::Escape as usize][0x5D] = Transition::new(State::OscString, ActionType::OscStart);
    table[State::Escape as usize][0x5E] = Transition::new(State::SosPmApcString, ActionType::None);
    table[State::Escape as usize][0x5F] =
        Transition::new(State::SosPmApcString, ActionType::ApcStart);
    set_range(
        table,
        State::Escape,
        0x60,
        0x7E,
        Transition::new(State::Ground, ActionType::EscDispatch),
    );
    table[State::Escape as usize][0x7F] = Transition::new(State::Escape, ActionType::Ignore);
}

const fn apply_escape_intermediate_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::EscapeIntermediate,
        0x00,
        0x17,
        Transition::new(State::EscapeIntermediate, ActionType::Execute),
    );
    table[State::EscapeIntermediate as usize][0x19] =
        Transition::new(State::EscapeIntermediate, ActionType::Execute);
    set_range(
        table,
        State::EscapeIntermediate,
        0x1C,
        0x1F,
        Transition::new(State::EscapeIntermediate, ActionType::Execute),
    );
    set_range(
        table,
        State::EscapeIntermediate,
        0x20,
        0x2F,
        Transition::new(State::EscapeIntermediate, ActionType::Collect),
    );
    set_range(
        table,
        State::EscapeIntermediate,
        0x30,
        0x7E,
        Transition::new(State::Ground, ActionType::EscDispatch),
    );
    table[State::EscapeIntermediate as usize][0x7F] =
        Transition::new(State::EscapeIntermediate, ActionType::Ignore);
}

const fn apply_csi_entry_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::CsiEntry,
        0x00,
        0x17,
        Transition::new(State::CsiEntry, ActionType::Execute),
    );
    table[State::CsiEntry as usize][0x19] = Transition::new(State::CsiEntry, ActionType::Execute);
    set_range(
        table,
        State::CsiEntry,
        0x1C,
        0x1F,
        Transition::new(State::CsiEntry, ActionType::Execute),
    );
    set_range(
        table,
        State::CsiEntry,
        0x20,
        0x2F,
        Transition::new(State::CsiIntermediate, ActionType::Collect),
    );
    set_range(
        table,
        State::CsiEntry,
        0x30,
        0x39,
        Transition::new(State::CsiParam, ActionType::Param),
    );
    table[State::CsiEntry as usize][0x3A] = Transition::new(State::CsiParam, ActionType::Param);
    table[State::CsiEntry as usize][0x3B] = Transition::new(State::CsiParam, ActionType::Param);
    set_range(
        table,
        State::CsiEntry,
        0x3C,
        0x3F,
        Transition::new(State::CsiParam, ActionType::Collect),
    );
    set_range(
        table,
        State::CsiEntry,
        0x40,
        0x7E,
        Transition::new(State::Ground, ActionType::CsiDispatch),
    );
    table[State::CsiEntry as usize][0x7F] = Transition::new(State::CsiEntry, ActionType::Ignore);
}

const fn apply_csi_param_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::CsiParam,
        0x00,
        0x17,
        Transition::new(State::CsiParam, ActionType::Execute),
    );
    table[State::CsiParam as usize][0x19] = Transition::new(State::CsiParam, ActionType::Execute);
    set_range(
        table,
        State::CsiParam,
        0x1C,
        0x1F,
        Transition::new(State::CsiParam, ActionType::Execute),
    );
    set_range(
        table,
        State::CsiParam,
        0x20,
        0x2F,
        Transition::new(State::CsiIntermediate, ActionType::Collect),
    );
    set_range(
        table,
        State::CsiParam,
        0x30,
        0x39,
        Transition::new(State::CsiParam, ActionType::Param),
    );
    table[State::CsiParam as usize][0x3A] = Transition::new(State::CsiParam, ActionType::Param);
    table[State::CsiParam as usize][0x3B] = Transition::new(State::CsiParam, ActionType::Param);
    set_range(
        table,
        State::CsiParam,
        0x3C,
        0x3F,
        Transition::new(State::CsiIgnore, ActionType::None),
    );
    set_range(
        table,
        State::CsiParam,
        0x40,
        0x7E,
        Transition::new(State::Ground, ActionType::CsiDispatch),
    );
    table[State::CsiParam as usize][0x7F] = Transition::new(State::CsiParam, ActionType::Ignore);
}

const fn apply_csi_intermediate_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::CsiIntermediate,
        0x00,
        0x17,
        Transition::new(State::CsiIntermediate, ActionType::Execute),
    );
    table[State::CsiIntermediate as usize][0x19] =
        Transition::new(State::CsiIntermediate, ActionType::Execute);
    set_range(
        table,
        State::CsiIntermediate,
        0x1C,
        0x1F,
        Transition::new(State::CsiIntermediate, ActionType::Execute),
    );
    set_range(
        table,
        State::CsiIntermediate,
        0x20,
        0x2F,
        Transition::new(State::CsiIntermediate, ActionType::Collect),
    );
    set_range(
        table,
        State::CsiIntermediate,
        0x30,
        0x3F,
        Transition::new(State::CsiIgnore, ActionType::None),
    );
    set_range(
        table,
        State::CsiIntermediate,
        0x40,
        0x7E,
        Transition::new(State::Ground, ActionType::CsiDispatch),
    );
    table[State::CsiIntermediate as usize][0x7F] =
        Transition::new(State::CsiIntermediate, ActionType::Ignore);
}

const fn apply_csi_ignore_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::CsiIgnore,
        0x00,
        0x17,
        Transition::new(State::CsiIgnore, ActionType::Execute),
    );
    table[State::CsiIgnore as usize][0x19] = Transition::new(State::CsiIgnore, ActionType::Execute);
    set_range(
        table,
        State::CsiIgnore,
        0x1C,
        0x1F,
        Transition::new(State::CsiIgnore, ActionType::Execute),
    );
    set_range(
        table,
        State::CsiIgnore,
        0x20,
        0x3F,
        Transition::new(State::CsiIgnore, ActionType::Ignore),
    );
    set_range(
        table,
        State::CsiIgnore,
        0x40,
        0x7E,
        Transition::new(State::Ground, ActionType::None),
    );
    table[State::CsiIgnore as usize][0x7F] = Transition::new(State::CsiIgnore, ActionType::Ignore);
}

/// Generate the transition table at compile time.
///
/// This creates a 256 x 14 table (~7 KB) that maps
/// (current_state, input_byte) -> (next_state, action).
///
/// Based on the vt100.net DEC ANSI parser state machine.
pub(crate) const fn generate_table() -> [[Transition; 256]; State::COUNT] {
    let mut table = [[Transition::new(State::Ground, ActionType::None); 256]; State::COUNT];

    apply_anywhere_transitions(&mut table);
    apply_ground_transitions(&mut table);
    apply_escape_transitions(&mut table);
    apply_escape_intermediate_transitions(&mut table);
    apply_csi_entry_transitions(&mut table);
    apply_csi_param_transitions(&mut table);
    apply_csi_intermediate_transitions(&mut table);
    apply_csi_ignore_transitions(&mut table);
    dcs_osc::apply_dcs_entry_transitions(&mut table);
    dcs_osc::apply_dcs_param_transitions(&mut table);
    dcs_osc::apply_dcs_intermediate_transitions(&mut table);
    dcs_osc::apply_dcs_passthrough_transitions(&mut table);
    dcs_osc::apply_dcs_ignore_transitions(&mut table);
    dcs_osc::apply_osc_string_transitions(&mut table);
    dcs_osc::apply_sos_pm_apc_string_transitions(&mut table);

    table
}

/// The compile-time generated transition table.
pub static TRANSITIONS: [[Transition; 256]; State::COUNT] = generate_table();
