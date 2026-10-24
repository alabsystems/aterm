// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Protocol-level keyboard encoding: key types, modifier and mode flags, and the CSI u, xterm
//! and legacy encoders, with no platform dependency. [`super::input`] is the editor/plugin
//! layer; the [`TryFrom`] bridge on [`super::input::KeyCode`] crosses between the two.
//! Platform keymaps (such as `aterm-winit-keymap`) live in separate crates so Cargo feature
//! unification never links AppKit into non-GUI crates.

mod dom_map;
mod encode;
mod key_types;
mod mode;
mod term_mode;

pub use dom_map::{encode_dom_key, map_dom_key};
pub use encode::{
    encode_key, encode_key_with_event, encode_key_with_layout, encode_key_with_text,
    is_modifier_or_lock_key, layout_letter_twin, shifted_character, typed_character,
};
pub use key_types::{Key, KeyEventType, Modifiers, NamedKey};
pub use mode::KeyboardMode;
pub use term_mode::TermMode;

#[cfg(test)]
mod tests;
