// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Protocol-level keyboard encoding types and logic.
//!
//! Use `keyboard` for terminal protocol encoding (CSI u, xterm, legacy).
//! Use [`super::input`] for editor/plugin/application logic.
//! Use the [`TryFrom`] bridge on [`super::input::KeyCode`] when crossing
//! between the two layers.
//!
//! This module provides the shared keyboard encoding contract used by both
//! `aterm-core-ffi` and `aterm-alacritty-bridge`. Key types, modifier flags,
//! terminal mode flags, and encoding functions all live here so neither crate
//! needs to depend on the other for keyboard functionality.

// The DOM (KeyboardEvent.key) → engine key map, the web bindings' sibling of
// the winit map. Pure string matching (no platform deps), so it belongs here.
mod dom_map;
mod encode;
mod key_types;
mod mode;
mod term_mode;
// THE winit→engine key map (K-2) is NOT here: it is `aterm-winit-keymap`, its
// own crate. It sat here behind an optional `winit-keymap` feature meant to keep
// non-GUI consumers winit-free, and workspace feature unification defeated that
// — `aterm-ctl` came out of a plain `cargo build --workspace` linking AppKit.
// Keeping this module platform-free is the point; a new platform map goes in a
// crate of its own, never behind a feature of aterm-types.

pub use dom_map::{encode_dom_key, map_dom_key};
pub use encode::{
    encode_key, encode_key_with_event, encode_key_with_layout, is_modifier_or_lock_key,
    shifted_character,
};
pub use key_types::{Key, KeyEventType, Modifiers, NamedKey};
pub use mode::KeyboardMode;
pub use term_mode::TermMode;

#[cfg(test)]
mod tests;
