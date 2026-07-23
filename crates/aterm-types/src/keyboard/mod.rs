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
// winit_map. Pure string matching (no platform deps), so no feature gate.
mod dom_map;
mod encode;
mod key_types;
mod mode;
mod term_mode;
// K-2: the winit→engine key map (the GUI's platform keyboard, and the future
// native shell's, mapped into the engine's bridge-agnostic `Key`). Behind the
// `winit-keymap` feature so non-GUI consumers never link winit.
#[cfg(feature = "winit-keymap")]
mod winit_map;

pub use dom_map::{encode_dom_key, map_dom_key};
pub use encode::{encode_key, encode_key_with_event, encode_key_with_layout, shifted_character};
pub use key_types::{Key, KeyEventType, Modifiers, NamedKey};
pub use mode::KeyboardMode;
pub use term_mode::TermMode;
#[cfg(feature = "winit-keymap")]
pub use winit_map::{base_layout_key_for, map_logical_key, map_named_key, map_physical_numpad};

#[cfg(test)]
mod tests;
