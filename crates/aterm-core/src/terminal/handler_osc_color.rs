// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC color handlers: OSC 4 (palette), 10/11/12 (default fg/bg/cursor),
//! 17/19 (selection bg/fg), 21 (kitty extended color queries), 104 (reset
//! indexed colors) and 30001/30101 (kitty color stack push/pop).

use super::handler::TerminalHandler;
use aterm_types::{ColorPalette, Rgb};

/// Per-sequence response cap for OSC 4 / OSC 21 palette queries when no
/// [`PolicyEngine`][pe] is installed (with one, the cap and cross-sequence
/// throttle come from its `"palette"` rate limit).
///
/// The response rate limiter bounds only total output, so this caps one
/// sequence's `;idx;?` amplification; pairs past it are dropped. 16 covers
/// palette inspectors while bounding a 256-index query to ~320 bytes.
///
/// [pe]: aterm_policy::engine::PolicyEngine
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
    pub(super) fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Foreground),
            1 => Some(Self::Background),
            2 => Some(Self::Cursor),
            _ => None,
        }
    }

    /// Parse from a case-insensitive color name (OSC 21 / kitty protocol).
    pub(super) fn from_name(name: &str) -> Option<Self> {
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

    /// The [`ColorTarget`](super::ColorTarget) for change/query callbacks.
    pub(super) fn color_target(self) -> super::ColorTarget {
        match self {
            Self::Foreground => super::ColorTarget::Foreground,
            Self::Background => super::ColorTarget::Background,
            Self::Cursor => super::ColorTarget::Cursor,
        }
    }
}

/// OSC 21 queries distinguish a concrete color, a known field whose value is
/// dynamic/undefined, and an unknown field. Kitty assigns different wire
/// responses to all three (`rgb:...`, empty, and `?`, respectively).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Osc21QueryValue {
    Color(Rgb),
    Undefined,
    Unknown,
}

/// Longest unknown OSC 21 key echoed back; kitty's longest is 30 bytes.
const MAX_OSC21_ECHO_KEY_BYTES: usize = 64;

/// Whether an unrecognised OSC 21 key may be echoed in a `key=?` reply: only
/// the spelling kitty's key names use (ASCII letters, digits, `_`, `-`), short.
fn osc_21_key_is_echoable(key: &str) -> bool {
    key.len() <= MAX_OSC21_ECHO_KEY_BYTES
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl TerminalHandler<'_> {
    /// The per-sequence OSC palette-query response cap: the active policy
    /// engine's `"palette"` `per_sequence_max`, or
    /// [`LEGACY_PALETTE_PER_SEQUENCE_MAX`] without an engine. A policy value of
    /// 0 ("disabled") also falls back, so the handler always has a cap.
    pub(super) fn palette_per_sequence_cap(&self) -> usize {
        if let Some(cfg) = self.policy.rate_limit_config("palette")
            && cfg.per_sequence_max > 0
        {
            return cfg.per_sequence_max as usize;
        }
        LEGACY_PALETTE_PER_SEQUENCE_MAX
    }

    /// Debit the policy engine's `"palette"` bucket by one response pair;
    /// `true` when permitted. Without an engine this returns `true` and the
    /// per-sequence cap is the only bound.
    fn palette_rate_limit_consume_one(&mut self) -> bool {
        let clock = aterm_policy::limits::SystemClock;
        self.policy
            .rate_limit_try_consume("palette", 1, &clock)
            .unwrap_or(true)
    }

    /// Handle OSC 4 (`OSC 4 ; index ; spec ST`, pairs repeating): set a palette
    /// entry, or query it with `?`.
    pub(super) fn handle_osc_4(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.len() < 3 {
            return; // Need at least: 4, index, spec
        }

        // Process pairs: index, spec, index, spec, ... Query responses from this
        // sequence are capped (`palette_per_sequence_cap()`) so a 256-index query
        // cannot amplify; set operations are unaffected.
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
                // Drop queries past the per-sequence cap, but keep scanning so
                // later set operations still apply.
                if responses_emitted >= per_seq_cap {
                    i += 2;
                    continue;
                }
                // Cross-sequence rate limit via the policy engine's `"palette"`
                // bucket; a denial drops this pair like the per-sequence cap.
                if !self.palette_rate_limit_consume_one() {
                    i += 2;
                    continue;
                }
                // Replies echo the request's terminator (BEL vs ST), as xterm's do.
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
                // Set: gated by `modes.allow_palette_reconfigure` (default off);
                // queries above stay ungated. A refused set is reported to the
                // host once per session; an invalid spec is ignored. Sets emit
                // no PTY response.
                if let Some(color) = ColorPalette::parse_color_spec(spec) {
                    if self.modes.allow_palette_reconfigure {
                        self.color.palette_mut().set(index, color);
                        self.fire_color_change_callback(
                            super::ColorTarget::Palette,
                            color,
                            super::ColorChangeOp::Set,
                        );
                    } else {
                        self.note_palette_set_refused();
                    }
                }
            }

            i += 2;
        }
    }

    /// Handle OSC 10/11/12 (foreground, background, cursor): `start_at` 0, 1 or
    /// 2 picks the first slot and further specs cascade to the next (OSC 10 with
    /// several sets fg, then bg, then cursor). `?` answers
    /// `OSC N ; rgb:RRRR/GGGG/BBBB ST`.
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
                let palette_color = self.get_dynamic_color(slot);
                let color = self
                    .color
                    .query_callback
                    .as_mut()
                    .and_then(|cb| cb(slot.color_target()))
                    .unwrap_or(palette_color);
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

    /// Handle OSC 19, the selection foreground (`OSC 19 ; spec ST`; `?` queries,
    /// answered with the request's terminator). With no explicit selection
    /// foreground the query reports the default foreground.
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
            self.fire_color_change_callback(
                super::ColorTarget::SelectionForeground,
                color,
                super::ColorChangeOp::Set,
            );
        }
    }

    /// Handle OSC 21, kitty's extended colors (`OSC 21 ; key=value ; ... ST`).
    /// Keys name `foreground`, `background`, `cursor`, `selection_background`,
    /// `selection_foreground` or a palette index `0-255`. `key=?` answers
    /// `key=rgb:...`, `key=` (known but dynamic/undefined) or `key=?` (unknown);
    /// an empty value selects dynamic behavior where supported; a bare key
    /// resets to the host-configured default.
    pub(super) fn handle_osc_21(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.len() < 2 {
            return;
        }

        // Per-sequence response-pair cap (see `palette_per_sequence_cap`).
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
                    // Kitty answers an unknown key with `key=?`, the one reply
                    // carrying bytes the terminal did not produce: echo only a
                    // key spelled like a real one, so no DEL, space or
                    // punctuation reaches the shell's input.
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
            // The host's query callback may override, as for OSC 10/11/12.
            let palette_color = self.get_dynamic_color(slot);
            if let Some(color) = self
                .color
                .query_callback
                .as_mut()
                .and_then(|cb| cb(slot.color_target()))
            {
                return Osc21QueryValue::Color(color);
            }
            // Unlike OSC 12's legacy single-color query, OSC 21 can faithfully
            // report a dynamic cursor as undefined. A host query override above
            // wins because it describes the color the host actually renders.
            if slot == DynamicColorSlot::Cursor && self.color.cursor_color.is_none() {
                return Osc21QueryValue::Undefined;
            }
            return Osc21QueryValue::Color(palette_color);
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

    /// Select a dynamic/undefined value for the OSC 21 fields that support it.
    /// Foreground, background, and indexed palette entries have no dynamic
    /// representation in the core and therefore ignore an empty value.
    fn osc_21_set_dynamic_color(&mut self, key: &str) {
        if key.eq_ignore_ascii_case("cursor") {
            self.color.cursor_color = None;
            self.fire_color_change_callback(
                super::ColorTarget::Cursor,
                self.color.default_foreground,
                super::ColorChangeOp::Dynamic,
            );
            return;
        }
        if key.eq_ignore_ascii_case("selection_background") {
            self.color.selection_background = None;
            self.fire_color_change_callback(
                super::ColorTarget::SelectionBackground,
                self.color.default_background,
                super::ColorChangeOp::Dynamic,
            );
            return;
        }
        if key.eq_ignore_ascii_case("selection_foreground") {
            self.color.selection_foreground = None;
            self.fire_color_change_callback(
                super::ColorTarget::SelectionForeground,
                self.color.default_foreground,
                super::ColorChangeOp::Dynamic,
            );
        }
    }

    /// Reset a bare OSC 21 key to the same host-configured baseline as its
    /// dedicated reset sequence (OSC 104/110-112/117/119).
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
        self.fire_color_change_callback(
            super::ColorTarget::Palette,
            color,
            super::ColorChangeOp::Reset,
        );
    }

    fn osc_21_set_color(&mut self, key: &str, color: Rgb) {
        if let Some(slot) = DynamicColorSlot::from_name(key) {
            // Only the indexed palette is gated; named colors are theme surface.
            self.set_dynamic_color(slot, color);
            return;
        }
        if key.eq_ignore_ascii_case("selection_background") {
            self.color.selection_background = Some(color);
            self.fire_color_change_callback(
                super::ColorTarget::SelectionBackground,
                color,
                super::ColorChangeOp::Set,
            );
            return;
        }
        if key.eq_ignore_ascii_case("selection_foreground") {
            self.color.selection_foreground = Some(color);
            self.fire_color_change_callback(
                super::ColorTarget::SelectionForeground,
                color,
                super::ColorChangeOp::Set,
            );
            return;
        }
        if let Ok(index) = key.parse::<u8>() {
            // A palette slot: gated by allow_palette_reconfigure (fail-closed).
            if !self.modes.allow_palette_reconfigure {
                self.note_palette_set_refused();
                return;
            }
            self.color.palette_mut().set(index, color);
            self.fire_color_change_callback(
                super::ColorTarget::Palette,
                color,
                super::ColorChangeOp::Set,
            );
        }
    }

    /// Handle OSC 104: reset indexed colors (`OSC 104 [; index ...] ST`), all
    /// 256 when no index is given. Colors reset to the theme palette from
    /// `apply_config` when one exists, like OSC 110/111's
    /// `configured_foreground`/`configured_background`.
    pub(super) fn handle_osc_104(&mut self, params: &[&[u8]]) {
        if params.len() <= 1 {
            // No indices: reset all to the theme palette, else xterm defaults.
            let configured = self.color.configured_palette.clone();
            if let Some(configured) = configured {
                *self.color.palette_mut() = configured;
            } else {
                self.color.palette_mut().reset();
            }
        } else {
            // The callback fires only if at least one index was valid.
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
        self.fire_color_change_callback(
            super::ColorTarget::Palette,
            Rgb { r: 0, g: 0, b: 0 },
            super::ColorChangeOp::Reset,
        );
    }

    /// Handle OSC 30001: push the color state (256-color palette, default
    /// fg/bg, cursor, selection bg/fg) onto the kitty color stack, max depth
    /// 16, dropping the oldest entry when full.
    ///
    /// The entry is the program's diff from the configured baseline, not the
    /// absolute colors, so a pop after a theme switch lands on the new theme.
    pub(super) fn handle_osc_30001(&mut self) {
        use super::callbacks::COLOR_STACK_MAX_DEPTH;

        if self.color.stack.len() >= COLOR_STACK_MAX_DEPTH {
            self.color.stack.pop_front();
        }
        let entry = self.color.capture_overrides();
        self.color.stack.push_back(entry);
    }

    /// Handle OSC 30101: pop the kitty color stack and restore all dynamic
    /// colors to the baseline plus the popped diff; a no-op when empty.
    ///
    /// The palette is restored only under `allow_palette_reconfigure`, like
    /// OSC 4/21 sets: a pop is a program writing the palette too.
    pub(super) fn handle_osc_30101(&mut self) {
        if let Some(entry) = self.color.stack.pop_back() {
            let palette = self.modes.allow_palette_reconfigure;
            self.color.restore_baseline(palette);
            self.color.apply_overrides(&entry, palette);
            let (fg, bg) = (self.color.default_foreground, self.color.default_background);

            if palette {
                self.fire_color_change_callback(
                    super::ColorTarget::Palette,
                    Rgb { r: 0, g: 0, b: 0 },
                    super::ColorChangeOp::Set,
                );
            }
            self.fire_color_change_callback(
                super::ColorTarget::Foreground,
                fg,
                super::ColorChangeOp::Set,
            );
            self.fire_color_change_callback(
                super::ColorTarget::Background,
                bg,
                super::ColorChangeOp::Set,
            );
            // Keep explicit vs dynamic: reporting a dynamic cursor as
            // `Set(foreground)` would freeze renderers on that foreground.
            if let Some(cursor_color) = self.color.cursor_color {
                self.fire_color_change_callback(
                    super::ColorTarget::Cursor,
                    cursor_color,
                    super::ColorChangeOp::Set,
                );
            } else {
                self.fire_color_change_callback(
                    super::ColorTarget::Cursor,
                    self.color.default_foreground,
                    super::ColorChangeOp::Dynamic,
                );
            }
            // A None selection background is dynamic, not a reset.
            if let Some(sel_bg) = self.color.selection_background {
                self.fire_color_change_callback(
                    super::ColorTarget::SelectionBackground,
                    sel_bg,
                    super::ColorChangeOp::Set,
                );
            } else {
                self.fire_color_change_callback(
                    super::ColorTarget::SelectionBackground,
                    self.color.default_background,
                    super::ColorChangeOp::Dynamic,
                );
            }
            // Selection foreground (OSC 19) likewise.
            if let Some(sel_fg) = self.color.selection_foreground {
                self.fire_color_change_callback(
                    super::ColorTarget::SelectionForeground,
                    sel_fg,
                    super::ColorChangeOp::Set,
                );
            } else {
                self.fire_color_change_callback(
                    super::ColorTarget::SelectionForeground,
                    self.color.default_foreground,
                    super::ColorChangeOp::Dynamic,
                );
            }
        }
    }

    /// Handle OSC 17: highlight (selection) background color. Set with
    /// `OSC 17 ; color-spec ST`, query with `OSC 17 ; ? ST`. Part of xterm's
    /// dynamic color family (OSC 10-19).
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
            self.fire_color_change_callback(
                super::ColorTarget::SelectionBackground,
                color,
                super::ColorChangeOp::Set,
            );
        }
    }

    /// Reset the selection background color (OSC 117) to the default
    /// selection appearance.
    pub(super) fn reset_selection_background(&mut self) {
        self.color.selection_background = self.color.configured_selection_background;
        let color = self
            .color
            .selection_background
            .unwrap_or(self.color.default_background);
        self.fire_color_change_callback(
            super::ColorTarget::SelectionBackground,
            color,
            super::ColorChangeOp::Reset,
        );
    }

    /// Reset the selection foreground color (OSC 119) to the default selection
    /// appearance.
    pub(super) fn reset_selection_foreground(&mut self) {
        self.color.selection_foreground = self.color.configured_selection_foreground;
        let color = self
            .color
            .selection_foreground
            .unwrap_or(self.color.default_foreground);
        self.fire_color_change_callback(
            super::ColorTarget::SelectionForeground,
            color,
            super::ColorChangeOp::Reset,
        );
    }

    /// Reset a dynamic color to its default (OSC 110/111/112).
    pub(super) fn reset_dynamic_color(&mut self, index: usize) {
        if let Some(slot) = DynamicColorSlot::from_index(index) {
            self.reset_dynamic_color_slot(slot);
        }
    }

    /// Get the current color for a dynamic color slot.
    pub(super) fn get_dynamic_color(&self, slot: DynamicColorSlot) -> Rgb {
        match slot {
            DynamicColorSlot::Foreground => self.color.default_foreground,
            DynamicColorSlot::Background => self.color.default_background,
            DynamicColorSlot::Cursor => self
                .color
                .cursor_color
                .unwrap_or(self.color.default_foreground),
        }
    }

    /// Set a dynamic color and fire the change callback.
    pub(super) fn set_dynamic_color(&mut self, slot: DynamicColorSlot, color: Rgb) {
        match slot {
            DynamicColorSlot::Foreground => self.color.default_foreground = color,
            DynamicColorSlot::Background => self.color.default_background = color,
            DynamicColorSlot::Cursor => self.color.cursor_color = Some(color),
        }
        self.fire_color_change_callback(slot.color_target(), color, super::ColorChangeOp::Set);
    }

    /// Reset a dynamic color to its theme-configured default (`configured_*`,
    /// set by `apply_config`) and fire the change callback.
    fn reset_dynamic_color_slot(&mut self, slot: DynamicColorSlot) {
        let color = match slot {
            DynamicColorSlot::Foreground => {
                let fg = self.color.configured_foreground;
                self.color.default_foreground = fg;
                fg
            }
            DynamicColorSlot::Background => {
                let bg = self.color.configured_background;
                self.color.default_background = bg;
                bg
            }
            DynamicColorSlot::Cursor => {
                self.color.cursor_color = self.color.configured_cursor;
                self.color
                    .configured_cursor
                    .unwrap_or(self.color.default_foreground)
            }
        };
        self.fire_color_change_callback(slot.color_target(), color, super::ColorChangeOp::Reset);
    }

    /// Record a palette set (OSC 4, numeric OSC 21) refused by the
    /// `allow_palette_reconfigure` gate. Only the session's first refusal calls
    /// the host's `palette_refused_callback` (a theme script sends dozens), so
    /// a theme tool that silently did nothing (base16-shell, pywal, `theme.sh`)
    /// can be traced.
    fn note_palette_set_refused(&mut self) {
        if self.color.palette_set_refused {
            return;
        }
        self.color.palette_set_refused = true;
        if let Some(callback) = self.color.palette_refused_callback.as_mut() {
            callback();
        }
    }

    /// Record a color change (snapshot authority, full damage) and fire the
    /// change callback if one is registered.
    pub(super) fn fire_color_change_callback(
        &mut self,
        target: super::ColorTarget,
        color: Rgb,
        op: super::ColorChangeOp,
    ) {
        // Once OSC touches a color, the terminal's value is authoritative for
        // owned render snapshots; `Terminal::new` leaves these unset so a
        // standalone renderer can use its own theme.
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
        // A recolor changes painted cells without changing content: mark full
        // damage (like DECSCNM) so it advances `damage_epoch` and is presented.
        self.grid.damage_mut().mark_full();
        if let Some(ref mut cb) = self.color.change_callback {
            cb(target, color, op);
        }
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

    /// Count OSC-response frames in a raw response buffer. Each OSC 4
    /// response starts with `ESC ]` (0x1b 0x5d). Inter-frame terminators
    /// are either BEL (0x07) or ST (ESC `\\`) which never match `ESC ]`.
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

    /// Build OSC 4 params with `n` query pairs, as owned byte vectors callers
    /// borrow as `&[&[u8]]`.
    fn build_osc_4_query_params(n: usize) -> Vec<Vec<u8>> {
        let mut params: Vec<Vec<u8>> = Vec::with_capacity(1 + n * 2);
        params.push(b"4".to_vec());
        for idx in 0..n {
            params.push(idx.to_string().into_bytes());
            params.push(b"?".to_vec());
        }
        params
    }

    /// A 256-pair OSC 4 query emits at most `LEGACY_PALETTE_PER_SEQUENCE_MAX`
    /// response frames.
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

    /// An 8-pair OSC 4 query (below the cap) emits all 8 responses.
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

    /// A single OSC 4 query through the real parser emits exactly one frame.
    #[test]
    fn osc_4_query_single_pair_emits_one_frame() {
        let mut term = Terminal::new(24, 80);

        term.process(b"\x1b]4;1;?\x1b\\");

        let response = term
            .take_response()
            .expect("single OSC 4 query must emit a response");
        let frames = count_osc_frames(&response);
        assert_eq!(frames, 1, "single OSC 4 query must emit exactly 1 frame");
    }

    /// Mixed set+query OSC 4: once the query cap is hit, later queries drop but
    /// later set operations in the same sequence still apply.
    #[test]
    fn osc_4_set_operations_unaffected_by_query_cap() {
        let mut term = Terminal::new(24, 80);
        // OSC 4 set is fail-closed by default; opt in to exercise the query cap.
        term.modes_mut().allow_palette_reconfigure = true;

        // Build params: `4`, then LEGACY_PALETTE_PER_SEQUENCE_MAX + 1 query
        // pairs, then one SET pair (index 200, rgb:ff/00/00).
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
        let (r, g, b) = term.palette_color_components(200);
        assert_eq!(
            (r, g, b),
            (0xff, 0x00, 0x00),
            "SET operation after the query cap must still apply",
        );
    }

    /// OSC 21 with more than LEGACY_PALETTE_PER_SEQUENCE_MAX query pairs bounds
    /// the pairs in its single reply. Calls the handler directly to exceed the
    /// parser's param limit.
    #[test]
    fn osc_21_query_pairs_capped_at_per_sequence_limit() {
        let mut term = Terminal::new(24, 80);

        // 32 indexed palette queries (twice the cap).
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
        // Count '=' separators to infer pair count in the single response.
        let pair_count = response.iter().filter(|b| **b == b'=').count();
        assert_eq!(
            pair_count, LEGACY_PALETTE_PER_SEQUENCE_MAX,
            "OSC 21 response must contain exactly {} pairs (cap), got {}",
            LEGACY_PALETTE_PER_SEQUENCE_MAX, pair_count,
        );
    }

    /// A bare OSC recolor with no glyph written marks full grid damage, so
    /// `damage_epoch` advances and the recolor is presented.
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

        // A bare OSC 11 (set default background) with no accompanying grid write.
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

    /// Cursor-state escapes (DECTCEM, DECSCUSR, DEC mode 12) advance
    /// `damage_epoch` but never mark the whole grid, which would force a
    /// whole-screen raster (ncurses `curs_set()` sends `\E[?12l\E[?25h`). The
    /// vacated row is `aterm-render`'s job: its cursor diff marks both rows.
    #[test]
    fn cursor_state_escapes_damage_the_cursor_cell_not_the_whole_grid() {
        let mut term = Terminal::new(24, 80);
        term.process(b"X");
        term.take_damage();

        // DECTCEM hide: scoped, and the epoch still advances.
        let e0 = term.damage_epoch();
        term.process(b"\x1b[?25l");
        assert!(term.has_damage(), "DECTCEM hide must mark damage");
        assert_eq!(term.damage_epoch(), e0 + 1, "hide advances damage_epoch");
        assert!(
            !term.grid().damage().is_full(),
            "DECTCEM hide must NOT mark the whole grid"
        );

        // A cursor move combined with a visibility change: the mark lands on the
        // new position and is still scoped. CUP is 1-indexed, so this is row 10.
        term.take_damage();
        term.process(b"\x1b[11;41H");
        term.take_damage();
        let e1 = term.damage_epoch();
        term.process(b"\x1b[?25h");
        assert_eq!(term.damage_epoch(), e1 + 1, "show advances damage_epoch");
        assert!(
            !term.grid().damage().is_full(),
            "DECTCEM show must NOT mark the whole grid after a cursor move"
        );
        assert!(
            term.grid().damage().is_row_damaged(10),
            "the NEW cursor row carries the scoped damage"
        );

        // DECSCUSR shape change (steady bar): same contract.
        term.take_damage();
        let e2 = term.damage_epoch();
        term.process(b"\x1b[6 q");
        assert_eq!(
            term.damage_epoch(),
            e2 + 1,
            "DECSCUSR advances damage_epoch"
        );
        assert!(
            !term.grid().damage().is_full(),
            "DECSCUSR must NOT mark the whole grid"
        );

        // DEC mode 12 (the other half of terminfo's `cnorm`): the cursor is a
        // steady bar, so enabling blink is a real change.
        term.take_damage();
        let e3 = term.damage_epoch();
        term.process(b"\x1b[?12h");
        assert!(
            term.has_damage(),
            "DEC mode 12 blink change must mark damage"
        );
        assert_eq!(term.damage_epoch(), e3 + 1, "mode 12 advances damage_epoch");
        assert!(
            !term.grid().damage().is_full(),
            "DEC mode 12 must NOT mark the whole grid"
        );
    }

    /// OSC 19 is stored for render snapshots, answers queries with the request
    /// terminator and is cleared by OSC 119.
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

        // Dynamic-color queries echo the request terminator (BEL here).
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

    /// OSC 21 queries distinguish concrete, known-but-undefined (empty) and
    /// unknown (`?`) fields; a fresh terminal's selection/cursor fields are empty.
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

    /// An unknown key is echoed only when it is spelled like a key: one
    /// carrying DEL, a space or punctuation, or an over-long one, gets no
    /// reply pair at all, so OSC 21 cannot type chosen text into the shell.
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

    /// A host query callback wins over a dynamic core cursor.
    #[test]
    fn osc_21_dynamic_cursor_query_honors_host_override() {
        let mut term = Terminal::new(4, 8);
        let host_cursor = aterm_types::Rgb::new(0x21, 0x43, 0x65);
        term.set_color_query_callback(move |target| {
            (target == crate::terminal::ColorTarget::Cursor).then_some(host_cursor)
        });

        term.process(b"\x1b]21;cursor=?\x1b\\");
        let expected = format!(
            "\x1b]21;cursor={}\x1b\\",
            aterm_types::ColorPalette::format_color_spec(host_cursor),
        )
        .into_bytes();
        assert_eq!(term.take_response(), Some(expected));
    }

    /// Empty OSC 21 values select dynamic behavior; bare keys reset to the
    /// host-configured baseline.
    #[test]
    fn osc_21_empty_values_are_dynamic_and_bare_keys_reset() {
        use std::sync::{Arc, Mutex};

        let mut term = Terminal::new(4, 8);
        let configured_cursor = aterm_types::Rgb::new(0x10, 0x20, 0x30);
        let configured_bg = aterm_types::Rgb::new(0x40, 0x50, 0x60);
        let configured_fg = aterm_types::Rgb::new(0x70, 0x80, 0x90);
        let mut config = crate::config::TerminalConfig::default();
        config.cursor_color = Some(configured_cursor);
        config.selection_background = Some(configured_bg);
        config.selection_foreground = Some(configured_fg);
        term.apply_config(&config);
        let events = Arc::new(Mutex::new(Vec::new()));
        let callback_events = Arc::clone(&events);
        term.set_color_change_callback(move |target, color, op| {
            callback_events
                .lock()
                .expect("callback event lock")
                .push((target, color, op));
        });

        term.process(
            b"\x1b]21;cursor=rgb:aa/bb/cc;\
              selection_background=rgb:bb/cc/dd;\
              selection_foreground=rgb:cc/dd/ee\x1b\\",
        );
        events.lock().expect("callback event lock").clear();
        term.process(
            b"\x1b]21;cursor=;selection_background=;\
              selection_foreground=\x1b\\",
        );
        assert_eq!(term.cursor_color(), None);
        assert_eq!(term.selection_background(), None);
        assert_eq!(term.selection_foreground(), None);
        {
            let events = events.lock().expect("callback event lock");
            assert_eq!(events.len(), 3);
            assert!(
                events
                    .iter()
                    .all(|(_, _, op)| *op == crate::terminal::ColorChangeOp::Dynamic),
                "empty values must remain distinguishable from configured resets"
            );
        }
        assert_eq!(
            term.cell_frame(4, 8).selection_fg,
            crate::render::COLOR_DYNAMIC,
            "the frame must preserve an explicit automatic foreground instead of delegating to a stale renderer setting",
        );

        events.lock().expect("callback event lock").clear();
        term.process(
            b"\x1b]21;cursor;selection_background;\
              selection_foreground\x1b\\",
        );
        assert_eq!(term.cursor_color(), Some(configured_cursor));
        assert_eq!(term.selection_background(), Some(configured_bg));
        assert_eq!(term.selection_foreground(), Some(configured_fg));
        let events = events.lock().expect("callback event lock");
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|(_, _, op)| *op == crate::terminal::ColorChangeOp::Reset),
            "bare keys must report configured Reset rather than Dynamic"
        );
    }

    /// A bare numeric key is an indexed-color reset, not a SET: like OSC 104 it
    /// works while palette reconfiguration is blocked.
    #[test]
    fn osc_21_bare_numeric_key_resets_without_set_permission() {
        let mut term = Terminal::new(4, 8);
        let default = term.palette_color_components(42);

        // Seed through host config, not the wire SET gate.
        let mut palette = aterm_types::ColorPalette::new();
        palette.set(42, aterm_types::Rgb::new(0xaa, 0xbb, 0xcc));
        let mut config = crate::config::TerminalConfig::default();
        config.custom_palette = Some(palette);
        term.apply_config(&config);
        assert_eq!(term.palette_color_components(42), (0xaa, 0xbb, 0xcc));

        // With a configured palette, bare reset returns to that host baseline.
        term.modes_mut().allow_palette_reconfigure = true;
        term.process(b"\x1b]21;42=rgb:01/02/03\x1b\\");
        assert_eq!(term.palette_color_components(42), (0x01, 0x02, 0x03));
        term.modes_mut().allow_palette_reconfigure = false;
        term.process(b"\x1b]21;42\x1b\\");
        assert_eq!(term.palette_color_components(42), (0xaa, 0xbb, 0xcc));

        // Removing the host palette makes the next bare reset return to the
        // built-in value, still without granting SET permission.
        config.custom_palette = None;
        term.apply_config(&config);
        term.modes_mut().allow_palette_reconfigure = true;
        term.process(b"\x1b]21;42=rgb:04/05/06\x1b\\");
        assert_eq!(term.palette_color_components(42), (0x04, 0x05, 0x06));
        term.modes_mut().allow_palette_reconfigure = false;
        term.process(b"\x1b]21;42\x1b\\");
        assert_eq!(term.palette_color_components(42), default);
    }

    /// Color-stack pop reports a saved dynamic cursor as Dynamic, not
    /// Set(foreground), which would freeze a host renderer on that foreground.
    #[test]
    fn color_stack_pop_reports_dynamic_cursor_as_dynamic() {
        use std::sync::{Arc, Mutex};

        let mut term = Terminal::new(4, 8);
        let events = Arc::new(Mutex::new(Vec::new()));
        let callback_events = Arc::clone(&events);
        term.set_color_change_callback(move |target, color, op| {
            callback_events
                .lock()
                .expect("callback event lock")
                .push((target, color, op));
        });

        term.process(
            b"\x1b]30001\x1b\\\
              \x1b]12;rgb:aa/bb/cc\x1b\\",
        );
        events.lock().expect("callback event lock").clear();
        term.process(b"\x1b]30101\x1b\\");

        assert_eq!(term.cursor_color(), None);
        let events = events.lock().expect("callback event lock");
        assert!(
            events.iter().any(|&(target, color, op)| {
                target == crate::terminal::ColorTarget::Cursor
                    && color == term.default_foreground()
                    && op == crate::terminal::ColorChangeOp::Dynamic
            }),
            "restoring a saved dynamic cursor must preserve Dynamic for callback consumers",
        );
    }

    /// A color-stack round trip restores both selection colors: restoring only
    /// the background leaves selected glyph ink on a post-push value.
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

    /// Both selection-color resets follow their configured theme baselines (the
    /// OSC 110/111/112/104 convention).
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
