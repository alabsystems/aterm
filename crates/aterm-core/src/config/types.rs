// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Configuration types for aterm-core terminals.

use aterm_types::{ColorPalette, CursorStyle, ParagraphDirection, Rgb};

pub use aterm_types::BiDiMode;

// ============================================================================
// BiDi Configuration
// ============================================================================

/// BiDi configuration for terminal display.
#[derive(Debug, Clone, PartialEq)]
pub struct BiDiConfig {
    /// BiDi display mode (Disabled, Implicit, or Explicit).
    pub mode: BiDiMode,
    /// Default paragraph direction for auto-detection.
    pub direction: ParagraphDirection,
    /// Whether to reorder non-spacing marks according to UBA rule L3.
    pub reorder_nsm: bool,
    /// Whether to apply BiDi reordering on the alternate screen (vim, less, …).
    /// Default: true.
    pub alternate_screen: bool,
    /// Whether to apply Arabic contextual shaping (initial, medial, final and
    /// isolated forms, LAM-ALEF ligatures).
    ///
    /// Has no effect without the `arabic-shaping` feature. Default: false.
    pub arabic_shaping: bool,
}

impl Default for BiDiConfig {
    fn default() -> Self {
        Self {
            mode: BiDiMode::default(),
            direction: ParagraphDirection::default(),
            reorder_nsm: true,
            alternate_screen: true,
            arabic_shaping: false,
        }
    }
}

impl BiDiConfig {
    /// Create a disabled BiDi configuration.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn disabled() -> Self {
        Self {
            mode: BiDiMode::Disabled,
            ..Default::default()
        }
    }

    /// Returns true if BiDi processing is enabled.
    #[cfg(test)]
    #[inline]
    pub(crate) fn is_enabled(&self) -> bool {
        self.mode != BiDiMode::Disabled
    }
}

/// Terminal configuration that can change at runtime without recreating the
/// terminal: display (cursor style, color, visibility), colors and palette,
/// behavior (scrollback, auto-wrap, focus reporting), performance (memory
/// budget, sync timeout) and BiDi.
///
/// `Send + Sync`; applying it to a terminal needs `&mut Terminal`.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "terminal config has many boolean options"
)]
pub struct TerminalConfig {
    // === Display Settings ===
    /// Cursor style (block, underline, bar).
    pub cursor_style: CursorStyle,

    /// Whether the cursor should blink.
    ///
    /// A mirror, not the authority: the renderer and DEC mode 12 use the blink
    /// bit of [`Self::cursor_style`], and `apply_config` sets this to match.
    /// Setting this alone changes nothing; [`CursorStyle::with_blink`] folds a
    /// blink preference into a shape.
    pub cursor_blink: bool,

    /// Cursor color override (None uses default from color scheme).
    pub cursor_color: Option<Rgb>,

    /// Whether cursor is visible (DECTCEM mode 25).
    pub cursor_visible: bool,

    // === Color Settings ===
    /// Default foreground color.
    pub default_foreground: Rgb,

    /// Default background color.
    pub default_background: Rgb,

    /// Selection background color override (None uses renderer default).
    /// Can be queried/modified via OSC 21 selection_background.
    pub selection_background: Option<Rgb>,

    /// Selection foreground color override (None uses renderer contrast logic).
    /// Can be queried/modified via OSC 19 or OSC 21 `selection_foreground`.
    pub selection_foreground: Option<Rgb>,

    /// Custom color palette (if any).
    /// When `None`, uses the default xterm 256-color palette.
    pub custom_palette: Option<ColorPalette>,

    // === Behavior Settings ===
    /// Maximum number of scrollback lines to retain.
    ///
    /// `None` means unlimited (bounded only by `memory_budget`). Older lines
    /// are discarded, or moved to cold storage if configured. Default:
    /// `Some(100_000)` ([`aterm_scrollback::DEFAULT_LINE_LIMIT`]).
    pub scrollback_limit: Option<usize>,

    /// Auto-wrap mode (DECAWM mode 7).
    /// When enabled, lines wrap at the right margin.
    pub auto_wrap: bool,

    /// Focus reporting mode (mode 1004).
    /// When enabled, terminal sends focus/blur notifications.
    pub focus_reporting: bool,

    /// Bracketed paste mode (mode 2004).
    /// When enabled, pasted text is wrapped in escape sequences.
    pub bracketed_paste: bool,

    /// Allow OSC 52 clipboard queries (Pd = "?").
    ///
    /// When disabled (default), aterm-core ignores OSC 52 query requests and
    /// does not invoke the clipboard callback or emit a response.
    pub allow_osc52_query: bool,

    /// Allow CSI t window operations 1 through 21.
    ///
    /// When disabled (default), the gated manipulation and report operations
    /// are silently ignored. Title-stack operations 22 and 23 remain available;
    /// hosts still need a window callback for operations that require the OS.
    pub allow_window_ops: bool,

    /// Allow desktop notification OSC sequences.
    ///
    /// Gates OSC 9, OSC 99 (kitty) and OSC 777 notifications; when disabled the
    /// callback is not invoked and the OSC 99 `p=?` response is not written.
    ///
    /// Default: `false` (fail-closed). Hosts opt in after wiring a notification
    /// callback, since notifications reach a user-visible surface.
    pub allow_notifications: bool,

    /// Allow OSC 4 / OSC 21 indexed palette set operations.
    ///
    /// Gates numeric-index sets (`OSC 4;N;spec`, `OSC 21;N=spec`). Queries are
    /// always allowed; OSC 21 named-slot sets (`foreground`, `background`,
    /// `cursor`, `selection_*`) go through dynamic-color callbacks and are
    /// unaffected. Default: `false` (fail-closed).
    pub allow_palette_reconfigure: bool,

    // === Performance Settings ===
    /// Memory budget for scrollback (in bytes).
    /// This controls when lines are moved from hot to warm to cold storage.
    pub memory_budget: usize,

    /// Synchronized output timeout in milliseconds.
    /// How long to wait before forcing sync mode off.
    pub sync_timeout_ms: u64,

    // === Text Settings ===
    /// Ambiguous-width characters treated as double-width (CJK mode).
    ///
    /// When enabled, characters with Unicode East Asian Width property "Ambiguous"
    /// (e.g., `°`, `§`, `×`, box-drawing) occupy 2 cells instead of 1.
    /// This matches the behavior of CJK locale terminal emulators.
    pub ambiguous_width_double: bool,

    /// SGR 1 (bold) promotes indexed colors 0–7 to their bright 8–15 siblings
    /// (the classic xterm behavior). Default `true`; `false` keeps bold a pure
    /// weight change (config `bold_is_bright`).
    pub bold_is_bright: bool,

    /// SGR 2 (dim/faint): the fraction of the foreground retained, `0.0..=1.0`.
    /// The fg is blended toward the cell background in linear light
    /// (`color_resolve::dim_toward_bg`), so faint text recedes on dark and light
    /// themes, keeping at least this share of the fg's contrast (in log space)
    /// so dark ink on a light ground never sinks into the mid-greys. Default
    /// `0.5` (config `faint_opacity`).
    pub faint_opacity: f32,

    // === BiDi Settings ===
    /// Bidirectional text configuration (how RTL text is displayed).
    pub bidi: BiDiConfig,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            // Display
            cursor_style: CursorStyle::BlinkingBlock,
            cursor_blink: true,
            cursor_color: None,
            cursor_visible: true,
            default_foreground: aterm_types::DEFAULT_FOREGROUND,
            default_background: aterm_types::DEFAULT_BACKGROUND,
            selection_background: None,
            selection_foreground: None,
            custom_palette: None,
            // Behavior
            scrollback_limit: Some(aterm_scrollback::DEFAULT_LINE_LIMIT),
            auto_wrap: true,
            focus_reporting: false,
            bracketed_paste: false,
            allow_osc52_query: false,
            allow_window_ops: false,
            allow_notifications: false,
            allow_palette_reconfigure: false,
            // Performance
            memory_budget: 100 * 1024 * 1024, // 100 MB
            sync_timeout_ms: 1000,            // 1 second
            // Text
            ambiguous_width_double: false,
            bold_is_bright: true,
            faint_opacity: aterm_types::DIM_FACTOR,
            // BiDi
            bidi: BiDiConfig::default(),
        }
    }
}

/// One aspect of the terminal configuration that changed, so a host updates
/// only what it must.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ConfigChange {
    /// Cursor style changed (block, underline, bar).
    CursorStyle,
    /// Cursor blink setting changed.
    CursorBlink,
    /// Cursor color changed.
    CursorColor,
    /// Cursor visibility changed.
    CursorVisible,
    /// Color scheme changed (foreground, background, or palette).
    Colors,
    /// Scrollback limit changed.
    ScrollbackLimit,
    /// Auto-wrap mode changed.
    AutoWrap,
    /// Focus reporting mode changed.
    FocusReporting,
    /// Bracketed paste mode changed.
    BracketedPaste,
    /// OSC 52 clipboard query policy changed.
    Osc52ClipboardQuery,
    /// Memory budget changed.
    MemoryBudget,
    /// Sync timeout changed.
    SyncTimeout,
    /// BiDi configuration changed (mode, direction, security).
    BiDi,
    /// CSI t window manipulation policy changed.
    WindowOps,
    /// Desktop notification policy (OSC 9/99/777) changed.
    Notifications,
    /// OSC 4 / OSC 21 indexed palette reconfigure policy changed.
    PaletteReconfigure,
    /// Ambiguous-width character mode changed.
    AmbiguousWidth,
    /// Style-attribute policy changed (`bold_is_bright` / `faint_opacity`).
    StylePolicy,
}
