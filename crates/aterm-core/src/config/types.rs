// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Configuration types for aterm-core terminals.

use crate::platform::FontDescriptor;
use aterm_types::{ColorPalette, CursorStyle, ParagraphDirection, Rgb};

// BiDi mode extracted to aterm-types crate (#2440).
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
    /// Whether to apply BiDi processing in alternate screen mode.
    ///
    /// When true, BiDi reordering is applied to the alternate screen buffer
    /// (used by fullscreen applications like vim, less, etc.).
    /// When false, alternate screen content is displayed without BiDi reordering.
    ///
    /// Default: true (matching aTerm.app's alternateScreenBidi setting).
    pub alternate_screen: bool,
    /// Whether to apply Arabic contextual text shaping.
    ///
    /// When true, Arabic characters are transformed to their contextual forms
    /// (initial, medial, final, isolated) based on surrounding characters.
    /// This includes LAM-ALEF ligatures and proper joining behavior.
    ///
    /// Requires the `arabic-shaping` feature to be enabled.
    /// When the feature is disabled, this setting has no effect.
    ///
    /// Default: false (opt-in to avoid overhead for non-Arabic users).
    ///
    /// # References
    /// - Issue #1861
    /// - mintty's Arabic shaping implementation
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

// ============================================================================
// Scrollback Backend Configuration
// ============================================================================

/// Scrollback storage backend configuration.
///
/// Controls whether scrollback is stored entirely in memory (default) or uses
/// disk-backed cold tier storage for unlimited history.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum ScrollbackBackend {
    /// Memory-only scrollback with tiered compression.
    ///
    /// All scrollback data is kept in RAM using hot/warm/cold tiers:
    /// - Hot: Uncompressed lines (fast access)
    /// - Warm: LZ4 compressed blocks
    /// - Cold: Zstd compressed blocks (evicted when memory budget exceeded)
    #[default]
    Memory,

    /// Disk-backed scrollback for unlimited history.
    ///
    /// Like Memory, but cold tier is persisted to disk:
    /// - Hot: Uncompressed lines (fast access)
    /// - Warm: LZ4 compressed blocks
    /// - Cold: Zstd compressed, stored on disk with LRU cache
    Disk(DiskBackendConfig),
}

/// Configuration for disk-backed scrollback storage.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskBackendConfig {
    /// Path to the cold tier storage file.
    pub path: std::path::PathBuf,
    /// Maximum lines in hot tier before promotion (default: 1000).
    pub hot_limit: usize,
    /// Maximum lines in warm tier before eviction (default: 10000).
    pub warm_limit: usize,
    /// LRU cache size for cold tier pages (default: 64).
    pub cold_cache_size: usize,
}

#[cfg(test)]
impl DiskBackendConfig {
    /// Create a new disk backend config with the given path.
    #[must_use]
    pub(crate) fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            hot_limit: 1000,
            warm_limit: 10_000,
            cold_cache_size: 64,
        }
    }

    /// Set hot tier limit.
    #[must_use]
    pub(crate) fn with_hot_limit(mut self, limit: usize) -> Self {
        self.hot_limit = limit;
        self
    }

    /// Set warm tier limit.
    #[must_use]
    pub(crate) fn with_warm_limit(mut self, limit: usize) -> Self {
        self.warm_limit = limit;
        self
    }
}

/// Terminal configuration settings.
///
/// This struct bundles all configurable aspects of a terminal that can be
/// changed at runtime without recreating the terminal instance.
///
/// # Configuration Categories
///
/// - **Display**: Cursor style, cursor blink, font descriptor
/// - **Colors**: Foreground, background, cursor color, palette
/// - **Behavior**: Scrollback limit, auto-wrap, focus reporting
/// - **Performance**: Memory budget, sync timeout
/// - **BiDi**: Bidirectional text mode, paragraph direction, security
///
/// # Thread Safety
///
/// `TerminalConfig` is `Send + Sync` and can be safely shared between threads.
/// The actual application of configuration to a terminal requires mutable
/// access to the terminal.
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
    pub cursor_blink: bool,

    /// Cursor color override (None uses default from color scheme).
    pub cursor_color: Option<Rgb>,

    /// Whether cursor is visible (DECTCEM mode 25).
    pub cursor_visible: bool,

    /// Font descriptor (family, size, weight, italic).
    pub font: FontDescriptor,

    // === Color Settings ===
    /// Default foreground color.
    pub default_foreground: Rgb,

    /// Default background color.
    pub default_background: Rgb,

    /// Selection background color override (None uses renderer default).
    /// Can be queried/modified via OSC 21 selection_background.
    pub selection_background: Option<Rgb>,

    /// Custom color palette (if any).
    /// When `None`, uses the default xterm 256-color palette.
    pub custom_palette: Option<ColorPalette>,

    // === Behavior Settings ===
    /// Maximum number of scrollback lines to retain.
    ///
    /// `None` means unlimited scrollback (limited only by `memory_budget`).
    /// When the scrollback exceeds this limit, older lines are discarded
    /// (or moved to cold storage if configured).
    ///
    /// Default: `Some(100_000)` (see [`aterm_scrollback::DEFAULT_LINE_LIMIT`],
    /// #7929).
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

    /// Allow CSI t window operations 1 through 21 (#7139).
    ///
    /// When disabled (default), the gated manipulation and report operations
    /// are silently ignored. Title-stack operations 22 and 23 remain available;
    /// hosts still need a window callback for operations that require the OS.
    pub allow_window_ops: bool,

    /// Allow desktop notification OSC sequences (#7878 CF-009, #7918).
    ///
    /// Gates OSC 9 (Terminal), OSC 99 (kitty), and OSC 777 (Konsole/Contour)
    /// desktop notifications. When disabled, the notification callback is
    /// not invoked and the OSC 99 `p=?` capability response is not written.
    ///
    /// **Default: `false` (fail-closed, #7918).** Hosts must explicitly opt
    /// in after wiring a notification callback. The prior default (`true`)
    /// left the CF-009 gate cosmetic: any runtime that accepted the default
    /// exposed OSC 9/99/777 dispatch without ever making a conscious policy
    /// decision. Parallels `allow_window_ops` (default false) rather than
    /// `allow_osc52_set` (default true) because notifications reach a
    /// user-visible surface, not just the wire.
    pub allow_notifications: bool,

    /// Allow OSC 4 / OSC 21 indexed palette SET operations (#7937 F01-3).
    ///
    /// Gates `OSC 4;N;spec` and `OSC 21;N=spec` (numeric-index) SET requests.
    /// Query operations are always allowed; OSC 21 named-slot sets
    /// (`foreground`, `background`, `cursor`, `selection_background`) are
    /// unaffected — those go through dynamic-color callbacks, not the
    /// indexed palette.
    ///
    /// **Default: `false` (fail-closed, #7937).** Hosts that want programs to
    /// recolor the 256-entry palette opt in explicitly. Parallels
    /// `allow_window_ops`, `allow_osc52_query`, and `allow_notifications`.
    pub allow_palette_reconfigure: bool,

    // === Performance Settings ===
    /// Memory budget for scrollback (in bytes).
    /// This controls when lines are moved from hot to warm to cold storage.
    pub memory_budget: usize,

    /// Synchronized output timeout in milliseconds.
    /// How long to wait before forcing sync mode off.
    pub sync_timeout_ms: u64,

    // === Scrollback Settings ===
    /// Storage backend for scrollback history.
    ///
    /// # Important
    /// This field is **construction-time only** -- it is read during `Terminal::new()`
    /// and ignored by [`Terminal::apply_config()`]. Switching backends requires
    /// migrating live scrollback data between storage tiers, which is not
    /// supported. To change the backend, create a new terminal. Consider using
    /// [`ConfigBuilder::scrollback_backend()`](super::builder::ConfigBuilder::scrollback_backend)
    /// instead.
    pub scrollback_backend: ScrollbackBackend,

    // === Text Settings ===
    /// Ambiguous-width characters treated as double-width (CJK mode).
    ///
    /// When enabled, characters with Unicode East Asian Width property "Ambiguous"
    /// (e.g., `°`, `§`, `×`, box-drawing) occupy 2 cells instead of 1.
    /// This matches the behavior of CJK locale terminal emulators.
    pub ambiguous_width_double: bool,

    /// SGR 1 (bold) promotes indexed colors 0–7 to their bright 8–15 siblings
    /// (the classic xterm behavior). Default `true` — byte-identical to the
    /// historical unconditional promotion; `false` keeps bold a pure weight
    /// change (config `bold_is_bright`, W5f).
    pub bold_is_bright: bool,

    /// SGR 2 (dim/faint): the fraction of the foreground retained, `0.0..=1.0`.
    /// The fg is blended TOWARD THE CELL BACKGROUND in linear light by this
    /// fraction (see `color_resolve::dim_toward_bg`), so faint text recedes on
    /// both dark and light themes. Default `0.5` (config `faint_opacity`, W5e).
    pub faint_opacity: f32,

    // === BiDi Settings ===
    /// Bidirectional text configuration.
    /// Controls how RTL text (Arabic, Hebrew, etc.) is displayed.
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
            font: FontDescriptor::default(),
            // Colors — single source of truth (was 255,255,255 here vs 229,229,229
            // in the runtime terminal state; unified to avoid the divergence the
            // color audit flagged). See aterm_types::DEFAULT_FOREGROUND.
            default_foreground: aterm_types::DEFAULT_FOREGROUND,
            default_background: aterm_types::DEFAULT_BACKGROUND,
            selection_background: None,
            custom_palette: None,
            // Behavior
            scrollback_limit: Some(aterm_scrollback::DEFAULT_LINE_LIMIT),
            auto_wrap: true,
            focus_reporting: false,
            bracketed_paste: false,
            allow_osc52_query: false,
            allow_window_ops: false,
            // #7918 HN-P1: fail-closed. Hosts must explicitly opt in
            // (e.g. via `Terminal::set_allow_notifications(true)`) after
            // wiring a notification callback.
            allow_notifications: false,
            // #7937 HN-P1 F01-3: fail-closed palette reconfigure. Hosts that
            // ship a themeable palette opt in explicitly.
            allow_palette_reconfigure: false,
            // Performance
            memory_budget: 100 * 1024 * 1024, // 100 MB
            sync_timeout_ms: 1000,            // 1 second
            // Scrollback
            scrollback_backend: ScrollbackBackend::default(),
            // Text
            ambiguous_width_double: false,
            bold_is_bright: true,
            faint_opacity: aterm_types::DIM_FACTOR,
            // BiDi
            bidi: BiDiConfig::default(),
        }
    }
}

impl TerminalConfig {
    /// Create a configuration builder for fluent API.
    ///
    /// Returns a [`ConfigBuilder`](super::builder::ConfigBuilder) initialized
    /// with default values. This is the recommended way to construct a
    /// `TerminalConfig` when you only need to override a few fields.
    #[must_use]
    pub fn builder() -> super::builder::ConfigBuilder {
        super::builder::ConfigBuilder::new()
    }

    /// Compare with another config and return list of changes.
    ///
    /// This is useful for determining what UI elements need to be updated
    /// after a configuration change.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn diff(&self, other: &Self) -> Vec<ConfigChange> {
        let mut changes = Vec::new();

        if self.cursor_style != other.cursor_style {
            changes.push(ConfigChange::CursorStyle);
        }
        if self.cursor_blink != other.cursor_blink {
            changes.push(ConfigChange::CursorBlink);
        }
        if self.cursor_color != other.cursor_color {
            changes.push(ConfigChange::CursorColor);
        }
        if self.cursor_visible != other.cursor_visible {
            changes.push(ConfigChange::CursorVisible);
        }
        if self.font != other.font {
            changes.push(ConfigChange::Font);
        }
        if self.default_foreground != other.default_foreground
            || self.default_background != other.default_background
            || self.selection_background != other.selection_background
            || self.custom_palette != other.custom_palette
        {
            changes.push(ConfigChange::Colors);
        }
        if self.scrollback_limit != other.scrollback_limit {
            changes.push(ConfigChange::ScrollbackLimit);
        }
        if self.auto_wrap != other.auto_wrap {
            changes.push(ConfigChange::AutoWrap);
        }
        if self.focus_reporting != other.focus_reporting {
            changes.push(ConfigChange::FocusReporting);
        }
        if self.bracketed_paste != other.bracketed_paste {
            changes.push(ConfigChange::BracketedPaste);
        }
        if self.allow_osc52_query != other.allow_osc52_query {
            changes.push(ConfigChange::Osc52ClipboardQuery);
        }
        if self.allow_window_ops != other.allow_window_ops {
            changes.push(ConfigChange::WindowOps);
        }
        if self.allow_notifications != other.allow_notifications {
            changes.push(ConfigChange::Notifications);
        }
        if self.allow_palette_reconfigure != other.allow_palette_reconfigure {
            changes.push(ConfigChange::PaletteReconfigure);
        }
        if self.memory_budget != other.memory_budget {
            changes.push(ConfigChange::MemoryBudget);
        }
        if self.sync_timeout_ms != other.sync_timeout_ms {
            changes.push(ConfigChange::SyncTimeout);
        }
        if self.bidi != other.bidi {
            changes.push(ConfigChange::BiDi);
        }
        if self.ambiguous_width_double != other.ambiguous_width_double {
            changes.push(ConfigChange::AmbiguousWidth);
        }
        if self.bold_is_bright != other.bold_is_bright
            || (self.faint_opacity - other.faint_opacity).abs() > f32::EPSILON
        {
            changes.push(ConfigChange::StylePolicy);
        }
        // Note: scrollback_backend is construction-time only — not diffed here.

        changes
    }
}

/// Types of configuration changes.
///
/// Used to identify what aspects of the terminal configuration have changed,
/// enabling efficient UI updates.
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
    /// Font descriptor changed (family, size, weight, italic).
    Font,
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
    /// Desktop notification policy (OSC 9/99/777) changed (#7878).
    Notifications,
    /// OSC 4 / OSC 21 indexed palette reconfigure policy changed (#7937).
    PaletteReconfigure,
    /// Ambiguous-width character mode changed.
    AmbiguousWidth,
    /// Style-attribute policy changed (`bold_is_bright` / `faint_opacity`, W5).
    StylePolicy,
}
