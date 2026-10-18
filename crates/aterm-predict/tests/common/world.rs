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
use aterm_core::render::RenderInput;
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::{EditKey, EditOutcome, KeySample, PredictMode, Predictor, Rule, Seen};
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
    /// Home or Ctrl-A: to the start of the buffer.
    Home,
    /// End or Ctrl-E: to the end of the buffer.
    End,
    /// The Delete key (`ESC [ 3 ~`): the glyph under the cursor erased.
    DeleteKey,
    /// Alt-B: back to the start of the word (a word motion is not an arrow).
    WordLeft,
    /// Up: the previous command from history, replacing the line.
    Up,
    /// Escape alone: a prefix the editor waits on, nothing drawn.
    Esc,
    /// The keypad's Left under DECKPAM (`ESC O t`): no motion; readline and
    /// zle leave it unbound.
    KeypadLeft,
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
}

pub(crate) const UNPROMPTED: [Unprompted; 18] = [
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
    /// Hands the keypad's Left, which DECKPAM turns into `ESC O t`, to the
    /// predictor as `←` (FRAME FIDELITY: the far end does not move).
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
            Self::PaintsBesideTheGuess | Self::PaintsOverWideGlyphs => Some("locality"),
            Self::ForcesAlways => Some("secrecy"),
            Self::IgnoresLineSubmits | Self::IgnoresAmbiguousWidth => Some("fidelity"),
            Self::TailBlind
            | Self::WordMotionAsArrow
            | Self::KeypadArrowAsArrow
            | Self::CursorAfterNewestGlyph => Some("frame-fidelity"),
            Self::KeepsStaleGhosts | Self::StaleClock => Some("timeliness"),
            Self::PaintsPartialState => Some("atomicity"),
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

pub(crate) const MUTANT_HOSTS: [Host; 19] = [
    Host::FrozenCoordinates,
    Host::BlindToAltScreen,
    Host::PaintsBesideTheGuess,
    Host::PaintsOverWideGlyphs,
    Host::ForcesAlways,
    Host::IgnoresLineSubmits,
    Host::IgnoresAmbiguousWidth,
    Host::TailBlind,
    Host::KeepsStaleGhosts,
    Host::StaleClock,
    Host::Rule(Rule::ExpiryOwes),
    Host::Rule(Rule::TailGuard),
    Host::CursorAfterNewestGlyph,
    Host::Rule(Rule::SmallestMatch),
    Host::Rule(Rule::NetNullUncredited),
    Host::Rule(Rule::WholeStateMatch),
    Host::Rule(Rule::MotionClassProof),
    Host::Rule(Rule::StateExpiryFlushes),
    Host::Rule(Rule::CarryCursor),
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
///   erase leaves or clears and the decoration rule counts; an unseen buffer tail
///   it would pull left needs a line known only up to the middle of its buffer,
///   which no line here is (the known line grows only from proved appends).
/// * `EditOpsOwe` off owes nothing for a declined arrow or a flushed motion. An
///   arrow moves only the cursor, and a guess typed after one with nothing
///   placed is a glyph-lane guess at the live cursor whose proof the tail guard
///   and the cursor advance already refuse; the sweep finds no counterexample.
pub(crate) fn rule_clause(rule: Rule) -> Option<&'static str> {
    match rule {
        Rule::ExpiryOwes | Rule::CancelTransit | Rule::UncreditedVoids | Rule::StaleAnchor => {
            Some("fidelity")
        }
        Rule::TypedOwesSameGlyph
        | Rule::CursorAdvance
        | Rule::CancelOwes
        | Rule::SmallestMatch
        | Rule::NetNullUncredited => Some("attribution"),
        Rule::TailGuard
        | Rule::StandingTail
        | Rule::MotionClassProof
        | Rule::ProofsEndWithTheEpoch
        | Rule::TextEdges
        | Rule::CarryCursor => Some("frame-fidelity"),
        Rule::WholeStateMatch => Some("provenance"),
        Rule::StateExpiryFlushes => Some("timeliness"),
        Rule::TypedOwes
        | Rule::TypedOwesCleanTail
        | Rule::VacatedBlankInMatch
        | Rule::MotionsNeverProve
        | Rule::VacateRefusal
        | Rule::EditOpsOwe => None,
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
}

/// The editors a sweep rotates through.
pub(crate) const EDITORS: [Editor; 4] = [
    Editor::ReadlineIch,
    Editor::Readline,
    Editor::Zle,
    Editor::Cooked,
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
    /// The cursor the GUI draws one past the newest painted ghost; `None` at the
    /// right margin, where it clamps the column and the drawn cursor is a guess
    /// about the far end's wrap policy, not a claim.
    cursor: Option<(u16, u16)>,
    space: Space,
    unprompted: u64,
    /// The oldest pending guess's key at this frame.
    oldest: KeyId,
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

    /// Text printed at the start of a fresh row, then the prompt: the column is
    /// exact again.
    fn prompt_after(&mut self, text: &str) {
        self.reply(format!("{text}{PROMPT}"));
        self.col = PROMPT.chars().count();
        self.reliable = true;
        self.rprompt = None;
        // The right prompt, drawn in italics behind a cursor save and restore,
        // where it leaves at least one column between it and the prompt.
        let lo = self.cols.saturating_sub(1 + RPROMPT.chars().count());
        if self.draws_rprompt && lo > self.col + 1 {
            self.reply(format!(
                "\x1b7\x1b[{}G\x1b[3m{RPROMPT}\x1b[23m\x1b8",
                lo + 1
            ));
            self.rprompt = Some(lo);
        }
    }

    /// Erase the right prompt before an insert that would bring the line up to
    /// it (no column left between them) or that shifts the row (a mid-line
    /// insert): zsh hides its RPROMPT rather than let the buffer run into it.
    fn hide_rprompt_for(&mut self, c: char) {
        let Some(lo) = self.rprompt else {
            return;
        };
        let wide = self.wide;
        let end = PROMPT.chars().count()
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

    /// Cursor back over the tail after a redraw, as a fresh string.
    fn line_redraw(&self) -> String {
        let text: String = self.line.iter().collect();
        let tail: usize = self
            .line
            .get(self.cur..)
            .unwrap_or_default()
            .iter()
            .map(|&c| width(c, self.wide))
            .sum();
        if tail == 0 {
            format!("{PROMPT}{text}")
        } else {
            format!("{PROMPT}{text}\x1b[{tail}D")
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
        if self.cur < self.line.len() && width(c, self.wide) > 0 {
            if self.col >= self.cols {
                self.reliable = false; // an insert from the parked slot is beyond this model
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
            if self.col + width(c, wide) + tail > self.cols {
                self.reliable = false;
            }
            if matches!(self.editor, Editor::Readline | Editor::Zle) {
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
                return;
            }
            self.reply(format!("\x1b[{}@", width(c, self.wide)));
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

    /// Clear the line being edited.
    fn reset_line(&mut self) {
        self.line.clear();
        self.cooked_cells.clear();
        self.cur = 0;
    }

    /// The line discipline is cooked: the tty echoes and edits the line.
    fn cooked(&self) -> bool {
        self.editor == Editor::Cooked || self.cat
    }

    /// A key at an edge of the line (`←` at its start, `→` or Delete at its
    /// end, an unbound sequence): readline beeps, zle stays silent.
    fn edge(&mut self) {
        if matches!(self.editor, Editor::Readline | Editor::ReadlineIch) {
            self.reply("\x07");
        }
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
        if self.editor == Editor::Zle {
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
            self.edge();
            return;
        };
        self.cur += 1;
        let w = width(c, self.wide);
        let fits = self.col + w < self.cols;
        match self.editor {
            Editor::Readline if fits => {
                self.reply("\x1b[C".repeat(w));
                self.col += w;
            }
            Editor::Zle if fits => {
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
    }

    /// Up: the previous command from history replaces the line, redrawn from the
    /// prompt. A line that wrapped leaves the cursor below the prompt's row,
    /// where the carriage return does not reach it: beyond this model.
    fn recall(&mut self) {
        const HISTORY: &str = "ls -la";
        let wide = self.wide;
        let before: usize = self
            .line
            .get(..self.cur)
            .unwrap_or_default()
            .iter()
            .map(|&c| width(c, wide))
            .sum();
        if PROMPT.chars().count() + before >= self.cols {
            self.reliable = false;
        }
        self.reset_line();
        self.line.extend(HISTORY.chars());
        self.cur = self.line.len();
        self.reply(format!("\r\x1b[K{PROMPT}{HISTORY}"));
        self.col = PROMPT.chars().count() + HISTORY.chars().count();
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
        self.reset_line();
        // The submitted line keeps its right prompt; the editor is done with it.
        self.rprompt = None;
        match choice % 8 {
            4 => {
                self.reply("\r\n");
                self.noecho = true;
                self.noecho_serial += 1;
                let serial = self.noecho_serial;
                self.emit(SegKind::NoEchoPrompt(serial), "Password: ");
                self.col = "Password: ".len();
                self.reliable = true;
            }
            5 => {
                self.reply("\r\n");
                self.col = 0;
                self.alt = true;
                self.alt_from_command = true;
                self.reply("\x1b[?1049h\x1b[H\x1b[2J~\r\n~ app");
            }
            6 => {
                self.reply("\r\n");
                self.prompt_after("\x1b[H\x1b[2J");
            }
            7 => {
                let mut out = String::from("\r\n");
                for i in 0..7 {
                    let _ = write!(out, "line {i}\r\n");
                }
                self.prompt_after(&out);
            }
            // `cat`: a cooked program reads the tty until ^C, on the row after
            // the line, so a line the shell proved meets a cooked far end.
            3 if (choice / 8) % 2 == 1 => {
                self.reply("\r\n");
                self.col = 0;
                self.cat = true;
                self.reliable = true;
            }
            _ => {
                let mut out = String::from("\r\n");
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
        self.reply(format!("\x1b[?1049l{PROMPT}"));
        if self.alt_from_command {
            // The main screen comes back at the fresh row the command left.
            self.col = PROMPT.chars().count();
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
                Key::Unmodelled(_) => self.reply("\x1b[2;1H~"),
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
                    self.reset_line();
                    self.prompt_after("\r\nok\r\n");
                }
                Key::Unmodelled(Unmodelled::Interrupt) => {
                    self.noecho = false;
                    self.cat = false;
                    self.reset_line();
                    self.prompt_after("^C\r\n");
                }
                Key::Left | Key::Right | Key::Unmodelled(_) => {}
            }
        } else if self.cooked() {
            self.consume_cooked(id, key, choice);
        } else {
            match key {
                Key::Glyph(c) => self.insert(*c, Some(id), choice),
                Key::Backspace => self.erase_back(1),
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
                Key::Unmodelled(Unmodelled::Tab) => {
                    for c in "ion ".chars() {
                        self.insert(c, None, 0);
                    }
                }
                Key::Unmodelled(Unmodelled::KillLine) => self.erase_back(self.cur),
                Key::Unmodelled(Unmodelled::KillWord) => {
                    let n = self.word_before();
                    self.erase_back(n);
                }
                Key::Unmodelled(Unmodelled::Interrupt) => {
                    self.reset_line();
                    self.prompt_after("^C\r\n");
                }
                Key::Unmodelled(Unmodelled::Home) => {
                    let before: Vec<char> = self.line.get(..self.cur).unwrap_or_default().to_vec();
                    self.cur = 0;
                    let back = self.back_over(&before);
                    self.reply(back);
                }
                Key::Unmodelled(Unmodelled::End) => {
                    while let Some(&c) = self.line.get(self.cur) {
                        self.cur += 1;
                        self.put(SegKind::Reply, c);
                    }
                }
                Key::Unmodelled(Unmodelled::DeleteKey) => self.delete_forward(),
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
                self.reset_line();
                self.prompt_after("^C\r\n");
            }
            Key::Unmodelled(Unmodelled::Home) => self.tty_echo(&seq("\x1b[H"), SegKind::Reply),
            Key::Unmodelled(Unmodelled::End) => self.tty_echo(&seq("\x1b[F"), SegKind::Reply),
            Key::Unmodelled(Unmodelled::DeleteKey) => {
                self.tty_echo(&seq("\x1b[3~"), SegKind::Reply);
            }
            Key::Unmodelled(Unmodelled::WordLeft) => {
                self.tty_echo(&seq("\x1bb"), SegKind::Reply);
            }
            Key::Unmodelled(Unmodelled::Up) => self.tty_echo(&seq("\x1b[A"), SegKind::Reply),
            Key::Unmodelled(Unmodelled::Esc) => self.tty_echo(&seq("\x1b"), SegKind::Reply),
            Key::Unmodelled(Unmodelled::KeypadLeft) => {
                self.tty_echo(&seq("\x1bOt"), SegKind::Reply);
            }
        }
    }

    /// Output nobody typed for.
    fn unprompted_output(&mut self, what: Unprompted, param: u32) {
        // Whatever it draws, the editor no longer knows where its right prompt
        // is, so it stops managing it (it stays on screen until overwritten).
        self.rprompt = None;
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
                let n = 1 + param % 9;
                self.unprompted(format!("\x1b7\x1b[{n}C<rp>\x1b8"));
            }
            Unprompted::Suggest => {
                if self.cur == self.line.len() && !self.noecho && !self.alt {
                    let s = ["ion", "sne", "ab", "dice", "e a"][(param % 5) as usize];
                    let n = s.chars().count();
                    self.unprompted(format!("\x1b[2m{s}\x1b[0m\x1b[{n}D"));
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
                    self.unprompted(format!("\r\n{PROMPT}"));
                    self.col = PROMPT.chars().count();
                    self.reliable = true;
                }
            }
            Unprompted::Ris => {
                self.alt = false;
                self.cat = false;
                self.reset_line();
                self.unprompted(format!("\x1bc{PROMPT}"));
                self.col = PROMPT.chars().count();
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
        .map(|r| {
            (0..cols)
                .map(|c| {
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
                })
                .collect()
        })
        .collect()
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

/// How `host` paints a state chain's frame into `d` (`(cols, blank)` the grid
/// width and the terminal blank): `shown`, the chain's cells, and
/// `chain_cursor`, the cursor the predictor gives it. aterm-gui's rule: ALL OR
/// NONE, asking the painter's own refusal first
/// (`RenderInput::prediction_ghost_paintable`), and the chain's cursor drawn
/// only when every cell was painted and the cursor's own cell is no wide
/// glyph's right half; with no cursor from the predictor, the glyph lane's
/// cursor, one past the newest cell painted. Returns which cells were painted
/// and the cursor drawn (`None`: the engine's).
pub(crate) fn paint_chain(
    d: &mut RenderInput,
    shown: &[(u16, u16, char)],
    chain_cursor: Option<(u16, u16)>,
    (cols, blank): (u16, RenderCell),
    host: Host,
) -> (Vec<bool>, Option<(u16, u16)>) {
    let width = usize::from(cols);
    let paintable = shown
        .iter()
        .all(|&(r, c, _)| d.prediction_ghost_paintable(usize::from(r), usize::from(c), width));
    let mut painted = vec![false; shown.len()];
    if paintable || host == Host::PaintsPartialState {
        for (&(r, c, ch), painted) in shown.iter().zip(&mut painted) {
            *painted = d.paint_prediction_ghost(usize::from(r), usize::from(c), ch, width, blank);
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

/// A fresh engine of the world's size, under its ambiguous-width policy: the
/// wide one is the host's `ambiguous_width = "wide"`, applied the way the host
/// applies its config (`Terminal::apply_config`); RIS keeps it, as a host
/// setting.
pub(crate) fn engine(cfg: Config) -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);
    if cfg.wide {
        let config = aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..aterm_core::config::TerminalConfig::default()
        };
        term.apply_config(&config);
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
    stats: Stats,
    /// The newest Enter pressed: a state chain displayed with an op typed before
    /// it is carried across its terminator.
    last_enter: Option<KeyId>,
    /// The PROVENANCE oracle's base for the state chain in flight (see
    /// [`OracleBase`]).
    oracle: Option<OracleBase>,
    /// A step-by-step account, kept only when replaying a shrunk failure.
    trace: Option<String>,
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
        let term = engine(cfg);
        let generation = term.content_scroll_state().coordinate_generation();
        let mut predictor = Predictor::new(Self::host_mode_of(cfg));
        if let Host::Rule(rule) = cfg.host {
            predictor.disable_rule_for_test(rule);
        }
        let start = Instant::now();
        Self {
            cfg,
            term,
            reference: engine(cfg),
            scratch: RenderInput::empty(),
            predictor,
            program: Program::new(COLS, cfg),
            glass: Vec::new(),
            press_scratch: RenderInput::empty(),
            press_row: Vec::new(),
            now: start,
            last_press: start,
            keys: Vec::new(),
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
        }
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

    /// The edit key the host hands `Predictor::predict_edit` for `key`: a bare
    /// main-block arrow or Backspace, as aterm-gui classifies the unfolded event.
    /// A mutant also hands it a word motion, or a keypad arrow, as `←`.
    fn edit_key_of(&self, key: &Key) -> Option<EditKey> {
        match (key, self.cfg.host) {
            (Key::Backspace, _) => Some(EditKey::Backspace),
            (Key::Left, _) => Some(EditKey::Left),
            (Key::Right, _) => Some(EditKey::Right),
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
        // always reads it).
        let wants_row =
            (matches!(key, Key::Glyph(_)) && self.predictor.wants_key_row()) || edit.is_some();
        // The key-time sample, read under one terminal hold.
        let cursor = self.term.cursor();
        let coords = self.host_generation();
        let grid = (self.term.cols(), self.term.rows());
        let no_echo = self.host_alt();
        let has_row = wants_row && self.sample_press_row(cursor.row, cursor.col);
        // The local termios: canonical with ECHO clear. Invisible behind a link.
        let tty_swallows = !self.cfg.remote && self.program.noecho && !self.program.alt;
        let now = self.now;
        let mode = self.host_mode();
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
                };
                let outcome = self.predictor.predict_edit(k, &sample, now);
                match outcome {
                    EditOutcome::Armed => {
                        self.stats.edit_keys_armed += 1;
                        if self.program.cooked() && k != EditKey::Backspace {
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
                    self.predictor.note_line_submit(now);
                }
                self.sync(None, false)?;
                true
            }
            Key::Backspace | Key::Left | Key::Right | Key::Unmodelled(_) | Key::Paste(_) => {
                self.predictor.note_unmodelled_input(now);
                self.sync(None, false)?;
                true
            }
        };
        if let Some(log) = self.keys.get_mut(id) {
            log.ends_epoch = ends_epoch;
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
        true
    }

    /// The program reads up to `n` keys.
    fn program_step(&mut self, n: u8, choice: u32) {
        for i in 0..n {
            let Some(id) = self.program.inbox.pop_front() else {
                break;
            };
            let reliable = self.program.reliable;
            let Some(key) = self.keys.get(id).map(|log| log.key.clone()) else {
                continue;
            };
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
                if let Some(log) = seg.cause.and_then(|k| self.keys.get_mut(k)) {
                    log.last_output_at = Some(now);
                    log.last_output_window = window;
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
        let boundary = before
            .iter()
            .rev()
            .find(|log| log.ends_epoch)
            .map(|log| log.typed_at);
        before.iter().enumerate().any(|(j, log)| {
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
    /// because no ghost is involved and the decoration is the program's to move.
    /// A ghost GLYPH where the settled screen differs is a violation, decoration
    /// or not.
    fn settled(&mut self, k: KeyId) -> Result<(), Violation> {
        if self.cfg.mode != PredictMode::Adaptive {
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
            let mut wrong = Vec::new();
            for (r, (shown, settled)) in f.text.iter().zip(&truth).enumerate() {
                for (c, (a, b)) in shown.iter().zip(settled).enumerate() {
                    if a == b {
                        continue;
                    }
                    let ghost = f.ghosts.contains(&(r, c));
                    let decoration = f.decoration.get(r).and_then(|row| row.get(c)) == Some(&true)
                        || truth_decoration.get(r).and_then(|row| row.get(c)) == Some(&true);
                    if !ghost && decoration {
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
    fn attributed(&mut self, credited: &[u64]) -> Result<(), Violation> {
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
                || self.overdue_predecessor(k)
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
    fn overdue_predecessor(&self, k: KeyId) -> bool {
        let window = ECHO_DEADLINE.max(self.predictor.glitch_window_for_test());
        let unarrived = |j: KeyId| {
            self.program.inbox.contains(&j)
                || self.program.pipe.iter().any(|seg| seg.cause == Some(j))
        };
        self.keys
            .get(..k)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .any(|(j, log)| {
                unarrived(j) && self.now.saturating_duration_since(log.typed_at) > window
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
        if self.cfg.mode != PredictMode::Adaptive || space != self.space() {
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
        {
            let input = &self.scratch;
            let blind = self.cfg.host == Host::TailBlind;
            self.predictor.reconcile_seen(
                Some((cursor.row, cursor.col)),
                coords,
                alt,
                now,
                |r, c| {
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
        self.attributed(&credited)?;
        self.sync(None, false)?;
        self.read_oracle_base();
        let shown: Vec<(u16, u16, char)> = self
            .predictor
            .overlay(now)
            .iter()
            .map(|p| (p.row, p.col, p.ch))
            .collect();
        self.sync(None, false)?;
        // A state chain's frame is its displayed state, painted all or none.
        let chain_cursor = self.predictor.overlay_cursor();
        if chain_cursor.is_some() || (self.predictor.engaged_for_test() && !shown.is_empty()) {
            return self.frame_chain(&shown, chain_cursor);
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
        chain_cursor: Option<(u16, u16)>,
    ) -> Result<(), Violation> {
        let visible = self.predictor.visible_ops_for_test();
        let ids: Vec<KeyId> = self.mirror.iter().copied().take(visible).collect();
        let Some(&newest) = ids.last() else {
            return Err(violation(
                "provenance",
                format!("a chain's cursor {chain_cursor:?} is drawn with no op displayed"),
            ));
        };
        let mut d = self.scratch.clone();
        let blank = self.term.implicit_blank_render_cell();
        let (painted, drawn) = paint_chain(
            &mut d,
            shown,
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
            if ids.iter().any(|&k| {
                self.keys
                    .get(k)
                    .is_some_and(|l| matches!(l.edit, Some(EditKey::Left | EditKey::Right)))
            }) {
                self.stats.motions_displayed += 1;
            }
            if self
                .last_enter
                .is_some_and(|enter| ids.first().is_some_and(|&k| k < enter))
            {
                self.stats.carried_chains += 1;
            }
            if self.cfg.mode == PredictMode::Adaptive {
                self.record_frame(&d, &cells, newest, drawn);
            }
        }
        self.glass = shown
            .iter()
            .zip(&painted)
            .filter(|&(_, &p)| p)
            .map(|(&(r, c, ch), _)| (newest, r, c, ch))
            .collect();
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
            self.record_frame(d, &cells, newest, cursor);
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
    ) {
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
        let frame = FrameShown {
            text: frame_text(d, rows, cols),
            decoration,
            ghosts,
            cursor,
            space: self.space(),
            unprompted: self.unprompted_delivered,
            oldest,
        };
        if let Some(log) = self.keys.get_mut(newest)
            && log.frames.last().is_none_or(|last| {
                last.text != frame.text
                    || last.cursor != frame.cursor
                    || last.decoration != frame.decoration
                    || last.space != frame.space
                    || last.unprompted != frame.unprompted
                    || last.oldest != frame.oldest
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
        self.now += Duration::from_millis(u64::from(ms));
        let now = self.host_clock();
        if self.predictor.next_deadline().is_some_and(|d| d <= now) {
            self.predictor.expire_due(now);
            self.sync(None, false)?;
            self.frame()?;
        }
        Ok(())
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
            Op::Unprompted(what, param) => self.program.unprompted_output(what, param),
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
                n => Key::Unmodelled(
                    [
                        Unmodelled::Tab,
                        Unmodelled::Tab,
                        Unmodelled::Tab,
                        Unmodelled::KillLine,
                        Unmodelled::KillWord,
                        Unmodelled::Interrupt,
                        Unmodelled::Home,
                        Unmodelled::End,
                        Unmodelled::DeleteKey,
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
    let events = sim.predictor.events_for_test();
    sim.stats.expiry_owes = events.expiry_owes;
    sim.stats.tail_refusals = events.tail_refusals;
    sim.stats.decorated_proofs = events.decorated_proofs;
    sim.stats.typed_owes = events.typed_owes;
    sim.stats.standing_voids = events.standing_voids;
    sim.stats.advance_waits = events.advance_waits;
    sim.stats.motions_proven = events.motions_proven;
    sim.stats.net_null_retired = events.net_null_retired;
    sim.stats.transit_waits = events.transit_waits;
    sim.stats.decline_motions = events.decline_motions;
    sim.stats.vacate_refusals = events.vacate_refusals;
    result.map(|()| sim.stats)
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

/// The op that presses `←` and draws a frame.
pub(crate) const LEFT: Op = Op::Key(87, 1);

/// The op that presses `→` and draws a frame.
pub(crate) const RIGHT: Op = Op::Key(87, 2);

/// The op that presses Backspace and draws a frame.
pub(crate) const BACKSPACE: Op = Op::Key(70, 1);
