// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal struct definition and root state accessors.

use super::callbacks::WindowCallback;
use super::grouped_state::SixelState;
use super::grouped_state::{
    BiDiGroupState, ClipboardState, ColorState, CursorSaveState, DcsState, Iterm2State, MarksState,
    NotificationState, ShellIntegrationState, TitleState,
};
use super::transient_state::TransientState;
use super::types::{CurrentStyle, TaskbarProgress, TerminalModes};

use crate::grid::Grid;
use crate::parser::Parser;

use aterm_types::charset::CharacterSetState;
use aterm_types::{KittyKeyboardState, XtermKeyboardState};

pub use aterm_grid::RowBandMove;

/// Number of row-band moves retained in [`ContentScrollState::bands`].
pub const CONTENT_SCROLL_BAND_RING: usize = 16;

/// Cumulative, non-consuming summary of terminal content-coordinate motion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentScrollState {
    /// Total rows of composable, full-screen upward motion on the active screen.
    pub uniform_up_rows: u64,
    /// Number of coordinate-invalidating content-motion batches observed.
    pub invalidation_epoch: u64,
    /// Invalidating batches whose whole row motion is recorded in [`bands`](Self::bands).
    pub band_batches: u64,
    /// Total band moves recorded; `bands[seq % 16]` holds move `seq`.
    pub band_seq: u64,
    /// The ring of the most recent [`CONTENT_SCROLL_BAND_RING`] moves.
    pub bands: [RowBandMove; CONTENT_SCROLL_BAND_RING],
    /// Full clears of the primary screen from home (ED 2 or ED 0 with the
    /// cursor at the top-left: `clear`, Ctrl-L). No row moved, so
    /// [`Self::delta_since`] reads none of them, but no row kept its content:
    /// whatever a host anchored to the old screen's letters has nothing left
    /// under it. An ED 2 with the cursor elsewhere is a repaint in place (a
    /// prompt's redraw, an input box moved by a program), whose letters come
    /// back where they were: the owner's 2026-09-10 report "when the screen
    /// is refreshed the rainbow disapears" (`an_erase_does_not_reset_the_cursor_effects_and_the_ribbon_survives_it`).
    pub screen_clears: u64,
}

/// Host coordinate update action between two [`ContentScrollState`] snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentScrollDelta {
    /// No previous snapshot: adopt `current` and translate nothing.
    Baseline,
    /// Nothing moved.
    Unchanged,
    /// The whole screen moved up by exactly this many rows (no epoch step).
    Translate(u16),
    /// Every epoch step since `previous` is explained by `bands[first_seq..first_seq + count]`.
    Bands {
        /// Sequence number of the oldest move to replay.
        first_seq: u64,
        /// How many moves to replay, at most [`CONTENT_SCROLL_BAND_RING`].
        count: u8,
    },
    /// Something moved rows that no exact transform describes: discard.
    Invalidate,
}

impl ContentScrollState {
    #[inline]
    pub(super) const fn record_uniform_up(&mut self, rows: u64) {
        self.uniform_up_rows = self.uniform_up_rows.saturating_add(rows);
    }

    #[inline]
    pub(super) const fn invalidate(&mut self) {
        self.invalidation_epoch = self.invalidation_epoch.saturating_add(1);
    }

    /// Record that `invalidate`'s epoch step is explained by `moves` and append them.
    pub(super) fn record_band_batch(&mut self, moves: &[RowBandMove]) {
        for &m in moves {
            let slot = usize::try_from(self.band_seq % CONTENT_SCROLL_BAND_RING as u64)
                .unwrap_or_default();
            if let Some(entry) = self.bands.get_mut(slot) {
                *entry = m;
            }
            self.band_seq = self.band_seq.wrapping_add(1);
        }
        self.band_batches = self.band_batches.saturating_add(1);
    }

    /// Band move with sequence number `seq` (valid for the last 16 moves).
    #[must_use]
    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn band(&self, seq: u64) -> RowBandMove {
        let slot = (seq % CONTENT_SCROLL_BAND_RING as u64) as usize;
        self.bands[slot]
    }

    /// Monotonic counter advancing whenever content rows move under active screen coordinates.
    #[must_use]
    #[inline]
    pub const fn coordinate_generation(&self) -> u64 {
        self.uniform_up_rows.wrapping_add(self.invalidation_epoch)
    }

    /// Compute the [`ContentScrollDelta`] between `previous` and `current`.
    #[must_use]
    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn delta_since(previous: Option<Self>, current: Self) -> ContentScrollDelta {
        let Some(p) = previous else {
            return ContentScrollDelta::Baseline;
        };
        if current.uniform_up_rows < p.uniform_up_rows
            || current.invalidation_epoch < p.invalidation_epoch
            || current.band_batches < p.band_batches
            || current.band_seq < p.band_seq
        {
            return ContentScrollDelta::Invalidate;
        }
        let d_u = current.uniform_up_rows - p.uniform_up_rows;
        let d_e = current.invalidation_epoch - p.invalidation_epoch;
        let d_bb = current.band_batches - p.band_batches;
        let d_bs = current.band_seq - p.band_seq;
        if d_e == 0 {
            if d_bs != 0 || d_bb != 0 {
                return ContentScrollDelta::Invalidate;
            }
            return if d_u == 0 {
                ContentScrollDelta::Unchanged
            } else if d_u <= u16::MAX as u64 {
                ContentScrollDelta::Translate(d_u as u16)
            } else {
                ContentScrollDelta::Invalidate
            };
        }
        if d_u == 0 && d_e == d_bb && d_bs >= 1 && d_bs <= CONTENT_SCROLL_BAND_RING as u64 {
            ContentScrollDelta::Bands {
                first_seq: p.band_seq,
                count: d_bs as u8,
            }
        } else {
            ContentScrollDelta::Invalidate
        }
    }
}

/// Host values last applied for app-negotiated DEC modes (7, 1004, 2004).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent host value per DEC mode, not a state machine"
)]
pub(super) struct ConfiguredModes {
    /// Host value last applied for DECAWM (mode 7).
    pub(super) auto_wrap: bool,
    /// Host value last applied for focus reporting (mode 1004).
    pub(super) focus_reporting: bool,
    /// Host value last applied for bracketed paste (mode 2004).
    pub(super) bracketed_paste: bool,
}

impl Default for ConfiguredModes {
    /// DEC power-on and `TerminalConfig` defaults.
    #[inline]
    fn default() -> Self {
        Self {
            auto_wrap: true,
            focus_reporting: false,
            bracketed_paste: false,
        }
    }
}

/// Terminal emulator: a [`Parser`] driving a [`Grid`].
pub struct Terminal {
    /// The terminal grid.
    pub(super) grid: Grid,
    /// The VT parser.
    pub(super) parser: Parser,
    /// Terminal modes.
    pub(super) modes: TerminalModes,
    /// Host config's last-applied values for app-negotiated modes ([`ConfiguredModes`]).
    pub(super) configured_modes: ConfiguredModes,
    /// Current text style.
    pub(super) style: CurrentStyle,
    /// Character set state (G0-G3, GL, single shift).
    pub(super) charset: CharacterSetState,
    /// Inactive grid (main grid while alternate screen is up, else parked alternate buffer).
    pub(super) alt_grid: Option<Grid>,
    /// Grouped cursor save/restore state (DECSC/DECRC + mode 1049).
    pub(super) cursor_save: CursorSaveState,
    /// Grouped window/icon title state and callback.
    pub(super) title: TitleState,
    /// Bell callback (called when BEL is received).
    pub(super) bell_callback: Option<Box<dyn FnMut() + Send>>,
    /// Host resolver for Kitty non-direct transmission mediums (`t=f`, `t=t`, `t=s`), reached
    /// only through a capability ([`super::kitty_auth`]).
    pub(super) kitty_file_resolver: super::kitty_auth::KittyResolverSlot,
    /// Last time a BEL callback fired, for rate limiting BEL floods.
    pub(super) last_bell_time: Option<aterm_time::Instant>,
    /// Monotonic count of bells that fired past the throttle (`subscribe events`).
    pub(super) bell_total: u64,
    /// Monotonic count of desktop notifications the program asked for, counted before the
    /// `allow_notifications` gate.
    pub(super) notify_total: u64,
    /// Host-preferred default cursor style, restored on RIS/DECSTR.
    pub(super) default_cursor_style: aterm_types::CursorStyle,
    /// Host default for alternate scroll (DEC 1007), restored on RIS, DECSTR, and handback.
    pub(super) host_alternate_scroll: bool,
    /// Grouped notification state (OSC 9, OSC 99, OSC 777).
    pub(super) notifications: NotificationState,
    /// Grouped OSC 52 clipboard callback state.
    pub(super) clipboard: ClipboardState,
    /// Grouped state for Terminal OSC 1337 protocol extensions.
    pub(super) iterm2: Iterm2State,
    /// Transient state cleared on reset plus the response buffer owed to the PTY peer.
    pub(super) transient: TransientState,
    /// The observation kernel: armed `await`/quiescence watchers over `content_seq`.
    pub(super) watchers: super::observe::WatcherSet,
    /// Reusable scratch for the observation kernel's per-batch row scan.
    pub(super) row_text_scratch: Vec<Option<String>>,
    /// Reusable viewport row bitmask for damage-scoped refills.
    pub(super) refill_mask_scratch: Vec<u64>,
    /// The alt-screen scroll-off archive and its `process_at` hook state (`alt_archive.rs`).
    pub(super) alt_archive: super::alt_archive::AltArchiveState,
    /// Current working directory path from OSC 7 (`file://hostname/path`).
    pub(super) current_working_directory: Option<String>,
    /// Grouped color state (palette, defaults, cursor, selection).
    pub(super) color: ColorState,
    /// Grouped BiDi state: configuration, resolver and per-line render cache.
    pub(super) bidi_state: BiDiGroupState,
    /// Grouped DCS (Device Control String) processing state.
    pub(super) dcs: DcsState,
    /// Grouped shell integration state (OSC 133, output blocks, command marks).
    pub(super) shell: ShellIntegrationState,
    /// Grouped marks and annotations state.
    pub(super) marks_state: MarksState,
    /// Taskbar progress state (ConEmu OSC 9;4), for the host's taskbar/dock.
    pub(super) taskbar_progress: Option<TaskbarProgress>,
    /// The program's own state report (OSC 7501, [`super::program_state`]).
    pub(super) program_state: super::program_state::ProgramStateReport,
    /// Kitty keyboard protocol state.
    pub(super) kitty_keyboard: KittyKeyboardState,
    /// xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).
    pub(super) xterm_keyboard: XtermKeyboardState,
    /// Lock-free mirror of `keyboard_mode()` and `mouse_mode()` ([`super::ModeMirror`]).
    pub(super) mode_mirror: std::sync::Arc<super::ModeMirror>,
    /// Host watchers of this terminal's frame ([`super::FrameWatchers`]).
    pub(super) frame_watchers: Option<std::sync::Arc<dyn super::FrameWatchers>>,
    /// Grouped Sixel graphics processing state.
    pub(super) sixel: SixelState,
    /// Window operations callback for CSI t (XTWINOPS).
    pub(super) window_callback: Option<WindowCallback>,
    /// Text selection, managed by the UI but kept here so scrolls and edits adjust it.
    pub(super) text_selection: crate::selection::TextSelection,
    /// Cell coordinates from the last encoded mouse report (`Terminal::note_mouse_report`).
    pub(super) mouse_report_memo: Option<super::mouse::MouseReportMemo>,
    /// Main screen selection parked while the alternate screen is active.
    pub(super) parked_text_selection: crate::selection::TextSelection,
    /// Selection restored by a grid resize undo ([`super::resize_journal::ResizeUndoSelection`]).
    pub(super) resize_undo_selection: Option<super::resize_journal::ResizeUndoSelection>,
    /// Last custody transition that fired ([`super::custody`]).
    pub(super) last_custody: Option<super::custody::CustodyTransition>,
    /// Last transition that took the reading position or highlight.
    pub(super) last_custody_change: Option<super::custody::CustodyTransition>,
    /// Last transition that took a live highlight the user did not release.
    pub(super) last_selection_taker: Option<super::custody::CustodyTransition>,
    /// Recorded reader gestures (typing, paste, scroll, selection, return to live).
    pub(super) reader_gestures: u64,
    /// Whether platform secure keyboard entry is enabled.
    pub(super) secure_keyboard_entry: bool,
    /// Synchronized-output (mode 2026) timeout; default 1 s.
    pub(super) sync_timeout_duration: std::time::Duration,
    /// Host-side capability authorization for OSC 52 clipboard set and query.
    pub(super) clipboard_auth: super::clipboard_auth::ClipboardAuth,
    /// Host-side authorization for the OSC 133 / 633 capability nonce.
    pub(super) shell_integration_auth: super::shell_integration_auth::ShellIntegrationAuth,
    /// Extra OSC 8 URI schemes authorized beyond the built-in allowlist.
    pub(super) hyperlink_auth: super::hyperlink_auth::HyperlinkAuth,
    /// Policy engine and compiled gate verdicts ([`super::policy_gates`]).
    pub(super) policy: super::policy_gates::PolicyState,
    /// Monotonic damage epoch ([`Terminal::damage_epoch`]).
    pub(super) damage_epoch: u64,
    /// The current grid damage already advanced `damage_epoch`; cleared by `take_damage`.
    pub(super) damage_epoch_counted: bool,
    /// Process-unique engine identity stamped into every render snapshot.
    pub(super) extract_identity: u64,
    /// Extraction-continuity generation, bumped by every [`take_damage`](Terminal::take_damage).
    pub(super) extract_gen: u64,
    /// Full-history search index slot ([`Terminal::search_plan`], [`Terminal::indexed_search`]).
    pub(super) search_slot: Option<super::search_index::SearchSlot>,
    /// The budgeted resumable search, if any ([`Terminal::search_budgeted`]).
    pub(super) budgeted_search: Option<super::search_budgeted::BudgetedSearchState>,
    /// Completed search-index builds ([`Terminal::search_index_rebuilds`]).
    pub(super) search_index_rebuilds: u64,
    /// Incremental refreshes among them, each also counted in `search_index_rebuilds`.
    pub(super) search_index_refreshes: u64,
    /// Monotonic revision of top-anchored protected-footer row insertions.
    pub(super) absolute_row_revision: u64,
    /// Main-screen rows written by each drawer while holding the terminal (`holder_fence`).
    pub(super) holder_fence: super::HolderFence,
    /// [`screen_mutations`](Self::screen_mutations) when a checkpoint's fence was installed.
    pub(super) holder_carried_at: Option<u64>,
    /// Monotonic repaint-blink epoch bumped on DECTCEM hide inside DEC 2026 sync output.
    pub(super) repaint_blink_epoch: u64,
    /// Foreground handback evidence bits asserted since the host last took them.
    pub(super) evidence_asserted: u16,
    /// Cumulative host projection of content-coordinate motion, updated once per batch.
    pub(super) content_scroll_state: ContentScrollState,
    /// Journal of active-grid resizes ([`super::resize_journal`]).
    pub(super) resize_journal: super::resize_journal::ResizeJournal,
    /// Journal of host-side mutations ([`super::host_journal`]).
    pub(super) host_journal: super::host_journal::HostJournal,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("grid", &self.grid)
            .field("parser", &self.parser)
            .field("modes", &self.modes)
            .field("style", &self.style)
            .field("charset", &self.charset)
            .field("title", &self.title.window)
            .finish_non_exhaustive()
    }
}

impl Terminal {
    /// The active grid.
    #[inline]
    #[must_use]
    pub const fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Main-buffer grid regardless of alternate-screen state.
    #[inline]
    #[must_use]
    pub const fn main_grid(&self) -> &Grid {
        if self.modes.alternate_screen {
            match self.alt_grid.as_ref() {
                Some(alt) => alt,
                None => &self.grid,
            }
        } else {
            &self.grid
        }
    }

    /// The active grid, mutably.
    #[inline]
    pub const fn grid_mut(&mut self) -> &mut Grid {
        self.transient.sync_end_caret = None;
        &mut self.grid
    }

    /// Whether the grid holds damage since the last [`take_damage`](Self::take_damage).
    #[must_use]
    #[inline]
    pub fn has_damage(&self) -> bool {
        self.grid.damage().has_damage()
    }

    /// Clear grid damage after a present and re-arm [`damage_epoch`](Self::damage_epoch).
    #[inline]
    pub fn take_damage(&mut self) {
        self.grid.clear_damage();
        self.damage_epoch_counted = false;
        self.extract_gen = self.extract_gen.wrapping_add(1);
    }

    /// Monotonic counter that advances at most once per damage session on net-new grid damage.
    #[inline]
    pub fn damage_epoch(&mut self) -> u64 {
        if !self.damage_epoch_counted && self.grid.damage().has_damage() {
            self.damage_epoch = self.damage_epoch.wrapping_add(1);
            self.damage_epoch_counted = true;
        }
        self.damage_epoch
    }

    /// Monotonic content generation of the active grid ([`Grid::content_gen`]).
    #[must_use]
    #[inline]
    pub const fn content_seq(&self) -> u64 {
        self.grid.content_gen()
    }

    /// Monotonic count of non-empty processed batches and Kitty animation frame steps.
    #[must_use]
    #[inline]
    pub const fn screen_mutations(&self) -> u64 {
        self.transient.screen_mutations
    }

    /// Cumulative content-scroll snapshot for diffing with [`ContentScrollState::delta_since`].
    #[must_use]
    #[inline]
    pub const fn content_scroll_state(&self) -> ContentScrollState {
        let mut state = self.content_scroll_state;
        state.screen_clears = self.transient.primary_clears;
        state
    }

    /// Revision for top-anchored protected-footer row insertions.
    #[must_use]
    #[inline]
    pub const fn absolute_row_revision(&self) -> u64 {
        self.absolute_row_revision
    }

    /// The text selection state.
    #[must_use]
    #[inline]
    pub const fn text_selection(&self) -> &crate::selection::TextSelection {
        &self.text_selection
    }

    /// The text selection state, mutably.
    #[inline]
    pub const fn text_selection_mut(&mut self) -> &mut crate::selection::TextSelection {
        &mut self.text_selection
    }

    /// Get the terminal modes.
    #[inline]
    #[must_use]
    pub const fn modes(&self) -> &TerminalModes {
        &self.modes
    }

    /// Monotonic count of synchronized-update (mode 2026) closes.
    #[inline]
    #[must_use]
    pub const fn sync_end_seq(&self) -> u64 {
        self.transient.sync_end_seq
    }

    /// Cursor cell at the latest ordered synchronized-output close on this screen and grid.
    #[inline]
    #[must_use]
    pub fn sync_end_caret(&self) -> Option<(u16, u16)> {
        self.transient
            .sync_end_caret
            .filter(|end| {
                end.seq == self.transient.sync_end_seq
                    && end.screen == self.transient.screen_replaced
                    && end.grid == (self.grid.rows(), self.grid.cols())
            })
            .map(|end| end.caret)
    }

    /// Whether the open synchronized-output window accepted a complete PTY action since `?2026h`.
    #[inline]
    #[must_use]
    pub const fn sync_open_dirty(&self) -> bool {
        self.modes.synchronized_output && self.transient.sync_open_dirty
    }

    /// Whether the program's DEC 2026 bracket is open with content written into it.
    #[inline]
    #[must_use]
    pub const fn sync_frame_unfinished(&self) -> bool {
        self.transient.app_sync_open && self.transient.app_sync_dirty
    }

    /// Whether a whole-frame reader must wait at `now` for an unfinished DEC 2026 frame.
    #[inline]
    #[must_use]
    pub fn frame_unfinished_at(&self, now: aterm_time::Instant) -> bool {
        self.frame_hang_deadline().is_some_and(|hang| now < hang)
    }

    /// Deadline when the unfinished frame's hang detector fires ([`Self::frame_unfinished_at`]).
    #[inline]
    #[must_use]
    pub fn frame_hang_deadline(&self) -> Option<aterm_time::Instant> {
        if !self.sync_frame_unfinished() {
            return None;
        }
        let since = self.transient.frame_unfinished_since?;
        Some(since.checked_add(super::FRAME_HANG).unwrap_or(since))
    }

    /// DEC 2026 timeout (`TerminalConfig::sync_timeout_ms`) for waiting on an unfinished frame.
    #[inline]
    #[must_use]
    pub const fn sync_timeout(&self) -> std::time::Duration {
        self.sync_timeout_duration
    }

    /// Whether the screen holds a DEC 2026 frame closed by `?2026l` with no motion since.
    #[inline]
    #[must_use]
    pub fn screen_is_closed_frame(&self) -> bool {
        !self.sync_frame_unfinished()
            && self.transient.app_sync_closed
                == Some(super::transient_state::ClosedFrame {
                    content_gen: self.grid.content_gen(),
                    screen: self.transient.screen_replaced,
                    grid: (self.grid.rows(), self.grid.cols()),
                })
    }

    /// Record the end of a processed batch for unfinished-frame and drawn-time tracking.
    pub(super) fn note_batch_end(&mut self) {
        let now = self.transient.process_now;
        if self.sync_frame_unfinished() {
            self.transient.frame_unfinished_since.get_or_insert(now);
        } else {
            self.transient.frame_unfinished_since = None;
        }
        let mark = (self.grid.content_gen(), self.transient.screen_replaced);
        if self.transient.drawn.is_none_or(|(_, seen)| seen != mark) {
            self.transient.drawn = Some((now, mark));
        }
    }

    /// When the screen last changed ([`Self::note_batch_end`]); `None` before any batch.
    #[inline]
    #[must_use]
    pub fn drawn_at(&self) -> Option<aterm_time::Instant> {
        self.transient.drawn.map(|(at, _)| at)
    }

    /// Whether the VT parser is in Ground state outside a tmux passthrough wrapper.
    #[inline]
    #[must_use]
    pub const fn parser_is_ground(&self) -> bool {
        self.parser.state().is_ground() && !self.parser.tmux_passthrough().is_open()
    }

    /// Whether the alternate screen is active.
    #[inline]
    #[must_use]
    pub const fn is_alternate_screen(&self) -> bool {
        self.modes.alternate_screen
    }
}

#[cfg(test)]
mod damage_epoch_tests {
    use super::Terminal;

    #[test]
    fn damage_epoch_advances_on_write_not_on_noop() {
        let mut term = Terminal::new(4, 10);

        assert!(term.has_damage(), "fresh terminal must start damaged");
        let e0 = term.damage_epoch();
        assert_eq!(e0, 1, "first observed epoch is 1");
        assert_eq!(term.damage_epoch(), e0, "epoch is stable within a session");

        term.take_damage();
        assert!(!term.has_damage(), "take_damage clears grid damage");
        assert_eq!(term.damage_epoch(), e0, "no new damage => epoch unchanged");

        term.process(b"hello");
        assert!(term.has_damage(), "a write must damage the grid");
        let e1 = term.damage_epoch();
        assert_eq!(e1, e0 + 1, "a write advances the epoch by one");
        assert_eq!(term.damage_epoch(), e1, "still one session until cleared");

        term.take_damage();
        assert_eq!(term.damage_epoch(), e1, "cleared => epoch holds");

        term.process(b"");
        assert!(!term.has_damage(), "an empty write damages nothing");
        assert_eq!(
            term.damage_epoch(),
            e1,
            "no-op write must NOT advance the epoch"
        );

        term.process(b"world");
        let e2 = term.damage_epoch();
        assert_eq!(e2, e1 + 1, "the next write advances the epoch again");
        assert!(e2 > e1, "epoch is monotonic");
    }

    #[test]
    fn content_seq_advances_on_content_not_on_viewport_scroll() {
        let mut term = Terminal::new(3, 10);

        let g0 = term.content_seq();
        assert!(g0 > 0, "content_gen is initialized nonzero");

        term.process(b"hello");
        let g1 = term.content_seq();
        assert!(g1 > g0, "a cell write advances content_gen");

        term.process(b"");
        assert_eq!(
            term.content_seq(),
            g1,
            "a no-op process must NOT advance content_gen"
        );

        term.process(b"\x1b[K");
        let g2 = term.content_seq();
        assert!(g2 > g1, "a line erase advances content_gen");

        for _ in 0..10 {
            term.process(b"line\r\n");
        }
        let g3 = term.content_seq();
        assert!(g3 > g2, "a content scroll (scroll_up) advances content_gen");

        let epoch_before = term.damage_epoch();
        term.take_damage();
        term.grid_mut().scroll_display(2);
        assert!(
            term.has_damage(),
            "a viewport scroll still damages the grid (for the renderer)"
        );
        assert!(
            term.damage_epoch() > epoch_before,
            "a viewport scroll DOES advance damage_epoch"
        );
        assert_eq!(
            term.content_seq(),
            g3,
            "a pure viewport scroll must NOT advance content_gen"
        );

        term.grid_mut().scroll_display(0);
        assert_eq!(
            term.content_seq(),
            g3,
            "a no-op viewport scroll must NOT advance content_gen"
        );

        term.process(b"x");
        let g4 = term.content_seq();
        assert!(g4 > g3, "content_gen is monotonic across further writes");
    }
}

#[cfg(test)]
mod content_scroll_delta_tests {
    use super::{CONTENT_SCROLL_BAND_RING, ContentScrollDelta, ContentScrollState, RowBandMove};
    use aterm_models_core::content_scroll_band_history_model as band_history_model;

    #[test]
    fn band_history_model_proves_and_catches_overwritten_replay() {
        let model = band_history_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn terminal_band_history_conforms_through_overflow_and_rebaseline() {
        use crate::terminal::TerminalBuilder;
        let model = band_history_model();
        assert_eq!(
            model
                .consts
                .iter()
                .find(|(name, _)| *name == "Cap")
                .unwrap()
                .1,
            CONTENT_SCROLL_BAND_RING as i64
        );
        let mut caught_overflow = 0;
        let mut caught_unexplained = 0;
        for poison_at in [None, Some(0), Some(1), Some(8), Some(16), Some(17)] {
            let mut term = TerminalBuilder::new()
                .size(57, 151)
                .ring_buffer_size(1000)
                .build();
            let mut baseline = term.content_scroll_state();
            let mut expected = model.init_state();
            for _ in 0..2 {
                let mut written = Vec::new();
                for step in 0..=CONTENT_SCROLL_BAND_RING + 1 {
                    if poison_at == Some(step) {
                        term.process(b"\x1bc");
                        assert!(model.fire("Poison", &mut expected));
                    }
                    if step > 0 {
                        let top = (step % 3) as u16;
                        let bytes = format!("\x1b[{};52r\x1b[52;1H\n", top + 1);
                        term.process(bytes.as_bytes());
                        written.push(RowBandMove {
                            top,
                            bottom: 51,
                            delta: -1,
                        });
                        assert!(model.fire("Step", &mut expected));
                    }
                    let current = term.content_scroll_state();
                    let actual = match ContentScrollState::delta_since(Some(baseline), current) {
                        ContentScrollDelta::Unchanged => 0,
                        ContentScrollDelta::Bands { first_seq, count } => {
                            assert_eq!(first_seq, baseline.band_seq);
                            assert_eq!(usize::from(count), written.len());
                            for (offset, &movement) in written.iter().enumerate() {
                                assert_eq!(current.band(first_seq + offset as u64), movement);
                            }
                            1
                        }
                        ContentScrollDelta::Invalidate => 2,
                        other => panic!("unexpected reader decision {other:?}"),
                    };
                    assert_eq!(
                        actual, expected["verdict"],
                        "step {step}, poison {poison_at:?}"
                    );
                    assert_eq!(
                        current.band_seq - baseline.band_seq,
                        expected["moves"] as u64
                    );
                    if actual == 2 {
                        let mut unsafe_replay = expected.clone();
                        unsafe_replay.insert("verdict", 1);
                        assert!(!model.check_invariant("ReplayComplete", &unsafe_replay));
                        if expected["unexplained"] == 1 {
                            caught_unexplained += 1;
                        } else {
                            caught_overflow += 1;
                        }
                    }
                }
                baseline = term.content_scroll_state();
                assert!(model.fire("Baseline", &mut expected));
                assert_eq!(
                    ContentScrollState::delta_since(Some(baseline), baseline),
                    ContentScrollDelta::Unchanged
                );
            }
        }
        assert_eq!(caught_overflow, 2);
        assert!(caught_unexplained > 0);
    }

    fn state(uniform: u64, epoch: u64, batches: u64, seq: u64) -> ContentScrollState {
        ContentScrollState {
            uniform_up_rows: uniform,
            invalidation_epoch: epoch,
            band_batches: batches,
            band_seq: seq,
            ..Default::default()
        }
    }

    #[test]
    fn delta_since_replays_only_fully_explained_epochs() {
        let prev = state(10, 4, 2, 7);
        assert_eq!(
            ContentScrollState::delta_since(None, prev),
            ContentScrollDelta::Baseline
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), prev),
            ContentScrollDelta::Unchanged
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(13, 4, 2, 7)),
            ContentScrollDelta::Translate(3)
        );
        assert_eq!(
            ContentScrollState::delta_since(
                Some(prev),
                state(10 + u64::from(u16::MAX) + 1, 4, 2, 7)
            ),
            ContentScrollDelta::Invalidate,
            "an unrepresentable exact delta retires instead of under-translating"
        );

        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 8)),
            ContentScrollDelta::Bands {
                first_seq: 7,
                count: 1
            }
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 12)),
            ContentScrollDelta::Bands {
                first_seq: 7,
                count: 5
            }
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 9, 7, 12)),
            ContentScrollDelta::Bands {
                first_seq: 7,
                count: 5
            }
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 6, 3, 8)),
            ContentScrollDelta::Invalidate
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(13, 5, 3, 8)),
            ContentScrollDelta::Invalidate,
            "later uniform output cannot hide an intervening non-uniform batch"
        );
        let ring = CONTENT_SCROLL_BAND_RING as u64;
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 7 + ring)),
            ContentScrollDelta::Bands {
                first_seq: 7,
                count: 16
            }
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 7 + ring + 1)),
            ContentScrollDelta::Invalidate,
            "seventeen moves between presents overflow the ring: today's reset"
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 7)),
            ContentScrollDelta::Invalidate
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 4, 2, 8)),
            ContentScrollDelta::Invalidate
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 4, 3, 7)),
            ContentScrollDelta::Invalidate
        );
        for regressed in [
            state(9, 4, 2, 7),
            state(10, 3, 2, 7),
            state(10, 4, 1, 7),
            state(10, 4, 2, 6),
        ] {
            assert_eq!(
                ContentScrollState::delta_since(Some(prev), regressed),
                ContentScrollDelta::Invalidate
            );
        }
    }
}
