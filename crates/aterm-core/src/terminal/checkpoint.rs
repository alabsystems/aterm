// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `TerminalCheckpoint` — a scoped, round-trippable projection of a live
//! [`Terminal`] (GREEN-ORDER step 4 / design `HIERARCHICAL_SESSIONS.md` B.3.2).
//!
//! The live [`Terminal`] is **neither `Clone` nor `Serialize`**: it holds five
//! `Box<dyn FnMut>` callbacks, two `Instant`s, four host-auth state machines, an
//! `Option<PolicyEngine>`, and a live `Parser`. A byte-identical clone is
//! impossible by construction. A [`TerminalCheckpoint`] is therefore a *precise
//! projection with a documented exclusion list*, not a clone — see the EXCLUDED
//! and DEFERRED blocks below.
//!
//! This increment captures the buffer-state core: both grid bodies (full cell
//! fidelity via the `Line` codec), per-grid cursor / scroll-region / pending-wrap
//! / tab-stops, size, and a set of cheap `Copy`/snapshot leaf fields. The
//! round-trip is proven by the in-module property test (`mod tests`), which is
//! the ship gate for this step.
//
// ===========================================================================
// EXCLUDED (host bindings, re-bound here):
//   - the five callbacks (bell, cursor_style, buffer_activation, window,
//     text_sizing)
//   - policy (PolicyState: the installed PolicyEngine + its compiled gate table)
//   - live auth nonces / capabilities (clipboard_auth, shell_integration_auth,
//     hyperlink_auth, dcs_auth)
// These are HOST effects, not buffer state. They are re-bound by the host on
// `from_checkpoint` via `HostBindings`. For this increment `HostBindings::none()`
// installs the same defaults `Terminal::new` does; real callbacks/policy/auth
// rebinding lands in later work. Callback-driven side effects (OSC 9/99
// notifications, OSC 52 clipboard writes, window ops, bell fires) are NOT
// replayed — they are host effects, not state.
//
// DEFERRED (later stages — NOT captured in this increment, and why):
//   - grouped sub-projections: color, transient, shell, marks,
//     semantic, iterm2, vi, text_selection — each needs its own per-field Repr
//     (palette stacks, SGR stack, DECSC slots, OSC133 blocks, …) and is out of
//     scope for the buffer-core increment.
//   - sixel: the decoded image store needs a lossy-edge Repr (B.4).
//   - clock-domain fields: bell_ticks (last_bell_time), sync_ticks (sync_start),
//     and rate_limiter (token bucket) — these require a Clock seam mapping
//     Instant -> Ticks that does not exist yet; capturing them faithfully on a
//     forked timeline is the explicit B.4 must-fix, separate from this step.
//   - serde: the checkpoint stores leaf engine types BY VALUE and relies on
//     `PartialEq` for the round-trip gate; on-the-wire serialization (the
//     `grid: Vec<u8>` bytes are already serde-ready) is a later concern.
//   - there is no style-id to carry: `CurrentStyle`'s four semantic fields are
//     the whole rendition, and the writers read colours inline. On restore we set
//     `style` and call `apply_style_change()`, which refreshes the writer caches
//     and re-arms the rebuilt grid's BCE cursor template (see `from_checkpoint`).
// ===========================================================================

use aterm_types::charset::CharacterSetState;
use aterm_types::{KittyKeyboardStateSnapshot, TaskbarProgress, XtermKeyboardState};

use super::Terminal;
use super::types::{CurrentStyle, TerminalModes};
use crate::grid::{CellFlags, Cursor, Grid, PackedColor, SavedCursorState};
use crate::scrollback::{Scrollback, deserialize_lines, serialize_lines};

/// Host-side bindings re-installed on `from_checkpoint`.
///
/// A checkpoint deliberately omits the live `Terminal`'s callbacks, policy
/// engine, and auth nonces (see the EXCLUDED block at the top of this module).
/// `HostBindings` is where the host re-supplies them. For this increment it is
/// intentionally empty (all `None`); real fields are added as the rebinding
/// work lands. `HostBindings::none()` is enough to hydrate a fully-living,
/// introspectable `Terminal` whose buffer state matches the source exactly.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct HostBindings {
    // Placeholder for the five callbacks / policy engine / auth state that a
    // host re-binds. All `None`/empty in this increment; documented as deferred.
    _private: (),
}

impl HostBindings {
    /// A null/empty set of host bindings.
    ///
    /// Installs the same host-effect defaults as `Terminal::new` (no callbacks,
    /// no policy engine, default auth posture).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

/// Per-grid cursor + region + wrap + tab-stop projection.
///
/// Captured independently for the main grid and (when present) the alt grid,
/// because each carries its own cursor and scroll region.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GridCursorRepr {
    /// Cursor row (0-based, grid-relative).
    pub cursor_row: u16,
    /// Cursor column (0-based, grid-relative).
    pub cursor_col: u16,
    /// Deferred-wrap (pending-wrap / wrap-next) flag.
    pub pending_wrap: bool,
    /// DECSTBM scroll-region top (inclusive).
    pub scroll_top: u16,
    /// DECSTBM scroll-region bottom (inclusive).
    pub scroll_bottom: u16,
    /// DECSLRM horizontal-margin left (inclusive). Lives in the same grid
    /// cursor_state as the scroll region and must round-trip, or a checkpoint
    /// taken under DECLRMM (mode 69, captured in `modes`) restores with the mode
    /// flag on but full-width margins — so margin-aware wrap/clamp/ICH/DCH/scroll
    /// diverge from the live engine and `checkpoint() != replay.checkpoint()`.
    pub margin_left: u16,
    /// DECSLRM horizontal-margin right (inclusive).
    pub margin_right: u16,
    /// Per-column tab stops (`true` = stop set at that column).
    pub tab_stops: Vec<bool>,
}

impl GridCursorRepr {
    fn capture(grid: &Grid) -> Self {
        let region = grid.scroll_region();
        let margins = grid.horizontal_margins();
        Self {
            cursor_row: grid.cursor_row(),
            cursor_col: grid.cursor_col(),
            pending_wrap: grid.pending_wrap(),
            scroll_top: region.top,
            scroll_bottom: region.bottom,
            margin_left: margins.left,
            margin_right: margins.right,
            tab_stops: grid.tab_stops().to_vec(),
        }
    }

    fn apply(&self, grid: &mut Grid) {
        grid.set_scroll_region(self.scroll_top, self.scroll_bottom);
        // set_horizontal_margins self-validates (full margins recompute
        // has_horizontal_margins=false), so this is safe even if cols changed.
        grid.set_horizontal_margins(self.margin_left, self.margin_right);
        grid.set_cursor(self.cursor_row, self.cursor_col);
        grid.set_pending_wrap(self.pending_wrap);
        grid.restore_tab_stops(&self.tab_stops);
    }
}

/// Minimal, by-value style projection.
///
/// `CurrentStyle` carries cached fields that are pure functions of the four
/// semantically-meaningful inputs `(fg, bg, flags, protected)`, so we capture
/// only those and rebuild via `CurrentStyle::new(...)` on restore. This both
/// avoids depending on `PartialEq` for `CurrentStyle`'s private cache and keeps
/// the round-trip honest (the cache is recomputed from the four inputs, then
/// `apply_style_change()` re-arms the rebuilt grid's BCE cursor template).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleRepr {
    /// Foreground color.
    pub fg: PackedColor,
    /// Background color.
    pub bg: PackedColor,
    /// SGR cell flags (bold, italic, underline, …).
    pub flags: CellFlags,
    /// DECSCA selective-erase protection.
    pub protected: bool,
}

impl StyleRepr {
    fn capture(style: &CurrentStyle) -> Self {
        Self {
            fg: style.fg,
            bg: style.bg,
            flags: style.flags,
            protected: style.protected,
        }
    }

    fn into_style(self) -> CurrentStyle {
        CurrentStyle::new(self.fg, self.bg, self.flags, self.protected)
    }
}

/// Wire-stable DECSC/DECRC slot. The style is stored as semantic raw fields so
/// restore rebuilds `CurrentStyle` caches from them rather than carrying any
/// grid-local index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct SavedCursorPendingWrap(bool);

impl SavedCursorPendingWrap {
    const fn new(value: bool) -> Self {
        Self(value)
    }

    const fn get(self) -> bool {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SavedCursorRepr {
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub style_fg_bits: u32,
    pub style_bg_bits: u32,
    pub style_flag_bits: u16,
    pub style_protected: bool,
    pub origin_mode: bool,
    pub auto_wrap: bool,
    pub charset: CharacterSetState,
    pub pending_wrap: SavedCursorPendingWrap,
    pub underline_color: Option<u32>,
}

impl SavedCursorRepr {
    fn capture(saved: SavedCursorState) -> Self {
        Self {
            cursor_row: saved.cursor.row,
            cursor_col: saved.cursor.col,
            style_fg_bits: saved.style.fg.0,
            style_bg_bits: saved.style.bg.0,
            style_flag_bits: saved.style.flags.0,
            style_protected: saved.style.protected,
            origin_mode: saved.origin_mode,
            auto_wrap: saved.auto_wrap,
            charset: saved.charset,
            pending_wrap: SavedCursorPendingWrap::new(saved.pending_wrap),
            underline_color: saved.underline_color,
        }
    }

    fn into_saved(self) -> SavedCursorState {
        SavedCursorState {
            cursor: Cursor::new(self.cursor_row, self.cursor_col),
            style: CurrentStyle::new(
                PackedColor(self.style_fg_bits),
                PackedColor(self.style_bg_bits),
                CellFlags(self.style_flag_bits),
                self.style_protected,
            ),
            origin_mode: self.origin_mode,
            auto_wrap: self.auto_wrap,
            charset: self.charset,
            pending_wrap: self.pending_wrap.get(),
            underline_color: self.underline_color,
        }
    }
}

/// A scoped, round-trippable projection of a live [`Terminal`] (B.3.2).
///
/// Equality is *structural*: `checkpoint() == from_checkpoint(&c).checkpoint()`
/// is the re-checkpoint identity proven by the round-trip test. The grid bodies
/// are stored as `serialize_lines`-encoded bytes (scrollback-then-visible); all
/// other captured fields are stored by value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCheckpoint {
    /// Grid rows (visible).
    pub rows: u16,
    /// Grid cols.
    pub cols: u16,
    /// Main-grid body: `serialize_lines(grid.checkpoint_lines())`
    /// (scrollback-then-visible).
    pub grid: Vec<u8>,
    /// How many of `grid`'s leading line records are SCROLLBACK rather than
    /// visible rows. `grid` therefore holds `history_lines + rows` records.
    ///
    /// Carried explicitly rather than inferred from the blob because the consumer
    /// must bound its allocation from the AUTHENTICATED meta before it decodes any
    /// bytes — a length taken from the untrusted payload itself could authorize an
    /// arbitrary allocation. `0` is the visible-only projection, which is what
    /// every producer before this field emitted, so it is also the safe default
    /// for a checkpoint arriving without it.
    ///
    /// The ALTERNATE grid always carries `0`: the live alt screen keeps no
    /// scrollback, and `restore_grid` rebuilds it with a zero-length ring.
    pub history_lines: u32,
    /// Main-grid cursor/region/wrap/tab projection.
    pub cursor: GridCursorRepr,
    /// Alt-grid body, if an alt grid exists.
    pub alt_grid: Option<Vec<u8>>,
    /// Alt-grid cursor/region/wrap/tab projection, if an alt grid exists.
    pub alt_cursor: Option<GridCursorRepr>,
    /// DECSC/DECRC save slot for the main screen.
    pub saved_cursor_main: Option<SavedCursorRepr>,
    /// DECSC/DECRC save slot for the alternate screen.
    pub saved_cursor_alt: Option<SavedCursorRepr>,
    /// Terminal modes (Copy; ~40 boolean/enum fields).
    pub modes: TerminalModes,
    /// Current SGR style (minimal by-value projection).
    pub style: StyleRepr,
    /// Character set state (G0-G3, GL, GR, single-shift).
    pub charset: CharacterSetState,
    /// Kitty keyboard protocol state (via snapshot/restore).
    pub kitty_keyboard: KittyKeyboardStateSnapshot,
    /// xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).
    pub xterm_keyboard: XtermKeyboardState,
    /// Taskbar progress (ConEmu OSC 9;4).
    pub taskbar_progress: Option<TaskbarProgress>,
    /// Secure keyboard entry mode.
    pub secure_keyboard_entry: bool,
    /// Current working directory (OSC 7).
    pub current_working_directory: Option<String>,
    /// Parser was in Ground state at capture time (B.3.3 invariant).
    pub parser_ground: bool,
}

impl Terminal {
    /// Capture a [`TerminalCheckpoint`] — a pure read, no host effects, no fs.
    ///
    /// The parser MUST be in Ground state (B.3.3): a checkpoint taken
    /// mid-sequence would silently lose the parser's partial state (which is not
    /// in the projection). This is `debug_assert`ed.
    #[must_use]
    pub fn checkpoint(&self) -> TerminalCheckpoint {
        self.checkpoint_with_scrollback(true)
    }

    /// Capture the exact visible terminal state without copying scrollback history.
    ///
    /// This is the latency-bounded process-handoff projection: screen continuity
    /// needs the visible rows, cursor, modes, and saved alternate grid, but it must
    /// not turn an update click into work proportional to an arbitrarily deep history.
    #[must_use]
    pub fn checkpoint_visible(&self) -> Option<TerminalCheckpoint> {
        self.checkpoint_carry(0)
    }

    /// The seamless-handoff projection: the visible screen plus at most
    /// `max_history` lines of the most recent scrollback.
    ///
    /// `max_history == 0` is [`Self::checkpoint_visible`] — what the overlap
    /// handoff carried before this existed, and the reason an in-session update
    /// left every tab with a single screen of history. A positive bound keeps the
    /// capture cost `O((rows + max_history) × cols)` while preserving history the
    /// user can actually scroll back to.
    ///
    /// Restore needs no counterpart: `restore_grid` already reads the last `rows`
    /// lines as the visible grid and pushes everything before them into an
    /// unlimited scrollback.
    #[must_use]
    pub fn checkpoint_carry(&self, max_history: usize) -> Option<TerminalCheckpoint> {
        self.parser_is_ground()
            .then(|| self.checkpoint_bounded(max_history))
    }

    fn checkpoint_with_scrollback(&self, include_scrollback: bool) -> TerminalCheckpoint {
        // `bounded(MAX)` is exactly the full history and `bounded(0)` exactly the
        // visible screen, so one parameter expresses every mode and the main and
        // alt grids can never drift apart in what they capture.
        self.checkpoint_bounded(if include_scrollback { usize::MAX } else { 0 })
    }

    fn checkpoint_bounded(&self, max_history: usize) -> TerminalCheckpoint {
        debug_assert!(
            self.parser_is_ground(),
            "checkpoint() requires parser_is_ground() (B.3.3)"
        );

        let grid_lines = self.grid.checkpoint_lines_bounded(max_history);
        // How many of those records are history, as the consumer must be told.
        // Derived from the produced vector rather than from `max_history` so it is
        // exact when the ring holds fewer lines than the bound allows.
        let history_lines = u32::try_from(
            grid_lines
                .len()
                .saturating_sub(usize::from(self.grid.rows())),
        )
        .unwrap_or(u32::MAX);
        let grid_bytes = serialize_lines(&grid_lines);
        let cursor = GridCursorRepr::capture(&self.grid);

        let (alt_grid, alt_cursor) = match &self.alt_grid {
            Some(alt) => (
                // The live alternate screen keeps no scrollback, so this is the
                // visible rows whatever the bound; going through the same accessor
                // keeps that true by construction rather than by comment.
                Some(serialize_lines(&alt.checkpoint_lines_bounded(max_history))),
                Some(GridCursorRepr::capture(alt)),
            ),
            None => (None, None),
        };

        TerminalCheckpoint {
            rows: self.grid.rows(),
            cols: self.grid.cols(),
            grid: grid_bytes,
            history_lines,
            cursor,
            alt_grid,
            alt_cursor,
            saved_cursor_main: self.cursor_save.main.map(SavedCursorRepr::capture),
            saved_cursor_alt: self.cursor_save.alt.map(SavedCursorRepr::capture),
            modes: self.modes,
            style: StyleRepr::capture(&self.style),
            charset: self.charset,
            kitty_keyboard: self.kitty_keyboard.snapshot(),
            xterm_keyboard: self.xterm_keyboard,
            taskbar_progress: self.taskbar_progress,
            secure_keyboard_entry: self.secure_keyboard_entry,
            current_working_directory: self.current_working_directory.clone(),
            parser_ground: self.parser_is_ground(),
        }
    }

    /// Rebuild a fully-living [`Terminal`] from a checkpoint, re-binding host
    /// effects via `host` (B.3.2).
    ///
    /// The rebuilt terminal's *buffer state* matches the source exactly (proven
    /// by the round-trip test); host bindings (callbacks/policy/auth) are NOT
    /// from the checkpoint — they come from `host` (see the EXCLUDED block).
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "from_checkpoint takes ownership of host bindings (callbacks/policy/auth) \
                  and installs them; this increment's HostBindings is empty, but the \
                  by-value signature is the stable rebinding seam (B.3.2) and must not \
                  churn when real Box<dyn FnMut> fields land"
    )]
    pub fn from_checkpoint(c: &TerminalCheckpoint, host: HostBindings) -> Terminal {
        // `host` is intentionally consumed even though this increment's
        // HostBindings is empty: it pins the API so callers wire rebinding here
        // and the signature does not churn when real fields land.
        let _ = host;

        // Which slot holds the ALTERNATE buffer (→ no scrollback ring) is NOT
        // fixed: `grid` is the *active* buffer and `alt_grid` is the *saved* one.
        // The active grid is the alt buffer iff we are on the alt screen; the
        // saved grid is the alt buffer iff we are NOT (it is a saved alt buffer
        // under 1047/1049 exit semantics). Keying `is_alt` off the slot alone
        // wrongly handed the saved MAIN buffer a 0-ring when captured under 1049
        // (alternate_screen == true), discarding its scrollback and breaking the
        // re-checkpoint identity.
        let on_alt = c.modes.alternate_screen;
        let active_grid = restore_grid(c.rows, c.cols, &c.grid, &c.cursor, on_alt);
        let mut terminal = Terminal::with_grid(active_grid);

        // Saved grid (same restore path; alt buffer ⟺ we are NOT on alt), if present.
        if let (Some(alt_bytes), Some(alt_cursor)) = (&c.alt_grid, &c.alt_cursor) {
            terminal.alt_grid = Some(restore_grid(c.rows, c.cols, alt_bytes, alt_cursor, !on_alt));
        }

        // Leaf fields by value.
        terminal.modes = c.modes;
        terminal.cursor_save.main = c.saved_cursor_main.map(SavedCursorRepr::into_saved);
        terminal.cursor_save.alt = c.saved_cursor_alt.map(SavedCursorRepr::into_saved);
        terminal.charset = c.charset;
        terminal.kitty_keyboard.restore_snapshot(c.kitty_keyboard);
        terminal.xterm_keyboard = c.xterm_keyboard;
        terminal.taskbar_progress = c.taskbar_progress;
        terminal.secure_keyboard_entry = c.secure_keyboard_entry;
        terminal
            .current_working_directory
            .clone_from(&c.current_working_directory);

        // Style: set the semantic style, then re-arm the REBUILT grid's BCE
        // cursor template from it. `into_style()` already rebuilds the writer
        // caches (`CurrentStyle::new`), but the fresh grid's cursor template is
        // default, so a restored non-default background would otherwise be lost
        // by the first scroll/erase that ran before the next SGR (#7522).
        terminal.style = c.style.into_style();
        {
            // We reach the SGR view via the generated handler split.
            let (_parser, mut handler) = terminal.split_for_process();
            handler.sgr_style().apply_style_change();
        }

        terminal
    }

    /// Restore a checkpoint's buffer state INTO an already-configured live
    /// terminal (the seamless-update adopt path): the caller built `self` via
    /// the normal config-applied constructor (callbacks/policy/auth wired), and
    /// this replaces only what [`Self::from_checkpoint`] would have captured —
    /// grids, cursor state, modes, charset, keyboard state, style. Host
    /// bindings and auth state are untouched, so this composes with the spawn
    /// path's configure_* wiring instead of re-deriving it.
    pub fn restore_checkpoint(&mut self, c: &TerminalCheckpoint) {
        // The grids are REPLACED below, so every generation stamp the cached
        // search index (and any budgeted-search cursor) was keyed against —
        // `content_gen`, `absolute_row_counter`, `history_renumber_epoch` —
        // restarts from a fresh grid's values. Those stamps are only meaningful
        // within one grid lineage; carrying the caches across a wholesale
        // replacement would let a coincidental stamp collision serve results
        // from the PRE-restore content (and would break the incremental
        // refresh's "retention only advances" model). Drop them; the next
        // search rebuilds from the restored buffer.
        self.release_search_index();
        let on_alt = c.modes.alternate_screen;
        self.grid = restore_grid(c.rows, c.cols, &c.grid, &c.cursor, on_alt);
        self.alt_grid = match (&c.alt_grid, &c.alt_cursor) {
            (Some(alt_bytes), Some(alt_cursor)) => {
                Some(restore_grid(c.rows, c.cols, alt_bytes, alt_cursor, !on_alt))
            }
            _ => None,
        };
        self.modes = c.modes;
        self.cursor_save.main = c.saved_cursor_main.map(SavedCursorRepr::into_saved);
        self.cursor_save.alt = c.saved_cursor_alt.map(SavedCursorRepr::into_saved);
        self.charset = c.charset;
        self.kitty_keyboard.restore_snapshot(c.kitty_keyboard);
        self.xterm_keyboard = c.xterm_keyboard;
        self.taskbar_progress = c.taskbar_progress;
        self.secure_keyboard_entry = c.secure_keyboard_entry;
        self.current_working_directory
            .clone_from(&c.current_working_directory);
        // Style: semantic value, then re-arm the REBUILT grid's BCE cursor
        // template from it (see `from_checkpoint`).
        self.style = c.style.into_style();
        let (_parser, mut handler) = self.split_for_process();
        handler.sgr_style().apply_style_change();
        // In-place hydration swaps the entire coordinate lineage underneath
        // existing host consumers. Preserve the cumulative clocks but publish
        // one fail-closed epoch edge so cursor effects/search-adjacent caches
        // cannot remain attached to pre-restore cells. A freshly constructed
        // `from_checkpoint` terminal needs no edge: consumers baseline it.
        self.content_scroll_state.invalidate();
        // Hydration replaced BOTH grids and `modes.alternate_screen` with another
        // session's, so every selection anchor now names content from a lineage this
        // terminal never saw. `text_selection` is deferred from the checkpoint
        // `Repr`, so neither slot was restored — they were simply left standing, and
        // the parked one would come back on the next alt exit as a highlight with no
        // history behind it.
        // Only the PARKED slot, which this design introduced and therefore owns.
        // Hydration arguably invalidates the LIVE selection too — its anchors name a
        // lineage this terminal never saw — but that is a pre-existing gap, and
        // clearing it here is user-visible on the seamless-update ADOPT path
        // (`spawn.rs`'s `restore_checkpoint`): a highlight would vanish across an
        // in-place update. Left alone deliberately rather than changed as a side
        // effect of screen-scoping.
        self.parked_text_selection.clear();
    }
}

/// The serde-carryable half of a [`TerminalCheckpoint`]: every scalar field,
/// WITHOUT the two grid byte blobs (which stay on the binary `Line` codec —
/// embedding megabyte byte arrays in TOML manifests is not a wire format).
///
/// Purpose: the SEAMLESS UPDATE screen carry. The outgoing (pre-update) process
/// serializes this into the handoff manifest and writes the grid blobs as
/// sidecar files; the incoming (post-update) process reassembles the full
/// checkpoint via [`CheckpointMeta::into_checkpoint`] and hydrates the adopted
/// session's engine with [`Terminal::from_checkpoint`], so the window comes
/// back showing exactly the screen (prompt included) the user had. The generic
/// and one-release legacy serializer remains additive via `serde(default)`.
/// Modern seamless schema 1 is stricter: `aterm-gui` requires every semantic
/// key before deserializing and binds the canonical result into its adoption
/// proof. The style channel crosses the `aterm-grid` types as raw bits
/// (`PackedColor(pub u32)`, `CellFlags(pub u16)`) to keep serde out of the
/// grid crate.
#[cfg(feature = "serde")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointMeta {
    /// Grid rows (visible).
    pub rows: u16,
    /// Grid cols.
    pub cols: u16,
    /// How many leading records of the main grid blob are SCROLLBACK rather than
    /// visible rows (see `TerminalCheckpoint::history_lines`).
    ///
    /// `#[serde(default)]` on purpose: every producer before this field existed
    /// emitted a visible-only blob, and `0` is exactly that. A checkpoint arriving
    /// without the key is therefore read correctly rather than rejected — which is
    /// the whole compatibility story, because in a handoff the PARENT is always
    /// the OLDER build and the CHILD the newer one, so the consumer is the side
    /// that has to accept both shapes.
    #[serde(default)]
    pub history_lines: u32,
    /// Main-grid cursor/region/wrap/tab projection.
    pub cursor: GridCursorRepr,
    /// Alt-grid cursor projection, if an alt grid exists.
    #[serde(default)]
    pub alt_cursor: Option<GridCursorRepr>,
    /// Main-screen DECSC/DECRC save slot.
    #[serde(default)]
    pub saved_cursor_main: Option<SavedCursorRepr>,
    /// Alternate-screen DECSC/DECRC save slot.
    #[serde(default)]
    pub saved_cursor_alt: Option<SavedCursorRepr>,
    /// Terminal modes.
    #[serde(default)]
    pub modes: TerminalModes,
    /// Current SGR foreground as raw `PackedColor` bits (see the struct doc).
    #[serde(default)]
    pub style_fg_bits: u32,
    /// Current SGR background as raw `PackedColor` bits.
    #[serde(default)]
    pub style_bg_bits: u32,
    /// Current SGR `CellFlags` bits (bold, italic, underline, …).
    #[serde(default)]
    pub style_flag_bits: u16,
    /// DECSCA selective-erase protection.
    #[serde(default)]
    pub style_protected: bool,
    /// Character set state (G0-G3, GL, GR, single-shift).
    #[serde(default)]
    pub charset: CharacterSetState,
    /// Kitty keyboard protocol state.
    #[serde(default)]
    pub kitty_keyboard: KittyKeyboardStateSnapshot,
    /// xterm keyboard modifier/format options.
    #[serde(default)]
    pub xterm_keyboard: XtermKeyboardState,
    /// Taskbar progress (ConEmu OSC 9;4).
    #[serde(default)]
    pub taskbar_progress: Option<TaskbarProgress>,
    /// Secure keyboard entry mode.
    #[serde(default)]
    pub secure_keyboard_entry: bool,
    /// Current working directory (OSC 7).
    #[serde(default)]
    pub current_working_directory: Option<String>,
}

#[cfg(feature = "serde")]
impl CheckpointMeta {
    /// Project a full checkpoint into its carryable meta half.
    ///
    /// EXHAUSTIVE destructure on purpose: adding a field to
    /// [`TerminalCheckpoint`] must fail THIS build until the carry decides to
    /// ship or deliberately drop it — silent drift is how a "seamless" update
    /// quietly loses state.
    #[must_use]
    pub fn from_checkpoint(c: &TerminalCheckpoint) -> Self {
        let TerminalCheckpoint {
            rows,
            cols,
            grid: _, // sidecar blob, not meta
            history_lines,
            alt_grid: _, // sidecar blob, not meta
            cursor,
            alt_cursor,
            saved_cursor_main,
            saved_cursor_alt,
            modes,
            style,
            charset,
            kitty_keyboard,
            xterm_keyboard,
            taskbar_progress,
            secure_keyboard_entry,
            current_working_directory,
            // Always true by checkpoint()'s B.3.3 precondition; re-imposed on
            // reassembly rather than trusted from the wire.
            parser_ground: _,
        } = c;
        Self {
            rows: *rows,
            cols: *cols,
            cursor: cursor.clone(),
            alt_cursor: alt_cursor.clone(),
            saved_cursor_main: *saved_cursor_main,
            history_lines: *history_lines,
            saved_cursor_alt: *saved_cursor_alt,
            modes: *modes,
            style_fg_bits: style.fg.0,
            style_bg_bits: style.bg.0,
            style_flag_bits: style.flags.0,
            style_protected: style.protected,
            charset: *charset,
            kitty_keyboard: *kitty_keyboard,
            xterm_keyboard: *xterm_keyboard,
            taskbar_progress: *taskbar_progress,
            secure_keyboard_entry: *secure_keyboard_entry,
            current_working_directory: current_working_directory.clone(),
        }
    }

    /// Reassemble a full [`TerminalCheckpoint`] from the meta + grid blobs.
    ///
    /// `alt_grid` presence must match `alt_cursor` (both from the same wire);
    /// a half-present pair degrades to no alt grid rather than a mismatched
    /// restore.
    #[must_use]
    pub fn into_checkpoint(self, grid: Vec<u8>, alt_grid: Option<Vec<u8>>) -> TerminalCheckpoint {
        let (alt_grid, alt_cursor) = match (alt_grid, self.alt_cursor) {
            (Some(g), Some(c)) => (Some(g), Some(c)),
            _ => (None, None),
        };
        TerminalCheckpoint {
            rows: self.rows,
            cols: self.cols,
            grid,
            history_lines: self.history_lines,
            cursor: self.cursor,
            alt_grid,
            alt_cursor,
            saved_cursor_main: self.saved_cursor_main,
            saved_cursor_alt: self.saved_cursor_alt,
            modes: self.modes,
            style: StyleRepr {
                fg: PackedColor(self.style_fg_bits),
                bg: PackedColor(self.style_bg_bits),
                flags: CellFlags(self.style_flag_bits),
                protected: self.style_protected,
            },
            charset: self.charset,
            kitty_keyboard: self.kitty_keyboard,
            xterm_keyboard: self.xterm_keyboard,
            taskbar_progress: self.taskbar_progress,
            secure_keyboard_entry: self.secure_keyboard_entry,
            current_working_directory: self.current_working_directory,
            parser_ground: true,
        }
    }
}

/// Rebuild a single grid from `serialize_lines` bytes + a cursor projection.
///
/// The byte stream is `scrollback-then-visible` (the `checkpoint_lines` layout):
/// the last `rows` lines are the visible rows; everything before is scrollback,
/// oldest first. We attach the scrollback to a tiered store (preserving order),
/// then restore the visible rows via the shared `fill_row_from_line` path, then
/// apply the cursor/region/wrap/tabs.
fn restore_grid(rows: u16, cols: u16, bytes: &[u8], cursor: &GridCursorRepr, is_alt: bool) -> Grid {
    let lines = deserialize_lines(bytes);
    let visible_start = lines.len().saturating_sub(rows as usize);
    let (scrollback_lines, visible_lines) = lines.split_at(visible_start);

    let mut grid = if is_alt {
        // The ALTERNATE screen has NO scrollback (matches enter_alternate_screen).
        // A restored alt buffer must discard scrolled-off lines exactly like the
        // live one; giving it a 1000-line ring let it accrue PHANTOM history that
        // the source never had, and diverged from the live engine on re-checkpoint.
        Grid::with_scrollback(rows, cols, 0)
    } else {
        let mut scrollback = Scrollback::with_defaults();
        // Don't let the default line cap silently drop restored history.
        scrollback.set_line_limit(None);
        for line in scrollback_lines {
            // push_line is infallible for the in-memory tier.
            scrollback.push_line(line.clone());
        }
        Grid::with_tiered_scrollback(rows, cols, 1000, scrollback)
    };
    grid.restore_visible_from_lines(visible_lines);
    cursor.apply(&mut grid);
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a fresh terminal with a byte stream that exercises EVERY captured
    /// field, leaving the parser in Ground state.
    fn build_rich_terminal(rows: u16, cols: u16) -> Terminal {
        let mut t = Terminal::new(rows, cols);

        // --- text + SGR: bold + underline + 256-color fg + truecolor bg ---
        t.process(b"\x1b[1;4;38;5;202;48;2;10;20;30mstyled\x1b[0m\r\n");

        // --- plain lines, more than `rows` so scrollback fills ---
        for i in 0..(rows as usize + 6) {
            t.process(format!("line{i}\r\n").as_bytes());
        }

        // --- DECSTBM scroll region (rows 2..rows-1, 1-based) ---
        t.process(format!("\x1b[2;{}r", rows - 1).as_bytes());

        // --- cursor move + pending-wrap: write the last column on a row ---
        t.process(b"\x1b[3;1H"); // row 3
        let last_col_fill: String = std::iter::repeat('Z').take(cols as usize).collect();
        t.process(last_col_fill.as_bytes()); // fills to last col -> pending_wrap set

        // --- tab stops: clear all then set one via HTS ---
        t.process(b"\x1b[4;1H"); // move somewhere safe
        t.process(b"\x1b[3g"); // TBC 3: clear all tab stops
        t.process(b"\x1b[1;5H\x1bH"); // move to col 5, HTS sets a tab stop here

        // --- charset designation: G0 = DEC special graphics ---
        t.process(b"\x1b(0");

        // --- kitty keyboard: push flags ---
        t.process(b"\x1b[>5u");

        // --- XTMODKEYS (xterm keyboard) ---
        t.process(b"\x1b[>4;2m");

        // --- taskbar progress (ConEmu OSC 9;4): this engine has NO byte-stream
        //     handler for OSC 9;4 (handle_osc_9 explicitly ignores `9;4;...`;
        //     the field is otherwise only cleared by reset). It is a host-set
        //     leaf, so we set it directly to guarantee a non-default captured
        //     value and exercise its capture/restore path. Honest note: this
        //     field round-trips by value but is not driver-reachable today. ---
        t.taskbar_progress = Some(TaskbarProgress::Normal(42));

        // --- secure keyboard entry: also host-set (no OSC); use the setter ---
        t.set_secure_keyboard_entry(true);

        // --- OSC 7 cwd ---
        t.process(b"\x1b]7;file://host/tmp/work\x07");

        // --- alt screen: enter 1049, write into alt, then we keep alt active ---
        t.process(b"\x1b[?1049h");
        t.process(b"ALT-SCREEN-CONTENT\r\n");

        assert!(
            t.parser_is_ground(),
            "test stream must leave parser in Ground state"
        );
        t
    }

    /// Read a cell's (char, flags, fg, bg) at (row, col) on the ACTIVE grid.
    fn cell_signature(
        t: &Terminal,
        row: u16,
        col: u16,
    ) -> (char, CellFlags, PackedColor, PackedColor) {
        let cell = *t
            .grid()
            .row(row)
            .and_then(|r| r.get(col))
            .expect("cell in range");
        (
            cell.char(),
            cell.flags(),
            cell.fg_color().unwrap_or(PackedColor::DEFAULT_FG),
            cell.bg_color().unwrap_or(PackedColor::DEFAULT_BG),
        )
    }

    #[test]
    fn checkpoint_roundtrip_full_projection() {
        let (rows, cols) = (12u16, 40u16);
        let t = build_rich_terminal(rows, cols);

        // Sanity: we actually captured non-default state.
        assert!(t.modes.alternate_screen, "alt screen active at capture");
        assert!(t.secure_keyboard_entry, "secure input captured");
        assert!(t.taskbar_progress.is_some(), "taskbar captured");
        assert!(t.current_working_directory.is_some(), "cwd captured");
        assert!(
            t.alt_grid.is_some(),
            "alt grid present (main saved under alt)"
        );

        let c0 = t.checkpoint();
        let h = Terminal::from_checkpoint(&c0, HostBindings::none());
        let c1 = h.checkpoint();

        // (A) re-checkpoint equality — the ship gate.
        assert_eq!(c0, c1, "re-checkpoint equality (c0 == c1)");

        // (B) rendered content equality.
        assert_eq!(
            t.visible_content(),
            h.visible_content(),
            "visible_content equal"
        );
        for r in 0..rows as usize {
            assert_eq!(t.row_text(r), h.row_text(r), "row_text equal for row {r}");
        }

        // cursor equality.
        assert_eq!(t.cursor(), h.cursor(), "cursor equal");

        // scroll region + modes + charset equality.
        assert_eq!(
            t.grid().scroll_region(),
            h.grid().scroll_region(),
            "scroll region equal"
        );
        assert_eq!(t.modes, h.modes, "modes equal");
        assert_eq!(t.charset, h.charset, "charset equal");
        assert_eq!(t.taskbar_progress, h.taskbar_progress, "taskbar equal");
        assert_eq!(
            t.secure_keyboard_entry, h.secure_keyboard_entry,
            "secure input equal"
        );
        assert_eq!(
            t.current_working_directory, h.current_working_directory,
            "cwd equal"
        );
        assert_eq!(t.xterm_keyboard, h.xterm_keyboard, "xterm keyboard equal");
        assert_eq!(
            t.kitty_keyboard.snapshot(),
            h.kitty_keyboard.snapshot(),
            "kitty keyboard equal"
        );
        assert_eq!(
            t.grid().tab_stops(),
            h.grid().tab_stops(),
            "tab stops equal"
        );
        assert_eq!(
            t.grid().pending_wrap(),
            h.grid().pending_wrap(),
            "pending wrap equal"
        );
    }

    #[test]
    fn visible_checkpoint_restores_screen_without_carrying_history() {
        let (rows, cols) = (12u16, 40u16);
        let t = build_rich_terminal(rows, cols);
        let full = t.checkpoint();
        let visible = t.checkpoint_visible().expect("parser is Ground");

        assert_eq!(deserialize_lines(&visible.grid).len(), rows as usize);
        let full_bytes = full.grid.len() + full.alt_grid.as_ref().map_or(0, Vec::len);
        let visible_bytes = visible.grid.len() + visible.alt_grid.as_ref().map_or(0, Vec::len);
        assert!(
            visible_bytes < full_bytes,
            "deep saved/main-grid history is excluded from handoff carry"
        );
        if let Some(alt) = visible.alt_grid.as_ref() {
            assert_eq!(deserialize_lines(alt).len(), rows as usize);
        }

        let restored = Terminal::from_checkpoint(&visible, HostBindings::none());
        assert_eq!(t.visible_content(), restored.visible_content());
        assert_eq!(t.cursor(), restored.cursor());
        assert_eq!(
            Some(visible),
            restored.checkpoint_visible(),
            "restored parser remains Ground"
        );
    }

    #[test]
    fn visible_checkpoint_rejects_partial_parser_sequence() {
        let mut terminal = Terminal::new(4, 20);
        terminal.process(b"prefix\x1b[");
        assert!(!terminal.parser_is_ground());
        assert!(
            terminal.checkpoint_visible().is_none(),
            "shipping handoff must fail closed instead of dropping parser bytes"
        );
        terminal.process(b"0m");
        assert!(terminal.parser_is_ground());
        assert!(terminal.checkpoint_visible().is_some());
    }

    #[test]
    fn visible_checkpoint_is_live_frame_not_scrolled_viewport_and_continues() {
        let mut terminal = Terminal::new(4, 20);
        for line in 0..10 {
            terminal.process(format!("line-{line}\r\n").as_bytes());
        }
        let live = terminal
            .checkpoint_visible()
            .expect("Ground at live bottom");
        terminal.scroll_display(3);
        assert!(terminal.grid().display_offset() > 0);
        let scrolled = terminal
            .checkpoint_visible()
            .expect("Ground while scrolled");
        assert_eq!(
            live, scrolled,
            "viewport navigation cannot replace the PTY's live continuation frame"
        );

        let mut restored = Terminal::from_checkpoint(&scrolled, HostBindings::none());
        terminal.process(b"tail");
        restored.process(b"tail");
        assert_eq!(
            terminal.checkpoint_visible(),
            restored.checkpoint_visible(),
            "restored live frame continues identically after new PTY output"
        );
    }

    #[test]
    fn decsc_saved_cursor_survives_handoff_and_decrc_continuation() {
        let mut terminal = Terminal::new(6, 24);
        terminal.process(b"\x1b[3;5H\x1b[1;31m\x1b7");
        terminal.process(b"\x1b[H\x1b[0mchanged");
        let checkpoint = terminal.checkpoint_visible().expect("Ground after DECSC");
        assert!(checkpoint.saved_cursor_main.is_some());
        let mut restored = Terminal::from_checkpoint(&checkpoint, HostBindings::none());

        let continuation = b"\x1b8X";
        terminal.process(continuation);
        restored.process(continuation);
        assert_eq!(terminal.cursor(), restored.cursor());
        assert_eq!(
            cell_signature(&terminal, 2, 4),
            cell_signature(&restored, 2, 4)
        );
        assert_eq!(
            terminal.checkpoint_visible(),
            restored.checkpoint_visible(),
            "DECRC and subsequent styled output must remain equivalent"
        );
    }

    #[test]
    fn checkpoint_post_hydration_styled_write_matches() {
        // Proves the writer caches were correctly rebuilt: a styled write on
        // both the source and the hydrated terminal must produce identical
        // cells. (If from_checkpoint had left the caches or the BCE cursor
        // template unrebuilt, the hydrated write would diverge.)
        let (rows, cols) = (10u16, 30u16);
        let mut t = build_rich_terminal(rows, cols);
        let c0 = t.checkpoint();
        let mut h = Terminal::from_checkpoint(&c0, HostBindings::none());

        // Same styled write to both (move home, set a fresh distinctive style).
        let seq = b"\x1b[H\x1b[1;3;38;2;1;2;3;48;5;9mQ\x1b[0m";
        t.process(seq);
        h.process(seq);

        assert!(t.parser_is_ground() && h.parser_is_ground());

        let ts = cell_signature(&t, 0, 0);
        let hs = cell_signature(&h, 0, 0);
        assert_eq!(
            ts, hs,
            "post-hydration styled cell identical (char, flags, fg, bg)"
        );

        // And re-checkpoints still agree after the identical follow-on writes.
        assert_eq!(
            t.checkpoint(),
            h.checkpoint(),
            "post-write re-checkpoint equality"
        );
    }

    #[test]
    fn checkpoint_alt_screen_toggle_on_hydrated() {
        // Toggle alt-screen OFF on the hydrated terminal and confirm it tracks
        // the source doing the same — the main grid (saved under alt at capture)
        // must come back identically.
        let (rows, cols) = (10u16, 30u16);
        let mut t = build_rich_terminal(rows, cols);
        let c0 = t.checkpoint();
        let mut h = Terminal::from_checkpoint(&c0, HostBindings::none());

        // Exit alt screen on both.
        t.process(b"\x1b[?1049l");
        h.process(b"\x1b[?1049l");

        assert!(!t.modes.alternate_screen && !h.modes.alternate_screen);
        assert_eq!(
            t.visible_content(),
            h.visible_content(),
            "main screen restored identically after alt toggle"
        );
        for r in 0..rows as usize {
            assert_eq!(
                t.row_text(r),
                h.row_text(r),
                "row {r} equal after alt toggle"
            );
        }
        assert_eq!(
            t.checkpoint(),
            h.checkpoint(),
            "re-checkpoint equal after alt toggle"
        );
    }

    #[test]
    fn checkpoint_no_alt_grid_when_absent() {
        // A plain terminal that never entered alt screen has no alt grid in the
        // checkpoint, and still round-trips.
        let mut t = Terminal::new(6, 20);
        t.process(b"hello\r\nworld\r\n");
        assert!(t.parser_is_ground());
        let c0 = t.checkpoint();
        assert!(c0.alt_grid.is_none(), "no alt grid captured");
        assert!(c0.alt_cursor.is_none());

        let h = Terminal::from_checkpoint(&c0, HostBindings::none());
        assert_eq!(c0, h.checkpoint(), "re-checkpoint equal (no alt)");
        assert_eq!(t.visible_content(), h.visible_content());
        assert!(h.alt_grid.is_none(), "hydrated has no alt grid");
    }

    #[test]
    fn checkpoint_roundtrips_decslrm_horizontal_margins() {
        // DECSLRM left/right margins live in the grid cursor_state alongside the
        // DECSTBM scroll region. They are NOT recoverable from any other captured
        // field, so the projection must carry them explicitly — otherwise a
        // checkpoint taken under DECLRMM (mode 69) restores with the mode flag on
        // but FULL-width margins, and margin-aware wrap/clamp/ICH/DCH/scroll
        // diverge from the live engine (checkpoint() != replay.checkpoint()).
        let (rows, cols) = (10u16, 40u16);
        let mut t = Terminal::new(rows, cols);
        // Enable DECLRMM (mode 69), then set non-default horizontal margins
        // (cols 5..=30, 1-based) via DECSLRM.
        t.process(b"\x1b[?69h");
        t.process(b"\x1b[5;30s");
        assert!(t.parser_is_ground());

        // Sanity: the live grid actually holds the margins we set (1-based DECSLRM
        // 5..=30 maps to 0-based 4..=29).
        let live = t.grid().horizontal_margins();
        assert_eq!((live.left, live.right), (4, 29), "margins set on live grid");

        let c0 = t.checkpoint();
        assert_eq!(
            (c0.cursor.margin_left, c0.cursor.margin_right),
            (4, 29),
            "margins captured into the checkpoint projection"
        );

        let h = Terminal::from_checkpoint(&c0, HostBindings::none());
        let restored = h.grid().horizontal_margins();
        assert_eq!(
            (restored.left, restored.right),
            (4, 29),
            "margins restored onto the hydrated grid"
        );
        assert_eq!(
            c0,
            h.checkpoint(),
            "re-checkpoint identity holds with horizontal margins"
        );
    }

    #[test]
    fn checkpoint_roundtrips_on_main_with_retained_alt_buffer() {
        // Mode 47 leaves you on the MAIN screen while a populated alt buffer is
        // RETAINED in `alt_grid` (xterm keeps the alternate buffer for re-entry).
        // This exercises the `!alternate_screen` branch of from_checkpoint's
        // is_alt determination: the ACTIVE grid (main) must keep its scrollback
        // ring, while the SAVED grid (the retained alt buffer) must restore with
        // NO scrollback ring. A slot-keyed restore (grid→main, alt_grid→alt) gets
        // this case right by luck but the symmetric 1049 case (on alt) wrong — so
        // pin BOTH polarities. Here the polarity is inverted vs the 1049 tests.
        let (rows, cols) = (8u16, 24u16);
        let mut t = Terminal::new(rows, cols);
        // Fill main with more than `rows` lines so it carries real scrollback.
        for i in 0..(rows as usize + 5) {
            t.process(format!("main{i}\r\n").as_bytes());
        }
        // Enter mode-47 alt, scribble, then EXIT back to main (47 retains alt).
        t.process(b"\x1b[?47h");
        t.process(b"ALT-LINE\r\n");
        t.process(b"\x1b[?47l");
        assert!(t.parser_is_ground());
        assert!(!t.modes.alternate_screen, "back on the main screen");
        assert!(
            t.alt_grid.is_some(),
            "alt buffer retained in alt_grid after mode-47 exit"
        );

        let c0 = t.checkpoint();
        assert!(c0.alt_grid.is_some(), "retained alt buffer captured");
        let h = Terminal::from_checkpoint(&c0, HostBindings::none());
        assert!(!h.modes.alternate_screen, "hydrated stays on main");
        assert!(h.alt_grid.is_some(), "hydrated retains the alt buffer");
        assert_eq!(
            c0,
            h.checkpoint(),
            "re-checkpoint identity (on main, alt buffer retained)"
        );
        assert_eq!(
            t.visible_content(),
            h.visible_content(),
            "main content equal after restore"
        );
    }

    #[test]
    fn restored_alt_screen_accrues_no_scrollback_when_scrolled() {
        // #5 behavioral proof: a restored ALT screen must accrue NO scrollback
        // when scrolled, exactly like a live alt screen. The bug this guards is a
        // restored alt buffer handed a 1000-line ring — re-checkpoint identity
        // alone CANNOT see it (the captured bytes never carry alt scrollback),
        // so we must actually scroll the restored buffer and compare to the live
        // engine. With a phantom 1000-ring the restored buffer would diverge from
        // `replay_from_checkpoint_matches_live_engine` the moment it scrolls.
        let (rows, cols) = (6u16, 16u16);
        let mut t = Terminal::new(rows, cols);
        t.process(b"\x1b[?1049h"); // enter alt screen (active grid = alt, 0-ring)
        assert!(t.parser_is_ground() && t.modes.alternate_screen);

        let c0 = t.checkpoint();
        let mut h = Terminal::from_checkpoint(&c0, HostBindings::none());
        assert!(h.modes.alternate_screen, "hydrated is on the alt screen");

        // Scroll BOTH well past `rows` lines.
        for i in 0..(rows as usize * 3) {
            let line = format!("s{i}\r\n");
            t.process(line.as_bytes());
            h.process(line.as_bytes());
        }
        assert!(t.parser_is_ground() && h.parser_is_ground());

        assert_eq!(
            t.grid().scrollback_lines(),
            0,
            "live alt screen accrues no scrollback"
        );
        assert_eq!(
            h.grid().scrollback_lines(),
            0,
            "restored alt screen must ALSO accrue no scrollback (#5: 0-ring, not 1000)"
        );
        assert_eq!(
            t.checkpoint(),
            h.checkpoint(),
            "restored alt screen stays in lockstep with the live engine after scrolling"
        );
    }

    #[test]
    fn in_place_restore_advances_content_scroll_epoch_once() {
        let mut source = Terminal::new(3, 12);
        source.process(b"restored");
        let checkpoint = source.checkpoint();

        let mut live = Terminal::new(3, 12);
        live.process(b"\x1b[3;1H\n");
        let before = live.content_scroll_state();
        live.restore_checkpoint(&checkpoint);
        let after = live.content_scroll_state();
        assert_eq!(after.uniform_up_rows, before.uniform_up_rows);
        assert_eq!(
            after.invalidation_epoch,
            before.invalidation_epoch + 1,
            "wholesale in-place grid replacement invalidates existing host coordinates"
        );

        live.process(b"");
        assert_eq!(
            live.content_scroll_state(),
            after,
            "the restored grid carries no latent scroll sentinel to double-report"
        );
    }
}
