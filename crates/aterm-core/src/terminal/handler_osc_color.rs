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

            let Ok(index) = index_str.parse::<u8>() else {
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
        let Ok(index) = key.parse::<u8>() else {
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
        let Ok(index) = key.parse::<u8>() else {
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
        if let Ok(index) = key.parse::<u8>() {
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
                    && let Ok(index) = index_str.parse::<u8>()
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
mod osc_color_response_cap_tests {
    //! The per-sequence response cap on OSC palette queries. The parser stops at
    //! 16 OSC params (`MAX_OSC_PARAMS`), so these tests call the handler
    //! directly via `split_for_process` to feed more.
    use super::LEGACY_PALETTE_PER_SEQUENCE_MAX;
    use crate::terminal::Terminal;
    use crate::terminal::response_capability::ResponseCapability;

    fn count_osc_frames(bytes: &[u8]) -> usize {
        let mut count = 0;
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b']' {
                count += 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        count
    }

    fn build_osc_4_query_params(n: usize) -> Vec<Vec<u8>> {
        let mut params: Vec<Vec<u8>> = Vec::with_capacity(1 + n * 2);
        params.push(b"4".to_vec());
        for idx in 0..n {
            params.push(idx.to_string().into_bytes());
            params.push(b"?".to_vec());
        }
        params
    }

    #[test]
    fn osc_4_query_256_pairs_capped_at_per_sequence_limit() {
        let mut term = Terminal::new(24, 80);
        let params_owned = build_osc_4_query_params(256);

        let cap = ResponseCapability::mint_for_dispatch();
        let (_parser, mut handler) = term.split_for_process();
        let params_refs: Vec<&[u8]> = params_owned.iter().map(Vec::as_slice).collect();
        handler.handle_osc_4(&cap, &params_refs);
        drop(handler);

        let response = term
            .take_response()
            .expect("256-pair OSC 4 query must emit at least one response");
        let frames = count_osc_frames(&response);
        assert_eq!(
            frames, LEGACY_PALETTE_PER_SEQUENCE_MAX,
            "256-pair OSC 4 query must emit exactly {} response frames, got {}. \
             Without the cap this would be 256.",
            LEGACY_PALETTE_PER_SEQUENCE_MAX, frames,
        );
    }

    #[test]
    fn osc_4_query_8_pairs_all_emit_below_cap() {
        let mut term = Terminal::new(24, 80);
        let params_owned = build_osc_4_query_params(8);

        let cap = ResponseCapability::mint_for_dispatch();
        let (_parser, mut handler) = term.split_for_process();
        let params_refs: Vec<&[u8]> = params_owned.iter().map(Vec::as_slice).collect();
        handler.handle_osc_4(&cap, &params_refs);
        drop(handler);

        let response = term
            .take_response()
            .expect("8-pair OSC 4 query must emit responses");
        let frames = count_osc_frames(&response);
        assert_eq!(
            frames, 8,
            "8-pair OSC 4 query (below cap of {}) must emit all 8 response \
             frames, got {}",
            LEGACY_PALETTE_PER_SEQUENCE_MAX, frames,
        );
    }

    #[test]
    fn osc_4_set_operations_unaffected_by_query_cap() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_palette_reconfigure = true;

        let mut params_owned: Vec<Vec<u8>> = Vec::new();
        params_owned.push(b"4".to_vec());
        for idx in 0..=LEGACY_PALETTE_PER_SEQUENCE_MAX {
            params_owned.push(idx.to_string().into_bytes());
            params_owned.push(b"?".to_vec());
        }
        params_owned.push(b"200".to_vec());
        params_owned.push(b"rgb:ff/00/00".to_vec());

        let cap = ResponseCapability::mint_for_dispatch();
        let (_parser, mut handler) = term.split_for_process();
        let params_refs: Vec<&[u8]> = params_owned.iter().map(Vec::as_slice).collect();
        handler.handle_osc_4(&cap, &params_refs);
        drop(handler);

        let response = term.take_response().unwrap_or_default();
        let frames = count_osc_frames(&response);
        assert_eq!(
            frames, LEGACY_PALETTE_PER_SEQUENCE_MAX,
            "query responses must be capped at {} even when mixed with set \
             operations",
            LEGACY_PALETTE_PER_SEQUENCE_MAX,
        );
        assert_eq!(
            term.palette_color(200),
            aterm_types::Rgb::new(0xff, 0x00, 0x00),
            "SET operation after the query cap must still apply",
        );
    }

    #[test]
    fn osc_21_query_pairs_capped_at_per_sequence_limit() {
        let mut term = Terminal::new(24, 80);

        let mut params_owned: Vec<Vec<u8>> = Vec::new();
        params_owned.push(b"21".to_vec());
        for idx in 0u8..32 {
            params_owned.push(format!("{idx}=?").into_bytes());
        }

        let cap = ResponseCapability::mint_for_dispatch();
        let (_parser, mut handler) = term.split_for_process();
        let params_refs: Vec<&[u8]> = params_owned.iter().map(Vec::as_slice).collect();
        handler.handle_osc_21(&cap, &params_refs);
        drop(handler);

        let response = term
            .take_response()
            .expect("OSC 21 query must emit a response");
        let reply_str = std::str::from_utf8(&response).expect("OSC 21 reply is ASCII");
        let body = reply_str
            .strip_prefix("\x1b]21;")
            .and_then(|s| s.strip_suffix("\x1b\\").or_else(|| s.strip_suffix('\x07')))
            .expect("OSC 21 reply must carry header and terminator");
        let pairs: Vec<&str> = body.split(';').collect();
        assert_eq!(
            pairs.len(),
            LEGACY_PALETTE_PER_SEQUENCE_MAX,
            "OSC 21 response must contain exactly {} pairs (cap), got {}",
            LEGACY_PALETTE_PER_SEQUENCE_MAX,
            pairs.len(),
        );
        for (idx, pair) in pairs.iter().enumerate() {
            assert!(
                pair.starts_with(&format!("{idx}=rgb:")),
                "unexpected OSC 21 pair[{idx}]: {pair:?}"
            );
        }
    }

    #[test]
    fn osc_recolor_with_no_glyph_marks_damage_and_advances_epoch() {
        let mut term = Terminal::new(24, 80);
        term.process(b"X");
        term.take_damage();
        let epoch0 = term.damage_epoch();
        assert!(
            !term.has_damage(),
            "damage consumed by the simulated present"
        );

        term.process(b"\x1b]11;rgb:0000/0000/8080\x1b\\");
        assert!(
            term.has_damage(),
            "an OSC default-bg recolor must mark grid damage even with no glyph written",
        );
        assert_eq!(
            term.damage_epoch(),
            epoch0 + 1,
            "the recolor must advance damage_epoch so the frontend repaints",
        );
    }

    #[test]
    fn osc_19_selection_foreground_set_query_and_reset_are_stateful() {
        let mut term = Terminal::new(4, 8);
        term.take_damage();
        let before = term.damage_epoch();
        let foreground = aterm_types::Rgb::new(0x11, 0x22, 0x33);

        term.process(b"\x1b]19;rgb:11/22/33\x1b\\");
        assert_eq!(term.selection_foreground(), Some(foreground));
        assert!(
            term.has_damage(),
            "OSC 19 SET must repaint a live selection"
        );
        assert!(
            term.damage_epoch() > before,
            "OSC 19 SET advances the frontend redraw key"
        );

        term.process(b"\x1b]19;?\x07");
        let expected = format!(
            "\x1b]19;{}\x07",
            aterm_types::ColorPalette::format_color_spec(foreground),
        )
        .into_bytes();
        assert_eq!(term.take_response(), Some(expected));

        term.take_damage();
        term.process(b"\x1b]119\x1b\\");
        assert_eq!(term.selection_foreground(), None);
        assert!(
            term.has_damage(),
            "OSC 119 RESET must repaint a live selection"
        );
    }

    #[test]
    fn osc_21_selection_foreground_set_and_query_share_osc_19_state() {
        let mut term = Terminal::new(4, 8);
        let foreground = aterm_types::Rgb::new(0x12, 0x34, 0x56);

        term.process(b"\x1b]21;selection_foreground=rgb:12/34/56\x07");
        assert_eq!(term.selection_foreground(), Some(foreground));

        term.process(b"\x1b]21;selection_foreground=?\x07");
        let expected = format!(
            "\x1b]21;selection_foreground={}\x07",
            aterm_types::ColorPalette::format_color_spec(foreground),
        )
        .into_bytes();
        assert_eq!(term.take_response(), Some(expected));
    }

    #[test]
    fn osc_21_queries_report_undefined_and_unknown_fields() {
        let mut term = Terminal::new(4, 8);

        term.process(
            b"\x1b]21;selection_background=?;selection_foreground=?;\
              cursor=?;not_a_color=?\x1b\\",
        );
        assert_eq!(
            term.take_response(),
            Some(
                b"\x1b]21;selection_background=;selection_foreground=;\
                  cursor=;not_a_color=?\x1b\\"
                    .to_vec()
            ),
        );
    }

    #[test]
    fn osc_21_unknown_key_is_echoed_only_when_well_formed() {
        let mut term = Terminal::new(4, 8);
        let long = "k".repeat(super::MAX_OSC21_ECHO_KEY_BYTES + 1);
        let seq = format!(
            "\x1b]21;rm\x7f-rf=?;echo pwned=?;a/b=?;{long}=?;transparent_background_color1=?\x1b\\"
        );
        term.process(seq.as_bytes());
        assert_eq!(
            term.take_response(),
            Some(b"\x1b]21;transparent_background_color1=?\x1b\\".to_vec()),
        );

        term.process(b"\x1b]21;bad key=?;x\x7f=?\x1b\\");
        assert_eq!(term.take_response(), None, "nothing echoable, no reply");
    }

    #[test]
    fn osc_21_empty_values_are_dynamic_and_bare_keys_reset() {
        let mut term = Terminal::new(4, 8);
        let configured_cursor = aterm_types::Rgb::new(0x10, 0x20, 0x30);
        let configured_bg = aterm_types::Rgb::new(0x40, 0x50, 0x60);
        let configured_fg = aterm_types::Rgb::new(0x70, 0x80, 0x90);
        let mut config = crate::config::TerminalConfig::default();
        config.cursor_color = Some(configured_cursor);
        config.selection_background = Some(configured_bg);
        config.selection_foreground = Some(configured_fg);
        term.apply_config(&config);

        term.process(
            b"\x1b]21;cursor=rgb:aa/bb/cc;\
              selection_background=rgb:bb/cc/dd;\
              selection_foreground=rgb:cc/dd/ee\x1b\\",
        );
        term.process(
            b"\x1b]21;cursor=;selection_background=;\
              selection_foreground=\x1b\\",
        );
        assert_eq!(term.cursor_color(), None);
        assert_eq!(term.selection_background(), None);
        assert_eq!(term.selection_foreground(), None);
        assert_eq!(
            term.cell_frame(4, 8).selection_fg,
            crate::render::COLOR_DYNAMIC,
            "the frame must preserve an explicit automatic foreground instead of delegating to a stale renderer setting",
        );

        term.process(
            b"\x1b]21;cursor;selection_background;\
              selection_foreground\x1b\\",
        );
        assert_eq!(term.cursor_color(), Some(configured_cursor));
        assert_eq!(term.selection_background(), Some(configured_bg));
        assert_eq!(term.selection_foreground(), Some(configured_fg));
    }

    #[test]
    fn osc_21_bare_numeric_key_resets_without_set_permission() {
        let mut term = Terminal::new(4, 8);
        let default = term.palette_color(42);

        let mut palette = aterm_types::ColorPalette::new();
        palette.set(42, aterm_types::Rgb::new(0xaa, 0xbb, 0xcc));
        let mut config = crate::config::TerminalConfig::default();
        config.custom_palette = Some(palette);
        term.apply_config(&config);
        assert_eq!(
            term.palette_color(42),
            aterm_types::Rgb::new(0xaa, 0xbb, 0xcc)
        );

        term.modes_mut().allow_palette_reconfigure = true;
        term.process(b"\x1b]21;42=rgb:01/02/03\x1b\\");
        assert_eq!(
            term.palette_color(42),
            aterm_types::Rgb::new(0x01, 0x02, 0x03)
        );
        term.modes_mut().allow_palette_reconfigure = false;
        term.process(b"\x1b]21;42\x1b\\");
        assert_eq!(
            term.palette_color(42),
            aterm_types::Rgb::new(0xaa, 0xbb, 0xcc)
        );

        config.custom_palette = None;
        term.apply_config(&config);
        term.modes_mut().allow_palette_reconfigure = true;
        term.process(b"\x1b]21;42=rgb:04/05/06\x1b\\");
        assert_eq!(
            term.palette_color(42),
            aterm_types::Rgb::new(0x04, 0x05, 0x06)
        );
        term.modes_mut().allow_palette_reconfigure = false;
        term.process(b"\x1b]21;42\x1b\\");
        assert_eq!(term.palette_color(42), default);
    }

    #[test]
    fn color_stack_pop_restores_a_dynamic_cursor_as_dynamic() {
        let mut term = Terminal::new(4, 8);
        term.process(
            b"\x1b]30001\x1b\\\
              \x1b]12;rgb:aa/bb/cc\x1b\\",
        );
        assert!(term.cursor_color().is_some());
        term.process(b"\x1b]30101\x1b\\");
        assert_eq!(term.cursor_color(), None);
    }

    #[test]
    fn color_stack_round_trip_restores_both_selection_colors() {
        let mut term = Terminal::new(4, 8);
        let saved_bg = aterm_types::Rgb::new(0x10, 0x20, 0x30);
        let saved_fg = aterm_types::Rgb::new(0x40, 0x50, 0x60);

        term.process(
            b"\x1b]17;rgb:10/20/30\x1b\\\
              \x1b]19;rgb:40/50/60\x1b\\\
              \x1b]30001\x1b\\\
              \x1b]17;rgb:a0/b0/c0\x1b\\\
              \x1b]19;rgb:d0/e0/f0\x1b\\\
              \x1b]30101\x1b\\",
        );

        assert_eq!(term.selection_background(), Some(saved_bg));
        assert_eq!(term.selection_foreground(), Some(saved_fg));
    }

    #[test]
    fn selection_color_resets_and_ris_restore_host_baselines() {
        let mut term = Terminal::new(4, 8);
        let configured_bg = aterm_types::Rgb::new(0x21, 0x32, 0x43);
        let configured_fg = aterm_types::Rgb::new(0x54, 0x65, 0x76);
        let mut config = crate::config::TerminalConfig::default();
        config.selection_background = Some(configured_bg);
        config.selection_foreground = Some(configured_fg);
        term.apply_config(&config);

        term.process(
            b"\x1b]17;rgb:aa/bb/cc\x1b\\\
              \x1b]19;rgb:dd/ee/ff\x1b\\\
              \x1b]117\x1b\\\
              \x1b]119\x1b\\",
        );
        assert_eq!(term.selection_background(), Some(configured_bg));
        assert_eq!(term.selection_foreground(), Some(configured_fg));

        term.process(
            b"\x1b]17;rgb:aa/bb/cc\x1b\\\
              \x1b]19;rgb:dd/ee/ff\x1b\\\
              \x1bc",
        );
        assert_eq!(
            term.selection_background(),
            Some(configured_bg),
            "RIS restores the configured selection background"
        );
        assert_eq!(
            term.selection_foreground(),
            Some(configured_fg),
            "RIS restores configured selected-text ink"
        );
    }
}
