// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DCS, OSC, and APC/SOS/PM state transitions.

use crate::state::State;

use super::set_range;
use super::types::{ActionType, Transition};

pub(super) const fn apply_dcs_entry_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::DcsEntry,
        0x00,
        0x17,
        Transition::new(State::DcsEntry, ActionType::Ignore),
    );
    table[State::DcsEntry as usize][0x19] = Transition::new(State::DcsEntry, ActionType::Ignore);
    set_range(
        table,
        State::DcsEntry,
        0x1C,
        0x1F,
        Transition::new(State::DcsEntry, ActionType::Ignore),
    );
    set_range(
        table,
        State::DcsEntry,
        0x20,
        0x2F,
        Transition::new(State::DcsIntermediate, ActionType::Collect),
    );
    set_range(
        table,
        State::DcsEntry,
        0x30,
        0x39,
        Transition::new(State::DcsParam, ActionType::Param),
    );
    table[State::DcsEntry as usize][0x3A] = Transition::new(State::DcsIgnore, ActionType::None);
    table[State::DcsEntry as usize][0x3B] = Transition::new(State::DcsParam, ActionType::Param);
    set_range(
        table,
        State::DcsEntry,
        0x3C,
        0x3F,
        Transition::new(State::DcsParam, ActionType::Collect),
    );
    set_range(
        table,
        State::DcsEntry,
        0x40,
        0x7E,
        Transition::new(State::DcsPassthrough, ActionType::DcsHook),
    );
    table[State::DcsEntry as usize][0x7F] = Transition::new(State::DcsEntry, ActionType::Ignore);
}

pub(super) const fn apply_dcs_param_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::DcsParam,
        0x00,
        0x17,
        Transition::new(State::DcsParam, ActionType::Ignore),
    );
    table[State::DcsParam as usize][0x19] = Transition::new(State::DcsParam, ActionType::Ignore);
    set_range(
        table,
        State::DcsParam,
        0x1C,
        0x1F,
        Transition::new(State::DcsParam, ActionType::Ignore),
    );
    set_range(
        table,
        State::DcsParam,
        0x20,
        0x2F,
        Transition::new(State::DcsIntermediate, ActionType::Collect),
    );
    set_range(
        table,
        State::DcsParam,
        0x30,
        0x39,
        Transition::new(State::DcsParam, ActionType::Param),
    );
    table[State::DcsParam as usize][0x3A] = Transition::new(State::DcsIgnore, ActionType::None);
    table[State::DcsParam as usize][0x3B] = Transition::new(State::DcsParam, ActionType::Param);
    set_range(
        table,
        State::DcsParam,
        0x3C,
        0x3F,
        Transition::new(State::DcsIgnore, ActionType::None),
    );
    set_range(
        table,
        State::DcsParam,
        0x40,
        0x7E,
        Transition::new(State::DcsPassthrough, ActionType::DcsHook),
    );
    table[State::DcsParam as usize][0x7F] = Transition::new(State::DcsParam, ActionType::Ignore);
}

pub(super) const fn apply_dcs_intermediate_transitions(
    table: &mut [[Transition; 256]; State::COUNT],
) {
    set_range(
        table,
        State::DcsIntermediate,
        0x00,
        0x17,
        Transition::new(State::DcsIntermediate, ActionType::Ignore),
    );
    table[State::DcsIntermediate as usize][0x19] =
        Transition::new(State::DcsIntermediate, ActionType::Ignore);
    set_range(
        table,
        State::DcsIntermediate,
        0x1C,
        0x1F,
        Transition::new(State::DcsIntermediate, ActionType::Ignore),
    );
    set_range(
        table,
        State::DcsIntermediate,
        0x20,
        0x2F,
        Transition::new(State::DcsIntermediate, ActionType::Collect),
    );
    set_range(
        table,
        State::DcsIntermediate,
        0x30,
        0x3F,
        Transition::new(State::DcsIgnore, ActionType::None),
    );
    set_range(
        table,
        State::DcsIntermediate,
        0x40,
        0x7E,
        Transition::new(State::DcsPassthrough, ActionType::DcsHook),
    );
    table[State::DcsIntermediate as usize][0x7F] =
        Transition::new(State::DcsIntermediate, ActionType::Ignore);
}

pub(super) const fn apply_dcs_passthrough_transitions(
    table: &mut [[Transition; 256]; State::COUNT],
) {
    set_range(
        table,
        State::DcsPassthrough,
        0x00,
        0x17,
        Transition::new(State::DcsPassthrough, ActionType::DcsPut),
    );
    table[State::DcsPassthrough as usize][0x19] =
        Transition::new(State::DcsPassthrough, ActionType::DcsPut);
    set_range(
        table,
        State::DcsPassthrough,
        0x1C,
        0x1F,
        Transition::new(State::DcsPassthrough, ActionType::DcsPut),
    );
    set_range(
        table,
        State::DcsPassthrough,
        0x20,
        0x7E,
        Transition::new(State::DcsPassthrough, ActionType::DcsPut),
    );
    table[State::DcsPassthrough as usize][0x7F] =
        Transition::new(State::DcsPassthrough, ActionType::Ignore);
    // Override the anywhere C1 transitions so DCS payloads can contain UTF-8
    // continuation bytes and binary data (Sixel, tmux passthrough).
    // Leave 0x9C as ST terminator for DCS. Unlike OscString (which treats 0x9C as data for UTF-8 correctness), DCS payloads use 0x9C as the standard ST.
    set_range(
        table,
        State::DcsPassthrough,
        0x80,
        0x9B,
        Transition::new(State::DcsPassthrough, ActionType::DcsPut),
    );
    // 0x9C remains as ST (string terminator) — do not override
    set_range(
        table,
        State::DcsPassthrough,
        0x9D,
        0xFF,
        Transition::new(State::DcsPassthrough, ActionType::DcsPut),
    );
}

pub(super) const fn apply_dcs_ignore_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::DcsIgnore,
        0x00,
        0x17,
        Transition::new(State::DcsIgnore, ActionType::Ignore),
    );
    table[State::DcsIgnore as usize][0x19] = Transition::new(State::DcsIgnore, ActionType::Ignore);
    set_range(
        table,
        State::DcsIgnore,
        0x1C,
        0x1F,
        Transition::new(State::DcsIgnore, ActionType::Ignore),
    );
    set_range(
        table,
        State::DcsIgnore,
        0x20,
        0x7F,
        Transition::new(State::DcsIgnore, ActionType::Ignore),
    );
}

pub(super) const fn apply_osc_string_transitions(table: &mut [[Transition; 256]; State::COUNT]) {
    set_range(
        table,
        State::OscString,
        0x00,
        0x06,
        Transition::new(State::OscString, ActionType::Ignore),
    );
    table[State::OscString as usize][0x07] = Transition::new(State::Ground, ActionType::OscEnd);
    set_range(
        table,
        State::OscString,
        0x08,
        0x17,
        Transition::new(State::OscString, ActionType::Ignore),
    );
    table[State::OscString as usize][0x19] = Transition::new(State::OscString, ActionType::Ignore);
    set_range(
        table,
        State::OscString,
        0x1C,
        0x1F,
        Transition::new(State::OscString, ActionType::Ignore),
    );
    set_range(
        table,
        State::OscString,
        0x20,
        0x7F,
        Transition::new(State::OscString, ActionType::OscPut),
    );
    // Override ALL C1 transitions (0x80-0x9F) so OSC payloads can contain
    // UTF-8 continuation bytes. Notably, 0x9C is a valid UTF-8 continuation
    // byte (appears in CJK like 本=E6 9C AC) — treating it as C1 ST would
    // prematurely terminate titles containing those characters (#3745).
    // OSC strings are terminated by BEL (0x07) or ESC \ only.
    set_range(
        table,
        State::OscString,
        0x80,
        0x9F,
        Transition::new(State::OscString, ActionType::OscPut),
    );
    set_range(
        table,
        State::OscString,
        0xA0,
        0xFF,
        Transition::new(State::OscString, ActionType::OscPut),
    );
}

pub(super) const fn apply_sos_pm_apc_string_transitions(
    table: &mut [[Transition; 256]; State::COUNT],
) {
    set_range(
        table,
        State::SosPmApcString,
        0x00,
        0x17,
        Transition::new(State::SosPmApcString, ActionType::ApcPut),
    );
    table[State::SosPmApcString as usize][0x19] =
        Transition::new(State::SosPmApcString, ActionType::ApcPut);
    set_range(
        table,
        State::SosPmApcString,
        0x1C,
        0x1F,
        Transition::new(State::SosPmApcString, ActionType::ApcPut),
    );
    set_range(
        table,
        State::SosPmApcString,
        0x20,
        0x7F,
        Transition::new(State::SosPmApcString, ActionType::ApcPut),
    );
    // Override the anywhere C1 transitions so APC/SOS/PM payloads can contain
    // UTF-8 continuation bytes. Leave 0x9C as ST terminator. Same pattern as
    // OscString (#3761).
    set_range(
        table,
        State::SosPmApcString,
        0x80,
        0x9B,
        Transition::new(State::SosPmApcString, ActionType::ApcPut),
    );
    // 0x9C remains as ST (string terminator) — do not override
    set_range(
        table,
        State::SosPmApcString,
        0x9D,
        0xFF,
        Transition::new(State::SosPmApcString, ActionType::ApcPut),
    );
}
