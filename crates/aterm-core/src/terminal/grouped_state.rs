// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grouped sub-state structs for [`Terminal`](super::Terminal), each bundling
//! fields passed together to
//! [`TerminalHandler`](super::handler::TerminalHandler) during processing.

mod protocol;

use super::callbacks::{AdvancedNotificationCallback, ClipboardCallback, NotificationCallback};
use super::shell::{Annotation, CommandMark, OutputBlock, ShellState, TerminalMark};
use super::transient_state::{DEFAULT_BACKGROUND, DEFAULT_FOREGROUND};
use super::types::SavedCursorState;
use super::{PendingNotificationsMap, UserVarsMap};
use aterm_types::{ColorPalette, Rgb};
use std::collections::VecDeque;
use std::sync::Arc;

// Re-exports for TerminalHandler field types, so handler.rs imports via
// `super::` rather than reaching into external crates.
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

/// A snapshot of all color state saved by OSC 30001 (XTPUSHCOLORS). Per the
/// Kitty color stack protocol, push/pop covers every dynamic color: palette,
/// default fg/bg, cursor and both selection colors.
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

/// Grouped color state: the 256-color palette, default fg/bg, cursor color,
/// selection fg/bg, and the Kitty OSC 30001/30101 color stack.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-channel authority bits (fg, bg, cursor) plus an unrelated rendering flag, not a state machine"
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
    /// Theme-configured foreground color, set by `apply_config`; OSC 110
    /// resets `default_foreground` to it.
    pub(super) configured_foreground: Rgb,
    /// Whether a host setter/config or an OSC mutation made the foreground
    /// authoritative for render snapshots. A fresh `Terminal::new` leaves it
    /// false, so the renderer's theme applies.
    pub(super) frame_foreground_authoritative: bool,
    /// Theme-configured background color, set by `apply_config`; OSC 111
    /// resets `default_background` to it.
    pub(super) configured_background: Rgb,
    /// Whether a host setter/config or an OSC mutation made the background
    /// authoritative for render snapshots.
    pub(super) frame_background_authoritative: bool,
    /// Theme-configured palette, set by `apply_config` when the theme provides
    /// `custom_palette`. OSC 104 resets to it; `None` resets to xterm defaults.
    pub(super) configured_palette: Option<ColorPalette>,
    /// Theme-configured cursor color, set by `apply_config`, restored by OSC
    /// 112 and RIS. `None` falls back to the live default foreground.
    pub(super) configured_cursor: Option<Rgb>,
    /// Whether a host setter/config or an OSC mutation made the cursor policy
    /// authoritative for render snapshots. Distinct from
    /// `cursor_color.is_some()`: OSC 21 `cursor=` selects an authoritative
    /// dynamic foreground fallback.
    pub(super) frame_cursor_authoritative: bool,
    /// Cursor color (OSC 12, reset via OSC 112). None = use foreground.
    pub(super) cursor_color: Option<Rgb>,
    /// Host-configured selection background, restored by OSC 117 and RIS.
    pub(super) configured_selection_background: Option<Rgb>,
    /// Selection background color (OSC 21). None = renderer default.
    pub(super) selection_background: Option<Rgb>,
    /// Host-configured selection foreground, restored by OSC 119 and RIS.
    /// `None` leaves selected-text contrast to the renderer.
    pub(super) configured_selection_foreground: Option<Rgb>,
    /// Selection foreground color (OSC 19). None = renderer default.
    pub(super) selection_foreground: Option<Rgb>,
    /// Host style policy: SGR 1 promotes indexed 0–7 to bright 8–15.
    /// Config-level (like `configured_*`), so `reset()` leaves it alone.
    pub(super) bold_is_bright: bool,
    /// Host style policy: fraction of fg retained by SGR 2, blended
    /// toward the cell bg in linear light. Config-level; survives `reset()`.
    pub(super) faint_opacity: f32,
    /// Callback for dynamic color changes (OSC 10/11/12, OSC 110/111/112).
    pub(super) change_callback: Option<super::callbacks::ColorChangeCallback>,
    /// Callback for dynamic color queries (OSC 10/11/12 with `?`). A
    /// `Some(Rgb)` result replaces the palette color in the response.
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

    /// Reset all color state to the theme-configured values (RIS /
    /// Terminal::reset): palette, dynamic colors, cursor color and color stack.
    /// Callbacks and configured defaults are kept (they are not terminal
    /// state).
    pub(super) fn reset(&mut self) {
        if let Some(ref configured) = self.configured_palette {
            self.palette = configured.clone();
        } else {
            self.palette.reset();
        }
        self.stack.clear();
        // Reset to the configured default, never a transient state: reverse
        // video (SGR 7 / DECSCNM) is applied at render time in color_resolve, so
        // it is neither captured nor preserved here (xterm semantics).
        self.default_foreground = self.configured_foreground;
        self.default_background = self.configured_background;
        self.cursor_color = self.configured_cursor;
        self.selection_background = self.configured_selection_background;
        self.selection_foreground = self.configured_selection_foreground;
    }
}

/// The last accepted OSC 133 shell integration marker (A/B/C/D), enforcing
/// the A→B→C→D order; out-of-order markers are ignored.
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
/// Bundles the shell state machine, command marks and output blocks.
/// Accessed from OSC 133/633 handlers, shell_api.rs and blocks_api.rs.
pub(super) struct ShellIntegrationState {
    /// Shell integration state machine (OSC 133).
    pub(super) state: ShellState,
    /// Current phase in the A→B→C→D state machine: only `None`/
    /// `CommandFinished` → A, A → B, B → C, C → D; out-of-order markers are
    /// ignored.
    pub(super) phase: ShellIntegrationPhase,
    /// Current command mark being built (OSC 133).
    pub(super) current_mark: Option<CommandMark>,
    /// Completed command marks (FIFO eviction via VecDeque).
    pub(super) command_marks: VecDeque<CommandMark>,
    /// Output blocks (command+output units) for block-based model (FIFO eviction).
    pub(super) output_blocks: VecDeque<OutputBlock>,
    /// Current block being built (in progress).
    pub(super) current_block: Option<OutputBlock>,
    /// Next block ID to assign.
    pub(super) next_block_id: u64,
    /// Monotonic count of completed commands (OSC 133;D). Unlike end
    /// timestamps it never repeats, so it answers "did a new command finish".
    /// Preserved across `reset()` like `next_block_id`.
    pub(super) completed_seq: u64,
    /// Number of block command/output reads that found the block's rows
    /// already evicted from scrollback. A `Cell` so the `&self` accessors can
    /// count.
    pub(super) eviction_reads: std::cell::Cell<u64>,
    /// The newest block whose evicted COMMAND, and whose evicted OUTPUT, a
    /// read has found (`blocks_api::BlockSide`'s order). An evicted block
    /// stays evicted, and so is every older one (ids and rows only grow), so a
    /// read of a block no newer than these is counted but is not news.
    pub(super) eviction_seen: [std::cell::Cell<Option<u64>>; 2],
    /// How many times a read found a NEWER block's side evicted (the news
    /// `eviction_seen` admits). Only the 1st, 2nd, 4th … of them is logged: a
    /// program can start blocks at will.
    pub(super) eviction_news: std::cell::Cell<u64>,
    /// Which integration body the shell runs: the 16 lowercase hex digits of
    /// its script folder, as the body signs before every prompt
    /// (`633;P;AtermIntegration=<rev>;id=<nonce>`). Recorded only from a mark
    /// that passed the nonce gate; `None` until the first. Kept across
    /// `reset()` (RIS does not change the shell's script) and carried across a
    /// seamless update like the nonce (`TerminalCheckpoint::shell_integration_rev`).
    pub(super) integration_rev: Option<[u8; 16]>,
}

impl ShellIntegrationState {
    pub(super) fn new() -> Self {
        Self {
            state: ShellState::Ground,
            phase: ShellIntegrationPhase::None,
            current_mark: None,
            command_marks: VecDeque::new(),
            output_blocks: VecDeque::new(),
            current_block: None,
            next_block_id: 0,
            completed_seq: 0,
            eviction_reads: std::cell::Cell::new(0),
            eviction_seen: [const { std::cell::Cell::new(None) }; 2],
            eviction_news: std::cell::Cell::new(0),
            integration_rev: None,
        }
    }

    /// Reset data fields while preserving the block ID counter.
    pub(super) fn reset(&mut self) {
        self.state = ShellState::Ground;
        self.phase = ShellIntegrationPhase::None;
        self.current_mark = None;
        self.command_marks.clear();
        self.output_blocks.clear();
        self.current_block = None;
        // next_block_id is not reset so block IDs stay unique across resets
    }
}

/// Grouped cursor save/restore state: one save slot per screen, like xterm's
/// `screen->sc[whichBuf]`. DECSC/DECRC, SCOSC/SCORC, mode 1048 and mode 1049
/// share these slots, so a 1049 round trip leaves the slot saved for a later
/// bare DECRC.
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

/// Window title (OSC 0/2), icon name (OSC 0/1), the title-change epoch and
/// the XTWINOPS title stack.
pub(super) struct TitleState {
    /// Window title (OSC 0 or OSC 2).
    pub(super) window: Arc<str>,
    /// Icon name (OSC 0 or OSC 1).
    pub(super) icon: Arc<str>,
    /// Title stack for CSI 22/23 t: (icon_name, window_title) pairs, capped at
    /// `TITLE_STACK_MAX_DEPTH`.
    pub(super) stack: Vec<(Arc<str>, Arc<str>)>,
    /// Monotonic counter bumped each time a tab-label value changes (not on
    /// no-op re-sets): the window title, or the shell-reported cwd (OSC 7 / OSC
    /// 633 `P;Cwd=`) shown when no title is set. Hosts read it with a
    /// lock-free `Relaxed` load. See
    /// [`Terminal::title_epoch`](super::Terminal::title_epoch).
    pub(super) epoch: std::sync::atomic::AtomicU64,
}

impl TitleState {
    pub(super) fn new() -> Self {
        Self {
            window: Arc::from(""),
            icon: Arc::from(""),
            stack: Vec::new(),
            epoch: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Reset title data (xterm clears titles on RIS). Hosts see the clear
    /// through [`Terminal::title_epoch`](super::Terminal::title_epoch).
    pub(super) fn reset(&mut self) {
        // Clearing a non-empty window title is a real title change: bump the
        // epoch so hosts see the RIS clear.
        if !self.window.is_empty() {
            self.epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.window = Arc::from("");
        self.icon = Arc::from("");
        self.stack.clear();
    }
}

/// Notification callbacks (OSC 9, OSC 99/777) and the OSC 99 multi-part
/// accumulation map.
pub(super) struct NotificationState {
    /// Simple desktop notification callback (OSC 9).
    pub(super) callback: Option<NotificationCallback>,
    /// Advanced desktop notification callback (OSC 99 - kitty protocol).
    pub(super) advanced_callback: Option<AdvancedNotificationCallback>,
    /// In-progress notifications being built from OSC 99 chunks.
    pub(super) pending: PendingNotificationsMap,
}

// OSC 9/99/777 notifications are gated by `modes.allow_notifications`, and the
// multi-part pending map is bounded by `MAX_PENDING_NOTIFICATIONS`; there is no
// separate callback-rate limiter.

impl NotificationState {
    pub(super) fn new() -> Self {
        Self {
            callback: None,
            advanced_callback: None,
            pending: PendingNotificationsMap::default(),
        }
    }

    /// Reset pending notifications while preserving callbacks.
    pub(super) fn reset(&mut self) {
        self.pending.clear();
    }
}

/// The OSC 52 clipboard callback.
pub(super) struct ClipboardState {
    /// Clipboard callback for OSC 52 operations.
    pub(super) callback: Option<ClipboardCallback>,
}

impl ClipboardState {
    pub(super) fn new() -> Self {
        Self { callback: None }
    }
}

/// Grouped state for Terminal OSC 1337 protocol extensions.
///
/// Bundles the OSC 1337 state the engine keeps: user variables, cursor line
/// highlight, shell integration version, remote host, and the cell pixel size.
pub(super) struct Iterm2State {
    /// User variables (OSC 1337 SetUserVar).
    pub(super) user_vars: UserVarsMap,
    /// First-insertion order of `user_vars` keys, for deterministic
    /// oldest-first eviction at capacity (`HashMap` iteration order is not
    /// deterministic). Re-inserting a key keeps its position. See
    /// `set_user_var` in `shell_api.rs`.
    pub(super) user_vars_order: std::collections::VecDeque<String>,
    /// Cursor line highlight state (OSC 1337 HighlightCursorLine); `None`
    /// until a command is received.
    pub(super) highlight_cursor_line: Option<bool>,
    /// Last reported shell integration version (OSC 1337 ShellIntegrationVersion).
    pub(super) shell_integration_version: Option<super::types::Iterm2ShellIntegrationVersion>,
    /// Current remote host (OSC 1337 RemoteHost).
    pub(super) remote_host: Option<super::types::RemoteHost>,
    /// Cell pixel size `(width, height)` for converting pixel/auto image
    /// dimensions (OSC 1337 `File=`) into a cell footprint. Set from the
    /// frontend's font metrics via [`Terminal::set_cell_pixel_size`];
    /// [`DEFAULT_CELL_PX`] otherwise. Cell-unit dimensions ignore it.
    pub(super) cell_px: (u16, u16),
    /// Whether `cell_px` was measured by a host
    /// ([`Terminal::set_cell_pixel_size`]) rather than [`DEFAULT_CELL_PX`]. The
    /// placeholder is fine for footprint arithmetic but not as an answer: the
    /// XTWINOPS pixel reports (CSI 14 t / 16 t) read
    /// [`Terminal::host_cell_pixel_size`] and stay silent while this is false.
    pub(super) cell_px_from_host: bool,
}

/// Default cell pixel size for inline-image footprint math before the frontend
/// reports real metrics; affects only pixel/auto-sized images.
pub(super) const DEFAULT_CELL_PX: (u16, u16) = (8, 16);

impl Iterm2State {
    pub(super) fn new() -> Self {
        Self {
            user_vars: UserVarsMap::default(),
            user_vars_order: std::collections::VecDeque::new(),
            highlight_cursor_line: None,
            shell_integration_version: None,
            remote_host: None,
            cell_px: DEFAULT_CELL_PX,
            cell_px_from_host: false,
        }
    }

    /// The cell box a host measured, or `None` for the placeholder: the one
    /// predicate behind [`Terminal::host_cell_pixel_size`] and the XTWINOPS
    /// pixel reports. A zero axis counts as unreported.
    pub(super) fn host_cell_px(&self) -> Option<(u16, u16)> {
        let (w, h) = self.cell_px;
        (self.cell_px_from_host && w > 0 && h > 0).then_some((w, h))
    }

    /// Reset the program-set fields; the host's cell size survives.
    pub(super) fn reset(&mut self) {
        self.remote_host = None;
        self.highlight_cursor_line = None;
        self.shell_integration_version = None;
        self.user_vars.clear();
        self.user_vars_order.clear();
    }
}

/// Grouped BiDi (bidirectional text) state: the configuration and, with
/// feature `bidi`, the reorder scratch.
pub(super) struct BiDiGroupState {
    /// BiDi configuration (mode, direction, alternate screen, etc.).
    pub(super) config: crate::config::BiDiConfig,
    /// Reusable per-row scratch buffers for the render-time visual reorder
    /// (feature `bidi`), kept so their capacity persists across rows and frames.
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

/// Reusable per-row scratch for BiDi visual reordering (feature `bidi`):
/// `Terminal::apply_bidi_reorder` clears and refills these for each RTL row
/// instead of allocating.
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
    /// Per-cell paired-bracket properties for the current row (UAX #9 N0).
    pub(super) brackets: Vec<aterm_bidi::Bracket>,
    /// UAX #9 working memory for
    /// [`aterm_bidi::reorder_cells_with_classes_into`], reused across rows.
    pub(super) bidi: aterm_bidi::Scratch,
    /// The resulting visual→logical CELL permutation.
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
            brackets: Vec::new(),
            bidi: aterm_bidi::Scratch::default(),
            cell_order: Vec::new(),
        }
    }
}
