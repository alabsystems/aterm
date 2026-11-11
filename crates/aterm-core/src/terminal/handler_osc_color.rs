// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC color query, set, reset, and stack handlers.

use super::handler::TerminalHandler;
use aterm_types::{ColorPalette, Rgb};

/// Per-sequence response cap for OSC 4 / OSC 21 palette queries when the policy sets none.
pub(super) const LEGACY_PALETTE_PER_SEQUENCE_MAX: usize = 16;

/// The three dynamic colors addressable via OSC 10/11/12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DynamicColorSlot {
    Foreground,
    Background,
    Cursor,
}

impl DynamicColorSlot {
    /// Parse from OSC 10/11/12 offset index (0=fg, 1=bg, 2=cursor).
    #[inline]
    pub(super) const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Foreground),
            1 => Some(Self::Background),
            2 => Some(Self::Cursor),
            _ => None,
        }
    }

    /// Parse from a case-insensitive color name (OSC 21 / kitty protocol).
    #[inline]
    pub(super) const fn from_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("foreground") {
            return Some(Self::Foreground);
        }
        if name.eq_ignore_ascii_case("background") {
            return Some(Self::Background);
        }
        if name.eq_ignore_ascii_case("cursor") {
            return Some(Self::Cursor);
        }
        None
    }

    /// The [`ColorTarget`](super::ColorTarget) a change of this slot notes.
    #[inline]
    pub(super) const fn color_target(self) -> super::ColorTarget {
        match self {
            Self::Foreground => super::ColorTarget::Foreground,
            Self::Background => super::ColorTarget::Background,
            Self::Cursor => super::ColorTarget::Cursor,
        }
    }
}

/// An OSC 21 query's value: a concrete color, a known field with no value, or an unknown field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Osc21QueryValue {
    Color(Rgb),
    Undefined,
    Unknown,
}

const MAX_OSC21_ECHO_KEY_BYTES: usize = 64;

/// Whether an unrecognised OSC 21 key may be echoed in a `key=?` reply.
#[inline]
fn osc_21_key_is_echoable(key: &str) -> bool {
    key.len() <= MAX_OSC21_ECHO_KEY_BYTES
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[inline]
fn parse_palette_index(key: &str) -> Option<u8> {
    key.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| key.parse::<u8>().ok())
        .flatten()
}

impl TerminalHandler<'_> {
    /// The per-sequence OSC palette-query response cap ([`LEGACY_PALETTE_PER_SEQUENCE_MAX`]).
    #[inline]
    pub(super) fn palette_per_sequence_cap(&self) -> usize {
        if let Some(cfg) = self.policy.rate_limit_config("palette")
            && cfg.per_sequence_max > 0
        {
            return cfg.per_sequence_max as usize;
        }
        LEGACY_PALETTE_PER_SEQUENCE_MAX
    }

    /// Debit the policy engine's `"palette"` bucket by one response pair at the batch's clock.
    #[inline]
    fn palette_rate_limit_consume_one(&mut self) -> bool {
        let clock = super::response_rate_limiter::BatchClock(self.transient.process_now);
        self.policy
            .rate_limit_try_consume("palette", 1, &clock)
            .unwrap_or(true)
    }

    /// Handle OSC 4 (`OSC 4 ; index ; spec ST`, pairs repeating).
    pub(super) fn handle_osc_4(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.len() < 3 {
            return; // Need at least: 4, index, spec
        }

        let per_seq_cap = self.palette_per_sequence_cap();
        let mut i = 1;
        let mut responses_emitted: usize = 0;
        while i + 1 < params.len() {
            let Ok(index_str) = std::str::from_utf8(params[i]) else {
                i += 2;
                continue;
            };

            let Some(index) = parse_palette_index(index_str) else {
                i += 2;
                continue;
            };

            let Ok(spec) = std::str::from_utf8(params[i + 1]) else {
                i += 2;
                continue;
            };

            if spec == "?" {
                if responses_emitted >= per_seq_cap {
                    i += 2;
                    continue;
                }
                if !self.palette_rate_limit_consume_one() {
                    i += 2;
                    continue;
                }
                let color = self.color.palette().get(index);
                let st = if self.transient.last_osc_bel_terminated {
                    "\x07"
                } else {
                    "\x1b\\"
                };
                let response = format!(
                    "\x1b]4;{};{}{}",
                    index,
                    ColorPalette::format_color_spec(color),
                    st,
                );
                self.send_response(cap, response.as_bytes());
                responses_emitted += 1;
            } else {
                // Set palette entry.
                if let Some(color) = ColorPalette::parse_color_spec(spec) {
                    if self.modes.allow_palette_reconfigure {
                        self.color.palette_mut().set(index, color);
                        self.note_color_change(super::ColorTarget::Palette);
                    } else {
                        self.note_palette_set_refused();
                    }
                }
            }

            i += 2;
        }
    }

    /// Handle OSC 10/11/12 (foreground, background, cursor).
    pub(super) fn handle_osc_10_11_12(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
        start_at: usize,
    ) {
        if params.len() < 2 {
            return;
        }

        let mut color_index = start_at;
        for spec_bytes in &params[1..] {
            let Some(slot) = DynamicColorSlot::from_index(color_index) else {
                break;
            };

            let Ok(spec) = std::str::from_utf8(spec_bytes) else {
                color_index += 1;
                continue;
            };

            if spec == "?" {
                let color = self.get_dynamic_color(slot);
                let osc_code = 10 + color_index;
                let st = if self.transient.last_osc_bel_terminated {
                    "\x07"
                } else {
                    "\x1b\\"
                };
                let response = format!(
                    "\x1b]{};{}{}",
                    osc_code,
                    ColorPalette::format_color_spec(color),
                    st,
                );
                self.send_response(cap, response.as_bytes());
            } else if let Some(color) = ColorPalette::parse_color_spec(spec) {
                self.set_dynamic_color(slot, color);
            }

            color_index += 1;
        }
    }

    /// Handle OSC 19, the selection foreground.
    pub(super) fn handle_osc_19(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        let Some(spec_bytes) = params.get(1) else {
            return;
        };
        let Ok(spec) = std::str::from_utf8(spec_bytes) else {
            return;
        };
        if spec == "?" {
            let color = self
                .color
                .selection_foreground
                .unwrap_or(self.color.default_foreground);
            let st = if self.transient.last_osc_bel_terminated {
                "\x07"
            } else {
                "\x1b\\"
            };
            let response = format!("\x1b]19;{}{}", ColorPalette::format_color_spec(color), st);
            self.send_response(cap, response.as_bytes());
            return;
        }
        if let Some(color) = ColorPalette::parse_color_spec(spec) {
            self.color.selection_foreground = Some(color);
            self.note_color_change(super::ColorTarget::SelectionForeground);
        }
    }

    /// Handle OSC 21, kitty's extended colors (`OSC 21 ; key=value ; ... ST`).
    pub(super) fn handle_osc_21(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.len() < 2 {
            return;
        }

        let per_seq_cap = self.palette_per_sequence_cap();
        let mut query_pairs: Vec<String> = Vec::new();

        for param in &params[1..] {
            let Ok(pair) = std::str::from_utf8(param) else {
                continue;
            };
            let Some((raw_key, raw_value)) = pair.split_once('=') else {
                let key = pair.trim();
                if !key.is_empty() {
                    self.osc_21_reset_color(key);
                }
                continue;
            };

            let key = raw_key.trim();
            if key.is_empty() {
                continue;
            }

            let value = raw_value.trim();
            if value == "?" {
                if query_pairs.len() >= per_seq_cap {
                    continue;
                }
                if !self.palette_rate_limit_consume_one() {
                    continue;
                }
                match self.osc_21_query_color(key) {
                    Osc21QueryValue::Color(color) => {
                        let spec = ColorPalette::format_color_spec(color);
                        query_pairs.push(format!("{key}={spec}"));
                    }
                    Osc21QueryValue::Undefined => query_pairs.push(format!("{key}=")),
                    Osc21QueryValue::Unknown if osc_21_key_is_echoable(key) => {
                        query_pairs.push(format!("{key}=?"));
                    }
                    Osc21QueryValue::Unknown => {}
                }
                continue;
            }

            if value.is_empty() {
                self.osc_21_set_dynamic_color(key);
                continue;
            }

            if let Some(color) = ColorPalette::parse_color_spec(value) {
                self.osc_21_set_color(key, color);
            }
        }

        if !query_pairs.is_empty() {
            let st = if self.transient.last_osc_bel_terminated {
                "\x07"
            } else {
                "\x1b\\"
            };
            let response = format!("\x1b]21;{}{}", query_pairs.join(";"), st);
            self.send_response(cap, response.as_bytes());
        }
    }

    fn osc_21_query_color(&mut self, key: &str) -> Osc21QueryValue {
        if let Some(slot) = DynamicColorSlot::from_name(key) {
            if slot == DynamicColorSlot::Cursor && self.color.cursor_color.is_none() {
                return Osc21QueryValue::Undefined;
            }
            return Osc21QueryValue::Color(self.get_dynamic_color(slot));
        }
        if key.eq_ignore_ascii_case("selection_background") {
            return self
                .color
                .selection_background
                .map_or(Osc21QueryValue::Undefined, Osc21QueryValue::Color);
        }
        if key.eq_ignore_ascii_case("selection_foreground") {
            return self
                .color
                .selection_foreground
                .map_or(Osc21QueryValue::Undefined, Osc21QueryValue::Color);
        }
        let Some(index) = parse_palette_index(key) else {
            return Osc21QueryValue::Unknown;
        };
        Osc21QueryValue::Color(self.color.palette().get(index))
    }

    fn osc_21_set_dynamic_color(&mut self, key: &str) {
        if key.eq_ignore_ascii_case("cursor") {
            self.color.cursor_color = None;
            self.note_color_change(super::ColorTarget::Cursor);
            return;
        }
        if key.eq_ignore_ascii_case("selection_background") {
            self.color.selection_background = None;
            self.note_color_change(super::ColorTarget::SelectionBackground);
            return;
        }
        if key.eq_ignore_ascii_case("selection_foreground") {
            self.color.selection_foreground = None;
            self.note_color_change(super::ColorTarget::SelectionForeground);
        }
    }

    fn osc_21_reset_color(&mut self, key: &str) {
        if let Some(slot) = DynamicColorSlot::from_name(key) {
            self.reset_dynamic_color_slot(slot);
            return;
        }
        if key.eq_ignore_ascii_case("selection_background") {
            self.reset_selection_background();
            return;
        }
        if key.eq_ignore_ascii_case("selection_foreground") {
            self.reset_selection_foreground();
            return;
        }
        let Some(index) = parse_palette_index(key) else {
            return;
        };
        let color = if let Some(ref configured) = self.color.configured_palette {
            configured.get(index)
        } else {
            ColorPalette::new().get(index)
        };
        self.color.palette_mut().set(index, color);
        self.note_color_change(super::ColorTarget::Palette);
    }

    fn osc_21_set_color(&mut self, key: &str, color: Rgb) {
        if let Some(slot) = DynamicColorSlot::from_name(key) {
            self.set_dynamic_color(slot, color);
            return;
        }
        if key.eq_ignore_ascii_case("selection_background") {
            self.color.selection_background = Some(color);
            self.note_color_change(super::ColorTarget::SelectionBackground);
            return;
        }
        if key.eq_ignore_ascii_case("selection_foreground") {
            self.color.selection_foreground = Some(color);
            self.note_color_change(super::ColorTarget::SelectionForeground);
            return;
        }
        if let Some(index) = parse_palette_index(key) {
            if !self.modes.allow_palette_reconfigure {
                self.note_palette_set_refused();
                return;
            }
            self.color.palette_mut().set(index, color);
            self.note_color_change(super::ColorTarget::Palette);
        }
    }

    /// Handle OSC 104: reset indexed colors.
    pub(super) fn handle_osc_104(&mut self, params: &[&[u8]]) {
        if params.len() <= 1 {
            let configured = self.color.configured_palette.clone();
            if let Some(configured) = configured {
                *self.color.palette_mut() = configured;
            } else {
                self.color.palette_mut().reset();
            }
        } else {
            let mut any_reset = false;
            for param in &params[1..] {
                if let Ok(index_str) = std::str::from_utf8(param)
                    && let Some(index) = parse_palette_index(index_str)
                {
                    let configured_color = self
                        .color
                        .configured_palette
                        .as_ref()
                        .map(|configured| configured.get(index));
                    if let Some(color) = configured_color {
                        self.color.palette_mut().set(index, color);
                    } else {
                        self.color.palette_mut().reset_color(index);
                    }
                    any_reset = true;
                }
            }
            if !any_reset {
                return;
            }
        }
        self.note_color_change(super::ColorTarget::Palette);
    }

    /// Handle OSC 30001.
    pub(super) fn handle_osc_30001(&mut self) {
        use super::callbacks::COLOR_STACK_MAX_DEPTH;

        if self.color.stack.len() >= COLOR_STACK_MAX_DEPTH {
            self.color.stack.pop_front();
        }
        let entry = self.color.capture_overrides();
        self.color.stack.push_back(entry);
    }

    /// Handle OSC 30101: pop the kitty color stack.
    pub(super) fn handle_osc_30101(&mut self) {
        if let Some(entry) = self.color.stack.pop_back() {
            let palette = self.modes.allow_palette_reconfigure;
            self.color.restore_baseline(palette);
            self.color.apply_overrides(&entry, palette);
            if palette {
                self.note_color_change(super::ColorTarget::Palette);
            }
            self.note_color_change(super::ColorTarget::Foreground);
            self.note_color_change(super::ColorTarget::Background);
            self.note_color_change(super::ColorTarget::Cursor);
            self.note_color_change(super::ColorTarget::SelectionBackground);
            self.note_color_change(super::ColorTarget::SelectionForeground);
        }
    }

    /// Handle OSC 17: highlight (selection) background color.
    pub(super) fn handle_osc_17(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        let Some(spec_bytes) = params.get(1) else {
            return;
        };
        let Ok(spec) = std::str::from_utf8(spec_bytes) else {
            return;
        };
        if spec == "?" {
            let color = self
                .color
                .selection_background
                .unwrap_or(self.color.default_background);
            let st = if self.transient.last_osc_bel_terminated {
                "\x07"
            } else {
                "\x1b\\"
            };
            let response = format!("\x1b]17;{}{}", ColorPalette::format_color_spec(color), st);
            self.send_response(cap, response.as_bytes());
        } else if let Some(color) = ColorPalette::parse_color_spec(spec) {
            self.color.selection_background = Some(color);
            self.note_color_change(super::ColorTarget::SelectionBackground);
        }
    }

    /// Reset the selection background color (OSC 117) to the default selection appearance.
    #[inline]
    pub(super) fn reset_selection_background(&mut self) {
        self.color.selection_background = self.color.configured_selection_background;
        self.note_color_change(super::ColorTarget::SelectionBackground);
    }

    /// Reset the selection foreground color (OSC 119) to the default selection appearance.
    #[inline]
    pub(super) fn reset_selection_foreground(&mut self) {
        self.color.selection_foreground = self.color.configured_selection_foreground;
        self.note_color_change(super::ColorTarget::SelectionForeground);
    }

    /// Reset a dynamic color to its default (OSC 110/111/112).
    #[inline]
    pub(super) fn reset_dynamic_color(&mut self, index: usize) {
        if let Some(slot) = DynamicColorSlot::from_index(index) {
            self.reset_dynamic_color_slot(slot);
        }
    }

    /// Get the current color for a dynamic color slot.
    #[inline]
    pub(super) const fn get_dynamic_color(&self, slot: DynamicColorSlot) -> Rgb {
        match slot {
            DynamicColorSlot::Foreground => self.color.default_foreground,
            DynamicColorSlot::Background => self.color.default_background,
            DynamicColorSlot::Cursor => match self.color.cursor_color {
                Some(color) => color,
                None => self.color.default_foreground,
            },
        }
    }

    /// Set a dynamic color and note the change.
    #[inline]
    pub(super) fn set_dynamic_color(&mut self, slot: DynamicColorSlot, color: Rgb) {
        match slot {
            DynamicColorSlot::Foreground => self.color.default_foreground = color,
            DynamicColorSlot::Background => self.color.default_background = color,
            DynamicColorSlot::Cursor => self.color.cursor_color = Some(color),
        }
        self.note_color_change(slot.color_target());
    }

    /// Reset a dynamic color to its theme-configured default.
    #[inline]
    fn reset_dynamic_color_slot(&mut self, slot: DynamicColorSlot) {
        match slot {
            DynamicColorSlot::Foreground => {
                self.color.default_foreground = self.color.configured_foreground;
            }
            DynamicColorSlot::Background => {
                self.color.default_background = self.color.configured_background;
            }
            DynamicColorSlot::Cursor => {
                self.color.cursor_color = self.color.configured_cursor;
            }
        }
        self.note_color_change(slot.color_target());
    }

    /// Record a palette set refused by the `allow_palette_reconfigure` gate.
    #[inline]
    fn note_palette_set_refused(&mut self) {
        if self.color.palette_set_refused {
            return;
        }
        self.color.palette_set_refused = true;
        if let Some(callback) = self.color.palette_refused_callback.as_mut() {
            callback();
        }
    }

    /// Record an OSC color set or reset of `target`.
    #[inline]
    pub(super) fn note_color_change(&mut self, target: super::ColorTarget) {
        match target {
            super::ColorTarget::Foreground => {
                self.color.frame_foreground_authoritative = true;
            }
            super::ColorTarget::Background => {
                self.color.frame_background_authoritative = true;
            }
            super::ColorTarget::Cursor => {
                self.color.frame_cursor_authoritative = true;
            }
            _ => {}
        }
        self.grid.damage_mut().mark_full();
    }
}

#[cfg(test)]
#[path = "handler_osc_color_osc_color_response_cap_tests.rs"]
mod osc_color_response_cap_tests;
