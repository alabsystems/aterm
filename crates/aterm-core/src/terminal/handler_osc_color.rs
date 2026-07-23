// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC color handlers for the terminal.
//!
//! This module contains handlers for color-related OSC sequences:
//! - OSC 4: Color palette manipulation
//! - OSC 10/11/12: Default foreground/background/cursor colors
//! - OSC 19: Selection foreground color callback
//! - OSC 21: Extended color queries (kitty protocol)
//! - OSC 104: Reset indexed colors
//! - OSC 30001/30101: Kitty color stack push/pop

use super::handler::TerminalHandler;
use aterm_types::{ColorPalette, Rgb};

/// Fallback per-sequence response cap for OSC palette queries (#7883) used
/// when no [`PolicyEngine`][pe] is installed.
///
/// A single OSC 4 / OSC 21 sequence can embed many `;idx;?` pairs (up to 256
/// for OSC 4). Without a per-sequence bound, one sequence can emit ~5 KiB of
/// back-pressure on the PTY writer — a cheap amplification vector. The
/// response rate limiter bounds *total* output rate, but only a per-sequence
/// cap bounds single-sequence amplification.
///
/// When this cap is hit the remaining query pairs are silently dropped,
/// matching the rate-limiter drop policy. 16 is generous enough for any
/// legitimate palette inspector (debuggers typically query <= 16 well-known
/// indices) while bounding a malicious 256-index query to ~320 bytes of
/// response bytes.
///
/// When a policy engine is installed (#7995), the cap and cross-sequence
/// throttle are sourced from `engine.rate_limit_config("palette")`. This
/// constant is the fallback used by legacy hosts that have not installed a
/// policy.
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

impl TerminalHandler<'_> {
    /// Resolve the per-sequence OSC palette-query response cap (#7995).
    ///
    /// Returns the `per_sequence_max` from the active policy engine's
    /// `"palette"` rate-limit entry when one is installed, falling back to
    /// [`LEGACY_PALETTE_PER_SEQUENCE_MAX`] otherwise. A policy entry with
    /// `per_sequence_max = 0` (meaning "disabled" in the policy schema) also
    /// falls back to the legacy constant so a handler never receives a
    /// non-cap; the fail-closed policy posture applies to response gating,
    /// not to the per-sequence amplification bound.
    pub(super) fn palette_per_sequence_cap(&self) -> usize {
        if let Some(engine) = self.policy_engine.as_ref()
            && let Some(cfg) = engine.rate_limit_config("palette")
            && cfg.per_sequence_max > 0
        {
            return cfg.per_sequence_max as usize;
        }
        LEGACY_PALETTE_PER_SEQUENCE_MAX
    }

    /// Debit the active policy engine's `"palette"` bucket by one response
    /// pair (#7995). Returns `true` when the response is permitted.
    ///
    /// When no policy engine is installed the per-sequence cap enforced by
    /// [`Self::palette_per_sequence_cap`] is the only bound; this helper
    /// returns `true` so the legacy behavior (no cross-sequence throttle)
    /// is preserved.
    fn palette_rate_limit_consume_one(&mut self) -> bool {
        if let Some(engine) = self.policy_engine.as_mut() {
            let clock = aterm_policy::limits::SystemClock;
            engine.rate_limit_try_consume("palette", 1, &clock)
        } else {
            true
        }
    }

    /// Handle OSC 4 - color palette manipulation.
    ///
    /// Format: `OSC 4 ; index ; spec ST`
    /// - To set: `OSC 4 ; index ; color-spec ST` (e.g., `OSC 4 ; 1 ; rgb:ff/00/00 ST`)
    /// - To query: `OSC 4 ; index ; ? ST` (terminal responds with current color)
    pub(super) fn handle_osc_4(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.len() < 3 {
            return; // Need at least: 4, index, spec
        }

        // Process pairs: index, spec, index, spec, ...
        //
        // Per-sequence response cap (#7883, now sourced from the
        // `"palette"` policy entry in #7995): track query responses emitted
        // from THIS sequence so a 256-index `;idx;?` query cannot amplify
        // into ~5 KiB of PTY back-pressure. Set operations are unaffected.
        // The cap comes from `palette_per_sequence_cap()` — the active
        // policy engine's bucket config, or the legacy constant when no
        // engine is installed.
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
                // Per-sequence cap: silently drop queries past the limit
                // (#7883). Continue scanning so later set operations in the
                // same sequence still apply.
                if responses_emitted >= per_seq_cap {
                    i += 2;
                    continue;
                }
                // Cross-sequence rate limit: consult the policy engine's
                // `"palette"` bucket (#7995). A denial here silently drops
                // this pair — same contract as the per-sequence cap.
                if !self.palette_rate_limit_consume_one() {
                    i += 2;
                    continue;
                }
                // Query: respond with current color.
                // Match request terminator (BEL vs ST) per xterm behavior (#7548).
                let color = self.color.palette.get(index);
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
                // Set: parse the color spec and update palette.
                //
                // #7937 F01-3: SET is gated by `modes.allow_palette_reconfigure`
                // (fail-closed). Hosts that ship a themeable palette opt in
                // explicitly; by default a program cannot recolor the
                // 256-entry index over the wire. Query branch above remains
                // ungated — queries only echo what the terminal reports.
                if self.modes.allow_palette_reconfigure
                    && let Some(color) = ColorPalette::parse_color_spec(spec)
                {
                    self.color.palette.set(index, color);
                    self.fire_color_change_callback(
                        super::ColorTarget::Palette,
                        color,
                        super::ColorChangeOp::Set,
                    );
                }
                // Invalid color specs (or set-while-disabled) are silently
                // ignored — no PTY response is emitted for set ops anyway.
            }

            i += 2;
        }
    }

    /// Handle OSC 10/11/12 - foreground, background, and cursor colors.
    ///
    /// These OSC sequences support cascading: when OSC 10 is received with multiple
    /// color specifications, the first sets foreground, second sets background,
    /// third sets cursor. OSC 11 with multiple specs sets background and cursor.
    ///
    /// - `start_at = 0`: Start with foreground (OSC 10)
    /// - `start_at = 1`: Start with background (OSC 11)
    /// - `start_at = 2`: Start with cursor (OSC 12)
    ///
    /// Query format: `OSC N ; ? ST` responds with `OSC N ; rgb:RRRR/GGGG/BBBB ST`
    pub(super) fn handle_osc_10_11_12(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
        start_at: usize,
    ) {
        if params.len() < 2 {
            return;
        }

        // Process color specifications starting at the given index
        // params[0] is the OSC code, params[1..] are the color specs
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
                // Match request terminator (BEL vs ST) per xterm behavior (#7548).
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

    /// Handle OSC 19 - selection foreground color.
    ///
    /// aterm-core does not yet persist selection foreground as part of the
    /// terminal theme model, but the host callback surface needs to observe the
    /// color value for aTerm.app's callback migration.
    pub(super) fn handle_osc_19(&mut self, params: &[&[u8]]) {
        let Some(spec_bytes) = params.get(1) else {
            return;
        };
        let Ok(spec) = std::str::from_utf8(spec_bytes) else {
            return;
        };
        if spec == "?" {
            return;
        }
        if let Some(color) = ColorPalette::parse_color_spec(spec) {
            self.fire_color_change_callback(
                super::ColorTarget::SelectionForeground,
                color,
                super::ColorChangeOp::Set,
            );
        }
    }

    /// Handle OSC 21 - Extended color queries (kitty protocol).
    ///
    /// Format: `OSC 21 ; key=value ; key=value ST`
    ///
    /// Keys can be named colors (`foreground`, `background`, `cursor`,
    /// `selection_background`) or indexed palette entries (`0-255`).
    ///
    /// Query format: `key=?` responds with `key=rgb:RRRR/GGGG/BBBB`.
    pub(super) fn handle_osc_21(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.len() < 2 {
            return;
        }

        // Per-sequence response-pair cap (#7883, sourced from policy in #7995):
        // bound response size so a 256-pair query cannot amplify into ~5 KiB
        // of PTY back-pressure. See `palette_per_sequence_cap` for the
        // policy/legacy fallback.
        let per_seq_cap = self.palette_per_sequence_cap();
        let mut query_pairs: Vec<String> = Vec::new();

        for param in &params[1..] {
            let Ok(pair) = std::str::from_utf8(param) else {
                continue;
            };
            let Some((raw_key, raw_value)) = pair.split_once('=') else {
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
                // Cross-sequence rate limit via the policy engine's
                // `"palette"` bucket (#7995). A denial silently drops this
                // pair — same contract as the per-sequence cap.
                if !self.palette_rate_limit_consume_one() {
                    continue;
                }
                if let Some(color) = self.osc_21_query_color(key) {
                    let spec = ColorPalette::format_color_spec(color);
                    query_pairs.push(format!("{key}={spec}"));
                }
                continue;
            }

            if let Some(color) = ColorPalette::parse_color_spec(value) {
                self.osc_21_set_color(key, color);
            }
        }

        if !query_pairs.is_empty() {
            // Match request terminator (BEL vs ST) per xterm behavior (#7548).
            let st = if self.transient.last_osc_bel_terminated {
                "\x07"
            } else {
                "\x1b\\"
            };
            let response = format!("\x1b]21;{}{}", query_pairs.join(";"), st);
            self.send_response(cap, response.as_bytes());
        }
    }

    fn osc_21_query_color(&mut self, key: &str) -> Option<Rgb> {
        if let Some(slot) = DynamicColorSlot::from_name(key) {
            // Consult host query_callback for dynamic colors, matching
            // OSC 10/11/12 behavior. The host may override the reported
            // color (e.g., when the UI layer uses a different theme).
            let palette_color = self.get_dynamic_color(slot);
            let color = self
                .color
                .query_callback
                .as_mut()
                .and_then(|cb| cb(slot.color_target()))
                .unwrap_or(palette_color);
            return Some(color);
        }
        if key.eq_ignore_ascii_case("selection_background") {
            return self.color.selection_background;
        }
        let index = key.parse::<u8>().ok()?;
        Some(self.color.palette.get(index))
    }

    fn osc_21_set_color(&mut self, key: &str, color: Rgb) {
        if let Some(slot) = DynamicColorSlot::from_name(key) {
            // Named dynamic-color slots (foreground, background, cursor)
            // are NOT gated — they route through the host's
            // set_dynamic_color callback, not the indexed palette.
            self.set_dynamic_color(slot, color);
            return;
        }
        if key.eq_ignore_ascii_case("selection_background") {
            // Selection background is tracked separately from the 256 palette
            // indices and is part of the theme-surface, not the indexed
            // palette. Keep it ungated for parity with dynamic colors.
            self.color.selection_background = Some(color);
            self.fire_color_change_callback(
                super::ColorTarget::SelectionBackground,
                color,
                super::ColorChangeOp::Set,
            );
            return;
        }
        if let Ok(index) = key.parse::<u8>() {
            // Numeric index: this is a reconfigure of a palette slot.
            // #7937 F01-3: gated by allow_palette_reconfigure (fail-closed).
            if !self.modes.allow_palette_reconfigure {
                return;
            }
            self.color.palette.set(index, color);
            self.fire_color_change_callback(
                super::ColorTarget::Palette,
                color,
                super::ColorChangeOp::Set,
            );
        }
    }

    /// Handle OSC 104 - Reset indexed color(s) to theme-configured defaults.
    ///
    /// Format: `OSC 104 [; index [; index ...]] ST`
    ///
    /// If no indices are specified, reset all 256 colors.
    /// If indices are specified, reset only those colors.
    ///
    /// When a theme-configured palette exists (set via `apply_config`),
    /// colors reset to those theme values rather than hardcoded xterm
    /// defaults. This matches the OSC 110/111 pattern where foreground
    /// and background reset to `configured_foreground`/`configured_background`.
    pub(super) fn handle_osc_104(&mut self, params: &[&[u8]]) {
        if params.len() <= 1 {
            // No indices - reset all colors to theme-configured palette
            // (or xterm defaults if no theme palette is configured).
            if let Some(ref configured) = self.color.configured_palette {
                self.color.palette = configured.clone();
            } else {
                self.color.palette.reset();
            }
        } else {
            // Reset specific colors — only fire callback if at least one was valid.
            let mut any_reset = false;
            for param in &params[1..] {
                if let Ok(index_str) = std::str::from_utf8(param) {
                    if let Ok(index) = index_str.parse::<u8>() {
                        // Reset to theme-configured color if available,
                        // otherwise to xterm default.
                        if let Some(ref configured) = self.color.configured_palette {
                            self.color.palette.set(index, configured.get(index));
                        } else {
                            self.color.palette.reset_color(index);
                        }
                        any_reset = true;
                    }
                }
            }
            if !any_reset {
                return;
            }
        }
        // Notify UI that palette colors changed.
        self.fire_color_change_callback(
            super::ColorTarget::Palette,
            Rgb { r: 0, g: 0, b: 0 },
            super::ColorChangeOp::Reset,
        );
    }

    /// Handle OSC 30001 - Push color stack (Kitty protocol).
    ///
    /// Pushes the current color state onto the stack. The stack has a
    /// maximum depth of 16 entries; if full, the oldest entry is discarded.
    ///
    /// Per the Kitty protocol, all dynamic colors are saved: the 256-color
    /// palette, default foreground/background, cursor color, and selection
    /// background.
    pub(super) fn handle_osc_30001(&mut self) {
        use super::callbacks::COLOR_STACK_MAX_DEPTH;
        use super::grouped_state::ColorStackEntry;

        // If stack is at max depth, evict oldest entry (O(1) with VecDeque)
        if self.color.stack.len() >= COLOR_STACK_MAX_DEPTH {
            self.color.stack.pop_front();
        }

        // Push full color state onto stack
        self.color.stack.push_back(ColorStackEntry {
            palette: self.color.palette.clone(),
            default_foreground: self.color.default_foreground,
            default_background: self.color.default_background,
            cursor_color: self.color.cursor_color,
            selection_background: self.color.selection_background,
        });
    }

    /// Handle OSC 30101 - Pop color stack (Kitty protocol).
    ///
    /// Pops the most recently pushed color state from the stack and
    /// restores all dynamic colors: palette, default foreground/background,
    /// cursor color, and selection background. If the stack is empty,
    /// does nothing (no-op).
    pub(super) fn handle_osc_30101(&mut self) {
        if let Some(entry) = self.color.stack.pop_back() {
            self.color.palette = entry.palette;
            self.color.default_foreground = entry.default_foreground;
            self.color.default_background = entry.default_background;
            self.color.cursor_color = entry.cursor_color;
            self.color.selection_background = entry.selection_background;

            // Notify UI that palette was bulk-restored from stack.
            self.fire_color_change_callback(
                super::ColorTarget::Palette,
                Rgb { r: 0, g: 0, b: 0 },
                super::ColorChangeOp::Set,
            );
            // Notify UI that dynamic colors were restored.
            self.fire_color_change_callback(
                super::ColorTarget::Foreground,
                entry.default_foreground,
                super::ColorChangeOp::Set,
            );
            self.fire_color_change_callback(
                super::ColorTarget::Background,
                entry.default_background,
                super::ColorChangeOp::Set,
            );
            // Notify UI that cursor color was restored (#7469).
            // Without this, the cursor keeps the pre-pop color until the next
            // explicit OSC 12, because the UI layer never learns the value changed.
            let cursor_color = entry.cursor_color.unwrap_or(entry.default_foreground);
            self.fire_color_change_callback(
                super::ColorTarget::Cursor,
                cursor_color,
                super::ColorChangeOp::Set,
            );
            // Notify UI that selection background was restored (#7469).
            // When the popped entry has None for selection_background, fire a
            // Reset so the UI stops using a stale custom color.
            if let Some(sel_bg) = entry.selection_background {
                self.fire_color_change_callback(
                    super::ColorTarget::SelectionBackground,
                    sel_bg,
                    super::ColorChangeOp::Set,
                );
            } else {
                self.fire_color_change_callback(
                    super::ColorTarget::SelectionBackground,
                    Rgb { r: 0, g: 0, b: 0 },
                    super::ColorChangeOp::Reset,
                );
            }
        }
    }

    /// Handle OSC 17 — highlight (selection) background color.
    ///
    /// Set format: `OSC 17 ; color-spec ST`
    /// Query format: `OSC 17 ; ? ST` — responds with current selection background.
    ///
    /// Part of the xterm dynamic color family (OSC 10-19). Used by
    /// applications like vim and tmux to customize selection appearance.
    /// (#7555)
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
            // Query: respond with current selection background.
            // Fall back to the default background if no selection color is set.
            let color = self
                .color
                .selection_background
                .unwrap_or(self.color.default_background);
            // Match request terminator (BEL vs ST) per xterm behavior (#7548).
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

    /// Reset the selection (highlight) background color (OSC 117).
    ///
    /// Clears the custom selection background, reverting to the terminal's
    /// default selection appearance. (#7555)
    pub(super) fn reset_selection_background(&mut self) {
        self.color.selection_background = None;
        self.fire_color_change_callback(
            super::ColorTarget::SelectionBackground,
            Rgb { r: 0, g: 0, b: 0 },
            super::ColorChangeOp::Reset,
        );
    }

    /// Reset the custom selection foreground color (OSC 119).
    ///
    /// Clears the custom selection foreground, reverting to the terminal's
    /// default selection appearance.
    pub(super) fn reset_selection_foreground(&mut self) {
        self.fire_color_change_callback(
            super::ColorTarget::SelectionForeground,
            Rgb { r: 0, g: 0, b: 0 },
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

    /// Reset a dynamic color to its theme-configured default and fire the
    /// change callback.
    ///
    /// Uses `configured_foreground`/`configured_background` (set by
    /// `apply_config`) instead of hardcoded constants, so OSC 110/111/112
    /// resets honor the active theme (#7443).
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
                self.color.cursor_color = None;
                self.color.default_foreground
            }
        };
        self.fire_color_change_callback(slot.color_target(), color, super::ColorChangeOp::Reset);
    }

    /// Fire the color change callback if one is registered.
    pub(super) fn fire_color_change_callback(
        &mut self,
        target: super::ColorTarget,
        color: Rgb,
        op: super::ColorChangeOp,
    ) {
        // Any color SET/RESET (default fg/bg/cursor, an ANSI palette index, the
        // selection bg, an OSC 104 reset) recolors already-painted cells WITHOUT
        // changing their content — so mark full grid damage, exactly like DECSCNM
        // (`set_reverse_video`). This advances `damage_epoch`, so a recolor that
        // arrives with NO accompanying grid write still bumps the frontend's redraw
        // key and is presented (otherwise the D-1 early-out swallows it and the
        // screen keeps the old colors until the next grid write). This helper is
        // called ONLY on real mutations — color QUERIES emit a PTY response and never
        // reach here — so it never over-damages on a query.
        self.grid.damage_mut().mark_full();
        if let Some(ref mut cb) = self.color.change_callback {
            cb(target, color, op);
        }
    }
}

#[cfg(test)]
mod osc_color_response_cap_tests {
    //! Regression tests for the per-sequence response cap on OSC palette
    //! queries (#7883). A single crafted sequence like
    //! `ESC]4;0;?;1;?;...;255;?ST` could trigger one response write per
    //! query pair — at 256 queries that is ~5 KiB of PTY back-pressure.
    //! The cap bounds handler-level single-sequence amplification while
    //! leaving legitimate small batches untouched.
    //!
    //! Note: the VT parser itself already truncates OSC sequences at 16
    //! params (`MAX_OSC_PARAMS`, see `aterm-parser/src/lib.rs`), so the
    //! worst end-to-end input via `Terminal::process` is ~7 query pairs
    //! per sequence. These tests therefore exercise the handler directly
    //! via `split_for_process` so they can feed >16 params and exercise
    //! the cap. This models defense-in-depth if the parser's `MAX_OSC_PARAMS`
    //! is ever raised, or if a future OSC carrier delivers more params.
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

    /// Build params slice-of-slices for OSC 4 with `n` query pairs.
    /// Returns a pair of owning Vec<String> (backing storage) so callers
    /// can borrow as `&[&[u8]]` without lifetime issues.
    fn build_osc_4_query_params(n: usize) -> Vec<Vec<u8>> {
        let mut params: Vec<Vec<u8>> = Vec::with_capacity(1 + n * 2);
        params.push(b"4".to_vec());
        for idx in 0..n {
            params.push(idx.to_string().into_bytes());
            params.push(b"?".to_vec());
        }
        params
    }

    /// A 256-pair OSC 4 query must emit at most `LEGACY_PALETTE_PER_SEQUENCE_MAX`
    /// response frames; the remainder are silently dropped (#7883).
    ///
    /// Bypasses the parser to feed 256 params directly to `handle_osc_4`,
    /// which is the handler-level defense being tested.
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
            "256-pair OSC 4 query must emit exactly {} response frames, got {} (#7883). \
             Without the cap this would be 256.",
            LEGACY_PALETTE_PER_SEQUENCE_MAX, frames,
        );
    }

    /// An 8-pair OSC 4 query (below the cap) must emit all 8 responses.
    /// Guards against an over-broad cap that would break legitimate batches.
    /// Uses direct handler invocation for consistency with the 256-pair test.
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
             frames, got {} (#7883)",
            LEGACY_PALETTE_PER_SEQUENCE_MAX, frames,
        );
    }

    /// A single-query OSC 4 via the real parser must emit exactly one
    /// response frame. Sanity check that the cap scaffolding does not
    /// regress the common case end-to-end.
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

    /// Mixed set+query OSC 4 sequence: once the query cap is hit, remaining
    /// queries drop silently but later SET operations in the same sequence
    /// must still apply. Guards against an over-broad cap that would break
    /// palette mutation batched with queries (#7883).
    #[test]
    fn osc_4_set_operations_unaffected_by_query_cap() {
        let mut term = Terminal::new(24, 80);
        // #7937 F01-3: OSC 4 SET is fail-closed by default. This test
        // exercises the #7883 query-cap/SET interaction, so we must opt in.
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
             operations (#7883)",
            LEGACY_PALETTE_PER_SEQUENCE_MAX,
        );
        // Verify the SET at the end still applied.
        let (r, g, b) = term.palette_color_components(200);
        assert_eq!(
            (r, g, b),
            (0xff, 0x00, 0x00),
            "SET operation after the query cap must still apply (#7883)",
        );
    }

    /// OSC 21 with > LEGACY_PALETTE_PER_SEQUENCE_MAX query pairs must bound the
    /// number of response pairs in the single reply frame (#7883). OSC 21
    /// emits one response *write*, but the size grows with the number of
    /// pairs — the cap bounds that size. Invoked via the handler directly
    /// to exceed the parser's 16-param OSC truncation.
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
            "OSC 21 response must contain exactly {} pairs (cap), got {} (#7883)",
            LEGACY_PALETTE_PER_SEQUENCE_MAX, pair_count,
        );
    }

    /// REGRESSION (integration audit): a bare OSC recolor with NO glyph written
    /// must mark grid damage so `damage_epoch` advances — otherwise the frontend's
    /// D-1 redraw early-out (keyed on `damage_epoch`) swallows the recolor and the
    /// screen keeps the old colors until the next grid write. A color SET/RESET
    /// recolors already-painted cells, so it is full-screen damage (like DECSCNM).
    #[test]
    fn osc_recolor_with_no_glyph_marks_damage_and_advances_epoch() {
        let mut term = Terminal::new(24, 80);
        // Paint a glyph, then consume the damage (as a real present does).
        term.process(b"X");
        term.take_damage();
        let epoch0 = term.damage_epoch();
        assert!(
            !term.has_damage(),
            "damage consumed by the simulated present"
        );

        // A bare OSC 11 (set default background) with NO accompanying grid write.
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

    /// REGRESSION (integration audit): sibling of the recolor swallow — a bare
    /// cursor visibility (DECTCEM) or shape (DECSCUSR) change with NO grid write must
    /// mark grid damage so `damage_epoch` advances; otherwise the frontend's D-1
    /// redraw early-out (whose key carries no engine cursor-visible/shape term)
    /// swallows it and the cursor's appearance is stale until the next grid write.
    #[test]
    fn cursor_visibility_and_shape_changes_mark_damage() {
        let mut term = Terminal::new(24, 80);
        term.process(b"X");
        term.take_damage();
        let e0 = term.damage_epoch();
        assert!(!term.has_damage());

        // DECTCEM hide (CSI ?25l) — cursor was visible, no glyph written.
        term.process(b"\x1b[?25l");
        assert!(term.has_damage(), "DECTCEM hide must mark grid damage");
        assert_eq!(term.damage_epoch(), e0 + 1, "hide advances damage_epoch");

        // DECSCUSR: set steady block, consume, then steady bar — a real shape change.
        term.process(b"\x1b[2 q");
        term.take_damage();
        let e1 = term.damage_epoch();
        term.process(b"\x1b[6 q");
        assert!(
            term.has_damage(),
            "DECSCUSR shape change must mark grid damage"
        );
        assert_eq!(
            term.damage_epoch(),
            e1 + 1,
            "shape change advances damage_epoch"
        );
    }
}
