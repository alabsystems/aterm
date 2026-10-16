// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Protocol-level keyboard encoding: key types, modifier and mode flags, and the CSI u, xterm
//! and legacy encoders, with no platform dependency. [`super::input`] is the editor/plugin
//! layer; the [`TryFrom`] bridge on [`super::input::KeyCode`] crosses between the two.

// The DOM (KeyboardEvent.key) → engine key map: pure string matching, no platform deps.
mod dom_map;
mod encode;
mod key_types;
mod mode;
mod term_mode;
// The winit → engine key map is its own crate, `aterm-winit-keymap`: a feature here would be
// unified on across the workspace and link AppKit into non-GUI crates. A platform map goes in
// a crate of its own, never behind a feature of aterm-types.

pub use dom_map::{encode_dom_key, map_dom_key};
pub use encode::{
    encode_key, encode_key_with_event, encode_key_with_layout, encode_key_with_text,
    is_modifier_or_lock_key, shifted_character, typed_character,
};
pub use key_types::{Key, KeyEventType, Modifiers, NamedKey};
pub use mode::KeyboardMode;
pub use term_mode::TermMode;

#[cfg(test)]
mod tests;
