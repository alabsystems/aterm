// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal struct definition and root state accessors.

use super::callbacks::WindowCallback;
#[cfg(feature = "sixel")]
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

/// How many row-band moves [`ContentScrollState::bands`] remembers. Denser
/// bursts or a delayed present can overwrite unread moves; `delta_since` then
/// answers `Invalidate` and the host discards old coordinates.
pub const CONTENT_SCROLL_BAND_RING: usize = 16;

/// Cumulative, non-consuming summary of terminal content-coordinate motion.
///
/// Hosts keep a previous copy and diff it against a later snapshot, normally
/// through [`ContentScrollState::delta_since`]. An advance in
/// [`uniform_up_rows`](Self::uniform_up_rows) means the whole active-screen
/// viewport moved up by that many rows. An advance in
/// [`invalidation_epoch`](Self::invalidation_epoch) means "discard": a
/// non-uniform mutation moved rows.
///
/// [`band_batches`](Self::band_batches) and [`bands`](Self::bands) are an
/// optional exact transform: an epoch step whose whole row motion was a short
/// list of [`RowBandMove`]s (DECSTBM region scroll, reverse index at a region
/// top, IL/DL, the top-anchored archival scroll) is counted in `band_batches`
/// and its moves appended to the `bands` ring. A consumer that can translate
/// bands applies them, oldest first, when every epoch step since its snapshot
/// is explained (`delta_since` decides); other consumers treat any epoch
/// change as an invalidation. `record_band_batch` is only called right after
/// `invalidate`.
///
/// All counters are monotonic for the lifetime of a [`Terminal`] and survive
/// RIS and [`Terminal::reset`] (which bump the epoch without a band batch, so
/// consumers discard). Not checkpointed. `Copy`, 128 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentScrollState {
    /// Total rows of composable, full-screen upward motion on the active screen.
    pub uniform_up_rows: u64,
    /// Number of coordinate-invalidating content-motion batches observed.
    pub invalidation_epoch: u64,
    /// Invalidating batches whose whole row motion is in [`bands`](Self::bands),
    /// a subset of [`invalidation_epoch`](Self::invalidation_epoch). Counted only
    /// for a non-empty exact band record, with no whole-screen scroll, no screen
    /// switch and no splice degraded to `Invalidate` in the same batch.
    pub band_batches: u64,
    /// Band moves recorded for the Terminal's life; `bands[seq % 16]` holds move
    /// `seq`. A consumer reads the moves `previous.band_seq..current.band_seq`.
    pub band_seq: u64,
    /// The ring of the most recent [`CONTENT_SCROLL_BAND_RING`] moves.
    pub bands: [RowBandMove; CONTENT_SCROLL_BAND_RING],
}

/// What a host should do with the coordinates it cached at `previous` now that
/// the terminal reads `current`; the one decision every host calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentScrollDelta {
    /// No previous snapshot: adopt `current` and translate nothing.
    Baseline,
    /// Nothing moved.
    Unchanged,
    /// The whole screen moved up by exactly this many rows (no epoch step).
    Translate(u16),
    /// Every epoch step since `previous` is explained by the `count` band moves
    /// `bands[first_seq..first_seq + count]` (oldest first; `count` is 1..=16).
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
    pub(super) fn record_uniform_up(&mut self, rows: u64) {
        self.uniform_up_rows = self.uniform_up_rows.saturating_add(rows);
    }

    #[inline]
    pub(super) fn invalidate(&mut self) {
        self.invalidation_epoch = self.invalidation_epoch.saturating_add(1);
    }

    /// Record that the epoch step just taken (by `invalidate`) is fully explained
    /// by `moves`: append them to the ring, oldest first, and count the batch.
    /// Called only by `post_process`, right after `invalidate`, with a non-empty
    /// exact record.
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

    /// The band move with sequence number `seq` — valid for the last
    /// [`CONTENT_SCROLL_BAND_RING`] moves, i.e. `seq` in
    /// `band_seq - 16..band_seq`; older sequence numbers read whatever overwrote
    /// the slot, which is why [`Self::delta_since`] never hands out more than 16.
    #[must_use]
    #[inline]
    pub fn band(&self, seq: u64) -> RowBandMove {
        let slot = usize::try_from(seq % CONTENT_SCROLL_BAND_RING as u64).unwrap_or_default();
        self.bands.get(slot).copied().unwrap_or_default()
    }

    /// One number that changes exactly when content rows moved under the active
    /// screen's coordinates: any uniform scroll or any invalidating batch (band
    /// moves included, since a band batch is always an epoch step). For a host
    /// that only needs "did every cached row coordinate go stale" — the
    /// predictive-echo generation — rather than the exact transform.
    ///
    /// Both counters only grow, so their sum moves whenever either does: it
    /// would take a combined advance of 2^64 to come back to the same value.
    #[must_use]
    #[inline]
    pub fn coordinate_generation(&self) -> u64 {
        self.uniform_up_rows.wrapping_add(self.invalidation_epoch)
    }

    /// The host decision between two snapshots (see [`ContentScrollDelta`]).
    ///
    /// With `d_u/d_e/d_bb/d_bs` the advances of `uniform_up_rows`,
    /// `invalidation_epoch`, `band_batches` and `band_seq`:
    ///
    /// * no `previous`: `Baseline`;
    /// * any counter went backwards (another terminal's snapshot): `Invalidate`;
    /// * `d_e == 0`: `d_u == 0` is `Unchanged`, a `d_u` that fits `u16` is
    ///   `Translate`, larger is `Invalidate`; a band count moving without an
    ///   epoch step is impossible and answered `Invalidate`;
    /// * `d_e > 0`: `Bands` iff `d_u == 0` (pinned by
    ///   `delta_since_replays_only_fully_explained_epochs`), every epoch step was
    ///   a band batch (`d_e == d_bb`) and the moves fit the ring
    ///   (`1 <= d_bs <= 16`); otherwise `Invalidate`.
    #[must_use]
    pub fn delta_since(previous: Option<Self>, current: Self) -> ContentScrollDelta {
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
                // A band batch is always an epoch step; bands without one are a
                // snapshot from some other lineage.
                return ContentScrollDelta::Invalidate;
            }
            return if d_u == 0 {
                ContentScrollDelta::Unchanged
            } else if let Ok(rows) = u16::try_from(d_u) {
                ContentScrollDelta::Translate(rows)
            } else {
                ContentScrollDelta::Invalidate
            };
        }
        match u8::try_from(d_bs) {
            Ok(count)
                if d_u == 0
                    && d_e == d_bb
                    && (1..=CONTENT_SCROLL_BAND_RING).contains(&usize::from(count)) =>
            {
                ContentScrollDelta::Bands {
                    first_seq: p.band_seq,
                    count,
                }
            }
            _ => ContentScrollDelta::Invalidate,
        }
    }
}

/// The host-configuration values last applied for the three
/// [`apply_config`](Terminal::apply_config) settings an application also
/// controls over the wire: DECAWM (mode 7), focus reporting (1004) and
/// bracketed paste (2004).
///
/// For these, a live value that differs from the config usually means the
/// shell set the mode, so a reload diffs against the previous config rather
/// than re-asserting the config value. For bracketed paste this is a safety
/// property: dropping the paste guards while the shell still frames pastes
/// turns a multi-line clipboard payload into executed keystrokes.
///
/// The seed is the power-on value; RIS and DECSTR already return these modes
/// to it ([`TerminalModes::new`], `handle_decstr`).
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
    /// The host's default for alternate scroll (DEC 1007), set by
    /// [`Terminal::set_host_alternate_scroll`] (not a `TerminalConfig` field).
    /// The foreground handback restores 1007 to this value, not to power-on off.
    pub(super) alternate_scroll: bool,
}

impl Default for ConfiguredModes {
    /// The DEC power-on values, which are also `TerminalConfig`'s defaults, so
    /// applying a default config to a fresh terminal writes nothing.
    fn default() -> Self {
        Self {
            auto_wrap: true,
            focus_reporting: false,
            bracketed_paste: false,
            alternate_scroll: false,
        }
    }
}

/// Terminal emulator.
///
/// Combines a [`Parser`] and a [`Grid`] to provide full terminal emulation.
pub struct Terminal {
    /// The terminal grid.
    pub(super) grid: Grid,
    /// The VT parser.
    pub(super) parser: Parser,
    /// Terminal modes.
    pub(super) modes: TerminalModes,
    /// Host config's last-applied values for the app-negotiated modes (see
    /// [`ConfiguredModes`]). Never forwarded to the VT handler: no escape
    /// sequence may move a host setting.
    pub(super) configured_modes: ConfiguredModes,
    /// Current text style.
    pub(super) style: CurrentStyle,
    /// Character set state (G0-G3, GL, single shift).
    pub(super) charset: CharacterSetState,
    /// Alternate screen grid (for applications like vim).
    pub(super) alt_grid: Option<Grid>,
    /// Grouped cursor save/restore state (DECSC/DECRC + mode 1049).
    pub(super) cursor_save: CursorSaveState,
    /// Grouped window/icon title state and callback.
    pub(super) title: TitleState,
    /// Bell callback (called when BEL is received).
    pub(super) bell_callback: Option<Box<dyn FnMut() + Send>>,
    /// Host resolver for Kitty non-direct transmission mediums (`t=f` file,
    /// `t=t` temp file, `t=s` POSIX shared memory). The engine stays pure and
    /// wasm-safe: it hands the host `(medium, path/name)` and the host, under its
    /// own security policy, returns the image bytes or `None`. Absent by default
    /// (non-direct mediums are skipped). Set via [`Terminal::set_kitty_file_resolver`].
    #[allow(clippy::type_complexity)]
    pub(super) kitty_file_resolver: Option<
        Box<dyn Fn(crate::terminal::kitty_graphics::KittyMedium, &str) -> Option<Vec<u8>> + Send>,
    >,
    /// Last time a BEL callback was fired, for rate limiting against BEL floods.
    // aterm_time::Instant (std on native, JS clock on wasm): assigned from
    // transient.process_now, so must share its type.
    pub(super) last_bell_time: Option<aterm_time::Instant>,
    /// Monotonic count of bells that fired (past the throttle), for poll and
    /// watermark observers (`subscribe events` emits `EVENT … bell`). Unlike
    /// the take-and-clear `bell_pending`, never resets. Not checkpointed.
    pub(super) bell_total: u64,
    /// Host-preferred default cursor style: used before any DECSCUSR and
    /// restored on RIS/DECSTR. Kept on `Terminal` (not in `modes`) so it survives
    /// the `TerminalModes::new()` reset. Set via [`Terminal::set_default_cursor_style`].
    pub(super) default_cursor_style: aterm_types::CursorStyle,
    /// Grouped notification state (OSC 9, OSC 99, OSC 777).
    pub(super) notifications: NotificationState,
    /// Grouped OSC 52 clipboard callback state.
    pub(super) clipboard: ClipboardState,
    /// Grouped state for Terminal OSC 1337 protocol extensions.
    pub(super) iterm2: Iterm2State,
    /// Grouped transient state cleared on reset (hyperlink, underline color,
    /// last graphic char, VT52, sync, SGR stack), plus the response buffer,
    /// which a reset keeps: its replies are owed to the PTY peer.
    pub(super) transient: TransientState,
    /// The observation kernel: armed `await`/quiescence watchers over
    /// `content_seq`. Observation-only and never checkpointed, so it cannot
    /// affect replay determinism.
    pub(super) watchers: super::observe::WatcherSet,
    /// Reusable scratch for the observation kernel's per-batch row scan: never
    /// checkpointed, never mutates the surface. `observe_at` takes it, resizes it
    /// to the visible row count and refills it through
    /// [`row_text_into`](Self::row_text_into), reusing the allocations across
    /// batches.
    pub(super) row_text_scratch: Vec<Option<String>>,
    /// Reusable row mask for the damage-scoped refill
    /// ([`cell_frame_damage_scoped_into`](Self::cell_frame_damage_scoped_into)):
    /// one bit per viewport row, `rows.div_ceil(64)` words. Filled from the
    /// tracker's row bits (or the exposed scroll strip) before the fill, which
    /// needs `&mut self`, and taken/put back so refills reuse the capacity.
    /// Render scratch: never checkpointed, never VT state.
    pub(super) refill_mask_scratch: Vec<u64>,
    /// The alt-screen scroll-off archive and its `process_at` hook state (see
    /// `alt_archive.rs`). Observation-only: never checkpointed, never forwarded
    /// to the handler. A self-update handoff carries its tail beside the
    /// checkpoint (`alt_archive_carry_head`/`_rows`, then `alt_archive_import`).
    pub(super) alt_archive: super::alt_archive::AltArchiveState,
    /// Current working directory from OSC 7 (`file://hostname/path`), path
    /// portion only.
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
    /// Kitty keyboard protocol state.
    pub(super) kitty_keyboard: KittyKeyboardState,
    /// xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).
    pub(super) xterm_keyboard: XtermKeyboardState,
    /// Lock-free mirror of `keyboard_mode()` / `mouse_mode()` (see
    /// [`super::ModeMirror`]), shared with the session's input seam so key
    /// presses need not take this terminal's mutex. Refreshed at the end of
    /// every `process()` batch and by every host mutator that moves an input.
    pub(super) mode_mirror: std::sync::Arc<super::ModeMirror>,
    /// Grouped Sixel graphics processing state.
    #[cfg(feature = "sixel")]
    pub(super) sixel: SixelState,
    /// Window operations callback for CSI t (XTWINOPS).
    ///
    /// Called when window manipulation or query sequences are received.
    pub(super) window_callback: Option<WindowCallback>,
    /// Text selection state. Managed by the UI layer but stored here so it can
    /// be adjusted as the terminal scrolls or text changes.
    pub(super) text_selection: crate::selection::TextSelection,
    /// What the last mouse report the host encoded for this terminal carried
    /// (`Terminal::note_mouse_report`): xterm's `mouse_row`/`mouse_col`, the
    /// memo behind "motion is reported only if the pointer has moved to a
    /// different character cell". Written by the UI layer's input seam under
    /// this terminal's lock, like `text_selection`; never VT state, never
    /// checkpointed.
    pub(super) mouse_report_memo: Option<super::mouse::MouseReportMemo>,
    /// The other screen's selection, parked across an alternate-screen switch.
    ///
    /// A selection belongs to the screen it was made on. While
    /// `alternate_screen` is set this slot holds the main screen's selection.
    /// Invariant: empty whenever `alternate_screen` is false. `post_process`
    /// parks with `mem::take` on the batch that enters alt and restores with
    /// `mem::take` on the batch that leaves, so this is never a durable second
    /// selection and RIS, `clear_scrollback`, `restore_checkpoint` and resizes
    /// need not clear it separately.
    ///
    /// A batch that exits and re-enters runs neither arm; the alt screen's own
    /// selection is cleared on `alt_screen_left_in_batch`, and this slot stays
    /// parked.
    pub(super) parked_text_selection: crate::selection::TextSelection,
    /// The active selection as it stood before the first shrink of an open grid
    /// resize undo (`aterm_grid`'s `resize_undo`), restored when that undo is
    /// handed back whole: the shrink clamps an anchor on a demoted row and the
    /// grow would land it one row low. See
    /// [`super::resize_journal::ResizeUndoSelection`].
    pub(super) resize_undo_selection: Option<super::resize_journal::ResizeUndoSelection>,
    /// Press custody: which custody transition last fired (see
    /// [`super::custody`]). Written at each site that decides custody, read by
    /// the `custody` control verb and the `PressCustody` conformance. Not
    /// checkpointed: it describes this session's last event, not VT state.
    pub(super) last_custody: Option<super::custody::CustodyTransition>,
    /// Press custody: the last recorded transition that actually took the
    /// reading position or the highlight (see
    /// [`super::custody::CustodyTransition::always_takes_custody`] and
    /// `Terminal::note_custody_at`). `last_custody` is overwritten by every
    /// `OutputAtLive`, so this keeps the answer the `custody` verb needs. Not
    /// checkpointed.
    pub(super) last_custody_change: Option<super::custody::CustodyTransition>,
    /// The last transition that took a live highlight the user did not release.
    /// Separate from `last_custody_change`, which a deliberate deselect
    /// overwrites.
    pub(super) last_selection_taker: Option<super::custody::CustodyTransition>,
    /// How many reader gestures this session has recorded
    /// ([`super::custody::CustodyTransition::is_reader_gesture`]): typing, paste,
    /// scroll, selection, return to live. A counter, so output cannot hide a
    /// gesture. The self-update scrollback restore compares it to learn whether
    /// the session was used between adopt and import. Not checkpointed.
    pub(super) reader_gestures: u64,
    /// Secure keyboard entry mode: tells the UI layer to enable platform secure
    /// input (e.g. macOS `EnableSecureEventInput()`); the UI makes the calls.
    pub(super) secure_keyboard_entry: bool,
    /// Vi mode navigation state (cursor, marks, inline search).
    pub(super) vi: crate::vi_mode::ViMode,
    /// Timeout for synchronized output (mode 2026), from
    /// `TerminalConfig.sync_timeout_ms` via `apply_config()`. Default 1 s.
    pub(super) sync_timeout_duration: std::time::Duration,
    /// Host-side authorization for OSC 52 clipboard set and query (see
    /// [`super::clipboard_auth`]): the zero-sized
    /// [`super::clipboard_auth::ClipboardWriteCapability`] and
    /// [`super::clipboard_auth::ClipboardQueryCapability`] tokens are the only
    /// way a handler reaches the clipboard callback, minted only after
    /// [`super::Terminal::authorize_clipboard_access`].
    pub(super) clipboard_auth: super::clipboard_auth::ClipboardAuth,
    /// Host-side authorization for the OSC 133 / 633 shell integration
    /// capability nonce, installed via
    /// [`super::Terminal::authorize_shell_integration`]. When
    /// [`super::types::TerminalModes::require_shell_integration_nonce`] is set
    /// and `verify_nonce` rejects, the handlers drop the sequence and count it.
    /// See [`super::shell_integration_auth`].
    pub(super) shell_integration_auth: super::shell_integration_auth::ShellIntegrationAuth,
    /// Extra OSC 8 URI schemes the host authorized beyond the built-in safe
    /// allowlist ([`super::Terminal::authorize_hyperlink_scheme`]). See
    /// [`super::hyperlink_auth`]: the scheme set is the whole OSC 8 admission
    /// decision; what a click may open is decided by the host at press time.
    pub(super) hyperlink_auth: super::hyperlink_auth::HyperlinkAuth,
    /// OSC / escape-sequence policy: the engine installed via
    /// [`super::Terminal::apply_policy_engine`] plus the gate verdicts compiled
    /// from it, kept in one field so a gate never answers from a policy that is
    /// no longer installed (see [`super::policy_gates`]).
    ///
    /// Defaults to "no engine installed", where every gate defers to the
    /// `TerminalModes::allow_*` / per-capability `authorized` bits.
    pub(super) policy: super::policy_gates::PolicyState,
    /// Monotonic damage epoch: bumped once per damage session, the first time
    /// [`Terminal::damage_epoch`] observes net-new grid damage after the previous
    /// [`Terminal::take_damage`]. An unchanged epoch lets a renderer skip a
    /// redraw. See [`Terminal::has_damage`].
    pub(super) damage_epoch: u64,
    /// Whether the current grid damage has already advanced `damage_epoch`.
    /// Cleared by `take_damage`, so the epoch advances on a real write and not
    /// on a no-op.
    pub(super) damage_epoch_counted: bool,
    /// Process-unique identity of this engine instance, stamped into every
    /// render snapshot. Damage-scoped re-extraction must never take a scratch
    /// filled from a different terminal as its baseline (two same-size panes
    /// sharing a scratch pass every dims check, and per-terminal epochs collide).
    /// Never 0, so an `empty()` scratch never matches.
    pub(super) extract_identity: u64,
    /// Extraction-continuity generation, bumped on every
    /// [`take_damage`](Terminal::take_damage). A scratch filled at generation G is
    /// a sound damage-scoped baseline only while no other consumer has taken
    /// damage since; otherwise the tracker's bits undercount the rows changed
    /// since the fill. Starts at 1 so a never-filled scratch (gen 0) fails.
    pub(super) extract_gen: u64,
    /// Cached full-content search index.
    ///
    /// `indexed_search` rebuilds a [`TerminalSearch`] over retained scrollback
    /// plus visible rows (keyed by absolute row) only when the active grid's
    /// content changes. The key `(alternate_screen, content_seq())` fully
    /// determines the indexed set: `content_gen` bumps on every content mutation
    /// but not on a pure viewport scroll. See [`Terminal::indexed_search`].
    pub(super) search_index: Option<super::search_index::CachedSearchIndex>,
    /// In-flight budgeted resumable search, if any, keyed to the same
    /// `(alternate_screen, content_seq())` snapshot as `search_index`; a
    /// mismatched or superseded cursor restarts. See [`Terminal::search_budgeted`].
    pub(super) budgeted_search: Option<super::search_budgeted::BudgetedSearchState>,
    /// Count of search-index rebuilds (cache misses) in
    /// [`Terminal::indexed_search`], exposed via
    /// [`Terminal::search_index_rebuilds`] so callers can confirm cache reuse.
    pub(super) search_index_rebuilds: u64,
    /// Count of incremental refreshes (evicted prefix dropped, previous visible
    /// and appended rows re-fed) in [`Terminal::indexed_search`]. Each is also
    /// counted in `search_index_rebuilds`, so the difference is the number of
    /// full rebuilds.
    pub(super) search_index_refreshes: u64,
    /// Monotonic revision of top-anchored protected-footer row insertions. A
    /// top-anchored partial scroll inserts logical rows before a protected
    /// footer (ordinary full-screen output does not bump this), so hosts caching
    /// absolute anchors must recompute them when it changes.
    pub(super) absolute_row_revision: u64,
    /// Monotonic repaint-blink epoch: bumped on each DECTCEM hide (`CSI ?25l`)
    /// processed while DEC 2026 synchronized output is active, the
    /// per-keystroke repaint pattern of full-redraw TUIs (vim/less and ConPTY
    /// hide without sync). A read-only projection for hosts' cursor-effect
    /// classifiers: never reset, never checkpointed, never gates bytes.
    pub(super) repaint_blink_epoch: u64,
    /// The foreground handback's asserted evidence: each
    /// [`program_evidence`](super::program_evidence) bit whose setter the
    /// dispatcher parsed since the host last took it
    /// ([`Terminal::take_evidence_asserted`]). Never checkpointed.
    pub(super) evidence_asserted: u16,
    /// Read-only host projection of content-coordinate motion, updated once per
    /// batch at post-processing. Cumulative, so several renderers can observe it
    /// independently.
    pub(super) content_scroll_state: ContentScrollState,
    /// What each resize did to the active grid, newest
    /// [`RESIZE_JOURNAL_CAP`](super::RESIZE_JOURNAL_CAP) kept, with a lifetime
    /// ordinal (see [`super::resize_journal`]). Written only by the two resize
    /// entry points; never checkpointed.
    pub(super) resize_journal: super::resize_journal::ResizeJournal,
}

// Grouped sub-state structs extracted to grouped_state.rs.

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
    /// Get a reference to the grid.
    #[must_use]
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// The main-buffer grid, regardless of alt-screen state.
    ///
    /// While the alternate screen is active the main grid (with the only real
    /// scrollback) is the inactive grid in `alt_grid`. Use this, not
    /// [`grid`](Self::grid), to reach scrollback that must survive an alt-screen
    /// session, e.g. for snapshot/cold-restore.
    #[must_use]
    pub fn main_grid(&self) -> &Grid {
        if self.modes.alternate_screen {
            self.alt_grid.as_ref().unwrap_or(&self.grid)
        } else {
            &self.grid
        }
    }

    /// Get a mutable reference to the grid.
    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.grid
    }

    /// Whether the grid holds unconsumed damage: anything affecting the
    /// rendered grid since the last [`take_damage`](Self::take_damage),
    /// including the initial post-construction full damage. Visual-only state
    /// (blink phase, bell flash, selection) is not tracked here.
    #[must_use]
    #[inline]
    pub fn has_damage(&self) -> bool {
        self.grid.damage().has_damage()
    }

    /// Consume and clear the grid's damage after a present: resets the
    /// [`Damage`](crate::grid::Damage) tracker (reusing its allocations) and
    /// re-arms [`damage_epoch`](Self::damage_epoch). Call exactly once per real
    /// present.
    pub fn take_damage(&mut self) {
        self.grid.clear_damage();
        self.damage_epoch_counted = false;
        // Consuming the damage session invalidates every other scratch's
        // damage-scoped continuity; bumping the generation here lets
        // `cell_frame_damage_scoped_into` detect that in O(1).
        self.extract_gen = self.extract_gen.wrapping_add(1);
    }

    /// A monotonic counter that advances on net-new grid damage.
    ///
    /// Bumped at most once per damage session (the latch is cleared by
    /// [`take_damage`](Self::take_damage)): a real write, scroll, erase or resize
    /// advances it, a no-op `process()` does not, and repeated calls without a
    /// `take_damage` return the same value. It keys off the grid's damage
    /// tracker, so every path that marks damage feeds it.
    pub fn damage_epoch(&mut self) -> u64 {
        if !self.damage_epoch_counted && self.grid.damage().has_damage() {
            self.damage_epoch = self.damage_epoch.wrapping_add(1);
            self.damage_epoch_counted = true;
        }
        self.damage_epoch
    }

    /// Monotonic content generation of the active grid ([`Grid::content_gen`]):
    /// advances on every content mutation but not on a pure viewport scroll,
    /// so caches can compare it in O(1).
    #[must_use]
    #[inline]
    pub fn content_seq(&self) -> u64 {
        self.grid.content_gen()
    }

    /// How many times over this terminal's life something reached the screen
    /// on a path [`content_seq`](Self::content_seq) may not see: every
    /// non-empty output batch [`process`](Self::process) /
    /// [`process_at`](Self::process_at) consumed (whether or not its bytes
    /// changed anything the seq tracks), and every Kitty animation frame step
    /// [`advance_kitty_animations`](Self::advance_kitty_animations) made. Never
    /// reset (RIS included). A conservative change witness: equal values mean
    /// no output reached the terminal and no animation stepped in between.
    #[must_use]
    #[inline]
    pub fn screen_mutations(&self) -> u64 {
        self.transient.screen_mutations
    }

    /// Return the cumulative, non-consuming content-scroll snapshot.
    ///
    /// Diff this copy against a previously observed value — through
    /// [`ContentScrollState::delta_since`]. If
    /// [`ContentScrollState::invalidation_epoch`] changed, discard cached grid
    /// coordinates unless every step is explained by band moves
    /// (`ContentScrollDelta::Bands`); otherwise the rise in
    /// [`ContentScrollState::uniform_up_rows`] is an exact upward translation.
    #[must_use]
    #[inline]
    pub fn content_scroll_state(&self) -> ContentScrollState {
        self.content_scroll_state
    }

    /// Revision for top-anchored protected-footer row insertions.
    #[must_use]
    #[inline]
    pub fn absolute_row_revision(&self) -> u64 {
        self.absolute_row_revision
    }

    /// Get a reference to the text selection state.
    #[must_use]
    #[inline]
    pub fn text_selection(&self) -> &crate::selection::TextSelection {
        &self.text_selection
    }

    /// Get a mutable reference to the text selection state.
    #[inline]
    pub fn text_selection_mut(&mut self) -> &mut crate::selection::TextSelection {
        &mut self.text_selection
    }

    // Vi mode accessors in state_accessors.rs.

    // Scrollback, memory, and clear methods in buffer_api.rs.

    /// Get the terminal modes.
    #[must_use]
    pub fn modes(&self) -> &TerminalModes {
        &self.modes
    }

    /// Monotonic count of synchronized-update (mode 2026) closes: every
    /// processed `?2026l`, reset or timeout force-clear. A present-hold host
    /// releases when it advances; the mode level alone cannot tell a closed and
    /// reopened bracket from one still open.
    #[must_use]
    pub fn sync_end_seq(&self) -> u64 {
        self.transient.sync_end_seq
    }

    /// Whether the open synchronized-output window has accepted a complete PTY
    /// action since its `?2026h`.
    ///
    /// A conservative glass-safety signal, not a damage predicate: queries and
    /// no-op actions count. `false` with an advanced
    /// [`sync_end_seq`](Self::sync_end_seq) lets a presenter show the just-closed
    /// frame while a new, clean window is open; `true` means the grid may hold
    /// a prefix of the next frame and stays held (subject to the timeout).
    #[must_use]
    pub fn sync_open_dirty(&self) -> bool {
        self.modes.synchronized_output && self.transient.sync_open_dirty
    }

    /// Whether the screen holds a frame the program has not finished: its own
    /// DEC 2026 bracket is open and it has written into it since `?2026h`.
    ///
    /// Unlike the mode level, this survives the `sync_timeout_ms` force-clear
    /// and ends only on the program's `?2026l`, DECSTR or RIS. A bracket closed
    /// and reopened with nothing written reads `false`.
    ///
    /// Hosts use it to know that the visible cursor may be the program's write
    /// position rather than its caret: a TUI hiding the cursor for a long
    /// repaint walks it across the screen until the close.
    #[must_use]
    pub fn sync_frame_unfinished(&self) -> bool {
        self.transient.app_sync_open && self.transient.app_sync_dirty
    }

    // format_paste in buffer_api.rs.

    /// Whether the VT parser is in Ground state (no pending escape sequence).
    /// The I/O-queue fast path uses it to route reads after a chunk that ended
    /// mid-sequence through the ordered slow path.
    #[must_use]
    pub fn parser_is_ground(&self) -> bool {
        self.parser.state().is_ground()
    }

    /// Check if alternate screen is active.
    #[must_use]
    pub fn is_alternate_screen(&self) -> bool {
        self.modes.alternate_screen
    }

    // Scroll display, response buffer, and viewport methods in buffer_api.rs.

    // Shell integration, output blocks, and semantic APIs live in:
    // - shell_api.rs
    // - blocks_api.rs
    // - semantic_api.rs
}

#[cfg(test)]
mod damage_epoch_tests {
    use super::Terminal;

    /// A fresh terminal starts dirty (reports damage and a first epoch);
    /// `take_damage` clears it; a real write re-damages and advances the epoch;
    /// a no-op write does not.
    #[test]
    fn damage_epoch_advances_on_write_not_on_noop() {
        let mut term = Terminal::new(4, 10);

        // Fresh terminal: full damage pending, first epoch observation == 1.
        assert!(term.has_damage(), "fresh terminal must start damaged");
        let e0 = term.damage_epoch();
        assert_eq!(e0, 1, "first observed epoch is 1");
        // Idempotent within a session: re-reading without clearing is stable.
        assert_eq!(term.damage_epoch(), e0, "epoch is stable within a session");

        // After consuming damage, the screen is clean: no damage, same epoch.
        term.take_damage();
        assert!(!term.has_damage(), "take_damage clears grid damage");
        assert_eq!(term.damage_epoch(), e0, "no new damage => epoch unchanged");

        // A real write damages the grid and advances the epoch exactly once.
        term.process(b"hello");
        assert!(term.has_damage(), "a write must damage the grid");
        let e1 = term.damage_epoch();
        assert_eq!(e1, e0 + 1, "a write advances the epoch by one");
        assert_eq!(term.damage_epoch(), e1, "still one session until cleared");

        term.take_damage();
        assert_eq!(term.damage_epoch(), e1, "cleared => epoch holds");

        // A no-op process (empty input leaves the grid untouched) must not
        // advance the epoch.
        term.process(b"");
        assert!(!term.has_damage(), "an empty write damages nothing");
        assert_eq!(
            term.damage_epoch(),
            e1,
            "no-op write must NOT advance the epoch"
        );

        // A second real write advances again — monotonic.
        term.process(b"world");
        let e2 = term.damage_epoch();
        assert_eq!(e2, e1 + 1, "the next write advances the epoch again");
        assert!(e2 > e1, "epoch is monotonic");
    }

    /// A scrollback scroll damages the grid (so it feeds the epoch), but a
    /// scroll that does not move the viewport changes nothing.
    #[test]
    fn scroll_damages_only_when_the_viewport_moves() {
        let mut term = Terminal::new(3, 10);
        // Build some scrollback so there is room to scroll up.
        for _ in 0..10 {
            term.process(b"line\r\n");
        }
        term.take_damage();
        let before = term.damage_epoch();

        // Scrolling into history moves the viewport => grid damage => new epoch.
        term.grid_mut().scroll_display(2);
        assert!(term.has_damage(), "scrolling the viewport damages the grid");
        let scrolled = term.damage_epoch();
        assert_eq!(scrolled, before + 1, "a real scroll advances the epoch");

        term.take_damage();
        // Scrolling down by 0 does not move the viewport => no damage.
        term.grid_mut().scroll_display(0);
        assert!(!term.has_damage(), "a zero-delta scroll damages nothing");
        assert_eq!(
            term.damage_epoch(),
            scrolled,
            "no-op scroll must NOT advance the epoch"
        );
    }

    /// content_gen advances on a content mutation (write, erase, content scroll),
    /// not on a no-op or a pure `scroll_display`, and is monotonic. Unlike
    /// `damage_epoch`, a viewport scroll leaves it untouched, so a cached search
    /// index survives scrolling.
    #[test]
    fn content_seq_advances_on_content_not_on_viewport_scroll() {
        let mut term = Terminal::new(3, 10);

        // Fresh terminal: content_gen initialized nonzero so `0` is a usable
        // "never observed" sentinel.
        let g0 = term.content_seq();
        assert!(g0 > 0, "content_gen is initialized nonzero");

        // (a) A cell write advances content_gen.
        term.process(b"hello");
        let g1 = term.content_seq();
        assert!(g1 > g0, "a cell write advances content_gen");

        // (b) A no-op process (empty input) does not advance content_gen.
        term.process(b"");
        assert_eq!(
            term.content_seq(),
            g1,
            "a no-op process must NOT advance content_gen"
        );

        // (a) A line erase (EL — erase to end of line) advances content_gen.
        term.process(b"\x1b[K");
        let g2 = term.content_seq();
        assert!(g2 > g1, "a line erase advances content_gen");

        // (a) A content scroll (newlines that push rows into scrollback via
        // scroll_up) advances content_gen.
        for _ in 0..10 {
            term.process(b"line\r\n");
        }
        let g3 = term.content_seq();
        assert!(g3 > g2, "a content scroll (scroll_up) advances content_gen");

        // (b) A pure viewport scroll changes what is shown, not the content: it
        // advances damage_epoch but not content_gen.
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

        // A zero-delta viewport scroll likewise leaves content_gen unchanged.
        term.grid_mut().scroll_display(0);
        assert_eq!(
            term.content_seq(),
            g3,
            "a no-op viewport scroll must NOT advance content_gen"
        );

        // (c) Monotonic: a further write advances again and never decreases.
        term.process(b"x");
        let g4 = term.content_seq();
        assert!(g4 > g3, "content_gen is monotonic across further writes");
    }
}

#[cfg(test)]
mod content_scroll_delta_tests {
    use super::{CONTENT_SCROLL_BAND_RING, ContentScrollDelta, ContentScrollState, RowBandMove};

    /// The writer can overwrite the bounded ring while a reader is between
    /// presents. A reader may replay only a complete, explained interval.
    fn band_history_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            ContentScrollBandHistory {
                const Cap = 16;
                const Buggy = 0;
                var moves = 0;
                var unexplained = 0;
                // Unchanged, Bands, Invalidate.
                var verdict = 0;
                action Step when (moves <= Cap) {
                    moves = moves + 1;
                    verdict = if unexplained == 1 { 2 } else {
                        if moves <= Cap - 1 || Buggy == 1 { 1 } else { 2 }
                    };
                }
                action Poison when (unexplained == 0) {
                    unexplained = 1;
                    verdict = 2;
                }
                action Baseline when (moves > 0 || unexplained == 1) {
                    moves = 0;
                    unexplained = 0;
                    verdict = 0;
                }
                invariant Bounds: moves <= Cap + 1 && unexplained <= 1 && verdict <= 2;
                invariant ReplayComplete: verdict == 0 || verdict == 2 ||
                    (moves > 0 && moves <= Cap && unexplained == 0);
                invariant RetireIncomplete: (moves <= Cap && unexplained == 0) || verdict == 2;
            }
        }
    }

    #[test]
    fn band_history_model_proves_and_catches_overwritten_replay() {
        let model = band_history_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    /// Real VT processing writes the history; the shipping snapshot reader
    /// decides what hosts may replay. Re-baselining and writing a second
    /// interval exercises physical ring wrap.
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
                        // RIS is an unexplained coordinate replacement. Later
                        // exact moves must not make that lost interval replayable.
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

    /// The whole decision table: the `d_e == 0` rows (`Baseline`, `Unchanged`,
    /// `Translate`, overflow to `Invalidate`), and `Bands` only when every epoch
    /// step since the snapshot was a band batch, no uniform rows intervened and
    /// the moves fit the 16-ring.
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

        // One explained epoch step carrying one move.
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 8)),
            ContentScrollDelta::Bands {
                first_seq: 7,
                count: 1
            }
        );
        // One explained step carrying five composed-into-one... and five separate.
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 12)),
            ContentScrollDelta::Bands {
                first_seq: 7,
                count: 5
            }
        );
        // Several explained steps between two presents replay together.
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 9, 7, 12)),
            ContentScrollDelta::Bands {
                first_seq: 7,
                count: 5
            }
        );
        // Two epoch steps, one band batch: the other step is unexplained.
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 6, 3, 8)),
            ContentScrollDelta::Invalidate
        );
        // Uniform rows beside a band batch: the epoch wins, as it always has.
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(13, 5, 3, 8)),
            ContentScrollDelta::Invalidate,
            "later uniform output cannot hide an intervening non-uniform batch"
        );
        // Exactly the ring fits; one more does not.
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
        // A band batch with no moves cannot happen; refused rather than trusted.
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 7)),
            ContentScrollDelta::Invalidate
        );
        // Bands without an epoch step are another lineage's numbers.
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 4, 2, 8)),
            ContentScrollDelta::Invalidate
        );
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), state(10, 4, 3, 7)),
            ContentScrollDelta::Invalidate
        );
        // Every regression is an invalidation.
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

    /// The ring is addressed by sequence number modulo its length, and a batch of
    /// several moves lands them oldest first.
    #[test]
    fn the_band_ring_is_addressed_by_sequence_and_wraps() {
        let mut s = ContentScrollState::default();
        let moves: Vec<RowBandMove> = (0..20u16)
            .map(|i| RowBandMove {
                top: i,
                bottom: 56,
                delta: 1,
            })
            .collect();
        s.invalidate();
        s.record_band_batch(&moves[..3]);
        assert_eq!((s.band_batches, s.band_seq), (1, 3));
        assert_eq!(s.band(0).top, 0);
        assert_eq!(s.band(2).top, 2);
        s.invalidate();
        s.record_band_batch(&moves[3..]);
        assert_eq!((s.band_batches, s.band_seq), (2, 20));
        assert_eq!(s.band(19).top, 19);
        assert_eq!(s.band(4).top, 4, "the 16 newest moves are intact");
        assert_eq!(s.band(3).top, 19, "seq 3's slot was overwritten by seq 19");
    }
}
