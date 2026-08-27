// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grouped sub-state structs for [`Terminal`](super::Terminal).
//!
//! Each struct bundles related fields that are passed together to
//! [`TerminalHandler`](super::handler::TerminalHandler) during processing.
//! Extracted from `terminal/mod.rs` to reduce file size (#1977).

mod protocol;

use super::callbacks::{
    self, AdvancedNotificationCallback, ClipboardCallback, CopyToClipboardCallback,
    NotificationCallback, RemoteHostCallback, SemanticBlockCallback, SemanticButtonCallback,
};
use super::shell::{Annotation, CommandMark, OutputBlock, ShellCallback, ShellState, TerminalMark};
use super::transient_state::{DEFAULT_BACKGROUND, DEFAULT_FOREGROUND};
use super::types::{CopyToClipboardState, SavedCursorState, SemanticBlock, SemanticButton};
use super::{PendingNotificationsMap, UserVarsMap};
use aterm_types::{ColorPalette, Rgb};
use std::collections::VecDeque;
use std::sync::Arc;

// Re-exports for TerminalHandler struct field types (#2157).
// These let handler.rs import via `super::` instead of reaching directly into
// external crate modules, reducing handler.rs fan-out from 11 to 5.
#[cfg(feature = "sixel")]
pub(super) use protocol::SixelState;
pub(super) use protocol::{DcsState, DcsType};

/// Grouped state for terminal marks and annotations.
///
/// Bundles marks (OSC 1337 SetMark) and annotations (OSC 1337 AddAnnotation)
/// with their ID counters. Accessed from `handler_osc.rs` and `shell_api.rs`.
pub(super) struct MarksState {
    /// User-created marks (OSC 1337 SetMark).
    pub(super) marks: VecDeque<TerminalMark>,
    /// Next mark ID to assign.
    pub(super) next_mark_id: u64,
    /// Annotations (OSC 1337 AddAnnotation).
    pub(super) annotations: VecDeque<Annotation>,
    /// Next annotation ID to assign.
    pub(super) next_annotation_id: u64,
}

impl MarksState {
    pub(super) fn new() -> Self {
        Self {
            marks: VecDeque::new(),
            next_mark_id: 0,
            annotations: VecDeque::new(),
            next_annotation_id: 0,
        }
    }
}

/// A snapshot of all color state saved by OSC 30001 (XTPUSHCOLORS).
///
/// Per the Kitty color stack protocol, push/pop must save and restore
/// the full set of dynamic colors — not just the 256-color palette.
/// This bundles palette, default foreground/background, cursor color,
/// and both selection colors into a single stack entry.
#[derive(Debug, Clone)]
pub(super) struct ColorStackEntry {
    /// The 256-color palette snapshot.
    pub(super) palette: ColorPalette,
    /// Default foreground color (OSC 10).
    pub(super) default_foreground: Rgb,
    /// Default background color (OSC 11).
    pub(super) default_background: Rgb,
    /// Cursor color (OSC 12). `None` = use foreground.
    pub(super) cursor_color: Option<Rgb>,
    /// Selection background color (OSC 21). `None` = renderer default.
    pub(super) selection_background: Option<Rgb>,
    /// Selection foreground color (OSC 19). `None` = renderer default.
    pub(super) selection_foreground: Option<Rgb>,
}

/// Grouped state for terminal color management.
///
/// Bundles the 256-color palette, default foreground/background colors,
/// cursor color, selection foreground/background, and the color stack used by
/// Kitty's OSC 30001/30101 push/pop protocol. Accessed from OSC handlers
/// (handler_osc.rs), SGR resolution (handler_sgr.rs), config API, and
/// the public colors API.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the three frame_*_authoritative flags are INDEPENDENT per-channel authority bits (fg, bg, cursor can each be host- or OSC-owned in any combination), so they are not a state machine an enum could model; bold_is_bright is an unrelated rendering flag"
)]
pub(super) struct ColorState {
    /// Maps 256 indexed colors to RGB values (OSC 4).
    pub(super) palette: ColorPalette,
    /// Saved color states for Kitty OSC 30001/30101 push/pop.
    pub(super) stack: VecDeque<ColorStackEntry>,
    /// Default foreground color (OSC 10, reset via OSC 110).
    pub(super) default_foreground: Rgb,
    /// Default background color (OSC 11, reset via OSC 111).
    pub(super) default_background: Rgb,
    /// Theme-configured foreground color, set by `apply_config`.
    ///
    /// OSC 110 resets `default_foreground` to this value (not the hardcoded
    /// constant). Updated whenever the host applies a new theme (#7443).
    pub(super) configured_foreground: Rgb,
    /// Whether a host setter/config or an OSC mutation made the foreground
    /// authoritative for render snapshots. A pristine `Terminal::new` retains
    /// the historical renderer-theme fallback until one of those boundaries.
    pub(super) frame_foreground_authoritative: bool,
    /// Theme-configured background color, set by `apply_config`.
    ///
    /// OSC 111 resets `default_background` to this value (not the hardcoded
    /// constant). Updated whenever the host applies a new theme (#7443).
    pub(super) configured_background: Rgb,
    /// Whether a host setter/config or an OSC mutation made the background
    /// authoritative for render snapshots.
    pub(super) frame_background_authoritative: bool,
    /// Theme-configured palette, set by `apply_config` when the theme
    /// provides `custom_palette`.
    ///
    /// OSC 104 (reset indexed colors) resets to this palette instead of
    /// hardcoded xterm defaults, matching the OSC 110/111 pattern for
    /// foreground/background. `None` means no theme palette was configured,
    /// so resets fall back to xterm defaults.
    pub(super) configured_palette: Option<ColorPalette>,
    /// Theme-configured cursor color, set by `apply_config`.
    ///
    /// OSC 112 and RIS restore this value. `None` means the configured
    /// fallback is the live default foreground.
    pub(super) configured_cursor: Option<Rgb>,
    /// Whether a host setter/config or an OSC mutation made the cursor policy
    /// authoritative for render snapshots. This is distinct from
    /// `cursor_color.is_some()`: OSC 21 `cursor=` deliberately selects an
    /// authoritative dynamic foreground fallback.
    pub(super) frame_cursor_authoritative: bool,
    /// Cursor color (OSC 12, reset via OSC 112). None = use foreground.
    pub(super) cursor_color: Option<Rgb>,
    /// Host-configured selection background.
    ///
    /// OSC 117 and RIS restore this value, preserving the same reset-to-theme
    /// contract as the configured foreground/background/cursor colors.
    pub(super) configured_selection_background: Option<Rgb>,
    /// Selection background color (OSC 21). None = renderer default.
    pub(super) selection_background: Option<Rgb>,
    /// Host-configured selection foreground.
    ///
    /// OSC 119 and RIS restore this value, mirroring the configured selection
    /// background. `None` delegates selected-text contrast to the renderer.
    pub(super) configured_selection_foreground: Option<Rgb>,
    /// Selection foreground color (OSC 19). None = renderer default.
    pub(super) selection_foreground: Option<Rgb>,
    /// Host style policy (W5f): SGR 1 promotes indexed 0–7 to bright 8–15.
    /// Config-level (like `configured_*`), so `reset()` leaves it alone.
    pub(super) bold_is_bright: bool,
    /// Host style policy (W5e): fraction of fg retained by SGR 2, blended
    /// toward the cell bg in linear light. Config-level; survives `reset()`.
    pub(super) faint_opacity: f32,
    /// Callback for dynamic color changes (OSC 10/11/12, OSC 110/111/112).
    pub(super) change_callback: Option<super::callbacks::ColorChangeCallback>,
    /// Callback for dynamic color queries (OSC 10/11/12 with `?`).
    ///
    /// When set, called before responding to color queries. If the callback
    /// returns `Some(Rgb)`, that color is used in the response instead of
    /// the palette color. Returns `None` to use the palette color.
    pub(super) query_callback: Option<super::callbacks::ColorQueryCallback>,
}

impl ColorState {
    pub(super) fn new() -> Self {
        Self {
            palette: ColorPalette::new(),
            stack: VecDeque::new(),
            default_foreground: DEFAULT_FOREGROUND,
            default_background: DEFAULT_BACKGROUND,
            configured_foreground: DEFAULT_FOREGROUND,
            frame_foreground_authoritative: false,
            configured_background: DEFAULT_BACKGROUND,
            frame_background_authoritative: false,
            configured_palette: None,
            configured_cursor: None,
            frame_cursor_authoritative: false,
            cursor_color: None,
            configured_selection_background: None,
            selection_background: None,
            configured_selection_foreground: None,
            selection_foreground: None,
            bold_is_bright: true,
            faint_opacity: aterm_types::DIM_FACTOR,
            change_callback: None,
            query_callback: None,
        }
    }

    /// Reset all color state to defaults (RIS / Terminal::reset).
    ///
    /// Restores palette, dynamic colors, cursor color, and color stack
    /// to their theme-configured values (not hardcoded constants).
    /// Preserves callbacks (they are transport-level registrations,
    /// not terminal state) and configured defaults (they are
    /// theme-level settings, not terminal state).
    pub(super) fn reset(&mut self) {
        if let Some(ref configured) = self.configured_palette {
            self.palette = configured.clone();
        } else {
            self.palette.reset();
        }
        self.stack.clear();
        // OSC 110/111 (and RIS/DECSTR) reset to the CONFIGURED default, never to a
        // transient state: a reverse-video swap (SGR 7 / DECSCNM) is applied at
        // RENDER time in color_resolve, so it neither survives nor is captured by this
        // reset — standard xterm semantics (reset-to-configured, not reset-to-current).
        self.default_foreground = self.configured_foreground;
        self.default_background = self.configured_background;
        self.cursor_color = self.configured_cursor;
        self.selection_background = self.configured_selection_background;
        self.selection_foreground = self.configured_selection_foreground;
    }
}

/// Tracks the last accepted OSC 133 shell integration marker (A/B/C/D).
///
/// Used to enforce the valid A→B→C→D state machine transition order.
/// Out-of-order markers are silently ignored, matching Terminal behavior (#7668).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ShellIntegrationPhase {
    /// No marker has been accepted yet (initial state, or after reset).
    #[default]
    None,
    /// Prompt start marker accepted (OSC 133 ; A).
    PromptStart,
    /// Command input start marker accepted (OSC 133 ; B).
    CommandStart,
    /// Command execution start marker accepted (OSC 133 ; C).
    CommandExec,
    /// Command finished marker accepted (OSC 133 ; D).
    CommandFinished,
}

/// Grouped state for shell integration (OSC 133/633).
///
/// Bundles the shell state machine, command marks, output blocks, and
/// the shell callback. Accessed from OSC 133/633 handlers, shell_api.rs,
/// blocks_api.rs, and semantic_api.rs.
pub(super) struct ShellIntegrationState {
    /// Shell integration state machine (OSC 133).
    pub(super) state: ShellState,
    /// Current phase in the A→B→C→D state machine.
    ///
    /// Enforces valid transitions: only `None`/`CommandFinished` → A,
    /// A → B, B → C, C → D. Out-of-order markers are silently ignored (#7668).
    pub(super) phase: ShellIntegrationPhase,
    /// Current command mark being built (OSC 133).
    pub(super) current_mark: Option<CommandMark>,
    /// Completed command marks (FIFO eviction via VecDeque).
    pub(super) command_marks: VecDeque<CommandMark>,
    /// Shell integration callback.
    pub(super) callback: Option<ShellCallback>,
    /// Output blocks (command+output units) for block-based model (FIFO eviction).
    pub(super) output_blocks: VecDeque<OutputBlock>,
    /// Current block being built (in progress).
    pub(super) current_block: Option<OutputBlock>,
    /// Next block ID to assign.
    pub(super) next_block_id: u64,
    /// Monotonic count of COMPLETED commands (OSC 133;D observed). Strictly
    /// increases per completion — the unambiguous "did a new command finish"
    /// probe (two completions in one millisecond share an end timestamp but
    /// never a seq). Preserved across `reset()` like `next_block_id`, so a
    /// reset can never replay an old value to observers.
    pub(super) completed_seq: u64,
    /// Session counter: number of times a block's command/output read found the
    /// block's rows already EVICTED from scrollback (DL-1 observability). Uses a
    /// `Cell` so the `&self` `block_output`/`block_command` accessors can record
    /// an eviction without taking `&mut self`.
    pub(super) eviction_reads: std::cell::Cell<u64>,
}

impl ShellIntegrationState {
    pub(super) fn new() -> Self {
        Self {
            state: ShellState::Ground,
            phase: ShellIntegrationPhase::None,
            current_mark: None,
            command_marks: VecDeque::new(),
            callback: None,
            output_blocks: VecDeque::new(),
            current_block: None,
            next_block_id: 0,
            completed_seq: 0,
            eviction_reads: std::cell::Cell::new(0),
        }
    }

    /// Reset data fields while preserving callback and block ID counter.
    pub(super) fn reset(&mut self) {
        self.state = ShellState::Ground;
        self.phase = ShellIntegrationPhase::None;
        self.current_mark = None;
        self.command_marks.clear();
        self.output_blocks.clear();
        self.current_block = None;
        // next_block_id is NOT reset so block IDs stay unique across resets
    }
}

/// Grouped cursor save/restore state for DECSC/DECRC and mode 1049 screen switches.
///
/// Bundles the per-buffer cursor save slots — one per screen, exactly like
/// xterm's `screen->sc[whichBuf]`. DECSC/DECRC, SCOSC/SCORC, mode 1048, and
/// mode 1049 all share these slots (xterm CursorSave/CursorRestore), so a
/// 1049 round trip leaves the slot saved for a later bare DECRC.
pub(super) struct CursorSaveState {
    /// Saved cursor state for main screen (xterm sc[0]).
    pub(super) main: Option<SavedCursorState>,
    /// Saved cursor state for alt screen (xterm sc[1]).
    pub(super) alt: Option<SavedCursorState>,
}

impl CursorSaveState {
    pub(super) fn new() -> Self {
        Self {
            main: None,
            alt: None,
        }
    }

    /// Reset all saved cursor positions (called during terminal reset).
    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }
}

/// Grouped title/icon state and callback.
///
/// Bundles window title (OSC 0/2), icon name (OSC 0/1), title callback, and
/// title stack state for XTWINOPS push/pop operations.
pub(super) struct TitleState {
    /// Window title (OSC 0 or OSC 2).
    pub(super) window: Arc<str>,
    /// Icon name (OSC 0 or OSC 1).
    pub(super) icon: Arc<str>,
    /// Title change callback (invoked for window title updates, legacy v2).
    pub(super) callback: Option<callbacks::TitleCallback>,
    /// Title event callback with type discriminator (v3).
    ///
    /// Fires for all OSC 0/1/2 title changes with the title type.
    pub(super) event_callback: Option<callbacks::TitleEventCallback>,
    /// Title stack for CSI 22/23 t push/pop operations.
    ///
    /// Stores (icon_name, window_title) pairs. Capped at `TITLE_STACK_MAX_DEPTH`
    /// to prevent unbounded memory growth from malicious sequences.
    pub(super) stack: Vec<(Arc<str>, Arc<str>)>,
    /// Monotonic counter bumped each time a tab-LABEL value actually changes
    /// (not on no-op re-sets): the window title, or the shell-reported cwd
    /// (OSC 7 / OSC 633 `P;Cwd=` — shown by the GUI when no title is set).
    /// Lets the host UI (e.g. the GUI tab strip) detect a tab's title/cwd
    /// drift with a lock-free `Relaxed` atomic load instead of grabbing a
    /// terminal lock per output burst. See
    /// [`Terminal::title_epoch`](super::Terminal::title_epoch).
    pub(super) epoch: std::sync::atomic::AtomicU64,
}

impl TitleState {
    pub(super) fn new() -> Self {
        Self {
            window: Arc::from(""),
            icon: Arc::from(""),
            callback: None,
            event_callback: None,
            stack: Vec::new(),
            epoch: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Reset title data while preserving callback (xterm clears titles on RIS).
    ///
    /// Fires the title callback so the host UI updates its titlebar.
    pub(super) fn reset(&mut self) {
        // Clearing a non-empty window title is a real title change; bump the
        // change epoch so a host UI observing a background tab sees the RIS
        // clear via a lock-free `title_epoch()` load (no bump if already empty).
        if !self.window.is_empty() {
            self.epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.window = Arc::from("");
        self.icon = Arc::from("");
        self.stack.clear();
        // Notify host that the title was cleared.
        if let Some(ref mut callback) = self.callback {
            callback(&self.window);
        }
        if let Some(ref mut callback) = self.event_callback {
            callback(aterm_types::TitleType::WindowAndIcon, &self.window);
        }
    }
}

/// Grouped notification callback and accumulation state.
///
/// Bundles simple notifications (OSC 9), advanced/kitty notifications (OSC 99),
/// and the pending notification map for multi-part accumulation.
pub(super) struct NotificationState {
    /// Simple desktop notification callback (OSC 9).
    pub(super) callback: Option<NotificationCallback>,
    /// Advanced desktop notification callback (OSC 99 - kitty protocol).
    pub(super) advanced_callback: Option<AdvancedNotificationCallback>,
    /// In-progress notifications being built from OSC 99 chunks.
    pub(super) pending: PendingNotificationsMap,
    /// Counter for generating unique anonymous notification IDs.
    /// Prevents collisions when multiple anonymous multi-part notifications
    /// are interleaved.
    pub(super) anon_counter: u32,
}

// NOTE: the OSC 9/99/777 notification rate-limiter (`check_rate_limit`,
// `MAX_NOTIFICATIONS_PER_WINDOW`, `NOTIFICATION_WINDOW`, #7138) was deleted
// here. The OSC 9/99/777 handlers (`handler_osc_notify.rs`) are gated by the
// host's `modes.allow_notifications` authorization; the multi-part pending
// map is bounded by `MAX_PENDING_NOTIFICATIONS`, so an unauthorized or flood
// path cannot reach a callback or grow unbounded. A per-window callback-rate
// limiter could be reintroduced if a host needs it; see git history for the
// original implementation.

impl NotificationState {
    pub(super) fn new() -> Self {
        Self {
            callback: None,
            advanced_callback: None,
            pending: PendingNotificationsMap::default(),
            anon_counter: 0,
        }
    }

    /// Reset pending notifications while preserving callbacks.
    pub(super) fn reset(&mut self) {
        self.pending.clear();
        self.anon_counter = 0;
    }
}

/// Grouped clipboard callback and copy-capture state.
///
/// Bundles OSC 52 clipboard callback with Terminal OSC 1337 CopyToClipboard state.
pub(super) struct ClipboardState {
    /// Clipboard callback for OSC 52 operations.
    pub(super) callback: Option<ClipboardCallback>,
    /// Callback for OSC 1337 named pasteboard operations.
    pub(super) copy_callback: Option<CopyToClipboardCallback>,
    /// State for CopyToClipboard text capture mode.
    pub(super) copy_state: Option<CopyToClipboardState>,
}

impl ClipboardState {
    pub(super) fn new() -> Self {
        Self {
            callback: None,
            copy_callback: None,
            copy_state: None,
        }
    }

    /// Reset in-progress copy capture while preserving callbacks.
    pub(super) fn reset(&mut self) {
        self.copy_state = None;
    }
}

/// Grouped state for Terminal OSC 1337 protocol extensions.
///
/// Bundles Terminal-specific callbacks and state: profile, badge, colors,
/// cursor line highlight, variable reporting, cell size, shell integration
/// version, remote host, and user variables.
// The OSC 1337 callback fields below are registered and invoked via the FFI
// app-callback layer (ffi_bridge/app_callbacks); they are inert (never read) in
// the default lib build, where no callback consumer is compiled.
#[allow(
    dead_code,
    reason = "OSC 1337 callback registry consumed by the FFI app-callback layer"
)]
pub(super) struct Iterm2State {
    /// Callback for OSC 1337 SetProfile requests.
    pub(super) set_profile_callback: Option<callbacks::SetProfileCallback>,
    /// Callback for OSC 1337 SetBadgeFormat requests.
    pub(super) set_badge_format_callback: Option<callbacks::SetBadgeFormatCallback>,
    /// Callback for OSC 1337 SetColors requests.
    pub(super) set_colors_callback: Option<callbacks::SetColorsCallback>,
    /// Callback for OSC 1337 HighlightCursorLine requests.
    pub(super) highlight_cursor_line_callback: Option<callbacks::HighlightCursorLineCallback>,
    /// Callback for OSC 1337 ReportVariable queries.
    pub(super) report_variable_callback: Option<callbacks::ReportVariableCallback>,
    /// Callback for OSC 1337 ReportCellSize queries.
    pub(super) report_cell_size_callback: Option<callbacks::ReportCellSizeCallback>,
    /// Callback for OSC 1337 ShellIntegrationVersion reports.
    pub(super) shell_integration_version_callback:
        Option<callbacks::ShellIntegrationVersionCallback>,
    /// User variables (OSC 1337 SetUserVar).
    pub(super) user_vars: UserVarsMap,
    /// Insertion order of `user_vars` keys, for deterministic oldest-first
    /// eviction when the map is at capacity. `UserVarsMap` is a `HashMap` whose
    /// iteration order is non-deterministic, so eviction cannot rely on it; this
    /// queue records first-insertion order (re-inserting an existing key does
    /// not change its position). See `set_user_var` in `shell_api.rs`.
    pub(super) user_vars_order: std::collections::VecDeque<String>,
    /// Cursor line highlight state (OSC 1337 HighlightCursorLine).
    ///
    /// `None` means no command has been received yet.
    pub(super) highlight_cursor_line: Option<bool>,
    /// Last reported shell integration version (OSC 1337 ShellIntegrationVersion).
    pub(super) shell_integration_version: Option<super::types::Iterm2ShellIntegrationVersion>,
    /// Current remote host (OSC 1337 RemoteHost).
    pub(super) remote_host: Option<super::types::RemoteHost>,
    /// Callback for remote host change events.
    pub(super) remote_host_callback: Option<RemoteHostCallback>,
    /// Generic KVP callback for all OSC 1337 commands (first-refusal interceptor).
    pub(super) kvp_callback: Option<callbacks::KvpCallback>,
    /// Cell pixel size `(width, height)` used to convert pixel/auto image
    /// dimensions (OSC 1337 `File=`) into a CELL footprint. The frontend sets it
    /// from its real font metrics via [`Terminal::set_cell_pixel_size`]; the
    /// default ([`DEFAULT_CELL_PX`]) keeps the engine self-contained for headless
    /// tests. Cell-unit dimensions (`width=10`) never consult this.
    pub(super) cell_px: (u16, u16),
    /// Whether `cell_px` is a MEASUREMENT (a host called
    /// [`Terminal::set_cell_pixel_size`]) rather than [`DEFAULT_CELL_PX`].
    ///
    /// The placeholder is a fine convenience for footprint arithmetic, which
    /// has to produce *some* cell count either way. It is NOT an acceptable
    /// answer to a question — an XTWINOPS pixel report (CSI 14 t / 16 t) that
    /// hands back 8x16 tells a sixel or image-capable TUI to lay its output out
    /// on a font that does not exist. Those reports consult
    /// [`Terminal::host_cell_pixel_size`] instead and stay SILENT while this is
    /// false, which is the honest answer for a headless engine or a session
    /// whose renderer has not finished building.
    pub(super) cell_px_from_host: bool,
}

/// Default cell pixel size used for inline-image footprint math before the
/// frontend reports real metrics. A typical 80×24 terminal at common DPI; only
/// affects pixel/auto-sized images, never cell-unit (`width=N`) ones.
pub(super) const DEFAULT_CELL_PX: (u16, u16) = (8, 16);

impl Iterm2State {
    pub(super) fn new() -> Self {
        Self {
            set_profile_callback: None,
            set_badge_format_callback: None,
            set_colors_callback: None,
            highlight_cursor_line_callback: None,
            report_variable_callback: None,
            report_cell_size_callback: None,
            shell_integration_version_callback: None,
            user_vars: UserVarsMap::default(),
            user_vars_order: std::collections::VecDeque::new(),
            highlight_cursor_line: None,
            shell_integration_version: None,
            remote_host: None,
            remote_host_callback: None,
            kvp_callback: None,
            cell_px: DEFAULT_CELL_PX,
            cell_px_from_host: false,
        }
    }

    /// The cell box a HOST measured, or `None` for the placeholder.
    ///
    /// The single predicate behind [`Terminal::host_cell_pixel_size`] and the
    /// XTWINOPS pixel reports, so "is this a measurement?" is answered in one
    /// place. A zero axis counts as unreported: no font has a zero-wide cell,
    /// and reporting one tells the application to divide by zero.
    pub(super) fn host_cell_px(&self) -> Option<(u16, u16)> {
        let (w, h) = self.cell_px;
        (self.cell_px_from_host && w > 0 && h > 0).then_some((w, h))
    }

    /// Reset data fields while preserving callbacks.
    pub(super) fn reset(&mut self) {
        self.remote_host = None;
        self.highlight_cursor_line = None;
        self.shell_integration_version = None;
        self.user_vars.clear();
        self.user_vars_order.clear();
    }
}

/// Semantic block storage.
type SemanticBlockMap = std::collections::HashMap<String, SemanticBlock>;

/// Grouped state for semantic blocks/buttons and callbacks (OSC 1337).
///
/// Bundles semantic code blocks, semantic buttons, and their callbacks.
/// Accessed from OSC 1337 handlers, semantic API accessors, and callback setters.
pub(super) struct SemanticState {
    /// Semantic code blocks (OSC 1337 Block).
    /// Maps block ID to block data. Open blocks are tracked until closed.
    pub(super) blocks: SemanticBlockMap,
    /// Semantic buttons (OSC 1337 Button).
    /// Buttons attached to terminal content for copy or custom actions.
    /// Uses VecDeque for O(1) FIFO eviction at capacity (vs O(n) Vec::remove(0)).
    pub(super) buttons: std::collections::VecDeque<SemanticButton>,
    /// Callback for semantic block events.
    #[allow(
        dead_code,
        reason = "registered/invoked via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(super) block_callback: Option<SemanticBlockCallback>,
    /// Callback for semantic button events.
    #[allow(
        dead_code,
        reason = "registered/invoked via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(super) button_callback: Option<SemanticButtonCallback>,
}

impl SemanticState {
    pub(super) fn new() -> Self {
        Self {
            blocks: SemanticBlockMap::new(),
            buttons: std::collections::VecDeque::new(),
            block_callback: None,
            button_callback: None,
        }
    }

    /// Reset data fields while preserving callbacks.
    pub(super) fn reset(&mut self) {
        self.blocks.clear();
        self.buttons.clear();
    }
}

/// Grouped BiDi (bidirectional text) state.
///
/// Holds the BiDi configuration only — the resolver and render cache live
/// behind the permanently compiled-out `aterm-bidi` integration.
pub(super) struct BiDiGroupState {
    /// BiDi configuration (mode, direction, alternate screen, etc.).
    pub(super) config: crate::config::BiDiConfig,
    /// Reusable per-row scratch buffers for the render-time visual reorder
    /// (feature `bidi`). Held here so their capacity persists across the
    /// `apply_bidi_reorder` row loop AND across frames.
    #[cfg(feature = "bidi")]
    pub(super) scratch: BidiScratch,
}

impl BiDiGroupState {
    pub(super) fn new() -> Self {
        Self {
            config: crate::config::BiDiConfig::default(),
            #[cfg(feature = "bidi")]
            scratch: BidiScratch::new(),
        }
    }
}

/// Reusable per-row scratch for BiDi visual reordering (feature `bidi`).
///
/// The hot `Terminal::apply_bidi_reorder` loop clears + refills these each RTL
/// row instead of allocating fresh `Vec`s, so a stable terminal size reorders
/// without per-row/per-frame allocation. Output stays byte-identical.
#[cfg(feature = "bidi")]
pub(super) struct BidiScratch {
    /// Per-cell BiDi classes for the current row (computed once per row).
    pub(super) classes: Vec<aterm_bidi::BidiClass>,
    /// Per-cell wide-continuation flags for the current row.
    pub(super) wide: Vec<bool>,
    /// Inverse permutation: `inv[logical] = visual column`.
    pub(super) inv: Vec<usize>,
    /// Holds the reordered (visual-order) row while it is swapped into the frame.
    pub(super) row_tmp: Vec<crate::terminal::RenderCell>,
    /// Working buffers threaded into [`aterm_bidi::reorder_cells_with_classes_into`]
    /// so the per-row UAX #9 resolution reuses these instead of heap-allocating ~7
    /// transient `Vec`s per RTL row per frame. `cell_order` holds the resulting
    /// visual→logical CELL permutation; the rest are inner scratch (logical-char
    /// classes, lead-cell map, wide-pair flags, resolved types, embedding levels,
    /// and the logical-char order).
    pub(super) logical: Vec<aterm_bidi::BidiClass>,
    pub(super) lead_cell: Vec<usize>,
    pub(super) has_cont: Vec<bool>,
    pub(super) types: Vec<aterm_bidi::BidiClass>,
    pub(super) levels: Vec<u8>,
    pub(super) char_order: Vec<usize>,
    pub(super) cell_order: Vec<usize>,
}

#[cfg(feature = "bidi")]
impl BidiScratch {
    pub(super) fn new() -> Self {
        Self {
            classes: Vec::new(),
            wide: Vec::new(),
            inv: Vec::new(),
            row_tmp: Vec::new(),
            logical: Vec::new(),
            lead_cell: Vec::new(),
            has_cont: Vec::new(),
            types: Vec::new(),
            levels: Vec::new(),
            char_order: Vec::new(),
            cell_order: Vec::new(),
        }
    }
}
