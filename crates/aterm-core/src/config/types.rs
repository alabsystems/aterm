// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Configuration types for aterm-core terminals.

use aterm_types::{ColorPalette, CursorStyle, ParagraphDirection, Rgb};

pub use aterm_types::BiDiMode;

/// BiDi configuration for terminal display.
#[derive(Debug, Clone, PartialEq)]
pub struct BiDiConfig {
    /// BiDi display mode.
    pub mode: BiDiMode,
    /// Default paragraph direction for auto-detection.
    pub direction: ParagraphDirection,
    /// Whether to reorder non-spacing marks per UBA rule L3 (stored only).
    pub reorder_nsm: bool,
    /// Whether to apply BiDi reordering on the alternate screen (stored only).
    pub alternate_screen: bool,
}

impl Default for BiDiConfig {
    #[inline]
    fn default() -> Self {
        Self {
            mode: BiDiMode::default(),
            direction: ParagraphDirection::default(),
            reorder_nsm: true,
            alternate_screen: true,
        }
    }
}

/// Runtime terminal configuration applied via [`Terminal::apply_config`](crate::terminal::Terminal::apply_config).
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "terminal config has many boolean options"
)]
pub struct TerminalConfig {
    /// Cursor style (block, underline, bar).
    pub cursor_style: CursorStyle,

    /// Whether the cursor should blink (superseded by the blink bit of [`Self::cursor_style`]).
    pub cursor_blink: bool,

    /// Cursor color override (`None`: the color scheme's default).
    pub cursor_color: Option<Rgb>,

    /// Whether cursor is visible (DECTCEM mode 25).
    pub cursor_visible: bool,

    /// Default foreground color.
    pub default_foreground: Rgb,

    /// Default background color.
    pub default_background: Rgb,

    /// Selection background override (`None`: renderer default), queried and set via OSC 17 / OSC 21.
    pub selection_background: Option<Rgb>,

    /// Selection foreground override (`None`: renderer contrast logic), queried and set via OSC 19 / OSC 21.
    pub selection_foreground: Option<Rgb>,

    /// Custom color palette; `None` uses the default xterm 256-color palette.
    pub custom_palette: Option<ColorPalette>,

    /// Host baseline appearance reported to programs (DSR 996 / mode 2031); `None` keeps the existing baseline.
    pub color_scheme: Option<aterm_types::Appearance>,

    /// Maximum scrollback lines to retain (`None` is bounded only by `memory_budget`).
    pub scrollback_limit: Option<usize>,

    /// Auto-wrap mode (DECAWM, mode 7): lines wrap at the right margin.
    pub auto_wrap: bool,

    /// Focus reporting mode (mode 1004): the terminal reports focus in/out.
    pub focus_reporting: bool,

    /// Bracketed paste mode (mode 2004): pasted text is wrapped in escape sequences.
    pub bracketed_paste: bool,

    /// Allow OSC 52 clipboard queries (`Pd = "?"`).
    pub allow_osc52_query: bool,

    /// Allow CSI t window operations 1 through 21 (title-stack operations 22 and 23 remain enabled).
    pub allow_window_ops: bool,

    /// Allow desktop notifications (OSC 9, OSC 99, OSC 777); defaults to `false`.
    pub allow_notifications: bool,

    /// Allow OSC 4 / OSC 21 indexed palette sets; defaults to `false`.
    pub allow_palette_reconfigure: bool,

    /// Scrollback memory budget in bytes.
    pub memory_budget: usize,

    /// Synchronized output (mode 2026) timeout in milliseconds before sync is forced off.
    pub sync_timeout_ms: u64,

    /// Whether East Asian Ambiguous-width characters occupy 2 cells instead of 1.
    pub ambiguous_width_double: bool,

    /// Whether SGR 1 (bold) promotes indexed colors 0–7 to bright 8–15.
    pub bold_is_bright: bool,

    /// SGR 2 (dim/faint) foreground contrast retention in `0.0..=1.0` (default `0.5`).
    pub faint_opacity: f32,

    /// Theme-pair WCAG contrast floor clamped to `1.0..=21.0` (default `1.0`, off).
    pub indexed_minimum_contrast: f32,

    /// Bidirectional text configuration (how RTL text is displayed).
    pub bidi: BiDiConfig,
}

impl Default for TerminalConfig {
    #[inline]
    fn default() -> Self {
        Self {
            cursor_style: CursorStyle::BlinkingBlock,
            cursor_blink: true,
            cursor_color: None,
            cursor_visible: true,
            default_foreground: aterm_types::DEFAULT_FOREGROUND,
            default_background: aterm_types::DEFAULT_BACKGROUND,
            selection_background: None,
            selection_foreground: None,
            custom_palette: None,
            color_scheme: None,
            scrollback_limit: Some(aterm_scrollback::DEFAULT_LINE_LIMIT),
            auto_wrap: true,
            focus_reporting: false,
            bracketed_paste: false,
            allow_osc52_query: false,
            allow_window_ops: false,
            allow_notifications: false,
            allow_palette_reconfigure: false,
            memory_budget: 100 * 1024 * 1024, // 100 MB
            sync_timeout_ms: 1000,            // 1 second
            ambiguous_width_double: false,
            bold_is_bright: true,
            faint_opacity: aterm_types::DIM_FACTOR,
            indexed_minimum_contrast: 1.0,
            bidi: BiDiConfig::default(),
        }
    }
}

/// One aspect of the terminal configuration that changed.
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
    /// BiDi configuration changed.
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
    /// Reported colour scheme (DSR 996 / mode 2031) changed.
    ColorScheme,
}
