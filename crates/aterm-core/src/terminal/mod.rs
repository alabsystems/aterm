// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal emulator — combines the parser and grid.
//!
//! See handler modules (`handler_csi.rs`, `handler_esc.rs`, `handler_osc.rs`)
//! for per-sequence documentation.

/// UAX #9 BiDi visual-reordering bridge (off-by-default `bidi` feature → `aterm-bidi`).
#[cfg(feature = "bidi")]
mod bidi_reorder;
mod bidi_stubs;
mod blocks_api;
mod buffer_api;
mod builder;
mod callback_clearers;
mod callback_setters;
mod callbacks;
mod checkpoint;
pub(crate) mod clipboard_auth;
pub mod color_resolve;
mod colors_api;
mod config_api;
mod constructors;
mod content;
mod csi_dispatch_table;
pub(crate) mod dcs_auth;
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
pub(crate) mod host_traits;
pub(crate) mod hyperlink_auth;
mod keyboard_mode;
/// Kitty graphics protocol (APC `G`) command parser (KITTY-CORE foundation).
pub mod kitty_graphics;
/// Kitty graphics Unicode placeholder (U+10EEEE) diacritic decoding (M3).
mod kitty_placeholder;
#[cfg(test)]
pub mod mouse;
#[cfg(not(test))]
mod mouse;
/// The Observation Kernel (L0): event-driven `await`/quiescence primitive over
/// `content_seq`. Ephemeral, surface-non-mutating, checkpoint-excluded.
mod observe;
mod policy_bridge;
mod policy_gates;
mod processing;
mod render_cells;
mod reset;
mod response_capability;
mod response_rate_limiter;
mod selection;
mod shell;
pub(crate) mod shell_integration_auth;
mod window_auth;

// Phase 1+2: Universal block model, process detection, and AtermApi.
mod shell_api;

pub mod scrollback_shared_budget;
mod search_budgeted;
mod search_index;
mod stack_response;
mod state;
mod state_accessors;
mod transient_state;
mod types;

// TRUST_NATIVE_TLA Phase 2: the source↔spec cross-reference closure gate was
// RELOCATED out of aterm-core (it lived at `src/terminal/spec_xref_gate.rs` in Phase
// 0) into aterm-gui's TEST build (`#[cfg(test)] mod spec_xref_gate` in
// `aterm-gui/src/main.rs`). Reason: the Phase-2 ISOLATION anchors live in
// aterm-sandbox/pty/gpu/core, and `inventory` only collects from crates LINKED into
// the gate's test binary; aterm-gui links them all (with each crate's `spec-anchors`
// feature on), so it is the one binary that sees EVERY machine's anchors —
// `terminal_modes` (still emitted by aterm-core's anchors, now visible downstream via
// its `spec-anchors` feature) PLUS the six ISOLATION machines.

use callbacks::{
    BufferActivationCallback, MAX_DCS_CALLBACK_BYTES, MAX_DCS_GLOBAL_BUDGET, SGR_STACK_MAX_DEPTH,
    TITLE_STACK_MAX_DEPTH, WindowCallback,
};
pub use callbacks::{ColorChangeOp, ColorTarget};
pub use colors_api::ColorPaletteMut;
#[cfg(feature = "sixel")]
use grouped_state::SixelState;
use grouped_state::{
    ClipboardState, ColorState, CursorSaveState, DcsState, DcsType, Iterm2State, MarksState,
    NotificationState, SemanticState, ShellIntegrationState, TitleState,
};
use reset::{ResetGroups, reset_common_fields};
use transient_state::{TransientState, Vt52CursorState};

pub(crate) use aterm_types::charset::CharacterSetState;
pub use aterm_types::{ColorPalette, Rgb};
pub use aterm_types::{KittyKeyboardFlags, KittyKeyboardState};
pub use blocks_api::BlockText;
pub use builder::{TIERED_RING_CAP_DEFAULT, TerminalBuilder};
pub use callbacks::{
    CALLBACK_REGISTRY, CallbackCategory, CallbackInfo, SshConductorCallbackEvent,
    TmuxCallbackEvent, callback_by_name, callback_count, callback_info,
};
#[cfg(feature = "serde")]
pub use checkpoint::CheckpointMeta;
pub use checkpoint::{GridCursorRepr, HostBindings, StyleRepr, TerminalCheckpoint};
// The injected-clock seam, re-exported so an out-of-crate replay/lash harness
// can feed a FIXED ClockReading and get bit-deterministic state regardless of
// real wall-clock pacing (the determinism a faithful replay relies on).
pub use observe::{
    RowMatch, RowRange, Satisfaction, SurfaceSource, WatchId, WatcherSet, WatcherSpec,
    first_matching_row,
};
pub use processing::ClockReading;
pub use shell::{
    Annotation, BlockState, CommandMark, OutputBlock, ShellEvent, ShellState, TerminalMark,
};
pub use types::{
    ClipboardOperation, ClipboardSelection, CopyToClipboardOperation, CurrentStyle, CursorStyle,
    Iterm2CellSize, Iterm2SetColor, Iterm2ShellIntegrationVersion, MouseEncoding, MouseMode,
    TerminalModes, TerminalSize, TerminalSnapshot,
};
#[allow(
    unused_imports,
    reason = "RemoteHost re-export used by session::terminal_state; dead_code propagation hides usage"
)]
pub(crate) use types::{
    MultipartFileOperation, RemoteHost, SavedCursorState, SemanticBlock, SemanticBlockEvent,
    SemanticButton, SemanticButtonEvent, SemanticButtonType,
};
// Terminal-internal: not re-exported to crate level
pub use aterm_types::XtermKeyboardState;
pub use aterm_types::{WindowOperation, WindowResponse};
pub use clipboard_auth::ClipboardAccess;
pub use render_cells::{RenderCell, UnderlineStyle};
pub use search_budgeted::{BudgetedSearchError, BudgetedSearchStep};
pub use state::{ContentScrollState, Terminal};
use types::{SgrPushMask, SgrStackEntry, TaskbarProgress};

use crate::grid::Cursor;

/// Feature-gated terminal-internal constants needed by extracted test crates.
#[cfg(test)]
pub mod testing {
    /// Maximum completed command marks (OSC 133).
    pub const COMMAND_MARKS_MAX: usize = super::shell::COMMAND_MARKS_MAX;
    /// Maximum completed output blocks (OSC 133).
    pub const OUTPUT_BLOCKS_MAX: usize = super::shell::OUTPUT_BLOCKS_MAX;
    /// Maximum number of terminal marks (OSC 1337 SetMark).
    pub const TERMINAL_MARKS_MAX: usize = super::shell::TERMINAL_MARKS_MAX;
    /// Maximum number of annotations (OSC 1337 AddAnnotation).
    pub const ANNOTATIONS_MAX: usize = super::shell::ANNOTATIONS_MAX;
    /// Maximum number of semantic code blocks (OSC 1337 Block).
    pub const SEMANTIC_BLOCKS_MAX: usize = super::shell::SEMANTIC_BLOCKS_MAX;
    /// Maximum number of semantic buttons (OSC 1337 Button).
    pub const SEMANTIC_BUTTONS_MAX: usize = super::shell::SEMANTIC_BUTTONS_MAX;
    /// Maximum OSC 52 clipboard query response size (64 KiB).
    pub const MAX_OSC52_QUERY_RESPONSE_BYTES: usize = super::MAX_OSC52_QUERY_RESPONSE_BYTES;
    // Re-export types needed by extracted tests (from their origin crate).
    pub use aterm_types::osc::{SemanticBlockEvent, SemanticButtonEvent};
}

// Type alias for user_vars HashMap.
pub(crate) type UserVarsMap = std::collections::HashMap<String, String>;

// Pending notification map.
pub(crate) type PendingNotificationsMap = std::collections::HashMap<String, types::Notification>;

/// Maximum **decoded** clipboard bytes for an OSC 52 query response (Pd = "?").
///
/// This caps the size of **raw/decoded** data that aterm-core will accept from
/// the clipboard callback before base64 encoding and emitting as an OSC 52
/// response. Responses exceeding this limit are silently dropped to avoid
/// excessive memory use and accidental large exfil.
///
/// # Wire size note
///
/// The actual emitted response is larger due to:
/// - Base64 encoding (~33% expansion: 3 input bytes → 4 output bytes)
/// - OSC framing overhead (~12 bytes for `ESC ] 52 ; Pc ; ... BEL`)
///
/// A 64KB decoded payload becomes ~85KB on the wire. If wire-size constraints
/// are critical, hosts should apply their own caps before returning data from
/// the clipboard callback.
///
/// # Rationale
///
/// 64 KiB is a deliberately tight, clipboard-specific cap: a clipboard query
/// response should never be large, and a tight bound limits accidental large
/// exfil. This is independent of the parser's `MAX_OSC_DATA` input limit (which
/// is generous — 8 MiB — to carry OSC 1337 inline images); clipboard data does
/// not need that headroom, so it keeps its own smaller cap.
pub(crate) const MAX_OSC52_QUERY_RESPONSE_BYTES: usize = 64 * 1024;

/// Maximum bytes captured by OSC 1337 `CopyToClipboard` text capture mode.
///
/// While capture mode is active, every printed character is appended until
/// `EndCopy` is received. This cap prevents unbounded growth when peers never
/// send `EndCopy` (accidental or malicious).
pub(crate) const MAX_COPY_TO_CLIPBOARD_CAPTURE_BYTES: usize = 10 * 1024 * 1024;

/// Maximum bytes allowed in the terminal response buffer.
///
/// Responses from query sequences (for example DSR/DA/DECRQSS/OSC queries) are
/// buffered until the host drains them via `take_response()`. This cap prevents
/// unbounded memory growth when a single input batch generates excessive
/// responses before the host has a chance to read.
pub(crate) const MAX_RESPONSE_BUFFER_SIZE: usize = 1024 * 1024;

/// Maximum title/icon-name length in bytes for OSC 0/1/2 and the public
/// `set_title`/`set_icon_name` API. Matches OSC 777's cap in
/// `handler_osc_notify.rs`. The parser's OSC buffer allows up to 65534
/// bytes, but real terminal titles are short strings.
const MAX_TITLE_BYTES: usize = 1024;

/// Maximum hyperlink URL length in bytes for OSC 8 sequences.
///
/// URLs exceeding this limit are silently ignored (consistent with other
/// terminals). 8 KiB is generous for any realistic URL while preventing
/// memory abuse from malicious sequences. Part of #7172.
const MAX_HYPERLINK_URL_BYTES: usize = 8192;

/// Maximum current-working-directory path length in bytes for OSC 7 and the
/// OSC 633 `P` Cwd property.
///
/// The OSC parser admits up to `MAX_OSC_DATA` (8 MiB) of payload, and the cwd
/// path it carries is otherwise length-unbounded: OSC 7's percent-decoded path
/// and OSC 633-P's `Cwd` value flow into the count-capped (but not byte-capped)
/// `osc_events` queue and the shell mark/block working-directory fields, where
/// a callback-based host can retain them indefinitely. Paths longer than this
/// are rejected (left unchanged / not queued). 4 KiB matches the conventional
/// `PATH_MAX`, which is generous for any real directory.
pub(crate) const MAX_CWD_PATH_BYTES: usize = 4096;

/// Maximum command-line / semantic-text length in bytes for the OSC 633 `E`
/// (VS Code explicit command) and `F`/`G`/`H` (progress) payloads.
///
/// Same hazard as [`MAX_CWD_PATH_BYTES`] (#7172): the OSC parser admits up to
/// `MAX_OSC_DATA` (8 MiB), and the `E` command text is RETAINED in the count-capped
/// (but not byte-capped) shell mark/block `commandline` fields, where a host can
/// hold it indefinitely — so an unbounded `E` can pin megabytes per retained entry.
/// Command lines are legitimately longer than a path (long pipelines), so this cap
/// is more generous than `MAX_CWD_PATH_BYTES` while still bounding abuse. Bounding
/// the escaped input also bounds the unescape work (unescape only shrinks).
pub(crate) const MAX_COMMANDLINE_BYTES: usize = 64 * 1024;

// ----------------------------------------------------------------------------
// Type-safe conversion helpers
// ----------------------------------------------------------------------------

/// Convert u16 SGR parameter to u8 color index.
/// SGR color parameters are in 0-255 range; values >255 saturate to 255.
#[inline]
pub(crate) fn sgr_color_u8(val: u16) -> u8 {
    val.try_into().unwrap_or(u8::MAX)
}

// Terminal struct, Debug impl, and root state accessors live in state.rs.
