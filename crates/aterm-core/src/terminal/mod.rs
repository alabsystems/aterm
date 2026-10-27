// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal emulator: the parser driving the grid.

mod alt_archive;
/// Tier-1 conformance of the alt-screen selection park to `AltSelectionPark`.
#[cfg(test)]
mod alt_selection_park_conformance;
mod bidi_reorder;
mod bidi_stubs;
mod blocks_api;
mod buffer_api;
mod builder;
mod callback_setters;
mod callbacks;
mod checkpoint;
mod checkpoint_canon;
mod checkpoint_holder;
mod checkpoint_output;
#[cfg(feature = "serde")]
mod checkpoint_parser;
mod checkpoint_state;
pub(crate) mod clipboard_auth;
pub mod color_resolve;
mod colors_api;
mod config_api;
mod constructors;
mod content;
mod csi_dispatch_table;
pub mod custody;
mod foreground_handback;
mod frame_watchers;
mod grouped_state;
mod handler;
mod handler_dec;
mod handler_decrqss;
mod handler_osc;
mod handler_osc_1337;
mod handler_osc_color;
mod handler_osc_notify;
mod handler_osc_shell;
mod handler_report;
mod handler_sgr;
mod handler_state;
mod handler_window;
mod handler_xtgettcap;
mod handler_xtsmgraphics;
mod history_carry;
mod holder_fence;
/// Tier-1 conformance of the holder fence to `HolderFence`.
#[cfg(test)]
mod holder_fence_conformance;
mod host_journal;
pub(crate) mod host_traits;
pub(crate) mod hyperlink_auth;
mod image_reflow;
mod keyboard_mode;
mod kitty_auth;
mod kitty_floating;
/// Kitty graphics protocol (APC `G`) command parser.
pub mod kitty_graphics;
mod kitty_placeholder;
mod mode_mirror;
#[cfg(test)]
pub mod mouse;
#[cfg(not(test))]
mod mouse;
mod notify_auth;
mod observe;
mod policy_bridge;
mod policy_gates;
mod processing;
pub mod program_state;
mod render_cells;
pub mod repaint_nudge;
mod reset;
mod resize_journal;
mod response_capability;
mod response_rate_limiter;
mod selection;
mod serialize_ansi;
mod shell;
pub(crate) mod shell_integration_auth;
mod window_auth;

/// Non-Copy, non-Default seal field for capability tokens.
#[derive(Debug)]
struct Seal;

mod shell_api;

pub mod scrollback_shared_budget;
mod search_budgeted;
mod search_index;
mod search_plan;
mod search_query;
mod search_rows;
mod stack_response;
mod state;
mod state_accessors;
#[cfg(test)]
mod terminal_modes_conformance;
mod transient_state;
mod types;

pub use callbacks::ColorTarget;
use callbacks::{
    MAX_DCS_GLOBAL_BUDGET, SGR_STACK_MAX_DEPTH, TITLE_STACK_MAX_DEPTH, WindowCallback,
};
pub use config_api::{ColorBaseline, ColorRebase};
use grouped_state::SixelState;
use grouped_state::{
    ClipboardState, ColorState, CursorSaveState, DcsState, DcsType, Iterm2State, MarksState,
    NotificationState, ShellIntegrationState, TitleState,
};
use reset::{ResetGroups, reset_common_fields};
pub use state_accessors::ColorSchemeSource;
use transient_state::{TransientState, Vt52CursorState};

pub use alt_archive::{
    ALT_ARCHIVE_DEFAULT_BUDGET, ALT_ARCHIVE_MAX_ROWS, ALT_ARCHIVE_MIN_SHARE,
    ALT_ARCHIVE_ROW_OVERHEAD, ALT_ARCHIVE_TOTAL_BUDGET, AltArchive, AltArchiveBudget,
    AltArchiveCarry, AltArchiveDiffer, AltArchiveFence, AltArchiveGap, AltArchiveGapKind,
    AltArchiveImport, AltArchiveQuery, AltArchiveRead, alt_archive_row_charge,
};
pub use aterm_parser::{CarryRefusal, HookedDcsCarry, ParserCarry};
pub use aterm_types::XtermKeyboardState;
pub(crate) use aterm_types::charset::CharacterSetState;
pub use aterm_types::{ColorPalette, Rgb};
pub use aterm_types::{KittyKeyboardFlags, KittyKeyboardState};
pub use aterm_types::{WindowOperation, WindowResponse};
pub use blocks_api::BlockText;
pub use buffer_api::HistoryShed;
pub use builder::{TIERED_RING_CAP_DEFAULT, TerminalBuilder};
#[cfg(feature = "serde")]
pub use checkpoint::{CHECKPOINT_META_REQUIRED_KEYS, CheckpointMeta};
pub use checkpoint::{
    DcsQueryCarry, DcsQueryKind, GridCursorRepr, PaneStepCarry, ReplayCarry, ShellIntegrationNonce,
    ShellIntegrationPosture, StyleRepr, TerminalCheckpoint,
};
pub use checkpoint_canon::{
    CHECKPOINT_CANON_VERSION, CONFIG_CANON_VERSION, CanonFault, PANE_CARRY_CANON_VERSION,
    POLICY_CANON_VERSION, are_canonical_hyperlink_schemes, policy_canonical, policy_from_canonical,
};
pub use checkpoint_holder::{
    HolderAnchorRepr, HolderMarkRepr, HolderRepr, HolderSealRepr, HolderSpanRepr,
};
pub use checkpoint_output::{
    HyperlinkRepr, OutputStateRepr, SavedModeRepr, SgrPushRepr, Vt52AddressRepr,
};
#[cfg(feature = "serde")]
pub use checkpoint_parser::ParserRepr;
pub use checkpoint_state::{
    ColorOverridesRepr, ColorRepr, ColorSlotRepr, ColorStackEntryRepr, CwdReportRepr,
    OutputBlockRepr, PaletteOverrideRepr, ShellRepr, ShellSpanRepr, TitleRepr,
};
pub use clipboard_auth::ClipboardAccess;
pub use custody::CustodyTransition;
pub use foreground_handback::{ForegroundHandback, evidence as program_evidence};
pub use frame_watchers::FrameWatchers;
pub use holder_fence::{
    CutKind, HolderAdoption, HolderCut, HolderFence, HolderView, MAX_HOLDER_MARKS, MAX_HOLDER_SPANS,
};
pub use host_journal::{HOST_JOURNAL_CAP, HostOp, HostReport};
pub use mode_mirror::ModeMirror;
pub use observe::{
    FRAME_HANG, Frame, GONE_SETTLE, RowMatch, RowRange, RowVerdict, Satisfaction, SurfaceSource,
    WatchId, WatcherSet, WatcherSpec, first_matching_row,
};
pub use processing::{ClockReading, KittyAnimationStep};
pub use render_cells::{RenderCell, Script, UnderlineStyle};
pub use resize_journal::{RESIZE_JOURNAL_CAP, RenderDebt, ResizeReport};
pub use search_budgeted::{BudgetedSearchError, BudgetedSearchStep};
pub use search_index::SearchDiscard;
pub use search_plan::{
    BuiltSearch, SearchKey, SearchLayout, SearchPlan, SearchRequest, SearchVerdict, SearchWindow,
};
#[cfg(any(test, debug_assertions))]
pub use search_query::take_narrowed_query_steps;
pub use search_query::{
    NarrowStack, WrapFrame, join_wrapped_rows, query_search_index, query_search_index_narrowing,
};
pub(crate) use search_rows::history_line_text_cells;
pub use search_rows::{AbsRowAt, AbsRowText, SearchRows};
pub use selection::smart_word_in_cells;
pub use shell::{
    Annotation, BlockState, CommandMark, OutputBlock, ShellState, StartFlags, StartKind, StartMark,
    TerminalMark,
};
pub use shell_api::InputRegionStart;
pub use state::{
    CONTENT_SCROLL_BAND_RING, ContentScrollDelta, ContentScrollState, RowBandMove, Terminal,
};
pub(crate) use types::SavedCursorState;
pub use types::{
    ClipboardOperation, ClipboardSelection, CurrentStyle, CursorStyle,
    Iterm2ShellIntegrationVersion, MouseEncoding, MouseMode, TerminalModes, TerminalSize,
};
use types::{SgrPushMask, SgrStackEntry, TaskbarProgress};

use crate::grid::Cursor;

pub(crate) type UserVarsMap = std::collections::HashMap<String, String>;

pub(crate) type PendingNotificationsMap = std::collections::HashMap<String, types::Notification>;

/// Maximum decoded clipboard bytes answered to an OSC 52 query (`Pd = ?`).
pub(crate) const MAX_OSC52_QUERY_RESPONSE_BYTES: usize = 64 * 1024;

/// Maximum bytes in the terminal response buffer.
pub(crate) const MAX_RESPONSE_BUFFER_SIZE: usize = 1024 * 1024;

/// Maximum title/icon-name length in bytes for OSC 0/1/2 and `set_title`.
const MAX_TITLE_BYTES: usize = 1024;

/// Maximum OSC 8 hyperlink URL length in bytes; longer URLs are ignored.
const MAX_HYPERLINK_URL_BYTES: usize = 8192;

/// Maximum working-directory path length in bytes for OSC 7 and OSC 633 `P` Cwd.
pub(crate) const MAX_CWD_PATH_BYTES: usize = 4096;

/// Maximum command-line text length in bytes for OSC 633 `E` and `F`/`G`/`H` payloads.
pub(crate) const MAX_COMMANDLINE_BYTES: usize = 64 * 1024;

/// An SGR color parameter as a palette index; values above 255 saturate.
#[inline]
pub(crate) const fn sgr_color_u8(val: u16) -> u8 {
    if val > 255 {
        u8::MAX
    } else {
        val.to_le_bytes()[0]
    }
}
