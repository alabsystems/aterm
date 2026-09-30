// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `TerminalCheckpoint` — a scoped, round-trippable projection of a live
//! [`Terminal`] (GREEN-ORDER step 4 / design `HIERARCHICAL_SESSIONS.md` B.3.2).
//!
//! The live [`Terminal`] is **neither `Clone` nor `Serialize`**: it holds host
//! callbacks (`Box<dyn FnMut>`: bell, window, colour change/query,
//! notifications, clipboard), three host-auth gates (clipboard,
//! shell-integration, hyperlink), a policy engine, and a live `Parser`. A
//! byte-identical clone is impossible by construction. A [`TerminalCheckpoint`]
//! is therefore a *precise projection with a documented exclusion list*, not a
//! clone — see the EXCLUDED block below.
//!
//! It captures both grid bodies (full cell fidelity via the `Line` codec) with
//! their absolute row numbering, per-grid cursor / scroll-region / pending-wrap
//! / tab-stops, size, modes, style, charsets, keyboard state, the colours an
//! application set, the shell-integration state and the titles a program set
//! — window title, icon name and title stack (`checkpoint_state.rs`).
//! The round-trip is proven by the in-module property test (`mod tests`), which
//! is the ship gate.
//
// ===========================================================================
// EXCLUDED (and why):
//   - host effects: the callbacks, the policy engine, and the live auth gates
//     (the shell-integration nonce travels only on the seamless carry, and the
//     adopting host authorizes it). A restore never replays a host effect
//     (bell, notification, clipboard write, window op). `from_checkpoint`
//     gives `Terminal::new`'s defaults; a host installs its own through the
//     ordinary setters, or adopts into a configured terminal with
//     `restore_checkpoint`.
//   - in-flight and session-only state: `transient` (the response buffer and
//     rate limiter, the REP character, an open OSC 8 hyperlink or SGR 58
//     underline colour, a VT52 `ESC Y` waiting for its address — a carried
//     session resumes with none of them open, and the seamless capture
//     re-parks rather than carry an open link or a pending address over
//     queued output (`Terminal::output_state_uncarried`);
//     a synchronized-update window stays open, since `modes` crosses whole,
//     with its timer re-armed at the restore), `vi` mode, `text_selection`, the
//     observation watchers and `last_custody` (see `state.rs`), which
//     describe the person's interaction with THIS process, not the screen.
//   - `marks_state` (OSC 1337 SetMark/AddAnnotation), `semantic` zones and
//     `iterm2` image placements: iTerm2-protocol extras with no consumer that
//     survives an update; OSC 133/633 blocks, which the product does read,
//     ARE carried.
//   - sixel and other decoded images: priced out of the handoff (megabytes
//     per screen); the cells they covered carry.
//   - the bell and response-rate clocks (`last_bell_time`, `bell_total`, the
//     response rate limiter): live supervision state that reads real time on
//     purpose (`state.rs`, `processing.rs`).
//   - the title EPOCH: a host's change signal, not state. The titles
//     themselves ARE carried (they were not until the round-four plan's item
//     8b, and a carried tab lost its label); a restore that changes the window
//     title bumps this terminal's own epoch instead.
//
// The seamless-handoff wire form is `CheckpointMeta` (serde, every scalar
// field) plus the grid blobs as sidecars.
// ===========================================================================

use aterm_parser::ParserCarry;
use aterm_types::charset::CharacterSetState;
use aterm_types::{KittyKeyboardStateSnapshot, TaskbarProgress, XtermKeyboardState};

use super::Terminal;
use super::checkpoint_state::{ColorRepr, ShellRepr, TitleRepr};
use super::types::{CurrentStyle, TerminalModes};
use crate::grid::{CellFlags, Cursor, Grid, PackedColor, SavedCursorState};
use crate::scrollback::{Scrollback, deserialize_lines, serialize_lines};

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
    /// Whether TBC 3 has suppressed the every-8 defaults. The OTHER half of
    /// the tab state, and the half a resize reads: without it a restored grid
    /// that had been told "clear ALL tab stops" re-seeds the default into the
    /// columns the next widen adds. `#[serde(default)]` for the additive rule
    /// this file already follows — a checkpoint written before this field
    /// existed restores `false`, which is what those producers meant.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tab_defaults_suppressed: bool,
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
            tab_defaults_suppressed: grid.tab_defaults_suppressed(),
        }
    }

    fn apply(&self, grid: &mut Grid) {
        grid.set_scroll_region(self.scroll_top, self.scroll_bottom);
        // set_horizontal_margins self-validates (full margins recompute
        // has_horizontal_margins=false), so this is safe even if cols changed.
        grid.set_horizontal_margins(self.margin_left, self.margin_right);
        grid.set_cursor(self.cursor_row, self.cursor_col);
        grid.set_pending_wrap(self.pending_wrap);
        grid.restore_tab_stops(&self.tab_stops, self.tab_defaults_suppressed);
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
    /// Project a DECSC slot for the wire — RAW, exactly as the engine keeps it.
    ///
    /// The slot is absolute coordinates that nothing reads until DECRC or a
    /// 1049 exit, and those clamp to the grid AT THAT MOMENT (`Grid::set_cursor`;
    /// xterm's `CursorRestore` does the same to a `sc->row` its `ScreenResize`
    /// never touched). `Terminal::resize` therefore leaves `cursor_save` alone,
    /// and after a shrink the slot can honestly name a row the grid no longer
    /// has. That is state, not damage: if the grid grows back before the slot
    /// is read, the cursor lands on its original row, as it should.
    ///
    /// So the projection must not clamp. Measured 2026-09-22 (this fix's first
    /// draft did): a successor restores at the 55-row size the outgoing process
    /// parked at, its "finishing" bar folds seconds later and every session
    /// grows back to 56, and when the user then quits the full-screen app the
    /// restored engine put the shell's cursor one row ABOVE where the
    /// pre-update engine — and xterm — put it. The in-memory checkpoint that
    /// temporal replay hydrates from would have diverged the same way.
    ///
    /// What the handoff wire admits for this slot is the consumer's business,
    /// and it is the engine's protocol ceiling, not the current grid — see
    /// `seamless::checkpoint_meta_bound_violation`, and the incident recorded
    /// there: with the bound at the grid, every automatic self-update from
    /// v0.87.0 through v0.90.0 refused on the owner's daily driver.
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

/// The shell-integration capability nonce an adopted shell keeps emitting
/// (`ATERM_SHELL_NONCE`, injected once at its first spawn). Carried ONLY by the
/// seamless-handoff projection ([`Terminal::checkpoint_carry`]), so the successor
/// can authorize the value the running shell already signs its OSC 133/633 marks
/// with. A capability, not buffer state: `Debug` never prints it, and the
/// in-memory projections ([`Terminal::checkpoint`]) never capture it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ShellIntegrationNonce(pub [u8; 32]);

impl std::fmt::Debug for ShellIntegrationNonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShellIntegrationNonce(..)")
    }
}

impl ShellIntegrationNonce {
    /// Lowercase hex, the wire form the shell's `id=` parameter uses.
    #[must_use]
    pub fn to_hex(&self) -> String {
        use std::fmt::Write as _;
        self.0.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    /// Parse exactly 64 hex digits; anything else is `None` (a malformed carry
    /// degrades to "no nonce", never to a partial one).
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let hi = char::from(pair[0]).to_digit(16)?;
            let lo = char::from(pair[1]).to_digit(16)?;
            out[i] = u8::try_from(hi * 16 + lo).ok()?;
        }
        Some(Self(out))
    }
}

/// Whether a terminal's OSC 133/633 marks can reach it, as the host reports it
/// (`status integration=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellIntegrationPosture {
    /// No nonce is required: marks dispatch unauthenticated (integration off,
    /// or a `-e` session).
    Off,
    /// A nonce is required and one is authorized.
    On,
    /// A nonce is required and NONE is authorized, so every mark is dropped —
    /// an adopted shell whose handoff did not carry its nonce. The requirement
    /// is kept (clearing it would let any program's output forge marks); this
    /// posture is how the loss is said instead of hidden.
    Degraded,
}

impl ShellIntegrationPosture {
    /// The wire word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Degraded => "degraded",
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
    /// The parser this checkpoint restores into is in Ground (B.3.3): `parser`
    /// is `None`, and either the live parser was Ground with nothing pending at
    /// capture time, or the capture
    /// ([`Terminal::checkpoint_carry_abandoning_partial`]) cancelled, as CAN
    /// would, a partial sequence no carry can hold (a hooked DCS string).
    /// `false` with `parser` set: the restore continues that partial state.
    /// `false` with `parser` `None` is an in-memory checkpoint of a parser
    /// inside a hooked DCS string, which no handoff carries.
    pub parser_ground: bool,
    /// The parser's partial state ([`ParserCarry`], the round-five plan's item
    /// 12): a sequence split across PTY reads (a CSI, an OSC title, the head
    /// of a UTF-8 glyph), which the restored parser continues so the rest of
    /// it — still queued on the PTY — is read as the sequence it is, not as
    /// text. `None` for a Ground parser with nothing pending. A restore
    /// validates it ([`aterm_parser::Parser::restore_carry`]) and, on a
    /// refusal, resumes at Ground.
    pub parser: Option<ParserCarry>,
    /// The authorized shell-integration nonce — set ONLY by the
    /// seamless-handoff projections ([`Terminal::checkpoint_carry`] and
    /// [`Terminal::checkpoint_carry_abandoning_partial`]), and
    /// `None` in every in-memory checkpoint. Never installed by a restore: the
    /// adopting host authorizes it explicitly (auth is a host binding, see the
    /// EXCLUDED block).
    pub shell_integration_nonce: Option<ShellIntegrationNonce>,
    /// The absolute row counter of the grid in `grid`. A restore continues it
    /// (`Grid::continue_absolute_numbering`), so the absolute rows `shell`
    /// carries name the same lines on the restored grid.
    pub absolute_row_counter: u64,
    /// The same for the grid in `alt_grid` (`0` when there is none).
    pub alt_absolute_row_counter: u64,
    /// Application-set colours, as a diff from the configured baseline.
    pub color: ColorRepr,
    /// Shell-integration (OSC 133/633) state.
    pub shell: ShellRepr,
    /// The integration BODY the shell last signed that it runs
    /// ([`Terminal::shell_integration_rev`], the LOADER / BODY split of
    /// 2026-09-26) — set, like the nonce, ONLY by the seamless-handoff carry
    /// projections, so the successor's `status integration_rev=` can name an
    /// adopted shell's body before its next prompt re-signs it. A FACT about
    /// the shell, not an authority: a restore installs it (a checkpoint without
    /// one leaves the engine's alone).
    pub shell_integration_rev: Option<String>,
    /// The titles a program set ([`TitleRepr`]). `Some` in every checkpoint a
    /// terminal projects, an untitled one included; `None` ONLY for one
    /// reassembled from a handoff whose producer carried no title — a build
    /// before this field, or a value that did not read. A restore then leaves
    /// the engine's title alone, and the adopting host may seed it from what
    /// else it knows (`aterm-gui`'s handoff record). Presence is the whole
    /// signal, so it is never folded into an empty title.
    pub title: Option<TitleRepr>,
}

impl Terminal {
    /// Whether OSC 133/633 marks can reach this terminal
    /// ([`ShellIntegrationPosture`]): required-and-authorized, required with no
    /// nonce in use (every mark dropped so far), or not required. A re-key the
    /// shell has not taken yet ([`Self::authorize_shell_integration_on_first_mark`])
    /// is not in use: it reads `Degraded` until a mark signed with it arrives.
    #[must_use]
    pub fn shell_integration_posture(&self) -> ShellIntegrationPosture {
        match (
            self.modes.require_shell_integration_nonce,
            self.shell_integration_auth.nonce_in_use().is_some(),
        ) {
            (false, _) => ShellIntegrationPosture::Off,
            (true, true) => ShellIntegrationPosture::On,
            (true, false) => ShellIntegrationPosture::Degraded,
        }
    }

    /// Capture a [`TerminalCheckpoint`] — a pure read, no host effects, no fs.
    ///
    /// The parser MUST be in Ground state (B.3.3): a checkpoint taken
    /// mid-sequence would silently lose the parser's partial state (which is not
    /// in the projection). This is `debug_assert`ed.
    #[must_use]
    pub fn checkpoint(&self) -> TerminalCheckpoint {
        self.checkpoint_with_scrollback(true)
    }

    /// [`Self::checkpoint`] for an engine that may be parked INSIDE a partial
    /// sequence: the same full projection, taken without the Ground
    /// precondition, carrying the partial state in
    /// [`TerminalCheckpoint::parser`] so a terminal restored from it continues
    /// the sequence with the next byte exactly as this one would. `None` only
    /// for a partial sequence no carry can hold
    /// ([`Self::partial_sequence_uncarried`], a hooked DCS string).
    ///
    /// Since the parser carry an adopted engine can be restored mid-sequence
    /// (an update's reader parked inside a split CSI), and the recorder's
    /// first keyframe of such an engine is taken before its reader runs — so
    /// it cannot wait for a Ground point without losing the bytes that reach
    /// one. Like `checkpoint`, it carries no shell-integration nonce.
    #[must_use]
    pub fn checkpoint_continuing(&self) -> Option<TerminalCheckpoint> {
        self.partial_sequence_uncarried()
            .is_none()
            .then(|| self.project_bounded(usize::MAX, usize::MAX, self.parser_is_ground()))
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
    ///
    /// `None` only when the parser holds a partial sequence no carry can hold
    /// ([`Self::partial_sequence_uncarried`]); any other partial state rides
    /// in [`TerminalCheckpoint::parser`].
    ///
    /// This (with its mid-sequence twin,
    /// [`Self::checkpoint_carry_abandoning_partial`]) is also the one
    /// projection that carries the authorized shell-integration nonce
    /// ([`TerminalCheckpoint::shell_integration_nonce`]): the adopted shell
    /// keeps signing its marks with it across the update.
    #[must_use]
    pub fn checkpoint_carry(&self, max_history: usize) -> Option<TerminalCheckpoint> {
        self.partial_sequence_uncarried().is_none().then(|| {
            let mut c = self.project_bounded(max_history, 0, true);
            c.shell_integration_nonce = self.carried_shell_integration_nonce();
            c.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
            c
        })
    }

    /// [`Self::checkpoint_carry`] for a parser that may hold a partial
    /// sequence no carry can: the same projection (visible screen plus at most
    /// `max_history` lines, the inactive grid pinned to its visible rows),
    /// taken WITHOUT that precondition, plus the name of the parser state
    /// whose partial sequence the projection leaves out (`None` when nothing
    /// was left out, in which case the checkpoint equals
    /// `checkpoint_carry(max_history)`).
    ///
    /// SINCE THE PARSER CARRY (the round-five plan's item 12) almost every
    /// partial sequence is CARRIED, not abandoned: a split CSI, an OSC title
    /// still arriving, the head of a UTF-8 glyph ride in
    /// [`TerminalCheckpoint::parser`] and the successor continues them; an
    /// unhooked DCS string, an SOS/PM/APC string and an OSC over the carry cap
    /// ride as strings to swallow to their terminator. What is left out, and
    /// what the rest of this doc describes, is a HOOKED DCS string
    /// (`DcsPassthrough`), whose state lives in its handler.
    ///
    /// Why it exists: a session's parser can sit outside Ground indefinitely
    /// — an unterminated OSC/DCS/APC string (`printf '\e]0;x'`) ends only on
    /// BEL, ST, CAN, SUB or ESC, and an ssh session stalled mid-escape or a
    /// Ctrl-S mid-SGR leaves a CSI half-read. The park gate only waits for a
    /// quiet PTY, so `checkpoint_carry` returning `None` made the in-session
    /// update refuse "a terminal parser was mid-sequence" on every attempt for
    /// as long as that tab stayed that way (the 2026-09-22/23 update audit).
    ///
    /// The semantics are CAN (0x18) applied to the COPY: the carried
    /// checkpoint describes the engine as if the partial sequence had been
    /// cancelled, so `parser_ground` is `true` and a terminal restored from it
    /// starts in Ground and treats the next byte as fresh input. The LIVE
    /// terminal is only read — its parser keeps the partial sequence — so a
    /// handoff that rolls back resumes byte-exactly where it parked.
    ///
    /// WHAT ABANDONING COSTS DEPENDS ON THE REST OF THE STRING, and the caller
    /// owns that. Whatever of it the program still sends reaches the restored
    /// engine's Ground parser as ordinary input: the rest of a sixel payload
    /// prints as text — exactly what a CAN mid-string does in any terminal.
    /// For a STALLED string (the program went quiet, the case this exists for)
    /// that tail is at most a late handful of bytes, and dropping one
    /// half-received image is the whole cost; refusing cost the update. For a
    /// string whose tail is ALREADY QUEUED on the PTY (a reader parked in the
    /// middle of a flood) it is visible garbage in the transcript, so the
    /// seamless capture does not abandon there: it asks
    /// [`Self::partial_sequence_uncarried`] first and treats such a parser over
    /// queued output as a timing miss, re-parked a moment later on a sequence
    /// boundary (the 2026-09-24 review of the update audit branch).
    #[must_use]
    pub fn checkpoint_carry_abandoning_partial(
        &self,
        max_history: usize,
    ) -> (TerminalCheckpoint, Option<&'static str>) {
        // A FAULT POINT for this projection failing (a const `false` outside
        // tests): it is the one a seamless capture takes of a LIVE session's
        // grids, and so the producer path a successor's `carry = "repaint"`
        // handoff policy is sealed to route around — a test proves the Repaint
        // rung never reaches it.
        assert!(
            !crate::fault::triggered("checkpoint.carry_projection"),
            "fault injected: the carry projection failed"
        );
        let mut carry = self.project_bounded(max_history, 0, true);
        carry.shell_integration_nonce = self.carried_shell_integration_nonce();
        carry.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
        (carry, self.partial_sequence_uncarried())
    }

    /// The SCALAR half of [`Self::checkpoint_carry_abandoning_partial`]`(0)` —
    /// exactly the [`CheckpointMeta`] of that carry — WITHOUT projecting either
    /// grid.
    ///
    /// For the seamless capture's Repaint rung (`aterm-gui`'s
    /// `seamless::repaint_carry_for_wire`), which carries a blank canonical screen
    /// and reads nothing of the grids but their geometry and cursors. Taking the
    /// full carry there serialized the visible and the inactive grid inside the
    /// frozen window only to throw both away — and ran the very grid code a
    /// successor's `carry = "repaint"` handoff policy is sent to route around when
    /// a release finds it slow or panicking.
    #[cfg(feature = "serde")]
    #[must_use]
    pub fn carry_meta_abandoning_partial(&self) -> CheckpointMeta {
        let mut scalars = self.project(0, 0, true, false);
        scalars.shell_integration_nonce = self.carried_shell_integration_nonce();
        scalars.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
        CheckpointMeta::from_checkpoint(&scalars)
    }

    /// The authorized shell-integration nonce as the two seamless-handoff
    /// projections carry it — [`Self::checkpoint_carry`] and
    /// [`Self::checkpoint_carry_abandoning_partial`], and no other. Only a nonce
    /// IN USE: a re-key the shell has not taken yet stays behind, and the
    /// successor re-keys the shell afresh.
    fn carried_shell_integration_nonce(&self) -> Option<ShellIntegrationNonce> {
        self.shell_integration_auth
            .nonce_in_use()
            .map(ShellIntegrationNonce)
    }

    /// The parser state a partial escape sequence is sitting in (`CsiParam`,
    /// `OscString`, `SosPmApcString`, …), or `None` at Ground. Whether that
    /// sequence is carried or abandoned is
    /// [`Self::partial_sequence_uncarried`]'s answer. A pure read of the
    /// parser; nothing is projected.
    #[must_use]
    pub fn partial_sequence_state(&self) -> Option<&'static str> {
        let state = self.parser.state();
        (!state.is_ground()).then(|| state.name())
    }

    /// The parser state a partial sequence NO CARRY CAN HOLD is sitting in —
    /// `DcsPassthrough`, a hooked DCS string whose state lives in its handler
    /// — or `None` when the parser's state is carried whole
    /// ([`TerminalCheckpoint::parser`]). What the seamless capture asks before
    /// it treats a mid-sequence parser over queued output as a timing miss: a
    /// carried sequence continues in the successor, so its queued tail is no
    /// hazard. A pure read.
    #[must_use]
    pub fn partial_sequence_uncarried(&self) -> Option<&'static str> {
        self.parser
            .carry()
            .is_none()
            .then(|| self.parser.state().name())
    }

    /// The parser state a partial sequence is sitting in when the carry holds
    /// it only as one to SWALLOW to its terminator, where this engine would
    /// still have delivered it — an OSC payload over the carry's cap (an OSC 52
    /// clipboard write, an OSC 1337 image), an unhooked DCS header, an APC
    /// string with its consumer started (a kitty-graphics command) — or `None`
    /// ([`aterm_parser::Parser::carry_swallows_sequence`]). The seamless
    /// capture treats such a parser over queued output like
    /// [`Self::partial_sequence_uncarried`]'s: a timing miss, re-parked on a
    /// sequence boundary, because the successor would swallow the queued tail
    /// and dispatch nothing (round six of the update audit, finding 30). Over
    /// a quiet PTY the carry still goes: a stalled sequence may never end, and
    /// swallowing its eventual tail beats printing it. A pure read.
    #[must_use]
    pub fn partial_sequence_swallowed(&self) -> Option<&'static str> {
        self.parser
            .carry_swallows_sequence()
            .then(|| self.parser.state().name())
    }

    /// The ENGINE state (not the parser's) that output still to come relies on
    /// and no checkpoint carries, or `None`: `"Osc8HyperlinkOpen"` while an
    /// OSC 8 link is open (its text so far linked, its close still to come),
    /// `"Vt52CursorAddress"` while a VT52 `ESC Y` waits for its row and column
    /// bytes. A restored engine resumes with neither, so the rest of the link
    /// text lands unlinked — in scrollback for good — and the two address
    /// bytes print as text. The seamless capture asks this beside
    /// [`Self::partial_sequence_swallowed`] and treats either over queued
    /// output the same way: a timing miss, re-parked when the queued output
    /// has moved past it (round six of the update audit, finding 47 (b) and
    /// (d)). Over a quiet PTY it is carried as before (a link left open may
    /// never close). The REP character and an SGR 58 underline colour are not
    /// named: the one repeats a character a redraw puts back, the other tints
    /// an underline. A pure read.
    #[must_use]
    pub fn output_state_uncarried(&self) -> Option<&'static str> {
        if self.transient.vt52_cursor_state != super::Vt52CursorState::None {
            Some("Vt52CursorAddress")
        } else if self.transient.current_hyperlink.is_some() {
            Some("Osc8HyperlinkOpen")
        } else {
            None
        }
    }

    /// A carried synchronized-update window (mode 2026) gets a fresh timer.
    /// `modes` crosses whole, so a session parked between `?2026h` and
    /// `?2026l` resumes with the mode set — but the window's start instant is
    /// `transient` state and does not cross, and the engine's own timeout
    /// fires only from that instant: a program that died before its `?2026l`
    /// left the mode on for good (round six of the update audit, finding 47).
    /// Armed as `?2026h` arms it, at this engine's processing clock, so the
    /// carried window ends at its closing `?2026l`, as it would have, or times
    /// out like any other.
    fn arm_carried_sync_window(&mut self) {
        self.transient.sync_start = self
            .modes
            .synchronized_output
            .then_some(self.transient.process_now);
    }

    /// The parser's partial state as a checkpoint carries it: `None` for a
    /// Ground parser with nothing pending (a fresh parser already is that) and
    /// for a hooked DCS string (which no carry can hold).
    fn carried_parser(&self) -> Option<ParserCarry> {
        self.parser.carry().filter(|carry| !carry.is_ground())
    }

    /// Install a checkpoint's parser carry. `None` leaves the parser as it is
    /// (every checkpoint before the carry, and a Ground one). A carry the
    /// parser refuses — a hostile or skewed producer — costs the partial
    /// sequence and nothing else: the parser resumes at Ground, as it did
    /// before the carry existed.
    fn restore_parser_carry(&mut self, carry: Option<&ParserCarry>) {
        let Some(carry) = carry else {
            return;
        };
        if let Err(refusal) = self.parser.restore_carry(carry) {
            self.parser.reset();
            aterm_log::warn!(
                "checkpoint restore: the carried parser state was refused ({refusal}); the \
                 parser resumes at Ground"
            );
        }
    }

    /// Whether the engine holds an INACTIVE grid — the one a checkpoint
    /// carries as `alt_grid` (the saved primary while the alternate screen is
    /// up, or the alternate screen's grid kept after it was left).
    ///
    /// A carry's cell cost depends on it (the inactive grid is a second visible
    /// grid), and the seamless capture must price a carry exactly as the
    /// consumer and `screen_digest` will — with the inactive grid only when it
    /// is really carried — BEFORE it pays for the projection. Pricing every
    /// exact carry as if it had one made the capture refuse an exact visible
    /// screen that fit, and then carry the same session blank at the same
    /// geometry and cost (the 2026-09-24 review of the 2026-09-22/23 update
    /// audit fixes). A pure read; nothing is projected.
    #[must_use]
    pub fn has_inactive_grid(&self) -> bool {
        self.alt_grid.is_some()
    }

    fn checkpoint_with_scrollback(&self, include_scrollback: bool) -> TerminalCheckpoint {
        // `bounded(MAX)` is exactly the full history and `bounded(0)` exactly the
        // visible screen, so one bound still expresses every mode here. A FULL
        // checkpoint is in-memory only — it never crosses the handoff wire — so
        // both grids may carry the same depth, which is what this projection did
        // before the bounded-carry rewrite.
        let bound = if include_scrollback { usize::MAX } else { 0 };
        self.checkpoint_bounded(bound, bound)
    }

    /// `inactive_max_history` bounds the INACTIVE grid (`alt_grid`) separately
    /// from the active one, because the two have different consumers. The
    /// seamless-handoff wire pins the inactive blob to exactly `rows` records
    /// (see [`Self::checkpoint_carry`]); an in-memory full checkpoint has no
    /// such wire and keeps everything.
    fn checkpoint_bounded(
        &self,
        max_history: usize,
        inactive_max_history: usize,
    ) -> TerminalCheckpoint {
        debug_assert!(
            self.parser_is_ground(),
            "checkpoint() requires parser_is_ground() (B.3.3)"
        );
        self.project_bounded(max_history, inactive_max_history, self.parser_is_ground())
    }

    /// The projection itself, with no parser precondition: `parser_ground` is
    /// what the checkpoint RECORDS about a parser whose state it does not
    /// carry. [`Self::checkpoint_bounded`] records the live state (and asserts
    /// it is Ground); [`Self::checkpoint_carry_abandoning_partial`] records
    /// `true`, because the partial sequence it leaves out is cancelled in the
    /// copy. A carried parser ([`TerminalCheckpoint::parser`]) is recorded
    /// `false` either way: the restore continues it.
    fn project_bounded(
        &self,
        max_history: usize,
        inactive_max_history: usize,
        parser_ground: bool,
    ) -> TerminalCheckpoint {
        self.project(max_history, inactive_max_history, parser_ground, true)
    }

    /// [`Self::project_bounded`], or — `grids == false` — its scalars alone: both
    /// grid blobs left EMPTY (the inactive one present exactly when the engine has
    /// an inactive grid) and `history_lines` 0, as a visible-only projection
    /// records it. Such a checkpoint is never a carry; it exists only to be read
    /// by [`CheckpointMeta::from_checkpoint`], which ignores the blobs
    /// ([`Self::carry_meta_abandoning_partial`]).
    fn project(
        &self,
        max_history: usize,
        inactive_max_history: usize,
        parser_ground: bool,
        grids: bool,
    ) -> TerminalCheckpoint {
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        let parser = self.carried_parser();
        let (grid_bytes, history_lines) = if grids {
            let grid_lines = self.grid.checkpoint_lines_bounded(max_history);
            // How many of those records are history, as the consumer must be told.
            // Derived from the produced vector rather than from `max_history` so it
            // is exact when the ring holds fewer lines than the bound allows.
            let history_lines = u32::try_from(
                grid_lines
                    .len()
                    .saturating_sub(usize::from(self.grid.rows())),
            )
            .unwrap_or(u32::MAX);
            (serialize_lines(&grid_lines), history_lines)
        } else {
            (Vec::new(), 0)
        };
        let cursor = GridCursorRepr::capture(&self.grid);

        // The oldest line of the PRIMARY lineage this projection carries, which
        // is where readable shell marks end. The primary is `alt_grid` while the
        // alternate screen is up (see `from_checkpoint`), carried to its own bound.
        let (primary, primary_bound) = if self.modes.alternate_screen {
            (self.alt_grid.as_ref(), inactive_max_history)
        } else {
            (Some(&self.grid), max_history)
        };
        let oldest_carried_row = primary.map_or(0, |g| {
            let carried = g.scrollback_lines().min(primary_bound) as u64;
            g.visible_to_absolute(0).saturating_sub(carried)
        });

        let (alt_grid, alt_cursor) = match &self.alt_grid {
            Some(inactive) => (
                // `alt_grid` is the INACTIVE grid, NOT "the alternate screen":
                // while the alternate screen is up it holds the SAVED PRIMARY,
                // scrollback and all (`buffer_api`: "the active grid, or the saved
                // primary while the alt screen is up"). The wire gives this blob
                // exactly `rows` records — the single `history_lines` beside it
                // describes the MAIN blob, so a history carried here is
                // unrepresentable — and the consumer validates it with
                // `history = 0`. Projecting it with `max_history` made the whole
                // capture non-canonical the moment any session sat on the
                // alternate screen, and the in-session update then refused to
                // apply, deterministically and forever, on that desk.
                //
                // Hence `inactive_max_history`, which the handoff projection pins
                // to 0 while a full in-memory checkpoint leaves it at the active
                // grid's bound. The old code shared ONE bound between the two
                // grids, which is precisely the conflation this fixes.
                Some(if grids {
                    serialize_lines(&inactive.checkpoint_lines_bounded(inactive_max_history))
                } else {
                    Vec::new()
                }),
                Some(GridCursorRepr::capture(inactive)),
            ),
            None => (None, None),
        };

        TerminalCheckpoint {
            rows,
            cols,
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
            parser_ground: parser_ground && parser.is_none(),
            parser,
            shell_integration_nonce: None,
            absolute_row_counter: self.grid.absolute_row_counter(),
            alt_absolute_row_counter: self.alt_grid.as_ref().map_or(0, Grid::absolute_row_counter),
            color: self.capture_color_repr(),
            shell: self.capture_shell_repr(oldest_carried_row),
            shell_integration_rev: None,
            title: Some(self.capture_title_repr()),
        }
    }

    /// Rebuild a fully-living [`Terminal`] from a checkpoint (B.3.2).
    ///
    /// The rebuilt terminal's *buffer state* matches the source exactly (proven
    /// by the round-trip test). It has `Terminal::new`'s host effects — no
    /// callbacks, no policy engine, default auth — because those are not in the
    /// checkpoint (see the EXCLUDED block); a host that wants its own installs
    /// them through the ordinary setters, or adopts into a configured terminal
    /// with [`Self::restore_checkpoint`].
    #[must_use]
    pub fn from_checkpoint(c: &TerminalCheckpoint) -> Terminal {
        // Which slot holds the ALTERNATE buffer (→ no scrollback ring) is NOT
        // fixed: `grid` is the *active* buffer and `alt_grid` is the *saved* one.
        // The active grid is the alt buffer iff we are on the alt screen; the
        // saved grid is the alt buffer iff we are NOT (it is a saved alt buffer
        // under 1047/1049 exit semantics). Keying `is_alt` off the slot alone
        // wrongly handed the saved MAIN buffer a 0-ring when captured under 1049
        // (alternate_screen == true), discarding its scrollback and breaking the
        // re-checkpoint identity.
        let on_alt = c.modes.alternate_screen;
        let mut active_grid = restore_grid(c.rows, c.cols, &c.grid, &c.cursor, on_alt);
        active_grid.continue_absolute_numbering(c.absolute_row_counter);
        let mut terminal = Terminal::with_grid(active_grid);

        // Saved grid (same restore path; alt buffer ⟺ we are NOT on alt), if present.
        if let (Some(alt_bytes), Some(alt_cursor)) = (&c.alt_grid, &c.alt_cursor) {
            let mut saved = restore_grid(c.rows, c.cols, alt_bytes, alt_cursor, !on_alt);
            saved.continue_absolute_numbering(c.alt_absolute_row_counter);
            terminal.alt_grid = Some(saved);
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
        terminal.apply_color_repr(&c.color);
        terminal.apply_shell_repr(&c.shell);
        if let Some(title) = &c.title {
            terminal.restore_title(title);
        }
        // Last of the input-side state: the title above is restored AS OSC
        // input, which must not run through a carried partial sequence.
        terminal.restore_parser_carry(c.parser.as_ref());
        terminal.arm_carried_sync_window();
        // `modes`/kitty/xterm were assigned by value above, after `with_grid`
        // published the fresh fold — republish for the hydrated state.
        terminal.refresh_mode_mirror();

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
        // RETENTION IS THIS PROCESS'S CONFIG, NOT THE CHECKPOINT'S.
        // `scrollback_lines` reached `self` through the host's normal
        // config-applied constructor (`new_live_terminal` → `apply_config` →
        // `set_scrollback_line_limit`) BEFORE this adopt runs — and
        // `restore_grid` below hands the rebuilt grid a brand-new store with
        // NO limit, deliberately, so the carried history is not dropped on the
        // way in. Nothing put the configured limit back, so every session a
        // seamless update adopted retained scrollback UNBOUNDED for the rest
        // of its life, silently withdrawing a setting that had already reached
        // the engine. (`apply_config` could not repair it either: it early-outs
        // on `current_limit != new_limit`, so only a live config EDIT re-armed
        // it.) Measured: `scrollback_lines = 50` retains 50 lines in a session
        // that never updated and 450 in one that did.
        //
        // Read HERE, while the grids the host configured are still installed —
        // `scrollback_line_limit()` reads the main grid, which `restore_grid`
        // is about to replace.
        let configured_scrollback_limit = self.scrollback_line_limit();
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
        self.grid
            .continue_absolute_numbering(c.absolute_row_counter);
        self.alt_grid = match (&c.alt_grid, &c.alt_cursor) {
            (Some(alt_bytes), Some(alt_cursor)) => {
                let mut saved = restore_grid(c.rows, c.cols, alt_bytes, alt_cursor, !on_alt);
                saved.continue_absolute_numbering(c.alt_absolute_row_counter);
                Some(saved)
            }
            _ => None,
        };
        self.modes = c.modes;
        // …and re-imposed HERE, after `modes` is the adopted session's: the
        // Terminal-level setter targets the PRIMARY-CONTENT grid (`alt_grid`
        // while the alternate screen is up, `grid` otherwise), and that choice
        // is only correct once the restored modes are in place. Going through
        // the setter rather than reaching into a grid keeps "which grid holds
        // the history" as ONE spelling.
        self.set_scrollback_line_limit(configured_scrollback_limit);
        self.cursor_save.main = c.saved_cursor_main.map(SavedCursorRepr::into_saved);
        self.cursor_save.alt = c.saved_cursor_alt.map(SavedCursorRepr::into_saved);
        self.charset = c.charset;
        self.kitty_keyboard.restore_snapshot(c.kitty_keyboard);
        self.xterm_keyboard = c.xterm_keyboard;
        self.taskbar_progress = c.taskbar_progress;
        self.secure_keyboard_entry = c.secure_keyboard_entry;
        self.current_working_directory
            .clone_from(&c.current_working_directory);
        // The shell's integration body, when the carry named one — a fact about
        // the adopted shell, which its next prompt re-signs anyway. Validated
        // like a signed mark; an absent or malformed one leaves the engine's.
        if let Some(rev) = c
            .shell_integration_rev
            .as_deref()
            .filter(|rev| crate::shell_integration::is_integration_rev(rev))
        {
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(rev.as_bytes());
            self.shell.integration_rev = Some(bytes);
        }
        // Style: semantic value, then re-arm the REBUILT grid's BCE cursor
        // template from it (see `from_checkpoint`).
        self.style = c.style.into_style();
        {
            let (_parser, mut handler) = self.split_for_process();
            handler.sgr_style().apply_style_change();
        }
        // The application's colours go over THIS process's configured theme,
        // and its shell-integration state (running command included) resumes.
        self.apply_color_repr(&c.color);
        self.apply_shell_repr(&c.shell);
        // The program's titles, as OSC input (`restore_title`). A checkpoint
        // that carried none (an older producer) leaves this engine's alone.
        if let Some(title) = &c.title {
            self.restore_title(title);
        }
        // After the title, which is restored as OSC input (see
        // `from_checkpoint`).
        self.restore_parser_carry(c.parser.as_ref());
        self.arm_carried_sync_window();
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
        // (Mode mirror: republished at the end of this fn — see below.)
        // Hydration arguably invalidates the LIVE selection too — its anchors name a
        // lineage this terminal never saw — but that is a pre-existing gap, and
        // clearing it here is user-visible on the seamless-update ADOPT path
        // (`spawn.rs`'s `restore_checkpoint`): a highlight would vanish across an
        // in-place update. Left alone deliberately rather than changed as a side
        // effect of screen-scoping.
        self.parked_text_selection.clear();
        // In-place hydration replaced `modes`, the kitty stack and the xterm
        // keyboard state outside `process()`: republish the lock-free fold so
        // the input seam encodes against the adopted session's modes.
        self.refresh_mode_mirror();
        // The alt-screen archive was following the replaced grids: flush, record a
        // `restore` gap, and rebaseline on what was just installed.
        self.alt_archive_after_restore();
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
    /// The authorized shell-integration nonce as 64 hex digits
    /// ([`TerminalCheckpoint::shell_integration_nonce`]). ADDITIVE and absent
    /// when there is none, so a parent without the field, and a session without
    /// integration, add no key for it; a value that is not 64
    /// hex digits reassembles as `None` (the successor reports the session
    /// `integration=degraded` rather than trusting a partial nonce).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_integration_nonce: Option<String>,
    /// [`TerminalCheckpoint::absolute_row_counter`]. ADDITIVE: `0` (a parent
    /// without the field) restores the fresh numbering, which is what every
    /// earlier handoff did. Unlike the keys below it is written for EVERY live
    /// session — a grid's counter is at least its rows, never `0` — so no
    /// session's meta is byte-identical to what an older parent wrote; an
    /// older child ignores the key, and the screen digest hashes the bytes
    /// as sent.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub absolute_row_counter: u64,
    /// [`TerminalCheckpoint::alt_absolute_row_counter`]. ADDITIVE, as above.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub alt_absolute_row_counter: u64,
    /// [`TerminalCheckpoint::color`]. ADDITIVE and absent when nothing is
    /// overridden, so a session with the host's colours adds no key for it.
    #[serde(default, skip_serializing_if = "ColorRepr::is_default")]
    pub color: ColorRepr,
    /// [`TerminalCheckpoint::shell`]. ADDITIVE and absent for a session that
    /// never saw a mark.
    #[serde(default, skip_serializing_if = "ShellRepr::is_default")]
    pub shell: ShellRepr,
    /// The integration body the shell last signed that it runs, 16 hex digits
    /// ([`TerminalCheckpoint::shell_integration_rev`], 2026-09-26). ADDITIVE and
    /// absent when there is none (a parent without the field, a shell whose
    /// script predates loaders), so it adds no key; a value that
    /// is not a folder address reassembles as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_integration_rev: Option<String>,
    /// [`TerminalCheckpoint::title`] (the round-four plan, item 8b). ADDITIVE,
    /// and WRITTEN FOR EVERY SESSION this build carries — `{}` when nothing
    /// titled it — because presence is what tells a successor that the
    /// producer carried titles at all: absent means a producer from before the
    /// field, whose successor may seed the tab's title from the handoff record
    /// instead, while `{}` means "untitled", which must stay untitled. An older
    /// child ignores the key, and the screen digest hashes the bytes as sent
    /// (`absolute_row_counter` set the precedent). A value that does not read
    /// costs the title and nothing else ([`lenient_title`]): the meta, and so
    /// the screen, still parse.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_title"
    )]
    pub title: Option<TitleRepr>,
    /// [`TerminalCheckpoint::parser`] (the round-five plan's item 12).
    /// ADDITIVE, and absent for a Ground parser with nothing pending — every
    /// session a quiet desk carries — so such a meta is byte-identical to what
    /// a producer before the field wrote. An older child ignores the key and
    /// resumes the parser at Ground, which is what it did before; the screen
    /// digest hashes the bytes as sent, so the carry is inside the adoption
    /// proof like every other meta field. A value that does not read costs the
    /// partial sequence and nothing else (`lenient_parser`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::checkpoint_parser::lenient_parser"
    )]
    pub parser: Option<super::checkpoint_parser::ParserRepr>,
}

/// Read a carried [`TitleRepr`], or `None` when the value is not one.
///
/// Why lenient: `CheckpointMeta` is parsed whole, and a strict field that
/// failed would fail it — the successor then adopts that session onto a blank
/// screen and repaints it (`aterm-gui`'s consumer degrade). A title is not
/// worth a screen, so a malformed one reads as "not carried", exactly like a
/// producer that never wrote the key.
#[cfg(feature = "serde")]
fn lenient_title<'de, D>(deserializer: D) -> Result<Option<TitleRepr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Title(TitleRepr),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Wire as serde::Deserialize>::deserialize(deserializer)? {
            Wire::Title(title) => Some(title),
            Wire::Unreadable(_) => None,
        },
    )
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero(n: &u64) -> bool {
    *n == 0
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
            // Derived on reassembly from `parser` (no carry = Ground), never
            // trusted from the wire.
            parser_ground: _,
            parser,
            shell_integration_nonce,
            absolute_row_counter,
            alt_absolute_row_counter,
            color,
            shell,
            shell_integration_rev,
            title,
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
            shell_integration_nonce: shell_integration_nonce.map(|n| n.to_hex()),
            absolute_row_counter: *absolute_row_counter,
            alt_absolute_row_counter: *alt_absolute_row_counter,
            color: color.clone(),
            shell: shell.clone(),
            shell_integration_rev: shell_integration_rev.clone(),
            title: title.clone(),
            parser: parser
                .as_ref()
                .map(super::checkpoint_parser::ParserRepr::from_carry),
        }
    }

    /// Reassemble a full [`TerminalCheckpoint`] from the meta + grid blobs.
    ///
    /// `alt_grid` presence must match `alt_cursor` (both from the same wire);
    /// a half-present pair degrades to no alt grid rather than a mismatched
    /// restore.
    #[must_use]
    pub fn into_checkpoint(self, grid: Vec<u8>, alt_grid: Option<Vec<u8>>) -> TerminalCheckpoint {
        let parser = self
            .parser
            .as_ref()
            .and_then(super::checkpoint_parser::ParserRepr::to_carry)
            .filter(|carry| !carry.is_ground());
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
            // A carry that does not read (an unknown state, bad hex) is no
            // carry: the parser resumes at Ground. One that reads but is not
            // valid is refused where it lands (`restore_parser_carry`).
            parser_ground: parser.is_none(),
            parser,
            shell_integration_nonce: self
                .shell_integration_nonce
                .as_deref()
                .and_then(ShellIntegrationNonce::from_hex),
            absolute_row_counter: self.absolute_row_counter,
            alt_absolute_row_counter: self.alt_absolute_row_counter,
            color: self.color,
            shell: self.shell,
            shell_integration_rev: self
                .shell_integration_rev
                .filter(|rev| crate::shell_integration::is_integration_rev(rev)),
            // Bounded where it lands (`Terminal::restore_title`), not here: the
            // reassembled checkpoint is data until a terminal takes it.
            title: self.title,
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

    /// THE REPAINT RUNG'S SCALARS, WITHOUT THE GRIDS: `carry_meta_abandoning_partial`
    /// is exactly the meta of the visible-only carry, for every state that moves a
    /// scalar — scrollback, the alternate screen (an inactive grid), a parser left
    /// mid-sequence, shell marks — and it never takes the carry projection (the
    /// fault point below would fail it), where the carry itself does.
    #[cfg(feature = "serde")]
    #[test]
    fn the_carry_meta_is_the_visible_carrys_meta_and_projects_no_grid() {
        let mut alt = build_rich_terminal(6, 24);
        alt.process(b"\x1b[?1049hon the alt screen\x1b]8;;https://example.com\x07link\x1b]8;;\x07");
        let mut partial = build_rich_terminal(6, 24);
        partial.process(b"\x1b]0;an unterminated title");
        for (label, terminal) in [
            ("plain", Terminal::new(4, 20)),
            ("rich", build_rich_terminal(6, 24)),
            ("alt screen", alt),
            ("mid-sequence", partial),
        ] {
            let (carry, _) = terminal.checkpoint_carry_abandoning_partial(0);
            let meta = crate::fault::with_armed("checkpoint.carry_projection", || {
                terminal.carry_meta_abandoning_partial()
            });
            assert_eq!(meta, CheckpointMeta::from_checkpoint(&carry), "{label}");
            let projected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::fault::with_armed("checkpoint.carry_projection", || {
                    terminal.checkpoint_carry_abandoning_partial(0)
                })
            }));
            assert!(projected.is_err(), "{label}: the carry does take it");
        }
    }

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

        // --- taskbar progress (ConEmu OSC 9;4, `handle_osc_9`) ---
        t.process(b"\x1b]9;4;1;42\x07");

        // --- secure keyboard entry: also host-set (no OSC); use the setter ---
        t.set_secure_keyboard_entry(true);

        // --- OSC 7 cwd ---
        t.process(b"\x1b]7;file://host/tmp/work\x07");

        // --- colours: an OSC 4 palette override, OSC 10/12 dynamic colours,
        //     and one XTPUSHCOLORS entry holding the overridden state. OSC 4
        //     SET is a host opt-in (`allow_palette_reconfigure`), and `modes`
        //     round-trips with it on. ---
        t.modes.allow_palette_reconfigure = true;
        t.process(b"\x1b]4;1;rgb:12/34/56\x07\x1b]10;rgb:aa/bb/cc\x07\x1b]12;rgb:01/02/03\x07");
        t.process(b"\x1b]30001\x07");
        t.process(b"\x1b]4;2;rgb:65/43/21\x07");

        // --- shell integration: one finished command, then one RUNNING (C
        //     without D), so phase, marks, blocks and counters all carry ---
        t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 9\r\n\x1b]133;C\x07");

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
        assert_eq!(
            t.taskbar_progress,
            Some(TaskbarProgress::Normal(42)),
            "taskbar captured"
        );
        assert!(t.current_working_directory.is_some(), "cwd captured");
        assert!(
            t.alt_grid.is_some(),
            "alt grid present (main saved under alt)"
        );

        let c0 = t.checkpoint();
        // Sanity: the colour and shell projections are non-trivial.
        assert_eq!(c0.color.palette.len(), 2, "two OSC 4 overrides");
        assert!(c0.color.foreground.is_some() && c0.color.cursor.is_some());
        assert_eq!(c0.color.stack.len(), 1, "one XTPUSHCOLORS entry");
        assert_eq!(c0.shell.command_marks.len(), 1, "one finished command");
        assert!(
            c0.shell.current_block.is_some(),
            "a running command's block"
        );
        assert_eq!(c0.shell.completed_seq, 1);
        let h = Terminal::from_checkpoint(&c0);
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

        let restored = Terminal::from_checkpoint(&visible);
        assert_eq!(t.visible_content(), restored.visible_content());
        assert_eq!(t.cursor(), restored.cursor());
        assert_eq!(
            Some(visible),
            restored.checkpoint_visible(),
            "restored parser remains Ground"
        );
    }

    /// A partial sequence rides the visible checkpoint (the round-five plan's
    /// item 12) — only a hooked DCS string, which no carry can hold, still
    /// fails closed instead of dropping parser bytes.
    #[test]
    fn visible_checkpoint_carries_a_partial_parser_sequence() {
        let mut terminal = Terminal::new(4, 20);
        terminal.process(b"prefix\x1b[");
        assert!(!terminal.parser_is_ground());
        let cp = terminal
            .checkpoint_visible()
            .expect("a split CSI is carried");
        assert!(!cp.parser_ground);
        assert_eq!(
            cp.parser.as_ref().map(|carry| carry.state.name()),
            Some("CsiEntry")
        );
        let mut restored = Terminal::from_checkpoint(&cp);
        restored.process(b"0m!");
        assert_eq!(
            restored.row_text(0).as_deref().map(str::trim_end),
            Some("prefix!"),
            "the tail finished the sequence instead of printing"
        );
        terminal.process(b"0m");
        assert!(terminal.parser_is_ground());
        assert!(terminal.checkpoint_visible().is_some());

        terminal.process(b"\x1bPq#0;2;0;0;0");
        assert_eq!(
            terminal.partial_sequence_uncarried(),
            Some("DcsPassthrough")
        );
        assert!(
            terminal.checkpoint_visible().is_none(),
            "a hooked DCS string fails closed"
        );
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

        let mut restored = Terminal::from_checkpoint(&scrolled);
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
        let mut restored = Terminal::from_checkpoint(&checkpoint);

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
        let mut h = Terminal::from_checkpoint(&c0);

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
        let mut h = Terminal::from_checkpoint(&c0);

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

        let h = Terminal::from_checkpoint(&c0);
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

        let h = Terminal::from_checkpoint(&c0);
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
        let h = Terminal::from_checkpoint(&c0);
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
        let mut h = Terminal::from_checkpoint(&c0);
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
    /// The engine keeps a DECSC slot RAW across a resize, as xterm does, and
    /// the wire carries it RAW — see `SavedCursorRepr::capture`. Pinned
    /// together because the 2026-09-22 refusal needed both to be true at once,
    /// and this fix's first draft clamped the wire, which was measured lossy
    /// once the grid grew back.
    #[test]
    fn a_resize_leaves_the_decsc_slot_raw_and_the_wire_carries_it_raw() {
        let mut t = Terminal::new(56, 149);
        t.process(b"\x1b[56;1H\x1b[?1049h");
        assert_eq!(t.cursor_save.main.map(|saved| saved.cursor.row), Some(55));
        t.resize(55, 149);
        assert_eq!(
            t.cursor_save.main.map(|saved| saved.cursor.row),
            Some(55),
            "the slot is absolute and untouched by the resize"
        );
        let cp = t.checkpoint_carry(0).expect("Ground");
        assert_eq!(cp.rows, 55);
        assert_eq!(
            cp.saved_cursor_main.map(|saved| saved.cursor_row),
            Some(55),
            "the wire carries the slot as the engine holds it, one row past the grid"
        );
        // Grown back before the slot is read, the cursor lands where it was
        // saved — on the live engine and on one restored from that wire alike.
        let mut restored = Terminal::from_checkpoint(&cp);
        for terminal in [&mut t, &mut restored] {
            terminal.resize(56, 149);
            terminal.process(b"\x1b[?1049l");
        }
        assert_eq!(t.cursor().row, 55);
        assert_eq!(restored.cursor(), t.cursor());
    }

    /// A SYNCHRONIZED-UPDATE WINDOW OPEN AT THE CHECKPOINT STILL TIMES OUT
    /// (round six of the update audit, finding 47). `modes` crosses whole, so
    /// the restored engine has mode 2026 set; its timer is re-armed at the
    /// restore, so a program that never sends `?2026l` cannot leave it set for
    /// good — on either restore path. CONTROL: a `?2026l` in the replayed tail
    /// closes it as it always did.
    ///
    /// RED before the fix: `sync_start` was `None` after the restore, the
    /// engine's timeout never fired, and the mode stayed on.
    #[test]
    fn a_carried_synchronized_update_window_still_times_out() {
        let mut t = Terminal::new(24, 80);
        t.process(b"$ vim\r\n\x1b[?2026h");
        let cp = t.checkpoint_carry(0).expect("carried");
        assert!(
            cp.modes.synchronized_output,
            "PRECONDITION: the window is open"
        );

        let mut fresh = Terminal::from_checkpoint(&cp);
        let mut adopted = Terminal::new(24, 80);
        adopted.restore_checkpoint(&cp);
        for (at, restored) in [("from_checkpoint", &mut fresh), ("restore", &mut adopted)] {
            assert!(restored.modes().synchronized_output(), "{at}: carried open");
            restored.backdate_sync_start(std::time::Duration::from_secs(120));
            restored.process(b"x");
            assert!(
                !restored.modes().synchronized_output(),
                "{at}: the carried window timed out"
            );
        }

        let mut closed = Terminal::from_checkpoint(&cp);
        closed.process(b"frame\x1b[?2026l");
        assert!(!closed.modes().synchronized_output());
    }

    /// The engine states a checkpoint does not carry and output still queued
    /// relies on (round six of the update audit, finding 47 (b) and (d)):
    /// an open OSC 8 link and a VT52 `ESC Y` waiting for its address bytes.
    /// `output_state_uncarried` names each while it is pending and neither
    /// once it is done — and the restore really does lose each, which is why
    /// the capture must not carry it over queued output.
    #[test]
    fn output_state_uncarried_names_an_open_link_and_a_pending_vt52_address() {
        let mut t = Terminal::new(4, 20);
        assert_eq!(t.output_state_uncarried(), None);

        t.process(b"\x1b]8;;https://example.com\x07lin");
        assert_eq!(t.output_state_uncarried(), Some("Osc8HyperlinkOpen"));
        let cp = t.checkpoint_carry(0).expect("carried");
        assert!(
            Terminal::from_checkpoint(&cp).current_hyperlink().is_none(),
            "the restore loses the open link"
        );
        t.process(b"k\x1b]8;;\x07");
        assert_eq!(t.output_state_uncarried(), None, "the link is closed");

        t.process(b"\x1b[?2l\x1bY");
        assert_eq!(t.output_state_uncarried(), Some("Vt52CursorAddress"));
        t.process(b"\x22");
        assert_eq!(
            t.output_state_uncarried(),
            Some("Vt52CursorAddress"),
            "the row is in, the column still to come"
        );
        t.process(b"\x24");
        assert_eq!(t.output_state_uncarried(), None, "the address is complete");
        assert_eq!((t.cursor().row, t.cursor().col), (2, 4));
        t.process(b"\x1b<");
        assert_eq!(t.output_state_uncarried(), None);
    }

    /// The 2026-09-22/23 update audit's stuck-parser desk: `printf '\e]0;x'`
    /// with no terminator leaves the parser in `OscString` until more output
    /// arrives. Since the parser carry (the round-five plan's item 12) the
    /// carry takes the partial OSC along: nothing is abandoned, the restored
    /// engine is still inside the title, and the terminator that arrives after
    /// the handoff completes it. The LIVE parser is only read, so a rolled-back
    /// handoff loses nothing either.
    #[test]
    fn an_unterminated_osc_is_carried_whole() {
        let mut t = Terminal::new(4, 20);
        t.process(b"hello\x1b]0;x");
        assert!(!t.parser_is_ground());
        let (cp, abandoned) = t.checkpoint_carry_abandoning_partial(0);
        assert_eq!(abandoned, None, "nothing is abandoned");
        assert_eq!(Some(&cp), t.checkpoint_carry(0).as_ref());
        assert!(!cp.parser_ground, "the restore continues the OSC");
        assert_eq!(cp.history_lines, 0);

        assert!(!t.parser_is_ground(), "the live parser is not mutated");
        t.process(b"\x07");
        assert_eq!(t.title(), "x", "the live partial OSC survived the capture");

        let mut restored = Terminal::from_checkpoint(&cp);
        assert!(!restored.parser_is_ground());
        restored.process(b"\x07 world");
        assert_eq!(restored.title(), "x", "the carried OSC completed");
        assert_eq!(
            restored.row_text(0).as_deref().map(str::trim_end),
            Some("hello world")
        );
    }

    /// A HOOKED DCS string is the one partial sequence no carry holds (its
    /// state lives in the handler): the abandoning carry projects it anyway,
    /// names what it dropped, and restores into Ground — exactly CAN applied
    /// to the copy.
    #[test]
    fn a_hooked_dcs_is_carried_by_abandoning_it() {
        let mut t = Terminal::new(4, 20);
        t.process(b"hello\x1bPq#0;2;0;0;0");
        assert!(t.checkpoint_carry(0).is_none(), "control: no exact carry");
        let (cp, abandoned) = t.checkpoint_carry_abandoning_partial(0);
        assert_eq!(
            abandoned,
            Some("DcsPassthrough"),
            "the dropped state is named"
        );
        assert!(cp.parser_ground && cp.parser.is_none());
        assert_eq!(
            t.parser.state().name(),
            "DcsPassthrough",
            "live parser untouched"
        );
        let mut restored = Terminal::from_checkpoint(&cp);
        assert!(restored.parser_is_ground());
        restored.process(b" world");
        assert_eq!(
            restored.row_text(0).as_deref().map(str::trim_end),
            Some("hello world")
        );
        let mut twin = Terminal::new(4, 20);
        twin.process(b"hello\x1bPq#0;2;0;0;0\x18");
        assert_eq!(twin.checkpoint_carry(0).expect("Ground"), cp);
    }

    /// Every non-Ground state the parser can be parked in is carried — whole,
    /// or (a DCS before its hook, an SOS/PM/APC string) as a string to ignore
    /// to its terminator — except a hooked DCS string, which is abandoned and
    /// named. None of them trips `checkpoint_bounded`'s Ground `debug_assert`
    /// (these tests run with debug assertions on, so a path through it would
    /// panic here), and each restored carry re-projects to itself.
    #[test]
    fn every_partial_parser_state_is_carried_and_named() {
        let partials: [(&[u8], &str, Option<&str>); 9] = [
            (b"\x1b", "Escape", Some("Escape")),
            (b"\x1b(", "EscapeIntermediate", Some("EscapeIntermediate")),
            (b"\x1b[", "CsiEntry", Some("CsiEntry")),
            (b"\x1b[1;", "CsiParam", Some("CsiParam")),
            (b"\x1b[?1$", "CsiIntermediate", Some("CsiIntermediate")),
            (b"\x1bP", "DcsEntry", Some("DcsIgnore")),
            (b"\x1bPq#0;2;0;0;0", "DcsPassthrough", None),
            (b"\x1b]2;half a title", "OscString", Some("OscString")),
            (b"\x1b_Gf=100;", "SosPmApcString", Some("SosPmApcString")),
        ];
        for (partial, expected, carried) in partials {
            let mut t = Terminal::new(6, 30);
            t.process(b"\x1b[1mbold\x1b[0m\r\n");
            for i in 0..12 {
                t.process(format!("history {i}\r\n").as_bytes());
            }
            t.process(partial);
            let live_state = t.parser.state();
            assert_eq!(live_state.name(), expected, "{partial:?}");

            let (cp, abandoned) = t.checkpoint_carry_abandoning_partial(5);
            assert_eq!(
                abandoned,
                carried.is_none().then_some(expected),
                "{partial:?}"
            );
            assert_eq!(
                cp.parser.as_ref().map(|carry| carry.state.name()),
                carried,
                "{partial:?}"
            );
            assert_eq!(cp.parser_ground, carried.is_none(), "{partial:?}");
            assert_eq!(cp.history_lines, 5, "the history bound still applies");
            assert_eq!(t.parser.state(), live_state, "live parser untouched");

            let restored = Terminal::from_checkpoint(&cp);
            assert_eq!(
                restored.parser.state().name(),
                carried.unwrap_or("Ground"),
                "{partial:?}"
            );
            assert_eq!(restored.visible_content(), t.visible_content());
            assert_eq!(
                restored.checkpoint_carry_abandoning_partial(5).0.parser,
                cp.parser,
                "{partial:?}: a restored carry re-projects to itself"
            );
        }
    }

    /// On a Ground parser the abandoning carry IS the ordinary carry — so a
    /// producer can call it unconditionally without changing a healthy desk's
    /// bytes (and so the adoption digest of every healthy carry is unchanged).
    #[test]
    fn the_abandoning_carry_of_a_ground_parser_is_the_ordinary_carry() {
        let t = build_rich_terminal(12, 40);
        for max_history in [0, 3, usize::MAX] {
            assert_eq!(
                t.checkpoint_carry_abandoning_partial(max_history),
                (t.checkpoint_carry(max_history).expect("Ground"), None)
            );
        }
    }

    /// The two pre-projection reads the seamless capture prices and gates a
    /// carry with agree with the projection they stand in for: the partial
    /// sequence named is the one the abandoning carry drops, and the inactive
    /// grid is present exactly when the checkpoint carries an `alt_grid` —
    /// before 1049 (none), on the alternate screen (the saved primary), and
    /// after leaving it (the kept alternate grid, which is why this cannot be
    /// read off `is_alternate_screen`).
    #[test]
    fn the_pre_projection_reads_agree_with_the_projection() {
        let mut t = Terminal::new(6, 30);
        assert_eq!(t.partial_sequence_state(), None);
        assert!(!t.has_inactive_grid());
        assert!(t.checkpoint_carry(0).expect("Ground").alt_grid.is_none());

        t.process(b"\x1b[?1049h");
        assert!(t.has_inactive_grid());
        assert!(t.checkpoint_carry(0).expect("Ground").alt_grid.is_some());

        t.process(b"\x1b[?1049l");
        assert!(!t.is_alternate_screen());
        assert_eq!(
            t.has_inactive_grid(),
            t.checkpoint_carry(0).expect("Ground").alt_grid.is_some(),
            "after 1049 exit the read still matches the projection"
        );

        t.process(b"\x1b[38;5;19");
        assert_eq!(t.partial_sequence_state(), Some("CsiParam"));
        assert_eq!(
            t.partial_sequence_uncarried(),
            None,
            "a split CSI is carried"
        );
        assert_eq!(
            t.checkpoint_carry_abandoning_partial(0).1,
            t.partial_sequence_uncarried()
        );
        t.process(b"6m");
        assert_eq!(t.partial_sequence_state(), None);

        t.process(b"\x1bPq#0;2;0;0;0");
        assert_eq!(t.partial_sequence_uncarried(), Some("DcsPassthrough"));
        assert_eq!(
            t.checkpoint_carry_abandoning_partial(0).1,
            t.partial_sequence_uncarried()
        );
    }

    /// The abandoning carry is a seamless-handoff projection too, so it carries
    /// the authorized shell-integration nonce exactly as `checkpoint_carry`
    /// does — a tab whose parser was mid-sequence at the park keeps signing
    /// its marks across the update (main's nonce carry, 2026-09-2x, meeting
    /// the update audit's abandoning carry in the merge).
    #[test]
    fn the_abandoning_carry_carries_the_shell_integration_nonce() {
        let nonce = [0x5Au8; 32];
        let mut t = Terminal::new(4, 20);
        t.authorize_shell_integration(nonce);
        t.set_require_shell_integration_nonce(true);
        let (ground, _) = t.checkpoint_carry_abandoning_partial(0);
        assert_eq!(
            ground.shell_integration_nonce,
            Some(ShellIntegrationNonce(nonce))
        );
        assert_eq!(Some(ground), t.checkpoint_carry(0));

        t.process(b"\x1b]0;half");
        let (partial, abandoned) = t.checkpoint_carry_abandoning_partial(0);
        assert_eq!(abandoned, None, "the partial OSC is carried");
        assert!(partial.parser.is_some());
        assert_eq!(
            partial.shell_integration_nonce,
            Some(ShellIntegrationNonce(nonce)),
            "a mid-sequence tab keeps its nonce across the handoff"
        );
    }

    /// The handoff projection carries the authorized shell-integration nonce;
    /// the in-memory one never does; the meta wire round-trips it; and a
    /// malformed value reassembles as no nonce. `Debug` never prints it.
    #[test]
    fn only_the_carry_projection_takes_the_shell_integration_nonce() {
        let nonce = [0xA5u8; 32];
        let mut t = Terminal::new(4, 20);
        assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::Off);
        t.authorize_shell_integration(nonce);
        t.set_require_shell_integration_nonce(true);
        assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);

        assert_eq!(t.checkpoint().shell_integration_nonce, None);
        let carry = t.checkpoint_carry(0).expect("Ground");
        assert_eq!(
            carry.shell_integration_nonce,
            Some(ShellIntegrationNonce(nonce))
        );
        assert!(!format!("{carry:?}").contains("a5a5"), "Debug redacts it");

        #[cfg(feature = "serde")]
        {
            let meta = CheckpointMeta::from_checkpoint(&carry);
            assert_eq!(
                meta.shell_integration_nonce.as_deref(),
                Some("a5".repeat(32).as_str())
            );
            let back = meta.clone().into_checkpoint(carry.grid.clone(), None);
            assert_eq!(back.shell_integration_nonce, carry.shell_integration_nonce);
            let mut bad = meta;
            bad.shell_integration_nonce = Some("a5".repeat(31));
            assert_eq!(
                bad.into_checkpoint(carry.grid.clone(), None)
                    .shell_integration_nonce,
                None,
                "a short nonce is no nonce"
            );
        }

        // An adopt that restores the modes (require = true) without the nonce
        // is DEGRADED, and says so; authorizing the carried value heals it.
        let mut adopted = Terminal::new(4, 20);
        adopted.restore_checkpoint(&carry);
        assert_eq!(
            adopted.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
        adopted.authorize_shell_integration(carry.shell_integration_nonce.expect("carried").0);
        assert_eq!(
            adopted.shell_integration_posture(),
            ShellIntegrationPosture::On
        );
    }

    /// THE BODY A SHELL RUNS (the LOADER / BODY split, 2026-09-26): only a SIGNED
    /// `633;P;AtermIntegration=<rev>` records it — with the gate off, or with a
    /// wrong id, the same bytes change nothing — and only a folder address is
    /// taken; a RIS keeps it. Like the nonce, only the carry projections take it
    /// across an update, the meta round-trips it and refuses a malformed one,
    /// and the adopting engine's restore installs it, so the successor names the
    /// shell's body before its next prompt re-signs it.
    #[test]
    fn a_signed_integration_rev_is_recorded_and_carried() {
        let key = [0x5Au8; 32];
        let id = format!(";id={}", ShellIntegrationNonce(key).to_hex());
        let mark = |rev: &str, id: &str| format!("\x1b]633;P;AtermIntegration={rev}{id}\x07");
        let (rev_a, rev_b) = ("0123456789abcdef", "fedcba9876543210");

        // The gate off: a program's output claims nothing.
        let mut t = Terminal::new(4, 20);
        t.process(mark(rev_a, "").as_bytes());
        assert_eq!(t.shell_integration_rev(), None, "unsigned: not recorded");

        t.authorize_shell_integration(key);
        t.set_require_shell_integration_nonce(true);
        t.process(mark(rev_a, ";id=00").as_bytes());
        assert_eq!(t.shell_integration_rev(), None, "a wrong id is dropped");
        for bad in ["0123456789ABCDEF", "0123456789abcde", "../../etc", ""] {
            t.process(mark(bad, &id).as_bytes());
            assert_eq!(t.shell_integration_rev(), None, "{bad:?} is no address");
        }
        t.process(mark(rev_a, &id).as_bytes());
        assert_eq!(t.shell_integration_rev(), Some(rev_a));
        t.process(mark(rev_b, &id).as_bytes());
        assert_eq!(t.shell_integration_rev(), Some(rev_b), "the latest wins");
        t.process(b"\x1bc");
        assert_eq!(t.shell_integration_rev(), Some(rev_b), "RIS keeps it");

        assert_eq!(t.checkpoint().shell_integration_rev, None);
        let carry = t.checkpoint_carry(0).expect("Ground");
        assert_eq!(carry.shell_integration_rev.as_deref(), Some(rev_b));
        assert_eq!(
            t.checkpoint_carry_abandoning_partial(0)
                .0
                .shell_integration_rev
                .as_deref(),
            Some(rev_b)
        );
        #[cfg(feature = "serde")]
        {
            let meta = CheckpointMeta::from_checkpoint(&carry);
            assert_eq!(meta.shell_integration_rev.as_deref(), Some(rev_b));
            let back = meta.clone().into_checkpoint(carry.grid.clone(), None);
            assert_eq!(back.shell_integration_rev.as_deref(), Some(rev_b));
            let mut bad = meta;
            bad.shell_integration_rev = Some("not-an-address".into());
            assert_eq!(
                bad.into_checkpoint(carry.grid.clone(), None)
                    .shell_integration_rev,
                None
            );
        }

        let mut adopted = Terminal::new(4, 20);
        assert_eq!(adopted.shell_integration_rev(), None);
        adopted.restore_checkpoint(&carry);
        assert_eq!(adopted.shell_integration_rev(), Some(rev_b), "installed");
        // A carry that names none leaves the adopting engine's alone.
        let mut plain = carry.clone();
        plain.shell_integration_rev = None;
        adopted.restore_checkpoint(&plain);
        assert_eq!(adopted.shell_integration_rev(), Some(rev_b));
    }

    /// A RE-KEY the shell has not taken yet (2026-09-24): the key verifies marks
    /// at once, but the posture stays `Degraded` and the handoff carry leaves it
    /// behind until a mark signed with it arrives — a Claude tab whose shell
    /// reaches its next prompt hours later must not read `integration=on` in the
    /// meantime, nor carry a key it never saw into the next update. A mark with
    /// the wrong id does not count. CONTROL: a nonce authorized the ordinary way
    /// (spawn, or a carried one) is in use at once, exactly as before.
    #[test]
    fn a_rekey_is_in_use_only_from_the_first_mark_signed_with_it() {
        let key = [0x3Cu8; 32];
        let mut t = Terminal::new(4, 20);
        t.set_require_shell_integration_nonce(true);
        t.authorize_shell_integration_on_first_mark(key);
        assert_eq!(
            t.shell_integration_posture(),
            ShellIntegrationPosture::Degraded,
            "handed over, not yet taken"
        );
        assert_eq!(
            t.checkpoint_carry(0)
                .expect("Ground")
                .shell_integration_nonce,
            None,
            "an untaken key is not carried"
        );
        t.process(format!("\x1b]133;A;id={}\x07", "1".repeat(64)).as_bytes());
        t.process(b"\x1b]133;A\x07");
        assert_eq!(
            t.shell_integration_posture(),
            ShellIntegrationPosture::Degraded,
            "a wrong or missing id is not the shell taking the key"
        );
        t.process(format!("\x1b]133;A;id={}\x07", ShellIntegrationNonce(key).to_hex()).as_bytes());
        assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);
        assert_eq!(
            t.checkpoint_carry(0)
                .expect("Ground")
                .shell_integration_nonce,
            Some(ShellIntegrationNonce(key)),
            "in use now, so carried"
        );

        // Control: the spawn-time authorization is in use at once.
        let mut spawned = Terminal::new(4, 20);
        spawned.set_require_shell_integration_nonce(true);
        spawned.authorize_shell_integration(key);
        assert_eq!(
            spawned.shell_integration_posture(),
            ShellIntegrationPosture::On
        );
        // …and it replaces a pending one outright.
        let mut both = Terminal::new(4, 20);
        both.set_require_shell_integration_nonce(true);
        both.authorize_shell_integration_on_first_mark(key);
        both.authorize_shell_integration(key);
        assert_eq!(
            both.shell_integration_posture(),
            ShellIntegrationPosture::On
        );
    }

    /// A TYPED re-key (2026-09-26) — the heal of a shell spawned before the
    /// re-key channel, whose key rides a one-use file a typed line reads — is
    /// the channel's key plus a way back. Taken back (the line never ran), it
    /// authorizes nothing and the authorization it replaced stands again: a
    /// shell that had lost its nonce is back to no nonce at all, so a mark
    /// signed with the withdrawn key is dropped. Settled (the file was read),
    /// it stays pending until its first mark exactly as the channel's key does,
    /// and can no longer be taken back. And once a mark carried it, neither
    /// verb moves it: a key in use is never withdrawn.
    #[test]
    fn a_typed_rekey_is_taken_back_until_the_shell_reads_it() {
        let key = [0x4Du8; 32];
        let signed = |k: &[u8; 32]| {
            format!("\x1b]133;A;id={}\x07", ShellIntegrationNonce(*k).to_hex()).into_bytes()
        };
        let lost = || {
            let mut t = Terminal::new(4, 20);
            t.set_require_shell_integration_nonce(true);
            t
        };

        // Withdrawn: back to the lost nonce, and the key's marks are dropped.
        let mut t = lost();
        t.authorize_shell_integration_rekey(key);
        assert_eq!(
            t.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
        assert!(t.withdraw_shell_integration_rekey(&key));
        assert!(!t.withdraw_shell_integration_rekey(&key), "once");
        let dropped = t.shell_integration_dropped_count();
        t.process(&signed(&key));
        assert_eq!(t.shell_integration_dropped_count(), dropped + 1);
        assert_eq!(
            t.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );

        // Settled: pending until its first mark, then on; no longer withdrawable.
        let mut t = lost();
        t.authorize_shell_integration_rekey(key);
        assert!(
            !t.settle_shell_integration_rekey(&[0x11; 32]),
            "not that key"
        );
        assert!(t.settle_shell_integration_rekey(&key));
        assert!(!t.withdraw_shell_integration_rekey(&key), "settled");
        assert_eq!(
            t.shell_integration_posture(),
            ShellIntegrationPosture::Degraded,
            "the file was read, no mark yet"
        );
        t.process(&signed(&key));
        assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);

        // Used before anyone settled it: in use, never withdrawn.
        let mut t = lost();
        t.authorize_shell_integration_rekey(key);
        t.process(&signed(&key));
        assert!(!t.withdraw_shell_integration_rekey(&key));
        assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);

        // A second typed key over an unsettled first: withdrawing it restores
        // the authorization from before EITHER — here the channel key the shell
        // was already handed, still pending its first mark.
        let channel = [0x5Eu8; 32];
        let mut t = lost();
        t.authorize_shell_integration_on_first_mark(channel);
        t.authorize_shell_integration_rekey([0x6F; 32]);
        t.authorize_shell_integration_rekey(key);
        assert!(!t.withdraw_shell_integration_rekey(&[0x6F; 32]), "replaced");
        assert!(t.withdraw_shell_integration_rekey(&key));
        t.process(&signed(&channel));
        assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);

        // An ordinary authorization ends the way back.
        let mut t = lost();
        t.authorize_shell_integration_rekey(key);
        t.authorize_shell_integration(key);
        assert!(!t.withdraw_shell_integration_rekey(&key));
        assert_eq!(t.shell_integration_posture(), ShellIntegrationPosture::On);
    }

    #[test]
    fn shell_integration_nonce_hex_round_trips_and_refuses_malformed() {
        let n = ShellIntegrationNonce(core::array::from_fn(|i| i as u8));
        assert_eq!(ShellIntegrationNonce::from_hex(&n.to_hex()), Some(n));
        assert_eq!(
            ShellIntegrationNonce::from_hex(&n.to_hex().to_uppercase()),
            Some(n)
        );
        assert_eq!(ShellIntegrationNonce::from_hex(""), None);
        assert_eq!(ShellIntegrationNonce::from_hex(&"g".repeat(64)), None);
        assert_eq!(ShellIntegrationNonce::from_hex(&"0".repeat(66)), None);
    }

    /// THE TITLES A PROGRAM SET CROSS THE CARRY (the round-four plan, item
    /// 8b). A program titles its tab (OSC 2), names its icon (OSC 1) and pushes
    /// its caller's title to put it back on exit (CSI 22 t); an in-session
    /// update used to restore none of the three, so the tab fell back to its
    /// directory and the program's exit restored nothing. All three now ride
    /// the carry and its meta, and the adopting engine holds them as the live
    /// one did — the pop included, with the epoch a host relabels by.
    ///
    /// FAILS WITHOUT THE FIX: the adopted engine's title is empty (the
    /// checkpoint had no title to restore) and the pop finds an empty stack.
    #[test]
    fn carry_keeps_title_icon_and_stack() {
        let mut t = Terminal::new(4, 20);
        t.process(b"\x1b]1;the-icon\x07\x1b]2;the caller\x07");
        t.process(b"\x1b[22;0t\x1b]2;vim main.rs\x07");
        let expected = TitleRepr {
            window: "vim main.rs".to_string(),
            icon: "the-icon".to_string(),
            stack: vec![("the-icon".to_string(), "the caller".to_string())],
        };
        let carry = t.checkpoint_carry(0).expect("Ground");
        assert_eq!(carry.title.as_ref(), Some(&expected), "projected whole");
        assert_eq!(
            t.checkpoint_carry_abandoning_partial(0).0.title.as_ref(),
            Some(&expected),
            "the mid-sequence projection takes it too"
        );
        #[cfg(feature = "serde")]
        let carry = {
            let meta = CheckpointMeta::from_checkpoint(&carry);
            assert_eq!(meta.title.as_ref(), Some(&expected), "the meta names it");
            assert_eq!(
                t.carry_meta_abandoning_partial().title.as_ref(),
                Some(&expected),
                "so does the Repaint rung's scalar-only meta"
            );
            let back = meta.into_checkpoint(carry.grid.clone(), carry.alt_grid.clone());
            assert_eq!(back, carry, "the meta reassembles the carry exactly");
            back
        };

        let mut adopted = Terminal::new(4, 20);
        let epoch = adopted.title_epoch();
        adopted.restore_checkpoint(&carry);
        assert_eq!(adopted.title(), "vim main.rs", "the tab keeps its label");
        assert_eq!(adopted.icon_name(), "the-icon");
        assert!(
            adopted.title_epoch() > epoch,
            "a restored title is a change the host must see"
        );
        adopted.process(b"\x1b[23;0t");
        assert_eq!(
            adopted.title(),
            "the caller",
            "the program's exit puts its caller's title back, across the update"
        );

        // The in-memory projection round-trips it too (the ship gate's identity).
        let rebuilt = Terminal::from_checkpoint(&t.checkpoint());
        assert_eq!(rebuilt.checkpoint(), t.checkpoint());
        assert_eq!(rebuilt.title(), "vim main.rs");

        // A checkpoint that carried NO title (an older producer) leaves the
        // adopting engine's own alone — what the host may have seeded stays.
        let mut untitled = carry.clone();
        untitled.title = None;
        let mut seeded = Terminal::new(4, 20);
        seeded.restore_title(&TitleRepr {
            window: "from the record".to_string(),
            ..TitleRepr::default()
        });
        seeded.restore_checkpoint(&untitled);
        assert_eq!(seeded.title(), "from the record");
        // …while an UNTITLED carry is a title: nothing, and it says so.
        let mut blank = carry;
        blank.title = Some(TitleRepr::default());
        seeded.restore_checkpoint(&blank);
        assert_eq!(seeded.title(), "", "an untitled program stays untitled");
    }

    /// A CARRIED TITLE IS OSC INPUT, NOT TRUSTED TEXT. The carry is a file the
    /// previous process wrote, and a title reaches the window chrome, the tab
    /// strip and `CSI 21 t` reports; so the restore holds it to exactly what
    /// the OSC 0/1/2 handler would have stored — 1024 bytes at most, no C0
    /// (tab excepted), no C1, no bidi override — and the stack to the engine's
    /// depth, keeping the entries a live engine keeps (its bottom ones: a push
    /// onto a full stack is dropped).
    ///
    /// FAILS WITHOUT THE FIX: `restore_title` does not exist (compile-red).
    #[test]
    fn a_carried_title_is_sanitized_and_capped() {
        let hostile = format!(
            "{}\x1b]2;owned\x07\u{202E}moc.evil\u{9b}\r\n{}",
            "x".repeat(5),
            "y".repeat(10 * 1024)
        );
        let repr = TitleRepr {
            window: hostile.clone(),
            icon: hostile.clone(),
            stack: (0..15)
                .map(|n| (format!("icon {n}\x07"), format!("window {n}\x1b")))
                .collect(),
        };
        let mut t = Terminal::new(4, 20);
        t.restore_title(&repr);
        let clean = |s: &str| {
            s.len() <= 1024
                && !s.chars().any(|c| {
                    (c < ' ' && c != '\t')
                        || ('\u{80}'..='\u{9f}').contains(&c)
                        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
                })
        };
        assert!(clean(t.title()), "window title: {:?}", t.title());
        assert!(clean(t.icon_name()), "icon name: {:?}", t.icon_name());
        assert!(t.title().starts_with("xxxxx]2;owned"), "{:?}", t.title());
        let stack = t.capture_title_repr().stack;
        assert_eq!(
            stack.len(),
            super::super::TITLE_STACK_MAX_DEPTH,
            "the stack is bounded"
        );
        assert_eq!(
            stack.first(),
            Some(&("icon 0".to_string(), "window 0".to_string())),
            "the bottom entries are the ones kept, controls stripped"
        );
        assert!(
            stack
                .iter()
                .all(|(icon, window)| clean(icon) && clean(window))
        );
        // What was installed is already what a restore makes of it, so the
        // next handoff carries it unchanged.
        let again = t.capture_title_repr();
        let mut twin = Terminal::new(4, 20);
        twin.restore_title(&again);
        assert_eq!(twin.capture_title_repr(), again, "restoring is idempotent");
    }
}
