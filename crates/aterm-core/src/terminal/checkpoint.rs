// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `TerminalCheckpoint`: a scoped, round-trippable projection of a live [`Terminal`].
//!
//! The live [`Terminal`] is neither `Clone` nor `Serialize` (host callbacks, host-auth
//! gates, a policy engine, a live `Parser`), so a checkpoint is a projection with the
//! exclusion list below, not a clone. It captures both grid bodies (full cell fidelity
//! via the `Line` codec) with their absolute row numbering, per-grid cursor / scroll
//! region / pending wrap / tab stops, size, modes, style, charsets, keyboard state,
//! the colours, shell-integration state and titles a program set
//! (`checkpoint_state.rs`), and the engine state output still to come relies on
//! (`checkpoint_output.rs`). The round-trip property test is in `mod tests`.
//
// Excluded, and why:
//   - host effects: the callbacks, the policy engine and the live auth gates (the
//     shell-integration nonce travels only on the seamless carry, and the adopting
//     host authorizes it). A restore never replays a host effect. `from_checkpoint`
//     gives `Terminal::new`'s defaults; a host installs its own through the setters,
//     or adopts into a configured terminal with `restore_checkpoint`.
//   - the rest of `transient`: the response buffer and rate limiter, one batch's own
//     scratch, what the host polls or observes (bell, OSC event queue, print anchor,
//     timings, counters, render debt), the orphaned-screen leave's notes (a restored
//     session takes the 1049 exit), and the Kitty image store with any chunked
//     transmission in progress. Its in-flight output state is carried
//     (`output_state`); a successor that predates that key resumes without it, and
//     the seamless capture re-parks rather than hand it an open link or a pending
//     VT52 address over queued output (`Terminal::output_state_in_flight`). A
//     synchronized-update window stays open (`modes` crosses whole), its timer
//     re-armed at the restore.
//   - `text_selection`, the observation watchers and `last_custody` (`state.rs`):
//     the person's interaction with this process, not the screen.
//   - `marks_state` (OSC 1337 SetMark/AddAnnotation), `semantic` zones and `iterm2`
//     image placements: extras no consumer needs across an update (OSC 133/633
//     blocks are carried).
//   - sixel and other decoded images: megabytes per screen; the cells they covered
//     carry.
//   - the bell and response-rate clocks: live supervision state that reads real time
//     on purpose (`state.rs`, `processing.rs`).
//   - the title epoch: a host's change signal; a restore that changes the window
//     title bumps this terminal's own epoch.
//
// The seamless-handoff wire form is `CheckpointMeta` (serde, every scalar field) plus
// the grid blobs as sidecars.

use aterm_parser::ParserCarry;
use aterm_types::charset::CharacterSetState;
use aterm_types::{KittyKeyboardStateSnapshot, TaskbarProgress, XtermKeyboardState};

use super::Terminal;
use super::checkpoint_holder::HolderRepr;
use super::checkpoint_output::OutputStateRepr;
use super::checkpoint_state::{ColorRepr, ShellRepr, TitleRepr};
use super::types::{CurrentStyle, TerminalModes};
use crate::grid::{CellFlags, Cursor, Grid, PackedColor, SavedCursorState};
use crate::scrollback::{Scrollback, deserialize_lines, serialize_lines};

/// Per-grid cursor + region + wrap + tab-stop projection, captured for the main grid
/// and (when present) the alt grid.
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
    /// DECSLRM horizontal-margin left (inclusive). Must round-trip, or a
    /// checkpoint taken under DECLRMM (mode 69, captured in `modes`) restores
    /// full-width margins and margin-aware wrap/clamp/ICH/DCH/scroll diverge.
    pub margin_left: u16,
    /// DECSLRM horizontal-margin right (inclusive).
    pub margin_right: u16,
    /// Per-column tab stops (`true` = stop set at that column).
    pub tab_stops: Vec<bool>,
    /// Whether TBC 3 suppressed the every-8 defaults: without it a restored grid told
    /// to clear all tab stops re-seeds the default into the columns the next widen
    /// adds. `serde(default)`: an older checkpoint restores `false`, as it meant.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tab_defaults_suppressed: bool,
    /// What the grid's rows-only resizes owe back at one edge
    /// (`Grid::handback_rows`): positive, the blank rows trims took that a grow
    /// appends before it reveals history afresh; negative, the history lines grows
    /// revealed that a shrink demotes before it trims afresh. Carried, and the
    /// restored grid's grow reveals from the tier its history is restored into, so
    /// a rows flap whose halves straddle an update handoff or a restore leaves the
    /// rows where the same flap with none between does. `serde(default)`: an older
    /// checkpoint owes nothing; written only when something is owed.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "is_zero_rows")
    )]
    pub handback_rows: i32,
    /// The natural moves made after what `handback_rows` owes
    /// (`Grid::handback_above`): rows demoted after the trims, or blank rows
    /// appended after the reveals, which go back first. `serde(default)`, written
    /// only when nonzero.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "is_zero_above")
    )]
    pub handback_above: u16,
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
            handback_rows: grid.handback_rows(),
            handback_above: grid.handback_above(),
        }
    }

    fn apply(&self, grid: &mut Grid) {
        grid.set_scroll_region(self.scroll_top, self.scroll_bottom);
        // set_horizontal_margins self-validates, so this is safe even if cols changed.
        grid.set_horizontal_margins(self.margin_left, self.margin_right);
        grid.set_cursor(self.cursor_row, self.cursor_col);
        grid.set_pending_wrap(self.pending_wrap);
        grid.restore_tab_stops(&self.tab_stops, self.tab_defaults_suppressed);
        // Last: owed from the restored grid's own counter on.
        grid.seed_handback(self.handback_rows, self.handback_above);
    }
}

/// Minimal, by-value style projection: the four inputs `(fg, bg, flags, protected)`
/// that `CurrentStyle`'s cached fields are pure functions of. A restore rebuilds the
/// cache with `CurrentStyle::new`, then `apply_style_change()` re-arms the rebuilt
/// grid's BCE cursor template.
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

/// A DECSC slot's deferred-wrap flag (a bare `bool` on the wire).
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

/// Wire-stable DECSC/DECRC slot: the style as raw semantic fields, so a restore
/// rebuilds `CurrentStyle`'s caches rather than carrying a grid-local index.
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
    /// Project a DECSC slot for the wire, raw, exactly as the engine keeps it.
    ///
    /// The slot holds absolute coordinates that nothing reads until DECRC or a 1049
    /// exit, which clamp to the grid then (`Grid::set_cursor`, as xterm's
    /// `CursorRestore` does). A resize moves the slot only with its text, so a slot on
    /// a row a shrink trimmed names a row the grid no longer has, and lands there if
    /// the grid grows back first: the projection must not clamp. The wire bounds the
    /// slot by the protocol ceiling, not the current grid
    /// (`seamless::checkpoint_meta_bound_violation`).
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
/// (`ATERM_SHELL_NONCE`, injected once at its first spawn). Carried only by the
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
    /// A nonce is required and none is authorized, so every mark is dropped —
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

/// A scoped, round-trippable projection of a live [`Terminal`].
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
    /// How many of `grid`'s leading line records are scrollback rather than
    /// visible rows. `grid` therefore holds `history_lines + rows` records.
    ///
    /// Carried explicitly so the consumer can bound its allocation from the
    /// authenticated meta before decoding any bytes. `0` is the visible-only
    /// projection and the default for a checkpoint without the field.
    ///
    /// The alternate grid always carries `0`: the live alt screen keeps no
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
    /// The parser this checkpoint restores into is in Ground: `parser` is `None`, and
    /// either the live parser was Ground with nothing pending, or the capture
    /// ([`Terminal::checkpoint_carry_abandoning_partial`]) cancelled, as CAN would, a
    /// partial sequence no carry can hold (a hooked DCS query). `false` with `parser`
    /// set: the restore continues that partial state. `false` with `parser` `None`: an
    /// in-memory checkpoint of a parser inside a hooked DCS query.
    pub parser_ground: bool,
    /// The parser's partial state ([`ParserCarry`]): a sequence split across
    /// PTY reads (a CSI, an OSC title, the head of a UTF-8 glyph), which the
    /// restored parser continues so the rest of it, still queued on the PTY,
    /// is read as that sequence. `None` for a Ground parser with nothing
    /// pending. A restore validates it
    /// ([`aterm_parser::Parser::restore_carry`]) and on refusal resumes at
    /// Ground.
    pub parser: Option<ParserCarry>,
    /// The authorized shell-integration nonce — set only by the
    /// seamless-handoff projections ([`Terminal::checkpoint_carry`] and
    /// [`Terminal::checkpoint_carry_abandoning_partial`]), and
    /// `None` in every in-memory checkpoint. Never installed by a restore: the
    /// adopting host authorizes it explicitly (auth is a host binding, see the
    /// excluded block).
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
    /// The integration body the shell last signed that it runs
    /// ([`Terminal::shell_integration_rev`]). Set, like the nonce, only by the
    /// seamless-handoff carry projections, so the successor's `status
    /// integration_rev=` can name an adopted shell's body before its next
    /// prompt re-signs it. A fact about the shell, not an authority: a restore
    /// installs it (a checkpoint without one leaves the engine's alone).
    pub shell_integration_rev: Option<String>,
    /// The titles a program set ([`TitleRepr`]). `Some` in every checkpoint a terminal
    /// projects, an untitled one included; `None` only when a handoff's producer
    /// carried no title (an older build, or a value that did not read), and then a
    /// restore leaves the engine's title alone for the adopting host to seed. Presence
    /// is the whole signal, so it is never folded into an empty title.
    pub title: Option<TitleRepr>,
    /// The engine state output still to come relies on ([`OutputStateRepr`]); a
    /// restore installs it whole.
    pub output_state: OutputStateRepr,
    /// The holder fence and the main grid's span anchors ([`HolderRepr`]),
    /// keyed like `shell`'s rows. Empty when no drawer ran; a restore
    /// installs it whole or not at all.
    pub holder: HolderRepr,
}

impl TerminalCheckpoint {
    /// Whether the parser carry holds part of an OSC 133/633 shell-integration mark
    /// past its command number, the payload a signed mark carries the session's nonce
    /// in (`Terminal::partial_sequence_is_shell_mark` at the capture). The
    /// crash-restore journal refuses to write or read such a keyframe. This is a
    /// keyframe-projection rule; raw journal tail bytes are not redacted.
    #[must_use]
    pub fn parser_holds_shell_mark(&self) -> bool {
        self.parser.as_ref().is_some_and(|carry| {
            carry.state == aterm_parser::State::OscString && osc_payload_is_shell_mark(&carry.osc)
        })
    }

    /// This checkpoint less its lineage's seed uncertainty
    /// ([`HolderRepr::seed_untrusted`]): what a restored engine shares with the
    /// live one it continues. A restore that took output after its install
    /// carries that bit by design (its old command start must not seed a range)
    /// and a live engine never does, so a test comparing the two compares
    /// everything else.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn without_seed_lineage(mut self) -> Self {
        self.holder.seed_untrusted = false;
        self
    }
}

/// Whether `osc`, an OSC payload collected so far, is an OSC 133/633 mark past its
/// command number: the command parsed as the OSC dispatcher parses it (the first
/// `;`-separated field as a `u32`, `handler_osc.rs`), and a `;` after it, past which a
/// signed mark's parameters (its nonce among them) begin. A payload still inside its
/// number holds no parameter yet.
fn osc_payload_is_shell_mark(osc: &[u8]) -> bool {
    let Some(command) = osc
        .iter()
        .position(|b| *b == b';')
        .and_then(|end| osc.get(..end))
    else {
        return false;
    };
    std::str::from_utf8(command)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|n| n == 133 || n == 633)
}

/// What a hooked DCS string's handler holds at a checkpoint
/// (`Terminal::hooked_dcs`), which decides whether a carry may swallow the
/// rest of the string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookedDcs {
    /// Nothing: the string is consumed and ignored, so swallowing its tail is
    /// exact. The carried state reads every byte still to come as the hooked
    /// string does, UTF-8 and an 8-bit ST included
    /// ([`aterm_parser::HOOKED_STRING_CARRY`]), and the handler's unhook does
    /// nothing a restored engine misses (it releases the bytes it counted and
    /// marks an open synchronized-update window dirty, which repaints nothing
    /// that changed).
    Nothing,
    /// A picture placed at the string's end, which a graphics stream's next
    /// frame replaces: swallowing it loses that one picture.
    Picture,
    /// A query whose end writes a reply the program waits for: nothing
    /// replaces it, so no carry holds it.
    Query,
}

/// Which completed shell marks and blocks a projection carries (the in-progress
/// mark and block always travel: a running command must still complete).
#[derive(Clone, Copy, Debug)]
enum MarksFrom {
    /// Those from the first that names a line the projected primary history
    /// holds: a mark wholly older than the carry names only lines it does not
    /// hold (every carry, and [`Terminal::checkpoint_journal`]).
    Carried,
    /// Those from the first that names a row at most this many rows above the
    /// primary's top visible row, or inside the carried history when that
    /// reaches further: the journal's keyframe, whose history ring brings the
    /// older lines back ([`Terminal::checkpoint_journal_marked`]).
    Within(usize),
    /// Every one the engine holds: the journal's recovery, whose scratch engine
    /// holds only what a keyframe carried
    /// ([`Terminal::checkpoint_continuing_swallowing`]).
    All,
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
    /// The parser must be in Ground state: a checkpoint taken
    /// mid-sequence would silently lose the parser's partial state (which is not
    /// in the projection). This is `debug_assert`ed.
    #[must_use]
    pub fn checkpoint(&self) -> TerminalCheckpoint {
        self.checkpoint_with_scrollback(true)
    }

    /// [`Self::checkpoint`] for an engine that may be parked inside a partial
    /// sequence: the same full projection without the Ground precondition,
    /// carrying the partial state in [`TerminalCheckpoint::parser`] so a
    /// restored terminal continues the sequence with the next byte. `None`
    /// only for a partial sequence no carry can hold
    /// ([`Self::partial_sequence_uncarried`], a hooked DCS string).
    ///
    /// Needed because an adopted engine can be restored mid-sequence, and the
    /// recorder's first keyframe of it is taken before its reader runs. Like
    /// `checkpoint`, it carries no shell-integration nonce.
    #[must_use]
    pub fn checkpoint_continuing(&self) -> Option<TerminalCheckpoint> {
        self.checkpoint_continuing_bounded(usize::MAX)
    }

    /// [`Self::checkpoint_continuing`] with each grid's history bounded to its
    /// most recent `max_history` lines, at a capture cost of
    /// `O((rows + max_history) × cols)`: the primary's (the saved one while the
    /// alternate screen is up) and the inactive grid's alike, as an engine whose
    /// scrollback limit is `max_history` projects in full.
    ///
    /// For a keyframe of a live engine taken under its lock: the temporal
    /// recorder's, after its reader dropped input, which must not cost work
    /// proportional to the whole history and may find the parser inside a
    /// sequence.
    #[must_use]
    pub fn checkpoint_continuing_bounded(&self, max_history: usize) -> Option<TerminalCheckpoint> {
        self.partial_sequence_uncarried()
            .is_none()
            .then(|| self.project_bounded(max_history, max_history, self.parser_is_ground()))
    }

    /// [`Self::checkpoint_continuing`] that never refuses: a hooked DCS query
    /// ([`Self::partial_sequence_uncarried`]) rides as a string to SWALLOW to its
    /// terminator, as a hooked string whose handler keeps nothing does
    /// ([`aterm_parser::Parser::carry_unhooking`]). The query's reply, which lives in
    /// its handler, is lost; a restored terminal swallows the rest of the string rather
    /// than print it. Equal to `checkpoint_continuing()` whenever that is `Some`.
    ///
    /// For the crash-restore journal's recovery (`aterm-restore`), whose replayed
    /// scratch engine can end inside such a query: the screen the replay reached is the
    /// one to restore, because falling back to the keyframe would undo everything the
    /// replay did, an ED3 or a RIS included. The caller counts the loss
    /// ([`Self::partial_sequence_uncarried`] and [`Self::partial_sequence_swallowed`]
    /// name it); it is never hidden. Carries no nonce, like `checkpoint_continuing`.
    ///
    /// It keeps EVERY completed shell mark and block the engine holds, those whose
    /// prompt is older than its history included, where `checkpoint_continuing` leaves
    /// those behind; that is its one other difference. The scratch engine holds only
    /// what the keyframe carried ([`Self::checkpoint_journal_marked`]), whose marks reach
    /// past its bridge because the history ring brings their lines back below the
    /// restored screen, at the same absolute rows. Filtered again here, by the scratch
    /// engine's own short history, they would be dropped after all.
    #[must_use]
    pub fn checkpoint_continuing_swallowing(&self) -> TerminalCheckpoint {
        let mut c = self.project(
            usize::MAX,
            usize::MAX,
            self.parser_is_ground(),
            true,
            MarksFrom::All,
        );
        if self.hooked_dcs() == Some(HookedDcs::Query) {
            c.parser = Some(self.parser.carry_unhooking());
            c.parser_ground = false;
        }
        c
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
    /// `max_history` lines of the most recent scrollback, at a capture cost of
    /// `O((rows + max_history) × cols)`. `max_history == 0` is
    /// [`Self::checkpoint_visible`].
    ///
    /// Restore needs no counterpart: `restore_grid` reads the last `rows`
    /// lines as the visible grid and pushes everything before them into an
    /// unlimited scrollback.
    ///
    /// `None` only when the parser holds a partial sequence no carry can hold
    /// ([`Self::partial_sequence_uncarried`]); any other partial state rides
    /// in [`TerminalCheckpoint::parser`].
    ///
    /// This and [`Self::checkpoint_carry_abandoning_partial`] are the only
    /// projections that carry the authorized shell-integration nonce
    /// ([`TerminalCheckpoint::shell_integration_nonce`]), so the adopted shell
    /// keeps signing its marks across the update.
    #[must_use]
    pub fn checkpoint_carry(&self, max_history: usize) -> Option<TerminalCheckpoint> {
        self.partial_sequence_uncarried().is_none().then(|| {
            let mut c = self.project_bounded(max_history, 0, true);
            c.shell_integration_nonce = self.carried_shell_integration_nonce();
            c.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
            c
        })
    }

    /// The crash-restore journal's keyframe projection (`aterm-restore`): the
    /// visible screen and both grids, plus a BRIDGE of at most `bridge` lines of
    /// the most recent history, at a capture cost of
    /// `O((rows + bridge) × cols)` under the terminal lock.
    ///
    /// The bridge rides on the grid that holds this terminal's history: the
    /// active grid on the main screen (`grid`, counted in `history_lines`), the
    /// saved primary while the alternate screen is up (`alt_grid`, whose blob
    /// then holds up to `rows + bridge` records — the journal's own decoder
    /// bounds it there, not at the handoff wire's `rows`). The other grid
    /// carries its visible rows only.
    ///
    /// Unlike the seamless carries it keeps the live parser's partial state
    /// ([`TerminalCheckpoint::parser`]; `parser_ground` records the live
    /// parser, as [`Self::checkpoint_continuing`] does) and the output state,
    /// because the journal replays the raw bytes that follow it: a sequence
    /// split across the keyframe continues through the carried parser state.
    /// It omits the dedicated nonce and integration-revision fields (a restored shell
    /// re-keys), and it is never cut inside an OSC 133/633 mark, whose raw payload the
    /// parser carry would hold with the nonce in it (`id=<hex>`;
    /// [`Self::partial_sequence_is_shell_mark`]). Raw tail replay bytes are outside this
    /// projection and are not redacted.
    ///
    /// `None` when [`Self::partial_sequence_uncarried`] or
    /// [`Self::partial_sequence_swallowed`] is `Some` (a hooked query, an
    /// over-cap OSC, or an active picture whose handler state cannot be
    /// carried), or when [`Self::partial_sequence_is_shell_mark`]. The journal
    /// defers its keyframe (K5) rather than silently drop that state or write
    /// the mark payload into its keyframe.
    #[must_use]
    pub fn checkpoint_journal(&self, bridge: usize) -> Option<TerminalCheckpoint> {
        (self.partial_sequence_uncarried().is_none()
            && self.partial_sequence_swallowed().is_none()
            && !self.partial_sequence_is_shell_mark())
        .then(|| {
            // `project_bounded`'s first bound is the active grid's, its second the
            // inactive one's; the history is the saved primary's while the
            // alternate screen is up (`from_checkpoint` restores it there).
            let (active, inactive) = if self.modes.alternate_screen {
                (0, bridge)
            } else {
                (bridge, 0)
            };
            self.project_bounded(active, inactive, self.parser_is_ground())
        })
    }

    /// [`Self::checkpoint_journal`] whose completed shell marks and blocks reach
    /// `marks` history lines above the screen, whether or not the bridge carries
    /// those lines: every one from the first that names a row at most `marks`
    /// rows above the history grid's top visible row (or inside the bridge, when
    /// that reaches further). Its grids are exactly `checkpoint_journal(bridge)`'s.
    ///
    /// The journal's bridge exists to meet its history ring, which carries every
    /// history line older than the bridge as text alone, and a ring that has caught
    /// up leaves the bridge empty. A mark's row is absolute, and the restore puts
    /// the ring's lines back at their own absolute rows below the restored screen,
    /// so a mark carried past the bridge names the line it named live; filtered by
    /// the bridge, a keyframe of a caught-up session would carry no mark or block
    /// in the scrollback at all, and prompt navigation and the block history would
    /// stop at the screen's top. A mark whose line does not come back names a row
    /// the restored engine never holds, as a live engine's mark does once its line
    /// is evicted (marks outlive eviction; only a clear or a reset drops them).
    ///
    /// `marks` bounds the cost instead of the history: the engine keeps up to a
    /// thousand marks and a thousand blocks, each with a working directory and a
    /// command line of up to 64 KiB, and the keyframe is cloned under the terminal
    /// lock and written to a fixed-size slot several times a second, so the journal
    /// passes the line bound its bridge had before the ring (512 lines, 256 under
    /// tail pressure), which an update's carry (256 lines) does not exceed.
    #[must_use]
    pub fn checkpoint_journal_marked(
        &self,
        bridge: usize,
        marks: usize,
    ) -> Option<TerminalCheckpoint> {
        (self.partial_sequence_uncarried().is_none()
            && self.partial_sequence_swallowed().is_none()
            && !self.partial_sequence_is_shell_mark())
        .then(|| {
            let (active, inactive) = if self.modes.alternate_screen {
                (0, bridge)
            } else {
                (bridge, 0)
            };
            self.project(
                active,
                inactive,
                self.parser_is_ground(),
                true,
                MarksFrom::Within(marks),
            )
        })
    }

    /// [`Self::checkpoint_carry`] for a parser that may hold a partial sequence no
    /// carry can: the same projection (visible screen plus at most `max_history`
    /// lines, the inactive grid pinned to its visible rows) without that
    /// precondition, plus the name of the parser state whose partial sequence it
    /// leaves out (`None` when nothing was left out: then it equals
    /// `checkpoint_carry(max_history)`).
    ///
    /// Almost every partial sequence is carried in [`TerminalCheckpoint::parser`] (a
    /// split CSI or DCS header, an OSC title, a UTF-8 head; an ignored DCS, SOS/PM/APC
    /// or oversize OSC, and a hooked DCS string whose handler keeps nothing or holds a
    /// picture, ride as a string to swallow). Left out is a hooked DCS query
    /// (`DcsPassthrough` under DECRQSS or XTGETTCAP), whose reply lives in its
    /// handler; a parser can sit there indefinitely (a stalled ssh session, an
    /// unterminated string), and the update must not refuse for it.
    ///
    /// The semantics are CAN (0x18) applied to the copy: `parser_ground` is `true` and
    /// a restored terminal treats the next byte as fresh input. The live terminal is
    /// only read, so a handoff that rolls back resumes byte-exactly. The rest of the
    /// string reaches the restored Ground parser as ordinary input: a few late bytes
    /// for a stalled string, but visible garbage for a tail already queued on the PTY,
    /// so the seamless capture asks [`Self::partial_sequence_uncarried`] first and
    /// re-parks on a sequence boundary instead.
    #[must_use]
    pub fn checkpoint_carry_abandoning_partial(
        &self,
        max_history: usize,
    ) -> (TerminalCheckpoint, Option<&'static str>) {
        // Fault point for this projection failing (const `false` outside
        // tests): a seamless capture of a live session's grids takes it, and
        // a test proves the `carry = "repaint"` rung never reaches it.
        assert!(
            !crate::fault::triggered("checkpoint.carry_projection"),
            "fault injected: the carry projection failed"
        );
        let mut carry = self.project_bounded(max_history, 0, true);
        carry.shell_integration_nonce = self.carried_shell_integration_nonce();
        carry.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
        (carry, self.partial_sequence_uncarried())
    }

    /// The scalar half of [`Self::checkpoint_carry_abandoning_partial`]`(0)` (exactly
    /// that carry's [`CheckpointMeta`]) without projecting either grid.
    ///
    /// For the seamless capture's Repaint rung (`aterm-gui`'s
    /// `seamless::repaint_carry_for_wire`), which carries a blank canonical screen and
    /// reads only the grids' geometry and cursors: the frozen window neither
    /// serializes grids it would throw away nor runs the grid code a successor's
    /// `carry = "repaint"` policy routes around.
    #[cfg(feature = "serde")]
    #[must_use]
    pub fn carry_meta_abandoning_partial(&self) -> CheckpointMeta {
        let mut scalars = self.project(0, 0, true, false, MarksFrom::Carried);
        scalars.shell_integration_nonce = self.carried_shell_integration_nonce();
        scalars.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
        CheckpointMeta::from_checkpoint(&scalars)
    }

    /// The authorized shell-integration nonce as the two seamless-handoff projections
    /// carry it ([`Self::checkpoint_carry`],
    /// [`Self::checkpoint_carry_abandoning_partial`]). Only a nonce in use: a re-key
    /// the shell has not taken yet stays behind, and the successor re-keys afresh.
    fn carried_shell_integration_nonce(&self) -> Option<ShellIntegrationNonce> {
        self.shell_integration_auth
            .nonce_in_use()
            .map(ShellIntegrationNonce)
    }

    /// The parser state a partial escape sequence is sitting in (`CsiParam`,
    /// `OscString`, `SosPmApcString`, …; `TmuxPassthrough` for a tmux
    /// passthrough wrapper open at a payload's Ground), or `None` at Ground. Whether that
    /// sequence is carried or abandoned is
    /// [`Self::partial_sequence_uncarried`]'s answer. A pure read of the
    /// parser; nothing is projected.
    #[must_use]
    pub fn partial_sequence_state(&self) -> Option<&'static str> {
        let state = self.parser.state();
        if state.is_ground() && self.parser.tmux_passthrough().is_open() {
            return Some("TmuxPassthrough");
        }
        (!state.is_ground()).then(|| state.name())
    }

    /// The parser state a partial sequence no carry can hold is sitting in
    /// (`DcsPassthrough` under a DCS query, DECRQSS or XTGETTCAP, whose reply lives in
    /// its handler; [`Self::parser_carry`]), or `None` when the parser's state is
    /// carried, whole or as a string to swallow ([`TerminalCheckpoint::parser`]). The
    /// seamless capture asks this before treating a mid-sequence parser over queued
    /// output as a timing miss: a carried sequence continues in the successor. A pure
    /// read.
    #[must_use]
    pub fn partial_sequence_uncarried(&self) -> Option<&'static str> {
        self.parser_carry()
            .is_none()
            .then(|| self.parser.state().name())
    }

    /// What a hooked DCS string's handler holds, or `None` when the parser is not
    /// inside one: in its payload, or at the ESC that may begin its ST, whose next
    /// byte decides whether it ends complete or broken off
    /// ([`aterm_parser::Parser::in_hooked_dcs`]). The parser cannot see it; this
    /// engine's DCS router can.
    fn hooked_dcs(&self) -> Option<HookedDcs> {
        if !self.parser.in_hooked_dcs() {
            return None;
        }
        Some(match self.dcs.dcs_type {
            // Consumed and ignored to the terminator (tmux control mode,
            // DECDLD, an unknown string): the handler keeps nothing and its
            // unhook does nothing.
            super::DcsType::None | super::DcsType::Unknown => HookedDcs::Nothing,
            // A picture placed at the unhook, which a stream's next frame
            // replaces.
            super::DcsType::Sixel => HookedDcs::Picture,
            // A query whose unhook writes a reply the program waits for.
            super::DcsType::Decrqss | super::DcsType::Xtgettcap => HookedDcs::Query,
        })
    }

    /// The parser's partial state as a carry holds it: `None` only for a hooked DCS
    /// whose handler a carry would rob of something nothing replaces (a query's
    /// reply). A hooked string whose handler keeps nothing, or holds a picture, rides
    /// as one to ignore to its terminator ([`aterm_parser::Parser::carry_unhooking`]),
    /// so the successor swallows its tail instead of printing it, and a tab streaming
    /// sixel frames or tmux control-mode lines does not miss every park.
    fn parser_carry(&self) -> Option<ParserCarry> {
        match self.hooked_dcs() {
            None => self.parser.carry(),
            Some(HookedDcs::Nothing | HookedDcs::Picture) => Some(self.parser.carry_unhooking()),
            Some(HookedDcs::Query) => None,
        }
    }

    /// The parser state a partial sequence is in when the carry would hold it only as
    /// one to swallow, where this engine would still have delivered it (an OSC over
    /// the carry's cap such as OSC 52 or 1337, an APC string whose consumer started,
    /// a hooked sixel string), or `None`
    /// ([`aterm_parser::Parser::carry_swallows_sequence`]; a hooked string whose
    /// handler keeps nothing loses nothing, so it is not named). Over queued output
    /// the seamless capture treats this as a timing miss and re-parks on a sequence
    /// boundary; over a quiet PTY the carry still goes, as a stalled sequence may
    /// never end. A pure read.
    #[must_use]
    pub fn partial_sequence_swallowed(&self) -> Option<&'static str> {
        (self.parser.carry_swallows_sequence() || self.hooked_dcs() == Some(HookedDcs::Picture))
            .then(|| self.parser.state().name())
    }

    /// Whether the parser sits inside an OSC 133/633 shell-integration mark past its
    /// command number: a carry would hold the mark's raw payload so far, where a signed
    /// mark carries the session's nonce (`id=<hex>`, from its third parameter on). The
    /// crash-restore journal never cuts a keyframe there
    /// ([`Self::checkpoint_journal`]); the seamless carries do, as they carry the nonce
    /// itself. A pure read.
    #[must_use]
    pub fn partial_sequence_is_shell_mark(&self) -> bool {
        self.parser.state() == aterm_parser::State::OscString
            && self
                .parser_carry()
                .is_some_and(|carry| osc_payload_is_shell_mark(&carry.osc))
    }

    /// Whether the sequence [`Self::partial_sequence_swallowed`] names is a picture
    /// (a kitty-graphics APC, an OSC 1337 image, a hooked sixel string), which a
    /// graphics stream's next frame replaces
    /// ([`aterm_parser::Parser::carry_swallows_picture`]). The seamless capture
    /// carries one rather than miss every park while the stream plays; an OSC 52
    /// clipboard write and every other swallowed sequence stay a miss. A pure read.
    #[must_use]
    pub fn partial_sequence_swallows_picture(&self) -> bool {
        self.parser.carry_swallows_picture() || self.hooked_dcs() == Some(HookedDcs::Picture)
    }

    /// The engine state (not the parser's) that output still to come relies on and
    /// that is in flight, or `None`: `"Osc8HyperlinkOpen"` while an OSC 8 link is
    /// open, `"Vt52CursorAddress"` while a VT52 `ESC Y` waits for its address bytes.
    ///
    /// Every checkpoint carries both ([`TerminalCheckpoint::output_state`]), but a
    /// successor that predates that carry resumes with neither (the rest of the link
    /// unlinked, the address bytes printed). So on a hop to an older build the
    /// seamless capture asks this beside [`Self::partial_sequence_swallowed`] and
    /// re-parks over queued output; over a quiet PTY it carries anyway (a link may
    /// never close). The rest of the output state is not named: an older successor
    /// never had it. A pure read.
    #[must_use]
    pub fn output_state_in_flight(&self) -> Option<&'static str> {
        if self.transient.vt52_cursor_state != super::Vt52CursorState::None {
            Some("Vt52CursorAddress")
        } else if self.transient.current_hyperlink.is_some() {
            Some("Osc8HyperlinkOpen")
        } else {
            None
        }
    }

    /// A carried synchronized-update window (mode 2026) gets a fresh timer: `modes`
    /// crosses whole but the window's start instant is `transient` and does not, and
    /// the engine's timeout fires only from it, so without re-arming a program that
    /// died before its `?2026l` would leave the mode on for good. Armed as `?2026h`
    /// arms it, at this engine's processing clock.
    fn arm_carried_sync_window(&mut self) {
        self.transient.sync_start = self
            .modes
            .synchronized_output
            .then_some(self.transient.process_now);
    }

    /// The parser's partial state as a checkpoint carries it: `None` for a
    /// Ground parser with nothing pending (a fresh parser already is that) and
    /// for a hooked DCS string no carry holds ([`Self::parser_carry`]).
    fn carried_parser(&self) -> Option<ParserCarry> {
        self.parser_carry().filter(|carry| !carry.is_ground())
    }

    /// Install a checkpoint's parser carry. `None` leaves the parser as it is.
    /// A carry the parser refuses (a hostile or skewed producer) costs the
    /// partial sequence and nothing else: the parser resumes at Ground.
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

    /// Whether the engine holds an inactive grid, the one a checkpoint carries
    /// as `alt_grid` (the saved primary while the alternate screen is up, or
    /// the alternate screen's grid kept after it was left).
    ///
    /// The seamless capture prices a carry's cell cost with this (the inactive
    /// grid is a second visible grid) exactly as the consumer and
    /// `screen_digest` will, before paying for the projection. A pure read.
    #[must_use]
    pub fn has_inactive_grid(&self) -> bool {
        self.alt_grid.is_some()
    }

    fn checkpoint_with_scrollback(&self, include_scrollback: bool) -> TerminalCheckpoint {
        // `bounded(MAX)` is the full history and `bounded(0)` the visible
        // screen. A full checkpoint is in-memory only (never on the handoff
        // wire), so both grids may carry the same depth.
        let bound = if include_scrollback { usize::MAX } else { 0 };
        self.checkpoint_bounded(bound, bound)
    }

    /// `inactive_max_history` bounds the inactive grid (`alt_grid`) separately
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
    /// what the checkpoint records about a parser whose state it does not
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
        self.project(
            max_history,
            inactive_max_history,
            parser_ground,
            true,
            MarksFrom::Carried,
        )
    }

    /// [`Self::project_bounded`], or — `grids == false` — its scalars alone: both
    /// grid blobs left empty (the inactive one present exactly when the engine has
    /// an inactive grid) and `history_lines` 0, as a visible-only projection
    /// records it. Such a checkpoint is never a carry; it exists only to be read
    /// by [`CheckpointMeta::from_checkpoint`], which ignores the blobs
    /// ([`Self::carry_meta_abandoning_partial`]).
    ///
    /// `marks` says which completed shell marks and blocks travel ([`MarksFrom`]).
    fn project(
        &self,
        max_history: usize,
        inactive_max_history: usize,
        parser_ground: bool,
        grids: bool,
        marks: MarksFrom,
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

        // The oldest line of the primary lineage this projection carries, which
        // is where readable shell marks end. The primary is `alt_grid` while the
        // alternate screen is up (see `from_checkpoint`), carried to its own bound.
        let (primary, primary_bound) = if self.modes.alternate_screen {
            (self.alt_grid.as_ref(), inactive_max_history)
        } else {
            (Some(&self.grid), max_history)
        };
        // The marks' floor is that line, reached further back for the journal's
        // keyframe and dropped for its recovery ([`MarksFrom`]).
        let oldest_carried_row = primary.map_or(0, |g| {
            let carried = g.scrollback_lines().min(primary_bound);
            let top = g.visible_to_absolute(0);
            match marks {
                MarksFrom::Carried => top.saturating_sub(carried as u64),
                MarksFrom::Within(lines) => top.saturating_sub(carried.max(lines) as u64),
                MarksFrom::All => 0,
            }
        });

        let (alt_grid, alt_cursor) = match &self.alt_grid {
            Some(inactive) => (
                // While the alternate screen is up the inactive grid is the saved
                // primary, scrollback and all. The wire gives this blob exactly
                // `rows` records (the consumer validates it with `history = 0`),
                // so the handoff pins `inactive_max_history` to 0; a full
                // in-memory checkpoint uses the active grid's bound.
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
            output_state: self.capture_output_state_repr(),
            holder: self.capture_holder_repr(oldest_carried_row),
        }
    }

    /// Rebuild a fully-living [`Terminal`] from a checkpoint. Its buffer state matches
    /// the source exactly (the round-trip test); its host effects are
    /// `Terminal::new`'s (see the excluded block), so a host installs its own through
    /// the setters, or adopts into a configured terminal with
    /// [`Self::restore_checkpoint`].
    #[must_use]
    pub fn from_checkpoint(c: &TerminalCheckpoint) -> Terminal {
        // Which slot holds the alternate buffer (no scrollback ring) varies:
        // `grid` is the active buffer and `alt_grid` the saved one, so the
        // active grid is the alt buffer iff on the alt screen, and the saved
        // grid iff not (a saved alt buffer under 1047/1049 exit semantics).
        // Keying off the slot alone would drop the saved main buffer's
        // scrollback when captured under 1049.
        let on_alt = c.modes.alternate_screen;
        let mut active_grid = restore_grid(c.rows, c.cols, &c.grid, &c.cursor, on_alt);
        active_grid.continue_absolute_numbering(c.absolute_row_counter);
        // Saved grid (same restore path; alt buffer ⟺ we are not on alt), if present.
        let saved_grid = match (&c.alt_grid, &c.alt_cursor) {
            (Some(alt_bytes), Some(alt_cursor)) => {
                let mut saved = restore_grid(c.rows, c.cols, alt_bytes, alt_cursor, !on_alt);
                saved.continue_absolute_numbering(c.alt_absolute_row_counter);
                Some(saved)
            }
            _ => None,
        };
        Self::restored(c, active_grid, saved_grid)
    }

    /// A copy of this engine that continues as it does: fed the same output and
    /// resizes from here, the two show the same rows and cursor and project equal
    /// checkpoints. For a replay whose variants share a prefix (`aterm_control`'s
    /// `ReplayEngine::fork`): fold the prefix once, then each variant from a fork.
    ///
    /// It is [`Self::from_checkpoint`] of the full projection, except that each
    /// grid is copied as this engine keeps it ([`Grid::replay_fork`]: the same
    /// ring under the same cap, where a restore attaches an unlimited tiered
    /// store), and the transient state later output reads that no checkpoint
    /// carries comes along: the orphaned-screen leave's notes, the
    /// synchronized-update timer, the last Kitty id assigned and the Kitty
    /// serials, and the live-image budget. The holder fence is this engine's,
    /// with the restore it continues: a fork is no restore of its own, so it
    /// awaits a host's adoption ([`Self::adopt_holder_after_restore`]) only
    /// where this engine does, untouched as long as this one would be, and a
    /// checkpoint of either carries the same seed uncertainty. A host's leave
    /// ([`Self::leave_alternate_screen`]) is taken inside the call that asks for
    /// it, so none is pending here. Host bindings are `Terminal::new`'s, as a
    /// restore's: a fork is exact for an engine set up as a fresh one, which a
    /// replay is.
    ///
    /// `None` when this engine holds state no fork carries: a parser inside a
    /// sequence (a picture or clipboard string would be swallowed), Kitty images,
    /// image numbers or a chunked transmission (the store is not copied), an
    /// open resize undo or a standing resize hand-back on either grid, or a
    /// tiered history store.
    #[must_use]
    pub fn replay_fork(&self) -> Option<Terminal> {
        let t = &self.transient;
        let kitty_idle = t.kitty_images.is_empty()
            && t.kitty_frames.is_empty()
            && t.kitty_variants.is_empty()
            && t.kitty_virtual.is_empty()
            && t.kitty_relations.is_empty()
            && t.kitty_floating.is_empty()
            && t.kitty_numbers.is_empty()
            && t.kitty_pending.is_none();
        if !self.parser_is_ground() || self.carried_parser().is_some() || !kitty_idle {
            return None;
        }
        let active = self.grid.replay_fork()?;
        let inactive = match &self.alt_grid {
            Some(grid) => Some(grid.replay_fork()?),
            None => None,
        };
        // The scalars, the shell marks of every line the grids carry included.
        let scalars = self.project(usize::MAX, usize::MAX, true, false, MarksFrom::Carried);
        let mut fork = Self::restored(&scalars, active, inactive);
        // `restored` installs the projection's fence as a carry awaiting
        // adoption, tainted where this engine's restore saw output first. The
        // fork holds the fence as this engine holds it, and this engine's
        // restore, if any, at the same distance behind its output count.
        fork.holder_fence.clone_from(&self.holder_fence);
        let since = |at: u64| t.screen_mutations.wrapping_sub(at);
        fork.holder_carried_at = self
            .holder_carried_at
            .map(|at| fork.transient.screen_mutations.wrapping_sub(since(at)));
        let to = &mut fork.transient;
        to.alt_entered_without_cursor_save = t.alt_entered_without_cursor_save;
        to.conhost_alt_screen_left_up = t.conhost_alt_screen_left_up;
        to.sync_start = t.sync_start;
        to.kitty_last_assigned_id = t.kitty_last_assigned_id;
        to.kitty_serial = t.kitty_serial;
        to.live_image_charge = t.live_image_charge;
        Some(fork)
    }

    /// [`Self::from_checkpoint`] over grids already rebuilt: everything else
    /// `c` carries, installed on a `Terminal::new`-configured engine.
    fn restored(c: &TerminalCheckpoint, active_grid: Grid, saved_grid: Option<Grid>) -> Terminal {
        let mut terminal = Terminal::with_grid(active_grid);
        terminal.alt_grid = saved_grid;

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

        // Style: set the semantic style, then re-arm the rebuilt grid's BCE
        // cursor template from it. `into_style()` rebuilds the writer caches,
        // but the fresh grid's template is default, so a restored non-default
        // background would be lost at the first scroll/erase before an SGR.
        terminal.style = c.style.into_style();
        {
            let (_parser, mut handler) = terminal.split_for_process();
            handler.sgr_style().apply_style_change();
        }
        terminal.apply_color_repr(&c.color);
        terminal.apply_shell_repr(&c.shell);
        terminal.install_holder_repr(&c.holder, main_counter(c));
        // The shell's integration body, when the carry named one — the same
        // install `restore_checkpoint` does, so the two restore doors agree
        // (the field's contract: a restore installs it, an absent or malformed
        // one leaves the engine's alone). Validated like a signed mark.
        if let Some(rev) = c
            .shell_integration_rev
            .as_deref()
            .filter(|rev| crate::shell_integration::is_integration_rev(rev))
        {
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(rev.as_bytes());
            terminal.shell.integration_rev = Some(bytes);
        }
        if let Some(title) = &c.title {
            terminal.restore_title(title);
        }
        // After the style: the open link goes through the OSC 8 handler.
        terminal.apply_output_state_repr(&c.output_state);
        // Last of the input-side state: the title above is restored as OSC input,
        // which must not run through a carried partial sequence.
        terminal.restore_parser_carry(c.parser.as_ref());
        terminal.arm_carried_sync_window();
        // `modes`/kitty/xterm were assigned by value above, after `with_grid`
        // published the fresh fold — republish for the hydrated state.
        terminal.refresh_mode_mirror();

        terminal
    }

    /// Restore a checkpoint's buffer state into an already-configured live terminal
    /// (the seamless-update adopt path): replaces only what [`Self::from_checkpoint`]
    /// would have captured, leaving host bindings and auth state untouched, so it
    /// composes with the spawn path's `configure_*` wiring.
    pub fn restore_checkpoint(&mut self, c: &TerminalCheckpoint) {
        // Scrollback retention is this process's config, not the checkpoint's.
        // The host's config reached `self` before this adopt, but
        // `restore_grid` hands the rebuilt grid an unlimited store so the
        // carried history is not dropped on the way in, and `apply_config`
        // only re-applies on change. So read the configured limit here, while
        // the host's grids are still installed, and re-impose it below.
        let configured_scrollback_limit = self.scrollback_line_limit();
        // The grids are replaced, so every generation stamp the cached search
        // index (and any budgeted-search cursor) was keyed against restarts;
        // a coincidental stamp match could otherwise serve pre-restore
        // results. Drop them; the next search rebuilds.
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
        // Re-imposed after `modes` is the adopted session's: the setter
        // targets the primary-content grid (`alt_grid` while the alternate
        // screen is up), which is only correct once the restored modes are in
        // place.
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
        // Style: semantic value, then re-arm the rebuilt grid's BCE cursor
        // template from it (see `from_checkpoint`).
        self.style = c.style.into_style();
        {
            let (_parser, mut handler) = self.split_for_process();
            handler.sgr_style().apply_style_change();
        }
        // The application's colours go over this process's configured theme,
        // and its shell-integration state (running command included) resumes.
        self.apply_color_repr(&c.color);
        // The latch came from the predecessor (`modes` above): what the program
        // was last told. Under this engine's theme its view may be stale; push
        // once if so and it kept 2031. `from_checkpoint` does not: the host's
        // `apply_config` that follows reconciles against the carried latch.
        let _ = self.reconcile_color_scheme();
        self.apply_shell_repr(&c.shell);
        self.install_holder_repr(&c.holder, main_counter(c));
        // The program's titles, as OSC input (`restore_title`). A checkpoint
        // that carried none (an older producer) leaves this engine's alone.
        if let Some(title) = &c.title {
            self.restore_title(title);
        }
        // The engine state output still to come relies on, the link through this
        // engine's own OSC 8 handler (an older producer's checkpoint holds none).
        self.apply_output_state_repr(&c.output_state);
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
        self.transient.sync_end_caret = None;
        // Hydration replaced both grids and `modes.alternate_screen`, so the
        // parked selection (restored on the next alt exit) would name content
        // from another lineage; clear it. The live selection is left alone, so
        // a highlight survives an in-place update.
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

/// The [`CheckpointMeta`] keys a reader requires before it deserializes a meta it did
/// not write in this process: the seamless update's screen carry (`aterm-gui`'s
/// `seamless::parse_checkpoint_meta`) and the crash-restore journal's keyframe
/// (`aterm-restore`). One list, so the two doors refuse the same metas.
///
/// FROZEN — THIS LIST MAY ONLY SHRINK (the 2026-09-22/23 update audit, plan
/// P0-2f/L3). The producer of a handoff is always the OLDER build, so a key
/// added here is a key every producer already in the field fails to send: each
/// of its sessions would arrive "unparseable". That used to refuse the whole
/// update; since P0-2 it degrades each such session to a blank, repainted
/// screen — every tab of every desk, on every hop from the older build. A new
/// meta field rides `#[serde(default)]` instead, and aterm-gui's
/// `the_required_meta_keys_are_frozen` pins this list verbatim. (A crash
/// journal is read by the build that relaunches after the crash, which may be
/// newer than the one that wrote it, for the same reason.)
#[cfg(feature = "serde")]
pub const CHECKPOINT_META_REQUIRED_KEYS: &[&str] = &[
    "rows",
    "cols",
    "cursor",
    "alt_cursor",
    "saved_cursor_main",
    "saved_cursor_alt",
    "modes",
    "style_fg_bits",
    "style_bg_bits",
    "style_flag_bits",
    "style_protected",
    "charset",
    "kitty_keyboard",
    "xterm_keyboard",
    "taskbar_progress",
    "secure_keyboard_entry",
    "current_working_directory",
];

/// The serde-carryable half of a [`TerminalCheckpoint`]: every scalar field, without
/// the two grid byte blobs (which stay on the binary `Line` codec).
///
/// The seamless update's screen carry: the outgoing process serializes this into the
/// handoff manifest and writes the grid blobs as sidecar files; the incoming process
/// reassembles the checkpoint ([`CheckpointMeta::into_checkpoint`]) and hydrates the
/// adopted session's engine. The generic serializer is additive via `serde(default)`:
/// a key a parent did not write restores its default, an older child ignores a key it
/// does not know, and the screen digest hashes the bytes as sent. Seamless schema 1 is
/// stricter (`aterm-gui` requires every semantic key and binds the canonical result
/// into its adoption proof). Style crosses as raw bits (`PackedColor(pub u32)`,
/// `CellFlags(pub u16)`) to keep serde out of the grid crate.
#[cfg(feature = "serde")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointMeta {
    /// Grid rows (visible).
    pub rows: u16,
    /// Grid cols.
    pub cols: u16,
    /// How many leading records of the main grid blob are scrollback rather than
    /// visible rows (see `TerminalCheckpoint::history_lines`). `serde(default)`: a
    /// producer without the field emitted a visible-only blob, which `0` is, and in a
    /// handoff the consumer is the newer build, so it accepts both shapes.
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
    /// ([`TerminalCheckpoint::shell_integration_nonce`]). Absent when there is none; a
    /// value that is not 64 hex digits reassembles as `None` (the successor reports
    /// the session `integration=degraded` rather than trusting a partial nonce).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_integration_nonce: Option<String>,
    /// [`TerminalCheckpoint::absolute_row_counter`]. `0` (a parent without the field)
    /// restores fresh numbering; written for every live session (a grid's counter is
    /// at least its rows).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub absolute_row_counter: u64,
    /// [`TerminalCheckpoint::alt_absolute_row_counter`], as above.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub alt_absolute_row_counter: u64,
    /// [`TerminalCheckpoint::color`]. Absent when nothing is overridden.
    #[serde(default, skip_serializing_if = "ColorRepr::is_default")]
    pub color: ColorRepr,
    /// [`TerminalCheckpoint::shell`]. Absent for a session that never saw a mark.
    #[serde(default, skip_serializing_if = "ShellRepr::is_default")]
    pub shell: ShellRepr,
    /// The integration body the shell last signed that it runs, 16 hex digits
    /// ([`TerminalCheckpoint::shell_integration_rev`]). Absent when there is none; a
    /// value that is not a folder address reassembles as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_integration_rev: Option<String>,
    /// [`TerminalCheckpoint::title`]. Written for every session, `{}` when nothing
    /// titled it: presence tells a successor the producer carried titles, so absent
    /// means it may seed the tab's title from the handoff record. A value that does
    /// not read costs only the title ([`lenient_title`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_title"
    )]
    pub title: Option<TitleRepr>,
    /// [`TerminalCheckpoint::parser`]. Absent for a Ground parser with nothing
    /// pending; an older child resumes at Ground. The screen digest covers it, so the
    /// carry is inside the adoption proof. A value that does not read costs only the
    /// partial sequence (`lenient_parser`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::checkpoint_parser::lenient_parser"
    )]
    pub parser: Option<super::checkpoint_parser::ParserRepr>,
    /// [`TerminalCheckpoint::output_state`]. Absent when none of it is set (a session
    /// that printed anything carries at least its REP character); an older child
    /// resumes with none of it. A value that does not read costs only this state
    /// (`lenient_output_state`), never the screen.
    #[serde(
        default,
        skip_serializing_if = "OutputStateRepr::is_default",
        deserialize_with = "super::checkpoint_output::lenient_output_state"
    )]
    pub output_state: OutputStateRepr,
    /// [`TerminalCheckpoint::holder`]. Absent when no drawer ran; an older child
    /// starts with no fence. A value that does not read costs only the fence
    /// (`lenient_holder`): every card stays bare until its drawer is seeded.
    #[serde(
        default,
        skip_serializing_if = "HolderRepr::is_empty",
        deserialize_with = "lenient_holder"
    )]
    pub holder: HolderRepr,
}

/// Read a carried [`HolderRepr`], or none when the value is not one: like
/// [`lenient_title`], a malformed fence must not cost the screen.
#[cfg(feature = "serde")]
fn lenient_holder<'de, D>(deserializer: D) -> Result<HolderRepr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Holder(HolderRepr),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Wire as serde::Deserialize>::deserialize(deserializer)? {
            Wire::Holder(holder) => holder,
            Wire::Unreadable(_) => HolderRepr {
                seed_untrusted: true,
                ..HolderRepr::default()
            },
        },
    )
}

/// Read a carried [`TitleRepr`], or `None` when the value is not one: `CheckpointMeta`
/// is parsed whole, and a failed strict field would send the session to a blank,
/// repainted screen (`aterm-gui`'s consumer degrade). A malformed title reads as not
/// carried, like a producer that never wrote the key.
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
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero_rows(n: &i32) -> bool {
    *n == 0
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero_above(n: &u16) -> bool {
    *n == 0
}

#[cfg(feature = "serde")]
impl CheckpointMeta {
    /// Project a full checkpoint into its carryable meta half.
    ///
    /// Exhaustive destructure on purpose: a new [`TerminalCheckpoint`] field fails
    /// this build until the carry ships it or deliberately drops it.
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
            output_state,
            holder,
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
            output_state: output_state.clone(),
            holder: holder.clone(),
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
            // Likewise judged where it lands (`apply_output_state_repr`).
            output_state: self.output_state,
            // Likewise (`install_holder_repr`).
            holder: self.holder,
        }
    }
}

/// The absolute row counter of the checkpoint's main grid: the inactive one
/// while the alternate screen is up.
fn main_counter(c: &TerminalCheckpoint) -> u64 {
    if c.modes.alternate_screen {
        c.alt_absolute_row_counter
    } else {
        c.absolute_row_counter
    }
}

/// Rebuild a single grid from `serialize_lines` bytes + a cursor projection. The
/// bytes are scrollback-then-visible (the `checkpoint_lines` layout): the last `rows`
/// lines are the visible rows, everything before is scrollback, oldest first, pushed
/// into a tiered store.
fn restore_grid(rows: u16, cols: u16, bytes: &[u8], cursor: &GridCursorRepr, is_alt: bool) -> Grid {
    let lines = deserialize_lines(bytes);
    let visible_start = lines.len().saturating_sub(rows as usize);
    let (scrollback_lines, visible_lines) = lines.split_at(visible_start);

    let mut grid = if is_alt {
        // The alternate screen keeps no scrollback (as `enter_alternate_screen`): a
        // ring would accrue history the source never had and diverge on re-checkpoint.
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

    /// The repaint rung's scalars without the grids: `carry_meta_abandoning_partial`
    /// equals the meta of the visible-only carry for every state that moves a
    /// scalar (scrollback, the alternate screen, a parser left mid-sequence,
    /// shell marks), and never takes the carry projection (the fault point
    /// would fail it).
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

    /// Drive a fresh terminal with a byte stream that exercises every captured
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
        //     set is a host opt-in (`allow_palette_reconfigure`), and `modes`
        //     round-trips with it on. ---
        t.modes.allow_palette_reconfigure = true;
        t.process(b"\x1b]4;1;rgb:12/34/56\x07\x1b]10;rgb:aa/bb/cc\x07\x1b]12;rgb:01/02/03\x07");
        t.process(b"\x1b]30001\x07");
        t.process(b"\x1b]4;2;rgb:65/43/21\x07");

        // --- shell integration: one finished command, then one running (C
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

    /// A restored engine's checkpoint with its restore lineage asserted and taken
    /// out: output reached the restore before any host adopted it, so it carries
    /// that lineage's seed uncertainty (`HolderRepr::seed_untrusted`), which a
    /// live engine never has and which is no session state. Every other field is
    /// held to the live engine's.
    fn lineage_taken(c: TerminalCheckpoint) -> TerminalCheckpoint {
        assert!(
            c.holder.seed_untrusted,
            "a restore output reached before adoption carries its lineage"
        );
        c.without_seed_lineage()
    }

    /// Read a cell's (char, flags, fg, bg) at (row, col) on the active grid.
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
        assert_eq!(c0.color.stack_overrides.len(), 1, "one XTPUSHCOLORS entry");
        assert_eq!(c0.shell.command_marks.len(), 1, "one finished command");
        assert!(
            c0.shell.current_block.is_some(),
            "a running command's block"
        );
        assert_eq!(c0.shell.completed_seq, 1);
        let h = Terminal::from_checkpoint(&c0);
        let c1 = h.checkpoint();

        assert_eq!(c0, c1, "re-checkpoint equality (c0 == c1)");

        assert_eq!(
            t.visible_content(),
            h.visible_content(),
            "visible_content equal"
        );
        for r in 0..rows as usize {
            assert_eq!(t.row_text(r), h.row_text(r), "row_text equal for row {r}");
        }

        assert_eq!(t.cursor(), h.cursor(), "cursor equal");

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

    /// A partial sequence rides the visible checkpoint; only a hooked DCS
    /// query, which no carry can hold, fails closed instead of dropping
    /// parser bytes.
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

        terminal.process(b"\x1bP$qm");
        assert_eq!(
            terminal.partial_sequence_uncarried(),
            Some("DcsPassthrough")
        );
        assert!(
            terminal.checkpoint_visible().is_none(),
            "a hooked DCS string fails closed"
        );
    }

    /// The bounded continuing checkpoint is the full one of the same engine
    /// under a scrollback limit of the bound: the primary keeps its newest `n`
    /// lines whether it is the active grid or the one the alternate screen
    /// saved, and a split sequence rides it. Negative control: the unbounded
    /// projection carries every line.
    #[test]
    fn a_bounded_continuing_checkpoint_is_the_full_one_under_that_scrollback_limit() {
        const BOUND: usize = 10;
        let engine = |alt: bool| {
            let mut t = Terminal::new(4, 20);
            for i in 0..30 {
                t.process(format!("line-{i}\r\n").as_bytes());
            }
            if alt {
                t.process(b"\x1b[?1049hon the alt screen");
            }
            t.process(b"\x1b[1;3");
            t
        };
        for alt in [false, true] {
            let bounded = engine(alt)
                .checkpoint_continuing_bounded(BOUND)
                .expect("a split CSI is carried");
            let mut limited = engine(alt);
            limited.set_scrollback_line_limit(Some(BOUND));
            assert_eq!(
                Some(&bounded),
                limited.checkpoint_continuing().as_ref(),
                "alt: {alt}"
            );
            let primary = if alt {
                bounded.alt_grid.as_deref().expect("the saved primary")
            } else {
                &bounded.grid[..]
            };
            assert_eq!(deserialize_lines(primary).len(), BOUND + 4, "alt: {alt}");
            let full = engine(alt).checkpoint_continuing().expect("carried");
            let full_primary = if alt {
                full.alt_grid.as_deref().expect("the saved primary")
            } else {
                &full.grid[..]
            };
            assert!(
                deserialize_lines(full_primary).len() > BOUND + 4,
                "alt: {alt}"
            );

            let mut restored = Terminal::from_checkpoint(&bounded);
            let mut live = engine(alt);
            restored.process(b"1mX");
            live.process(b"1mX");
            assert!(restored.parser_is_ground(), "the tail ended the CSI");
            assert_eq!(restored.visible_content(), live.visible_content());
        }
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
            restored.checkpoint_visible().map(lineage_taken),
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
            restored.checkpoint_visible().map(lineage_taken),
            "DECRC and subsequent styled output must remain equivalent"
        );
    }

    #[test]
    fn checkpoint_post_hydration_styled_write_matches() {
        // The writer caches and the BCE cursor template are rebuilt: a styled
        // write on the source and the hydrated terminal produces identical cells.
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
            lineage_taken(h.checkpoint()),
            "post-write re-checkpoint equality"
        );
    }

    #[test]
    fn checkpoint_alt_screen_toggle_on_hydrated() {
        // Toggle alt-screen off on the hydrated terminal and confirm it tracks
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
            lineage_taken(h.checkpoint()),
            "re-checkpoint equal after alt toggle"
        );
    }

    #[test]
    fn checkpoint_roundtrips_decslrm_horizontal_margins() {
        // DECSLRM margins are not recoverable from any other captured field, so
        // the projection carries them; otherwise a checkpoint under DECLRMM
        // (mode 69) restores full-width margins and diverges.
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
        // Mode 47 leaves you on the main screen with a populated alt buffer
        // retained in `alt_grid`. The active (main) grid must keep its
        // scrollback ring and the saved alt buffer must restore with none: the
        // inverse polarity of the 1049 tests.
        let (rows, cols) = (8u16, 24u16);
        let mut t = Terminal::new(rows, cols);
        // Fill main with more than `rows` lines so it carries real scrollback.
        for i in 0..(rows as usize + 5) {
            t.process(format!("main{i}\r\n").as_bytes());
        }
        // Enter mode-47 alt, scribble, then exit back to main (47 retains alt).
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
        // A restored alt screen accrues no scrollback when scrolled, like a live
        // one. Re-checkpoint identity alone cannot see a ring on it (the captured
        // bytes never carry alt scrollback), so both are scrolled and compared.
        let (rows, cols) = (6u16, 16u16);
        let mut t = Terminal::new(rows, cols);
        t.process(b"\x1b[?1049h"); // enter alt screen (active grid = alt, 0-ring)
        assert!(t.parser_is_ground() && t.modes.alternate_screen);

        let c0 = t.checkpoint();
        let mut h = Terminal::from_checkpoint(&c0);
        assert!(h.modes.alternate_screen, "hydrated is on the alt screen");

        // Scroll both well past `rows` lines.
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
            lineage_taken(h.checkpoint()),
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
    /// A resize moves a DECSC slot only with its text, and the wire carries it
    /// as the engine holds it (see `SavedCursorRepr::capture`): a shrink that
    /// demotes the saved primary's top row moves the slot up with it, and a
    /// grow that reveals the row again moves it back.
    #[test]
    fn a_resize_moves_the_decsc_slot_with_its_text_and_the_wire_carries_it() {
        let mut t = Terminal::new(56, 149);
        t.process(b"\x1b[56;1H\x1b[?1049h");
        assert_eq!(t.cursor_save.main.map(|saved| saved.cursor.row), Some(55));
        t.resize(55, 149);
        assert_eq!(
            t.cursor_save.main.map(|saved| saved.cursor.row),
            Some(54),
            "the demote moved the slot's row up one"
        );
        let cp = t.checkpoint_carry(0).expect("Ground");
        assert_eq!(cp.rows, 55);
        assert_eq!(
            cp.saved_cursor_main.map(|saved| saved.cursor_row),
            Some(54),
            "the wire carries the slot as the engine holds it"
        );
        t.resize(56, 149);
        t.process(b"\x1b[?1049l");
        assert_eq!(t.cursor().row, 55, "the reveal moved it back");
    }

    /// What a primary screen's rows-only resizes owe back at one edge rides the
    /// checkpoint (`GridCursorRepr::handback_rows`), through the meta wire too: a
    /// flap whose halves straddle a restore leaves every row where the program drew
    /// it, as one in a single session does. Control: the same restore with the
    /// hand-back forgotten keeps the revealed history line on top.
    #[test]
    fn a_flap_across_a_restore_hands_back_what_it_owes() {
        fn screen(t: &Terminal) -> Vec<String> {
            (0..usize::from(t.rows()))
                .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
                .collect()
        }
        let fresh = || {
            let mut t = Terminal::new(8, 20);
            for i in 0..20 {
                t.process(format!("h{i}\r\n").as_bytes());
            }
            t.process(b"\x1b[H\x1b[2Jtop\r\n");
            t
        };
        let drawn = screen(&fresh());
        // (heights before the restore, what is owed, the natural moves since):
        // a reveal; a trim; reveals until history ran out, then blank appends.
        let paths: [(&[u16], i32, u16); 3] = [(&[9], -1, 0), (&[7], 1, 0), (&[30], -13, 9)];
        for (path, owed, above) in paths {
            let mut t = fresh();
            for &rows in path {
                t.resize(rows, 20);
            }
            let carry = t.checkpoint_carry(usize::MAX).expect("Ground");
            assert_eq!(
                (carry.cursor.handback_rows, carry.cursor.handback_above),
                (owed, above),
                "{path:?}"
            );
            #[cfg(feature = "serde")]
            let carry =
                CheckpointMeta::from_checkpoint(&carry).into_checkpoint(carry.grid.clone(), None);
            let mut restored = Terminal::from_checkpoint(&carry);
            assert_eq!(
                (
                    restored.grid().handback_rows(),
                    restored.grid().handback_above()
                ),
                (owed, above),
                "{path:?}: seeded"
            );
            restored.resize(8, 20);
            assert_eq!(
                screen(&restored),
                drawn,
                "{path:?}: every row where it was drawn"
            );
        }

        // Trims then a demote: the demoted row rides as the natural move made
        // since. Its reveal draws on the history a restored grid keeps in the
        // tier; the screen it comes back to is checked by
        // `a_flap_across_a_restore_brings_a_demoted_row_back`.
        let mut t = fresh();
        t.resize(2, 20);
        t.resize(1, 20);
        let restored = Terminal::from_checkpoint(&t.checkpoint_carry(usize::MAX).expect("Ground"));
        assert_eq!(
            (
                restored.grid().handback_rows(),
                restored.grid().handback_above()
            ),
            (6, 1),
            "trims then a demote"
        );

        let mut t = fresh();
        t.resize(9, 20);
        let mut restored =
            Terminal::from_checkpoint(&t.checkpoint_carry(usize::MAX).expect("Ground"));
        restored.forget_handback();
        restored.resize(8, 20);
        assert_eq!(
            screen(&restored)[..2],
            ["h12", "top"],
            "forgotten, every row stays down"
        );
    }

    /// A flap whose second half must reveal a row its first half demoted comes back
    /// across a restore too. A restored grid keeps its history in the scrollback tier
    /// (`restore_grid`), and a rows-only grow revealed only from the ring, so it appended
    /// a blank instead: a full screen with the prompt on its last row (8 -> 7, restore,
    /// -> 8) came back one row higher than drawn, and a blank-tailed screen shrunk past
    /// its tail (8 -> 2 -> 1, restore, -> 8) came back blank. Each now matches the live
    /// terminal that ran the same flap with no restore between.
    #[test]
    fn a_flap_across_a_restore_brings_a_demoted_row_back() {
        fn picture(t: &Terminal) -> (Vec<String>, u16, u16) {
            let rows = (0..usize::from(t.rows()))
                .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
                .collect();
            (rows, t.cursor().row, t.cursor().col)
        }
        let full = || {
            let mut t = Terminal::new(8, 20);
            for i in 0..30 {
                t.process(format!("n{i}\r\n").as_bytes());
            }
            t.process(b"prompt$ ");
            t
        };
        let blank_tailed = || {
            let mut t = Terminal::new(8, 20);
            for i in 0..20 {
                t.process(format!("h{i}\r\n").as_bytes());
            }
            t.process(b"\x1b[H\x1b[2Jtop\r\n");
            t
        };
        let cases: [(&str, &dyn Fn() -> Terminal, &[u16]); 3] = [
            ("full screen, 7", &full, &[7]),
            ("full screen, 5", &full, &[5]),
            ("blank tail, 2 then 1", &blank_tailed, &[2, 1]),
        ];
        for (label, build, half) in cases {
            let drawn = picture(&build());
            let mut live = build();
            for &rows in half {
                live.resize(rows, 20);
            }
            let carry = live.checkpoint_carry(usize::MAX).expect("Ground");
            let mut restored = Terminal::from_checkpoint(&carry);
            live.resize(8, 20);
            restored.resize(8, 20);
            assert_eq!(picture(&live), drawn, "{label}: live");
            assert_eq!(picture(&restored), drawn, "{label}: across the restore");
        }
    }

    /// A synchronized-update window open at the checkpoint still times out:
    /// `modes` crosses whole, and the timer is re-armed at the restore, so a
    /// program that never sends `?2026l` cannot leave mode 2026 set, on either
    /// restore path. Control: a `?2026l` in the replayed tail closes it.
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

    /// `output_state_in_flight` names an open OSC 8 link and a VT52 `ESC Y` waiting
    /// for its address bytes while pending, and neither once done; the checkpoint
    /// carries each on both restore paths.
    #[test]
    fn output_state_in_flight_names_an_open_link_and_a_pending_vt52_address() {
        let mut t = Terminal::new(4, 20);
        assert_eq!(t.output_state_in_flight(), None);

        t.process(b"\x1b]8;id=x;https://example.com\x07lin");
        assert_eq!(t.output_state_in_flight(), Some("Osc8HyperlinkOpen"));
        let cp = t.checkpoint_carry(0).expect("carried");
        let mut adopted = Terminal::new(4, 20);
        adopted.restore_checkpoint(&cp);
        for (at, restored) in [
            ("from_checkpoint", Terminal::from_checkpoint(&cp)),
            ("restore", adopted),
        ] {
            assert_eq!(
                restored.current_hyperlink().map(AsRef::as_ref),
                Some("https://example.com"),
                "{at}: the restore keeps the open link"
            );
            assert_eq!(
                restored.output_state_in_flight(),
                Some("Osc8HyperlinkOpen"),
                "{at}"
            );
        }
        t.process(b"k\x1b]8;;\x07");
        assert_eq!(t.output_state_in_flight(), None, "the link is closed");

        t.process(b"\x1b[?2l\x1bY");
        assert_eq!(t.output_state_in_flight(), Some("Vt52CursorAddress"));
        t.process(b"\x22");
        assert_eq!(
            t.output_state_in_flight(),
            Some("Vt52CursorAddress"),
            "the row is in, the column still to come"
        );
        let mut half = Terminal::from_checkpoint(&t.checkpoint_carry(0).expect("carried"));
        t.process(b"\x24");
        assert_eq!(t.output_state_in_flight(), None, "the address is complete");
        assert_eq!((t.cursor().row, t.cursor().col), (2, 4));
        half.process(b"\x24");
        assert_eq!(
            (half.cursor().row, half.cursor().col),
            (2, 4),
            "the restored engine took the column the address was waiting for"
        );
        t.process(b"\x1b<");
        assert_eq!(t.output_state_in_flight(), None);
    }

    /// The in-flight output state the restore installs is judged where it
    /// lands, as the same state from a program would be: an open link this
    /// engine's OSC 8 handler refuses is not installed (its scheme, here, is
    /// not on the allowlist), a REP character that is a control is dropped,
    /// an SGR stack deeper than XTPUSHSGR keeps is cut to its newest entries, a
    /// push mask's stray bits are dropped, a saved mode this engine's XTSAVE
    /// does not know is dropped, and a checkpoint that carries none of it
    /// clears what the engine held.
    #[test]
    fn a_restored_output_state_goes_through_the_engines_own_doors() {
        use crate::terminal::{SavedModeRepr, SgrPushRepr};
        let pushed = |fg: u32, mask: u16| SgrPushRepr {
            fg,
            bg: 0,
            flags: 0,
            protected: false,
            underline_color: None,
            mask,
        };
        let depth = u32::try_from(crate::terminal::SGR_STACK_MAX_DEPTH).expect("small");
        let mut cp = Terminal::new(4, 20).checkpoint_carry(0).expect("Ground");
        cp.output_state = OutputStateRepr {
            rep: Some('\u{7}'),
            underline_color: Some(0x01_ff_00_00),
            hyperlink: Some(crate::terminal::HyperlinkRepr {
                uri: "javascript:alert(1)".into(),
                id: None,
            }),
            vt52_address: None,
            zwj_pending: true,
            sgr_stack: (0..depth + 1)
                .map(|n| pushed(n, 0x3FF))
                .chain([pushed(depth + 1, 0xFFFF)])
                .collect(),
            saved_modes: vec![
                SavedModeRepr {
                    mode: 7,
                    set: false,
                },
                SavedModeRepr {
                    mode: 65000,
                    set: true,
                },
            ],
        };
        let mut restored = Terminal::new(4, 20);
        restored.process(b"Y\x1b]8;;https://example.com\x07\x1b[#{\x1b[?25s");
        restored.restore_checkpoint(&cp);
        assert_eq!(
            restored.current_hyperlink(),
            None,
            "the refused link is not installed"
        );
        assert_eq!(
            restored.capture_output_state_repr(),
            OutputStateRepr {
                underline_color: Some(0x01_ff_00_00),
                zwj_pending: true,
                sgr_stack: (2..depth + 1)
                    .map(|n| pushed(n, 0x3FF))
                    .chain([pushed(depth + 1, 0x3FF)])
                    .collect(),
                saved_modes: vec![SavedModeRepr {
                    mode: 7,
                    set: false,
                }],
                ..OutputStateRepr::default()
            },
            "the newest pushes, the mask's own bits, the known mode; none of the engine's own"
        );
        restored.process(b"\x1b[3b");
        assert_eq!(
            restored.row_text(0).unwrap_or_default().trim_end(),
            "",
            "no REP character: the control was dropped, and the engine's own `Y` cleared"
        );
        // Control: the same shapes, well formed, are installed.
        cp.output_state.rep = Some('Y');
        cp.output_state.hyperlink = Some(crate::terminal::HyperlinkRepr {
            uri: "https://example.com".into(),
            id: Some("x".into()),
        });
        cp.output_state.sgr_stack = (0..depth).map(|n| pushed(n, 0x21)).collect();
        cp.output_state.saved_modes.truncate(1);
        restored.restore_checkpoint(&cp);
        assert_eq!(restored.capture_output_state_repr(), cp.output_state);
    }

    /// `printf '\e]0;x'` with no terminator leaves the parser in `OscString`;
    /// the carry takes the partial OSC along, so the restored engine is still
    /// inside the title and the terminator that arrives after the handoff
    /// completes it. The live parser is only read, so a rolled-back handoff
    /// loses nothing.
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

    /// A hooked DCS string rides the carry unless it is a query:
    ///
    /// * a string the engine only consumes (tmux control mode, a kitty remote-control
    ///   command) is carried as one to ignore and loses nothing: the restored engine
    ///   reads the queued tail (UTF-8 in the payload, an 8-bit ST) exactly as the live
    ///   one does, so it is not named as swallowed (a tmux passthrough wrapper hooks no
    ///   string: `handoff_parser_carry` carries it);
    /// * a sixel string is carried the same way and named a swallowed picture: its
    ///   tail is swallowed, whichever ST ends it, and the next frame replaces it;
    /// * a query (DECRQSS) stays uncarried, as its reply would be lost (the
    ///   abandoning carry drops it and names it, see
    ///   `every_partial_parser_state_is_carried_and_named`).
    #[test]
    fn a_hooked_dcs_rides_the_carry_unless_it_is_a_query() {
        let consumed: [(&[u8], &[u8]); 3] = [
            (
                b"\x1bP1000p%begin 1 1 0\n",
                "%output %1 caf\u{e9} \u{276f} ls\n%end\x1b\\after".as_bytes(),
            ),
            (
                b"\x1bP@kitty-cmd{\"t\":\"",
                "h\u{e9}llo \u{276f}\"}\x1b\\after".as_bytes(),
            ),
            (b"\x1bP@kitty-cmd{", b"\"t\":1}\x9cafter"),
        ];
        for (head, tail) in consumed {
            let mut live = Terminal::new(4, 30);
            live.process(b"one\r\n");
            live.process(head);
            assert_eq!(live.partial_sequence_state(), Some("DcsPassthrough"));
            assert_eq!(live.partial_sequence_uncarried(), None);
            assert_eq!(live.partial_sequence_swallowed(), None, "nothing is lost");
            assert!(!live.partial_sequence_swallows_picture());
            let cp = live.checkpoint_carry(0).expect("carried");
            assert_eq!(
                cp.parser.as_ref().map(|carry| carry.state),
                Some(aterm_parser::HOOKED_STRING_CARRY)
            );
            let mut restored = Terminal::from_checkpoint(&cp);
            restored.process(tail);
            live.process(tail);
            assert_eq!(
                restored.visible_content(),
                live.visible_content(),
                "{:?}",
                String::from_utf8_lossy(head)
            );
            assert_eq!(
                restored.row_text(1).as_deref().map(str::trim_end),
                Some("after"),
                "{:?}",
                String::from_utf8_lossy(head)
            );
            assert_eq!(restored.title(), live.title(), "the inner sequence lands");
        }

        for tail in [&b"#0!10~-\x1b\\after"[..], &b"#0!10~-\x9cafter"[..]] {
            let mut sixel = Terminal::new(4, 30);
            sixel.process(b"one\r\n\x1bPq#0;2;0;0;0");
            assert_eq!(sixel.partial_sequence_uncarried(), None);
            assert!(
                sixel.partial_sequence_swallowed().is_some(),
                "a sixel the engine decodes is a picture the carry swallows"
            );
            assert!(sixel.partial_sequence_swallows_picture());
            let cp = sixel.checkpoint_carry(0).expect("carried");
            let mut restored = Terminal::from_checkpoint(&cp);
            restored.process(tail);
            assert_eq!(
                restored.row_text(1).as_deref().map(str::trim_end),
                Some("after"),
                "{tail:?}: the tail is swallowed, never printed, and what follows prints"
            );
            assert_eq!(
                sixel.parser.state().name(),
                "DcsPassthrough",
                "live parser untouched"
            );
        }

        let mut query = Terminal::new(4, 30);
        query.process(b"\x1bP+q");
        assert_eq!(query.partial_sequence_uncarried(), Some("DcsPassthrough"));
        assert!(query.checkpoint_carry(0).is_none());
    }

    /// A hooked DCS whose ESC has arrived without the byte after it is still
    /// hooked: that byte decides whether the string ends at ST or is broken off.
    /// A query there stays uncarried, as inside its payload (its reply would be
    /// lost); a string the engine only consumes, or a sixel, rides as the ESC
    /// alone, so the restored engine reads the next byte as the live one does.
    #[test]
    fn a_hooked_dcs_at_the_escape_of_its_st_rides_the_carry_unless_it_is_a_query() {
        for head in [&b"\x1bP$qm\x1b"[..], b"\x1bP+q544E\x1b"] {
            let mut query = Terminal::new(4, 30);
            query.process(head);
            assert_eq!(query.take_response(), None, "{head:?}: not answered yet");
            assert!(query.partial_sequence_uncarried().is_some(), "{head:?}");
            assert!(query.checkpoint_carry(0).is_none(), "{head:?}");
        }
        for head in [&b"\x1bP1000p%end\x1b"[..], b"\x1bPq#0;2;0;0;0#0~\x1b"] {
            for tail in [&b"\\after"[..], b"]2;inner\x07after"] {
                let mut live = Terminal::new(4, 30);
                live.process(b"one\r\n");
                live.process(head);
                assert_eq!(live.partial_sequence_uncarried(), None, "{head:?}");
                let cp = live.checkpoint_carry(0).expect("carried");
                assert_eq!(
                    cp.parser.as_ref().map(|carry| carry.state),
                    Some(aterm_parser::State::Escape),
                    "{head:?}: the ESC alone"
                );
                let mut restored = Terminal::from_checkpoint(&cp);
                restored.process(tail);
                live.process(tail);
                assert_eq!(
                    restored.row_text(1).as_deref().map(str::trim_end),
                    Some("after"),
                    "{head:?} then {tail:?}"
                );
                assert_eq!(restored.title(), live.title(), "{head:?} then {tail:?}");
            }
        }
    }

    /// A string holding a character with a `0x9C` byte is swallowed whole, live and
    /// across a handoff split inside that character: U+672C 本 is `E6 9C AC`, and with
    /// C1 controls off a DCS or APC string ends at the 8-bit ST `0x9C`, so the byte
    /// must read as the character's. A carry taken between `E6` and `9C` holds the
    /// head through the meta wire, so the successor swallows the rest as the live
    /// engine does.
    #[test]
    fn a_string_with_a_0x9c_inside_a_character_is_swallowed_live_and_carried() {
        let strings: [(&[u8], &str); 3] = [
            (b"\x1bP1000p", "%output %1 \u{672c}\u{d71c}\u{1f71c}\n%end"),
            (b"\x1bP@kitty-cmd", "{\"t\":\"\u{672c}\"}"),
            (b"\x1b_G", "a=q,\u{672c}"),
        ];
        for (head, payload) in strings {
            let stream = [head, payload.as_bytes(), b"\x1b\\after"].concat();
            let mut live = Terminal::new(4, 30);
            live.process(b"one\r\n");
            live.process(&stream);
            assert_eq!(
                live.row_text(1).as_deref().map(str::trim_end),
                Some("after"),
                "{payload:?}: swallowed whole, live"
            );

            let split = head.len() + payload.find('\u{672c}').expect("holds 本") + 1;
            let (before, after) = stream.split_at(split);
            let mut outgoing = Terminal::new(4, 30);
            outgoing.process(b"one\r\n");
            outgoing.process(before);
            let cp = outgoing.checkpoint_carry(0).expect("carried");
            assert_eq!(
                cp.parser
                    .as_ref()
                    .map(|carry| carry.string_utf8_tail.as_slice()),
                Some(&[0xE6][..]),
                "{payload:?}: the carry holds the character's head"
            );
            #[cfg(feature = "serde")]
            let cp = CheckpointMeta::from_checkpoint(&cp)
                .into_checkpoint(cp.grid.clone(), cp.alt_grid.clone());
            let mut restored = Terminal::from_checkpoint(&cp);
            restored.process(after);
            assert_eq!(
                restored.row_text(1).as_deref().map(str::trim_end),
                Some("after"),
                "{payload:?}: swallowed whole, across the handoff"
            );
        }
    }

    /// Every non-Ground state the parser can be parked in is carried, whole (a DCS
    /// header among them) or as a string to ignore to its terminator (SOS/PM/APC, and
    /// a hooked DCS string whose handler keeps nothing or holds a picture), except a
    /// hooked DCS query, which is abandoned and named. None trips
    /// `checkpoint_bounded`'s Ground `debug_assert` (tests run with debug assertions,
    /// so a path through it panics), and each restored carry re-projects to itself.
    #[test]
    fn every_partial_parser_state_is_carried_and_named() {
        let partials: [(&[u8], &str, Option<&str>); 13] = [
            (b"\x1b", "Escape", Some("Escape")),
            (b"\x1b(", "EscapeIntermediate", Some("EscapeIntermediate")),
            (b"\x1b[", "CsiEntry", Some("CsiEntry")),
            (b"\x1b[1;", "CsiParam", Some("CsiParam")),
            (b"\x1b[?1$", "CsiIntermediate", Some("CsiIntermediate")),
            (b"\x1bP", "DcsEntry", Some("DcsEntry")),
            (b"\x1bP1;2", "DcsParam", Some("DcsParam")),
            (b"\x1bP1$", "DcsIntermediate", Some("DcsIntermediate")),
            (
                b"\x1bPq#0;2;0;0;0",
                "DcsPassthrough",
                Some("SosPmApcString"),
            ),
            // A tmux passthrough wrapper's head rides as itself.
            (b"\x1bPtmu", "DcsIgnore", Some("DcsIgnore")),
            (b"\x1bP$qm", "DcsPassthrough", None),
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

    /// On a Ground parser the abandoning carry is the ordinary carry, so a producer
    /// can call it unconditionally without changing a healthy carry's bytes (or its
    /// adoption digest).
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

        t.process(b"\x1bP$qm");
        assert_eq!(t.partial_sequence_uncarried(), Some("DcsPassthrough"));
        assert_eq!(
            t.checkpoint_carry_abandoning_partial(0).1,
            t.partial_sequence_uncarried()
        );
    }

    /// The abandoning carry carries the authorized shell-integration nonce as
    /// `checkpoint_carry` does, so a tab whose parser was mid-sequence at the
    /// park keeps signing its marks across the update.
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
        // is degraded, and says so; authorizing the carried value heals it.
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

    /// The body a shell runs: only a signed `633;P;AtermIntegration=<rev>`
    /// records it (with the gate off or a wrong id the bytes change nothing),
    /// only a folder address is taken, and RIS keeps it. Like the nonce, only
    /// the carry projections take it across an update, the meta round-trips it
    /// and refuses a malformed one, and the adopting engine's restore installs
    /// it.
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

        // The OTHER restore door agrees: `from_checkpoint` installs the carried
        // rev too (it used to drop it), and a carry naming none yields the fresh
        // engine's None.
        assert_eq!(
            Terminal::from_checkpoint(&carry).shell_integration_rev(),
            Some(rev_b),
            "from_checkpoint installs the carried rev"
        );
        assert_eq!(
            Terminal::from_checkpoint(&plain).shell_integration_rev(),
            None,
            "from_checkpoint of a carry naming none has none"
        );
    }

    /// A re-key the shell has not taken yet: the key verifies marks at once,
    /// but the posture stays `Degraded` and the handoff carry leaves it behind
    /// until a mark signed with it arrives. A mark with the wrong id does not
    /// count. Control: a nonce authorized the ordinary way (spawn, or a
    /// carried one) is in use at once.
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

    /// A typed re-key (healing a shell spawned before the re-key channel, its key on
    /// a one-use file a typed line reads) is the channel's key plus a way back. Taken
    /// back, it authorizes nothing and the prior authorization stands again; settled,
    /// it stays pending until its first mark and can no longer be taken back; once a
    /// mark carried it, neither verb moves it.
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
        // the authorization from before either — here the channel key the shell
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

    /// The titles a program set cross the carry: its tab title (OSC 2), icon
    /// name (OSC 1) and pushed title stack (CSI 22 t) ride the carry and its
    /// meta, and the adopting engine holds them as the live one did, the pop
    /// included, with the epoch a host relabels by.
    #[test]
    fn carry_keeps_title_icon_and_stack() {
        let mut t = Terminal::new(4, 20);
        t.process(b"\x1b]1;the-icon\x07\x1b]2;the caller\x07");
        t.process(b"\x1b[22;0t\x1b]2;vim main.rs\x07");
        let expected = TitleRepr {
            window: "vim main.rs".to_string(),
            icon: "the-icon".to_string(),
            stack: vec![("the-icon".to_string(), "the caller".to_string())],
            ..TitleRepr::default()
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

        // A checkpoint that carried no title (an older producer) leaves the
        // adopting engine's own, which the host may have seeded.
        let mut untitled = carry.clone();
        untitled.title = None;
        let mut seeded = Terminal::new(4, 20);
        seeded.restore_title(&TitleRepr {
            window: "from the record".to_string(),
            ..TitleRepr::default()
        });
        seeded.restore_checkpoint(&untitled);
        assert_eq!(seeded.title(), "from the record");
        // …while an untitled carry is a title: nothing, and it says so.
        let mut blank = carry;
        blank.title = Some(TitleRepr::default());
        seeded.restore_checkpoint(&blank);
        assert_eq!(seeded.title(), "", "an untitled program stays untitled");
    }

    /// Empty is a saved title too, while a slot absent from a selective push
    /// must stay absent. Exercise all three checkpoint projections through the
    /// real OSC/XTWINOPS path, with a nonempty handoff first in each projection.
    #[test]
    fn a_checkpoint_preserves_saved_empty_title_slots() {
        let mut observed = Vec::new();
        let mut expected = Vec::new();
        for projection in ["memory", "carry", "journal"] {
            for (label, prefix, tail, window, icon) in [
                (
                    "nonempty",
                    &b"\x1b]0;caller\x07\x1b[22t\x1b]0;program\x07"[..],
                    &b"\x1b[23t"[..],
                    "caller",
                    "caller",
                ),
                (
                    "empty window, unsaved icon",
                    b"\x1b]1;icon\x07\x1b[22;2t\x1b]0;program\x07",
                    b"\x1b[23;2t",
                    "",
                    "program",
                ),
                (
                    "empty icon, unsaved window",
                    b"\x1b]2;window\x07\x1b[22;1t\x1b]0;program\x07",
                    b"\x1b[23;1t",
                    "program",
                    "",
                ),
                (
                    "both saved empty",
                    b"\x1b[22t\x1b]0;program\x07",
                    b"\x1b[23t",
                    "",
                    "",
                ),
                (
                    "older saved empty window",
                    b"\x1b]1;icon\x07\x1b[22;2t\x1b[22;1t\x1b]0;program\x07",
                    b"\x1b[23;2t",
                    "",
                    "program",
                ),
            ] {
                let mut live = Terminal::new(4, 20);
                live.process(prefix);
                let checkpoint = match projection {
                    "memory" => live.checkpoint(),
                    "carry" => live.checkpoint_carry(0).expect("Ground carry"),
                    "journal" => live.checkpoint_journal(0).expect("Ground journal"),
                    _ => unreachable!(),
                };
                #[cfg(feature = "serde")]
                let checkpoint = {
                    let meta = CheckpointMeta::from_checkpoint(&checkpoint);
                    let back =
                        meta.into_checkpoint(checkpoint.grid.clone(), checkpoint.alt_grid.clone());
                    assert_eq!(back, checkpoint, "{projection}/{label}: meta reassembly");
                    back
                };
                let mut adopted = Terminal::from_checkpoint(&checkpoint);
                live.process(tail);
                adopted.process(tail);
                assert_eq!(
                    (live.title(), live.icon_name()),
                    (window, icon),
                    "{projection}/{label}: live premise"
                );
                if label == "nonempty" {
                    assert_eq!(
                        (adopted.title(), adopted.icon_name()),
                        (window, icon),
                        "{projection}: positive handoff"
                    );
                }
                observed.push((
                    projection,
                    label,
                    adopted.title().to_string(),
                    adopted.icon_name().to_string(),
                ));
                expected.push((projection, label, window.to_string(), icon.to_string()));
            }
        }
        assert_eq!(
            observed, expected,
            "a checkpoint must distinguish saved empty slots from slots a push did not save"
        );
    }

    /// A carried title is OSC input, not trusted text: the restore holds it to
    /// what the OSC 0/1/2 handler would store (1024 bytes at most, no C0 but
    /// tab, no C1, no bidi override) and the stack to the engine's depth,
    /// keeping the entries a live engine keeps (its bottom ones: a push onto a
    /// full stack is dropped).
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
            ..TitleRepr::default()
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

    /// The crash-restore journal's keyframe: a terminal restored from it and fed the same
    /// bytes as the live one stays identical to it, at Ground and with a sequence split
    /// across the keyframe (the parser carry continues it); the bridge bounds the history it
    /// carries; and a hooked DCS query, whose reply no carry can hold, defers it.
    #[test]
    fn the_journal_keyframe_continues_the_stream_it_was_cut_from() {
        let tail: &[u8] = b"lor\x1b[0m after\r\nnext line\r\n";
        for (label, cut) in [
            ("ground", &b"\x1b[31mco"[..]),
            ("mid-csi", &b"\x1b[31mco\x1b[3"[..]),
            ("mid-osc", &b"\x1b]0;a title being wri"[..]),
            ("mid-utf8", &b"caf\xc3"[..]),
        ] {
            let mut live = build_rich_terminal(6, 24);
            live.process(cut);
            let keyframe = live.checkpoint_journal(512).expect("carried");
            assert_eq!(
                (keyframe.parser.is_some(), keyframe.parser_ground),
                (label != "ground", label == "ground"),
                "{label}: a split sequence rides the keyframe's parser carry"
            );
            let mut restored = Terminal::from_checkpoint(&keyframe);
            assert_eq!(
                restored.checkpoint_journal(512),
                Some(keyframe.clone()),
                "{label}: the keyframe round-trips"
            );
            live.process(tail);
            restored.process(tail);
            assert_eq!(
                restored.checkpoint_continuing().map(lineage_taken),
                live.checkpoint_continuing(),
                "{label}: the replayed tail lands where the live one did"
            );
        }

        let mut deep = Terminal::new(6, 24);
        for i in 0..40 {
            deep.process(format!("history {i}\r\n").as_bytes());
        }
        let history = deep.grid.scrollback_lines();
        assert!(history > 8, "{history}");
        assert_eq!(deep.checkpoint_journal(8).expect("ground").history_lines, 8);
        assert_eq!(
            deep.checkpoint_journal(0).expect("ground").history_lines,
            0,
            "a zero bridge is the visible screen"
        );
        assert_eq!(
            deep.checkpoint_journal(usize::MAX)
                .expect("ground")
                .history_lines as usize,
            history,
            "a bridge past the history carries all of it"
        );

        let mut query = Terminal::new(4, 20);
        query.process(b"hello\x1bP$qm");
        assert!(
            query.checkpoint_journal(512).is_none(),
            "a hooked DCS query defers the keyframe"
        );
        query.process(b"\x1b\\");
        assert!(
            query.checkpoint_journal(512).is_some(),
            "and its end allows it"
        );
    }

    /// The journal's marked keyframe: its grids are the bridge's alone, its completed
    /// marks and blocks reach `marks` rows above the screen past a shorter bridge (the
    /// history ring brings their lines back), never further, and the recovery's
    /// projection of a terminal restored from it keeps every one of them, where the
    /// carry's own floor would drop those its short history does not reach. A mark or
    /// block reaches a row when one of its rows is at or past it: one whose output
    /// runs into the horizon travels though its prompt row is older.
    #[test]
    fn the_marked_keyframe_carries_the_marks_its_bridge_does_not_reach() {
        let mut t = Terminal::new(4, 30);
        for command in 0..5u8 {
            t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07");
            for _ in 0..9 {
                t.process(b"out\r\n");
            }
            t.process(format!("\x1b]133;D;{command}\x07").as_bytes());
        }
        let top = t.grid.visible_to_absolute(0);
        // Each completed mark's prompt row and one past its last row (the D's).
        let marks: Vec<(u64, u64)> = t
            .command_marks()
            .iter()
            .map(|m| (m.prompt_start_row, m.output_end_row.expect("finished") + 1))
            .collect();
        let rows: Vec<u64> = marks.iter().map(|&(prompt, _)| prompt).collect();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|&r| r < top), "premise: all in the history");
        let reaching = |floor: u64| -> Vec<u64> {
            marks
                .iter()
                .filter(|&&(_, end)| end > floor)
                .map(|&(prompt, _)| prompt)
                .collect()
        };
        let within = |c: &TerminalCheckpoint| -> Vec<u64> {
            c.shell
                .command_marks
                .iter()
                .map(|m| m.prompt_start_row)
                .collect()
        };
        let plain = t.checkpoint_journal(0).expect("ground");
        let on_screen = reaching(top);
        assert_eq!(
            within(&plain),
            on_screen,
            "the bridge's own floor: the last command's output reaches the screen"
        );
        assert_eq!(on_screen.len(), 1, "premise: one reaches the screen");
        let horizon = 25;
        let floor = top - horizon as u64;
        let marked = t.checkpoint_journal_marked(0, horizon).expect("ground");
        let want = reaching(floor);
        assert!(
            want.len() > on_screen.len() && want.len() < rows.len(),
            "premise: the horizon splits them: {want:?} of {rows:?}"
        );
        assert!(
            want.iter().any(|&prompt| prompt < floor),
            "premise: one reaches the horizon from a prompt row older than it: {want:?}"
        );
        assert_eq!(within(&marked), want, "the horizon, and no further");
        let blocks = |c: &TerminalCheckpoint| -> Vec<u64> {
            c.shell
                .output_blocks
                .iter()
                .chain(c.shell.current_block.as_ref())
                .map(|b| b.span.prompt_start_row)
                .collect()
        };
        // An archived block's `end_row` is one past its last row; the block
        // being built always travels.
        let want_blocks: Vec<u64> = t
            .all_blocks()
            .filter(|b| b.end_row.is_none_or(|end| end > floor))
            .map(|b| b.prompt_start_row)
            .collect();
        assert!(
            want_blocks.len() > 1,
            "premise: archived blocks inside the horizon"
        );
        assert_eq!(blocks(&marked), want_blocks, "their blocks too");
        assert_eq!(
            (marked.grid.clone(), marked.history_lines),
            (plain.grid.clone(), plain.history_lines),
            "the grids are the bridge's"
        );
        assert_eq!(
            within(&t.checkpoint_journal_marked(8, 0).expect("ground")),
            within(&t.checkpoint_journal(8).expect("ground")),
            "a bridge reaching further than the horizon keeps its own"
        );

        let restored = Terminal::from_checkpoint(&marked);
        assert_eq!(
            within(&restored.checkpoint_continuing_swallowing()),
            want,
            "the recovery's projection keeps them"
        );
        assert_eq!(
            within(&restored.checkpoint_continuing().expect("ground")),
            on_screen,
            "control: the carry's floor drops what its history does not reach"
        );
    }

    /// While the alternate screen is up the journal's bridge rides the saved primary,
    /// whose blob then holds the visible rows plus the bridge; the alternate grid keeps
    /// its visible rows, and the restored terminal gets its history back on the way out.
    #[test]
    fn the_journal_bridge_rides_the_saved_primary_on_the_alternate_screen() {
        let mut live = Terminal::new(5, 20);
        for i in 0..30 {
            live.process(format!("shell {i}\r\n").as_bytes());
        }
        live.process(b"\x1b[?1049hfull screen app");
        let keyframe = live.checkpoint_journal(10).expect("ground");
        assert_eq!(
            keyframe.history_lines, 0,
            "the alternate grid has no history"
        );
        assert_eq!(deserialize_lines(&keyframe.grid).len(), 5);
        let primary = keyframe.alt_grid.as_deref().expect("the saved primary");
        assert_eq!(
            deserialize_lines(primary).len(),
            5 + 10,
            "the bridge rides the saved primary"
        );
        let mut restored = Terminal::from_checkpoint(&keyframe);
        restored.process(b"\x1b[?1049l");
        live.process(b"\x1b[?1049l");
        assert_eq!(restored.visible_content(), live.visible_content());
        assert_eq!(restored.grid.scrollback_lines(), 10);
    }

    /// The journal's keyframe never carries the shell-integration nonce or the
    /// integration revision, even while integration is authorized and in use: the
    /// journal lives in memory a keeper holds, and a restored shell re-keys. The verified
    /// mark count is what the journal reads instead: it moves only for a mark whose nonce
    /// verified.
    #[test]
    fn the_journal_keyframe_carries_no_nonce_and_marks_are_counted() {
        let key = [7u8; 32];
        let mut t = Terminal::new(4, 40);
        t.set_require_shell_integration_nonce(true);
        t.authorize_shell_integration(key);
        assert_eq!(t.shell_marks_verified(), 0);
        t.process(
            format!(
                "\x1b]133;A;id={}\x07$ ",
                ShellIntegrationNonce(key).to_hex()
            )
            .as_bytes(),
        );
        assert_eq!(t.shell_marks_verified(), 1, "a verified mark is counted");
        t.process(format!("\x1b]133;B;id={}\x07", "1".repeat(64)).as_bytes());
        assert_eq!(
            t.shell_marks_verified(),
            1,
            "a mark with the wrong nonce is dropped, not counted"
        );
        assert_eq!(t.shell_integration_dropped_count(), 1);
        assert!(
            t.checkpoint_carry(0)
                .expect("ground")
                .shell_integration_nonce
                .is_some(),
            "control: the seamless carry takes the nonce"
        );
        let keyframe = t.checkpoint_journal(512).expect("ground");
        assert_eq!(keyframe.shell_integration_nonce, None);
        assert_eq!(keyframe.shell_integration_rev, None);

        let mut open = Terminal::new(4, 40);
        open.process(b"\x1b]133;A\x07");
        assert_eq!(
            open.shell_marks_verified(),
            0,
            "with no nonce required nothing is verified (and the replay applies the mark)"
        );
    }

    /// A journal waits for a lossless cut instead of turning a partially collected
    /// sequence into one to ignore. Ordinary split sequences still continue.
    #[test]
    fn the_journal_defers_sequences_a_carry_would_only_swallow() {
        for (label, head, tail) in [
            ("ground", &b"plain"[..], &b" tail"[..]),
            ("split-csi", &b"\x1b[3"[..], &b"1mred"[..]),
            ("split-osc", &b"\x1b]0;short "[..], &b"title\x07"[..]),
            ("split-glyph", &b"caf\xc3"[..], &b"\xa9"[..]),
            (
                "ignored-dcs",
                &b"\x1bPtmux;"[..],
                &b"ignored\x1b\\after"[..],
            ),
        ] {
            let mut live = Terminal::new(4, 20);
            live.process(head);
            assert_eq!(live.partial_sequence_uncarried(), None, "{label}");
            assert_eq!(live.partial_sequence_swallowed(), None, "{label}");
            let keyframe = live.checkpoint_journal(0).expect(label);
            let mut restored = Terminal::from_checkpoint(&keyframe);
            live.process(tail);
            restored.process(tail);
            assert_eq!(
                restored.checkpoint_continuing().map(lineage_taken),
                live.checkpoint_continuing(),
                "{label}: lossless parser state remains admitted"
            );
        }

        let mut long_title = b"\x1b]0;".to_vec();
        long_title.extend(std::iter::repeat_n(b'A', aterm_parser::OSC_CARRY_CAP + 1));
        let mut long_image = b"\x1b]1337;File=inline=1:".to_vec();
        long_image.extend(std::iter::repeat_n(b'A', aterm_parser::OSC_CARRY_CAP + 1));
        let mut refusals = Vec::new();
        for (label, head) in [
            ("oversize-title", &long_title[..]),
            ("oversize-image", &long_image[..]),
            ("active-apc", &b"\x1b_Ga=T,f=100;"[..]),
            ("hooked-sixel", &b"\x1bPq#0;2;0;0;0"[..]),
        ] {
            let mut live = Terminal::new(4, 20);
            live.process(head);
            assert_eq!(live.partial_sequence_uncarried(), None, "{label}");
            assert!(
                live.partial_sequence_swallowed().is_some(),
                "{label}: the actual engine has the intended carry state"
            );
            let before = live.checkpoint_continuing();
            let refused = live.checkpoint_journal(0).is_none();
            assert_eq!(
                live.checkpoint_continuing(),
                before,
                "{label}: the capture does not mutate the live parser"
            );
            refusals.push((label, refused));
            live.process(b"\x1b\\");
            assert!(live.checkpoint_journal(0).is_some(), "{label}: ended");
        }
        assert!(
            refusals.iter().all(|(_, refused)| *refused),
            "lossy parser states must defer the journal keyframe: {refusals:?}"
        );
    }

    /// The crash restore's projection never refuses: a hooked DCS query rides as a
    /// string to swallow (its reply is lost, the rest of the string is not printed), and
    /// every other state is exactly `checkpoint_continuing`'s.
    #[test]
    fn the_swallowing_projection_carries_a_hooked_query_as_a_string_to_swallow() {
        let mut query = Terminal::new(4, 20);
        query.process(b"before\r\n\x1bP+q544e");
        assert_eq!(query.partial_sequence_uncarried(), Some("DcsPassthrough"));
        assert_eq!(
            query.checkpoint_continuing(),
            None,
            "control: refused there"
        );
        let c = query.checkpoint_continuing_swallowing();
        assert!(!c.parser_ground);
        assert_eq!(c.shell_integration_nonce, None);
        let mut restored = Terminal::from_checkpoint(&c);
        restored.process(b"\x1b\\after");
        query.process(b"\x1b\\after");
        assert_eq!(
            restored.row_text(1),
            query.row_text(1),
            "the tail is swallowed"
        );
        assert!(
            query.take_response().is_some(),
            "control: the live engine answered the query"
        );
        assert!(
            restored.take_response().is_none(),
            "the restored one cannot: its reply lived in the handler"
        );

        for head in [&b"plain"[..], b"\x1b[3", b"\x1b]0;short ", b"\x1bPtmux;"] {
            let mut t = Terminal::new(4, 20);
            t.process(head);
            assert_eq!(
                Some(t.checkpoint_continuing_swallowing()),
                t.checkpoint_continuing(),
                "{head:?}: equal wherever checkpoint_continuing answers"
            );
        }
    }

    /// The journal never cuts inside an OSC 133/633 mark past its command number, where
    /// the carry would hold the mark's payload and a signed mark's nonce with it; every
    /// other OSC, and a mark still inside its number, is cut and carried as before.
    #[test]
    fn the_journal_is_never_cut_inside_a_shell_mark() {
        let hex = "5a".repeat(32);
        for (payload, mark) in [
            (format!("133;A;id={hex}"), true),
            ("133;".to_owned(), true),
            ("0133;B".to_owned(), true),
            (format!("633;E;ls;id={hex}"), true),
            ("133".to_owned(), false),
            ("13".to_owned(), false),
            ("0;a title".to_owned(), false),
            ("1337;SetMark".to_owned(), false),
            ("7;file:///tmp".to_owned(), false),
        ] {
            let mut t = Terminal::new(4, 80);
            t.process(format!("$ \x1b]{payload}").as_bytes());
            assert_eq!(t.partial_sequence_is_shell_mark(), mark, "{payload}");
            assert_eq!(t.checkpoint_journal(8).is_none(), mark, "{payload}");
            let carried = t.checkpoint_continuing().expect("an OSC is carried");
            assert_eq!(carried.parser_holds_shell_mark(), mark, "{payload}");
            t.process(b"\x07");
            assert!(!t.partial_sequence_is_shell_mark(), "{payload}: ended");
            assert!(t.checkpoint_journal(8).is_some(), "{payload}: ended");
        }
    }
}

#[cfg(test)]
#[path = "replay_fork_tests.rs"]
mod replay_fork_tests;
