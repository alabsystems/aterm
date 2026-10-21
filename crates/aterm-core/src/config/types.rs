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
    /// Whether to reorder non-spacing marks per UBA rule L3. Stored only: the engine's
    /// reorder does not read it.
    pub reorder_nsm: bool,
    /// Whether to apply BiDi reordering on the alternate screen. Stored only: the engine's
    /// reorder does not read it.
    pub alternate_screen: bool,
}

impl Default for BiDiConfig {
    fn default() -> Self {
        Self {
            mode: BiDiMode::default(),
            direction: ParagraphDirection::default(),
            reorder_nsm: true,
            alternate_screen: true,
        }
    }
}

/// Terminal configuration a host changes at runtime without recreating the terminal;
/// [`Terminal::apply_config`](crate::terminal::Terminal::apply_config) applies it.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "terminal config has many boolean options"
)]
pub struct TerminalConfig {
    // === Display Settings ===
    /// Cursor style (block, underline, bar).
    pub cursor_style: CursorStyle,

    /// Whether the cursor should blink. `apply_config` ignores it: the blink bit of
    /// [`Self::cursor_style`] is the authority (renderer, DEC mode 12), and
    /// [`CursorStyle::with_blink`] folds a blink preference into a shape.
    pub cursor_blink: bool,

    /// Cursor color override (`None`: the color scheme's default).
    pub cursor_color: Option<Rgb>,

    /// Whether cursor is visible (DECTCEM mode 25).
    pub cursor_visible: bool,

    // === Color Settings ===
    /// Default foreground color.
    pub default_foreground: Rgb,

    /// Default background color.
    pub default_background: Rgb,

    /// Selection background override (`None`: renderer default); OSC 17 / OSC 21 query and
    /// set the live value.
    pub selection_background: Option<Rgb>,

    /// Selection foreground override (`None`: renderer contrast logic); OSC 19 / OSC 21
    /// query and set the live value.
    pub selection_foreground: Option<Rgb>,

    /// Custom color palette; `None` uses the default xterm 256-color palette.
    pub custom_palette: Option<ColorPalette>,

    /// The host's baseline colour scheme: the appearance of
    /// `default_background` (a theme's declared appearance). Reported to
    /// programs (DSR 996 / mode 2031) while no program has moved the
    /// background. `None` keeps the baseline already set
    /// (`Terminal::set_color_scheme`), so a host that drives it by hand is
    /// unaffected.
    pub color_scheme: Option<aterm_types::Appearance>,

    // === Behavior Settings ===
    /// Maximum scrollback lines to retain; `None` is unlimited (bounded only by
    /// `memory_budget`). Default: [`aterm_scrollback::DEFAULT_LINE_LIMIT`].
    pub scrollback_limit: Option<usize>,

    /// Auto-wrap mode (DECAWM, mode 7): lines wrap at the right margin.
    pub auto_wrap: bool,

    /// Focus reporting mode (mode 1004): the terminal reports focus in/out.
    pub focus_reporting: bool,

    /// Bracketed paste mode (mode 2004): pasted text is wrapped in escape sequences.
    pub bracketed_paste: bool,

    /// Allow OSC 52 clipboard queries (Pd = "?"). When disabled (default), a query invokes no
    /// callback and gets no response.
    pub allow_osc52_query: bool,

    /// Allow CSI t window operations 1 through 21; when disabled (default) they are ignored.
    /// Title-stack operations 22 and 23 stay available; an operation that needs the OS still
    /// needs a window callback.
    pub allow_window_ops: bool,

    /// Allow desktop notifications (OSC 9, OSC 99 kitty, OSC 777); when disabled the callback
    /// is not invoked and the OSC 99 `p=?` response is not written. Default: `false`
    /// (fail-closed): a host opts in after wiring a notification callback.
    pub allow_notifications: bool,

    /// Allow OSC 4 / OSC 21 indexed palette sets (`OSC 4;N;spec`, `OSC 21;N=spec`). Queries are
    /// always allowed, and OSC 21 named-slot sets (`foreground`, `cursor`, `selection_*`, …)
    /// go through dynamic-color callbacks unaffected. Default: `false` (fail-closed).
    pub allow_palette_reconfigure: bool,

    // === Performance Settings ===
    /// Scrollback memory budget in bytes: when lines move from hot to warm to cold storage.
    pub memory_budget: usize,

    /// Synchronized output (mode 2026) timeout in milliseconds before sync is forced off.
    pub sync_timeout_ms: u64,

    // === Text Settings ===
    /// Ambiguous-width characters (East Asian Width "Ambiguous": `°`, `§`, `×`, box drawing)
    /// occupy 2 cells instead of 1, as in CJK-locale terminals.
    pub ambiguous_width_double: bool,

    /// SGR 1 (bold) promotes indexed colors 0–7 to bright 8–15 (xterm). Default `true`; `false`
    /// keeps bold a pure weight change.
    pub bold_is_bright: bool,

    /// SGR 2 (dim/faint): the fraction of the foreground retained, `0.0..=1.0`.
    /// The fg is blended toward the cell background
    /// (`color_resolve::dim_toward_bg`), so faint text recedes on dark and light
    /// themes: light ink as far as an sRGB (alpha) blend at this opacity, dark
    /// ink as a linear-light one. The mix is raised where needed so faint text keeps at least
    /// this share of the fg's contrast (in log space) and a readable
    /// `9^faint_opacity` (3:1 at the default), the latter capped at
    /// `contrast^((1+faint_opacity)/2)` so faint always stays fainter than its
    /// colour. Default `0.5` (config `faint_opacity`).
    pub faint_opacity: f32,

    /// The theme-pair contrast floor, a WCAG ratio clamped `1.0..=21.0`:
    /// where a cell's foreground and background are both the theme's own
    /// (the defaults or palette slots 0-15) and not both the defaults, the
    /// foreground is held to this ratio against a coloured cell and to at
    /// most 3:1 against the theme's background
    /// (`color_resolve::theme_pair_floor`). Truecolor and the 256-colour
    /// cube stay exact. Default `1.0`, off; aterm's app turns it on on
    /// Windows (config `minimum_contrast`).
    pub indexed_minimum_contrast: f32,

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
            color_scheme: None,
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
            indexed_minimum_contrast: 1.0,
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
    /// The colour scheme reported to programs (DSR 996 / mode 2031) changed;
    /// when the program set mode 2031 a `CSI ? 997 ; Ps n` push is queued in
    /// the response buffer.
    ColorScheme,
}
