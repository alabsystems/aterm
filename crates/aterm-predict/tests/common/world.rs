// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WORLD the safety theorem runs (`tests/reconciled_view_safety.rs`, which
//! states the theorem and holds the negative controls): a deterministic
//! simulation of a real `aterm-core` engine, a far-end program, a host wired the
//! way `aterm-gui` wires the predictor, and the clock, with the checker of every
//! clause. Its own file so another harness can run the same world (the
//! measurement of what prediction buys runs it under its own corpus).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use aterm_core::grid::LineSize;
use aterm_core::render::{RenderInput, SpanCell};
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::{
    EditKey, EditOutcome, EditorContext, GhostCell, InputStart, KeySample, PredictMode, Predictor,
    Rule, Seen,
};
use aterm_spec::rng::XorShift;

/// A small screen, so lines wrap and the bottom row scrolls within a few keys.
pub(crate) const ROWS: u16 = 6;
pub(crate) const COLS: u16 = 12;
/// The geometry a host resize toggles to (and back).
pub(crate) const ALT_GEOMETRY: (u16, u16) = (5, 14);
/// The crate's documented ceiling on the glitch window ("floor 250 ms, ceiling
/// 2 s"), plus one tick: past it every guess has either retired or expired.
pub(crate) const SETTLE: Duration = Duration::from_millis(2001);
/// The crate's documented FLOOR of the glitch window: no echo deadline the
/// predictor uses is shorter, so output that arrives within it is never "late".
/// The link model ([`Sim::late_predecessor`]) scales it up to the predictor's own
/// window when that is longer.
pub(crate) const ECHO_DEADLINE: Duration = Duration::from_millis(250);
/// The shell prompt the line editor prints.
pub(crate) const PROMPT: &str = "$ ";

/// The prompt a shell's `reset-prompt` redraws in [`PROMPT`]'s place and back
/// ([`Unprompted::ResetPrompt`]): another width, so the buffer moves.
pub(crate) const WIDE_PROMPT: &str = "$$$ ";
/// The right prompt the line editor draws with each prompt under
/// [`Config::rprompt`], zsh's RPROMPT: at the right edge, one column short of
/// it (zsh's `ZLE_RPROMPT_INDENT`), in italics, which is how this file tags a
/// cell the far end draws as DECORATION (see FRAME FIDELITY).
pub(crate) const RPROMPT: &str = "<r>";
/// The glyphs a press draws from (`Sim::key_for`): letters, space, an accent, a
/// wide and a combining glyph.
pub(crate) const TEXT: [char; 14] = [
    'a', 'b', 'c', 'd', 'e', 'i', 'n', 'o', 's', ' ', ' ', 'é', '日', '\u{301}',
];
/// The right-to-left letter a press now and then types under
/// [`Config::bidi`]: Hebrew alef, one cell wide, which the engine's implicit
/// BiDi draws in another order beside another right-to-left letter.
pub(crate) const RTL: char = '\u{5d0}';

pub(crate) type KeyId = usize;

/// A press the host sees.
#[derive(Clone, Debug)]
pub(crate) enum Key {
    Glyph(char),
    /// Backspace: an edit key, handed to `Predictor::predict_edit`.
    Backspace,
    /// A bare main-block arrow: an edit key.
    Left,
    Right,
    Enter,
    /// Home, End, Ctrl-A, Ctrl-E or Delete: a LEARNED key, handed to
    /// `Predictor::predict_edit` with the shell's authenticated buffer start
    /// (`Terminal::input_region_start`); the far end does what its keymap binds
    /// it to ([`Keymap`]).
    Learned(EditKey),
    Unmodelled(Unmodelled),
    Paste(Vec<char>),
}

/// The non-glyph presses the predictor does not model: they keep today's routing
/// (`note_unmodelled_input`), and the far end implements each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unmodelled {
    Tab,
    KillLine,
    KillWord,
    Interrupt,
    /// Alt-B: back to the start of the word (a word motion is not an arrow).
    WordLeft,
    /// Up: the previous command from history, replacing the line.
    Up,
    /// Escape alone: a prefix the editor waits on, nothing drawn.
    Esc,
    /// The keypad's Left while a Kitty mode that reports the keypad is on
    /// (DISAMBIGUATE: `CSI 57417 u`): no motion; readline and zle leave it
    /// unbound. Under the legacy encoder the keypad's Left sends the main
    /// block's `ESC [ D` and is [`Key::Left`].
    KeypadLeft,
    /// zsh's push-line (`ESC q`): the buffer goes onto the buffer stack, the
    /// line is cleared, and the prompt after the next command pops it pre-filled
    /// (`Program::push_line`). Under readline the pre-filled prompt is bash's
    /// operate-and-get-next's, whose `B` is drawn before the line.
    PushLine,
    /// Ctrl-K, kill-line: the buffer from the cursor to its end is erased
    /// (`Program::kill_to_end`). The theorem's generator never presses it
    /// (`Sim::key_for`); the measurement's corpus does (`tests/editing_win.rs`,
    /// a line retyped after Ctrl-A Ctrl-K).
    KillToEnd,
}

/// Output the program produced with nobody typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unprompted {
    Notice,
    Redraw,
    RightPrompt,
    Suggest,
    Clear,
    Jump,
    StatusLine,
    InsertLine,
    DeleteLine,
    ScrollUp,
    ReverseIndex,
    EnterAlt,
    LeaveAlt,
    PasswordNewRow,
    PasswordSameRow,
    SilentNoEcho,
    EchoOn,
    Ris,
    /// The shell re-executes itself with the next [`Keymap`] (`exec zsh` with
    /// another setup), the host re-keys its integration nonce for it, and it
    /// draws a fresh prompt signed with the new one: a new editor context.
    Rebind,
    /// A program prints an OSC 133 `A` and `B` with no nonce at the cursor (a
    /// `cat` of a log that holds them): the nonce gate drops both.
    ForgedMark,
    /// The shell pushes Kitty `DISAMBIGUATE` (fish 4 does at its prompt) or
    /// modifyOtherKeys level 2 (a precmd's `CSI > 4;2 m`, or a program's left
    /// behind), or pops what it pushed: Ctrl-A then arrives as `CSI 97;5u` or
    /// `CSI 27;5;97~`, which no keymap here binds.
    KittyChords,
    /// zsh's `reset-prompt` (an async prompt segment resolving, a vi-mode
    /// indicator): the prompt redrawn in place, [`PROMPT`] and [`WIDE_PROMPT`]
    /// in turn, the buffer after it. zle-line-init does not run, so no new `B`
    /// is sent: the engine keeps the old one where it stood.
    ResetPrompt,
}

pub(crate) const UNPROMPTED: [Unprompted; 19] = [
    Unprompted::ResetPrompt,
    Unprompted::Notice,
    Unprompted::Redraw,
    Unprompted::RightPrompt,
    Unprompted::Suggest,
    Unprompted::Clear,
    Unprompted::Jump,
    Unprompted::StatusLine,
    Unprompted::InsertLine,
    Unprompted::DeleteLine,
    Unprompted::ScrollUp,
    Unprompted::ReverseIndex,
    Unprompted::EnterAlt,
    Unprompted::LeaveAlt,
    Unprompted::PasswordNewRow,
    Unprompted::PasswordSameRow,
    Unprompted::SilentNoEcho,
    Unprompted::EchoOn,
    Unprompted::Ris,
];

/// One step of a run. State-dependent choices (which alphabet a glyph comes from,
/// what a command prints) are resolved when the op EXECUTES, from its own
/// parameter, so any sub-list of ops is still a well-defined run: that is what
/// lets a failure shrink.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    /// A press: `.0` picks the class, `.1` the detail.
    Key(u32, u32),
    /// The program reads up to `.0` queued keys; `.1` decides what a command prints.
    Program(u8, u32),
    /// Deliver `.0`/1024 of the output in flight (at least one byte; 1024 = all).
    Deliver(u16),
    /// Unprompted program output.
    Unprompted(Unprompted, u32),
    /// The clock moves (ms). A due expiry deadline wakes the host, as the GUI's
    /// deadline sweep does.
    Advance(u32),
    /// A redraw with no new output.
    Frame,
    /// The host changes the coordinate space: `true` resizes, `false` swaps the
    /// front pane away and back (the link estimate is kept, the screen is not).
    HostReset(bool),
}

/// How the host is wired. Only `Faithful` is aterm; the others are the negative
/// controls the checker must catch, each with the clause it breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    Faithful,
    /// Hands the predictor a generation that never moves (PROVENANCE).
    FrozenCoordinates,
    /// Never reports the alternate screen, at a key or at a frame (PROVENANCE).
    BlindToAltScreen,
    /// Paints each ghost one cell right of its guess (LOCALITY).
    PaintsBesideTheGuess,
    /// The ghost painter as it was before it refused wide glyphs: the guess's
    /// glyph written over the cell, its wide flags cleared (LOCALITY).
    PaintsOverWideGlyphs,
    /// Displays Adaptive's guesses as `Always` does (SECRECY).
    ForcesAlways,
    /// Never tells the predictor about a line terminator (FIDELITY).
    IgnoresLineSubmits,
    /// Never tells the predictor the terminal's ambiguous width, so under
    /// `ambiguous_width = "wide"` it guesses `é` one cell where the echo takes
    /// two (FIDELITY; run with [`Config::wide`]).
    IgnoresAmbiguousWidth,
    /// Reads the row as blank from the cursor rightward: right of it in the
    /// press's row sample, from it (the first tail cell, once an echo moved the
    /// cursor there) in the frame's observe. The tail guard sees no tail, so a
    /// mid-line echo proves its epoch (FRAME FIDELITY).
    TailBlind,
    /// Skips the erase frame: when the overlay comes back empty it leaves the
    /// last frame's ghosts on glass, as a host with no `pred_shown` would after a
    /// flush or an expiry (TIMELINESS, its "no longer pending" half).
    KeepsStaleGhosts,
    /// Reads its clock only at a keypress: every frame-time call (the reconcile,
    /// the overlay, the deadline sweep) gets the last press's instant, so nothing
    /// pending ever ages past its window and an unechoed guess stays on glass
    /// until the next key (TIMELINESS, its window half).
    StaleClock,
    /// Hands Alt-B, a word motion, to the predictor as a bare `←` (FRAME
    /// FIDELITY: the far end goes back a word, the frame a cell).
    WordMotionAsArrow,
    /// Hands the keypad's Left, which Kitty DISAMBIGUATE sends as
    /// `CSI 57417 u`, to the predictor as `←` (FRAME FIDELITY: the far end does
    /// not move).
    KeypadArrowAsArrow,
    /// Draws a state chain's cursor as it draws the glyph lane's, one past the
    /// newest ghost it painted, instead of the chain's own (FRAME FIDELITY: a
    /// chain ending in a motion leaves the cursor elsewhere).
    CursorAfterNewestGlyph,
    /// Paints a state chain's ghosts one by one, each the painter refuses left
    /// out, and draws the chain's cursor regardless (ATOMICITY).
    PaintsPartialState,
    /// Draws a state chain's cursor without asking about its own cell (CURSOR
    /// LOCALITY: a cursor on a wide glyph's right half).
    CursorOnAContinuation,
    /// Draws a state chain's cursor whether or not the span painter took its
    /// cells (ATOMICITY: a state's cursor over a frame that shows another
    /// state's cells).
    CursorWithoutSpan,
    /// Paints a shift's span on to the margin, the rest of the row moved with
    /// the text the predictor knows, as ICH would move it (LOCALITY: a right
    /// prompt or a suggestion past the line, which the far end leaves or
    /// clears, moved on glass).
    PaintsPastTheLine,
    /// Remembers the cell of the last input start it read, not which prompt it
    /// was, and hands it on whatever happened since, a command started or the
    /// rows moved, as a prompt no line terminator was pressed at (FRAME
    /// FIDELITY: a program's own prompt where the shell's stood is taken for
    /// the shell's buffer, and Home there for beginning-of-line). Handed with
    /// the prompt it was read at, the first-key gate refuses it: a terminator
    /// pressed at that prompt.
    StaleInputStart,
    /// Takes the input start from any OSC 133 `B` the engine kept, and the
    /// prompt a line terminator is pressed at from any `A`, with the
    /// capability-nonce gate never turned on (FRAME FIDELITY: a mark a
    /// program printed at the end of a pre-filled line, saying it stands where
    /// the buffer starts, is taken for the buffer's start).
    UnsignedInputStart,
    /// Takes the input start from the signed `B` of the prompt in front without
    /// asking whether it says it stands where the buffer starts (FRAME
    /// FIDELITY: zsh's `B` after a line pre-filled from its buffer stack is
    /// taken for the buffer's start, and Home there for beginning-of-line).
    UnattestedInputStart,
    /// Hands the predictor one editor context forever (FRAME FIDELITY: the
    /// bindings one shell taught are used in the next, which binds otherwise).
    StaleEditorContext,
    /// Never asks the terminal's Kitty keyboard mode, neither for R-KITTY nor
    /// for the editor context (FRAME FIDELITY: Ctrl-A goes out as `CSI 97;5u`,
    /// which the far end does not bind, and is shown as beginning-of-line).
    IgnoresKittyChords,
    /// Asks only Kitty's keyboard mode whether control chords go out escaped,
    /// for R-KITTY and the editor context (FRAME FIDELITY: under
    /// modifyOtherKeys level 2 Ctrl-A goes out as `CSI 27;5;97~`, which the far
    /// end does not bind, and is shown as beginning-of-line).
    IgnoresModifyOtherKeys,
    /// aterm's wiring around a predictor with one [`Rule`] switched off
    /// (`Predictor::disable_rule_for_test`): the clause that rule keeps.
    Rule(Rule),
}

impl Host {
    /// The clause this host breaks, or `None` for aterm's own wiring.
    pub(crate) fn breaks(self) -> Option<&'static str> {
        match self {
            Self::Faithful => None,
            Self::FrozenCoordinates | Self::BlindToAltScreen => Some("provenance"),
            Self::PaintsBesideTheGuess | Self::PaintsOverWideGlyphs | Self::PaintsPastTheLine => {
                Some("locality")
            }
            Self::ForcesAlways => Some("secrecy"),
            Self::IgnoresLineSubmits | Self::IgnoresAmbiguousWidth => Some("fidelity"),
            Self::TailBlind
            | Self::WordMotionAsArrow
            | Self::KeypadArrowAsArrow
            | Self::CursorAfterNewestGlyph
            | Self::StaleInputStart
            | Self::UnsignedInputStart
            | Self::UnattestedInputStart
            | Self::StaleEditorContext
            | Self::IgnoresKittyChords
            | Self::IgnoresModifyOtherKeys => Some("frame-fidelity"),
            Self::KeepsStaleGhosts | Self::StaleClock => Some("timeliness"),
            Self::PaintsPartialState | Self::CursorWithoutSpan => Some("atomicity"),
            Self::CursorOnAContinuation => Some("cursor-locality"),
            Self::Rule(rule) => rule_clause(rule),
        }
    }

    /// The ambiguous-width policy the mutant is run under: the wide one for the
    /// host that forgets it (the narrow policy is the predictor's default, so
    /// forgetting it there changes nothing), the narrow one for the rest.
    pub(crate) fn wide(self) -> bool {
        self == Self::IgnoresAmbiguousWidth
    }
}

/// The mutants the seeded sweep catches, each in its clause
/// (`the_checker_catches_every_mutant_host_in_its_clause`). The rest are caught
/// at their named cases only: `TailBlind` at `mid_line_over_a_space` (the
/// sweep's case that caught it moved when increment 4 put the integration
/// in the world), and the input-start, Kitty and end-of-line mutants at the
/// learned keys' cases, whose interleavings (a forged mark at the cell a line
/// then starts on, a program's prompt where the shell's stood, a learned
/// chord under a pushed Kitty mode or modifyOtherKeys, a suggestion at the end
/// of a burst, a line pre-filled from zsh's buffer stack) the sweep does not
/// reach on its seed. `StaleEditorContext` and `WitnessFreshPrompt` switched
/// off left the sweep when the first-key gate and the push-line and
/// reset-prompt motifs changed which lines it witnesses; each is caught at its
/// named case (`a_new_shell_relearns_its_bindings`,
/// `home_after_a_tab_completion_is_not_predicted`).
pub(crate) const MUTANT_HOSTS: [Host; 20] = [
    Host::FrozenCoordinates,
    Host::BlindToAltScreen,
    Host::PaintsBesideTheGuess,
    Host::PaintsOverWideGlyphs,
    Host::ForcesAlways,
    Host::IgnoresLineSubmits,
    Host::IgnoresAmbiguousWidth,
    Host::KeepsStaleGhosts,
    Host::StaleClock,
    Host::Rule(Rule::ExpiryOwes),
    Host::Rule(Rule::TailGuard),
    Host::CursorAfterNewestGlyph,
    Host::Rule(Rule::SmallestMatch),
    Host::Rule(Rule::NetNullUncredited),
    Host::Rule(Rule::MotionClassProof),
    Host::Rule(Rule::StateExpiryFlushes),
    Host::Rule(Rule::CarryCursor),
    Host::Rule(Rule::InsertShiftProof),
    Host::Rule(Rule::EraseShiftProof),
    Host::Rule(Rule::LearnedBindings),
];

/// The clause a predictor rule keeps, which a run with it switched off must
/// break (`every_rule_has_a_negative_control` holds each to one, in the sweep or
/// at a named case). The match is exhaustive and the rules are walked from
/// `Rule::ALL`, which the predictor derives from the enum itself, so a rule
/// added there fails to compile here until it names its clause. `None` for a
/// rule with no safety consequence in this world, pinned by `aterm-predict`'s
/// unit tests instead:
/// * `TypedOwes` off is the shipped cascade (every uncredited retirement owed
///   as any echo): it only refuses more credit;
/// * `TypedOwesCleanTail` off takes a typed owe over a moved tail, where a
///   shifted tail glyph could stand in a later guess's cell. Within one row the
///   cursor check flushes the guesses behind such a cell (the cursor still
///   stands on it), and the sixty-seed sweep, rewriting far ends included, finds
///   no counterexample.
/// * `VacatedBlankInMatch` off matches an erase's state while the erased glyph
///   still stands on its vacated cell, part-way through the erase's echo. That
///   state is the far end's own transit, a key whose output has begun: no frame
///   on a settled base shows it, and the credit is the key's own.
/// * `MotionsNeverProve` off lets a credited motion re-prove its epoch. An edit
///   key is armed only on a proven line, so the proof must have been voided while
///   the motion was in flight, and in this world only the standing tail does that
///   without ending the epoch or flushing the chain (a right prompt erased), which
///   leaves a line that still echoes.
/// * `VacateRefusal` off arms an erase beside text past the end of the line. In
///   this world that text is decoration (a suggestion, a right prompt), which an
///   erase leaves, clears or fetches anew into the vacated cell
///   (`Editor::Autosuggest`), and the decoration rule counts each
///   (`late_decoration`: the vacated blank under the suggestion fetched there);
///   an unseen buffer tail it would pull left needs a line known only up to the
///   middle of its buffer, which no line here is (the known line grows only from
///   proved appends and states the whole screen matched).
///   Two hundred seeds with it off find none, once a Backspace beside an unproven
///   shift no longer cancels its glyph (`CancelsOnlyKnownShifts`, whose break this
///   rule had masked at seed index 152).
/// * `MidLineDeclineFlushes` off declines a glyph typed inside the text that a
///   chain cannot place as a glyph at the end of the line is declined, its
///   pixels kept. The far end reads it after every op in flight, so each op's
///   output settles before its echo moves the tail, and the frames showing them
///   are held to those settled screens; a frame caught with its output landed
///   finds an opaque wide glyph or a state no longer matched, and flushes. The
///   flush stays as the fail-closed choice (a frame between two pieces of that
///   output is no settled screen), pinned by the predictor's unit test and the
///   named case `a_wide_glyph_mid_line_retires_the_tail`.
///   Two hundred seeds with it off find none.
/// * `EditOpsOwe` off owes nothing for a declined arrow or a flushed motion. An
///   arrow moves only the cursor, and a guess typed after one with nothing
///   placed is a glyph-lane guess at the live cursor whose proof the tail guard
///   and the cursor advance already refuse; the sweep finds no counterexample.
/// * `InsideRunsNeverProve` off lets a glyph-lane run started inside the known
///   text prove as the tail guard reads it. Since a glyph typed inside the text
///   is a shift of the edit lane, such a run is one the lane refused, and in
///   this twelve-column world that is R-ROW's refusal: the line's end, one
///   cell on, at the last column. The glyph typed after the run's echo then
///   pushes the tail past the margin, which leaves the editor's model, and the
///   frame showing it is out of FRAME FIDELITY's scope. The unit test
///   `a_run_started_inside_the_text_proves_nothing` holds the rule to the frame
///   between its glyph and its tail redraw, where the proof is earned.
///   Two hundred seeds with it off find none.
/// * `CancelOwesItsBlank` off credits a space typed on a cancelled cell with
///   the erase's blank, caught part-way (`\b ` landed, the step back not),
///   a credit ATTRIBUTION leaves out while the erase's own output is in
///   transit; the step back onto that cell then takes the credit back
///   (`CreditTakenBack`, the review of increment 4), before the space typed
///   next can be credited with the first one's real echo, which is what its
///   named case `a_cancelled_glyph_s_erase_is_no_space_s_echo` found. The unit
///   test of the same name holds it to the part-way frame.
/// * `CancelsOnlyKnownShifts` off pops a glyph typed inside the text with the
///   Backspace after it, as a pair that cancels, while overwrite mode replaced
///   a glyph with it and rubbed it out: the line lost a glyph its states still
///   hold. Once that output lands its cell reads blank (or the tail moved onto
///   it), and the transit wait now takes a blank only where an op of the chain
///   writes one (`TransitBlanks`, the review of increment 5): the `←` left in
///   flight writes none, so the chain is flushed before any frame on a settled
///   base shows it, which is what its named case
///   `a_backspace_cancels_no_glyph_of_an_unproven_shift` found. The unit test of
///   the same name holds it with `TransitBlanks` off too.
/// * `ScrollKeepsFirstKey` off shuts THE FIRST KEY's gate at a coordinate move,
///   as the reset it runs does: fewer lines are witnessed, and nothing more is
///   shown. The measurement of what predictive editing buys
///   (`tests/editing_win.rs`) counts the lines witnessed with it on and off.
/// * `ScreenAsItWas` off flushes a head guess whose cell still shows the glyph
///   its press read there, as the predictor did before the review of phase 0:
///   a glyph typed over text goes at the first frame after its press, and
///   nothing more is shown. The measurement's `Baseline` switches it off with
///   every other rule, which is what makes that variant the predictor before
///   the plan; its unit test holds the wait and the flush.
/// * `DarkOverText` off paints a guess typed over the line's text where the
///   line's end is not known, unshifted, as the predictor did before the
///   measurement found what that cost `Always`: the head waiting on the
///   screen as it was, and what is typed behind a head that hides nothing.
///   On, less is shown, so it keeps no clause: the measurement runs it off
///   beside `Full`'s `Always` runs (`PaintedOverText`) and counts the cells it
///   costs, and its unit tests hold the glass with it on and off.
pub(crate) fn rule_clause(rule: Rule) -> Option<&'static str> {
    match rule {
        Rule::ExpiryOwes
        | Rule::EngineWrapFlag
        | Rule::CancelTransit
        | Rule::UncreditedVoids
        | Rule::StaleAnchor
        | Rule::StaleCancel
        | Rule::CursorReach => Some("fidelity"),
        Rule::TypedOwesSameGlyph
        | Rule::CursorAdvance
        | Rule::CancelOwes
        | Rule::SmallestMatch
        | Rule::NetNullUncredited
        | Rule::ParkedAtTheEnd
        | Rule::ChainAttributedWhole
        | Rule::EditOwedAfter
        | Rule::InPlaceReadsFewer
        | Rule::PartWayEchoes
        | Rule::PartWayStepBack => Some("attribution"),
        Rule::TailGuard
        | Rule::StandingTail
        | Rule::MotionClassProof
        | Rule::ProofsEndWithTheEpoch
        | Rule::TextEdges
        | Rule::CarryCursor
        | Rule::MarkedEcho
        | Rule::InsertShiftProof
        | Rule::EraseShiftProof
        | Rule::ShiftProofDistinguishes
        | Rule::RightToLeftRows
        | Rule::WitnessFreshPrompt
        | Rule::WitnessFirstKey
        | Rule::LearnedBindings
        | Rule::EndRefusal
        | Rule::OpaqueHead
        | Rule::TransitBlanks
        | Rule::CleanTailInFlight => Some("frame-fidelity"),
        Rule::WholeStateMatch => Some("provenance"),
        Rule::TransitReach
        | Rule::SpaceNeverProves
        | Rule::StandingGlyph
        | Rule::DarkBehindAWait
        | Rule::FloorBelow => Some("secrecy"),
        Rule::CreditTakenBack => Some("attribution"),
        Rule::StateExpiryFlushes => Some("timeliness"),
        Rule::TypedOwes
        | Rule::TypedOwesCleanTail
        | Rule::VacatedBlankInMatch
        | Rule::MotionsNeverProve
        | Rule::VacateRefusal
        | Rule::EditOpsOwe
        | Rule::MidLineDeclineFlushes
        | Rule::InsideRunsNeverProve
        | Rule::CancelOwesItsBlank
        | Rule::CancelsOnlyKnownShifts
        | Rule::ScrollKeepsFirstKey
        | Rule::ScreenAsItWas
        | Rule::DarkOverText => None,
    }
}

/// One configuration of the world.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    pub(crate) mode: PredictMode,
    /// The program is behind a raw-mode link (ssh): the host cannot read its
    /// termios, so no-echo is left to the predictor's epochs.
    pub(crate) remote: bool,
    pub(crate) host: Host,
    /// The line editor moves to the next row itself after filling the last column
    /// (`Program::forced_wrap`) rather than leaving the terminal parked.
    pub(crate) forced_wrap: bool,
    /// `ambiguous_width = "wide"`: the engine writes East Asian Ambiguous glyphs
    /// (`é`) two cells wide, the far end measures them so, and the host tells
    /// the predictor at every press (`set_ambiguous_width_double`).
    pub(crate) wide: bool,
    /// The line editor draws [`RPROMPT`] with each prompt and erases it when the
    /// line would reach it (zsh's behaviour): DECORATION that a right-hand tail
    /// snapshot must keep proving through, and that a frame may show stale.
    pub(crate) rprompt: bool,
    /// The far end's line editor at the prompt, in the byte forms each sends
    /// ([`Editor`]).
    pub(crate) editor: Editor,
    /// Now and then a glyph typed is a right-to-left letter (`א`), which the
    /// engine's implicit BiDi draws in another order: a row a state chain can
    /// name no cell of (R-OPAQUE).
    pub(crate) bidi: bool,
    /// aterm's shell integration, as aterm-gui runs it for a shell it spawned:
    /// the host authorizes a capability nonce and requires it (as
    /// `launch_witness.rs` does), and the far end signs its OSC 133 `A`, `B`,
    /// `C` and `D` marks with it. The host then hands the predictor the
    /// authenticated buffer start (`Terminal::input_region_start`), and Home,
    /// End, Ctrl-A, Ctrl-E and Delete are learned keys; without it they stay
    /// input the predictor does not model.
    pub(crate) integration: bool,
    /// What the far end's line editor binds Home, End, Ctrl-A, Ctrl-E and
    /// Delete to ([`Keymap`]); [`Unprompted::Rebind`] moves to the next.
    pub(crate) keymap: Keymap,
}

/// What a line editor binds the learned keys to, each as measured (zsh 5.9
/// `-f` and with aterm's integration, its vi insert mode; bash 3.2.57's
/// readline). The predictor assumes none of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Keymap {
    /// readline, and zsh with aterm's integration (which binds Home, End and
    /// Delete in the keymap it loads into): Home and Ctrl-A beginning-of-line,
    /// End and Ctrl-E end-of-line, Delete delete-char (a beep at the end).
    Canonical,
    /// `zsh -f`: Home and End are undefined keys (a beep, nothing moves), and
    /// `ESC [ 3 ~` reads as an undefined prefix and a `~`, which is inserted;
    /// Ctrl-A and Ctrl-E are beginning- and end-of-line.
    ZshVanilla,
    /// zsh's vi insert mode: Ctrl-A and Ctrl-E self-insert, drawn `^A` and `^E`
    /// (two cells each); Home, End and Delete as the integration binds them.
    ViIns,
}

/// The keymaps [`Unprompted::Rebind`] cycles through, and a sweep rotates.
pub(crate) const KEYMAPS: [Keymap; 3] = [Keymap::Canonical, Keymap::ZshVanilla, Keymap::ViIns];

/// The session id the world's host hands the predictor's editor context: one
/// session, swapped away and back by a host reset.
pub(crate) const SESSION: u64 = 1;

/// The capability nonce the host authorizes for the `n`-th shell of a case
/// (each [`Unprompted::Rebind`] re-keys).
pub(crate) fn nonce(n: u8) -> [u8; 32] {
    [0xA0 ^ n; 32]
}

/// The far end's line editor, each in the byte forms measured from it (bash
/// 3.2.57's readline and zsh 5.9's zle under `script(1)`, one key every 250 ms,
/// `TERM=xterm-256color`; macOS 27's tty). Every one draws what it reads the
/// same way at the end of the line; they differ in the middle and at the edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Editor {
    /// readline: a mid-line insert writes the glyph, then the tail it shifts,
    /// then steps back over that tail (`Xcd\b\b`); `→` is `ESC [ C`; an
    /// arrow or Backspace at an edge beeps. Delivered in pieces, a frame can fall
    /// between a mid-line glyph and the redraw of its tail, the row right of it
    /// still as the press saw it (THE STANDING TAIL's case).
    Readline,
    /// readline on a terminal with ICH: a mid-line insert opens the gap first
    /// (`ESC [ 1 @`, then the glyph), and `→` reprints the glyph it steps over
    /// (readline's older form).
    ReadlineIch,
    /// zle: inserts as readline does, `→` is `ESC [ 1 C`, an erase at the end
    /// is `\b \b`, and an edge is silent.
    Zle,
    /// A cooked tty (ICANON, ECHO, ECHOE, ECHOCTL) under a program reading
    /// lines: a glyph is echoed and appended, an arrow echoes its sequence
    /// (`^[[D`, four cells, three characters into the line), a Backspace
    /// erases the line's last character (`\b \b`, twice for a `^X` cell pair).
    /// No motion is ever exact here, so no motion may ever show.
    Cooked,
    /// readline in overwrite mode (`overwrite-mode`, bound to Insert): a glyph
    /// typed inside the line REPLACES the glyph under the cursor, written in
    /// place; a Backspace inside it blanks the glyph before the cursor (`\b
    /// \b`) and moves nothing; at the end of the line both act as in insert
    /// mode. Motions and edges are readline's. No shift is ever exact here, so
    /// no shift may ever show.
    Overwrite,
    /// zle with an autosuggestion plugin (zsh-autosuggestions' behaviour): a
    /// suggestion is drawn dim after the buffer, from history, after an append
    /// whose glyph does not continue it; a glyph that does continue it
    /// consumes its first cell; any edit inside the buffer clears it and now
    /// and then finds one for the new buffer, drawn after its new end (it
    /// moves with the buffer's end as a shifted tail does); an erase at the end
    /// fetches it anew (into the cell the erase vacated); `→` at the end of the
    /// buffer, or End, ACCEPTS it into the buffer. The suggestion
    /// is DECORATION, which moves and changes as the far end pleases.
    Autosuggest,
}

/// The editors a sweep rotates through.
pub(crate) const EDITORS: [Editor; 6] = [
    Editor::ReadlineIch,
    Editor::Readline,
    Editor::Zle,
    Editor::Cooked,
    Editor::Overwrite,
    Editor::Autosuggest,
];

/// A coordinate space: a content generation and a screen epoch (bumped by every
/// alt-screen switch and host reset).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Space {
    generation: u64,
    screen: u64,
}

/// The cell a guess was armed at, and in which space.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Armed {
    row: u16,
    col: u16,
    ch: char,
    space: Space,
}

/// Where a guess was displayed: its cell, its space, and how much unprompted
/// output had reached the screen by then.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shown {
    row: u16,
    col: u16,
    space: Space,
    unprompted: u64,
}

/// Everything the run learned about one keystroke.
#[derive(Clone, Debug)]
pub(crate) struct KeyLog {
    key: Key,
    typed_at: Instant,
    armed: Option<Armed>,
    /// Typed while the program visibly sat at a no-echo prompt.
    secret: bool,
    shown: Option<Shown>,
    /// The line editor knew its column exactly when it read this key.
    consumed_reliable: bool,
    /// When the last byte of output this key caused reached the screen.
    last_output_at: Option<Instant>,
    /// The predictor's glitch window when that byte arrived: the deadline the
    /// link model holds this key's output to.
    last_output_window: Duration,
    /// Every distinct frame (Adaptive) whose newest painted ghost was this key's,
    /// still to be compared with the screen this key's output settles to
    /// (FRAME FIDELITY, `Sim::settled`).
    frames: Vec<FrameShown>,
    /// This press certainly ended the predictor's epoch: a terminator, a key or
    /// paste it does not model, a glyph it did not arm, a Backspace that
    /// cancelled nothing of ours. (A press that may not have ended it, an armed
    /// glyph or a Backspace popping our own guess, is never counted: the link
    /// model would claim too much behind it.)
    ends_epoch: bool,
    /// The press order the predictor gave this key's guess, if it armed one:
    /// how a credit (`Predictor::take_credited_for_test`) names it.
    seq: Option<u64>,
    /// This key's echo glyph has reached the screen (ATTRIBUTION).
    echo_landed: bool,
    /// Unprompted bytes delivered when it was pressed: output nobody typed for
    /// since then takes a credit out of ATTRIBUTION's scope.
    unprompted_at_press: u64,
    /// The edit key the host handed the predictor for this press, if any: what
    /// the PROVENANCE oracle folds it as.
    edit: Option<EditKey>,
    /// How Ctrl-A or Ctrl-E went out: under the keyboard mode the terminal
    /// had at the press (its C0 byte for every other key).
    chord: ChordForm,
}

/// How a control chord went out ([`KeyLog::chord`]), as the terminal's keyboard
/// mode at its press encoded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChordForm {
    /// Its C0 byte: `SOH` for Ctrl-A.
    Byte,
    /// A Kitty mode that reports functional keys: `CSI 97;5u`.
    Kitty,
    /// xterm's modifyOtherKeys at level 2: `CSI 27;5;97~`.
    ModifyOtherKeys,
}

impl ChordForm {
    /// How a chord goes out under `term`'s keyboard mode now.
    fn of(term: &Terminal) -> Self {
        if term.kitty_reports_functional_keys() {
            Self::Kitty
        } else if term.control_chords_escaped() {
            Self::ModifyOtherKeys
        } else {
            Self::Byte
        }
    }
}

/// The cursor a state chain's frame carries: the predictor's, and the one the
/// host drew (`None`: the engine's).
pub(crate) type ChainCursors = (Option<(u16, u16)>, Option<(u16, u16)>);

/// One cell's text as a frame shows it: the glyph, whether it is a wide
/// glyph's right half, and its grapheme cluster or combining marks.
pub(crate) type CellText = (char, bool, Option<Box<str>>);

/// A displayed frame, kept for FRAME FIDELITY: its text and drawn cursor, which
/// of its cells are ghosts and which of the screen under them were decoration,
/// and the scope it was displayed in.
#[derive(Clone, Debug)]
pub(crate) struct FrameShown {
    text: Vec<Vec<CellText>>,
    /// Cells the far end drew as decoration in the screen under the ghosts.
    decoration: Vec<Vec<bool>>,
    /// Frame cells a ghost was painted on.
    ghosts: Vec<(usize, usize)>,
    /// The ghosts that painted a BLANK over a cell the screen under them showed
    /// text on: a state chain's vacated cells, an erase's effect, no glyph.
    vacated: Vec<(usize, usize)>,
    /// The cursor the GUI draws one past the newest painted ghost; `None` at the
    /// right margin, where it clamps the column and the drawn cursor is a guess
    /// about the far end's wrap policy, not a claim.
    cursor: Option<(u16, u16)>,
    space: Space,
    unprompted: u64,
    /// The oldest pending guess's key at this frame.
    oldest: KeyId,
    /// What learned keys the frame shows (a state chain holding one).
    learned: LearnedShown,
}

/// Whether a displayed frame shows a learned key's op (Home, End, Ctrl-A,
/// Ctrl-E, Delete), and whether one of them is a Delete: FRAME FIDELITY's
/// comparisons of such frames are counted apart (`Stats::learned_fidelity_checks`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LearnedShown {
    any: bool,
    delete: bool,
}

/// One segment of program output: what it is, and which key caused it.
#[derive(Clone, Debug)]
pub(crate) struct Seg {
    bytes: Vec<u8>,
    kind: SegKind,
    cause: Option<KeyId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SegKind {
    /// Output a key caused that is not a glyph's echo.
    Reply,
    /// The echo of exactly one typed glyph.
    Echo(KeyId),
    /// Output nobody typed for.
    Unprompted,
    /// A no-echo prompt (`Password: `), serial of the no-echo stretch it opens.
    NoEchoPrompt(u64),
}

/// The far end: a line editor, a password mode, a full-screen app.
///
/// The line editor is a standard one: it knows the screen width and tracks its
/// own column, so it erases across the right margin and back up a wrapped row
/// the way zsh and readline do, and either moves to the next row explicitly
/// after filling the last column (`forced_wrap`) or leaves the terminal parked
/// in the deferred-wrap slot. Its column is exact (`reliable`) from a fresh
/// prompt on a fresh row until output it did not compute (unprompted output,
/// a resize); FIDELITY is only claimed while it is.
pub(crate) struct Program {
    line: Vec<char>,
    cur: usize,
    /// `true`: the line discipline (or the remote) is not echoing.
    noecho: bool,
    /// Which no-echo stretch this is; a prompt names the stretch it opened.
    noecho_serial: u64,
    alt: bool,
    /// The app on the alternate screen was started from a fresh row by a command.
    alt_from_command: bool,
    /// The screen width, as the far end learns it (SIGWINCH).
    cols: usize,
    /// The editor's column; `cols` means parked after filling the last column.
    col: usize,
    reliable: bool,
    forced_wrap: bool,
    /// The editor measures East Asian Ambiguous glyphs two cells wide, as the
    /// terminal writes them under `ambiguous_width = "wide"` ([`Config::wide`]).
    wide: bool,
    /// Each prompt draws [`RPROMPT`] ([`Config::rprompt`]).
    draws_rprompt: bool,
    /// The line editor at the prompt ([`Config::editor`]).
    editor: Editor,
    /// A cooked program a command started (`cat`) reads the tty until ^C: the
    /// line discipline is cooked whatever the editor at the prompt.
    cat: bool,
    /// Under a cooked tty, the cells each character of `line` takes on screen
    /// (two for a control character ECHOCTL draws as `^X`): what an erase
    /// takes back.
    cooked_cells: Vec<usize>,
    /// The column the right prompt starts at on the prompt's row, while it is on
    /// screen: the editor erases it before the line reaches it.
    rprompt: Option<usize>,
    /// [`Editor::Autosuggest`]'s suggestion, drawn right after the buffer's end
    /// while it is on screen; empty when there is none.
    suggestion: Vec<char>,
    /// The column the suggestion's first cell is on, while there is one.
    suggestion_at: usize,
    /// The shell signs its OSC 133 marks ([`Config::integration`]).
    integration: bool,
    /// Which shell of the case this is ([`nonce`]): each
    /// [`Unprompted::Rebind`] is a new one, with a new nonce.
    shell: u8,
    /// What the line editor binds the learned keys to.
    keymap: Keymap,
    /// The prompt the shell draws now: [`PROMPT`], or [`WIDE_PROMPT`] after a
    /// [`Unprompted::ResetPrompt`].
    prompt: &'static str,
    /// zsh's buffer stack ([`Unmodelled::PushLine`]): the line the next prompt
    /// draws pre-filled.
    pushed: Vec<char>,
    /// A command the shell marked with `C` runs: its `D` is owed before the
    /// next prompt's `A`.
    running: bool,
    /// The keyboard mode the shell pushed ([`Unprompted::KittyChords`]): Kitty
    /// `DISAMBIGUATE` or modifyOtherKeys level 2, under which a control chord
    /// goes out escaped, or neither.
    chords: ChordForm,
    /// How the key being read went out, if it is a control chord: as the
    /// terminal's keyboard mode at its press encoded it (`KeyLog::chord`). What
    /// the far end reads is the bytes the press sent, whatever its mode is by now.
    reading_chord: ChordForm,
    /// The cooked program a command started reads ONE line (`read -p`, after
    /// clearing the screen and printing a prompt of its own), then the shell
    /// prompts again.
    reads_one_line: bool,
    /// The words Tab completes ([`Sim::corpus_shell`]): the word before the
    /// cursor, a unique proper prefix of one of them, completed to it and a
    /// space; anything else a beep. Empty, Tab inserts `ion ` wherever it is
    /// pressed, the theorem's completion, which no word decides.
    completions: &'static [&'static str],
    /// The lines Enter submitted, newest last, which Up recalls
    /// ([`Sim::corpus_shell`]); `None`, Up recalls `ls -la` (the theorem's
    /// history, one line).
    history: Option<Vec<Vec<char>>>,
    /// The key being read, which everything emitted meanwhile is charged to.
    cause: Option<KeyId>,
    inbox: VecDeque<KeyId>,
    pipe: VecDeque<Seg>,
    /// Bytes of `pipe.front()` already delivered.
    head_off: usize,
}

/// The cells the terminal writes `c` with: East Asian Ambiguous glyphs two
/// wide under `ambiguous_width = "wide"` (`wide`), as the engine writes them
/// and a far end in a CJK locale measures them.
pub(crate) fn width(c: char, wide: bool) -> usize {
    if wide {
        aterm_grapheme::char_width_cjk(c)
    } else {
        aterm_grapheme::char_width(c)
    }
}

impl Program {
    fn new(cols: u16, cfg: Config) -> Self {
        Self {
            wide: cfg.wide,
            // A cooked program prints no right prompt.
            draws_rprompt: cfg.rprompt && cfg.editor != Editor::Cooked,
            editor: cfg.editor,
            cat: false,
            cooked_cells: Vec::new(),
            rprompt: None,
            suggestion: Vec::new(),
            suggestion_at: 0,
            integration: cfg.integration,
            shell: 0,
            keymap: cfg.keymap,
            prompt: PROMPT,
            pushed: Vec::new(),
            running: false,
            chords: ChordForm::Byte,
            reading_chord: ChordForm::Byte,
            reads_one_line: false,
            completions: &[],
            history: None,
            line: Vec::new(),
            cur: 0,
            noecho: false,
            noecho_serial: 0,
            alt: false,
            alt_from_command: false,
            cols: usize::from(cols),
            col: 0,
            reliable: true,
            forced_wrap: cfg.forced_wrap,
            cause: None,
            inbox: VecDeque::new(),
            pipe: VecDeque::new(),
            head_off: 0,
        }
    }

    fn emit(&mut self, kind: SegKind, s: impl Into<Vec<u8>>) {
        let bytes = s.into();
        if !bytes.is_empty() {
            self.pipe.push_back(Seg {
                bytes,
                kind,
                cause: self.cause,
            });
        }
    }

    fn reply(&mut self, s: impl Into<Vec<u8>>) {
        self.emit(SegKind::Reply, s);
    }

    fn unprompted(&mut self, s: impl Into<Vec<u8>>) {
        self.emit(SegKind::Unprompted, s);
    }

    /// Where printing `c` leaves the column (a glyph that does not fit, or one
    /// printed while parked, wraps to the next row first).
    fn advance(&mut self, c: char) {
        let w = width(c, self.wide);
        if c == '\r' {
            self.col = 0;
        } else if w > 0 {
            if self.col + w > self.cols {
                self.col = 0;
            }
            self.col += w;
        }
    }

    /// Print one character the editor positions itself.
    fn put(&mut self, kind: SegKind, c: char) {
        self.emit(kind, c.to_string());
        self.advance(c);
        if width(c, self.wide) > 0 && self.col >= self.cols && self.forced_wrap {
            self.reply("\r\n");
            self.col = 0;
        }
    }

    /// OSC 133 `mark` signed with this shell's nonce, as aterm's integration
    /// sends it; nothing without the integration.
    fn mark(&self, mark: &str) -> String {
        self.mark_saying(mark, "")
    }

    /// [`Self::mark`] with `word` after the signature (`;aterm_input=start`,
    /// as aterm's integration ends a `B` that stands where the buffer starts).
    fn mark_saying(&self, mark: &str, word: &str) -> String {
        if !self.integration {
            return String::new();
        }
        format!(
            "\x1b]133;{mark};id={}{word}\x07",
            aterm_core::terminal::ShellIntegrationNonce(nonce(self.shell)).to_hex()
        )
    }

    /// A command the shell runs starts: its `C` (after the line's newline,
    /// from the shell's preexec), and its `D` owed.
    fn start_command(&mut self) -> String {
        self.running = self.integration;
        self.mark("C")
    }

    /// The marks around a prompt about to be drawn: the `D` of a command that
    /// ran, then the `A` before the prompt, and the `B` after it, where the
    /// buffer starts, saying so: every caller draws it over an empty buffer.
    fn prompt_marks(&mut self) -> (String, String) {
        let done = if self.running {
            self.mark("D;0")
        } else {
            String::new()
        };
        self.running = false;
        (
            format!("{done}{}", self.mark("A")),
            self.mark_saying("B", ";aterm_input=start"),
        )
    }

    /// The line editor is zle ([`Editor::Zle`], [`Editor::Autosuggest`]): its
    /// `B` comes from zle-line-init, after zle drew the line.
    fn zle(&self) -> bool {
        matches!(self.editor, Editor::Zle | Editor::Autosuggest)
    }

    /// Text printed at the start of a fresh row, then the prompt: the column is
    /// exact again. A line pushed onto the buffer stack ([`Unmodelled::PushLine`])
    /// is popped into the buffer, pre-filled, the cursor after it. zle draws it
    /// before zle-line-init runs, so its `B` lands after the text and does not
    /// say it stands where the buffer starts (aterm's integration says so only
    /// over an empty buffer); readline draws it after the prompt that ends in
    /// the `B` (bash's operate-and-get-next, whose `B` is in PS1).
    fn prompt_after(&mut self, text: &str) {
        let (a, b) = self.prompt_marks();
        let prompt = self.prompt;
        let held = std::mem::take(&mut self.pushed);
        let line: String = held.iter().collect();
        if held.is_empty() {
            self.reply(format!("{text}{a}{prompt}{b}"));
        } else if self.zle() {
            let b = self.mark("B");
            self.reply(format!("{text}{a}{prompt}{line}{b}"));
        } else {
            self.reply(format!("{text}{a}{prompt}{b}{line}"));
        }
        self.col = prompt.chars().count() + self.col_span(&held);
        self.line = held;
        self.cur = self.line.len();
        // A pre-filled line that reaches the margin wraps: beyond this model.
        self.reliable = self.col < self.cols;
        self.rprompt = None;
        // The right prompt, drawn in italics behind a cursor save and restore,
        // where it leaves at least one column between it and the prompt. zsh
        // clears the row first (PROMPT_SP fills it with blanks and returns
        // before the prompt): a prompt drawn on a row a wrapped line or a
        // program left text on kept that text right of the right prompt, where
        // the erase that hides it later took it away with it, the very frame
        // before a correct ghost settling otherwise (seed index 1202 of the
        // review of increment 4).
        let lo = self.cols.saturating_sub(1 + RPROMPT.chars().count());
        if self.draws_rprompt && lo > self.col + 1 {
            self.reply(format!(
                "\x1b[K\x1b7\x1b[{}G\x1b[3m{RPROMPT}\x1b[23m\x1b8",
                lo + 1
            ));
            self.rprompt = Some(lo);
        }
    }

    /// zsh's push-line (`ESC q`): the buffer goes onto the buffer stack and
    /// the line is cleared, the prompt redrawn alone; the next prompt pops it
    /// pre-filled ([`Self::prompt_after`]). A line that wrapped leaves the
    /// cursor below the prompt's row, where the carriage return does not
    /// reach: beyond this model.
    fn push_line(&mut self) {
        let prompt = self.prompt;
        if prompt.chars().count() + self.col_span(&self.line) >= self.cols {
            self.reliable = false;
        }
        self.pushed = std::mem::take(&mut self.line);
        self.reset_line();
        self.reply(format!("\r\x1b[K{prompt}"));
        self.col = prompt.chars().count();
        self.rprompt = None;
    }

    /// Erase the right prompt before an insert that would bring the line up to
    /// it (no column left between them) or that shifts the row (a mid-line
    /// insert): zsh hides its RPROMPT rather than let the buffer run into it.
    fn hide_rprompt_for(&mut self, c: char) {
        let Some(lo) = self.rprompt else {
            return;
        };
        let wide = self.wide;
        let end = self.prompt.chars().count()
            + self.line.iter().map(|&g| width(g, wide)).sum::<usize>()
            + width(c, wide);
        if self.cur < self.line.len() || end >= lo {
            self.reply(format!("\x1b7\x1b[{}G\x1b[K\x1b8", lo + 1));
            self.rprompt = None;
        }
    }

    /// Step back over a glyph `w` columns wide: from the parked slot by an
    /// absolute column, across the left edge by a row up, else by backspaces.
    fn back(&mut self, w: usize) -> String {
        if w == 0 {
            String::new()
        } else if self.col >= self.cols {
            self.col = self.cols.saturating_sub(w);
            format!("\x1b[{}G", self.col + 1)
        } else if self.col >= w {
            self.col -= w;
            "\x08".repeat(w)
        } else {
            self.col = (self.cols + self.col).saturating_sub(w);
            format!("\x1b[A\x1b[{}G", self.col + 1)
        }
    }

    fn back_over(&mut self, chars: &[char]) -> String {
        let wide = self.wide;
        chars
            .iter()
            .rev()
            .map(|&c| self.back(width(c, wide)))
            .collect()
    }

    /// The cells `chars` take on screen.
    fn col_span(&self, chars: &[char]) -> usize {
        chars.iter().map(|&c| width(c, self.wide)).sum()
    }

    /// Cursor back over the tail after a redraw, as a fresh string. While echo
    /// is off the buffer is a secret, which no redraw shows: the prompt alone.
    /// (Redrawn, it was the far end echoing the keys typed at its no-echo read,
    /// which a frame then took for their echoes; the clause that holds such
    /// keys dark cannot hold a far end that displays them.)
    fn line_redraw(&self) -> String {
        if self.noecho {
            return self.prompt.to_owned();
        }
        let text: String = self.line.iter().collect();
        let tail: usize = self
            .line
            .get(self.cur..)
            .unwrap_or_default()
            .iter()
            .map(|&c| width(c, self.wide))
            .sum();
        let prompt = self.prompt;
        if tail == 0 {
            format!("{prompt}{text}")
        } else {
            format!("{prompt}{text}\x1b[{tail}D")
        }
    }

    /// Insert one character at the insertion point the way readline does on a
    /// terminal with ICH: at the end, just the glyph; in the middle, open a gap
    /// then write it, or, for a rewriting [`Editor`], write it, redraw the tail it
    /// shifts and step back over that tail. `echo_of` names the key whose echo
    /// this is, if any. `choice` decides whether an append's echo brings up an
    /// autosuggestion (see [`Program::suggest_after`]).
    fn insert(&mut self, c: char, echo_of: Option<KeyId>, choice: u32) {
        if width(c, self.wide) > 0 {
            self.hide_rprompt_for(c);
        }
        let kind = echo_of.map_or(SegKind::Reply, SegKind::Echo);
        let w = width(c, self.wide);
        if self.cur < self.line.len() && w > 0 {
            if self.col >= self.cols {
                self.reliable = false; // an insert from the parked slot is beyond this model
            }
            if self.editor == Editor::Overwrite {
                // Overwrite mode REPLACES the glyph under the cursor, written in
                // place; nothing right of it moves. A glyph of another width
                // than the one it replaces moves the tail after all: beyond this
                // model.
                let replaced = self.line.get(self.cur).copied().unwrap_or(' ');
                if width(replaced, self.wide) != w {
                    self.reliable = false;
                }
                self.put(kind, c);
                if let Some(cell) = self.line.get_mut(self.cur) {
                    *cell = c;
                }
                self.cur += 1;
                if echo_of.is_some() && self.cur == self.line.len() {
                    self.suggest_after(choice);
                }
                return;
            }
            // An insert whose shift pushes the tail past the right margin is
            // beyond it too: ICH drops what crosses the edge, where a real editor
            // rewraps the line onto the next row.
            let wide = self.wide;
            let tail: usize = self
                .line
                .get(self.cur..)
                .unwrap_or_default()
                .iter()
                .map(|&g| width(g, wide))
                .sum();
            if self.col + w + tail > self.cols {
                self.reliable = false;
            }
            if matches!(
                self.editor,
                Editor::Readline | Editor::Zle | Editor::Autosuggest
            ) {
                // The buffer's end before the insert: where a suggestion starts.
                let end = self.col + tail;
                // The glyph first, over the cell the tail starts on; then the
                // tail one cell on; then the cursor back to just after the glyph.
                self.put(kind, c);
                let rest: Vec<char> = self.line.get(self.cur..).unwrap_or_default().to_vec();
                for &t in &rest {
                    self.put(SegKind::Reply, t);
                }
                let back = self.back_over(&rest);
                self.reply(back);
                self.line.insert(self.cur, c);
                self.cur += 1;
                // An edit inside the buffer drops the suggestion: the shifted
                // tail wrote over its first cell, the rest is blanked; then one
                // for the new buffer may be found.
                self.blank_suggestion(end + w);
                self.refetch_after_edit(end + w);
                return;
            }
            // The gap ICH opens leaves a zero-width code point the cursor stands
            // before (a combining mark, its base left of the cursor) drawn on
            // that base, while the buffer now pairs it with the glyph inserted:
            // the editor no longer knows its screen.
            if self
                .line
                .get(self.cur)
                .is_some_and(|&next| width(next, self.wide) == 0)
            {
                self.reliable = false;
            }
            self.reply(format!("\x1b[{w}@"));
        }
        if self.editor == Editor::Autosuggest && w > 0 && self.cur == self.line.len() {
            self.append_along_suggestion(c, kind, echo_of.is_some(), choice);
            return;
        }
        self.put(kind, c);
        self.line.insert(self.cur, c);
        self.cur += 1;
        if echo_of.is_some() && self.cur == self.line.len() {
            self.suggest_after(choice);
        }
    }

    /// After an append's echo, now and then (`choice`), draw an autosuggestion
    /// right of the cursor, dim, and step back over it, as fish and
    /// zsh-autosuggestions draw one from history in the same output as the echo.
    /// Text right of a guess that appeared AFTER its press: the tail guard must
    /// keep proving a line through it (a run pressed at the end of the line had
    /// nothing to shift), and a guess typed onto it is held by the glyph lane.
    /// The editor does not manage it afterwards (the next append overwrites its
    /// first cell, as an unprompted [`Unprompted::Suggest`] is overwritten).
    fn suggest_after(&mut self, choice: u32) {
        if choice % 8 != 3 || self.noecho || self.alt {
            return;
        }
        let s = ["ion", "sne", "ab", "dice", "e a"][((choice / 8) % 5) as usize];
        let n = s.chars().count();
        // Dim, and italic: how this file tags DECORATION (see FRAME FIDELITY),
        // which a frame may show stale. A frame painting the guess this echo
        // answers cannot know the suggestion will appear beside it.
        self.reply(format!("\x1b[2;3m{s}\x1b[0m\x1b[{n}D"));
        if self.col + n >= self.cols {
            self.reliable = false; // the step back cannot climb a row
        }
    }

    /// [`Editor::Autosuggest`] appends `c` at the end of the buffer: a glyph that
    /// continues the suggestion takes its first cell (written plain over it,
    /// the rest left as it is); one that does not drops it, the rest of it
    /// blanked; and a typed glyph (`typed`) may bring up a new one from
    /// history (`choice`).
    fn append_along_suggestion(&mut self, c: char, kind: SegKind, typed: bool, choice: u32) {
        let continues = self.suggestion.first() == Some(&c) && self.suggestion_at == self.col;
        self.put(kind, c);
        self.line.push(c);
        self.cur = self.line.len();
        if continues {
            self.suggestion.remove(0);
            self.suggestion_at = self.col;
            return;
        }
        self.blank_suggestion(self.col);
        if typed && choice % 8 == 3 {
            let s = ["ion", "sne", "ab", "dice", "e a"][((choice / 8) % 5) as usize];
            self.draw_suggestion(&s.chars().collect::<Vec<char>>());
        }
    }

    /// Draw `s` as [`Editor::Autosuggest`]'s suggestion at the cursor, which
    /// stands at the end of the buffer: dim and italic (DECORATION, see FRAME
    /// FIDELITY), the cursor stepped back over it. Only where it fits short of
    /// the right prompt and the margin, as zsh-autosuggestions truncates.
    fn draw_suggestion(&mut self, s: &[char]) {
        let n = self.col_span(s);
        let limit = self.rprompt.map_or(self.cols, |lo| lo.saturating_sub(1));
        if n == 0 || self.noecho || self.alt || self.col + n >= limit {
            return;
        }
        let text: String = s.iter().collect();
        self.reply(format!("\x1b[2;3m{text}\x1b[0m\x1b[{n}D"));
        self.suggestion = s.to_vec();
        self.suggestion_at = self.col;
    }

    /// After an edit inside the buffer, [`Editor::Autosuggest`] fetches a
    /// suggestion for the new buffer, as zsh-autosuggestions does on every
    /// widget that changes it: now and then one is found (the buffer's length
    /// decides, so a run is a function of its ops), drawn dim and italic from
    /// `end`, the buffer's new end, behind a cursor save and restore (the
    /// cursor stays inside the buffer). It moves with the buffer's end, as the
    /// text a shift moves does: no cell tells the two apart.
    fn refetch_after_edit(&mut self, end: usize) {
        if self.editor != Editor::Autosuggest || self.line.len() % 3 != 2 {
            return;
        }
        let s: Vec<char> = "ion".chars().collect();
        let n = self.col_span(&s);
        let limit = self.rprompt.map_or(self.cols, |lo| lo.saturating_sub(1));
        if self.noecho || self.alt || end + n >= limit {
            return;
        }
        self.reply(format!("\x1b7\x1b[{}G\x1b[2;3mion\x1b[0m\x1b8", end + 1));
        self.suggestion = s;
        self.suggestion_at = end;
    }

    /// Drop [`Editor::Autosuggest`]'s suggestion, blanking its cells from
    /// column `from` on (behind a cursor save and restore, as the right prompt
    /// is hidden): the cells before `from` were written over already.
    fn blank_suggestion(&mut self, from: usize) {
        if self.suggestion.is_empty() {
            return;
        }
        let end = self.suggestion_at + self.col_span(&self.suggestion);
        let from = from.max(self.suggestion_at);
        self.suggestion.clear();
        if from < end {
            self.reply(format!(
                "\x1b7\x1b[{}G{}\x1b8",
                from + 1,
                " ".repeat(end - from)
            ));
        }
    }

    /// [`Editor::Autosuggest`] ACCEPTS its suggestion (`→` or End at the end
    /// of the buffer): its text joins the buffer, written plain over the dim
    /// cells, the cursor after it.
    fn accept_suggestion(&mut self) {
        let s = std::mem::take(&mut self.suggestion);
        for &c in &s {
            self.put(SegKind::Reply, c);
            self.line.push(c);
        }
        self.cur = self.line.len();
    }

    /// Clear the line being edited. A suggestion still on screen is the
    /// editor's no longer.
    fn reset_line(&mut self) {
        self.line.clear();
        self.cooked_cells.clear();
        self.cur = 0;
        self.suggestion.clear();
    }

    /// The line discipline is cooked: the tty echoes and edits the line.
    fn cooked(&self) -> bool {
        self.editor == Editor::Cooked || self.cat
    }

    /// A key at an edge of the line (`←` at its start, `→` or Delete at its
    /// end, an unbound sequence): readline beeps, zle stays silent.
    fn edge(&mut self) {
        if matches!(
            self.editor,
            Editor::Readline | Editor::ReadlineIch | Editor::Overwrite
        ) {
            self.reply("\x07");
        }
    }

    /// Backspace: [`Self::erase_back`] of one character, except in overwrite
    /// mode inside the line, where readline's rubout REPLACES the glyph before
    /// the cursor with a space and moves nothing (`\b \b`).
    fn rubout(&mut self) {
        if self.editor != Editor::Overwrite || self.cur == 0 || self.cur >= self.line.len() {
            self.erase_back(1);
            return;
        }
        let i = self.cur - 1;
        let gone = self.line.get(i).copied().unwrap_or(' ');
        let w = width(gone, self.wide);
        if w != 1 {
            self.reliable = false; // a wide glyph blanked by one space: beyond this model
        }
        if let Some(cell) = self.line.get_mut(i) {
            *cell = ' ';
        }
        self.cur = i;
        let back = self.back(w);
        self.reply(back);
        self.put(SegKind::Reply, ' ');
        let back = self.back(1);
        self.reply(back);
    }

    /// Erase the `n` characters before the insertion point and redraw the tail:
    /// readline clears to the end of the row first (taking a right prompt with
    /// it); zle rewrites the tail over the erased cells and blanks the cells it
    /// vacated (`\b \b` at the end of the line).
    fn erase_back(&mut self, n: usize) {
        let n = n.min(self.cur);
        if n == 0 {
            self.edge();
            return;
        }
        let start = self.cur - n;
        let erased: Vec<char> = self.line.drain(start..self.cur).collect();
        self.cur = start;
        let mut out = self.back_over(&erased);
        let tail: Vec<char> = self.line.get(self.cur..).unwrap_or_default().to_vec();
        // A tail that begins with a zero-width mark is redrawn onto the cell before
        // the cursor, or, at the start of a row, onto the cell the erase then
        // blanks, while the buffer holds it after the cursor, where the next glyph
        // inserted takes it on: the screen no longer shows the buffer (found by
        // the review of increment 4's sweep, seed index 1287, `a` shown plain
        // where it settled marked). Beyond this model, as an ICH in front of a
        // mark is (`Program::insert`).
        if tail.first().is_some_and(|&c| width(c, self.wide) == 0) {
            self.reliable = false;
        }
        if self.editor == Editor::Autosuggest && tail.is_empty() && n == 1 {
            // An erase at the end FETCHES the suggestion anew, into the cell it
            // vacated: the erased glyph and the old suggestion after it, or,
            // with none on screen, now and then a fresh one from history (the
            // frame showed that cell blank: a decoration it lacked).
            self.reply(out);
            let old = std::mem::take(&mut self.suggestion);
            let old_cells = self.col_span(&old);
            let fresh: Vec<char> = if !old.is_empty() {
                erased.iter().copied().chain(old).collect()
            } else if self.line.len() % 3 == 1 {
                "ion".chars().collect()
            } else {
                Vec::new()
            };
            self.draw_suggestion(&fresh);
            if self.suggestion.is_empty() {
                // Nothing drawn there: the vacated cells, and an old suggestion
                // after them, blanked (`\b \b`).
                let n = self.col_span(&erased) + old_cells;
                let blanks: Vec<char> = std::iter::repeat_n(' ', n).collect();
                for &c in &blanks {
                    self.put(SegKind::Reply, c);
                }
                let back = self.back_over(&blanks);
                self.reply(back);
            }
            return;
        }
        if matches!(self.editor, Editor::Zle | Editor::Autosuggest) {
            // An edit inside the buffer (or of more than one glyph) drops the
            // suggestion with the tail rewrite.
            let end = self.col + self.col_span(&tail) + self.col_span(&erased);
            self.blank_suggestion(end);
            self.reply(out);
            let wide = self.wide;
            let blanks = erased.iter().map(|&c| width(c, wide)).sum::<usize>();
            let rewritten: Vec<char> = tail
                .iter()
                .copied()
                .chain(std::iter::repeat_n(' ', blanks))
                .collect();
            for &c in &rewritten {
                self.put(SegKind::Reply, c);
            }
            let back = self.back_over(&rewritten);
            self.reply(back);
            let end = self.col + self.col_span(&tail);
            self.refetch_after_edit(end);
            return;
        }
        // Erasing to the end of the row takes a right prompt with it.
        out.push_str("\x1b[K");
        self.rprompt = None;
        self.reply(out);
        for &c in &tail {
            self.put(SegKind::Reply, c);
        }
        let back = self.back_over(&tail);
        self.reply(back);
    }

    /// `←`: one glyph back, or the edge.
    fn left(&mut self) {
        let Some(i) = self.cur.checked_sub(1) else {
            self.edge();
            return;
        };
        self.cur = i;
        let w = self.line.get(i).map_or(1, |&c| width(c, self.wide));
        let back = self.back(w);
        self.reply(back);
    }

    /// `→`: over the glyph under the cursor, or the edge. readline moves with
    /// `ESC [ C`, zle with `ESC [ 1 C`, and readline's older form (here with
    /// ICH) reprints the glyph; each reprints it where a move would reach the
    /// right margin, whose wrap a cursor move cannot take.
    fn right(&mut self) {
        let Some(&c) = self.line.get(self.cur) else {
            // At the end of the buffer zsh-autosuggestions ACCEPTS the
            // suggestion; anywhere else it is an edge.
            if self.editor == Editor::Autosuggest
                && !self.suggestion.is_empty()
                && self.suggestion_at == self.col
            {
                self.accept_suggestion();
            } else {
                self.edge();
            }
            return;
        };
        self.cur += 1;
        let w = width(c, self.wide);
        if w == 0 {
            // A code point that takes no cell (a combining mark the editor
            // steps over by code point): the cursor stays. (Sent as `ESC [ 0 C`
            // it moved a cell, a zero parameter being one, and the editor lost
            // its column while it claimed to know it.)
            return;
        }
        let fits = self.col + w < self.cols;
        match self.editor {
            Editor::Readline | Editor::Overwrite if fits => {
                self.reply("\x1b[C".repeat(w));
                self.col += w;
            }
            Editor::Zle | Editor::Autosuggest if fits => {
                self.reply(format!("\x1b[{w}C"));
                self.col += w;
            }
            _ => self.put(SegKind::Reply, c),
        }
    }

    /// The Delete key: the glyph under the cursor erased, the tail rewritten
    /// over it and the cell it vacated blanked; at the end of the line, the edge.
    fn delete_forward(&mut self) {
        if self.cur >= self.line.len() {
            self.edge();
            return;
        }
        let gone = self.line.remove(self.cur);
        let blanks = width(gone, self.wide);
        // An edit inside the buffer drops the suggestion, and may find one for
        // the new buffer.
        let rest = self.col_span(self.line.get(self.cur..).unwrap_or_default());
        self.blank_suggestion(self.col + rest + blanks);
        let rewritten: Vec<char> = self
            .line
            .get(self.cur..)
            .unwrap_or_default()
            .iter()
            .copied()
            .chain(std::iter::repeat_n(' ', blanks))
            .collect();
        for &c in &rewritten {
            self.put(SegKind::Reply, c);
        }
        let back = self.back_over(&rewritten);
        self.reply(back);
        self.refetch_after_edit(self.col + rest);
    }

    /// A learned key, as the line editor's keymap binds it ([`Keymap`]). Sent
    /// under Kitty `DISAMBIGUATE` or modifyOtherKeys level 2, Ctrl-A and Ctrl-E
    /// arrive as `CSI 97;5u` or `CSI 27;5;97~`, which no keymap here binds: a
    /// beep, as for any undefined key.
    fn learned_key(&mut self, key: EditKey) {
        if self.reading_chord != ChordForm::Byte {
            self.reply("\x07");
            return;
        }
        match (self.keymap, key) {
            (Keymap::ZshVanilla, EditKey::Home | EditKey::End) => self.reply("\x07"),
            (Keymap::ZshVanilla, EditKey::Delete) => self.insert('~', None, 0),
            (Keymap::ViIns, EditKey::CtrlA | EditKey::CtrlE) => {
                // Self-inserted, drawn as two glyphs, `^A` or `^E`.
                let letter = if key == EditKey::CtrlA { 'A' } else { 'E' };
                self.insert('^', None, 0);
                self.insert(letter, None, 0);
            }
            (_, EditKey::Home | EditKey::CtrlA) => self.line_start(),
            (_, EditKey::End | EditKey::CtrlE) => self.line_end(),
            (_, EditKey::Delete) => self.delete_forward(),
            (_, EditKey::Left | EditKey::Right | EditKey::Backspace) => {}
        }
    }

    /// beginning-of-line: the cursor back over the buffer before it.
    fn line_start(&mut self) {
        let before: Vec<char> = self.line.get(..self.cur).unwrap_or_default().to_vec();
        self.cur = 0;
        let back = self.back_over(&before);
        self.reply(back);
    }

    /// end-of-line: the cursor over the buffer after it, reprinting it. At the
    /// end of the buffer already, zsh-autosuggestions ACCEPTS its suggestion
    /// (end-of-line is one of its accept widgets, which accept only with the
    /// cursor at the end); anywhere else the suggestion stays where it is.
    fn line_end(&mut self) {
        let at_end = self.cur >= self.line.len();
        while let Some(&c) = self.line.get(self.cur) {
            self.cur += 1;
            self.put(SegKind::Reply, c);
        }
        if at_end && !self.suggestion.is_empty() && self.suggestion_at == self.col {
            self.accept_suggestion();
        }
    }

    /// Up: the previous command from history replaces the line, redrawn from the
    /// prompt. A line that wrapped leaves the cursor below the prompt's row,
    /// where the carriage return does not reach it: beyond this model. The
    /// history is `ls -la` alone, or, in the corpus shell, the lines Enter
    /// submitted, the newest recalled (a beep while there is none).
    fn recall(&mut self) {
        let recalled: Vec<char> = match &self.history {
            None => "ls -la".chars().collect(),
            Some(lines) => match lines.last() {
                Some(line) => line.clone(),
                None => {
                    self.reply("\x07");
                    return;
                }
            },
        };
        let wide = self.wide;
        let before: usize = self
            .line
            .get(..self.cur)
            .unwrap_or_default()
            .iter()
            .map(|&c| width(c, wide))
            .sum();
        if self.prompt.chars().count() + before >= self.cols {
            self.reliable = false;
        }
        // The row is cleared from the carriage return: a suggestion with it.
        self.reset_line();
        let text: String = recalled.iter().collect();
        self.reply(format!("\r\x1b[K{}{text}", self.prompt));
        self.col = self.prompt.chars().count() + self.col_span(&recalled);
        self.line = recalled;
        self.cur = self.line.len();
        self.rprompt = None;
    }

    /// Tab: in the corpus shell ([`Self::completions`]), the word before the
    /// cursor completed to the one word it is a proper prefix of, and a space,
    /// each glyph inserted as typed text is (so a completion inside the line
    /// shifts the tail as an insert does); a beep where it is no such prefix.
    /// Otherwise `ion `, the theorem's completion.
    fn complete(&mut self) {
        if self.completions.is_empty() {
            for c in "ion ".chars() {
                self.insert(c, None, 0);
            }
            return;
        }
        let completions = self.completions;
        let before = self.line.get(..self.cur).unwrap_or_default();
        let start = before.iter().rposition(|&c| c == ' ').map_or(0, |i| i + 1);
        let word: String = before.get(start..).unwrap_or_default().iter().collect();
        let mut hits = completions
            .iter()
            .filter(|w| w.starts_with(word.as_str()) && w.len() > word.len());
        let (Some(hit), None, false) = (hits.next(), hits.next(), word.is_empty()) else {
            self.reply("\x07");
            return;
        };
        let rest: Vec<char> = hit
            .chars()
            .skip(word.chars().count())
            .chain([' '])
            .collect();
        for c in rest {
            self.insert(c, None, 0);
        }
    }

    /// Ctrl-K, kill-line: the buffer from the cursor to its end erased. zle
    /// writes blanks over it and steps back (dropping a suggestion past it);
    /// readline clears to the end of the row (`ESC [ K`), a right prompt with
    /// it. At the end of the buffer nothing is drawn.
    fn kill_to_end(&mut self) {
        let tail: Vec<char> = self.line.drain(self.cur..).collect();
        if tail.is_empty() {
            return;
        }
        if matches!(self.editor, Editor::Zle | Editor::Autosuggest) {
            let end = self.col + self.col_span(&tail);
            self.blank_suggestion(end);
            let blanks: Vec<char> = std::iter::repeat_n(' ', self.col_span(&tail)).collect();
            for &c in &blanks {
                self.put(SegKind::Reply, c);
            }
            let back = self.back_over(&blanks);
            self.reply(back);
            return;
        }
        self.reply("\x1b[K");
        self.rprompt = None;
    }

    /// The cooked tty echoes `chars` as typed, each appended to its line:
    /// a control character as `^X` (ECHOCTL, two cells), a tab to the next tab
    /// stop, anything else as itself. `kind` is the segment the echo is (a
    /// typed glyph's echo, or a key's reply).
    fn tty_echo(&mut self, chars: &[char], kind: SegKind) {
        for &c in chars {
            let (shown, cells) = if c == '\t' {
                ("\t".to_owned(), 8 - self.col % 8)
            } else if c.is_control() {
                let caret = char::from((c as u8) ^ 0x40);
                (format!("^{caret}"), 2)
            } else {
                (c.to_string(), width(c, self.wide))
            };
            // A combining mark joins the cell before it, a tab past the last
            // stop sticks at the margin: beyond this model either way.
            if cells == 0 || (c == '\t' && self.col + cells >= self.cols) {
                self.reliable = false;
            }
            self.emit(kind, shown.clone());
            if c == '\t' {
                self.col = (self.col + cells).min(self.cols.saturating_sub(1));
            } else {
                for s in shown.chars() {
                    self.advance(s);
                }
            }
            self.line.push(c);
            self.cooked_cells.push(cells);
            self.cur = self.line.len();
        }
    }

    /// The cooked tty erases the last `n` characters of its line (VERASE with
    /// ECHOE, or VKILL and VWERASE with ECHOKE): `\b \b` for each cell, a tab
    /// backed over by `\b`s. It does not climb back up a row, nor step back out
    /// of the deferred-wrap slot as an editor would: beyond this model.
    fn tty_erase(&mut self, n: usize) {
        for _ in 0..n {
            let Some(c) = self.line.pop() else {
                break;
            };
            let cells = self.cooked_cells.pop().unwrap_or(1);
            if self.col >= self.cols || cells > self.col {
                self.reliable = false;
            }
            self.reply(if c == '\t' {
                "\x08".repeat(cells)
            } else {
                "\x08 \x08".repeat(cells)
            });
            self.col = self.col.saturating_sub(cells);
        }
        self.cur = self.line.len();
    }

    /// Enter under the cooked tty: the line goes to the program. `cat` writes it
    /// back (control characters as `^X`, as `cat -v` does) on the row after the
    /// echoed newline; the cooked program at the prompt runs it as a command.
    fn tty_enter(&mut self, choice: u32) {
        let text: String = self
            .line
            .iter()
            .map(|&c| {
                if c.is_control() {
                    format!("^{}", char::from((c as u8) ^ 0x40))
                } else {
                    c.to_string()
                }
            })
            .collect();
        if !self.cat {
            self.run_command(choice);
            return;
        }
        if self.reads_one_line {
            // `read` has its line: the script ends and the shell prompts.
            self.cat = false;
            self.reads_one_line = false;
            self.reset_line();
            self.prompt_after("\r\n");
            return;
        }
        self.reset_line();
        self.reply("\r\n");
        self.col = 0;
        self.reply(text.clone());
        for c in text.chars() {
            self.advance(c);
        }
        self.reply("\r\n");
        self.col = 0;
    }

    /// What Enter does at the shell: the line goes, a command runs.
    fn run_command(&mut self, choice: u32) {
        // zsh-autosuggestions drops the suggestion before the line is accepted.
        self.blank_suggestion(0);
        // The corpus shell keeps what was submitted, for Up (`recall`).
        if let Some(history) = self.history.as_mut().filter(|_| !self.line.is_empty()) {
            history.push(self.line.clone());
        }
        self.reset_line();
        // The submitted line keeps its right prompt; the editor is done with it.
        self.rprompt = None;
        let c = self.start_command();
        match choice % 8 {
            4 => {
                self.reply(format!("\r\n{c}"));
                self.noecho = true;
                self.noecho_serial += 1;
                let serial = self.noecho_serial;
                self.emit(SegKind::NoEchoPrompt(serial), "Password: ");
                self.col = "Password: ".len();
                self.reliable = true;
            }
            5 => {
                self.reply(format!("\r\n{c}"));
                self.col = 0;
                self.alt = true;
                self.alt_from_command = true;
                self.reply("\x1b[?1049h\x1b[H\x1b[2J~\r\n~ app");
            }
            6 => {
                self.reply(format!("\r\n{c}"));
                self.prompt_after("\x1b[H\x1b[2J");
            }
            7 => {
                let mut out = format!("\r\n{c}");
                for i in 0..7 {
                    let _ = write!(out, "line {i}\r\n");
                }
                self.prompt_after(&out);
            }
            // `cat`: a cooked program reads the tty until ^C, on the row after
            // the line, so a line the shell proved meets a cooked far end.
            3 if (choice / 8) % 2 == 1 => {
                self.reply(format!("\r\n{c}"));
                self.col = 0;
                self.cat = true;
                self.reliable = true;
            }
            // `read -p '$ '` after a `clear`: a cooked program reads one line
            // after a prompt of its own, drawn where the shell's stood, then the
            // shell prompts again. Its prompt is no shell's: no mark names it.
            3 if self.integration && (choice / 8) % 4 == 2 => {
                self.reply(format!("\r\n{c}\x1b[H\x1b[2J{PROMPT}"));
                self.col = PROMPT.chars().count();
                self.cat = true;
                self.reads_one_line = true;
                self.reliable = true;
            }
            _ => {
                let mut out = format!("\r\n{c}");
                for i in 0..(choice / 8) % 3 {
                    let _ = write!(out, "out{i}\r\n");
                }
                self.prompt_after(&out);
            }
        }
    }

    /// Back from the alternate screen to a fresh shell prompt.
    fn quit_app(&mut self) {
        self.alt = false;
        self.reset_line();
        self.rprompt = None;
        let (a, b) = self.prompt_marks();
        self.reply(format!("\x1b[?1049l{a}{}{b}", self.prompt));
        if self.alt_from_command {
            // The main screen comes back at the fresh row the command left.
            self.col = self.prompt.chars().count();
            self.reliable = true;
        } else {
            self.reliable = false;
        }
    }

    /// The program reads one key.
    fn consume(&mut self, id: KeyId, key: &Key, choice: u32) {
        self.cause = Some(id);
        if self.alt {
            // A full-screen app: it draws a typed glyph at its own cursor, and ^C
            // (or, like a pager, Enter now and then) quits it.
            match key {
                Key::Glyph(c) => self.emit(SegKind::Echo(id), c.to_string()),
                Key::Enter if choice.is_multiple_of(2) => self.quit_app(),
                Key::Enter => self.reply("\r\n"),
                Key::Backspace => self.reply("\x08"),
                Key::Left | Key::Right => self.reply("\x1b[2;2H"),
                Key::Paste(cs) => self.reply(cs.iter().collect::<String>()),
                Key::Unmodelled(Unmodelled::Interrupt) => self.quit_app(),
                Key::Unmodelled(_) | Key::Learned(_) => self.reply("\x1b[2;1H~"),
            }
        } else if self.noecho {
            // A password prompt: everything is taken silently until the line ends.
            match key {
                Key::Glyph(c) => self.line.push(*c),
                Key::Paste(cs) => self.line.extend(cs.iter().filter(|&&c| c != '\r')),
                Key::Backspace => {
                    self.line.pop();
                }
                Key::Enter => {
                    self.noecho = false;
                    self.cat = false;
                    self.reads_one_line = false;
                    self.reset_line();
                    self.prompt_after("\r\nok\r\n");
                }
                Key::Unmodelled(Unmodelled::Interrupt) => {
                    self.noecho = false;
                    self.cat = false;
                    self.reads_one_line = false;
                    self.reset_line();
                    self.prompt_after("^C\r\n");
                }
                Key::Left | Key::Right | Key::Unmodelled(_) | Key::Learned(_) => {}
            }
        } else if self.cooked() {
            self.consume_cooked(id, key, choice);
        } else {
            match key {
                Key::Glyph(c) => self.insert(*c, Some(id), choice),
                Key::Backspace => self.rubout(),
                Key::Left => self.left(),
                Key::Right => self.right(),
                Key::Enter => self.run_command(choice),
                Key::Paste(cs) => {
                    for &c in cs {
                        if c == '\r' {
                            self.run_command(choice);
                        } else {
                            self.insert(c, None, 0);
                        }
                    }
                }
                Key::Unmodelled(Unmodelled::Tab) => self.complete(),
                Key::Unmodelled(Unmodelled::KillLine) => self.erase_back(self.cur),
                Key::Unmodelled(Unmodelled::KillToEnd) => self.kill_to_end(),
                Key::Unmodelled(Unmodelled::KillWord) => {
                    let n = self.word_before();
                    self.erase_back(n);
                }
                Key::Unmodelled(Unmodelled::Interrupt) => {
                    self.reset_line();
                    self.prompt_after("^C\r\n");
                }
                Key::Learned(k) => self.learned_key(*k),
                Key::Unmodelled(Unmodelled::WordLeft) => {
                    let n = self.word_before();
                    let start = self.cur - n;
                    let word: Vec<char> =
                        self.line.get(start..self.cur).unwrap_or_default().to_vec();
                    self.cur = start;
                    let back = self.back_over(&word);
                    self.reply(back);
                }
                Key::Unmodelled(Unmodelled::Up) => self.recall(),
                Key::Unmodelled(Unmodelled::Esc) => {}
                Key::Unmodelled(Unmodelled::KeypadLeft) => self.edge(),
                Key::Unmodelled(Unmodelled::PushLine) => self.push_line(),
            }
        }
        self.cause = None;
    }

    /// How many characters before the insertion point a word motion or kill
    /// takes: the spaces right before it, then the word before them.
    fn word_before(&self) -> usize {
        let before = self.line.get(..self.cur).unwrap_or_default();
        let spaces = before.iter().rev().take_while(|&&c| c == ' ').count();
        let word = before
            .iter()
            .rev()
            .skip(spaces)
            .take_while(|&&c| c != ' ')
            .count();
        spaces + word
    }

    /// The cooked tty reads one key (see [`Editor::Cooked`]).
    fn consume_cooked(&mut self, id: KeyId, key: &Key, choice: u32) {
        let seq = |s: &str| s.chars().collect::<Vec<char>>();
        match key {
            Key::Glyph(c) => self.tty_echo(&[*c], SegKind::Echo(id)),
            Key::Backspace => self.tty_erase(1),
            Key::Left => self.tty_echo(&seq("\x1b[D"), SegKind::Reply),
            Key::Right => self.tty_echo(&seq("\x1b[C"), SegKind::Reply),
            Key::Enter => self.tty_enter(choice),
            Key::Paste(cs) => {
                for &c in cs {
                    if c == '\r' {
                        self.tty_enter(choice);
                    } else {
                        self.tty_echo(&[c], SegKind::Reply);
                    }
                }
            }
            Key::Unmodelled(Unmodelled::Tab) => self.tty_echo(&['\t'], SegKind::Reply),
            Key::Unmodelled(Unmodelled::KillLine) => self.tty_erase(self.line.len()),
            Key::Unmodelled(Unmodelled::KillWord) => {
                let n = self.word_before();
                self.tty_erase(n);
            }
            Key::Unmodelled(Unmodelled::Interrupt) => {
                // The tty echoes ^C and the program dies; the shell prompts.
                self.cat = false;
                self.reads_one_line = false;
                self.reset_line();
                self.prompt_after("^C\r\n");
            }
            // The tty echoes what the key sends: a control chord as `^X`, under
            // Kitty `DISAMBIGUATE` its `CSI u`, under modifyOtherKeys level 2 its
            // `CSI 27`.
            Key::Learned(k) => {
                let sent: &[char] = &match (k, self.reading_chord) {
                    (EditKey::Home, _) => seq("\x1b[H"),
                    (EditKey::End, _) => seq("\x1b[F"),
                    (EditKey::Delete, _) => seq("\x1b[3~"),
                    (EditKey::CtrlA, ChordForm::Byte) => vec!['\x01'],
                    (EditKey::CtrlE, ChordForm::Byte) => vec!['\x05'],
                    (EditKey::CtrlA, ChordForm::Kitty) => seq("\x1b[97;5u"),
                    (EditKey::CtrlE, ChordForm::Kitty) => seq("\x1b[101;5u"),
                    (EditKey::CtrlA, ChordForm::ModifyOtherKeys) => seq("\x1b[27;5;97~"),
                    (EditKey::CtrlE, ChordForm::ModifyOtherKeys) => seq("\x1b[27;5;101~"),
                    (EditKey::Left | EditKey::Right | EditKey::Backspace, _) => Vec::new(),
                };
                self.tty_echo(sent, SegKind::Reply);
            }
            Key::Unmodelled(Unmodelled::WordLeft) => {
                self.tty_echo(&seq("\x1bb"), SegKind::Reply);
            }
            Key::Unmodelled(Unmodelled::Up) => self.tty_echo(&seq("\x1b[A"), SegKind::Reply),
            Key::Unmodelled(Unmodelled::Esc) => self.tty_echo(&seq("\x1b"), SegKind::Reply),
            Key::Unmodelled(Unmodelled::KeypadLeft) => {
                self.tty_echo(&seq("\x1bOt"), SegKind::Reply);
            }
            Key::Unmodelled(Unmodelled::PushLine) => self.tty_echo(&seq("\x1bq"), SegKind::Reply),
            // Not the tty's VKILL (Ctrl-U): a character of the line, `^K`.
            Key::Unmodelled(Unmodelled::KillToEnd) => self.tty_echo(&['\x0b'], SegKind::Reply),
        }
    }

    /// Output nobody typed for.
    fn unprompted_output(&mut self, what: Unprompted, param: u32) {
        // Whatever it draws, the editor no longer knows where its right prompt
        // is, so it stops managing it (it stays on screen until overwritten),
        // nor its suggestion.
        self.rprompt = None;
        self.suggestion.clear();
        match what {
            Unprompted::Notice => {
                let redraw = self.line_redraw();
                self.unprompted(format!("\r\x1b[K[1] done\r\n{redraw}"));
                self.reliable = false;
            }
            Unprompted::Redraw => {
                let redraw = self.line_redraw();
                self.unprompted(format!("\r\x1b[K{redraw}"));
                self.reliable = false;
            }
            Unprompted::RightPrompt => {
                // In italics, as every right prompt here: DECORATION, which a
                // frame may show stale (FRAME FIDELITY's decoration rule). Drawn
                // plain, a line editor's erase that cleared it to the end of the
                // row (`\b ESC[K`) made every frame showing it beside a correct
                // ghost a violation, though nothing the predictor painted moved.
                let n = 1 + param % 9;
                self.unprompted(format!("\x1b7\x1b[{n}C\x1b[3m<rp>\x1b[23m\x1b8"));
            }
            Unprompted::Suggest => {
                if self.cur == self.line.len() && !self.noecho && !self.alt {
                    let s = ["ion", "sne", "ab", "dice", "e a"][(param % 5) as usize];
                    let n = s.chars().count();
                    // Dim and italic, as the editor's own suggestions: decoration.
                    self.unprompted(format!("\x1b[2;3m{s}\x1b[0m\x1b[{n}D"));
                    if self.col + n >= self.cols {
                        self.reliable = false; // the step back cannot climb a row
                    }
                }
            }
            Unprompted::Clear => {
                let redraw = self.line_redraw();
                self.unprompted(format!("\x1b[H\x1b[2J{redraw}"));
                self.reliable = false;
            }
            Unprompted::Jump => {
                let (r, c) = (1 + param % 7, 1 + (param / 7) % 15);
                self.unprompted(format!("\x1b[{r};{c}H"));
                self.reliable = false;
            }
            Unprompted::StatusLine => self.unprompted("\x1b7\x1b[1;1Hstatus\x1b[K\x1b8"),
            Unprompted::InsertLine | Unprompted::DeleteLine => {
                self.unprompted(if what == Unprompted::InsertLine {
                    "\x1b[L"
                } else {
                    "\x1b[M"
                });
                self.reliable = false;
            }
            Unprompted::ScrollUp => self.unprompted("\x1b[S"),
            Unprompted::ReverseIndex => self.unprompted("\x1bM"),
            Unprompted::EnterAlt => {
                if !self.alt {
                    self.alt = true;
                    self.alt_from_command = false;
                    self.unprompted("\x1b[?1049h\x1b[H\x1b[2J~ app");
                }
            }
            Unprompted::LeaveAlt => {
                if self.alt {
                    self.alt = false;
                    self.unprompted("\x1b[?1049l");
                    if self.alt_from_command {
                        // Left by the app with no prompt: the shell never redrew.
                        self.reliable = false;
                    }
                }
            }
            Unprompted::PasswordNewRow | Unprompted::PasswordSameRow => {
                if !self.noecho && !self.alt {
                    self.noecho = true;
                    self.noecho_serial += 1;
                    let serial = self.noecho_serial;
                    let text = if what == Unprompted::PasswordNewRow {
                        self.col = 0;
                        "\r\nPassword: "
                    } else {
                        " Password: "
                    };
                    for c in text.chars() {
                        self.advance(c);
                    }
                    self.reset_line();
                    self.emit(SegKind::NoEchoPrompt(serial), text);
                }
            }
            Unprompted::SilentNoEcho => {
                if !self.noecho && !self.alt {
                    // `read -s` with no prompt: nothing on screen says so.
                    self.noecho = true;
                    self.noecho_serial += 1;
                    self.reset_line();
                }
            }
            Unprompted::EchoOn => {
                if self.noecho {
                    self.noecho = false;
                    self.reset_line();
                    let (a, b) = self.prompt_marks();
                    self.unprompted(format!("\r\n{a}{}{b}", self.prompt));
                    self.col = self.prompt.chars().count();
                    self.reliable = true;
                }
            }
            Unprompted::Rebind => {
                // Only the shell at its prompt can exec itself.
                if !self.integration || self.alt || self.noecho || self.cat {
                    return;
                }
                let next = KEYMAPS
                    .iter()
                    .position(|&k| k == self.keymap)
                    .map_or(0, |i| (i + 1) % KEYMAPS.len());
                self.keymap = KEYMAPS[next];
                self.shell = self.shell.wrapping_add(1);
                self.running = false;
                self.reset_line();
                let (a, b) = self.prompt_marks();
                self.unprompted(format!("\r\n{a}{}{b}", self.prompt));
                self.col = self.prompt.chars().count();
                self.reliable = true;
            }
            Unprompted::ForgedMark => {
                // Drawn nowhere: the cursor and the column stay. The `B` says it
                // stands where the buffer starts, as aterm's integration's does.
                self.unprompted("\x1b]133;A\x07\x1b]133;B;aterm_input=start\x07");
            }
            Unprompted::KittyChords => {
                if !self.alt {
                    // Pushed: Kitty `DISAMBIGUATE` (fish 4 at its prompt), or
                    // modifyOtherKeys level 2 (a precmd's, or a program's left
                    // behind); popped: whichever is on.
                    let (bytes, next) = match (self.chords, param % 2) {
                        (ChordForm::Byte, 0) => ("\x1b[>1u", ChordForm::Kitty),
                        (ChordForm::Byte, _) => ("\x1b[>4;2m", ChordForm::ModifyOtherKeys),
                        (ChordForm::Kitty, _) => ("\x1b[<u", ChordForm::Byte),
                        (ChordForm::ModifyOtherKeys, _) => ("\x1b[>4;0m", ChordForm::Byte),
                    };
                    self.unprompted(bytes);
                    self.chords = next;
                }
            }
            Unprompted::ResetPrompt => {
                // Only the shell's line editor at its prompt, its line on the
                // prompt's row before and after (a carriage return reaches the
                // cursor's row alone): the prompt in the other width, the line
                // after it, the row cleared past it, the cursor back over the tail.
                // Its column is known: the editor stays reliable.
                if self.alt || self.noecho || self.cooked() {
                    return;
                }
                let next = if self.prompt == PROMPT {
                    WIDE_PROMPT
                } else {
                    PROMPT
                };
                let line = self.col_span(&self.line);
                let widest = self.prompt.chars().count().max(next.chars().count());
                if widest + line >= self.cols {
                    return;
                }
                let tail = self.col_span(self.line.get(self.cur..).unwrap_or_default());
                let text: String = self.line.iter().collect();
                let back = if tail == 0 {
                    String::new()
                } else {
                    format!("\x1b[{tail}D")
                };
                self.prompt = next;
                self.unprompted(format!("\r{next}{text}\x1b[K{back}"));
                self.col = next.chars().count() + line - tail;
            }
            Unprompted::Ris => {
                self.alt = false;
                self.cat = false;
                self.reads_one_line = false;
                self.chords = ChordForm::Byte;
                self.reset_line();
                let (a, b) = self.prompt_marks();
                self.unprompted(format!("\x1bc{a}{}{b}", self.prompt));
                self.col = self.prompt.chars().count();
                self.reliable = true;
            }
        }
    }

    fn pending_bytes(&self) -> usize {
        self.pipe.iter().map(|s| s.bytes.len()).sum::<usize>() - self.head_off
    }
}

/// A broken clause of the theorem.
#[derive(Clone, Debug)]
pub(crate) struct Violation {
    pub(crate) clause: &'static str,
    pub(crate) detail: String,
}

pub(crate) fn violation(clause: &'static str, detail: String) -> Violation {
    Violation { clause, detail }
}

/// Coverage, summed over a run, so a generator gone vacuous fails loudly.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    pub(crate) frames: u64,
    pub(crate) frames_displaying: u64,
    pub(crate) max_displayed: usize,
    pub(crate) guesses_armed: u64,
    pub(crate) secrets_typed: u64,
    pub(crate) fidelity_checks: u64,
    pub(crate) alt_frames: u64,
    pub(crate) generation_moves: u64,
    pub(crate) settled_cases: u64,
    /// Guesses in the overlay the painter refused (on half of a wide glyph).
    pub(crate) ghosts_refused: u64,
    /// Displayed frames compared whole with the screen their newest key's output
    /// settled to (FRAME FIDELITY).
    pub(crate) frame_fidelity_checks: u64,
    /// Cells of those frames that showed decoration the settled screen moved or
    /// dropped, or that the settled screen drew where the frame showed none:
    /// counted, not a violation (the decoration rule).
    pub(crate) stale_decoration: u64,
    /// Displaying frames (Adaptive) with a ghost on a row that holds a right
    /// prompt: lines with decoration that still prove.
    pub(crate) rprompt_frames: u64,
    /// TIMELINESS comparisons: ghosts on glass checked against the window.
    pub(crate) timeliness_checks: u64,
    /// ATTRIBUTION comparisons: credits checked against their key's echo.
    pub(crate) attribution_checks: u64,
    /// The predictor's own phase-0 decisions (`Predictor::events_for_test`).
    pub(crate) expiry_owes: u64,
    pub(crate) tail_refusals: u64,
    pub(crate) decorated_proofs: u64,
    pub(crate) typed_owes: u64,
    pub(crate) standing_voids: u64,
    pub(crate) advance_waits: u64,
    /// Turns a guess typed over text in a run not known to be at the end of
    /// the line was held off the glass, the head waiting on THE SCREEN AS IT
    /// WAS or the run's first such guess behind it (`Rule::DarkOverText`).
    pub(crate) text_waits: u64,
    /// Edit keys the predictor armed as ops of a state chain (host side).
    pub(crate) edit_keys_armed: u64,
    /// Arrows armed while the far end was a cooked tty: tracked, and (the class
    /// ladder) never shown.
    pub(crate) cooked_motions_armed: u64,
    /// Frames that displayed a state chain (its cells and cursor painted).
    pub(crate) edit_frames_displaying: u64,
    /// Of those, frames whose displayed state includes a motion.
    pub(crate) motions_displayed: u64,
    /// Of those, frames displaying a chain carried across its terminator.
    pub(crate) carried_chains: u64,
    /// PROVENANCE oracle comparisons of a displayed chain.
    pub(crate) oracle_checks: u64,
    /// The predictor's own edit-lane decisions (`Predictor::events_for_test`).
    pub(crate) motions_proven: u64,
    pub(crate) net_null_retired: u64,
    pub(crate) transit_waits: u64,
    pub(crate) decline_motions: u64,
    pub(crate) vacate_refusals: u64,
    /// Frames that displayed a SHIFT: a chain's state whose text moved, painted
    /// as the cells it came from.
    pub(crate) shifts_displayed: u64,
    /// Shifts armed while the far end was in overwrite mode: tracked, and (the
    /// class ladder) never shown.
    pub(crate) overwrite_shifts_armed: u64,
    /// Frames (Adaptive) that displayed a shift while the far end was in
    /// overwrite mode, which never shifts: the class ladder keeps this at zero.
    pub(crate) overwrite_shifts_displayed: u64,
    /// Cells of displayed frames where a state chain's vacated blank stood and
    /// the settled screen drew decoration (a suggestion the erase fetched):
    /// counted, not a violation (the decoration rule).
    pub(crate) late_decoration: u64,
    /// The predictor's own increment-3 decisions (`Predictor::events_for_test`).
    pub(crate) shifts_armed: u64,
    pub(crate) shifts_proven: u64,
    pub(crate) opaque_refusals: u64,
    pub(crate) mid_line_flushes: u64,
    pub(crate) ambiguous_shifts: u64,
    pub(crate) bidi_refusals: u64,
    /// Frames that displayed a state chain holding a learned key's op (Home,
    /// End, Ctrl-A, Ctrl-E, Delete): learned bindings shown at once.
    pub(crate) line_edge_displayed: u64,
    /// Of those, frames showing a learned Delete.
    pub(crate) delete_displayed: u64,
    /// FRAME FIDELITY's comparisons of a frame showing a learned key, and of one
    /// showing a learned Delete: a frame recorded (consecutive identical ones
    /// once) and in the clause's scope when its key settled.
    pub(crate) learned_fidelity_checks: u64,
    pub(crate) delete_fidelity_checks: u64,
    /// The predictor's own increment-4 decisions (`Predictor::events_for_test`).
    pub(crate) witnessed_lines: u64,
    pub(crate) line_edge_armed: u64,
    pub(crate) probes_armed: u64,
    pub(crate) bindings_learned: u64,
    pub(crate) bindings_unlearned: u64,
    pub(crate) kitty_refusals: u64,
}

impl Stats {
    pub(crate) fn add(&mut self, o: Stats) {
        self.frames += o.frames;
        self.frames_displaying += o.frames_displaying;
        self.max_displayed = self.max_displayed.max(o.max_displayed);
        self.guesses_armed += o.guesses_armed;
        self.secrets_typed += o.secrets_typed;
        self.fidelity_checks += o.fidelity_checks;
        self.alt_frames += o.alt_frames;
        self.generation_moves += o.generation_moves;
        self.settled_cases += o.settled_cases;
        self.ghosts_refused += o.ghosts_refused;
        self.frame_fidelity_checks += o.frame_fidelity_checks;
        self.stale_decoration += o.stale_decoration;
        self.rprompt_frames += o.rprompt_frames;
        self.timeliness_checks += o.timeliness_checks;
        self.attribution_checks += o.attribution_checks;
        self.expiry_owes += o.expiry_owes;
        self.tail_refusals += o.tail_refusals;
        self.decorated_proofs += o.decorated_proofs;
        self.typed_owes += o.typed_owes;
        self.standing_voids += o.standing_voids;
        self.advance_waits += o.advance_waits;
        self.text_waits += o.text_waits;
        self.edit_keys_armed += o.edit_keys_armed;
        self.cooked_motions_armed += o.cooked_motions_armed;
        self.edit_frames_displaying += o.edit_frames_displaying;
        self.motions_displayed += o.motions_displayed;
        self.carried_chains += o.carried_chains;
        self.oracle_checks += o.oracle_checks;
        self.motions_proven += o.motions_proven;
        self.net_null_retired += o.net_null_retired;
        self.transit_waits += o.transit_waits;
        self.decline_motions += o.decline_motions;
        self.vacate_refusals += o.vacate_refusals;
        self.shifts_displayed += o.shifts_displayed;
        self.overwrite_shifts_armed += o.overwrite_shifts_armed;
        self.overwrite_shifts_displayed += o.overwrite_shifts_displayed;
        self.late_decoration += o.late_decoration;
        self.shifts_armed += o.shifts_armed;
        self.shifts_proven += o.shifts_proven;
        self.opaque_refusals += o.opaque_refusals;
        self.mid_line_flushes += o.mid_line_flushes;
        self.ambiguous_shifts += o.ambiguous_shifts;
        self.bidi_refusals += o.bidi_refusals;
        self.line_edge_displayed += o.line_edge_displayed;
        self.delete_displayed += o.delete_displayed;
        self.learned_fidelity_checks += o.learned_fidelity_checks;
        self.delete_fidelity_checks += o.delete_fidelity_checks;
        self.witnessed_lines += o.witnessed_lines;
        self.line_edge_armed += o.line_edge_armed;
        self.probes_armed += o.probes_armed;
        self.bindings_learned += o.bindings_learned;
        self.bindings_unlearned += o.bindings_unlearned;
        self.kitty_refusals += o.kitty_refusals;
    }
}

/// What aterm-gui's frame observe answers at logical column `col` of frame row
/// `row` (`aterm-gui`'s `predict_seen::seen_at`), and what its key-time sample
/// reads for the cursor row: the cell's glyph, OPAQUE on a wide glyph's either
/// half, on a cluster or combining marks, and on a row drawn in another order or
/// size; a column past a trimmed row is the implicit blank.
pub(crate) fn seen_at(input: &RenderInput, row: usize, col: usize) -> Seen {
    let frame_col = input.visual_col(row, col);
    let cells = input.cells.get(row).map_or(&[][..], Vec::as_slice);
    let Some(cell) = cells.get(frame_col) else {
        return Seen::Blank;
    };
    let marked = input
        .clusters
        .get(row)
        .is_some_and(|marks| marks.iter().any(|(at, _)| *at == frame_col))
        || input
            .combining
            .get(row)
            .is_some_and(|marks| marks.iter().any(|(at, _)| *at == frame_col));
    let row_opaque = input
        .visual_to_logical
        .get(row)
        .is_some_and(|map| !map.is_empty())
        || input
            .line_sizes
            .get(row)
            .is_some_and(|size| *size != LineSize::SingleWidth);
    let opaque =
        row_opaque || cell.wide || cells.get(frame_col + 1).is_some_and(|next| next.wide) || marked;
    Seen::of_cell(cell.ch, opaque)
}

/// A frame's text, `rows` by `cols`, each cell as [`CellText`] (a column past a
/// trimmed row is the implicit blank).
pub(crate) fn frame_text(f: &RenderInput, rows: usize, cols: usize) -> Vec<Vec<CellText>> {
    (0..rows)
        .map(|r| (0..cols).map(|c| cell_text(f, r, c)).collect())
        .collect()
}

/// One cell of [`frame_text`].
fn cell_text(f: &RenderInput, r: usize, c: usize) -> CellText {
    let cell = f.cells.get(r).and_then(|row| row.get(c));
    let cluster = f
        .clusters
        .get(r)
        .and_then(|row| row.iter().find(|(at, _)| *at == c))
        .map(|(_, s)| s.clone());
    let marks = cluster.or_else(|| {
        f.combining
            .get(r)
            .and_then(|row| row.iter().find(|(at, _)| *at == c))
            .map(|(_, m)| m.iter().collect::<String>().into_boxed_str())
    });
    (
        cell.map_or(' ', |cell| cell.ch),
        cell.is_some_and(|cell| cell.wide),
        marks,
    )
}

/// The cursor aterm-gui draws on a frame whose newest painted ghost is at
/// `(row, col)`: one past it (`app_render.rs`). `None` at the right margin, where
/// the GUI clamps the column: the drawn cursor there is a guess about the far
/// end's wrap policy (parked or moved to the next row), not a claim FRAME
/// FIDELITY holds it to.
pub(crate) fn displayed_cursor(row: u16, col: u16, cols: u16) -> Option<(u16, u16)> {
    let next = col.saturating_add(1);
    (next < cols).then_some((row, next))
}

/// What a chain's cell of kind `kind` showing `ch` is to aterm-core's span
/// painter (`RenderInput::paint_prediction_span`), as aterm-gui maps it.
pub(crate) fn span_cell(ch: char, kind: GhostCell) -> SpanCell {
    match kind {
        GhostCell::Typed => SpanCell::Typed(ch),
        GhostCell::Moved { from } => SpanCell::Moved {
            from: usize::from(from),
            ch,
        },
        GhostCell::Vacated => SpanCell::Vacated,
    }
}

/// `Host::PaintsPastTheLine`'s span: the chain's own, and past its last cell
/// the rest of the row moved by the shift its last moved cell makes, as if
/// the text the predictor knows ran to the margin (an insert painted as ICH
/// paints it). A right prompt or a suggestion past the line moves with it.
fn span_past_the_line(d: &RenderInput, row: usize, span: &mut Vec<(usize, SpanCell)>, cols: usize) {
    let shift = span.iter().rev().find_map(|&(col, cell)| match cell {
        SpanCell::Moved { from, .. } => Some(col.wrapping_sub(from)),
        _ => None,
    });
    let (Some(shift), Some(&(last, _))) = (shift, span.last()) else {
        return;
    };
    let cells = d.cells.get(row).map_or(&[][..], Vec::as_slice);
    for col in last + 1..cols {
        let from = col.wrapping_sub(shift);
        let cell = match cells.get(from) {
            Some(source) if from < cols => SpanCell::Moved {
                from,
                ch: source.ch,
            },
            _ => SpanCell::Vacated,
        };
        span.push((col, cell));
    }
}

/// How `host` paints a state chain's frame into `d` (`(cols, blank)` the grid
/// width and the terminal blank): `shown`, the chain's cells with their kinds
/// (`GhostCell`), and `chain_cursor`, the cursor the predictor gives it.
/// aterm-gui's rule: the cells are one SPAN on the chain's row, painted ALL OR
/// NONE by aterm-core's span painter (`RenderInput::paint_prediction_span`:
/// typed glyphs as ghosts, moved text as the cells it came from, vacated cells
/// blank), and the chain's cursor drawn only when the span was painted and
/// the cursor's own cell is no wide glyph's right half once it was; with no
/// cursor from the predictor, the glyph lane's cursor, one past the newest
/// cell painted. Returns which cells were painted and the cursor drawn (`None`:
/// the engine's).
pub(crate) fn paint_chain(
    d: &mut RenderInput,
    (shown, kinds): (&[(u16, u16, char)], &[GhostCell]),
    chain_cursor: Option<(u16, u16)>,
    (cols, blank): (u16, RenderCell),
    host: Host,
) -> (Vec<bool>, Option<(u16, u16)>) {
    let width = usize::from(cols);
    let mut painted = vec![false; shown.len()];
    if host == Host::PaintsPartialState {
        // Each cell on its own, the ones the painter refuses left out.
        for (&(r, c, ch), painted) in shown.iter().zip(&mut painted) {
            *painted = d.paint_prediction_ghost(usize::from(r), usize::from(c), ch, width, blank);
        }
    } else if let Some(&(row, _, _)) = shown.first() {
        let row = usize::from(row);
        let mut span: Vec<(usize, SpanCell)> = shown
            .iter()
            .zip(kinds)
            .map(|(&(_, c, ch), &kind)| (usize::from(c), span_cell(ch, kind)))
            .collect();
        if host == Host::PaintsPastTheLine {
            span_past_the_line(d, row, &mut span, width);
        }
        let one_row = shown.iter().all(|&(r, _, _)| usize::from(r) == row);
        if one_row && d.paint_prediction_span(row, span, width, blank) {
            painted.fill(true);
        }
    }
    let all = painted.iter().all(|&p| p);
    let on_continuation = |(r, c): (u16, u16)| {
        let r = usize::from(r);
        d.cells
            .get(r)
            .and_then(|row| row.get(d.visual_col(r, usize::from(c))))
            .is_some_and(|cell| cell.wide)
    };
    let glyph_lane_cursor = shown
        .iter()
        .zip(&painted)
        .rfind(|&(_, &p)| p)
        .and_then(|(&(r, c, _), _)| displayed_cursor(r, c, cols));
    let drawn = match (host, chain_cursor) {
        (Host::CursorAfterNewestGlyph, _) | (_, None) => glyph_lane_cursor,
        (Host::PaintsPartialState, cursor) => cursor,
        (Host::CursorOnAContinuation, cursor) => cursor.filter(|_| all),
        (Host::CursorWithoutSpan, cursor) => cursor.filter(|&c| !on_continuation(c)),
        (_, cursor) => cursor.filter(|&c| all && !on_continuation(c)),
    };
    (painted, drawn)
}

/// ATOMICITY and CURSOR LOCALITY of a painted state chain's frame: `shown`
/// its cells, `painted` which were painted into `d`, `drawn` the cursor drawn
/// on a `(rows, cols)` grid. Every cell painted, or none and the engine's
/// cursor drawn (a part of a state is a frame no program draws, and a cursor
/// with none of its cells is a state's cursor over another state); and a drawn
/// cursor inside the grid, never on a wide glyph's right half.
pub(crate) fn chain_paint_clauses(
    shown: &[(u16, u16, char)],
    painted: &[bool],
    drawn: Option<(u16, u16)>,
    d: &RenderInput,
    (rows, cols): (u16, u16),
) -> Result<(), Violation> {
    let count = painted.iter().filter(|&&p| p).count();
    if count != 0 && count != shown.len() {
        return Err(violation(
            "atomicity",
            format!(
                "{count} of the chain's {} cells painted: {shown:?}",
                shown.len()
            ),
        ));
    }
    if count == 0 && !shown.is_empty() && drawn.is_some() {
        return Err(violation(
            "atomicity",
            format!("the chain's cursor {drawn:?} drawn over a refused paint of {shown:?}"),
        ));
    }
    if let Some((r, c)) = drawn {
        let row = usize::from(r);
        let continuation = d
            .cells
            .get(row)
            .and_then(|cells| cells.get(d.visual_col(row, usize::from(c))))
            .is_some_and(|cell| cell.wide);
        if r >= rows || c >= cols || continuation {
            return Err(violation(
                "cursor-locality",
                format!(
                    "the chain's cursor is drawn at ({r}, {c}): outside the grid or on a wide \
                     glyph's right half"
                ),
            ));
        }
    }
    Ok(())
}

/// Which cells of a frame the far end drew as decoration: the italic ones (only
/// decoration is drawn in italics in this world, and a ghost is never italic).
pub(crate) fn frame_decoration(f: &RenderInput, rows: usize, cols: usize) -> Vec<Vec<bool>> {
    (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| {
                    f.cells
                        .get(r)
                        .and_then(|row| row.get(c))
                        .is_some_and(|cell| cell.italic)
                })
                .collect()
        })
        .collect()
}

/// A fresh engine of `rows` by `cols` (the theorem's [`ROWS`] by [`COLS`], the
/// measurement's a screen wide enough for the command lines its corpus types),
/// under the world's ambiguous-width policy: the wide one is the host's
/// `ambiguous_width = "wide"`, applied the way the host applies its config
/// (`Terminal::apply_config`); RIS keeps it, as a host setting.
pub(crate) fn engine(cfg: Config, rows: u16, cols: u16) -> Terminal {
    let mut term = Terminal::new(rows, cols);
    if cfg.wide {
        let config = aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..aterm_core::config::TerminalConfig::default()
        };
        term.apply_config(&config);
    }
    // aterm's shell integration: the host authorizes the first shell's nonce
    // and requires it, so only that shell's signed marks are kept. A host that
    // never turns the gate on keeps any mark (`Host::UnsignedInputStart`).
    if cfg.integration {
        term.authorize_shell_integration(nonce(0));
        term.set_require_shell_integration_nonce(cfg.host != Host::UnsignedInputStart);
    }
    term
}

/// The simulated session: engine, reference fold, program, host and predictor.
pub(crate) struct Sim {
    cfg: Config,
    term: Terminal,
    /// The pure fold: the same output bytes and resizes, nothing else.
    reference: Terminal,
    scratch: RenderInput,
    predictor: Predictor,
    program: Program,
    now: Instant,
    /// The instant of the last keypress: the only clock `Host::StaleClock` reads.
    last_press: Instant,
    keys: Vec<KeyLog>,
    /// The keys whose output last reached the screen past its deadline: the
    /// only keys [`Sim::late_predecessor`] can find, kept where output lands so
    /// it reads them rather than every key typed before.
    late_outputs: std::collections::BTreeSet<KeyId>,
    /// The keystroke behind each pending guess, oldest first (see `sync`).
    mirror: VecDeque<KeyId>,
    screen_epoch: u64,
    was_alt: bool,
    last_generation: u64,
    /// Bytes of unprompted output that have reached the screen. Counted in
    /// bytes, not segments: a frame can fall inside an unprompted segment (a
    /// right prompt's cursor save and move delivered, its text not yet), and the
    /// output after that frame is still output the frame did not show.
    unprompted_delivered: u64,
    /// The no-echo stretch whose prompt has fully reached the screen.
    noecho_visible: Option<u64>,
    /// The echo glyph in delivery: its key and the cell it is landing on.
    landing: Option<(KeyId, u16, u16, Space, u64)>,
    /// The ghosts the last frame put on glass, with their keys: what a host that
    /// skips the erase frame leaves there (`Host::KeepsStaleGhosts`).
    glass: Vec<(KeyId, u16, u16, char)>,
    /// The press's extraction of the screen, for the key-time row sample.
    press_scratch: RenderInput,
    /// The cursor row the press hands the predictor, refilled in place.
    press_row: Vec<Seen>,
    /// The row under it, which the press hands too (`KeySample::below`), refilled
    /// in place; empty on the last row.
    press_below: Vec<Seen>,
    stats: Stats,
    /// The newest Enter pressed: a state chain displayed with an op typed before
    /// it is carried across its terminator.
    last_enter: Option<KeyId>,
    /// The PROVENANCE oracle's base for the state chain in flight (see
    /// [`OracleBase`]).
    oracle: Option<OracleBase>,
    /// The input start `Host::StaleInputStart` read last, and hands on.
    stale_start: Option<InputStart>,
    /// The shell whose nonce the host authorized last ([`Program::shell`]).
    shell: u8,
    /// A step-by-step account, kept only when replaying a shrunk failure.
    trace: Option<String>,
    /// The checker runs (every clause of the theorem, on every frame). A
    /// measurement of a predictor with rules switched off turns it off
    /// (`Sim::without_checks`): that predictor breaks clauses by design, and
    /// what it showed is what is measured.
    checks: bool,
    /// What the last frame put on glass (`Sim::on_glass`).
    glass_frame: Option<OnGlass>,
    /// What prediction cost the host, counted (`Sim::cost`).
    cost: Cost,
    /// The host's edit-lane setting (aterm-gui's `predictive_echo_editing`),
    /// handed to the predictor (`Predictor::set_editing`): on, every edit key
    /// reads the cursor row at its press; off, none does, since the predictor
    /// then routes them as it did before the lane and asks no row
    /// (`Sim::set_editing`).
    editing: bool,
}

/// What a frame put on glass: the displayed view `D`, the cursor drawn on it
/// (`None`: the engine's) and the rows a ghost was painted on.
#[derive(Clone, Debug)]
pub(crate) struct OnGlass {
    pub(crate) frame: RenderInput,
    pub(crate) cursor: Option<(u16, u16)>,
    pub(crate) rows: Vec<u16>,
}

/// What prediction cost the host, counted rather than timed: the observe calls
/// each frame's reconcile made, the ops the predictor folded each frame and each
/// press (`PredictEvents::ops_folded`), and the row cells each press read for
/// the key-time sample (the cursor row and the row under it).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Cost {
    pub(crate) reconciles: u64,
    pub(crate) observe_calls: u64,
    pub(crate) observe_max: u64,
    pub(crate) frame_folds: u64,
    pub(crate) frame_folds_max: u64,
    pub(crate) presses: u64,
    pub(crate) press_folds: u64,
    pub(crate) press_folds_max: u64,
    pub(crate) row_samples: u64,
    pub(crate) row_cells: u64,
    pub(crate) row_cells_max: u64,
}

impl Cost {
    pub(crate) fn add(&mut self, o: Cost) {
        self.reconciles += o.reconciles;
        self.observe_calls += o.observe_calls;
        self.observe_max = self.observe_max.max(o.observe_max);
        self.frame_folds += o.frame_folds;
        self.frame_folds_max = self.frame_folds_max.max(o.frame_folds_max);
        self.presses += o.presses;
        self.press_folds += o.press_folds;
        self.press_folds_max = self.press_folds_max.max(o.press_folds_max);
        self.row_samples += o.row_samples;
        self.row_cells += o.row_cells;
        self.row_cells_max = self.row_cells_max.max(o.row_cells_max);
    }
}

/// Where the PROVENANCE oracle starts a state chain's fold: the screen `A` at
/// the last frame whose reconcile retired the chain to a state the screen
/// matched (`Predictor::chain_matched_for_test`), read from `A` itself: the
/// text on the chain's row from `lo` to the end the predictor claims, and the
/// cursor. Only `lo` and that end are the predictor's.
#[derive(Clone, Debug)]
pub(crate) struct OracleBase {
    /// The oldest pending op's key when it was read: a base for that front only.
    front: Option<KeyId>,
    row: u16,
    lo: u16,
    text: Vec<char>,
    cursor: u16,
}

impl Sim {
    fn new(cfg: Config) -> Self {
        Self::sized(cfg, ROWS, COLS)
    }

    /// A session on a screen of `rows` by `cols` (the theorem's is [`ROWS`] by
    /// [`COLS`]; a host reset still toggles between those and
    /// [`ALT_GEOMETRY`]).
    pub(crate) fn sized(cfg: Config, rows: u16, cols: u16) -> Self {
        let term = engine(cfg, rows, cols);
        let generation = term.content_scroll_state().coordinate_generation();
        let mut predictor = Predictor::new(Self::host_mode_of(cfg));
        if let Host::Rule(rule) = cfg.host {
            predictor.disable_rule_for_test(rule);
        }
        let start = Instant::now();
        Self {
            cfg,
            term,
            reference: engine(cfg, rows, cols),
            scratch: RenderInput::empty(),
            predictor,
            program: Program::new(cols, cfg),
            glass: Vec::new(),
            press_scratch: RenderInput::empty(),
            press_row: Vec::new(),
            press_below: Vec::new(),
            now: start,
            last_press: start,
            keys: Vec::new(),
            late_outputs: std::collections::BTreeSet::new(),
            mirror: VecDeque::new(),
            screen_epoch: 0,
            was_alt: false,
            last_generation: generation,
            unprompted_delivered: 0,
            noecho_visible: None,
            landing: None,
            stats: Stats::default(),
            last_enter: None,
            oracle: None,
            trace: None,
            stale_start: None,
            shell: 0,
            checks: true,
            glass_frame: None,
            cost: Cost::default(),
            editing: true,
        }
    }

    /// The host's edit-lane setting, as aterm-gui's config hands it: to the
    /// predictor, and to the press, which reads the row for an edit key only
    /// with the lane on (`app_input.rs`, `pediting`).
    pub(crate) fn set_editing(&mut self, on: bool) {
        self.editing = on;
        self.predictor.set_editing(on);
    }

    /// The predictor, for a measurement to configure before the first key
    /// (`Predictor::set_editing`, `Predictor::disable_rule_for_test`) and to read
    /// its counts (`Predictor::events_for_test`).
    pub(crate) fn predictor_mut(&mut self) -> &mut Predictor {
        &mut self.predictor
    }

    /// The engine: the authoritative screen `A`.
    pub(crate) fn term(&self) -> &Terminal {
        &self.term
    }

    /// The checker off (see `Sim::checks`).
    pub(crate) fn without_checks(&mut self) {
        self.checks = false;
    }

    /// The coverage counts so far: the world's own, and the predictor's
    /// decisions (`Predictor::events_for_test`).
    pub(crate) fn stats(&self) -> Stats {
        let events = self.predictor.events_for_test();
        Stats {
            expiry_owes: events.expiry_owes,
            tail_refusals: events.tail_refusals,
            decorated_proofs: events.decorated_proofs,
            typed_owes: events.typed_owes,
            standing_voids: events.standing_voids,
            advance_waits: events.advance_waits,
            text_waits: events.text_waits,
            motions_proven: events.motions_proven,
            net_null_retired: events.net_null_retired,
            transit_waits: events.transit_waits,
            decline_motions: events.decline_motions,
            vacate_refusals: events.vacate_refusals,
            shifts_armed: events.shifts_armed,
            shifts_proven: events.shifts_proven,
            opaque_refusals: events.opaque_refusals,
            mid_line_flushes: events.mid_line_flushes,
            ambiguous_shifts: events.ambiguous_shifts,
            bidi_refusals: events.bidi_refusals,
            witnessed_lines: events.witnessed_lines,
            line_edge_armed: events.line_edge_armed,
            probes_armed: events.probes_armed,
            bindings_learned: events.bindings_learned,
            bindings_unlearned: events.bindings_unlearned,
            kitty_refusals: events.kitty_refusals,
            ..self.stats
        }
    }

    /// What prediction has cost the host so far.
    pub(crate) fn cost(&self) -> Cost {
        self.cost
    }

    /// What the last frame put on glass, `None` when it painted nothing and drew
    /// the engine's cursor (`D = A`).
    pub(crate) fn on_glass(&self) -> Option<&OnGlass> {
        self.glass_frame.as_ref()
    }

    /// The authoritative screen `A` the last frame reconciled against and
    /// painted over (`D` is it, where [`Self::on_glass`] is `None`).
    pub(crate) fn screen(&self) -> &RenderInput {
        &self.scratch
    }

    /// THE CORPUS SHELL: Tab completes the word before the cursor from
    /// `completions`, and Up recalls the newest line submitted, as a shell with
    /// a history and a completion table does. The theorem's world completes
    /// `ion ` and recalls `ls -la` whatever was typed.
    pub(crate) fn corpus_shell(&mut self, completions: &'static [&'static str]) {
        self.program.completions = completions;
        self.program.history = Some(Vec::new());
    }

    /// The shell draws its prompt (with its marks under the integration) and
    /// it reaches the screen, before the first key.
    pub(crate) fn fresh_prompt(&mut self) -> Result<(), Violation> {
        self.program.prompt_after("");
        let total = self.program.pending_bytes();
        self.deliver(total)?;
        self.frame()
    }

    /// A press, routed the way aterm-gui routes it, and the redraw after it.
    pub(crate) fn press_and_draw(&mut self, key: Key) -> Result<(), Violation> {
        let folded = self.predictor.events_for_test().ops_folded;
        self.press(key)?;
        let folds = self.predictor.events_for_test().ops_folded - folded;
        self.cost.presses += 1;
        self.cost.press_folds += folds;
        self.cost.press_folds_max = self.cost.press_folds_max.max(folds);
        self.frame()
    }

    /// The far end reads the oldest key it has not read; `choice` decides what
    /// a command it runs prints (`Program::run_command`). Returns the bytes of
    /// output that key caused.
    pub(crate) fn read_key(&mut self, choice: u32) -> usize {
        let before = self.program.pending_bytes();
        self.program_step(1, choice);
        self.program.pending_bytes() - before
    }

    /// The far end's buffer as it stands: its length and the cursor's index in
    /// it.
    pub(crate) fn buffer(&self) -> (usize, usize) {
        (self.program.line.len(), self.program.cur)
    }

    /// `n` bytes of the output in flight reach the screen, and the redraw after
    /// them.
    pub(crate) fn deliver_and_draw(&mut self, n: usize) -> Result<(), Violation> {
        self.deliver(n)?;
        self.frame()
    }

    /// The clock moves to `at` (never back); a deadline that falls due wakes the
    /// host's expiry sweep and its erase frame, as [`Op::Advance`] does.
    /// Returns whether that frame was drawn.
    pub(crate) fn advance_to(&mut self, at: Instant) -> Result<bool, Violation> {
        self.advance_by(at.saturating_duration_since(self.now))
    }

    /// The predictor's next deadline, for a host's timer.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.predictor.next_deadline()
    }

    /// The clock.
    pub(crate) fn now(&self) -> Instant {
        self.now
    }

    fn generation(&self) -> u64 {
        self.term.content_scroll_state().coordinate_generation()
    }

    fn space(&self) -> Space {
        Space {
            generation: self.generation(),
            screen: self.screen_epoch,
        }
    }

    /// What the host hands the predictor as the generation (a mutant freezes it).
    fn host_generation(&self) -> u64 {
        match self.cfg.host {
            Host::FrozenCoordinates => 0,
            _ => self.generation(),
        }
    }

    /// The display mode the host runs the predictor in (a mutant forces `Always`).
    fn host_mode_of(cfg: Config) -> PredictMode {
        match cfg.host {
            Host::ForcesAlways => PredictMode::Always,
            _ => cfg.mode,
        }
    }

    fn host_mode(&self) -> PredictMode {
        Self::host_mode_of(self.cfg)
    }

    /// The press path's `set_ambiguous_width_double`, after `set_mode` and before
    /// the predict (`app_input.rs`), with the terminal's policy (a mutant never
    /// calls it).
    fn tell_ambiguous_width(&mut self) {
        if self.cfg.host != Host::IgnoresAmbiguousWidth {
            self.predictor.set_ambiguous_width_double(self.cfg.wide);
        }
    }

    /// What the host reports as the alternate screen (a mutant never does).
    fn host_alt(&self) -> bool {
        match self.cfg.host {
            Host::BlindToAltScreen => false,
            _ => self.term.is_alternate_screen(),
        }
    }

    /// Where the line editor's buffer starts, as the host hands it to the
    /// predictor at a press: aterm-core's authenticated `B` that says it
    /// stands where the buffer starts, and its prompt
    /// (`Terminal::input_region_start`), or a mutant's. One that remembers the
    /// last it read hands it on whatever happened since; one that never turned
    /// the nonce gate on reads any `B` the engine kept, at the prompt in front;
    /// one that does not ask whether the `B` says it stands at the buffer's
    /// start takes the signed `B` of the prompt in front wherever it stands.
    fn host_input_start(&mut self) -> Option<InputStart> {
        let term = &self.term;
        // The `B` the engine keeps for the current block, on screen, at the
        // live bottom of the main screen: what a host that skipped one of
        // `input_region_start`'s conjuncts reads.
        let kept = |prompt: u64| {
            let block = term.current_block()?;
            let open = term.shell_state() == aterm_core::terminal::ShellState::EnteringCommand
                && !term.is_alternate_screen()
                && term.grid().display_offset() == 0;
            let row = block
                .command_start_row?
                .checked_sub(term.grid().visible_to_absolute(0))?;
            let row = u16::try_from(row).ok().filter(|&r| r < term.rows())?;
            open.then_some(InputStart {
                cell: (row, block.command_start_col?),
                prompt,
            })
        };
        let faithful = term.input_region_start().map(|s| InputStart {
            cell: (s.row, s.col),
            prompt: s.prompt,
        });
        match self.cfg.host {
            Host::StaleInputStart => {
                self.stale_start = faithful.or(self.stale_start);
                self.stale_start.map(|s| InputStart {
                    prompt: u64::MAX,
                    ..s
                })
            }
            Host::UnsignedInputStart => kept(term.current_block()?.id),
            Host::UnattestedInputStart => kept(term.prompt_in_front()?),
            _ => faithful,
        }
    }

    /// The prompt a line terminator is pressed at, as the host hands it to
    /// `Predictor::note_line_submit_at`: aterm-core's authenticated prompt in
    /// front (`Terminal::prompt_in_front`), or, for the host that never turned
    /// the nonce gate on, the block any `A` opened.
    fn host_submit_prompt(&self) -> Option<u64> {
        match self.cfg.host {
            Host::UnsignedInputStart => self
                .term
                .current_block()
                .filter(|_| {
                    self.term.shell_state() == aterm_core::terminal::ShellState::EnteringCommand
                })
                .map(|b| b.id),
            _ => self.term.prompt_in_front(),
        }
    }

    /// The editor context the host hands the predictor: the session, the
    /// nonce in use, DECCKM and whether control chords go out escaped, as
    /// aterm-gui reads them under the press's hold; a mutant's never changes,
    /// or never asks the keyboard mode, or asks Kitty's alone.
    fn host_context(&self) -> EditorContext {
        let term = &self.term;
        EditorContext::new(
            SESSION,
            match self.cfg.host {
                Host::StaleEditorContext => None,
                _ => term.shell_integration_nonce_in_use(),
            }
            .as_ref(),
            self.cfg.host != Host::StaleEditorContext && term.modes().application_cursor_keys,
            self.cfg.host != Host::StaleEditorContext && self.host_escaped_chords(),
        )
    }

    /// Whether the host says control chords go out escaped (R-KITTY):
    /// aterm-core's `control_chords_escaped`, or a mutant's that never asks, or
    /// asks Kitty's mode alone and misses modifyOtherKeys.
    fn host_escaped_chords(&self) -> bool {
        match self.cfg.host {
            Host::IgnoresKittyChords => false,
            Host::IgnoresModifyOtherKeys => self.term.kitty_reports_functional_keys(),
            _ => self.term.control_chords_escaped(),
        }
    }

    /// The edit key the host hands `Predictor::predict_edit` for `key`: a bare
    /// main-block arrow or Backspace, as aterm-gui classifies the unfolded event,
    /// and a learned key. A mutant also hands it a word motion, or a keypad
    /// arrow, as `←`.
    fn edit_key_of(&self, key: &Key) -> Option<EditKey> {
        match (key, self.cfg.host) {
            (Key::Backspace, _) => Some(EditKey::Backspace),
            (Key::Left, _) => Some(EditKey::Left),
            (Key::Right, _) => Some(EditKey::Right),
            (Key::Learned(k), _) => Some(*k),
            (Key::Unmodelled(Unmodelled::WordLeft), Host::WordMotionAsArrow)
            | (Key::Unmodelled(Unmodelled::KeypadLeft), Host::KeypadArrowAsArrow) => {
                Some(EditKey::Left)
            }
            _ => None,
        }
    }

    /// Note an alt-screen switch or a generation move the engine just made.
    fn observe_engine(&mut self) {
        let alt = self.term.is_alternate_screen();
        if alt != self.was_alt {
            self.was_alt = alt;
            self.screen_epoch += 1;
        }
        let generation = self.generation();
        if generation != self.last_generation {
            self.last_generation = generation;
            self.stats.generation_moves += 1;
        }
    }

    /// Re-derive which keystroke each pending guess belongs to. Guesses leave
    /// only from the front, from the back (`popped_back`: a Backspace cancelling
    /// our own) or all at once, and `armed` names the key a successful predict
    /// just appended; anything else is a predictor this file does not understand.
    fn sync(&mut self, armed: Option<KeyId>, popped_back: bool) -> Result<(), Violation> {
        let pending: Vec<(u16, u16, char)> = self
            .predictor
            .pending()
            .map(|p| (p.row, p.col, p.ch))
            .collect();
        if popped_back {
            self.mirror.pop_back();
        }
        let keep = pending.len() - usize::from(armed.is_some());
        while self.mirror.len() > keep {
            self.mirror.pop_front();
        }
        if let Some(k) = armed {
            let (row, col, ch) = *pending.last().expect("an armed guess is pending");
            let space = self.space();
            let seq = self.predictor.pending().last().map(|p| p.seq_for_test());
            if let Some(log) = self.keys.get_mut(k) {
                log.armed = Some(Armed {
                    row,
                    col,
                    ch,
                    space,
                });
                log.seq = seq;
            }
            self.mirror.push_back(k);
            self.stats.guesses_armed += 1;
        }
        let mirrored: Vec<(u16, u16, char)> = self
            .mirror
            .iter()
            .filter_map(|&k| self.keys.get(k)?.armed.map(|a| (a.row, a.col, a.ch)))
            .collect();
        if mirrored != pending {
            return Err(violation(
                "mirror",
                format!("pending guesses {pending:?} do not follow from the keys {mirrored:?}"),
            ));
        }
        Ok(())
    }

    /// The clock the host's frame-time calls read: the frame's, or, for the
    /// mutant that reads its clock only at a keypress, the last press's.
    fn host_clock(&self) -> Instant {
        match self.cfg.host {
            Host::StaleClock => self.last_press,
            _ => self.now,
        }
    }

    /// A press, routed the way aterm-gui routes it.
    fn press(&mut self, key: Key) -> Result<(), Violation> {
        self.note(|| format!("press {key:?}"));
        self.last_press = self.now;
        let id = self.keys.len();
        let secret = self.cfg.remote
            && self.program.noecho
            && !self.program.alt
            && self.noecho_visible == Some(self.program.noecho_serial)
            && matches!(key, Key::Glyph(_))
            && !self.program.inbox.iter().any(|&k| {
                matches!(
                    self.keys.get(k).map(|l| &l.key),
                    Some(Key::Enter | Key::Paste(_) | Key::Unmodelled(Unmodelled::Interrupt))
                )
            });
        if secret {
            self.stats.secrets_typed += 1;
        }
        self.keys.push(KeyLog {
            key: key.clone(),
            typed_at: self.now,
            armed: None,
            secret,
            shown: None,
            consumed_reliable: false,
            last_output_at: None,
            last_output_window: ECHO_DEADLINE,
            frames: Vec::new(),
            ends_epoch: false,
            seq: None,
            echo_landed: false,
            unprompted_at_press: self.unprompted_delivered,
            edit: None,
            chord: if matches!(key, Key::Learned(EditKey::CtrlA | EditKey::CtrlE)) {
                ChordForm::of(&self.term)
            } else {
                ChordForm::Byte
            },
        });
        // The edit key the host hands the predictor, if any: a bare arrow or
        // Backspace, as aterm-gui classifies them (a mutant also hands it a word
        // motion or a keypad arrow).
        let edit = self.edit_key_of(&key);
        if let Some(log) = self.keys.get_mut(id) {
            log.edit = edit;
        }
        // Whether the press reads the cursor row: asked before the terminal hold,
        // as aterm-gui asks it, so type-ahead never pays for the row (an edit key
        // reads it whenever the edit lane is on, and never with it off).
        let wants_row = (matches!(key, Key::Glyph(_)) && self.predictor.wants_key_row())
            || (edit.is_some() && self.editing);
        // The key-time sample, read under one terminal hold.
        let cursor = self.term.cursor();
        let coords = self.host_generation();
        let grid = (self.term.cols(), self.term.rows());
        let no_echo = self.host_alt();
        let has_row = wants_row && self.sample_press_row(cursor.row, cursor.col);
        // The integration's word on the buffer's start, the editor context,
        // and whether control chords go out escaped (Ctrl-A as Kitty's `CSI
        // u` or modifyOtherKeys' `CSI 27`), under the same hold; and, for a
        // line terminator, the prompt it is pressed at.
        let input_start = self.host_input_start();
        let context = self.host_context();
        let escaped_chords = self.host_escaped_chords();
        let submit_prompt = self.host_submit_prompt();
        // The engine's own deferred-wrap flag, read under the same hold.
        let wrap_pending = self.term.grid().pending_wrap();
        // The local termios: canonical with ECHO clear. Invisible behind a link.
        let tty_swallows = !self.cfg.remote && self.program.noecho && !self.program.alt;
        let now = self.now;
        let mode = self.host_mode();
        let shifts_before = self.predictor.events_for_test().shifts_armed;
        let ends_epoch = match key {
            _ if (matches!(key, Key::Glyph(_)) || edit.is_some()) && (no_echo || tty_swallows) => {
                // The no-echo gate: refused as input we did not model, and owed.
                self.predictor.note_unmodelled_input(now);
                self.sync(None, false)?;
                true
            }
            Key::Glyph(c) => {
                self.predictor.set_mode(mode);
                self.tell_ambiguous_width();
                let sample = KeySample {
                    cursor: (cursor.row, cursor.col),
                    coords,
                    grid,
                    row: has_row.then_some(self.press_row.as_slice()),
                    below: (has_row && !self.press_below.is_empty())
                        .then_some(self.press_below.as_slice()),
                    wrap_pending: Some(wrap_pending),
                    input_start,
                    context,
                    escaped_chords,
                };
                let accepted = self.predictor.predict_glyph(c, &sample, now);
                self.sync(accepted.then_some(id), false)?;
                !accepted
            }
            _ if let Some(k) = edit => {
                self.predictor.set_mode(mode);
                self.tell_ambiguous_width();
                let sample = KeySample {
                    cursor: (cursor.row, cursor.col),
                    coords,
                    grid,
                    row: has_row.then_some(self.press_row.as_slice()),
                    below: (has_row && !self.press_below.is_empty())
                        .then_some(self.press_below.as_slice()),
                    wrap_pending: Some(wrap_pending),
                    input_start,
                    context,
                    escaped_chords,
                };
                let outcome = self.predictor.predict_edit(k, &sample, now);
                match outcome {
                    EditOutcome::Armed => {
                        self.stats.edit_keys_armed += 1;
                        if self.program.cooked() && matches!(k, EditKey::Left | EditKey::Right) {
                            self.stats.cooked_motions_armed += 1;
                        }
                        self.sync(Some(id), false)?;
                    }
                    EditOutcome::Cancelled => self.sync(None, true)?,
                    EditOutcome::Declined => self.sync(None, false)?,
                }
                outcome == EditOutcome::Declined
            }
            Key::Enter => {
                self.last_enter = Some(id);
                if self.cfg.host != Host::IgnoresLineSubmits {
                    self.predictor.note_line_submit_at(submit_prompt, now);
                }
                self.sync(None, false)?;
                true
            }
            Key::Backspace
            | Key::Left
            | Key::Right
            | Key::Learned(_)
            | Key::Unmodelled(_)
            | Key::Paste(_) => {
                self.predictor.note_unmodelled_input(now);
                self.sync(None, false)?;
                true
            }
        };
        if let Some(log) = self.keys.get_mut(id) {
            log.ends_epoch = ends_epoch;
        }
        // A shift armed against overwrite mode, which never moves the tail: the
        // class ladder must keep it dark.
        if self.predictor.events_for_test().shifts_armed > shifts_before
            && self.program.editor == Editor::Overwrite
            && !self.program.cooked()
        {
            self.stats.overwrite_shifts_armed += 1;
        }
        self.program.inbox.push_back(id);
        Ok(())
    }

    /// Read the cursor row into `press_row` the way aterm-gui's press does
    /// (`predict_seen::seen_row`: the frame's extraction of that row, projected by
    /// [`seen_at`]). `TailBlind` reads everything right of the cursor as blank.
    /// Returns whether a row was read (always, on this host).
    fn sample_press_row(&mut self, row: u16, cursor_col: u16) -> bool {
        let rows = usize::from(self.term.rows());
        let cols = usize::from(self.term.cols());
        self.term
            .cell_frame_into(&mut self.press_scratch, rows, cols);
        self.press_row.clear();
        let blind = self.cfg.host == Host::TailBlind;
        let input = &self.press_scratch;
        self.press_row.extend((0..cols).map(|c| {
            if blind && c > usize::from(cursor_col) {
                Seen::Blank
            } else {
                seen_at(input, usize::from(row), c)
            }
        }));
        // The row under it, as aterm-gui reads it under the same hold (none on the
        // last row).
        self.press_below.clear();
        let below = usize::from(row) + 1;
        if below < rows {
            self.press_below
                .extend((0..cols).map(|c| seen_at(input, below, c)));
        }
        let read = (self.press_row.len() + self.press_below.len()) as u64;
        self.cost.row_samples += 1;
        self.cost.row_cells += read;
        self.cost.row_cells_max = self.cost.row_cells_max.max(read);
        true
    }

    /// The program reads up to `n` keys.
    fn program_step(&mut self, n: u8, choice: u32) {
        for i in 0..n {
            let Some(id) = self.program.inbox.pop_front() else {
                break;
            };
            let reliable = self.program.reliable;
            let Some((key, chord)) = self.keys.get(id).map(|log| (log.key.clone(), log.chord))
            else {
                continue;
            };
            self.program.reading_chord = chord;
            self.program
                .consume(id, &key, choice.rotate_left(u32::from(i) * 7));
            // Reliable when the editor knew its column as it read the key AND
            // the key's own output stayed inside the editor's model: an insert
            // from the parked slot is beyond it (`Program::insert` says so by
            // clearing `reliable`), so that key's echo is not one the fidelity
            // clauses can hold a guess to. (A prompt a key prints restores
            // `reliable` for the keys after it; the key itself was read without.)
            let stayed = self.program.reliable;
            if let Some(log) = self.keys.get_mut(id) {
                log.consumed_reliable = reliable && stayed;
            }
        }
    }

    /// One line of the step-by-step account, when one is kept.
    fn note(&mut self, line: impl FnOnce() -> String) {
        if let Some(trace) = self.trace.as_mut() {
            trace.push_str("  ");
            trace.push_str(&line());
            trace.push('\n');
        }
    }

    /// Feed `bytes` to the engine and the reference fold alike.
    fn feed(&mut self, bytes: &[u8]) {
        self.note(|| format!("deliver {:?}", String::from_utf8_lossy(bytes)));
        self.term.process(bytes);
        self.reference.process(bytes);
        self.observe_engine();
    }

    /// Deliver up to `n` bytes of output in flight, segment by segment, noting
    /// where each echoed glyph lands.
    fn deliver(&mut self, mut n: usize) -> Result<(), Violation> {
        while n > 0 {
            let Some(seg) = self.program.pipe.front().cloned() else {
                break;
            };
            let start = self.program.head_off;
            if start == 0 {
                match seg.kind {
                    SegKind::Echo(id) => {
                        // Where the glyph will be written: the cursor, or the next
                        // row's first cell if the terminal is parked in the deferred
                        // wrap (a scroll there moves the generation and voids the
                        // comparison by itself).
                        let cursor = self.term.cursor();
                        let (row, col) = if self.term.grid().pending_wrap() {
                            (cursor.row.saturating_add(1), 0)
                        } else {
                            (cursor.row, cursor.col)
                        };
                        let space = self.space();
                        self.landing = Some((id, row, col, space, self.unprompted_delivered));
                    }
                    SegKind::Unprompted | SegKind::NoEchoPrompt(_) | SegKind::Reply => {}
                }
            }
            let take = (seg.bytes.len() - start).min(n);
            let piece = seg
                .bytes
                .get(start..start + take)
                .unwrap_or_default()
                .to_vec();
            if matches!(seg.kind, SegKind::Unprompted | SegKind::NoEchoPrompt(_)) {
                self.unprompted_delivered += piece.len() as u64;
            }
            self.feed(&piece);
            n -= take;
            if start + take == seg.bytes.len() {
                self.program.pipe.pop_front();
                self.program.head_off = 0;
                let now = self.now;
                let window = self.predictor.glitch_window_for_test();
                if let Some(k) = seg.cause
                    && let Some(log) = self.keys.get_mut(k)
                {
                    log.last_output_at = Some(now);
                    log.last_output_window = window;
                    if now.saturating_duration_since(log.typed_at) > ECHO_DEADLINE.max(window) {
                        self.late_outputs.insert(k);
                    } else {
                        self.late_outputs.remove(&k);
                    }
                }
                match seg.kind {
                    SegKind::Echo(id) => {
                        if let Some(log) = self.keys.get_mut(id) {
                            log.echo_landed = true;
                        }
                        self.landed()?;
                    }
                    SegKind::NoEchoPrompt(serial)
                        if self.program.noecho && self.program.noecho_serial == serial =>
                    {
                        self.noecho_visible = Some(serial);
                    }
                    _ => {}
                }
                // The last byte of output this key caused has landed, and none
                // caused by a later key has: the screen its frames claimed.
                if let Some(k) = seg.cause
                    && !self
                        .program
                        .pipe
                        .front()
                        .is_some_and(|s| s.cause == Some(k))
                {
                    self.settled(k)?;
                }
            } else {
                self.program.head_off = start + take;
            }
        }
        if !self.program.noecho {
            self.noecho_visible = None;
        }
        Ok(())
    }

    /// THE LINK MODEL. A key typed before `id` whose output arrived later than
    /// its deadline is a LATE echo: it can stand in a later guess's cell, and with
    /// no `caused_by` on a PTY nothing positional can tell whose echo it is. The
    /// fidelity clauses claim nothing it can have touched: `id` itself, when the
    /// late output was still arriving after `id` was typed; and every key of an
    /// epoch in which it landed with a later key already typed, because standing
    /// in that key's cell it may have earned the proof `id` is displayed on (a
    /// declined glyph's echo 13 ms past its window credited the same glyph typed
    /// again, and the glyph after that was painted a cell early). An epoch
    /// boundary the harness can see for certain clears that taint
    /// (`KeyLog::ends_epoch`): the proof after it was earned after the late echo.
    ///
    /// The deadline is the predictor's own glitch window when the output arrived,
    /// never less than `ECHO_DEADLINE`: past it the predictor has given the key
    /// up (expired its guess, or let its owe lapse), so an echo inside it is one
    /// the predictor promised to account for. A fixed 250 ms deadline made the
    /// clauses vacuous on exactly the links prediction is for (an echo slower
    /// than 250 ms was always "late"), and hid that an expiry owed nothing.
    fn late_predecessor(&self, id: KeyId) -> bool {
        let before = self.keys.get(..id).unwrap_or_default();
        let Some(typed) = self.keys.get(id).map(|log| log.typed_at) else {
            return false;
        };
        // Only a key whose output arrived past its deadline can be one
        // (`late_outputs`); every other key fails `late` below.
        let mut late_before = self.late_outputs.range(..id).peekable();
        if late_before.peek().is_none() {
            return false;
        }
        let boundary = before
            .iter()
            .rev()
            .find(|log| log.ends_epoch)
            .map(|log| log.typed_at);
        late_before.any(|&j| {
            let Some(log) = before.get(j) else {
                return false;
            };
            log.last_output_at.is_some_and(|t| {
                let late = t.saturating_duration_since(log.typed_at)
                    > ECHO_DEADLINE.max(log.last_output_window);
                let over_a_later_key = self.keys.get(j + 1).is_some_and(|next| t >= next.typed_at);
                late && (t >= typed || (over_a_later_key && boundary.is_none_or(|b| t >= b)))
            })
        })
    }

    /// FRAME FIDELITY: every byte of output key `k` caused has reached the screen
    /// and nothing caused by a later key has. Every frame that displayed `k` as its
    /// newest painted ghost claimed to show this screen, and within the clause's
    /// scope it must be this screen, every cell of every row and the cursor.
    ///
    /// The scope: Adaptive; the same coordinate space; no unprompted output since
    /// the frame; every key from the frame's oldest pending guess through `k` read
    /// by a line editor that knew its column; no late predecessor (the link
    /// model). The DECORATION RULE: a cell the far end draws as decoration (a
    /// right prompt) may be shown stale, in a frame whose screen still had it or
    /// that lacked one the settled screen drew; that is counted, not a violation,
    /// because no typed glyph is involved and the decoration is the program's to
    /// move. A blank ghost (the cell a state chain's erase vacated) under
    /// decoration the settled screen drew is the second case: the frame showed
    /// no glyph there, only lacked what the far end then drew. A ghost GLYPH
    /// where the settled screen differs is a violation, decoration or not.
    fn settled(&mut self, k: KeyId) -> Result<(), Violation> {
        if !self.checks || self.cfg.mode != PredictMode::Adaptive {
            return Ok(());
        }
        let Some(log) = self.keys.get_mut(k) else {
            return Ok(());
        };
        let frames = std::mem::take(&mut log.frames);
        if frames.is_empty() || self.late_predecessor(k) {
            return Ok(());
        }
        let rows = usize::from(self.term.rows());
        let cols = usize::from(self.term.cols());
        let mut now_frame = RenderInput::empty();
        self.term.cell_frame_into(&mut now_frame, rows, cols);
        let truth = frame_text(&now_frame, rows, cols);
        let truth_decoration = frame_decoration(&now_frame, rows, cols);
        let cursor = self.term.cursor();
        let space = self.space();
        for f in frames {
            let reliable = self
                .keys
                .get(f.oldest..=k)
                .unwrap_or_default()
                .iter()
                .all(|j| j.consumed_reliable);
            if f.space != space || f.unprompted != self.unprompted_delivered || !reliable {
                continue;
            }
            if f.text.len() != truth.len() {
                continue; // a resize: another space, already excluded
            }
            self.stats.frame_fidelity_checks += 1;
            if f.learned.any {
                self.stats.learned_fidelity_checks += 1;
            }
            if f.learned.delete {
                self.stats.delete_fidelity_checks += 1;
            }
            let mut wrong = Vec::new();
            for (r, (shown, settled)) in f.text.iter().zip(&truth).enumerate() {
                for (c, (a, b)) in shown.iter().zip(settled).enumerate() {
                    if a == b {
                        continue;
                    }
                    let ghost = f.ghosts.contains(&(r, c));
                    let settled_decoration =
                        truth_decoration.get(r).and_then(|row| row.get(c)) == Some(&true);
                    let decoration = f.decoration.get(r).and_then(|row| row.get(c)) == Some(&true)
                        || settled_decoration;
                    // A VACATED ghost, the blank a state chain's erase painted
                    // over its glyph, shows no typed glyph: where the settled
                    // screen draws decoration on it (an autosuggestion the
                    // editor brings up in the erase's own output), the frame
                    // lacked a decoration the far end drew, as a frame with no
                    // ghost may.
                    let vacated = f.vacated.contains(&(r, c)) && settled_decoration;
                    if vacated {
                        self.stats.late_decoration += 1;
                    } else if !ghost && decoration {
                        self.stats.stale_decoration += 1;
                    } else {
                        wrong.push((r, c, a.0, b.0));
                    }
                }
            }
            let cursor_wrong = f.cursor.is_some_and(|fc| fc != (cursor.row, cursor.col));
            if !wrong.is_empty() || cursor_wrong {
                let text = |t: &[Vec<CellText>]| -> Vec<String> {
                    t.iter()
                        .map(|row| row.iter().map(|c| c.0).collect())
                        .collect()
                };
                return Err(violation(
                    "frame-fidelity",
                    format!(
                        "key {k} {:?} was displayed in a frame {:?} with the cursor at {:?}, \
                         but the screen it settles to is {:?} with the cursor at {:?}: cells \
                         (row, col, shown, settled) {wrong:?}",
                        self.keys.get(k).map(|l| &l.key),
                        text(&f.text),
                        f.cursor,
                        text(&truth),
                        (cursor.row, cursor.col)
                    ),
                ));
            }
        }
        Ok(())
    }

    /// ATTRIBUTION (every mode): the predictor credits a guess with its echo
    /// (measures the link with it, and may prove its epoch on it) only once that
    /// guess's own key has echoed. A PTY carries no `caused_by`, so the crate
    /// promises positional retirement exact for every echo inside its window
    /// instead; this holds it to that promise directly, where FIDELITY sees only
    /// a credit's consequence on a displayed guess, and only through a link
    /// model that takes the predictor's window on trust. A credit with no echo
    /// behind it is a sample of nothing: typing along an autosuggestion, each
    /// guess retired on the cell that already showed its glyph measured the
    /// link at a few milliseconds and collapsed the window that every owe is
    /// judged by (`cursor_advance_is_the_echo_s_evidence`).
    ///
    /// The scope is the link model's (no late predecessor, whose echo may be
    /// what stands in the cell, arrived or still overdue), a line editor that
    /// knows its column (a suggestion it drew past the margin left its cursor a
    /// row down, where no real editor leaves it), no unprompted output since the
    /// press (which can draw anything anywhere), and a frame on a BOUNDARY
    /// between keys' output:
    /// none caused by an earlier key part-way onto the screen. Inside a key's
    /// output (its echo landed, a suggestion it brings up drawn with the cursor
    /// not yet stepped back over it) the glyphs a guess typed ahead expects can
    /// stand in its cell with the cursor past it, and nothing positional can
    /// tell them from its echo: the `caused_by` a PTY does not carry. FRAME
    /// FIDELITY's settled base is the same scope. The same holds inside output
    /// nobody typed for, part-way when the credit is made though begun before
    /// the press: an unprompted autosuggestion drawn with the cursor not yet
    /// stepped back shows a typed glyph with the cursor past it (the forty-seed
    /// sweep's seed 33, a guess anchored on a cancelled glyph's cell under it).
    ///
    /// "Overdue" is judged by `window`, the predictor's window as it stood
    /// before the reconcile that made the credits: that is the window the
    /// predictor asked every owe under, and the credit's own sample may then
    /// widen it. Judged by the widened one, a cancelled glyph's erase unread for
    /// longer than the window the credit was made under (its typed owe lapsed,
    /// the cancelled echo still on screen) counted as on time, and the guess
    /// typed on the cancelled cell was held to an attribution the predictor
    /// never promised (two-hundred-seed sweep, seed index 173, case 267, a
    /// harness scope present on main before predictive editing's increment 3).
    fn attributed(&mut self, credited: &[u64], window: Duration) -> Result<(), Violation> {
        if !self.checks {
            return Ok(());
        }
        for &seq in credited {
            let Some(k) = self.keys.iter().rposition(|log| log.seq == Some(seq)) else {
                continue;
            };
            let Some(log) = self.keys.get(k) else {
                continue;
            };
            if log.unprompted_at_press != self.unprompted_delivered
                || !self.program.reliable
                || self.late_predecessor(k)
                || self.overdue_predecessor(k, window)
                || self.output_in_transit_before(k)
                || self.unprompted_in_transit()
            {
                continue;
            }
            self.stats.attribution_checks += 1;
            // Its own output begun counts: the far end may move the cursor for a
            // key before it echoes it (hiding a right prompt the line is about
            // to reach: save, jump right, erase, restore), and a space, whose
            // echo is a cursor advance, is then credited a few bytes early by
            // its own key's output, which is the right key.
            let own_output_begun = log.echo_landed
                || log.last_output_at.is_some()
                || (self.program.head_off > 0
                    && self
                        .program
                        .pipe
                        .front()
                        .is_some_and(|seg| seg.cause == Some(k)));
            if !own_output_begun {
                return Err(violation(
                    "attribution",
                    format!(
                        "key {k} {:?} was credited with its echo before any output of its own landed",
                        log.key
                    ),
                ));
            }
        }
        Ok(())
    }

    /// THE LINK MODEL at a moment rather than at an arrival: a key typed before
    /// `k` whose output has not all arrived (the program has not read it, or its
    /// output is still in the pipe) although its deadline has passed. Its output
    /// is late already, and wherever it lands it may stand in `k`'s cell, as an
    /// arrived late echo may (`late_predecessor`, which judges output only once it
    /// arrives, and so cannot see a credit made while it is still overdue): an
    /// erase the program has not read, its Backspace typed more than a window ago,
    /// leaves the cancelled glyph's echo and its cursor advance on screen, where a
    /// glyph typed again on that cell is retired by them.
    fn overdue_predecessor(&self, k: KeyId, window: Duration) -> bool {
        let window = ECHO_DEADLINE.max(window);
        let before = self.keys.get(..k).unwrap_or_default();
        // The keys whose output has not all arrived: the program's inbox and
        // the pipe's causes, read rather than every key typed before `k`.
        self.program
            .inbox
            .iter()
            .copied()
            .chain(self.program.pipe.iter().filter_map(|seg| seg.cause))
            .any(|j| {
                before
                    .get(j)
                    .is_some_and(|log| self.now.saturating_duration_since(log.typed_at) > window)
            })
    }

    /// Whether output nobody typed for is part-way onto the screen.
    fn unprompted_in_transit(&self) -> bool {
        self.program.head_off > 0
            && self.program.pipe.front().is_some_and(|seg| {
                matches!(seg.kind, SegKind::Unprompted | SegKind::NoEchoPrompt(_))
            })
    }

    /// Whether output caused by a key typed before `k` is part-way onto the
    /// screen: some of it delivered (a segment landed, or the one in delivery
    /// begun) and some still in the pipe.
    fn output_in_transit_before(&self, k: KeyId) -> bool {
        self.program.pipe.iter().enumerate().any(|(i, seg)| {
            seg.cause.is_some_and(|j| {
                j < k
                    && ((i == 0 && self.program.head_off > 0)
                        || self
                            .keys
                            .get(j)
                            .is_some_and(|log| log.last_output_at.is_some()))
            })
        })
    }

    /// FIDELITY: an echo just landed; a guess displayed for its key must have
    /// been on that cell, unless the space moved or unprompted output intervened.
    fn landed(&mut self) -> Result<(), Violation> {
        let Some((id, row, col, space, unprompted)) = self.landing.take() else {
            return Ok(());
        };
        if !self.checks || self.cfg.mode != PredictMode::Adaptive || space != self.space() {
            return Ok(());
        }
        let Some(log) = self.keys.get(id) else {
            return Ok(());
        };
        let Some(shown) = log.shown else {
            return Ok(());
        };
        if shown.space != space || shown.unprompted != unprompted || !log.consumed_reliable {
            return Ok(());
        }
        if self.late_predecessor(id) {
            return Ok(());
        }
        self.stats.fidelity_checks += 1;
        if (shown.row, shown.col) != (row, col) {
            return Err(violation(
                "fidelity",
                format!(
                    "key {id} {:?} was displayed at ({}, {}) but its echo landed at ({row}, {col})",
                    log.key, shown.row, shown.col
                ),
            ));
        }
        Ok(())
    }

    /// A frame: reconcile against the unpainted engine cells, then overlay, then
    /// check every clause of the theorem against what would be on glass.
    fn frame(&mut self) -> Result<(), Violation> {
        self.stats.frames += 1;
        let rows = usize::from(self.term.rows());
        let cols = usize::from(self.term.cols());
        self.term.cell_frame_into(&mut self.scratch, rows, cols);
        let cursor = self.term.cursor();
        let coords = self.host_generation();
        let alt = self.host_alt();
        let now = self.host_clock();
        self.predictor.set_mode(self.host_mode());
        // The window this turn's credits are judged under: as it stood before
        // them (`Sim::attributed`).
        let window = self.predictor.glitch_window_for_test();
        // THE COST of the frame, counted: the cells the reconcile asked the
        // host for, and the ops the reconcile and the overlay folded.
        let folded = self.predictor.events_for_test().ops_folded;
        let mut observed = 0u64;
        {
            let input = &self.scratch;
            let blind = self.cfg.host == Host::TailBlind;
            self.predictor.reconcile_seen(
                Some((cursor.row, cursor.col)),
                coords,
                alt,
                now,
                |r, c| {
                    observed += 1;
                    // After an echo the cursor stands on the first cell of the
                    // tail, so the blind host reads from it rightward.
                    if blind && r == cursor.row && c >= cursor.col {
                        Seen::Blank
                    } else {
                        seen_at(input, usize::from(r), usize::from(c))
                    }
                },
            );
        }
        let credited = self.predictor.take_credited_for_test();
        self.attributed(&credited, window)?;
        self.sync(None, false)?;
        self.read_oracle_base();
        let (shown, kinds): (Vec<(u16, u16, char)>, Vec<GhostCell>) = self
            .predictor
            .overlay(now)
            .iter()
            .map(|p| ((p.row, p.col, p.ch), p.ghost()))
            .unzip();
        let folds = self.predictor.events_for_test().ops_folded - folded;
        self.cost.reconciles += 1;
        self.cost.observe_calls += observed;
        self.cost.observe_max = self.cost.observe_max.max(observed);
        self.cost.frame_folds += folds;
        self.cost.frame_folds_max = self.cost.frame_folds_max.max(folds);
        self.sync(None, false)?;
        // A state chain's frame is its displayed state, painted all or none.
        let chain_cursor = self.predictor.overlay_cursor();
        if chain_cursor.is_some() || (self.predictor.engaged_for_test() && !shown.is_empty()) {
            return self.frame_chain(&shown, &kinds, chain_cursor);
        }
        // What the host puts on glass: the overlay, each ghost with its key; a host
        // that skips the erase frame leaves the last frame's ghosts instead.
        let current: Vec<(KeyId, u16, u16, char)> = self
            .mirror
            .iter()
            .zip(&shown)
            .map(|(&id, &(row, col, ch))| (id, row, col, ch))
            .collect();
        let glass = if self.cfg.host == Host::KeepsStaleGhosts && shown.is_empty() {
            self.glass.clone()
        } else {
            current
        };
        // What goes on glass: `A` with the ghosts painted (`None` when nothing is).
        let mut painted = vec![false; glass.len()];
        let displayed = (!glass.is_empty()).then(|| {
            let mut d = self.scratch.clone();
            let blank = self.term.implicit_blank_render_cell();
            for (&(_, row, col, ch), painted) in glass.iter().zip(&mut painted) {
                *painted = self.paint(&mut d, (row, col, ch), cols, blank);
            }
            d
        });
        self.stats.ghosts_refused += painted.iter().filter(|&&p| !p).count() as u64;
        if self.trace.is_some() {
            let screen: Vec<String> = self
                .scratch
                .cells
                .iter()
                .map(|cells| cells.iter().map(|c| c.ch).collect())
                .collect();
            let pending: Vec<(u16, u16, char)> = self
                .predictor
                .pending()
                .map(|p| (p.row, p.col, p.ch))
                .collect();
            self.note(|| {
                format!(
                    "frame cursor={cursor:?} screen={screen:?} pending={pending:?} shown={shown:?}"
                )
            });
        }
        let checked = self.check(&shown, &glass, displayed.as_ref(), &painted);
        // On glass: `D`, and the cursor the GUI draws one past the newest ghost
        // it painted (with none painted, `D = A` and the engine's cursor).
        let mut rows: Vec<u16> = glass
            .iter()
            .zip(&painted)
            .filter(|&(_, &p)| p)
            .map(|(&(_, row, _, _), _)| row)
            .collect();
        rows.dedup();
        let newest = glass
            .iter()
            .zip(&painted)
            .rfind(|&(_, &p)| p)
            .map(|(&(_, row, col, _), _)| (row, col));
        self.glass_frame = match (displayed, newest) {
            (Some(frame), Some((row, col))) => Some(OnGlass {
                frame,
                cursor: displayed_cursor(row, col, self.term.cols()),
                rows,
            }),
            _ => None,
        };
        self.glass = glass;
        checked
    }

    /// After a reconcile that retired a state chain to a state the screen
    /// matched, read the PROVENANCE oracle's base from `A` (see [`OracleBase`]).
    fn read_oracle_base(&mut self) {
        if !self.predictor.chain_matched_for_test() {
            return;
        }
        let Some((row, lo, hi, _)) = self.predictor.edit_line_for_test() else {
            return;
        };
        let cursor = self.term.cursor();
        let text: Option<Vec<char>> = (lo..hi)
            .map(
                |c| match seen_at(&self.scratch, usize::from(row), usize::from(c)) {
                    Seen::Blank => Some(' '),
                    Seen::Glyph(g) => Some(g),
                    Seen::Opaque(_) => None,
                },
            )
            .collect();
        self.oracle = text.filter(|_| cursor.row == row).map(|text| OracleBase {
            front: self.mirror.front().copied(),
            row,
            lo,
            text,
            cursor: cursor.col,
        });
    }

    /// A frame whose overlay is a state chain's displayed state (`shown`, its
    /// cells; `chain_cursor`, its cursor): painted ALL OR NONE, with the
    /// painter's own refusal asked first (`prediction_ghost_paintable`), and its
    /// cursor drawn only when every cell was painted, as aterm-gui paints one;
    /// with no cursor from the predictor, the host's glyph-lane cursor, one past
    /// the newest ghost it painted. A mutant paints what it can, or draws the
    /// cursor regardless, or always draws the glyph lane's.
    fn frame_chain(
        &mut self,
        shown: &[(u16, u16, char)],
        kinds: &[GhostCell],
        chain_cursor: Option<(u16, u16)>,
    ) -> Result<(), Violation> {
        let visible = self.predictor.visible_ops_for_test();
        let ids: Vec<KeyId> = self.mirror.iter().copied().take(visible).collect();
        let Some(&newest) = ids.last() else {
            if !self.checks {
                // A predictor with rules off, measured: what it shows with no op
                // displayed is nothing a host paints.
                self.glass_frame = None;
                self.glass.clear();
                return Ok(());
            }
            return Err(violation(
                "provenance",
                format!("a chain's cursor {chain_cursor:?} is drawn with no op displayed"),
            ));
        };
        let mut d = self.scratch.clone();
        let blank = self.term.implicit_blank_render_cell();
        let (painted, drawn) = paint_chain(
            &mut d,
            (shown, kinds),
            chain_cursor,
            (self.term.cols(), blank),
            self.cfg.host,
        );
        let cells: Vec<(u16, u16)> = shown
            .iter()
            .zip(&painted)
            .filter(|&(_, &p)| p)
            .map(|(&(r, c, _), _)| (r, c))
            .collect();
        if self.trace.is_some() {
            let screen: Vec<String> = self
                .scratch
                .cells
                .iter()
                .map(|cells| cells.iter().map(|c| c.ch).collect())
                .collect();
            let line = self.predictor.edit_line_for_test();
            self.note(|| {
                format!(
                    "frame (chain {line:?}, {visible} visible) screen={screen:?} shown={shown:?} \
                     cursor={chain_cursor:?} drawn={drawn:?}"
                )
            });
        }
        let checked = self.check_chain(&ids, shown, &painted, (chain_cursor, drawn), &d);
        if checked.is_ok() && (drawn.is_some() || !cells.is_empty()) {
            self.stats.edit_frames_displaying += 1;
            // A shift on glass: text the chain's state moved, painted as the
            // cell it came from.
            if kinds
                .iter()
                .zip(&painted)
                .any(|(&k, &p)| p && matches!(k, GhostCell::Moved { .. }))
            {
                self.stats.shifts_displayed += 1;
                if self.cfg.mode == PredictMode::Adaptive
                    && self.program.editor == Editor::Overwrite
                    && !self.program.cooked()
                {
                    self.stats.overwrite_shifts_displayed += 1;
                }
            }
            if ids.iter().any(|&k| {
                self.keys
                    .get(k)
                    .is_some_and(|l| matches!(l.edit, Some(EditKey::Left | EditKey::Right)))
            }) {
                self.stats.motions_displayed += 1;
            }
            let edit_of = |k: &KeyId| self.keys.get(*k).and_then(|l| l.edit);
            let learned = LearnedShown {
                any: ids.iter().any(|k| {
                    matches!(
                        edit_of(k),
                        Some(
                            EditKey::Home
                                | EditKey::End
                                | EditKey::CtrlA
                                | EditKey::CtrlE
                                | EditKey::Delete
                        )
                    )
                }),
                delete: ids.iter().any(|k| edit_of(k) == Some(EditKey::Delete)),
            };
            if learned.any {
                self.stats.line_edge_displayed += 1;
            }
            if learned.delete {
                self.stats.delete_displayed += 1;
            }
            if self
                .last_enter
                .is_some_and(|enter| ids.first().is_some_and(|&k| k < enter))
            {
                self.stats.carried_chains += 1;
            }
            if self.cfg.mode == PredictMode::Adaptive {
                self.record_frame(&d, &cells, newest, drawn, learned);
            }
        }
        self.glass = shown
            .iter()
            .zip(&painted)
            .filter(|&(_, &p)| p)
            .map(|(&(r, c, ch), _)| (newest, r, c, ch))
            .collect();
        // On glass: `D` and the chain's cursor, when the span was painted or the
        // cursor drawn (a motion paints no cell).
        let mut rows: Vec<u16> = cells.iter().map(|&(r, _)| r).collect();
        rows.dedup();
        self.glass_frame = (drawn.is_some() || !cells.is_empty()).then_some(OnGlass {
            frame: d,
            cursor: drawn,
            rows,
        });
        checked
    }

    /// The clauses on a state chain's frame: `ids` the keys of its visible ops,
    /// `shown` its cells and `painted` which were painted into `d`, the cursor
    /// the predictor gave it and the one drawn for it.
    ///
    /// * PROVENANCE: no op displayed over the alternate screen or outside the
    ///   space it was armed in; and the ORACLE (`oracle_check`).
    /// * SECRECY (Adaptive): no op of a key typed at a visible no-echo prompt.
    /// * TIMELINESS: every displayed op typed no longer ago than the window.
    /// * LOCALITY: every cell on the chain's row, from `lo`, inside the grid,
    ///   and `D` differs from `A` only there, never on half of a wide glyph.
    /// * ATOMICITY: every cell painted, or none and the engine's cursor drawn.
    /// * CURSOR LOCALITY: the drawn cursor inside the grid, never on a wide
    ///   glyph's right half.
    fn check_chain(
        &mut self,
        ids: &[KeyId],
        shown: &[(u16, u16, char)],
        painted: &[bool],
        (chain_cursor, drawn): ChainCursors,
        d: &RenderInput,
    ) -> Result<(), Violation> {
        if !self.checks {
            return Ok(());
        }
        let (rows, cols) = (self.term.rows(), self.term.cols());
        let alt = self.term.is_alternate_screen();
        let space = self.space();
        let window = self.predictor.glitch_window_for_test();
        for &id in ids {
            let log = self.keys.get(id).expect("a displayed op's key is logged");
            let armed = log.armed.expect("a displayed op was armed");
            if alt || armed.space != space {
                return Err(violation(
                    "provenance",
                    format!(
                        "key {id} {:?}, armed in {:?}, is displayed in {space:?} (alternate \
                         screen: {alt})",
                        log.key, armed.space
                    ),
                ));
            }
            if self.cfg.mode == PredictMode::Adaptive && log.secret {
                return Err(violation(
                    "secrecy",
                    format!(
                        "key {id} {:?}, typed at a visible no-echo prompt, is displayed",
                        log.key
                    ),
                ));
            }
            self.stats.timeliness_checks += 1;
            let age = self.now.saturating_duration_since(log.typed_at);
            if age > window {
                return Err(violation(
                    "timeliness",
                    format!(
                        "key {id} {:?} is displayed {age:?} after its press, past the {window:?} window",
                        log.key
                    ),
                ));
            }
        }
        let line = self.predictor.edit_line_for_test();
        for &(r, c, ch) in shown {
            let on_line = line.is_some_and(|(row, lo, _, _)| r == row && c >= lo);
            if !on_line || c >= cols {
                return Err(violation(
                    "locality",
                    format!("a chain's cell {ch:?} at ({r}, {c}) is off its line {line:?}"),
                ));
            }
        }
        let cells: Vec<(u16, u16, char)> = shown
            .iter()
            .zip(painted)
            .filter(|&(_, &p)| p)
            .map(|(&g, _)| g)
            .collect();
        self.locality_on_glass(&cells, d)?;
        chain_paint_clauses(shown, painted, drawn, d, (rows, cols))?;
        if drawn.is_some() && cells.len() == shown.len() {
            self.oracle_check(ids, d, chain_cursor)?;
        }
        Ok(())
    }

    /// THE PROVENANCE ORACLE: a displayed chain is the fold of its displayed
    /// keys over what `A` showed. From the base [`read_oracle_base`] took from `A`
    /// for the chain's oldest pending op, fold the keys of its visible ops (each
    /// as the host handed it: a glyph inserted at the cursor, `←` and `→` one
    /// cell, a Backspace erasing the glyph before the cursor), independently of
    /// the predictor's fold; then `D` on the chain's row, from `lo` to the
    /// furthest text end any of those states reaches, must be the folded text
    /// (a blank on each vacated cell), and the predictor's cursor the folded
    /// one (the cursor the host draws is FRAME FIDELITY's).
    /// Skipped while no base was read for that op (a chain whose first frame
    /// waited in transit).
    fn oracle_check(
        &mut self,
        ids: &[KeyId],
        d: &RenderInput,
        chain_cursor: Option<(u16, u16)>,
    ) -> Result<(), Violation> {
        let front = self.mirror.front().copied();
        let Some(base) = self.oracle.as_ref().filter(|b| b.front == front) else {
            return Ok(());
        };
        let (row, lo) = (base.row, base.lo);
        let mut text = base.text.clone();
        let mut cursor = base.cursor;
        let end = |text: &[char]| lo.saturating_add(u16::try_from(text.len()).unwrap_or(u16::MAX));
        let mut far = end(&text);
        for &id in ids {
            let log = self.keys.get(id).expect("a displayed op's key is logged");
            let at = usize::from(cursor.saturating_sub(lo));
            match (log.edit, &log.key) {
                (Some(EditKey::Left), _) => cursor = cursor.saturating_sub(1),
                (Some(EditKey::Right), _) => cursor = cursor.saturating_add(1),
                (Some(EditKey::Backspace), _) => {
                    if let Some(i) = at.checked_sub(1).filter(|&i| i < text.len()) {
                        text.remove(i);
                    }
                    cursor = cursor.saturating_sub(1);
                }
                // The learned keys as their canonical bindings: the line's
                // start, its end, the glyph under the cursor erased.
                (Some(EditKey::Home | EditKey::CtrlA), _) => cursor = lo,
                (Some(EditKey::End | EditKey::CtrlE), _) => cursor = end(&text),
                (Some(EditKey::Delete), _) => {
                    if at < text.len() {
                        text.remove(at);
                    }
                }
                (None, Key::Glyph(c)) => {
                    text.insert(at.min(text.len()), *c);
                    cursor = cursor.saturating_add(1);
                }
                (_, key) => {
                    return Err(violation(
                        "provenance",
                        format!(
                            "key {id} {key:?}, handed as neither a glyph nor an edit key, is displayed"
                        ),
                    ));
                }
            }
            far = far.max(end(&text));
        }
        self.stats.oracle_checks += 1;
        let r = usize::from(row);
        for c in lo..far {
            let want = text.get(usize::from(c - lo)).copied().unwrap_or(' ');
            let got = d
                .cells
                .get(r)
                .and_then(|cells| cells.get(d.visual_col(r, usize::from(c))))
                .map_or(' ', |cell| cell.ch);
            if got != want {
                return Err(violation(
                    "provenance",
                    format!(
                        "the chain displays {got:?} at ({row}, {c}), but its keys folded over \
                         the screen give {want:?}: {:?} from {:?}",
                        text, base.text
                    ),
                ));
            }
        }
        if chain_cursor.is_some_and(|c| c != (row, cursor)) {
            return Err(violation(
                "provenance",
                format!(
                    "the chain's cursor is {chain_cursor:?}, its keys fold to ({row}, {cursor})"
                ),
            ));
        }
        Ok(())
    }

    /// Paint one ghost the way this host does: aterm's painter, or a mutant's.
    fn paint(
        &self,
        d: &mut RenderInput,
        (row, col, ch): (u16, u16, char),
        cols: usize,
        blank: RenderCell,
    ) -> bool {
        let (row, col) = (usize::from(row), usize::from(col));
        match self.cfg.host {
            Host::PaintsBesideTheGuess => d.paint_prediction_ghost(row, col + 1, ch, cols, blank),
            Host::PaintsOverWideGlyphs => {
                let Some(cells) = d.cells.get_mut(row).filter(|_| col < cols) else {
                    return false;
                };
                if cells.len() <= col {
                    cells.resize(col + 1, blank);
                }
                cells.get_mut(col).is_some_and(|cell| {
                    cell.ch = ch;
                    cell.wide = false;
                    true
                })
            }
            _ => d.paint_prediction_ghost(row, col, ch, cols, blank),
        }
    }

    fn check(
        &mut self,
        shown: &[(u16, u16, char)],
        glass: &[(KeyId, u16, u16, char)],
        displayed: Option<&RenderInput>,
        painted: &[bool],
    ) -> Result<(), Violation> {
        let rows = self.term.rows();
        let cols = self.term.cols();
        let alt = self.term.is_alternate_screen();
        let space = self.space();
        if alt {
            self.stats.alt_frames += 1;
        }
        if !shown.is_empty() {
            self.stats.frames_displaying += 1;
            self.stats.max_displayed = self.stats.max_displayed.max(shown.len());
        }
        if !self.checks {
            return Ok(());
        }
        // The overlay is a leading run of the pending guesses (the mirror).
        let ids: Vec<KeyId> = self.mirror.iter().copied().take(shown.len()).collect();
        if ids.len() != shown.len() {
            return Err(violation(
                "provenance",
                format!("{} guesses displayed, {} pending", shown.len(), ids.len()),
            ));
        }
        // LOCALITY: inside the grid, one guess per cell.
        for (i, &(row, col, ch)) in shown.iter().enumerate() {
            if row >= rows || col >= cols {
                return Err(violation(
                    "locality",
                    format!("{ch:?} displayed at ({row}, {col}) outside the {rows}x{cols} grid"),
                ));
            }
            if let Some(&(_, _, other)) = shown
                .get(..i)
                .unwrap_or_default()
                .iter()
                .find(|&&(r, c, _)| (r, c) == (row, col))
            {
                return Err(violation(
                    "locality",
                    format!(
                        "{ch:?} displayed over the guess {other:?} on the same cell ({row}, {col}): {shown:?}"
                    ),
                ));
            }
        }
        // PROVENANCE: each displayed guess is its keystroke's, in this space.
        for (&id, &(row, col, ch)) in ids.iter().zip(shown) {
            let log = self.keys.get(id).expect("mirrored keys are logged");
            let armed = log.armed.expect("mirrored keys armed a guess");
            if (armed.row, armed.col, armed.ch) != (row, col, ch) {
                return Err(violation(
                    "provenance",
                    format!("key {id} armed {armed:?} but {ch:?} is displayed at ({row}, {col})"),
                ));
            }
            if alt {
                return Err(violation(
                    "provenance",
                    format!(
                        "key {id} {ch:?} displayed at ({row}, {col}) over the alternate screen"
                    ),
                ));
            }
            if armed.space != space {
                return Err(violation(
                    "provenance",
                    format!(
                        "key {id} {ch:?} armed in {:?} is displayed at ({row}, {col}) in {space:?}: \
                         the row it predicted is no longer there",
                        armed.space
                    ),
                ));
            }
            // SECRECY (Adaptive): never a key typed into a visible no-echo prompt.
            if self.cfg.mode == PredictMode::Adaptive && log.secret {
                return Err(violation(
                    "secrecy",
                    format!(
                        "key {id} {ch:?}, typed at a visible no-echo prompt, is displayed at ({row}, {col})"
                    ),
                ));
            }
        }
        // TIMELINESS (every mode): every ghost on glass is a guess still pending,
        // typed no longer ago than the predictor's window, so nothing flushed or
        // expired is shown after the flush, and a wrong cell lives at most one
        // window (2 s at the ceiling). `KeepsStaleGhosts` breaks the first half,
        // `StaleClock` the second. (The drawn cursor is this file's model of the
        // GUI's, `displayed_cursor`, one past the newest painted ghost; FRAME
        // FIDELITY compares it with the settled cursor. Nothing here reads
        // aterm-gui's cursor code.)
        let window = self.predictor.glitch_window_for_test();
        for &(id, row, col, ch) in glass {
            self.stats.timeliness_checks += 1;
            let log = self.keys.get(id).expect("a ghost's key is logged");
            if !self.mirror.contains(&id) {
                return Err(violation(
                    "timeliness",
                    format!(
                        "key {id} {ch:?} is on glass at ({row}, {col}), but its guess is no \
                         longer pending: flushed or expired"
                    ),
                ));
            }
            let age = self.now.saturating_duration_since(log.typed_at);
            if age > window {
                return Err(violation(
                    "timeliness",
                    format!(
                        "key {id} {ch:?} is on glass at ({row}, {col}) {age:?} after its press, \
                         past the {window:?} window"
                    ),
                ));
            }
        }
        // LOCALITY on glass: `D`, painted by the host's painter, differs from `A`
        // only at the frame cells of `O`, and never on half of a wide glyph.
        if let Some(d) = displayed {
            let on_glass: Vec<(u16, u16, char)> = glass
                .iter()
                .map(|&(_, row, col, ch)| (row, col, ch))
                .collect();
            self.locality_on_glass(&on_glass, d)?;
        }
        // Record where each Adaptive guess was displayed, for FIDELITY: only the
        // ones the painter put on glass (a ghost refused on a wide glyph was not
        // seen, whatever the overlay said).
        let unprompted = self.unprompted_delivered;
        for &(id, row, col, _) in glass
            .iter()
            .zip(painted)
            .filter(|&(_, &painted)| painted)
            .map(|(g, _)| g)
        {
            if let Some(log) = self.keys.get_mut(id) {
                log.shown = Some(Shown {
                    row,
                    col,
                    space,
                    unprompted,
                });
            }
        }
        // …and, for FRAME FIDELITY, the whole frame, kept for its newest painted
        // ghost's key until that key's output settles.
        if let (Some(d), Some(&(newest, row, col, _))) = (
            displayed,
            glass
                .iter()
                .zip(painted)
                .rfind(|&(_, &painted)| painted)
                .map(|(g, _)| g),
        ) && self.cfg.mode == PredictMode::Adaptive
        {
            let cells: Vec<(u16, u16)> = glass
                .iter()
                .zip(painted)
                .filter(|&(_, &painted)| painted)
                .map(|(&(_, r, c, _), _)| (r, c))
                .collect();
            let cursor = displayed_cursor(row, col, self.term.cols());
            self.record_frame(d, &cells, newest, cursor, LearnedShown::default());
        }
        Ok(())
    }

    /// Keep a displayed frame for FRAME FIDELITY (see `settled`), under the key of
    /// its newest painted ghost at `(row, col)`.
    fn record_frame(
        &mut self,
        d: &RenderInput,
        painted: &[(u16, u16)],
        newest: KeyId,
        cursor: Option<(u16, u16)>,
        learned: LearnedShown,
    ) {
        if !self.checks {
            return;
        }
        // A SETTLED BASE: every key typed before the oldest pending guess has
        // been read and all its output has landed. A frame caught mid-way
        // through an earlier key's output (a cancelled glyph drawn and about to
        // be erased) shows the program's own transit at cells no ghost touches,
        // which the user sees with or without prediction; FIDELITY still holds
        // each ghost's cell there, and this clause claims the rest only on a
        // base that is not about to move under it.
        let oldest = self.mirror.front().copied().unwrap_or(newest);
        let base_settled = !self.program.inbox.iter().any(|&k| k < oldest)
            && !self
                .program
                .pipe
                .iter()
                .any(|seg| seg.cause.is_some_and(|k| k < oldest));
        if !base_settled {
            return;
        }
        let rows = usize::from(self.term.rows());
        let cols = usize::from(self.term.cols());
        let decoration = frame_decoration(&self.scratch, rows, cols);
        let ghosts: Vec<(usize, usize)> = painted
            .iter()
            .map(|&(r, c)| {
                let r = usize::from(r);
                (r, self.scratch.visual_col(r, usize::from(c)))
            })
            .collect();
        if ghosts.iter().any(|&(r, c)| {
            decoration
                .get(r)
                .is_some_and(|line| line.iter().skip(c).any(|&d| d))
        }) {
            self.stats.rprompt_frames += 1;
        }
        let text = frame_text(d, rows, cols);
        let cell =
            |t: &[Vec<CellText>], (r, c): (usize, usize)| t.get(r).and_then(|l| l.get(c)).cloned();
        // What was under a ghost, read at the ghosts alone.
        let under =
            |(r, c): (usize, usize)| (r < rows && c < cols).then(|| cell_text(&self.scratch, r, c));
        let vacated = ghosts
            .iter()
            .copied()
            .filter(|&at| {
                cell(&text, at).is_some_and(|t| t.0 == ' ' && t.2.is_none())
                    && under(at).is_some_and(|u| u.0 != ' ')
            })
            .collect();
        let frame = FrameShown {
            text,
            decoration,
            ghosts,
            vacated,
            cursor,
            space: self.space(),
            unprompted: self.unprompted_delivered,
            oldest,
            learned,
        };
        if let Some(log) = self.keys.get_mut(newest)
            && log.frames.last().is_none_or(|last| {
                last.text != frame.text
                    || last.cursor != frame.cursor
                    || last.decoration != frame.decoration
                    || last.space != frame.space
                    || last.unprompted != frame.unprompted
                    || last.oldest != frame.oldest
                    || last.learned != frame.learned
            })
        {
            log.frames.push(frame);
        }
    }

    /// Compare `D` with `A` cell by cell (every field, the grapheme cluster and
    /// the combining marks; a cell past a trimmed row's end is the implicit
    /// blank the renderer fills it with). A changed cell must be the frame cell
    /// of a displayed guess (found through `visual_col`, as reconcile reads it),
    /// and must not be either half of a wide glyph in `A`: the renderer draws the
    /// glyph as one unit, so the other half changes with it.
    fn locality_on_glass(
        &self,
        shown: &[(u16, u16, char)],
        d: &RenderInput,
    ) -> Result<(), Violation> {
        let a = &self.scratch;
        let blank = self.term.implicit_blank_render_cell();
        let guessed: Vec<(usize, usize)> = shown
            .iter()
            .map(|&(r, c, _)| {
                let r = usize::from(r);
                (r, a.visual_col(r, usize::from(c)))
            })
            .collect();
        let cell = |f: &RenderInput, r: usize, c: usize| -> RenderCell {
            f.cells
                .get(r)
                .and_then(|row| row.get(c))
                .copied()
                .unwrap_or(blank)
        };
        let marks = |f: &RenderInput, r: usize, c: usize| {
            let cluster = f
                .clusters
                .get(r)
                .and_then(|row| row.iter().find(|(at, _)| *at == c))
                .map(|(_, s)| s.clone());
            let combining = f
                .combining
                .get(r)
                .and_then(|row| row.iter().find(|(at, _)| *at == c))
                .map(|(_, m)| m.clone());
            (cluster, combining)
        };
        for r in 0..a.cells.len().max(d.cells.len()) {
            let width = [a, d]
                .iter()
                .map(|f| f.cells.get(r).map_or(0, Vec::len))
                .max()
                .unwrap_or(0);
            for c in 0..width {
                let (ca, cd) = (cell(a, r, c), cell(d, r, c));
                if ca == cd && marks(a, r, c) == marks(d, r, c) {
                    continue;
                }
                if !guessed.contains(&(r, c)) {
                    return Err(violation(
                        "locality",
                        format!(
                            "({r}, {c}) shows {:?} over the authoritative {:?}, a cell no \
                             displayed guess predicted: {shown:?}",
                            cd.ch, ca.ch
                        ),
                    ));
                }
                if ca.wide || cell(a, r, c + 1).wide {
                    return Err(violation(
                        "locality",
                        format!(
                            "a ghost {:?} at ({r}, {c}) was painted over half of a wide glyph: \
                             the other half, which no guess predicted, changes with it",
                            cd.ch
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The host changes the coordinate space under the predictor.
    fn host_reset(&mut self, resize: bool) -> Result<(), Violation> {
        if resize {
            let (rows, cols) = if self.term.rows() == ROWS {
                ALT_GEOMETRY
            } else {
                (ROWS, COLS)
            };
            self.term.resize(rows, cols);
            self.reference.resize(rows, cols);
            self.predictor.reset();
            // SIGWINCH: the far end learns the width; its column is a guess now,
            // and so is where its right prompt went.
            self.program.cols = usize::from(cols);
            self.program.reliable = false;
            self.program.rprompt = None;
            self.program.suggestion.clear();
        } else {
            let estimate = self.predictor.take_link_estimate();
            self.predictor.restore_link_estimate(estimate);
        }
        self.screen_epoch += 1;
        self.observe_engine();
        self.sync(None, false)
    }

    /// The clock moves; a due deadline wakes the host's expiry sweep and its
    /// erase frame.
    fn advance(&mut self, ms: u32) -> Result<(), Violation> {
        self.advance_by(Duration::from_millis(u64::from(ms)))
            .map(|_drew| ())
    }

    /// [`Self::advance`] by any duration: the measurement's link runs in
    /// microseconds.
    fn advance_by(&mut self, by: Duration) -> Result<bool, Violation> {
        self.now += by;
        let now = self.host_clock();
        if self.predictor.next_deadline().is_some_and(|d| d <= now) {
            self.predictor.expire_due(now);
            self.sync(None, false)?;
            self.frame()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Resolve and run one op.
    fn step(&mut self, op: Op) -> Result<(), Violation> {
        self.note(|| format!("{op:?}"));
        match op {
            Op::Key(class, detail) => {
                let key = self.key_for(class, detail);
                self.press(key)?;
                // Most presses are followed by a redraw before the next one.
                if detail % 4 != 0 {
                    self.frame()?;
                }
            }
            Op::Program(n, choice) => self.program_step(n, choice),
            Op::Deliver(part) => {
                let total = self.program.pending_bytes();
                let n = if part >= 1024 {
                    total
                } else {
                    (total * usize::from(part) / 1024).max(1)
                };
                self.deliver(n)?;
                self.frame()?;
            }
            Op::Unprompted(what, param) => {
                self.program.unprompted_output(what, param);
                // A new shell: the host re-keys the integration for it, as
                // aterm-gui authorizes the nonce of a shell it spawns.
                if self.program.shell != self.shell {
                    self.shell = self.program.shell;
                    let key = nonce(self.shell);
                    self.term.authorize_shell_integration(key);
                    self.reference.authorize_shell_integration(key);
                }
            }
            Op::Advance(ms) => self.advance(ms)?,
            Op::Frame => self.frame()?,
            Op::HostReset(resize) => self.host_reset(resize)?,
        }
        Ok(())
    }

    /// Which key an op presses: a secret is drawn from digits, which nothing else
    /// on screen ever shows, everything else from letters, space, an accent, a
    /// wide and a combining glyph.
    fn key_for(&self, class: u32, detail: u32) -> Key {
        let secret_context = self.cfg.remote
            && self.program.noecho
            && !self.program.alt
            && self.noecho_visible == Some(self.program.noecho_serial);
        match class % 100 {
            0..=69 => {
                if secret_context {
                    Key::Glyph(char::from(b'0' + (detail % 10) as u8))
                } else if self.cfg.bidi && (detail >> 24) % 8 == 5 {
                    // Now and then a right-to-left letter (`Config::bidi`): a
                    // detail `glyph` never builds, so a named case's keys keep
                    // their meaning.
                    Key::Glyph(RTL)
                } else {
                    // The wide and combining glyphs are rare.
                    let i = detail as usize % (TEXT.len() * 4);
                    Key::Glyph(TEXT.get(i).copied().unwrap_or(TEXT[i % 11]))
                }
            }
            70..=78 => Key::Backspace,
            79..=86 => Key::Enter,
            87..=95 => match detail % 13 {
                1 => Key::Left,
                2 => Key::Right,
                // The learned keys; with the integration, now and then the
                // control chord in Home's or End's place (a detail no named
                // case without it reads otherwise).
                6 if self.cfg.integration && (detail / 13) % 2 == 1 => Key::Learned(EditKey::CtrlA),
                7 if self.cfg.integration && (detail / 13) % 2 == 1 => Key::Learned(EditKey::CtrlE),
                6 => Key::Learned(EditKey::Home),
                7 => Key::Learned(EditKey::End),
                8 => Key::Learned(EditKey::Delete),
                // Now and then push-line in Tab's place (a detail no named case
                // reads otherwise).
                0 if (detail / 13) % 4 == 3 => Key::Unmodelled(Unmodelled::PushLine),
                n => Key::Unmodelled(
                    [
                        Unmodelled::Tab,
                        Unmodelled::Tab,
                        Unmodelled::Tab,
                        Unmodelled::KillLine,
                        Unmodelled::KillWord,
                        Unmodelled::Interrupt,
                        Unmodelled::Tab,
                        Unmodelled::Tab,
                        Unmodelled::Tab,
                        Unmodelled::WordLeft,
                        Unmodelled::Up,
                        Unmodelled::Esc,
                        Unmodelled::KeypadLeft,
                    ][n as usize],
                ),
            },
            _ => {
                let len = 1 + detail as usize % 6;
                let mut text: Vec<char> = (0..len)
                    .map(|i| TEXT[(detail as usize / 7 + i * 3) % 11])
                    .collect();
                if detail.is_multiple_of(3) {
                    text.push('\r');
                }
                Key::Paste(text)
            }
        }
    }

    /// Quiescence: the program reads every key, every byte reaches the screen,
    /// and the clock passes the longest glitch window. Then nothing may be left
    /// on glass, and the engine must be the pure fold of the output.
    fn settle(&mut self) -> Result<(), Violation> {
        for _ in 0..64 {
            if self.program.inbox.is_empty() && self.program.pipe.is_empty() {
                break;
            }
            let n = u8::try_from(self.program.inbox.len().min(255)).unwrap_or(u8::MAX);
            self.program_step(n, 0);
            let total = self.program.pending_bytes();
            self.deliver(total)?;
            self.frame()?;
        }
        self.frame()?;
        self.now += SETTLE;
        let now = self.now;
        self.predictor.expire_due(now);
        self.sync(None, false)?;
        self.frame()?;
        if !self.predictor.idle() || self.predictor.next_deadline().is_some() {
            return Err(violation(
                "eventual",
                format!(
                    "{} guesses still pending {SETTLE:?} after the output settled",
                    self.predictor.pending().len()
                ),
            ));
        }
        let rows = usize::from(self.term.rows());
        let cols = usize::from(self.term.cols());
        let mut reference = RenderInput::empty();
        self.reference.cell_frame_into(&mut reference, rows, cols);
        let screen = |input: &RenderInput| -> Vec<String> {
            input
                .cells
                .iter()
                .map(|cells| cells.iter().map(|c| c.ch).collect())
                .collect()
        };
        if screen(&self.scratch) != screen(&reference)
            || self.term.cursor() != self.reference.cursor()
            || self.term.is_alternate_screen() != self.reference.is_alternate_screen()
        {
            return Err(violation(
                "eventual",
                format!(
                    "the settled screen {:?} is not the pure fold of the output {:?}",
                    screen(&self.scratch),
                    screen(&reference)
                ),
            ));
        }
        self.stats.settled_cases += 1;
        Ok(())
    }
}

/// Run one op list to quiescence. `Err` carries the op index (`ops.len()` for
/// the settle) and the broken clause.
pub(crate) fn run(cfg: Config, ops: &[Op]) -> Result<Stats, (usize, Violation)> {
    run_traced(cfg, ops, &mut None)
}

/// [`run`], keeping a step-by-step account in `trace` when it is `Some`.
pub(crate) fn run_traced(
    cfg: Config,
    ops: &[Op],
    trace: &mut Option<String>,
) -> Result<Stats, (usize, Violation)> {
    let mut sim = Sim::new(cfg);
    sim.trace = trace.take();
    let result = ops
        .iter()
        .enumerate()
        .try_for_each(|(i, &op)| sim.step(op).map_err(|v| (i, v)))
        .and_then(|()| sim.settle().map_err(|v| (ops.len(), v)));
    *trace = sim.trace.take();
    result.map(|()| sim.stats())
}

/// How fast the far end answers, which decides what Adaptive may show.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Link {
    Local,
    Slow,
    Erratic,
}

pub(crate) fn advance_ms(rng: &mut XorShift, link: Link) -> u32 {
    let r = rng.next_u64();
    let n = |lo: u64, hi: u64| u32::try_from(lo + (r >> 8) % (hi - lo)).unwrap_or(u32::MAX);
    match link {
        Link::Local => n(0, 3),
        Link::Slow => n(15, 90),
        Link::Erratic => match r % 10 {
            0..=4 => n(1, 10),
            5..=8 => n(20, 200),
            _ => n(250, 2600),
        },
    }
}

/// A run alternates CALM stretches (someone typing a command line at a shell
/// over the link: glyphs, the odd Backspace, the echo trailing behind) with
/// CHAOS stretches (every press class, unprompted output, host resets). Calm is
/// where Adaptive earns its proof and paints, so the clauses about displayed
/// guesses are exercised; chaos is where a guess on glass meets everything that
/// can move or overwrite its cell.
///
/// Calm stretches also fix a TYPO now and then ([`typo_fix`]): the cursor
/// walked back into the line, a pause, and a burst typed mid-line. That is
/// where a credited echo shifts the text right of it, which the tail guard must
/// keep from proving the epoch.
pub(crate) fn gen_ops(rng: &mut XorShift, len: usize, link: Link) -> Vec<Op> {
    let mut ops = Vec::with_capacity(len);
    while ops.len() < len {
        let calm = rng.below(10) < 7;
        let stretch = rng.range(20, 60);
        for _ in 0..stretch {
            let pick = rng.below(100);
            let a = rng.next_u64();
            let b = u32::try_from(a >> 32).unwrap_or_default();
            let c = u32::try_from(a & 0xffff_ffff).unwrap_or_default();
            if calm && pick == 99 {
                typo_fix(rng, link, &mut ops);
                continue;
            }
            if calm && pick == 98 {
                fresh_line(rng, link, &mut ops);
                continue;
            }
            if calm && pick == 97 {
                edit_motif(rng, link, &mut ops);
                continue;
            }
            if calm && pick == 96 {
                shift_motif(rng, link, &mut ops);
                continue;
            }
            if calm && pick == 95 {
                line_edge_motif(rng, link, &mut ops);
                continue;
            }
            let op = if calm {
                match pick {
                    0..=37 => {
                        // Mostly glyphs (`Sim::key_for` classes 0..70), now and then
                        // an arrow (87 with a detail of 1 or 2) or a Backspace
                        // (70..79), rarely an Enter (79..87).
                        match b % 100 {
                            0..=85 => Op::Key(b % 70, c),
                            86..=91 => Op::Key(87, 1 + c % 2),
                            92..=97 => Op::Key(70 + b % 9, c),
                            _ => Op::Key(79 + b % 8, c),
                        }
                    }
                    38..=55 => Op::Program(1, c),
                    56..=69 => Op::Deliver(1024),
                    // Now and then part of the output in flight: a frame inside
                    // one key's output (a glyph written, the tail it shifts not
                    // yet redrawn), where a typist meets it too.
                    70..=73 => Op::Deliver(u16::try_from(1 + b % 1023).unwrap_or(1)),
                    74..=95 => Op::Advance(advance_ms(rng, link)),
                    _ => Op::Frame,
                }
            } else {
                match pick {
                    0..=33 => Op::Key(b, c),
                    34..=50 => Op::Program(1 + (b % 3) as u8, c),
                    51..=67 => Op::Deliver(if b % 3 == 0 { 1024 } else { (b % 1024) as u16 }),
                    68..=81 => Op::Advance(advance_ms(rng, link)),
                    82..=87 => Op::Unprompted(*rng.pick(&UNPROMPTED), c),
                    88..=97 => Op::Frame,
                    _ => Op::HostReset(b % 2 == 0),
                }
            };
            ops.push(op);
        }
    }
    ops.truncate(len);
    ops
}

/// A fresh prompt, as a typist meets one: Enter, its command's output and the
/// new prompt (with its right prompt, under [`Config::rprompt`]) read and
/// delivered, a pause to read it, then a word typed at a typist's cadence, each
/// glyph read and echoed now and then, so some are typed ahead of their echo.
/// Where a line with a right prompt earns its proof and paints before the line
/// reaches the right prompt, and where a frame shows a right prompt the
/// settled screen has hidden. The commands are the ones that print a prompt on
/// the same screen: one clearing it (6) or printing up to two lines (0, 8, 16).
pub(crate) fn fresh_line(rng: &mut XorShift, link: Link, ops: &mut Vec<Op>) {
    let command = if rng.below(2) == 0 {
        6
    } else {
        u32::try_from(rng.below(3)).unwrap_or(0) * 8
    };
    // Enter (`Sim::key_for` class 79), with a frame after it.
    ops.extend([
        Op::Key(79, 1),
        Op::Program(1, command),
        Op::Advance(advance_ms(rng, link)),
        Op::Deliver(1024),
        Op::Advance(300 + u32::try_from(rng.below(400)).unwrap_or(0)),
    ]);
    let mut unread = 0u8;
    for _ in 0..3 + rng.below(4) {
        ops.push(glyph(TEXT[rng.index(9)]));
        ops.push(Op::Advance(40 + u32::try_from(rng.below(120)).unwrap_or(0)));
        unread += 1;
        if rng.below(4) == 0 {
            // The remote stalls past the glitch window with the glyph unread:
            // its guess expires (which widens the window), the next glyph is
            // typed, and then the remote reads both and their echoes land,
            // the stalled one late but inside the widened window.
            ops.extend([
                Op::Advance(200 + u32::try_from(rng.below(60)).unwrap_or(0)),
                glyph(TEXT[rng.index(9)]),
                Op::Program(unread + 1, 0),
                Op::Advance(advance_ms(rng, link)),
                Op::Deliver(1024),
            ]);
            unread = 0;
            continue;
        }
        if rng.below(3) != 0 {
            ops.extend([
                Op::Program(unread, 0),
                Op::Advance(advance_ms(rng, link)),
                Op::Deliver(1024),
            ]);
            unread = 0;
        }
    }
}

/// A typo fix, as a typist makes one: a word of two to four letters typed after
/// a space, each read and echoed; a pause to notice the typo; Lefts (unmodelled
/// here) back into the word, half the time all the way onto the space before
/// it, read and echoed; a pause past the glitch window; then one to three
/// glyphs typed mid-line in a burst, which the ops after it read and echo at
/// random. A burst typed at the space is where an insert's echo leaves a blank
/// under the next guess, so a credited echo that shifted the text right of it
/// would put the rest of the burst on glass over the moved tail.
pub(crate) fn typo_fix(rng: &mut XorShift, link: Link, ops: &mut Vec<Op>) {
    let notice = |rng: &mut XorShift| Op::Advance(300 + u32::try_from(rng.below(200)).unwrap_or(0));
    let letters = 2 + rng.below(3);
    for i in 0..=letters {
        let ch = if i == 0 { ' ' } else { TEXT[rng.index(9)] };
        ops.extend([
            glyph(ch),
            Op::Program(1, 0),
            Op::Advance(advance_ms(rng, link)),
            Op::Deliver(1024),
        ]);
    }
    ops.push(notice(rng));
    let arrows = if rng.below(2) == 0 {
        letters + 1
    } else {
        1 + rng.below(letters + 1)
    };
    let arrows = u8::try_from(arrows).unwrap_or(1);
    ops.extend((0..arrows).map(|_| LEFT));
    ops.push(Op::Program(arrows, 0));
    ops.push(Op::Advance(advance_ms(rng, link)));
    ops.push(Op::Deliver(1024));
    ops.push(notice(rng));
    for _ in 0..1 + rng.below(3) {
        let detail = u32::try_from(rng.next_u64() & 0xffff_ffff).unwrap_or(1);
        ops.push(Op::Key(u32::try_from(rng.below(70)).unwrap_or(0), detail));
    }
}

/// Line editing, as a typist edits: a word of three to six letters typed at the
/// end of the line, each read and echoed; a pause to notice; `←` one to four
/// times, then as many `→` back to the end, then one to three Backspaces at the
/// end, each key read and its output delivered now and then, in whole or in
/// part. The first `←`'s echo proves the motion class, so the arrows after it
/// show; a burst read together leaves the screen at a state the chain cannot
/// tell apart from its start (net-null); an echo delivered in pieces leaves it
/// part-way (the transit wait).
pub(crate) fn edit_motif(rng: &mut XorShift, link: Link, ops: &mut Vec<Op>) {
    for _ in 0..3 + rng.below(4) {
        ops.extend([
            glyph(TEXT[rng.index(9)]),
            Op::Program(1, 0),
            Op::Advance(advance_ms(rng, link)),
            Op::Deliver(1024),
        ]);
    }
    ops.push(Op::Advance(
        300 + u32::try_from(rng.below(200)).unwrap_or(0),
    ));
    let lefts = 1 + rng.index(4);
    let backspaces = 1 + rng.index(3);
    let keys = std::iter::repeat_n(LEFT, lefts)
        .chain(std::iter::repeat_n(RIGHT, lefts))
        .chain(std::iter::repeat_n(BACKSPACE, backspaces));
    // Now and then Enter follows the last key before it is read: the chain is
    // carried across its terminator, its pixels and cursor kept until its echo.
    let enter = rng.below(3) == 0;
    let last = 2 * lefts + backspaces - 1;
    let mut unread = 0u8;
    for (i, key) in keys.enumerate() {
        ops.push(key);
        unread += 1;
        if !(enter && i == last) && rng.below(3) != 0 {
            let part = if rng.below(4) == 0 {
                u16::try_from(1 + rng.below(1023)).unwrap_or(1)
            } else {
                1024
            };
            ops.extend([
                Op::Program(unread, 0),
                Op::Advance(advance_ms(rng, link)),
                Op::Deliver(part),
            ]);
            unread = 0;
        }
        ops.push(Op::Advance(40 + u32::try_from(rng.below(120)).unwrap_or(0)));
    }
    if enter {
        ops.push(Op::Key(79, 1));
        unread += 1;
    }
    ops.extend([
        Op::Program(unread, 0),
        Op::Advance(advance_ms(rng, link)),
        Op::Deliver(1024),
    ]);
}

/// Editing INSIDE the line, as a typist fixes a word: a word of three to six
/// letters typed at the end, each read and echoed; a pause to notice; `←` one
/// to three times, each read and echoed (the first proves the motion class);
/// then one to three glyphs, or Backspaces, typed there, each a SHIFT of the
/// text right of the cursor: the first usually read and echoed on its own,
/// which proves its class (or, in overwrite mode, does not), the rest now and
/// then in a burst, so a frame shows them with the tail moved before their
/// echo; then `→` back toward the end. Each key's output is delivered in whole
/// or in part, as `edit_motif`'s are.
pub(crate) fn shift_motif(rng: &mut XorShift, link: Link, ops: &mut Vec<Op>) {
    let letters = 3 + rng.index(4);
    for _ in 0..letters {
        ops.extend([
            glyph(TEXT[rng.index(9)]),
            Op::Program(1, 0),
            Op::Advance(advance_ms(rng, link)),
            Op::Deliver(1024),
        ]);
    }
    ops.push(Op::Advance(
        300 + u32::try_from(rng.below(200)).unwrap_or(0),
    ));
    let lefts = 1 + rng.index(3);
    for _ in 0..lefts {
        ops.extend([
            LEFT,
            Op::Program(1, 0),
            Op::Advance(advance_ms(rng, link)),
            Op::Deliver(1024),
        ]);
    }
    let erase = rng.below(3) == 0;
    let shifts = 1 + rng.index(3);
    let mut unread = 0u8;
    let keys = (0..shifts)
        .map(|_| {
            if erase {
                BACKSPACE
            } else {
                glyph(TEXT[rng.index(9)])
            }
        })
        .chain(std::iter::repeat_n(RIGHT, lefts))
        .collect::<Vec<Op>>();
    for (i, key) in keys.into_iter().enumerate() {
        ops.push(key);
        unread += 1;
        if (i == 0 && rng.below(4) != 0) || rng.below(3) == 0 {
            let part = if rng.below(4) == 0 {
                u16::try_from(1 + rng.below(1023)).unwrap_or(1)
            } else {
                1024
            };
            ops.extend([
                Op::Program(unread, 0),
                Op::Advance(advance_ms(rng, link)),
                Op::Deliver(part),
            ]);
            unread = 0;
        }
        ops.push(Op::Advance(40 + u32::try_from(rng.below(120)).unwrap_or(0)));
    }
    ops.extend([
        Op::Program(unread, 0),
        Op::Advance(advance_ms(rng, link)),
        Op::Deliver(1024),
    ]);
}

/// The ops that press each learned key and draw a frame (a detail
/// `Sim::key_for` maps to it, with `detail % 4 != 0`): Home, End, Ctrl-A and
/// Ctrl-E (Home and End without the integration) and Delete.
pub(crate) const HOME: Op = Op::Key(87, 6);
pub(crate) const END: Op = Op::Key(87, 7);
pub(crate) const CTRL_A: Op = Op::Key(87, 19);
pub(crate) const CTRL_E: Op = Op::Key(87, 46);
pub(crate) const DELETE: Op = Op::Key(87, 21);

/// The op that presses ^C (Interrupt) and draws a frame.
pub(crate) const INTERRUPT: Op = Op::Key(87, 5);

/// The op that presses zsh's push-line (`ESC q`) and draws a frame.
pub(crate) const PUSH_LINE: Op = Op::Key(87, 39);

/// The learned keys at a fresh prompt, as a typist uses them: Enter and a
/// command whose output ends in a prompt (signed under the integration), or
/// the one that clears the screen and reads a line after a prompt of its own;
/// a pause; a word typed, each glyph read and echoed; a pause to notice; the
/// line's two edges, Home and End or Ctrl-A and Ctrl-E, each pressed twice in
/// turn and read on its own, so their bindings are learned from their echoes
/// where the far end binds them so, and half the time Delete twice at the
/// start; then three to eight keys from those, Delete, `←` and a glyph (a
/// prefix after the start, a suffix after the end), each read and its output
/// delivered now and then, so a learned key is shown at once; and now and
/// then, between them, output nobody typed for that changes what the keys
/// reach: the shell re-executing with the next keymap under a new nonce, a
/// forged mark, Kitty's `DISAMBIGUATE` or modifyOtherKeys level 2 pushed or
/// popped, the prompt redrawn in another width with no new `B`. Now and then
/// the line before the Enter is pushed onto zsh's buffer stack, and the prompt
/// comes pre-filled with it.
pub(crate) fn line_edge_motif(rng: &mut XorShift, link: Link, ops: &mut Vec<Op>) {
    let command = [0, 6, 8, 19][rng.index(4)];
    // Whatever was in flight read and delivered, and ^C, which brings every
    // far end here back to its shell's prompt: so the Enter below is the key
    // the shell reads next.
    ops.extend([
        Op::Program(u8::MAX, 0),
        Op::Advance(advance_ms(rng, link)),
        Op::Deliver(1024),
        INTERRUPT,
        Op::Program(1, 0),
        Op::Advance(advance_ms(rng, link)),
        Op::Deliver(1024),
    ]);
    if rng.below(4) == 0 {
        for key in [
            glyph(TEXT[rng.index(9)]),
            glyph(TEXT[rng.index(9)]),
            PUSH_LINE,
        ] {
            ops.extend([
                key,
                Op::Program(1, 0),
                Op::Advance(advance_ms(rng, link)),
                Op::Deliver(1024),
            ]);
        }
    }
    ops.extend([
        Op::Key(79, 1),
        Op::Program(1, command),
        Op::Advance(advance_ms(rng, link)),
        Op::Deliver(1024),
        Op::Advance(300 + u32::try_from(rng.below(400)).unwrap_or(0)),
    ]);
    for _ in 0..3 + rng.below(4) {
        ops.extend([
            glyph(TEXT[rng.index(9)]),
            Op::Program(1, 0),
            Op::Advance(advance_ms(rng, link)),
            Op::Deliver(1024),
        ]);
    }
    ops.push(Op::Advance(
        300 + u32::try_from(rng.below(200)).unwrap_or(0),
    ));
    let (start, end) = if rng.below(2) == 0 {
        (HOME, END)
    } else {
        (CTRL_A, CTRL_E)
    };
    let read = |key: Op, rng: &mut XorShift, ops: &mut Vec<Op>| {
        ops.extend([
            key,
            Op::Program(1, 0),
            Op::Advance(advance_ms(rng, link)),
            Op::Deliver(1024),
            Op::Advance(40 + u32::try_from(rng.below(120)).unwrap_or(0)),
        ]);
    };
    for _ in 0..2 {
        for key in [start, end] {
            read(key, rng, ops);
        }
    }
    // Delete taught too, half the time: the line's start, then Delete twice
    // there, each read on its own, so delete-char is learned from two echoes
    // that shift the text left, where the far end binds it so; then the end,
    // `←`, and Delete once more, shown at once where it was learned.
    if rng.below(2) == 0 {
        for key in [start, DELETE, DELETE, end, LEFT, DELETE] {
            read(key, rng, ops);
        }
    }
    let mut unread = 0u8;
    for _ in 0..3 + rng.below(6) {
        let key = match rng.below(10) {
            0..=2 => start,
            3 | 4 => end,
            5 => [HOME, END, CTRL_A, CTRL_E][rng.index(4)],
            // Delete, half the time after a `←` (one at the end of the text
            // changes nothing and is not modelled).
            6 | 7 if rng.below(2) == 0 => {
                ops.push(LEFT);
                unread += 1;
                DELETE
            }
            6 | 7 => DELETE,
            8 => LEFT,
            // Now and then a combining mark, which joins the cell before it:
            // typed while a chain is in flight, its echo makes a cell of the
            // chain's line opaque (R-OPAQUE).
            _ if rng.below(4) == 0 => glyph('\u{301}'),
            _ => glyph(TEXT[rng.index(9)]),
        };
        ops.push(key);
        unread += 1;
        if rng.below(10) == 0 {
            let what = [
                Unprompted::Rebind,
                Unprompted::ForgedMark,
                Unprompted::KittyChords,
                Unprompted::ResetPrompt,
            ][rng.index(4)];
            ops.push(Op::Unprompted(
                what,
                u32::try_from(rng.below(2)).unwrap_or(0),
            ));
        }
        if rng.below(3) != 0 {
            let part = if rng.below(4) == 0 {
                u16::try_from(1 + rng.below(1023)).unwrap_or(1)
            } else {
                1024
            };
            ops.extend([
                Op::Program(unread, 0),
                Op::Advance(advance_ms(rng, link)),
                Op::Deliver(part),
            ]);
            unread = 0;
        }
        ops.push(Op::Advance(40 + u32::try_from(rng.below(120)).unwrap_or(0)));
    }
    ops.extend([
        Op::Program(unread, 0),
        Op::Advance(advance_ms(rng, link)),
        Op::Deliver(1024),
    ]);
    // Now and then a prefix, the end and a combining mark in one burst: the
    // mark, typed at the end of the text the chain leaves, is declined with
    // the chain's pixels kept, and its echo joins the line's last cell while
    // the chain is in flight, a cell no state can name (R-OPAQUE). (The start
    // and the end alone would leave the line as it was: a net-null burst.)
    if rng.below(3) == 0 {
        ops.extend([
            start,
            glyph(TEXT[rng.index(9)]),
            end,
            glyph('\u{301}'),
            Op::Program(4, 0),
            Op::Advance(advance_ms(rng, link)),
            Op::Deliver(1024),
        ]);
    }
}

/// Whether `ops` still breaks the same clause.
pub(crate) fn still_fails(cfg: Config, ops: &[Op], clause: &str) -> bool {
    matches!(run(cfg, ops), Err((_, v)) if v.clause == clause)
}

/// Delta debugging over the op list: the smallest list (by chunk removal) that
/// still breaks the same clause.
pub(crate) fn shrink(cfg: Config, ops: &[Op], clause: &str) -> Vec<Op> {
    let mut ops = ops.to_vec();
    let mut chunks = 2usize;
    while ops.len() >= 2 {
        let size = ops.len().div_ceil(chunks);
        let mut reduced = false;
        for start in (0..ops.len()).step_by(size) {
            let end = (start + size).min(ops.len());
            let candidate: Vec<Op> = ops
                .get(..start)
                .unwrap_or_default()
                .iter()
                .chain(ops.get(end..).unwrap_or_default())
                .copied()
                .collect();
            if still_fails(cfg, &candidate, clause) {
                ops = candidate;
                chunks = (chunks - 1).max(2);
                reduced = true;
                break;
            }
        }
        if !reduced {
            if chunks >= ops.len() {
                break;
            }
            chunks = (chunks * 2).min(ops.len());
        }
    }
    ops
}

/// Every configuration aterm ships: both display modes, a local and a remote far
/// end, and both ambiguous-width policies.
pub(crate) fn faithful_configs() -> Vec<Config> {
    let mut out = Vec::new();
    for mode in [PredictMode::Adaptive, PredictMode::Always] {
        for remote in [false, true] {
            for wide in [false, true] {
                out.push(Config {
                    mode,
                    remote,
                    host: Host::Faithful,
                    forced_wrap: false,
                    wide,
                    rprompt: false,
                    editor: Editor::ReadlineIch,
                    bidi: false,
                    integration: false,
                    keymap: Keymap::Canonical,
                });
            }
        }
    }
    out
}

pub(crate) const LINKS: [Link; 4] = [Link::Local, Link::Slow, Link::Erratic, Link::Slow];
pub(crate) const CASES: usize = 600;
pub(crate) const OPS_PER_CASE: usize = 320;
pub(crate) const SEED: u64 = 0x5AFE_EC40_7EED_2026;

/// The seed of SEED INDEX `i` of the many-seed sweeps: the committed seed xor
/// `i` times an odd constant that is not the golden ratio `case_world` steps
/// cases by (the reviews of increments 3 to 5 swept that family, indices 0 to
/// 4200). Index 0 is the committed seed.
pub(crate) fn seed_index(i: u64) -> u64 {
    SEED ^ i.wrapping_mul(0xA24B_AED4_963E_E407)
}

/// The theorem's sweep of every faithful configuration over each of the seed
/// indices `indices` (`seed_index`), on every core: each failure's report
/// (`sweep`), prefixed with its seed index, in order.
pub(crate) fn sweep_seed_indices(indices: impl IntoIterator<Item = u64>) -> Vec<String> {
    let items: Vec<(u64, Config)> = indices
        .into_iter()
        .flat_map(|i| faithful_configs().into_iter().map(move |cfg| (i, cfg)))
        .collect();
    aterm_spec::check::each(&items, |&(i, cfg)| {
        sweep(cfg, seed_index(i))
            .err()
            .map(|report| format!("seed index {i}: {report}"))
    })
    .into_iter()
    .flatten()
    .collect()
}

/// Case `case` of a sweep: its configuration (the line editor's wrap policy
/// alternates), seed, link and ops.
pub(crate) fn case_world(cfg: Config, seed: u64, case: usize) -> (Config, u64, Link, Vec<Op>) {
    let case_seed = seed ^ (case as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut rng = XorShift::new(case_seed);
    let link = LINKS[case % LINKS.len()];
    // The line editor's wrap policy, its right prompt and the form of its
    // mid-line insert rotate per case, so every configuration meets each
    // combination at no extra cost.
    let cfg = Config {
        forced_wrap: (case / LINKS.len()) % 2 == 1,
        rprompt: (case / (LINKS.len() * 2)) % 2 == 1,
        editor: EDITORS[(case / (LINKS.len() * 4)) % EDITORS.len()],
        bidi: (case / (LINKS.len() * 4 * EDITORS.len())) % 3 == 2,
        integration: !case.is_multiple_of(3),
        keymap: KEYMAPS[(case / 3) % KEYMAPS.len()],
        ..cfg
    };
    (cfg, case_seed, link, gen_ops(&mut rng, OPS_PER_CASE, link))
}

/// Run every case of `cfg`; the first violation is shrunk and returned as a report.
pub(crate) fn sweep(cfg: Config, seed: u64) -> Result<Stats, String> {
    let mut total = Stats::default();
    for case in 0..CASES {
        let (cfg, case_seed, link, ops) = case_world(cfg, seed, case);
        match run(cfg, &ops) {
            Ok(stats) => total.add(stats),
            Err((at, v)) => {
                let minimal = shrink(cfg, &ops, v.clause);
                let mut trace = Some(String::new());
                let replay = run_traced(cfg, &minimal, &mut trace).err();
                let mut report = format!(
                    "{cfg:?}: case {case} (seed {case_seed:#x}, {link:?} link) broke `{}` at op {at}: {}\n\
                     shrunk to {} ops",
                    v.clause,
                    v.detail,
                    minimal.len()
                );
                if let Some((at, v)) = replay {
                    let _ = write!(report, ", failing at op {at}: {}", v.detail);
                }
                let _ = write!(report, "\nminimal ops: {minimal:?}");
                let _ = write!(report, "\ntrace:\n{}", trace.unwrap_or_default());
                return Err(report);
            }
        }
    }
    Ok(total)
}

/// The op that presses `c` and draws a frame: a glyph class (0) and a detail
/// `Sim::key_for` maps to `c` outside a secret context, with `detail % 4 != 0`.
pub(crate) fn glyph(c: char) -> Op {
    let detail = (0u32..56)
        .find(|&d| {
            let i = d as usize;
            TEXT.get(i).copied().unwrap_or(TEXT[i % 11]) == c && d % 4 != 0
        })
        .expect("a glyph `Sim::key_for` can press");
    Op::Key(0, detail)
}

/// The op that presses [`RTL`] and draws a frame under [`Config::bidi`]: a
/// detail whose high byte `Sim::key_for` reads as the right-to-left letter
/// (and `% 4 != 0`).
pub(crate) const RTL_GLYPH: Op = Op::Key(0, (5 << 24) | 1);

/// The op that presses `←` and draws a frame.
pub(crate) const LEFT: Op = Op::Key(87, 1);

/// The op that presses `→` and draws a frame.
pub(crate) const RIGHT: Op = Op::Key(87, 2);

/// The op that presses Backspace and draws a frame.
pub(crate) const BACKSPACE: Op = Op::Key(70, 1);

/// The op that presses Alt-B (a word motion, input the predictor does not
/// model) and draws a frame.
pub(crate) const WORD_LEFT: Op = Op::Key(87, 9);
