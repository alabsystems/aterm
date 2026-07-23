// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal mode flags shared across aterm crates.
//!
//! Extracted from `aterm-core::terminal::types::modes` to keep the terminal
//! module focused on runtime state while preserving the canonical mode
//! contract for checkpointing, bridge integration, and API consumers
//! (Part of #5663, #2341).

use crate::mouse::{MouseEncoding, MouseMode};
use crate::{Appearance, BiDiMode, CursorStyle, ParagraphDirection, VtLevel};

/// Terminal mode flags.
//
// `PartialEq, Eq` enable by-value comparison in `TerminalCheckpoint` (B.3.2):
// every field is itself `PartialEq, Eq` (bools + Copy enums), so the derive is
// exact and adds no new invariants.
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
    /// When set, LF also performs CR.
    pub new_line_mode: bool,
    /// Bracketed paste mode.
    pub bracketed_paste: bool,
    /// Allow OSC 52 clipboard queries (Pd = "?").
    ///
    /// When disabled, aterm-core ignores clipboard query requests and does not
    /// invoke the clipboard callback or emit a response.
    pub allow_osc52_query: bool,
    /// Allow OSC 52 clipboard write (set) operations from programs (#7782).
    ///
    /// Gates the `OSC 52 ; <selection> ; <base64> ST` set path. When disabled
    /// (default), PTY-origin clipboard writes are silently dropped before the
    /// clipboard callback is invoked — attacker-controlled bytes never reach
    /// the host clipboard delegate.
    ///
    /// **Default: `false` (fail-closed, #7782).** Hosts that want programs to
    /// write to the system clipboard opt in explicitly via
    /// [`authorize_clipboard_access(ClipboardAccess::Write)`](super::super::Terminal::authorize_clipboard_access)
    /// (or the FFI `aterm_terminal_set_allow_osc52_set_v2`) after wiring a
    /// clipboard callback. This matches the "host policy bit" posture used
    /// for `allow_window_ops`, `allow_osc52_query`, `allow_notifications`,
    /// and `allow_palette_reconfigure`: wiring the callback is one signal,
    /// but a second explicit policy call is required before parser-origin
    /// OSC 52 set sequences can reach the callback.
    pub allow_osc52_set: bool,
    /// Mouse tracking mode (1000/1002/1003).
    pub mouse_mode: MouseMode,
    /// Mouse coordinate encoding (1006 for SGR).
    pub mouse_encoding: MouseEncoding,
    /// Focus reporting mode (1004).
    /// When enabled, terminal sends CSI I on focus and CSI O on blur.
    pub focus_reporting: bool,
    /// Synchronized output mode (2026).
    /// When enabled, rendering is deferred until mode is reset.
    /// This prevents screen tearing during rapid updates.
    pub synchronized_output: bool,
    /// Reverse video mode (DECSET 5).
    /// When enabled, screen colors are inverted.
    pub reverse_video: bool,
    /// Cursor blink mode (DECSET 12).
    /// When enabled, cursor blinks.
    pub cursor_blink: bool,
    /// Application keypad mode (DECKPAM/DECKPNM).
    /// When enabled, keypad sends application sequences instead of numeric keys.
    /// Set by ESC =, reset by ESC >.
    pub application_keypad: bool,
    /// 132 column mode (DECSET 3).
    /// When enabled, terminal uses 132 columns; when disabled, 80 columns.
    /// Note: aterm-core tracks this flag but doesn't actually resize the terminal.
    pub column_mode_132: bool,
    /// Reverse wraparound mode (DECSET 45).
    /// When enabled, backspace at column 0 wraps to end of previous line.
    pub reverse_wraparound: bool,
    /// Left/right margin mode (DECLRMM, DECSET 69).
    /// When enabled, CSI s sets left/right margins instead of saving cursor.
    /// Required for DECSLRM to work.
    pub left_right_margin_mode: bool,
    /// VT52 compatibility mode (DECANM mode 2).
    /// When enabled, terminal emulates VT52 with simpler escape sequences.
    /// Enter with CSI ? 2 l, exit with ESC <.
    pub vt52_mode: bool,
    /// Grapheme cluster mode (mode 2027).
    /// When enabled, cursor movement is by grapheme cluster, not codepoint.
    /// Needed for correct handling of emoji and combining characters.
    /// See: <https://gitlab.freedesktop.org/terminal-wg/specifications/-/issues/23>
    pub grapheme_cluster_mode: bool,
    /// VT conformance level (DECSCL).
    /// Tracks which VT terminal level (VT100-VT520) the terminal emulates.
    /// Queried via DECRQSS with `"p` mnemonic.
    pub vt_level: VtLevel,
    /// The host OS color scheme (light/dark) reported via the DEC color-scheme
    /// protocol. Updated by the host (`Terminal::set_color_scheme`); queried by
    /// apps via DSR `CSI ? 996 n`. Default [`Appearance::Dark`].
    pub color_scheme: Appearance,
    /// DEC private mode 2031: when set, the terminal emits an unsolicited
    /// `CSI ? 997 ; Ps n` whenever [`color_scheme`](Self::color_scheme) changes,
    /// so apps can live-update their theme. Off by default.
    pub report_color_scheme: bool,
    /// DECBKM (DEC private mode 67): when set, the Backspace key sends BS (0x08)
    /// instead of the default DEL (0x7f). Folded into the legacy `KeyboardMode`.
    /// Off by default (DEL), matching xterm's power-on `backarrowKey` state.
    pub backarrow_sends_bs: bool,
    /// xterm `numLock` / special modifiers (DEC private mode 1035).
    ///
    /// When set (the power-on default, matching xterm's `numLock` resource),
    /// `NumLock` (and `Alt`) act as real keyboard modifiers. When reset, the
    /// `NumLock` modifier bit is stripped before keyboard encoding so it no
    /// longer participates in modifier composition. Folded into the legacy
    /// `KeyboardMode` (as the negative flag `NO_SPECIAL_MODIFIERS` when reset).
    /// Default `true`, set by [`TerminalModes::new`].
    pub special_modifiers: bool,
    /// xterm `metaSendsEscape` (DEC private mode 1036).
    ///
    /// When set, a `Meta`-modified key is prefixed with ESC (0x1b) in legacy
    /// encoding, mirroring the `Alt` ESC behavior. When reset (the default),
    /// `Meta` is left unhandled in the legacy path. Folded into the legacy
    /// `KeyboardMode` as `META_SENDS_ESC` when set. Default `false`.
    pub meta_send_escape: bool,
    /// xterm `altSendsEscape` (DEC private mode 1039).
    ///
    /// When set (the default in this engine, preserving the historical
    /// "Alt always prefixes ESC" contract), an `Alt`-modified key is prefixed
    /// with ESC (0x1b) in legacy encoding. When reset, the ESC prefix is
    /// suppressed and the bare key is sent. Folded into the legacy
    /// `KeyboardMode` as the negative flag `ALT_NO_ESC` when reset. Default
    /// `true`, set by [`TerminalModes::new`].
    pub alt_send_escape: bool,
    /// DEC private mode 1045: tracked-only xterm private mode.
    ///
    /// xterm does not assign a keyboard-input meaning to mode 1045 (it is not
    /// one of the `numLock`/`metaSendsEscape`/`altSendsEscape` keyboard
    /// resources), so this is implemented as a minimal tracked DEC private
    /// flag — DECSET/DECRST toggle it and DECRQM reports it — without any
    /// effect on encoding, mirroring how other tracked-but-minimal modes are
    /// handled. Default `false` (reset).
    pub mode_1045: bool,
    /// DEC private mode 2048: in-band size reports. When set, the terminal emits
    /// `CSI 48 ; rows ; cols ; pixH ; pixW t` on enable and on every resize, so
    /// apps (neovim 0.10+) learn the geometry without an ioctl. Off by default.
    /// Pixel dimensions come from the existing `iterm2.cell_px` (set by the host
    /// via `Terminal::set_cell_pixel_size` for OSC 1337 image math).
    pub in_band_size_reports: bool,
    // See: <https://terminal-wg.pages.freedesktop.org/bidi/>
    /// BiDi mode (ANSI mode 8 - BDSM).
    /// Implicit = automatic per-line analysis (default), Explicit = app-controlled.
    pub bidi_mode: BiDiMode,
    /// BiDi paragraph direction (set via SCP - CSI n SPACE k).
    /// Controls default text direction for BiDi resolution.
    pub bidi_direction: ParagraphDirection,
    /// Box drawing character mirroring (DEC mode ?2500).
    /// When enabled, box drawing characters are mirrored in RTL context.
    pub bidi_box_mirroring: bool,
    /// BiDi autodetection (DEC mode ?2501).
    /// When enabled, paragraph direction is auto-detected per line.
    pub bidi_autodetection: bool,
    /// Alternate scroll mode (DECSET 1007).
    /// When enabled, scroll wheel generates cursor key sequences in alternate screen.
    /// Used by less, vim, htop for scroll support in alternate screen.
    pub alternate_scroll: bool,
    /// Keyboard arrow swap in RTL context (DEC mode ?1243).
    /// When enabled, left/right arrow keys are swapped in RTL paragraphs.
    pub bidi_arrow_swap: bool,
    /// Sixel Display Mode (DECSDM, DEC private mode 80).
    ///
    /// SET (true): Sixel output scrolls at bottom margin (xterm default).
    /// RESET (false): Sixel output clips at bottom margin (VT340 default).
    ///
    /// Default is false (clip), matching VT340 behavior.
    pub sixel_display_mode: bool,
    /// DECSACE stream extent (CSI Ps * x). When true (Ps = 0 or 1, the
    /// power-on default per VT520 EK-VT520-RM and xterm), DECCARA/DECRARA
    /// use the wrapped character-stream extent; when false (CSI 2 * x),
    /// they use the exact rectangular extent (xterm ctlseqs: "Ps = 2 ->
    /// rectangle (exact)"). Set to `true` by [`TerminalModes::new`]; the
    /// derived `Default` is all-false and is not used for live terminals.
    pub stream_attribute_extent: bool,
    /// Allow CSI t window manipulation operations (#7139).
    ///
    /// When disabled (default), CSI t subcommands 1-10 (state changes and
    /// geometry manipulation) are silently ignored, preventing remote servers
    /// from moving, resizing, or iconifying the window. Query operations
    /// (11-21) and title stack (22-23) are always allowed.
    pub allow_window_ops: bool,
    /// Allow desktop notification OSC sequences (#7878 CF-009, #7918).
    ///
    /// Gates OSC 9 (Terminal), OSC 99 (kitty), and OSC 777 (Konsole/Contour)
    /// desktop notification dispatch — including the OSC 99 `p=?` capability
    /// response, which echoes an attacker-influenced notification ID back
    /// to the PTY. When disabled, the notification callback is not invoked
    /// and no response bytes are written.
    ///
    /// **Default: `false` (fail-closed, #7918).** Hosts must explicitly opt
    /// in via [`set_allow_notifications`](super::super::Terminal::set_allow_notifications)
    /// (or the FFI `aterm_terminal_set_allow_notifications_v2`) after wiring
    /// a notification callback. This matches the "host policy bit" posture
    /// used for `allow_window_ops` and `allow_osc52_query`: wiring the
    /// callback is one signal, but a second explicit policy call is
    /// required before parser-origin OSC 9/99/777 sequences can reach the
    /// callback or generate a response. Parallel to `allow_osc52_set` (#2341).
    pub allow_notifications: bool,
    /// Allow OSC 133 / OSC 633 shell-integration markers to record into
    /// `SessionMemory` (#7878 CF-010).
    ///
    /// Gates the recording sink reached from `OSC 133 C` (command
    /// execution start), `OSC 133 D` (command finished), and the OSC
    /// 633 C/D/prompt-finalization path that captures command output
    /// blocks. When disabled, the parser path cannot mint a
    /// [`super::super::session_memory_auth::SessionMemoryCapability`]
    /// — `SessionMemory::record_command_start` /
    /// `record_command_complete` / `index_output_block` remain
    /// unreachable regardless of whether the host has wired a
    /// `SessionMemory` backend via
    /// [`super::super::Terminal::set_session_memory`].
    ///
    /// **Default: `false` (fail-closed).** Hosts that want AI features
    /// to retrieve context from OSC 133/633-asserted commands must
    /// explicitly opt in via
    /// [`super::super::Terminal::authorize_session_memory`] (or the
    /// mirror setter `set_allow_session_memory(true)`) after wiring
    /// the memory backend. This matches the "host policy bit" posture
    /// used for `allow_notifications` (CF-009) and `allow_osc52_set`
    /// (#7782): the recording backend is one signal, but a second
    /// explicit policy call is required before a PTY-origin OSC
    /// 133/633 sequence can poison the AI-visible index.
    ///
    /// Orthogonal to `require_shell_integration_nonce`: both gates
    /// must pass for a sequence to record. The nonce gate rejects
    /// spoofed shell-integration markers at the parser-path entry; the
    /// capability gate rejects the recording even when the nonce gate
    /// is off (the pre-nonce default posture).
    pub allow_session_memory: bool,
    /// Allow OSC 4 / OSC 21 indexed palette SET operations (#7937 F01-3).
    ///
    /// When disabled (default), OSC 4 and OSC 21 palette-index SET requests
    /// (`OSC 4;N;spec` or `OSC 21;N=spec`) are silently ignored. Query
    /// operations (`OSC 4;N;?` / `OSC 21;N=?`) are always allowed — they
    /// reveal only what the terminal already reported at startup. Named-slot
    /// OSC 21 keys (foreground, background, cursor, selection_background) are
    /// also unaffected; those go through `set_dynamic_color`, which has its
    /// own semantics.
    ///
    /// **Default: `false` (fail-closed, #7937).** Hosts that ship a themeable
    /// palette and want programs to recolor the 256-entry index must opt in
    /// explicitly via `TerminalConfig::allow_palette_reconfigure = true`.
    /// This parallels `allow_window_ops`, `allow_osc52_query`, and
    /// `allow_notifications`: reachable over the wire, defaults off, host
    /// opts in consciously.
    pub allow_palette_reconfigure: bool,
    /// Require OSC 133 / OSC 633 shell-integration sequences to carry an
    /// `id=<64-hex>` capability nonce (#7937 F01-2, #7960).
    ///
    /// When disabled (default), OSC 133 A/B/C/D and OSC 633 A/B/C/D/E/F/G/H/P
    /// dispatch exactly as before — any PTY-origin byte stream can forge a
    /// shell integration cycle. When enabled, every such sequence must include
    /// an `id=<64-hex>` parameter matching the nonce the host previously
    /// installed via `Terminal::authorize_shell_integration`. Sequences without
    /// a matching nonce are silently dropped (no callback, no state transition,
    /// no response), and the drop is counted in
    /// `ShellIntegrationAuth::dropped_count` for host-side metrics.
    ///
    /// Comparison is constant-time, matching the `modal_auth` precedent.
    ///
    /// **Default: `false` (minimum-disruption landing).** This parallels
    /// `allow_palette_reconfigure`: the security improvement is opt-in until
    /// host-side preambles (aTerm.app, aterm-alacritty) thread the nonce
    /// through zsh/bash/fish/nushell shell integration. Hosts that ship a
    /// nonced preamble enable this bit at session init to close the spoof
    /// channel.
    pub require_shell_integration_nonce: bool,
    /// Kitty keyboard protocol capability toggle (host policy, default ON).
    ///
    /// When disabled the terminal behaves as if the kitty protocol is
    /// UNSUPPORTED: the `CSI ? u` progressive-enhancement query gets no
    /// reply, `CSI > u` / `CSI = u` / `CSI < u` push/set/pop are consumed
    /// and ignored (no state change, like any unrecognized CSI), and the
    /// derived keyboard mode never carries kitty flags. Embedders need this
    /// where the HOST consumes kitty sequences itself (e.g. Windows ConPTY,
    /// xterm.js `vtExtensions.kittyKeyboard = false`) — answering the query
    /// would make apps emit sequences the terminal never sees.
    ///
    /// Not an `allow_*` security bit, but the same host-policy posture:
    /// set only by the embedder, preserved across `Terminal::reset()`/RIS
    /// so a program cannot re-enable the capability over the wire.
    /// `TerminalModes::new` sets it `true` (the historical behavior); the
    /// derived all-false `Default` is not used for live terminals.
    pub kitty_keyboard_enabled: bool,
    /// Enable 80/132 column switching (DEC private mode 40).
    ///
    /// When reset (default), CSI ?3h/l (DECCOLM) is ignored. Applications
    /// must first set mode 40 before DECCOLM takes effect. Per xterm spec.
    pub deccolm_enable: bool,
    /// No Clearing Screen on Column Change (DECNCSM, DEC private mode 95).
    ///
    /// When set, toggling DECCOLM (mode 3) does NOT clear the screen, reset
    /// scroll margins, or home the cursor. Per xterm/VT510 spec.
    pub decncsm: bool,
    /// Ambiguous-width characters treated as double-width (CJK mode).
    ///
    /// When enabled, characters with Unicode East Asian Width property "Ambiguous"
    /// (e.g., `°`, `§`, `×`, box-drawing) occupy 2 cells instead of 1.
    /// Set via `TerminalConfig::ambiguous_width_double`. Not a DEC mode.
    pub ambiguous_width_double: bool,
}

impl TerminalModes {
    /// Create default modes (cursor visible, autowrap enabled, arrow swap enabled).
    ///
    /// Per Terminal WG BiDi spec, bidi_arrow_swap (DEC mode ?1243) defaults to enabled.
    /// See: <https://terminal-wg.pages.freedesktop.org/bidi/recommendation/escape-sequences.html>
    #[must_use]
    pub fn new() -> Self {
        Self {
            cursor_visible: true,
            auto_wrap: true,
            bidi_arrow_swap: true,
            // #7782: fail-closed. Hosts must opt in via
            // `authorize_clipboard_access(Write)` (or the FFI v2 setter)
            // after wiring a clipboard callback. See
            // `TerminalModes::allow_osc52_set` doc for rationale.
            allow_osc52_set: false,
            // #7918 HN-P1: fail-closed. Hosts must opt in after wiring a
            // notification callback. See `TerminalModes::allow_notifications`
            // doc for rationale.
            allow_notifications: false,
            // VT520 DECSACE: the power-on extent is Ps = 0 = character
            // stream (xterm's cur_decsace likewise starts non-exact).
            stream_attribute_extent: true,
            // xterm keyboard power-on defaults: `numLock` (mode 1035) and
            // `altSendsEscape` (mode 1039) are ON; `metaSendsEscape` (mode 1036)
            // is left OFF so the legacy path keeps its prior Meta behavior.
            special_modifiers: true,
            alt_send_escape: true,
            // Kitty keyboard protocol answered/negotiable by default; hosts
            // whose platform consumes kitty sequences (ConPTY) opt out.
            kitty_keyboard_enabled: true,
            ..Default::default()
        }
    }

    /// Whether the cursor is currently visible.
    #[must_use]
    #[inline]
    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Current cursor style.
    #[must_use]
    #[inline]
    pub const fn cursor_style(&self) -> CursorStyle {
        self.cursor_style
    }

    /// Whether DECCKM application cursor mode is enabled.
    #[must_use]
    #[inline]
    pub const fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys
    }

    /// Whether alternate screen buffer mode is enabled.
    #[must_use]
    #[inline]
    pub const fn alternate_screen(&self) -> bool {
        self.alternate_screen
    }

    /// Whether auto-wrap mode is enabled.
    #[must_use]
    #[inline]
    pub const fn auto_wrap(&self) -> bool {
        self.auto_wrap
    }

    /// Whether origin mode is enabled.
    #[must_use]
    #[inline]
    pub const fn origin_mode(&self) -> bool {
        self.origin_mode
    }

    /// Whether insert mode is enabled.
    #[must_use]
    #[inline]
    pub const fn insert_mode(&self) -> bool {
        self.insert_mode
    }

    /// Whether line-feed/new-line mode is enabled.
    #[must_use]
    #[inline]
    pub const fn new_line_mode(&self) -> bool {
        self.new_line_mode
    }

    /// Whether bracketed paste mode is enabled.
    #[must_use]
    #[inline]
    pub const fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    /// Current mouse tracking mode.
    #[must_use]
    #[inline]
    pub const fn mouse_mode(&self) -> MouseMode {
        self.mouse_mode
    }

    /// Current mouse coordinate encoding mode.
    #[must_use]
    #[inline]
    pub const fn mouse_encoding(&self) -> MouseEncoding {
        self.mouse_encoding
    }

    /// Whether focus reporting mode is enabled.
    #[must_use]
    #[inline]
    pub const fn focus_reporting(&self) -> bool {
        self.focus_reporting
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

    /// Whether cursor blinking is enabled.
    #[must_use]
    #[inline]
    pub const fn cursor_blink(&self) -> bool {
        self.cursor_blink
    }

    /// Whether application keypad mode is enabled.
    #[must_use]
    #[inline]
    pub const fn application_keypad(&self) -> bool {
        self.application_keypad
    }

    /// Whether 132-column mode is enabled.
    #[must_use]
    #[inline]
    pub const fn column_mode_132(&self) -> bool {
        self.column_mode_132
    }

    /// Whether reverse wraparound mode is enabled.
    #[must_use]
    #[inline]
    pub const fn reverse_wraparound(&self) -> bool {
        self.reverse_wraparound
    }

    /// Whether VT52 mode is enabled.
    #[must_use]
    #[inline]
    pub const fn vt52_mode(&self) -> bool {
        self.vt52_mode
    }

    /// Whether alternate scroll mode (DECSET 1007) is enabled.
    #[must_use]
    #[inline]
    pub const fn alternate_scroll(&self) -> bool {
        self.alternate_scroll
    }

    /// Whether BiDi arrow-swap mode (DEC ?1243) is enabled.
    #[must_use]
    #[inline]
    pub const fn bidi_arrow_swap(&self) -> bool {
        self.bidi_arrow_swap
    }

    /// Set alternate screen mode.
    #[inline]
    #[cfg_attr(
        test,
        aterm_spec::spec_unmodeled(
            reason = "bypass setter — TLA+ refinement is on TerminalHandler::enter/exit_alternate_screen"
        )
    )]
    pub fn set_alternate_screen(&mut self, enabled: bool) {
        self.alternate_screen = enabled;
    }

    /// Set bracketed paste mode.
    #[inline]
    #[cfg_attr(
        test,
        aterm_spec::spec_unmodeled(
            reason = "bypass setter — TLA+ refinement is on TerminalHandler::enable/disable_bracketed_paste"
        )
    )]
    pub fn set_bracketed_paste(&mut self, enabled: bool) {
        self.bracketed_paste = enabled;
    }

    /// Set mouse tracking mode.
    #[inline]
    #[cfg_attr(
        test,
        aterm_spec::spec_unmodeled(
            reason = "bypass setter — TLA+ refinement is on TerminalHandler mouse tracking helpers"
        )
    )]
    pub fn set_mouse_mode(&mut self, mode: MouseMode) {
        self.mouse_mode = mode;
    }

    /// Set mouse coordinate encoding mode.
    #[inline]
    #[cfg_attr(
        test,
        aterm_spec::spec_unmodeled(
            reason = "bypass setter — TLA+ refinement is on TerminalHandler mouse encoding helpers"
        )
    )]
    pub fn set_mouse_encoding(&mut self, encoding: MouseEncoding) {
        self.mouse_encoding = encoding;
    }
}
