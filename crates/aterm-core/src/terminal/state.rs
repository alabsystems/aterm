// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal struct definition and root state accessors.

use super::callbacks::{BufferActivationCallback, TextSizingCallback, WindowCallback};
#[cfg(feature = "sixel")]
use super::grouped_state::SixelState;
use super::grouped_state::{
    BiDiGroupState, ClipboardState, ColorState, CursorSaveState, DcsState, Iterm2State, MarksState,
    NotificationState, SemanticState, ShellIntegrationState, TitleState,
};
use super::transient_state::TransientState;
use super::types::{CurrentStyle, TaskbarProgress, TerminalModes};

use crate::grid::Grid;
use crate::parser::Parser;
use crate::platform::FontDescriptor;

use aterm_types::charset::CharacterSetState;
use aterm_types::{KittyKeyboardState, Rgb, XtermKeyboardState};

/// Cumulative, non-consuming summary of terminal content-coordinate motion.
///
/// Hosts keep a previous copy and diff it against a later snapshot. An advance
/// in [`uniform_up_rows`](Self::uniform_up_rows) means the entire primary-screen
/// viewport moved upward by that many rows. An advance in
/// [`invalidation_epoch`](Self::invalidation_epoch) means at least one
/// non-uniform or otherwise ambiguous mutation occurred, so cached coordinates
/// must be discarded instead of translated.
///
/// Both counters are monotonic for the lifetime of a [`Terminal`], survive RIS
/// and direct [`Terminal::reset`], and are intentionally not checkpointed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentScrollState {
    /// Total rows of composable, full-screen upward motion on the primary screen.
    pub uniform_up_rows: u64,
    /// Number of coordinate-invalidating content-motion batches observed.
    pub invalidation_epoch: u64,
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
    /// Host resolver for Kitty NON-DIRECT transmission mediums (`t=f` file, `t=t`
    /// temp file, `t=s` POSIX shared memory). The engine never touches the
    /// filesystem or shared memory itself (it stays pure + wasm-safe); when a
    /// non-direct medium arrives it hands the host the `(medium, path/name)` and the
    /// host — under its OWN fail-closed security policy + I/O — returns the raw image
    /// bytes (or `None` to reject). Absent (the default) ⇒ non-direct mediums are
    /// skipped cleanly. Set via [`Terminal::set_kitty_file_resolver`].
    #[allow(clippy::type_complexity)]
    pub(super) kitty_file_resolver: Option<
        Box<dyn Fn(crate::terminal::kitty_graphics::KittyMedium, &str) -> Option<Vec<u8>> + Send>,
    >,
    /// Last time a BEL callback was fired (rate limiting).
    ///
    /// Prevents DoS via BEL flooding: a malicious program spamming 0x07
    /// would otherwise fire the callback millions of times per second,
    /// wasting CPU on cross-language callback overhead even when the UI
    /// layer has its own rate limiting.
    // web_time::Instant (std on native, JS clock on wasm): assigned from
    // transient.process_now, so must share its type.
    pub(super) last_bell_time: Option<web_time::Instant>,
    /// Monotonic count of bells that FIRED (past the throttle), for poll/watermark
    /// observers — the `subscribe events` stream emits `EVENT … bell` when it
    /// advances. Distinct from the take-and-clear `bell_pending` edge flag: this
    /// never resets (it is a running total, not a pending signal). NOT checkpointed
    /// (a live supervision counter, not replay state — like `last_bell_time`).
    pub(super) bell_total: u64,
    /// Cursor style change callback (called when DECSCUSR changes cursor style).
    pub(super) cursor_style_callback: Option<Box<dyn FnMut(aterm_types::CursorStyle) + Send>>,
    /// Host-preferred DEFAULT cursor style: the shape used before any DECSCUSR and
    /// restored on RIS/DECSTR. Distinct from the live `modes.cursor_style` (which an
    /// app drives via DECSCUSR) and persisted here on `Terminal` so it survives the
    /// `*modes = TerminalModes::new()` reset. Set via [`Terminal::set_default_cursor_style`].
    pub(super) default_cursor_style: aterm_types::CursorStyle,
    /// Buffer activation callback (called when switching between main/alt screen).
    pub(super) buffer_activation_callback: Option<BufferActivationCallback>,
    /// Grouped notification state (OSC 9, OSC 99, OSC 777).
    pub(super) notifications: NotificationState,
    /// Grouped clipboard and copy-capture callback state.
    pub(super) clipboard: ClipboardState,
    /// Grouped state for Terminal OSC 1337 protocol extensions.
    pub(super) iterm2: Iterm2State,
    /// Grouped transient state cleared on reset (response buffer, hyperlink,
    /// underline color, last graphic char, VT52, sync, SGR stack).
    pub(super) transient: TransientState,
    /// The Observation Kernel (L0): armed `await`/quiescence watchers over
    /// `content_seq`. EPHEMERAL, observation-only state — it never mutates the
    /// surface and is NEVER part of a `TerminalCheckpoint`, so it cannot perturb
    /// replay determinism (a fresh, empty set is reconstructed on hydration).
    pub(super) watchers: super::observe::WatcherSet,
    /// Persistent scratch for the Observation Kernel's per-batch row scan.
    ///
    /// EPHEMERAL, observation-only (like `watchers`): never checkpointed,
    /// never mutates the surface. `observe_at` `mem::take`s this, resizes it to
    /// the visible row count, and refills each slot through
    /// [`row_text_into`](Self::row_text_into) — reusing both the outer `Vec` and
    /// each inner `String`'s heap capacity across batches — so an armed
    /// `RowMatches` watcher no longer allocates one full-screen `Vec` plus one
    /// `String` per visible row on every processed batch.
    pub(super) row_text_scratch: Vec<Option<String>>,
    /// Current working directory (OSC 7).
    ///
    /// Set by shells when the directory changes.
    /// Format: `file://hostname/path/to/dir`
    /// We store just the path portion for convenience.
    pub(super) current_working_directory: Option<String>,
    /// Color palette for indexed colors (OSC 4).
    ///
    /// Grouped color state (palette, defaults, cursor, selection).
    pub(super) color: ColorState,
    /// Font descriptor for rendering text (family, size, weight, italic).
    pub(super) font: FontDescriptor,
    /// Grouped BiDi (bidirectional text) state.
    ///
    /// Bundles configuration, resolver, and per-line render cache.
    /// Accessed from bidi_rendering.rs, config_api.rs, colors_api.rs, and handler.rs.
    pub(super) bidi_state: BiDiGroupState,
    /// Grouped DCS (Device Control String) processing state.
    pub(super) dcs: DcsState,
    /// Grouped shell integration state (OSC 133, output blocks, command marks).
    pub(super) shell: ShellIntegrationState,
    /// Grouped marks and annotations state.
    pub(super) marks_state: MarksState,
    /// Grouped semantic blocks/buttons state and callbacks (OSC 1337).
    pub(super) semantic: SemanticState,
    /// Taskbar progress state (ConEmu OSC 9;4).
    ///
    /// Set by OSC 9;4;state;progress sequences. Host application can
    /// use this to display progress in taskbar/dock.
    pub(super) taskbar_progress: Option<TaskbarProgress>,
    /// Kitty keyboard protocol state.
    pub(super) kitty_keyboard: KittyKeyboardState,
    /// xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).
    pub(super) xterm_keyboard: XtermKeyboardState,
    /// Grouped Sixel graphics processing state.
    #[cfg(feature = "sixel")]
    pub(super) sixel: SixelState,
    /// Window operations callback for CSI t (XTWINOPS).
    ///
    /// Called when window manipulation or query sequences are received.
    pub(super) window_callback: Option<WindowCallback>,
    /// Callback for text sizing events (OSC 66 - Kitty protocol).
    ///
    /// Called when text sizing escape sequences are received.
    pub(super) text_sizing_callback: Option<TextSizingCallback>,
    /// Text selection state (mouse-based selection).
    ///
    /// Tracks the current text selection for copy operations. The selection is
    /// managed by the UI layer but stored here so it can be adjusted when the
    /// terminal scrolls or text changes.
    pub(super) text_selection: crate::selection::TextSelection,
    /// SELECTION CUSTODY, screen-scoped selection (the Phase-3 remainder): the OTHER
    /// screen's selection, parked across an alternate-screen switch.
    ///
    /// A selection belongs to the screen it was made on. While `alternate_screen`
    /// is set this slot holds the MAIN screen's selection — the name reads
    /// backwards on purpose, because it names the slot's ROLE (parked), not its
    /// occupant. Lifetime invariant: **empty whenever `alternate_screen` is
    /// false**. `post_process` parks with `mem::take` on the batch that enters alt
    /// and restores with `mem::take` on the batch that leaves, so the slot is never
    /// a durable second selection with a lifetime of its own — which is what keeps
    /// RIS, `clear_scrollback`, `restore_checkpoint` and a width resize from each
    /// acquiring an independent "must also clear the parked one" obligation for a
    /// selection that could outlive them.
    ///
    /// The two arms compare the batch's START screen with its END screen, so a
    /// batch that EXITS and RE-ENTERS runs neither. The ALT screen's own selection
    /// still dies there — `post_process` clears it on the exit paths' report
    /// (`alt_screen_left_in_batch`), because a 1049 exit drops that buffer outright
    /// and a `?47h` re-entry allocates a blank one with no damage of its own — and
    /// this slot is untouched, which is correct: its main-screen occupant was never
    /// restored, so it is still parked.
    pub(super) parked_text_selection: crate::selection::TextSelection,
    /// PRESS CUSTODY — which custody transition last fired (see
    /// [`super::custody`]). One byte: a fieldless enum in an `Option`, written by a
    /// single store at each site that DECIDES custody and read by the `custody`
    /// control verb and the Tier-1 `PressCustody` conformance.
    ///
    /// Session-only and deliberately NOT checkpointed: it describes the last event
    /// this session observed, not any VT protocol state, so a restored checkpoint
    /// starts with no recorded transition rather than a stale one from another run.
    pub(super) last_custody: Option<super::custody::CustodyTransition>,
    /// PRESS CUSTODY — the last recorded transition that actually TOOK the reading
    /// position or the highlight, as opposed to merely being the most recent event.
    ///
    /// A second byte in the same padding, latched by the subset of recordings whose
    /// own condition proves something moved (see
    /// [`super::custody::CustodyTransition::always_takes_custody`] and
    /// `Terminal::note_custody_at`). Without it the single slot is a truthful but
    /// useless answer to the question the design exists for: `OutputAtLive` writes it
    /// on every prompt, every `cat` and every `tail -f` line, so by the time a human
    /// types `custody` the event they are asking about is thousands of no-ops in the
    /// past.
    ///
    /// Session-only and deliberately NOT checkpointed, for the same reason as
    /// [`Self::last_custody`].
    pub(super) last_custody_change: Option<super::custody::CustodyTransition>,
    /// The last transition that took a live highlight the user did NOT release —
    /// the question the `custody` verb exists to answer. Kept apart from
    /// `last_custody_change` because that one is overwritten by a deliberate
    /// deselect, which is never the answer anyone is looking for.
    pub(super) last_selection_taker: Option<super::custody::CustodyTransition>,
    /// Secure keyboard entry mode.
    ///
    /// When enabled, indicates that the UI layer should enable platform-specific
    /// secure input mechanisms to prevent keylogging (e.g., macOS
    /// `EnableSecureEventInput()`). The terminal library sets this state,
    /// but the actual platform-specific security APIs must be called by the
    /// UI layer.
    pub(super) secure_keyboard_entry: bool,
    /// Vi mode navigation state (cursor, marks, inline search).
    pub(super) vi: crate::vi_mode::ViMode,
    /// Configured timeout for synchronized output mode (mode 2026).
    ///
    /// Loaded from `TerminalConfig.sync_timeout_ms` and applied via
    /// `apply_config()`. Defaults to 1 second.
    pub(super) sync_timeout_duration: std::time::Duration,
    /// Host-side authorization state for OSC 52 clipboard access
    /// (set + query). See [`super::clipboard_auth`] for the security
    /// model: the zero-sized [`super::clipboard_auth::ClipboardWriteCapability`]
    /// and [`super::clipboard_auth::ClipboardQueryCapability`] tokens
    /// are the **only** way a handler can reach the clipboard callback,
    /// and they can only be minted after the host calls
    /// [`super::Terminal::authorize_clipboard_access`]. Addresses
    /// CF-004 (ungated OSC 52 set) and CF-005 (runtime-bool query gate).
    pub(super) clipboard_auth: super::clipboard_auth::ClipboardAuth,
    /// Host-side authorization state for OSC 133 / OSC 633 shell
    /// integration capability-nonce (#7937 F01-2, #7960). Holds the
    /// 32-byte nonce installed by the host via
    /// [`super::Terminal::authorize_shell_integration`]. Enforcement is
    /// gated by [`super::types::TerminalModes::require_shell_integration_nonce`];
    /// when that bit is set and `verify_nonce` rejects, the OSC
    /// 133/633 handlers silently drop the sequence and increment the
    /// per-state drop counter. See [`super::shell_integration_auth`]
    /// for the security model.
    pub(super) shell_integration_auth: super::shell_integration_auth::ShellIntegrationAuth,
    /// Host-side authorization state for OSC 8 hyperlink URI acceptance.
    /// See [`super::hyperlink_auth`] for the security model: the zero-sized
    /// [`super::hyperlink_auth::HyperlinkCapability`] token is the **only**
    /// way a handler can write to `transient.current_hyperlink` once the
    /// refactor completes. Defaults to authorized (matches pre-refactor
    /// behavior — OSC 8 has been a universally supported terminal feature
    /// since xterm's 2017 patch). Hosts shipping a hardened profile can
    /// revoke via [`super::Terminal::revoke_hyperlinks`]. Addresses CF-014
    /// from `reports/2026-04-18-privilege-conflation-audit.md`.
    pub(super) hyperlink_auth: super::hyperlink_auth::HyperlinkAuth,
    /// Host-side authorization state for raw DCS callback delivery
    /// (OSC P ... ST → registered `FnMut(&[u8], u8)`). See
    /// [`super::dcs_auth`] for the security model: the zero-sized
    /// [`super::dcs_auth::DcsEmitCapability`] token is the **only**
    /// way a handler can reach `self.dcs.callback`. Defaults to
    /// authorized. Addresses CF-013 from
    /// `reports/2026-04-18-privilege-conflation-audit.md` — the raw
    /// payload delivered to host callbacks is PTY-origin and the
    /// emission site wraps it in `Provenance<&[u8], Pty>` at the type
    /// level before erasing provenance at the FFI boundary.
    pub(super) dcs_auth: super::dcs_auth::DcsAuth,
    /// OSC / escape-sequence policy: the engine installed via
    /// [`super::Terminal::apply_policy_engine`] **plus** the gate verdicts
    /// compiled from it.
    ///
    /// The pair is one field on purpose. Several capability gates consult a
    /// probe that is a compile-time constant, so their verdict can only change
    /// when the policy does; `PolicyState` resolves those once at install time
    /// and keeps the table inseparable from the engine that produced it, so a
    /// gate can never answer from a policy that is no longer installed. See
    /// [`super::policy_gates`] for the equivalence and staleness arguments.
    ///
    /// Defaults to "no engine installed", where every gate defers to the legacy
    /// `TerminalModes::allow_*` / per-capability `authorized` bits — existing
    /// callers see no behavioral change until they install an engine.
    pub(super) policy: super::policy_gates::PolicyState,
    /// Monotonic damage epoch (D-1): bumped once per "damage session" — the
    /// first time [`Terminal::damage_epoch`] observes net-new grid damage after
    /// the previous [`Terminal::take_damage`]. A renderer that records the epoch
    /// at present time can cheaply detect "nothing changed since I last drew"
    /// (epoch unchanged) and skip an entire redraw. See [`Terminal::has_damage`].
    pub(super) damage_epoch: u64,
    /// Whether the CURRENT grid damage has already advanced `damage_epoch`.
    /// Set when `damage_epoch` counts a damage session; cleared by
    /// `take_damage` so the next net-new damage bumps the epoch again. This is
    /// what makes the epoch advance on a real write but NOT on a no-op (a write
    /// that leaves the grid undamaged never flips this).
    pub(super) damage_epoch_counted: bool,
    /// Process-unique identity of THIS engine instance, stamped into every
    /// render snapshot (DMG-1 damage carrier). Damage-scoped re-extraction
    /// must never treat a scratch filled from a DIFFERENT terminal as a valid
    /// baseline: two same-dims panes sharing one reused scratch pass every
    /// dims/anchor check while holding each other's cells, and per-terminal
    /// `damage_epoch` values collide numerically (both count from 0). A
    /// process-global nonce (never 0, so an `empty()` scratch can never match)
    /// makes that aliasing structurally detectable instead of "unlikely".
    pub(super) extract_identity: u64,
    /// Extraction-continuity generation (DMG-1 damage carrier): bumped on every
    /// [`take_damage`](Terminal::take_damage). The grid's damage tracker holds
    /// "rows changed since the last take"; a scratch refilled at generation G is
    /// a sound damage-scoped baseline ONLY while no OTHER consumer has taken
    /// damage since (still G) — otherwise the tracker was reset mid-window and
    /// its bits UNDERCOUNT the rows that changed since the scratch's fill, which
    /// is exactly the silent-stale-row failure the carrier exists to prevent.
    /// Starts at 1 so a never-filled scratch (gen 0) always fails the check.
    pub(super) extract_gen: u64,
    /// Cached full-content search index (P1.0b).
    ///
    /// `indexed_search` rebuilds a [`TerminalSearch`] over the entire retained
    /// scrollback + visible rows (keyed by absolute row) only when the active
    /// grid's content changes. The cache key `(alternate_screen, content_seq())`
    /// is a complete summary of the indexed set: the indexed text and its
    /// absolute-row keys are a pure function of which grid is active and that
    /// grid's `content_gen`, which bumps on every content mutation (write,
    /// scroll-into-scrollback, erase, reflow/resize) but NOT on a pure viewport
    /// scroll (which never changes the retained set). See [`Terminal::indexed_search`].
    pub(super) search_index: Option<super::search_index::CachedSearchIndex>,
    /// In-flight budgeted resumable search (P1.1), if any. Keyed to the same
    /// `(alternate_screen, content_seq())` snapshot as `search_index`; a
    /// mismatched or superseded cursor restarts from scratch. See
    /// [`Terminal::search_budgeted`].
    pub(super) budgeted_search: Option<super::search_budgeted::BudgetedSearchState>,
    /// Count of full search-index REBUILDS (cache misses) performed by
    /// [`Terminal::indexed_search`]. Monotonic; bumped on every miss (and never
    /// on a reuse). Exposed via [`Terminal::search_index_rebuilds`] so callers
    /// (and tests) can confirm a repeat query reused the cache rather than
    /// rebuilt — the O(1) win. A plain counter, no behavioral effect.
    pub(super) search_index_rebuilds: u64,
    /// Count of INCREMENTAL refreshes (churn-bounded re-feeds of the stale
    /// cached index: evicted prefix dropped, previous visible rows + appended
    /// rows re-fed) performed by [`Terminal::indexed_search`]. Every refresh
    /// is also counted in `search_index_rebuilds` (it serves a cache miss);
    /// `search_index_rebuilds - search_index_refreshes` is therefore the
    /// number of FULL O(total-retained) rebuilds. A plain counter, no
    /// behavioral effect — it is the observable that pins "the churn path
    /// engaged" in tests and the churn bench.
    pub(super) search_index_refreshes: u64,
    /// Monotonic revision of top-anchored protected-footer row insertions.
    ///
    /// Ordinary full-screen output advances every live row uniformly and does
    /// not bump this value. A top-anchored partial scroll inserts logical rows
    /// before a protected footer, so hosts caching absolute anchors must
    /// recompute or transform them when this revision changes.
    pub(super) absolute_row_revision: u64,
    /// Monotonic REPAINT-BLINK epoch: bumped each time a DECTCEM HIDE
    /// (`CSI ?25l`) is processed WHILE DEC-2026 synchronized output is active.
    /// That hide-inside-sync pairing is the per-keystroke repaint choreography
    /// of modern full-redraw TUIs (Claude Code wraps EVERY keystroke's repaint
    /// in `?2026h · ?25l · redraw · ?25h · ?2026l`), and of nothing else
    /// observed: vim/less park the cursor hidden WITHOUT sync, and ConPTY
    /// hides per echo without sync — neither ever bumps this. A read-only
    /// display projection for hosts (cursor-effect classifiers), like
    /// [`Terminal::damage_epoch`]: never reset (monotonic across RIS), never
    /// checkpointed, never gates bytes.
    pub(super) repaint_blink_epoch: u64,
    /// Read-only host projection of content-coordinate motion.
    ///
    /// Updated once at the terminal post-processing boundary from the grid's
    /// precise selection-scroll signal. Unlike that take-and-clear signal,
    /// these cumulative counters can be observed independently by multiple
    /// renderers without one consumer starving another.
    pub(super) content_scroll_state: ContentScrollState,
}

// Grouped sub-state structs extracted to grouped_state.rs (#1977).

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
    /// Default foreground color (light gray - matches xterm default).
    pub const DEFAULT_FOREGROUND: Rgb = super::transient_state::DEFAULT_FOREGROUND;

    /// Default background color (black - matches xterm default).
    pub const DEFAULT_BACKGROUND: Rgb = super::transient_state::DEFAULT_BACKGROUND;

    /// Get a reference to the grid.
    #[must_use]
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Get a reference to the MAIN-buffer grid, regardless of alt-screen state.
    ///
    /// Entering the alternate screen (1049) replaces the active grid with a
    /// fresh scrollback-0 alt buffer and stashes the main grid (which holds the
    /// user's scrollback) in `alt_grid`. So when alt-screen is active the main
    /// buffer — and the only real scrollback history — is the inactive grid.
    /// Use this (not [`grid`](Self::grid)) to recover scrollback that must
    /// survive an alt-screen session, e.g. snapshot/cold-restore of a TUI.
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

    /// Mark the cursor cell as damaged for re-rendering.
    ///
    /// Call this before rendering when cursor visibility has been toggled
    /// (e.g., during cursor blink). This ensures the cursor cell is included
    /// in damage-based rendering even though no cell content changed.
    ///
    /// # Example
    ///
    /// In a cursor blink timer callback:
    /// ```text
    /// cursor_visible = !cursor_visible;
    /// terminal.mark_cursor_damage();
    /// renderer.render(&terminal, surface);
    /// ```
    pub fn mark_cursor_damage(&mut self) {
        self.grid.mark_cursor_damage();
    }

    /// Whether the grid currently holds unconsumed damage (D-1).
    ///
    /// True when anything that affects the rendered grid has changed since the
    /// last [`take_damage`](Self::take_damage): a write/scroll/erase/resize, or
    /// the initial post-construction full damage. A renderer uses this as the
    /// first half of its "do I need to repaint?" early-out (the other half is
    /// purely-visual state the grid doesn't track: cursor blink phase, a bell
    /// flash, the text selection — those the frontend compares itself).
    #[must_use]
    #[inline]
    pub fn has_damage(&self) -> bool {
        self.grid.damage().has_damage()
    }

    /// Consume and clear the grid's damage after a present (D-1).
    ///
    /// Resets the grid [`Damage`](crate::grid::Damage) tracker (reusing its
    /// allocations) and re-arms the [`damage_epoch`](Self::damage_epoch) counter
    /// so the NEXT net-new damage advances the epoch again. Call this exactly
    /// once per real present; afterwards [`has_damage`](Self::has_damage) is
    /// `false` until the grid changes.
    pub fn take_damage(&mut self) {
        self.grid.clear_damage();
        self.damage_epoch_counted = false;
        // DMG-1 carrier: consuming the damage session invalidates every OTHER
        // scratch's damage-scoped continuity (their "changed since my last
        // fill" superset just got reset under them). Bumping the generation
        // here — the single choke point every consumer already calls — is what
        // lets `cell_frame_damage_scoped_into` prove, in O(1), that its bits
        // still cover everything since the scratch's own fill.
        self.extract_gen = self.extract_gen.wrapping_add(1);
    }

    /// A monotonic counter that advances on net-new grid damage (D-1).
    ///
    /// The epoch is bumped at most ONCE per damage session: the first time this
    /// is called while the grid is damaged and the current damage has not yet
    /// been counted (the latch is cleared by [`take_damage`](Self::take_damage)).
    /// Consequences:
    /// - A real write/scroll/erase/resize advances the epoch.
    /// - A no-op `process()` (input that leaves the grid undamaged) does NOT —
    ///   `has_damage()` stays false, so nothing is counted.
    /// - Repeated calls without an intervening `take_damage` return the SAME
    ///   value (the session is already counted), so the renderer can compare it
    ///   against the epoch it recorded at the last present to decide whether to
    ///   redraw, and only `take_damage` after an ACTUAL present opens a new
    ///   session.
    ///
    /// Because it keys off the grid's own damage tracker, EVERY path that marks
    /// damage (VT processing, scrollback scroll, resize) feeds it for free; no
    /// per-mutation bookkeeping is required.
    pub fn damage_epoch(&mut self) -> u64 {
        if !self.damage_epoch_counted && self.grid.damage().has_damage() {
            self.damage_epoch = self.damage_epoch.wrapping_add(1);
            self.damage_epoch_counted = true;
        }
        self.damage_epoch
    }

    /// Monotonic content-generation sequence of the ACTIVE grid (P1.0).
    ///
    /// Forwards the active grid's [`Grid::content_gen`]: a value that advances
    /// on every CONTENT mutation (cell/line/scrollback change) but NOT on a pure
    /// viewport scroll. A cached search index or a peer session caches this and
    /// does an O(1) compare to decide whether a full re-read is needed.
    ///
    /// Read accessor only — `content_seq` is WRITE-ONLY at the grid layer (the
    /// fused `mark_content_*` wrappers), so reading it here cannot change any
    /// rendered output.
    #[must_use]
    #[inline]
    pub fn content_seq(&self) -> u64 {
        self.grid.content_gen()
    }

    /// Return the cumulative, non-consuming content-scroll snapshot.
    ///
    /// Diff this copy against a previously observed value. If
    /// [`ContentScrollState::invalidation_epoch`] changed, discard cached grid
    /// coordinates. Otherwise the increase in
    /// [`ContentScrollState::uniform_up_rows`] is an exact whole-screen upward
    /// translation, independent of retained scrollback capacity.
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

    /// Enable or disable 8-bit C1 control code interpretation (0x80-0x9F).
    ///
    /// By default, C1 controls are disabled for security in UTF-8 terminals.
    /// When disabled, bytes 0x80-0x9F are treated as invalid UTF-8 and replaced
    /// with the Unicode replacement character. This prevents escape sequence
    /// injection attacks where malicious data embeds C1 controls.
    ///
    /// Enable this only for legacy applications that require C1 support.
    ///
    /// See: dgl.cx/2023/09/ansi-terminal-security
    #[cfg(test)]
    pub fn set_c1_controls_enabled(&mut self, enabled: bool) {
        self.parser.set_c1_controls_enabled(enabled);
    }

    /// Get the terminal modes.
    #[must_use]
    pub fn modes(&self) -> &TerminalModes {
        &self.modes
    }

    /// Monotonic count of synchronized-update (mode 2026) window CLOSES: every
    /// processed `?2026l`, reset, or timeout force-clear bumps it. A present-hold
    /// host records this when it arms and releases as soon as it advances — the
    /// mode LEVEL alone cannot distinguish "the bracket I held for closed (and a
    /// new one opened)" from "the same bracket is still open", which under a
    /// flood of back-to-back brackets pins presents to ~1/timeout.
    #[must_use]
    pub fn sync_end_seq(&self) -> u64 {
        self.transient.sync_end_seq
    }

    // format_paste in buffer_api.rs.

    /// Restore remote host from session state.
    ///
    /// This sets the remote host state without invoking callbacks.
    /// Used for session resurrection via `SessionManager::restore_terminal`.
    #[cfg(test)] // called from session::terminal_state (test gated)
    #[allow(
        dead_code,
        reason = "consumed by the (un-wired) session test-support layer"
    )]
    pub(crate) fn restore_remote_host(&mut self, host: Option<super::types::RemoteHost>) {
        self.iterm2.remote_host = host;
    }

    /// Get the current title stack depth.
    ///
    /// The title stack stores pushed icon labels and window titles.
    /// Maximum depth is `TITLE_STACK_MAX_DEPTH` (10).
    #[cfg(test)]
    #[must_use]
    pub fn title_stack_depth(&self) -> usize {
        self.title.stack.len()
    }

    /// Global DCS budget bytes currently tracked (test-only).
    #[cfg(test)]
    #[must_use]
    pub fn dcs_total_bytes(&self) -> usize {
        self.dcs.total_bytes
    }

    /// Check if the VT parser is in Ground state.
    ///
    /// Returns `true` when the parser has no pending escape sequence. Used by
    /// the I/O-queue fast-path barrier to determine whether a PTY chunk left
    /// the parser mid-sequence, which requires forcing subsequent reads through
    /// the ordered slow path.
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

    /// Configure the response-sequence rate limiter (Part of #7874).
    ///
    /// Gates every call to `send_response` (DSR/DA/DECRQSS/XTGETTCAP/OSC
    /// color queries/title reports, etc.) so a malicious PTY peer cannot
    /// amplify bandwidth by spamming query sequences. Responses that
    /// exceed the rate are silently dropped — same contract as buffer
    /// overflow.
    ///
    /// # Parameters
    ///
    /// - `refill_bytes_per_sec`: token refill rate. Defaults to 100 KiB/s,
    ///   which is ~500x the peak legitimate response traffic during shell
    ///   startup. Set to `0` to freeze tokens at their current level (no
    ///   replenishment after burst is drained).
    /// - `burst_bytes`: maximum token balance / burst capacity. Defaults
    ///   to 64 KiB. Set to `0` to drop every response (kill switch).
    ///
    /// Calling this preserves the current token balance, clamped to the
    /// new capacity.
    pub fn set_response_rate_limit(&mut self, refill_bytes_per_sec: u64, burst_bytes: u64) {
        self.transient
            .response_rate_limiter
            .reconfigure(refill_bytes_per_sec, burst_bytes);
    }
}

#[cfg(test)]
mod damage_epoch_tests {
    use super::Terminal;

    /// D-1: a freshly constructed terminal starts dirty (it has never been
    /// presented), so it reports damage and a first epoch; `take_damage` clears
    /// it; a real write re-damages and ADVANCES the epoch; a no-op write does
    /// NOT advance it.
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

        // A no-op process (empty input leaves the grid untouched) must NOT
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

    /// D-1: a scrollback scroll damages the grid (so it feeds the epoch), but a
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

    /// P1.0 content_gen: advances on a CONTENT mutation (cell write / line erase
    /// / content scroll), does NOT advance on a no-op or a pure `scroll_display`
    /// viewport scroll, and is monotonic. Modeled on
    /// `damage_epoch_advances_on_write_not_on_noop`, but proves the
    /// CONTENT-vs-VIEWPORT divergence that `damage_epoch` does NOT have (a
    /// viewport scroll advances `damage_epoch` but must leave `content_seq`
    /// untouched, so a cached search index does not invalidate on a mere scroll).
    #[test]
    fn content_seq_advances_on_content_not_on_viewport_scroll() {
        let mut term = Terminal::new(3, 10);

        // Fresh terminal: content_gen initialized NONZERO so `0` is a usable
        // "never observed" sentinel.
        let g0 = term.content_seq();
        assert!(g0 > 0, "content_gen is initialized nonzero");

        // (a) A cell write advances content_gen.
        term.process(b"hello");
        let g1 = term.content_seq();
        assert!(g1 > g0, "a cell write advances content_gen");

        // (b) A no-op process (empty input) does NOT advance content_gen.
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

        // (b) A pure viewport scroll (scroll_display) changes what is SHOWN, not
        // the content, so content_gen must NOT advance — even though it damages
        // the grid and advances damage_epoch (proving the divergence).
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
