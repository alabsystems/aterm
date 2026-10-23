// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal mode flags shared across aterm crates.

use crate::mouse::{MouseEncoding, MouseMode};
use crate::{Appearance, BiDiMode, CursorStyle, ParagraphDirection, VtLevel};

/// Terminal mode flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "terminal modes are naturally boolean flags"
)]
pub struct TerminalModes {
    /// Cursor visible (DECTCEM).
    pub cursor_visible: bool,
    /// Cursor style (DECSCUSR).
    pub cursor_style: CursorStyle,
    /// Application cursor keys (DECCKM).
    pub application_cursor_keys: bool,
    /// Alternate screen buffer active.
    pub alternate_screen: bool,
    /// Auto-wrap mode (DECAWM).
    pub auto_wrap: bool,
    /// Origin mode (DECOM).
    pub origin_mode: bool,
    /// Insert mode (IRM, mode 4).
    pub insert_mode: bool,
    /// Line feed/new line mode (LNM, mode 20).
    pub new_line_mode: bool,
    /// Bracketed paste mode.
    pub bracketed_paste: bool,
    /// Allow OSC 52 clipboard queries (`Pd = "?"`).
    pub allow_osc52_query: bool,
    /// Allow OSC 52 clipboard write operations from programs.
    pub allow_osc52_set: bool,
    /// Mouse tracking mode (1000/1002/1003).
    pub mouse_mode: MouseMode,
    /// Mouse coordinate encoding (1006 for SGR).
    pub mouse_encoding: MouseEncoding,
    /// Focus reporting mode (1004).
    pub focus_reporting: bool,
    /// Synchronized output mode (2026).
    pub synchronized_output: bool,
    /// Reverse video mode (DECSET 5).
    pub reverse_video: bool,
    /// Cursor blink mode (DECSET 12).
    pub cursor_blink: bool,
    /// Application keypad mode (DECKPAM/DECKPNM).
    pub application_keypad: bool,
    /// 132-column mode (DECSET 3).
    pub column_mode_132: bool,
    /// Reverse wraparound mode (DECSET 45).
    pub reverse_wraparound: bool,
    /// Left/right margin mode (DECLRMM, DECSET 69).
    pub left_right_margin_mode: bool,
    /// VT52 compatibility mode (DECANM mode 2).
    pub vt52_mode: bool,
    /// Grapheme cluster mode (mode 2027).
    pub grapheme_cluster_mode: bool,
    /// VT conformance level (DECSCL).
    pub vt_level: VtLevel,
    /// Colour scheme last announced to the program.
    pub color_scheme: Appearance,
    /// DEC private mode 2031: emit unsolicited DSR 997N on palette flips.
    pub report_color_scheme: bool,
    /// DECBKM (DEC private mode 67): Backspace sends BS (`0x08`) instead of DEL (`0x7f`).
    pub backarrow_sends_bs: bool,
    /// xterm `numLock` / special modifiers (DEC private mode 1035).
    pub special_modifiers: bool,
    /// xterm `metaSendsEscape` (DEC private mode 1036).
    pub meta_send_escape: bool,
    /// xterm `altSendsEscape` (DEC private mode 1039).
    pub alt_send_escape: bool,
    /// DEC private mode 1045: tracked-only xterm private mode.
    pub mode_1045: bool,
    /// ConPTY win32-input-mode (DEC private mode 9001).
    #[cfg_attr(feature = "serde", serde(default))]
    pub win32_input_mode: bool,
    /// Host capability: session sits behind a Windows console host.
    #[cfg_attr(feature = "serde", serde(default))]
    pub win32_input_enabled: bool,
    /// Host capability: console host behind [`Self::win32_input_enabled`] is OpenConsole.
    #[cfg_attr(feature = "serde", serde(default))]
    pub win32_input_openconsole: bool,
    /// DEC private mode 2048: in-band size reports.
    pub in_band_size_reports: bool,
    /// BiDi mode.
    pub bidi_mode: BiDiMode,
    /// BiDi paragraph direction (SCP - `CSI n SPACE k`).
    pub bidi_direction: ParagraphDirection,
    /// Box drawing character mirroring (DEC mode ?2500).
    pub bidi_box_mirroring: bool,
    /// BiDi autodetection (DEC mode ?2501).
    pub bidi_autodetection: bool,
    /// Alternate scroll mode (DECSET 1007).
    pub alternate_scroll: bool,
    /// Keyboard arrow swap in RTL context (DEC mode ?1243).
    pub bidi_arrow_swap: bool,
    /// Sixel Display Mode (DECSDM, DEC private mode 80).
    pub sixel_display_mode: bool,
    /// DECSACE stream attribute extent (`CSI Ps * x`).
    pub stream_attribute_extent: bool,
    /// Allow `CSI t` window manipulation operations.
    pub allow_window_ops: bool,
    /// Allow desktop notification OSC sequences.
    pub allow_notifications: bool,
    /// Retired OSC 133 / OSC 633 `SessionMemory` recording gate.
    pub allow_session_memory: bool,
    /// Allow OSC 4 / OSC 21 indexed palette set operations.
    pub allow_palette_reconfigure: bool,
    /// Require OSC 133 / OSC 633 shell-integration sequences to carry a session nonce.
    pub require_shell_integration_nonce: bool,
    /// Kitty keyboard protocol capability toggle.
    pub kitty_keyboard_enabled: bool,
    /// Enable 80/132 column switching (DEC private mode 40).
    pub deccolm_enable: bool,
    /// No Clearing Screen on Column Change (DECNCSM, DEC private mode 95).
    pub decncsm: bool,
    /// Ambiguous-width characters treated as double-width (CJK mode).
    pub ambiguous_width_double: bool,
}

impl TerminalModes {
    /// Create default terminal modes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cursor_visible: true,
            cursor_blink: true,
            auto_wrap: true,
            bidi_arrow_swap: true,
            allow_osc52_set: false,
            allow_notifications: false,
            stream_attribute_extent: true,
            special_modifiers: true,
            alt_send_escape: true,
            kitty_keyboard_enabled: true,
            ..Default::default()
        }
    }

    /// Whether DECCKM application cursor mode is enabled.
    #[must_use]
    #[inline]
    pub const fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys
    }

    /// Whether auto-wrap mode is enabled.
    #[must_use]
    #[inline]
    pub const fn auto_wrap(&self) -> bool {
        self.auto_wrap
    }

    /// Whether bracketed paste mode is enabled.
    #[must_use]
    #[inline]
    pub const fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    /// Whether win32-input-mode is both enabled by the host and requested by the program.
    #[must_use]
    #[inline]
    pub const fn win32_input_active(&self) -> bool {
        self.win32_input_enabled && self.win32_input_mode
    }

    /// Whether synchronized output mode is enabled.
    #[must_use]
    #[inline]
    pub const fn synchronized_output(&self) -> bool {
        self.synchronized_output
    }

    /// Whether reverse video mode is enabled.
    #[must_use]
    #[inline]
    pub const fn reverse_video(&self) -> bool {
        self.reverse_video
    }

    /// Whether alternate scroll mode (DECSET 1007) is enabled.
    #[must_use]
    #[inline]
    pub const fn alternate_scroll(&self) -> bool {
        self.alternate_scroll
    }
}
