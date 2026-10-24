// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic simulation world and clause checker for `aterm-predict` tests.

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

/// Default simulation grid height.
pub(crate) const ROWS: u16 = 6;
pub(crate) const COLS: u16 = 12;
/// Alternate grid geometry used by host resize resets.
pub(crate) const ALT_GEOMETRY: (u16, u16) = (5, 14);
/// Glitch-window ceiling (2 s) plus one tick.
pub(crate) const SETTLE: Duration = Duration::from_millis(2001);
/// Glitch-window floor (250 ms).
pub(crate) const ECHO_DEADLINE: Duration = Duration::from_millis(250);
/// Default shell prompt.
pub(crate) const PROMPT: &str = "$ ";

/// Wider prompt used by [`Unprompted::ResetPrompt`].
pub(crate) const WIDE_PROMPT: &str = "$$$ ";
/// Italic right-hand prompt drawn under [`Config::rprompt`].
pub(crate) const RPROMPT: &str = "<r>";
/// Printable, wide, and combining glyphs sampled by `Sim::key_for`.
pub(crate) const TEXT: [char; 14] = [
    'a', 'b', 'c', 'd', 'e', 'i', 'n', 'o', 's', ' ', ' ', 'é', '日', '\u{301}',
];
/// Right-to-left letter (Hebrew alef) sampled under [`Config::bidi`].
pub(crate) const RTL: char = '\u{5d0}';

pub(crate) type KeyId = usize;

/// Simulated key event.
#[derive(Clone, Debug)]
pub(crate) enum Key {
    Glyph(char),
    /// Backspace routed to `Predictor::predict_edit`.
    Backspace,
    /// Main-block arrow routed to `Predictor::predict_edit`.
    Left,
    Right,
    Enter,
    /// Line-edge or Delete key routed to `Predictor::predict_edit` under shell integration.
    Learned(EditKey),
    Unmodelled(Unmodelled),
    Paste(Vec<char>),
}

/// Non-glyph keys routed to `Predictor::note_unmodelled_input`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unmodelled {
    Tab,
    KillLine,
    KillWord,
    Interrupt,
    /// Alt-B (backward-word).
    WordLeft,
    /// Up arrow (history recall).
    Up,
    /// Standalone Escape prefix.
    Esc,
    /// Keypad Left under Kitty `DISAMBIGUATE` (`CSI 57417 u`).
    KeypadLeft,
    /// zsh `push-line` (`ESC q`).
    PushLine,
    /// Ctrl-K (`kill-line`).
    KillToEnd,
}

/// Unprompted program output events.
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
    /// Re-executes the shell with the next [`Keymap`] and a fresh integration nonce.
    Rebind,
    /// Emits unsigned OSC 133 `A` and `B` marks at the cursor.
    ForgedMark,
    /// Pushes or pops Kitty `DISAMBIGUATE` or `modifyOtherKeys` level 2.
    KittyChords,
    /// Redraws the prompt in place using [`PROMPT`] or [`WIDE_PROMPT`] without a new `B` mark.
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

/// Single simulation step.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    /// Keypress (`.0` selects class, `.1` selects detail).
    Key(u32, u32),
    /// Program reads up to `.0` queued keys with command selector `.1`.
    Program(u8, u32),
    /// Delivers `.0`/1024 of the output in flight.
    Deliver(u16),
    /// Emits unprompted program output.
    Unprompted(Unprompted, u32),
    /// Advances the clock by `.0` ms and runs due expiry sweeps.
    Advance(u32),
    /// Renders a frame without new output.
    Frame,
    /// Resizes (`true`) or swaps the active pane (`false`).
    HostReset(bool),
}

/// Host wiring variant (`Faithful` or a negative-control mutant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    Faithful,
    /// Reports a frozen coordinate generation.
    FrozenCoordinates,
    /// Never reports the alternate screen.
    BlindToAltScreen,
    /// Paints each ghost one cell right of its guess.
    PaintsBesideTheGuess,
    /// Paints ghosts over wide glyphs without refusing.
    PaintsOverWideGlyphs,
    /// Runs the predictor in `Always` mode regardless of [`Config::mode`].
    ForcesAlways,
    /// Never notifies the predictor of line submits.
    IgnoresLineSubmits,
    /// Never configures `set_ambiguous_width_double`.
    IgnoresAmbiguousWidth,
    /// Samples the row right of the cursor as blank.
    TailBlind,
    /// Retains the previous frame's ghosts when the overlay becomes empty.
    KeepsStaleGhosts,
    /// Advances the host clock only on keypresses.
    StaleClock,
    /// Routes Alt-B as `EditKey::Left`.
    WordMotionAsArrow,
    /// Routes keypad Left under Kitty `DISAMBIGUATE` as `EditKey::Left`.
    KeypadArrowAsArrow,
    /// Draws a state chain's cursor one cell past its newest painted glyph.
    CursorAfterNewestGlyph,
    /// Paints state chain cells individually instead of all-or-none.
    PaintsPartialState,
    /// Draws a state chain's cursor even on a wide glyph's continuation cell.
    CursorOnAContinuation,
    /// Draws a state chain's cursor even when the span paint was refused.
    CursorWithoutSpan,
    /// Shifts cells past the end of the known line to the right margin.
    PaintsPastTheLine,
    /// Reuses the last seen input start across commands and coordinate changes.
    StaleInputStart,
    /// Accepts unsigned OSC 133 marks by disabling the nonce gate.
    UnsignedInputStart,
    /// Accepts any signed `B` mark without checking `aterm_input=start`.
    UnattestedInputStart,
    /// Uses a static [`EditorContext`] across shell rebinds and mode changes.
    StaleEditorContext,
    /// Ignores Kitty functional-key reporting when checking escaped chords.
    IgnoresKittyChords,
    /// Checks only Kitty mode (ignoring `modifyOtherKeys`) for escaped chords.
    IgnoresModifyOtherKeys,
    /// Disables a single predictor [`Rule`].
    Rule(Rule),
}

impl Host {
    /// Clause this mutant is expected to violate, or `None` for [`Host::Faithful`].
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

    /// Returns whether this mutant requires `ambiguous_width = "wide"`.
    pub(crate) fn wide(self) -> bool {
        self == Self::IgnoresAmbiguousWidth
    }
}

/// Mutant hosts caught by the seeded sweep in [`Host::breaks`].
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

/// Safety clause guarded by `rule`, or `None` for rules without a safety violation in this world.
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

/// Simulation configuration.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    pub(crate) mode: PredictMode,
    /// Simulates a raw remote link (ssh) where local termios cannot observe `ECHO`.
    pub(crate) remote: bool,
    pub(crate) host: Host,
    /// Emits explicit `\r\n` wraps when filling the last column.
    pub(crate) forced_wrap: bool,
    /// Enables East Asian Ambiguous double width (`ambiguous_width = "wide"`).
    pub(crate) wide: bool,
    /// Draws [`RPROMPT`] with each prompt and hides it before the line reaches it.
    pub(crate) rprompt: bool,
    /// Far-end line editor behavior.
    pub(crate) editor: Editor,
    /// Occasionally types [`RTL`] (`א`) to exercise BiDi reordering.
    pub(crate) bidi: bool,
    /// Enables authenticated OSC 133 shell integration marks and learned edit keys.
    pub(crate) integration: bool,
    /// Keymap binding for learned edit keys.
    pub(crate) keymap: Keymap,
}

/// Far-end keymap bindings for learned edit keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Keymap {
    /// Canonical bindings (Home/Ctrl-A start, End/Ctrl-E end, Delete delete-char).
    Canonical,
    /// `zsh -f` bindings (Home/End unbound, Delete inserts `~`, Ctrl-A/Ctrl-E canonical).
    ZshVanilla,
    /// zsh `viins` bindings (Ctrl-A/Ctrl-E self-insert `^A`/`^E`, Home/End/Delete canonical).
    ViIns,
}

/// Keymaps cycled by [`Unprompted::Rebind`] and sweeps.
pub(crate) const KEYMAPS: [Keymap; 3] = [Keymap::Canonical, Keymap::ZshVanilla, Keymap::ViIns];

/// Session ID passed to [`EditorContext`].
pub(crate) const SESSION: u64 = 1;

/// Capability nonce for the `n`-th shell instance.
pub(crate) fn nonce(n: u8) -> [u8; 32] {
    [0xA0 ^ n; 32]
}

/// Far-end line editor byte-output model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Editor {
    /// Readline without ICH: writes the glyph, redraws the shifted tail, and steps back.
    Readline,
    /// Readline with ICH (`ESC [ @`) and glyph-reprinting right arrow.
    ReadlineIch,
    /// Zsh line editor (`zle`): silent edges and `\b \b` end erases.
    Zle,
    /// Cooked tty discipline (`ICANON | ECHO | ECHOE | ECHOCTL`).
    Cooked,
    /// Readline `overwrite-mode`: replaces glyphs in place and rubs out with a blank.
    Overwrite,
    /// `zle` with `zsh-autosuggestions`.
    Autosuggest,
}

/// Editors rotated across sweep cases.
pub(crate) const EDITORS: [Editor; 6] = [
    Editor::ReadlineIch,
    Editor::Readline,
    Editor::Zle,
    Editor::Cooked,
    Editor::Overwrite,
    Editor::Autosuggest,
];

/// Coordinate generation and screen epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Space {
    generation: u64,
    screen: u64,
}

/// Armed guess coordinates, glyph, and coordinate space.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Armed {
    row: u16,
    col: u16,
    ch: char,
    space: Space,
}

/// Displayed guess coordinates, space, and unprompted byte count.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shown {
    row: u16,
    col: u16,
    space: Space,
    unprompted: u64,
}

/// Per-keystroke tracking log.
#[derive(Clone, Debug)]
pub(crate) struct KeyLog {
    key: Key,
    typed_at: Instant,
    armed: Option<Armed>,
    /// Typed while a no-echo prompt was visible on screen.
    secret: bool,
    shown: Option<Shown>,
    /// Whether the far-end editor had exact column tracking when consuming this key.
    consumed_reliable: bool,
    /// Timestamp when the last byte caused by this key landed.
    last_output_at: Option<Instant>,
    /// Predictor glitch window when [`Self::last_output_at`] landed.
    last_output_window: Duration,
    /// Adaptive frames whose newest painted ghost belonged to this key.
    frames: Vec<FrameShown>,
    /// Whether this keypress unconditionally ended the predictor's epoch.
    ends_epoch: bool,
    /// Predictor sequence number for this key's guess.
    seq: Option<u64>,
    /// Whether this key's echo glyph has landed.
    echo_landed: bool,
    /// Unprompted bytes delivered at press time.
    unprompted_at_press: u64,
    /// Edit key routed to the predictor for this press.
    edit: Option<EditKey>,
    /// Control-chord encoding active at press time.
    chord: ChordForm,
}

/// Wire encoding of a control chord at press time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChordForm {
    /// Raw C0 control byte.
    Byte,
    /// Kitty functional-key CSI `u` sequence.
    Kitty,
    /// xterm `modifyOtherKeys` level 2 sequence.
    ModifyOtherKeys,
}

impl ChordForm {
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

/// Predictor chain cursor and host-drawn cursor for a state chain frame.
pub(crate) type ChainCursors = (Option<(u16, u16)>, Option<(u16, u16)>);

/// Cell character, wide-continuation flag, and combining marks.
pub(crate) type CellText = (char, bool, Option<Box<str>>);

/// Snapshot of a displayed frame retained for frame-fidelity verification.
#[derive(Clone, Debug)]
pub(crate) struct FrameShown {
    text: Vec<Vec<CellText>>,
    decoration: Vec<Vec<bool>>,
    ghosts: Vec<(usize, usize)>,
    vacated: Vec<(usize, usize)>,
    cursor: Option<(u16, u16)>,
    space: Space,
    unprompted: u64,
    oldest: KeyId,
    learned: LearnedShown,
}

/// Flags indicating whether a frame displays a learned key or a learned Delete.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LearnedShown {
    any: bool,
    delete: bool,
}

/// Segment of program output in flight.
#[derive(Clone, Debug)]
pub(crate) struct Seg {
    bytes: Vec<u8>,
    kind: SegKind,
    cause: Option<KeyId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SegKind {
    Reply,
    Echo(KeyId),
    Unprompted,
    NoEchoPrompt(u64),
}

/// Simulated far-end program (line editor, cooked tty, password prompt, and alt-screen app).
pub(crate) struct Program {
    line: Vec<char>,
    cur: usize,
    noecho: bool,
    noecho_serial: u64,
    alt: bool,
    alt_from_command: bool,
    cols: usize,
    col: usize,
    reliable: bool,
    forced_wrap: bool,
    wide: bool,
    draws_rprompt: bool,
    editor: Editor,
    cat: bool,
    cooked_cells: Vec<usize>,
    rprompt: Option<usize>,
    suggestion: Vec<char>,
    suggestion_at: usize,
    integration: bool,
    shell: u8,
    keymap: Keymap,
    prompt: &'static str,
    pushed: Vec<char>,
    running: bool,
    chords: ChordForm,
    reading_chord: ChordForm,
    reads_one_line: bool,
    completions: &'static [&'static str],
    history: Option<Vec<Vec<char>>>,
    cause: Option<KeyId>,
    inbox: VecDeque<KeyId>,
    pipe: VecDeque<Seg>,
    head_off: usize,
}

/// Cell width of `c` under the given `ambiguous_width` policy.
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

    fn put(&mut self, kind: SegKind, c: char) {
        self.emit(kind, c.to_string());
        self.advance(c);
        if width(c, self.wide) > 0 && self.col >= self.cols && self.forced_wrap {
            self.reply("\r\n");
            self.col = 0;
        }
    }

    fn mark(&self, mark: &str) -> String {
        self.mark_saying(mark, "")
    }

    fn mark_saying(&self, mark: &str, word: &str) -> String {
        if !self.integration {
            return String::new();
        }
        format!(
            "\x1b]133;{mark};id={}{word}\x07",
            aterm_core::terminal::ShellIntegrationNonce(nonce(self.shell)).to_hex()
        )
    }

    fn start_command(&mut self) -> String {
        self.running = self.integration;
        self.mark("C")
    }

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

    fn zle(&self) -> bool {
        matches!(self.editor, Editor::Zle | Editor::Autosuggest)
    }

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
        self.reliable = self.col < self.cols;
        self.rprompt = None;
        let lo = self.cols.saturating_sub(1 + RPROMPT.chars().count());
        if self.draws_rprompt && lo > self.col + 1 {
            self.reply(format!(
                "\x1b[K\x1b7\x1b[{}G\x1b[3m{RPROMPT}\x1b[23m\x1b8",
                lo + 1
            ));
            self.rprompt = Some(lo);
        }
    }

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

    fn col_span(&self, chars: &[char]) -> usize {
        chars.iter().map(|&c| width(c, self.wide)).sum()
    }

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

    fn insert(&mut self, c: char, echo_of: Option<KeyId>, choice: u32) {
        if width(c, self.wide) > 0 {
            self.hide_rprompt_for(c);
        }
        let kind = echo_of.map_or(SegKind::Reply, SegKind::Echo);
        let w = width(c, self.wide);
        if self.cur < self.line.len() && w > 0 {
            if self.col >= self.cols {
                self.reliable = false;
            }
            if self.editor == Editor::Overwrite {
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
                let end = self.col + tail;
                self.put(kind, c);
                let rest: Vec<char> = self.line.get(self.cur..).unwrap_or_default().to_vec();
                for &t in &rest {
                    self.put(SegKind::Reply, t);
                }
                let back = self.back_over(&rest);
                self.reply(back);
                self.line.insert(self.cur, c);
                self.cur += 1;
                self.blank_suggestion(end + w);
                self.refetch_after_edit(end + w);
                return;
            }
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

    fn suggest_after(&mut self, choice: u32) {
        if choice % 8 != 3 || self.noecho || self.alt {
            return;
        }
        let s = ["ion", "sne", "ab", "dice", "e a"][((choice / 8) % 5) as usize];
        let n = s.chars().count();
        self.reply(format!("\x1b[2;3m{s}\x1b[0m\x1b[{n}D"));
        if self.col + n >= self.cols {
            self.reliable = false;
        }
    }

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

    fn accept_suggestion(&mut self) {
        let s = std::mem::take(&mut self.suggestion);
        for &c in &s {
            self.put(SegKind::Reply, c);
            self.line.push(c);
        }
        self.cur = self.line.len();
    }

    fn reset_line(&mut self) {
        self.line.clear();
        self.cooked_cells.clear();
        self.cur = 0;
        self.suggestion.clear();
    }

    fn cooked(&self) -> bool {
        self.editor == Editor::Cooked || self.cat
    }

    fn edge(&mut self) {
        if matches!(
            self.editor,
            Editor::Readline | Editor::ReadlineIch | Editor::Overwrite
        ) {
            self.reply("\x07");
        }
    }

    fn rubout(&mut self) {
        if self.editor != Editor::Overwrite || self.cur == 0 || self.cur >= self.line.len() {
            self.erase_back(1);
            return;
        }
        let i = self.cur - 1;
        let gone = self.line.get(i).copied().unwrap_or(' ');
        let w = width(gone, self.wide);
        if w != 1 {
            self.reliable = false;
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
        if tail.first().is_some_and(|&c| width(c, self.wide) == 0) {
            self.reliable = false;
        }
        if self.editor == Editor::Autosuggest && tail.is_empty() && n == 1 {
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
        out.push_str("\x1b[K");
        self.rprompt = None;
        self.reply(out);
        for &c in &tail {
            self.put(SegKind::Reply, c);
        }
        let back = self.back_over(&tail);
        self.reply(back);
    }

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

    fn right(&mut self) {
        let Some(&c) = self.line.get(self.cur) else {
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

    fn delete_forward(&mut self) {
        if self.cur >= self.line.len() {
            self.edge();
            return;
        }
        let gone = self.line.remove(self.cur);
        let blanks = width(gone, self.wide);
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

    fn learned_key(&mut self, key: EditKey) {
        if self.reading_chord != ChordForm::Byte {
            self.reply("\x07");
            return;
        }
        match (self.keymap, key) {
            (Keymap::ZshVanilla, EditKey::Home | EditKey::End) => self.reply("\x07"),
            (Keymap::ZshVanilla, EditKey::Delete) => self.insert('~', None, 0),
            (Keymap::ViIns, EditKey::CtrlA | EditKey::CtrlE) => {
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

    fn line_start(&mut self) {
        let before: Vec<char> = self.line.get(..self.cur).unwrap_or_default().to_vec();
        self.cur = 0;
        let back = self.back_over(&before);
        self.reply(back);
    }

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
        self.reset_line();
        let text: String = recalled.iter().collect();
        self.reply(format!("\r\x1b[K{}{text}", self.prompt));
        self.col = self.prompt.chars().count() + self.col_span(&recalled);
        self.line = recalled;
        self.cur = self.line.len();
        self.rprompt = None;
    }

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

    fn run_command(&mut self, choice: u32) {
        self.blank_suggestion(0);
        if let Some(history) = self.history.as_mut().filter(|_| !self.line.is_empty()) {
            history.push(self.line.clone());
        }
        self.reset_line();
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
            3 if (choice / 8) % 2 == 1 => {
                self.reply(format!("\r\n{c}"));
                self.col = 0;
                self.cat = true;
                self.reliable = true;
            }
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

    fn quit_app(&mut self) {
        self.alt = false;
        self.reset_line();
        self.rprompt = None;
        let (a, b) = self.prompt_marks();
        self.reply(format!("\x1b[?1049l{a}{}{b}", self.prompt));
        if self.alt_from_command {
            self.col = self.prompt.chars().count();
            self.reliable = true;
        } else {
            self.reliable = false;
        }
    }

    fn consume(&mut self, id: KeyId, key: &Key, choice: u32) {
        self.cause = Some(id);
        if self.alt {
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
                self.cat = false;
                self.reads_one_line = false;
                self.reset_line();
                self.prompt_after("^C\r\n");
            }
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
            Key::Unmodelled(Unmodelled::KillToEnd) => self.tty_echo(&['\x0b'], SegKind::Reply),
        }
    }

    fn unprompted_output(&mut self, what: Unprompted, param: u32) {
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
                let n = 1 + param % 9;
                self.unprompted(format!("\x1b7\x1b[{n}C\x1b[3m<rp>\x1b[23m\x1b8"));
            }
            Unprompted::Suggest => {
                if self.cur == self.line.len() && !self.noecho && !self.alt {
                    let s = ["ion", "sne", "ab", "dice", "e a"][(param % 5) as usize];
                    let n = s.chars().count();
                    self.unprompted(format!("\x1b[2;3m{s}\x1b[0m\x1b[{n}D"));
                    if self.col + n >= self.cols {
                        self.reliable = false;
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
                self.unprompted("\x1b]133;A\x07\x1b]133;B;aterm_input=start\x07");
            }
            Unprompted::KittyChords => {
                if !self.alt {
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

/// Clause violation details.
#[derive(Clone, Debug)]
pub(crate) struct Violation {
    pub(crate) clause: &'static str,
    pub(crate) detail: String,
}

pub(crate) fn violation(clause: &'static str, detail: String) -> Violation {
    Violation { clause, detail }
}

/// Cumulative coverage counters across simulation runs.
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
    pub(crate) ghosts_refused: u64,
    pub(crate) frame_fidelity_checks: u64,
    pub(crate) stale_decoration: u64,
    pub(crate) rprompt_frames: u64,
    pub(crate) timeliness_checks: u64,
    pub(crate) attribution_checks: u64,
    pub(crate) expiry_owes: u64,
    pub(crate) tail_refusals: u64,
    pub(crate) decorated_proofs: u64,
    pub(crate) typed_owes: u64,
    pub(crate) standing_voids: u64,
    pub(crate) advance_waits: u64,
    pub(crate) text_waits: u64,
    pub(crate) edit_keys_armed: u64,
    pub(crate) cooked_motions_armed: u64,
    pub(crate) edit_frames_displaying: u64,
    pub(crate) motions_displayed: u64,
    pub(crate) carried_chains: u64,
    pub(crate) oracle_checks: u64,
    pub(crate) motions_proven: u64,
    pub(crate) net_null_retired: u64,
    pub(crate) transit_waits: u64,
    pub(crate) decline_motions: u64,
    pub(crate) vacate_refusals: u64,
    pub(crate) shifts_displayed: u64,
    pub(crate) overwrite_shifts_armed: u64,
    pub(crate) overwrite_shifts_displayed: u64,
    pub(crate) late_decoration: u64,
    pub(crate) shifts_armed: u64,
    pub(crate) shifts_proven: u64,
    pub(crate) opaque_refusals: u64,
    pub(crate) mid_line_flushes: u64,
    pub(crate) ambiguous_shifts: u64,
    pub(crate) bidi_refusals: u64,
    pub(crate) line_edge_displayed: u64,
    pub(crate) delete_displayed: u64,
    pub(crate) learned_fidelity_checks: u64,
    pub(crate) delete_fidelity_checks: u64,
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

/// Projects logical cell `(row, col)` of `input` into [`Seen`].
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

/// Extracts a `rows` by `cols` grid of [`CellText`] from `f`: each cell is [`cell_text`]'s,
/// read a row at a time, its few marks placed once each instead of sought for every cell.
pub(crate) fn frame_text(f: &RenderInput, rows: usize, cols: usize) -> Vec<Vec<CellText>> {
    (0..rows)
        .map(|r| {
            let cells = f.cells.get(r).map_or(&[][..], Vec::as_slice);
            let mut line: Vec<CellText> = (0..cols)
                .map(|c| {
                    cells
                        .get(c)
                        .map_or((' ', false, None), |cell| (cell.ch, cell.wide, None))
                })
                .collect();
            // A cell's first cluster, then (with none) its first combining marks.
            for (at, cluster) in f.clusters.get(r).into_iter().flatten() {
                if let Some(text) = line.get_mut(*at)
                    && text.2.is_none()
                {
                    text.2 = Some(cluster.clone());
                }
            }
            for (at, marks) in f.combining.get(r).into_iter().flatten() {
                if let Some(text) = line.get_mut(*at)
                    && text.2.is_none()
                {
                    text.2 = Some(marks.iter().collect::<String>().into_boxed_str());
                }
            }
            line
        })
        .collect()
}

/// [`frame_text`] reads every cell as [`cell_text`] does: a row shorter than the grid, a missing
/// row, a wide cell, a cluster and combining marks on one cell, a second entry at a column, and
/// marks past the grid. NEGATIVE CONTROL: placing combining marks before clusters, or letting a
/// later entry at a column win, changes a cell.
#[test]
fn frame_text_reads_each_cell_as_cell_text() {
    let blank = Terminal::new(2, 4).implicit_blank_render_cell();
    let at = |ch, wide| RenderCell { ch, wide, ..blank };
    let mut f = RenderInput::empty();
    f.cells = vec![
        vec![
            at('a', false),
            at('\u{4e16}', false),
            at(' ', true),
            at('b', false),
        ],
        vec![at('c', false)],
        Vec::new(),
    ];
    f.clusters = vec![
        vec![
            (0, Box::from("a\u{301}")),
            (3, Box::from("b\u{300}")),
            (0, Box::from("x")),
        ],
        vec![(5, Box::from("past"))],
    ];
    f.combining = vec![
        vec![(0, Box::from(['\u{302}'])), (3, Box::from(['\u{303}']))],
        vec![
            (0, Box::from(['\u{304}'])),
            (0, Box::from(['\u{305}'])),
            (2, Box::from(['\u{306}'])),
        ],
        vec![(1, Box::from(['\u{307}', '\u{308}']))],
    ];
    let (rows, cols) = (4, 5);
    let read: Vec<Vec<CellText>> = (0..rows)
        .map(|r| (0..cols).map(|c| cell_text(&f, r, c)).collect())
        .collect();
    assert_eq!(frame_text(&f, rows, cols), read);
}

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

/// Returns the glyph-lane cursor one column past `(row, col)`, or `None` at the right margin.
pub(crate) fn displayed_cursor(row: u16, col: u16, cols: u16) -> Option<(u16, u16)> {
    let next = col.saturating_add(1);
    (next < cols).then_some((row, next))
}

/// Converts `(ch, kind)` into a [`SpanCell`] for `RenderInput::paint_prediction_span`.
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

/// Paints a state chain's span and resolves its drawn cursor under `host`.
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

/// Checks atomicity and cursor locality for a painted state chain frame.
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

/// Returns the per-cell italic decoration mask for `f`.
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

/// Creates a `Terminal` configured for `cfg`.
pub(crate) fn engine(cfg: Config, rows: u16, cols: u16) -> Terminal {
    let mut term = Terminal::new(rows, cols);
    if cfg.wide {
        let config = aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..aterm_core::config::TerminalConfig::default()
        };
        term.apply_config(&config);
    }
    if cfg.integration {
        term.authorize_shell_integration(nonce(0));
        term.set_require_shell_integration_nonce(cfg.host != Host::UnsignedInputStart);
    }
    term
}

/// Simulated session combining engine, reference fold, far-end program, host, and predictor.
pub(crate) struct Sim {
    cfg: Config,
    term: Terminal,
    reference: Terminal,
    scratch: RenderInput,
    predictor: Predictor,
    program: Program,
    now: Instant,
    last_press: Instant,
    keys: Vec<KeyLog>,
    late_outputs: std::collections::BTreeSet<KeyId>,
    mirror: VecDeque<KeyId>,
    screen_epoch: u64,
    was_alt: bool,
    last_generation: u64,
    unprompted_delivered: u64,
    noecho_visible: Option<u64>,
    landing: Option<(KeyId, u16, u16, Space, u64)>,
    glass: Vec<(KeyId, u16, u16, char)>,
    press_scratch: RenderInput,
    press_row: Vec<Seen>,
    press_below: Vec<Seen>,
    stats: Stats,
    last_enter: Option<KeyId>,
    oracle: Option<OracleBase>,
    stale_start: Option<InputStart>,
    shell: u8,
    trace: Option<String>,
    checks: bool,
    glass_frame: Option<OnGlass>,
    cost: Cost,
    editing: bool,
}

/// Displayed frame and cursor placed on glass.
#[derive(Clone, Debug)]
pub(crate) struct OnGlass {
    pub(crate) frame: RenderInput,
    pub(crate) cursor: Option<(u16, u16)>,
    pub(crate) rows: Vec<u16>,
}

/// Cumulative host observation and fold cost counters.
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

/// Authoritative line snapshot used as the base for [`Sim::oracle_check`].
#[derive(Clone, Debug)]
pub(crate) struct OracleBase {
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

    /// Creates a session with explicit `(rows, cols)` dimensions.
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

    /// Enables or disables the predictive edit lane.
    pub(crate) fn set_editing(&mut self, on: bool) {
        self.editing = on;
        self.predictor.set_editing(on);
    }

    /// Returns a mutable reference to the underlying [`Predictor`].
    pub(crate) fn predictor_mut(&mut self) -> &mut Predictor {
        &mut self.predictor
    }

    /// Returns the authoritative [`Terminal`].
    pub(crate) fn term(&self) -> &Terminal {
        &self.term
    }

    /// Disables theorem clause checking for measurement runs.
    pub(crate) fn without_checks(&mut self) {
        self.checks = false;
    }

    /// Returns cumulative coverage statistics.
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

    /// Returns cumulative host cost counters.
    pub(crate) fn cost(&self) -> Cost {
        self.cost
    }

    /// Returns the frame painted on glass by the last redraw, if any.
    pub(crate) fn on_glass(&self) -> Option<&OnGlass> {
        self.glass_frame.as_ref()
    }

    /// Returns the unpainted authoritative frame from the last redraw.
    pub(crate) fn screen(&self) -> &RenderInput {
        &self.scratch
    }

    /// Configures Tab completion words and history tracking for corpus runs.
    pub(crate) fn corpus_shell(&mut self, completions: &'static [&'static str]) {
        self.program.completions = completions;
        self.program.history = Some(Vec::new());
    }

    /// Emits and delivers an initial prompt before the first key.
    pub(crate) fn fresh_prompt(&mut self) -> Result<(), Violation> {
        self.program.prompt_after("");
        let total = self.program.pending_bytes();
        self.deliver(total)?;
        self.frame()
    }

    /// Processes `key` and renders a frame.
    pub(crate) fn press_and_draw(&mut self, key: Key) -> Result<(), Violation> {
        let folded = self.predictor.events_for_test().ops_folded;
        self.press(key)?;
        let folds = self.predictor.events_for_test().ops_folded - folded;
        self.cost.presses += 1;
        self.cost.press_folds += folds;
        self.cost.press_folds_max = self.cost.press_folds_max.max(folds);
        self.frame()
    }

    /// Consumes the oldest queued key in the far-end program and returns the bytes emitted.
    pub(crate) fn read_key(&mut self, choice: u32) -> usize {
        let before = self.program.pending_bytes();
        self.program_step(1, choice);
        self.program.pending_bytes() - before
    }

    /// Returns the far-end line length and cursor index.
    pub(crate) fn buffer(&self) -> (usize, usize) {
        (self.program.line.len(), self.program.cur)
    }

    /// Delivers `n` bytes of pending output and renders a frame.
    pub(crate) fn deliver_and_draw(&mut self, n: usize) -> Result<(), Violation> {
        self.deliver(n)?;
        self.frame()
    }

    /// Advances the clock to `at` and runs any due expiry frame.
    pub(crate) fn advance_to(&mut self, at: Instant) -> Result<bool, Violation> {
        self.advance_by(at.saturating_duration_since(self.now))
    }

    /// Returns the predictor's next expiry deadline.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.predictor.next_deadline()
    }

    /// Returns the current simulation timestamp.
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

    fn host_generation(&self) -> u64 {
        match self.cfg.host {
            Host::FrozenCoordinates => 0,
            _ => self.generation(),
        }
    }

    fn host_mode_of(cfg: Config) -> PredictMode {
        match cfg.host {
            Host::ForcesAlways => PredictMode::Always,
            _ => cfg.mode,
        }
    }

    fn host_mode(&self) -> PredictMode {
        Self::host_mode_of(self.cfg)
    }

    fn tell_ambiguous_width(&mut self) {
        if self.cfg.host != Host::IgnoresAmbiguousWidth {
            self.predictor.set_ambiguous_width_double(self.cfg.wide);
        }
    }

    fn host_alt(&self) -> bool {
        match self.cfg.host {
            Host::BlindToAltScreen => false,
            _ => self.term.is_alternate_screen(),
        }
    }

    fn host_input_start(&mut self) -> Option<InputStart> {
        let term = &self.term;
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

    fn host_escaped_chords(&self) -> bool {
        match self.cfg.host {
            Host::IgnoresKittyChords => false,
            Host::IgnoresModifyOtherKeys => self.term.kitty_reports_functional_keys(),
            _ => self.term.control_chords_escaped(),
        }
    }

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

    fn host_clock(&self) -> Instant {
        match self.cfg.host {
            Host::StaleClock => self.last_press,
            _ => self.now,
        }
    }

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
        let edit = self.edit_key_of(&key);
        if let Some(log) = self.keys.get_mut(id) {
            log.edit = edit;
        }
        let wants_row = (matches!(key, Key::Glyph(_)) && self.predictor.wants_key_row())
            || (edit.is_some() && self.editing);
        let cursor = self.term.cursor();
        let coords = self.host_generation();
        let grid = (self.term.cols(), self.term.rows());
        let no_echo = self.host_alt();
        let has_row = wants_row && self.sample_press_row(cursor.row, cursor.col);
        let input_start = self.host_input_start();
        let context = self.host_context();
        let escaped_chords = self.host_escaped_chords();
        let submit_prompt = self.host_submit_prompt();
        let wrap_pending = self.term.grid().pending_wrap();
        let tty_swallows = !self.cfg.remote && self.program.noecho && !self.program.alt;
        let now = self.now;
        let mode = self.host_mode();
        let shifts_before = self.predictor.events_for_test().shifts_armed;
        let ends_epoch = match key {
            _ if (matches!(key, Key::Glyph(_)) || edit.is_some()) && (no_echo || tty_swallows) => {
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
        if self.predictor.events_for_test().shifts_armed > shifts_before
            && self.program.editor == Editor::Overwrite
            && !self.program.cooked()
        {
            self.stats.overwrite_shifts_armed += 1;
        }
        self.program.inbox.push_back(id);
        Ok(())
    }

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
            let stayed = self.program.reliable;
            if let Some(log) = self.keys.get_mut(id) {
                log.consumed_reliable = reliable && stayed;
            }
        }
    }

    fn note(&mut self, line: impl FnOnce() -> String) {
        if let Some(trace) = self.trace.as_mut() {
            trace.push_str("  ");
            trace.push_str(&line());
            trace.push('\n');
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.note(|| format!("deliver {:?}", String::from_utf8_lossy(bytes)));
        self.term.process(bytes);
        self.reference.process(bytes);
        self.observe_engine();
    }

    fn deliver(&mut self, mut n: usize) -> Result<(), Violation> {
        while n > 0 {
            let Some(seg) = self.program.pipe.front().cloned() else {
                break;
            };
            let start = self.program.head_off;
            if start == 0 {
                match seg.kind {
                    SegKind::Echo(id) => {
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

    fn late_predecessor(&self, id: KeyId) -> bool {
        let before = self.keys.get(..id).unwrap_or_default();
        let Some(typed) = self.keys.get(id).map(|log| log.typed_at) else {
            return false;
        };
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
                continue;
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

    fn overdue_predecessor(&self, k: KeyId, window: Duration) -> bool {
        let window = ECHO_DEADLINE.max(window);
        let before = self.keys.get(..k).unwrap_or_default();
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

    fn unprompted_in_transit(&self) -> bool {
        self.program.head_off > 0
            && self.program.pipe.front().is_some_and(|seg| {
                matches!(seg.kind, SegKind::Unprompted | SegKind::NoEchoPrompt(_))
            })
    }

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
        let window = self.predictor.glitch_window_for_test();
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
        let chain_cursor = self.predictor.overlay_cursor();
        if chain_cursor.is_some() || (self.predictor.engaged_for_test() && !shown.is_empty()) {
            return self.frame_chain(&shown, &kinds, chain_cursor);
        }
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
        let mut rows: Vec<u16> = cells.iter().map(|&(r, _)| r).collect();
        rows.dedup();
        self.glass_frame = (drawn.is_some() || !cells.is_empty()).then_some(OnGlass {
            frame: d,
            cursor: drawn,
            rows,
        });
        checked
    }

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
        let ids: Vec<KeyId> = self.mirror.iter().copied().take(shown.len()).collect();
        if ids.len() != shown.len() {
            return Err(violation(
                "provenance",
                format!("{} guesses displayed, {} pending", shown.len(), ids.len()),
            ));
        }
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
            if self.cfg.mode == PredictMode::Adaptive && log.secret {
                return Err(violation(
                    "secrecy",
                    format!(
                        "key {id} {ch:?}, typed at a visible no-echo prompt, is displayed at ({row}, {col})"
                    ),
                ));
            }
        }
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
        if let Some(d) = displayed {
            let on_glass: Vec<(u16, u16, char)> = glass
                .iter()
                .map(|&(_, row, col, ch)| (row, col, ch))
                .collect();
            self.locality_on_glass(&on_glass, d)?;
        }
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
            // A row the two frames hold alike passes every cell's test below.
            if a.cells.get(r) == d.cells.get(r)
                && a.clusters.get(r) == d.clusters.get(r)
                && a.combining.get(r) == d.combining.get(r)
            {
                continue;
            }
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

    fn advance(&mut self, ms: u32) -> Result<(), Violation> {
        self.advance_by(Duration::from_millis(u64::from(ms)))
            .map(|_drew| ())
    }

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

    fn step(&mut self, op: Op) -> Result<(), Violation> {
        self.note(|| format!("{op:?}"));
        match op {
            Op::Key(class, detail) => {
                let key = self.key_for(class, detail);
                self.press(key)?;
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
                    Key::Glyph(RTL)
                } else {
                    let i = detail as usize % (TEXT.len() * 4);
                    Key::Glyph(TEXT.get(i).copied().unwrap_or(TEXT[i % 11]))
                }
            }
            70..=78 => Key::Backspace,
            79..=86 => Key::Enter,
            87..=95 => match detail % 13 {
                1 => Key::Left,
                2 => Key::Right,
                6 if self.cfg.integration && (detail / 13) % 2 == 1 => Key::Learned(EditKey::CtrlA),
                7 if self.cfg.integration && (detail / 13) % 2 == 1 => Key::Learned(EditKey::CtrlE),
                6 => Key::Learned(EditKey::Home),
                7 => Key::Learned(EditKey::End),
                8 => Key::Learned(EditKey::Delete),
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

/// Runs `ops` to quiescence under `cfg`.
pub(crate) fn run(cfg: Config, ops: &[Op]) -> Result<Stats, (usize, Violation)> {
    run_traced(cfg, ops, &mut None)
}

/// Runs `ops` to quiescence, recording a step trace when `trace` is `Some`.
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

/// Simulated link latency profile.
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

/// Generates a deterministic sequence of `len` simulation ops.
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
                    0..=37 => match b % 100 {
                        0..=85 => Op::Key(b % 70, c),
                        86..=91 => Op::Key(87, 1 + c % 2),
                        92..=97 => Op::Key(70 + b % 9, c),
                        _ => Op::Key(79 + b % 8, c),
                    },
                    38..=55 => Op::Program(1, c),
                    56..=69 => Op::Deliver(1024),
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

/// Appends a fresh-prompt typing motif to `ops`.
pub(crate) fn fresh_line(rng: &mut XorShift, link: Link, ops: &mut Vec<Op>) {
    let command = if rng.below(2) == 0 {
        6
    } else {
        u32::try_from(rng.below(3)).unwrap_or(0) * 8
    };
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

/// Appends a mid-line typo-fix motif to `ops`.
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

/// Appends a line-editing motion and backspace motif to `ops`.
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

/// Appends a mid-line shift (insert or erase) motif to `ops`.
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

/// Learned-key ops for Home, End, Ctrl-A, Ctrl-E, and Delete.
pub(crate) const HOME: Op = Op::Key(87, 6);
pub(crate) const END: Op = Op::Key(87, 7);
pub(crate) const CTRL_A: Op = Op::Key(87, 19);
pub(crate) const CTRL_E: Op = Op::Key(87, 46);
pub(crate) const DELETE: Op = Op::Key(87, 21);

/// Interrupt (`^C`) op.
pub(crate) const INTERRUPT: Op = Op::Key(87, 5);

/// zsh `push-line` (`ESC q`) op.
pub(crate) const PUSH_LINE: Op = Op::Key(87, 39);

/// Appends a learned line-edge and Delete motif to `ops`.
pub(crate) fn line_edge_motif(rng: &mut XorShift, link: Link, ops: &mut Vec<Op>) {
    let command = [0, 6, 8, 19][rng.index(4)];
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
            6 | 7 if rng.below(2) == 0 => {
                ops.push(LEFT);
                unread += 1;
                DELETE
            }
            6 | 7 => DELETE,
            8 => LEFT,
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

/// Returns whether `ops` still fails `clause` under `cfg`.
pub(crate) fn still_fails(cfg: Config, ops: &[Op], clause: &str) -> bool {
    matches!(run(cfg, ops), Err((_, v)) if v.clause == clause)
}

/// Delta-debugs `ops` down to a minimal sequence failing `clause`.
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

/// Returns all shipped faithful configurations.
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

/// Derives the sweep seed for index `i` from [`SEED`].
pub(crate) fn seed_index(i: u64) -> u64 {
    SEED ^ i.wrapping_mul(0xA24B_AED4_963E_E407)
}

/// Sweeps all [`faithful_configs`] across `indices` in parallel.
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

/// Builds the configuration, seed, link profile, and op list for `case`.
pub(crate) fn case_world(cfg: Config, seed: u64, case: usize) -> (Config, u64, Link, Vec<Op>) {
    let case_seed = seed ^ (case as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut rng = XorShift::new(case_seed);
    let link = LINKS[case % LINKS.len()];
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

/// Runs all [`CASES`] for `cfg` and `seed`, shrinking the first failure.
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

/// Returns an [`Op::Key`] that presses printable glyph `c` and renders a frame.
pub(crate) fn glyph(c: char) -> Op {
    let detail = (0u32..56)
        .find(|&d| {
            let i = d as usize;
            TEXT.get(i).copied().unwrap_or(TEXT[i % 11]) == c && d % 4 != 0
        })
        .expect("a glyph `Sim::key_for` can press");
    Op::Key(0, detail)
}

/// [`Op::Key`] that presses [`RTL`] under [`Config::bidi`].
pub(crate) const RTL_GLYPH: Op = Op::Key(0, (5 << 24) | 1);

/// Left-arrow op.
pub(crate) const LEFT: Op = Op::Key(87, 1);

/// Right-arrow op.
pub(crate) const RIGHT: Op = Op::Key(87, 2);

/// Backspace op.
pub(crate) const BACKSPACE: Op = Op::Key(70, 1);

/// Alt-B (word-left) op.
pub(crate) const WORD_LEFT: Op = Op::Key(87, 9);
