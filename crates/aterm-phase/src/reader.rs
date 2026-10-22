// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One screen reader per program, and the one place that picks it.
//!
//! The crate's grammar outside [`crate::agy`], [`crate::muse`] and [`crate::gemini`] is Claude
//! Code's; run on a shell it reads a quoted `Esc to cancel` as a box, and on Codex it reads the
//! trust gate as `idle`. So a caller names the program ([`identify`]) and asks that program's
//! reader ([`ScreenReader`]):
//!
//! * [`ClaudeReader`] — this crate's Claude Code grammar, behind the trait.
//!   Its `idle` is authoritative only at its prompt box (the `❯` composer,
//!   or shell mode's `!`) or on a wall: the screens between a launch and the
//!   REPL read `idle` NOT authoritatively; and read where the terminal's
//!   cursor is known ([`read_at`]), only at the box that HOLDS the cursor,
//!   never at a box an earlier run left on the screen;
//! * [`CodexReader`] — Codex by NAME only: it reads nothing on Codex's
//!   screen (aterm's Codex screen lane was retired on 2026-10-16), so every
//!   reading is `idle` NOT authoritatively and the server publishes
//!   `agent=unknown`. A Codex's own state is its OSC 7501 report, shown and
//!   never decided on;
//! * [`AgyReader`] — agy's grammar ([`crate::agy`], Google's Antigravity
//!   CLI, measured on 1.4.0): its boxes with their roles, the spinner row as
//!   busy, its footer's `? for shortcuts` as idle, its sign-in wall and
//!   interrupt;
//! * [`MuseReader`] — Muse Code's grammar ([`crate::muse`], measured on
//!   1.4.9-R6297.1 and 1.4.10-R6427.1): its trust box, approval boxes and
//!   question with their roles, the status row as busy, a reply as the last
//!   thing drawn as idle, its walls (a refused key, the usage wall, a failed
//!   model call) and its interrupt;
//! * [`GeminiReader`] — Gemini CLI's grammar ([`crate::gemini`], Google's
//!   Gemini CLI, measured on 0.46.0): its boxes with their roles (its
//!   usage-limit dialog and its folder trust among them), the spinner on its
//!   status row as busy, `? for shortcuts` there as idle, its API error rows
//!   as walls and its cancel as the person's;
//! * [`GenericReader`] — any other program: never a prompt, a question or a
//!   wall, always `idle`, never authoritative. The screen alone says
//!   nothing program-neutral about whether a build or a REPL is working;
//!   aterm's own `status phase=` (running | quiet) is the authority there.
//!
//! A policy that acts on a phase requires [`Reading::phase_authoritative`]: a reader's
//! default is not evidence. [`read`] is the whole reading in one call.

use crate::anchors::guard_regex;
use crate::login::LoginScreen;
use crate::phase::{
    Phase, busy_signal, composer_draft, context_left, context_left_shown, has_composer_frame,
    has_shell_mode_frame, prompt_box_holds, worker_phase,
};
use crate::prompt::{CancelEffect, PromptKind, PromptV2, parse_prompt_v2};
use crate::turn::{continuation_suggestion, goal_active, interrupted, said_tail};
use crate::wall::{Wall, memory_wall, wall};
use crate::{agy, gemini, muse};

/// The program a reader is for. Every variant is in [`Program::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Program {
    Claude,
    Codex,
    /// Google's Antigravity CLI.
    Agy,
    /// Meta's Muse Code.
    Muse,
    /// Google's Gemini CLI.
    Gemini,
    /// Anything else, or nothing named.
    Generic,
}

/// Each program's successor in declaration order, `None` after the last: an
/// exhaustive match, so a variant added to [`Program`] does not compile until
/// it is placed here, and the walk below holds [`Program::ALL`] to this chain.
#[inline]
const fn next_program(p: Program) -> Option<Program> {
    match p {
        Program::Claude => Some(Program::Codex),
        Program::Codex => Some(Program::Agy),
        Program::Agy => Some(Program::Muse),
        Program::Muse => Some(Program::Gemini),
        Program::Gemini => Some(Program::Generic),
        Program::Generic => None,
    }
}

// [`Program::ALL`] is exactly the chain [`next_program`] walks from the first
// declared variant, each at its own discriminant: checked at compile time.
const _: () = {
    let mut at = 0;
    let mut next = Some(Program::ALL[0]);
    while let Some(p) = next {
        assert!(at < Program::ALL.len() && p as usize == at && Program::ALL[at] as usize == at);
        at += 1;
        next = next_program(p);
    }
    assert!(at == Program::ALL.len());
};

impl Program {
    /// Every program, in declaration order: what must cover each one (the
    /// panic sweeps, the host's admission, a settings row per supervisable
    /// agent) iterates this, never a list of its own. Every variant is here BY
    /// CONSTRUCTION: [`next_program`] places each one (an exhaustive match, so
    /// a new variant does not compile until it is placed), and a compile-time
    /// walk of that chain holds this list to it.
    pub const ALL: [Program; 6] = [
        Program::Claude,
        Program::Codex,
        Program::Agy,
        Program::Muse,
        Program::Gemini,
        Program::Generic,
    ];

    /// The lowercase word the CLI prints.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Program::Claude => "claude",
            Program::Codex => "codex",
            Program::Agy => "agy",
            Program::Muse => "muse",
            Program::Gemini => "gemini",
            Program::Generic => "generic",
        }
    }

    /// The product's name as the harness's own surfaces print it ("Claude
    /// Code", "Codex", "Antigravity CLI", "Muse Code" — atpkg's spellings of
    /// those two — "Gemini CLI", the primer's; "the program" for any other,
    /// so a sentence that names it still reads) — the one display name
    /// there, so a message, a status line and a settings row name an agent
    /// alike. The primer and atpkg keep their own "Codex CLI".
    #[inline]
    #[must_use]
    pub const fn product(self) -> &'static str {
        match self {
            Program::Claude => "Claude Code",
            Program::Codex => "Codex",
            Program::Agy => "Antigravity CLI",
            Program::Muse => "Muse Code",
            Program::Gemini => "Gemini CLI",
            Program::Generic => "the program",
        }
    }

    /// Whether the supervisor's screen policy acts on this program: one whose
    /// reader is MEASURED and whose supervisor arms are written — Claude Code,
    /// agy since its reader passed its captures ([`crate::agy::fixtures`]), Muse Code
    /// since its boxes, question and walls were measured on 1.4.10-R6427.1
    /// ([`crate::muse::fixtures`]) and each is answered by the cursor and
    /// Enter it takes, and Gemini CLI since its reader passed its own
    /// ([`crate::gemini::fixtures`]) and a supervised session's press and
    /// typed line landed in its 0.46.0 under the lab. Never the generic
    /// reader, and never Codex: aterm's Codex screen lane was retired on
    /// 2026-10-16 (the owner: "retire the codex harness"), so a vendor `codex`
    /// is shown, never supervised, and codex-alab supervises itself, the
    /// window watching beside it. What has a `[harness.<agent>]` table is
    /// [`Program::harness_table`].
    #[inline]
    #[must_use]
    pub const fn supervisable(self) -> bool {
        match self {
            Program::Claude | Program::Agy | Program::Muse | Program::Gemini => true,
            Program::Codex | Program::Generic => false,
        }
    }

    /// Whether this program has a `[harness.<agent>]` table and a row on
    /// Settings ▸ Harness: every agent the supervisor acts on
    /// ([`Program::supervisable`]) and Codex, whose table codex-alab reads as
    /// its own policy (`harness policy`'s `agent-off`) while the window
    /// watches beside it. Never the generic reader.
    #[inline]
    #[must_use]
    pub const fn harness_table(self) -> bool {
        match self {
            Program::Claude | Program::Codex | Program::Agy | Program::Muse | Program::Gemini => {
                true
            }
            Program::Generic => false,
        }
    }

    /// This program's reader.
    #[inline]
    #[must_use]
    pub fn reader(self) -> &'static dyn ScreenReader {
        match self {
            Program::Claude => &ClaudeReader,
            Program::Codex => &CodexReader,
            Program::Agy => &AgyReader,
            Program::Muse => &MuseReader,
            Program::Gemini => &GeminiReader,
            Program::Generic => &GenericReader,
        }
    }
}

/// Everything one screen says, from one program's reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub program: Program,
    pub phase: Phase,
    /// Whether [`Self::phase`] is the reader's evidence rather than its
    /// default: `false` for the generic reader and the Codex reader always, for
    /// agy's and Muse Code's off their measured shapes ([`crate::agy::phase`],
    /// [`crate::muse::phase`]), and for
    /// Claude Code's `idle` with no composer frame drawn (its launch, before
    /// the REPL is up) — each says `idle` there because it cannot tell
    /// working from waiting, not because it saw either — and, read with the
    /// cursor ([`ScreenReader::read_at`]), for Claude Code's `idle` at a
    /// prompt box that does not hold the cursor (an earlier run's). A policy
    /// that acts on `idle` (a continuation, a first prompt) or on `prompt`
    /// requires it.
    pub phase_authoritative: bool,
    /// The wall the last turn ended on: `None` under a box, and while a turn runs only a
    /// health wall ([`ScreenReader::health_wall`]). Background work the agent left running
    /// is no running turn, so a wall behind it is read. A turn that ended on `API Error:
    /// 529` reads [`Phase::Idle`] with a wall here.
    pub wall: Option<Wall>,
    /// The box, when [`Self::phase`] is [`Phase::Prompt`].
    pub prompt: Option<PromptV2>,
    pub survey: bool,
    /// The context left as the screen shows it ([`ScreenReader::context_shown`]).
    pub context_left: Option<u8>,
    /// The program's own suggestion for the next message (needs the
    /// cursor's column; `None` without it).
    pub suggestion: Option<String>,
    pub goal_active: bool,
    /// The worker's last words ([`crate::turn::said_tail`]).
    pub said_tail: Option<String>,
    /// A PERSON stopped the last turn (Esc): the next message is theirs
    /// ([`crate::turn::interrupted`]).
    pub interrupted: bool,
    /// No turn yet ([`ScreenReader::fresh`]: [`crate::turn::fresh`]): an idle point here ended nothing — a
    /// session nobody has asked anything is not a worker stopped short.
    pub fresh: bool,
    /// The composer as `(caret row, lines)`, the caret row's text first —
    /// typed text or the dim placeholder, which only the cursor tells apart
    /// (at column 2 of the caret row: the placeholder, for Claude Code,
    /// and Muse Code's prompt hint, by its colour; agy draws none but
    /// its permission mode's hint, which its reader reads as no text, and its
    /// empty composer's text is empty: [`ScreenReader::placeholder`]; Gemini
    /// CLI's reader takes its placeholder out by its text, so its empty
    /// composer's is empty too); `None` with no composer on the screen (a
    /// box, a shell).
    pub composer: Option<(usize, Vec<String>)>,
    /// Usage credits carry the session, by the program's own notice
    /// ([`ScreenReader::credits`]). It is no wall; the supervisor's credits
    /// hold reads it ([`crate::credits`]).
    pub credits: Option<crate::credits::Credits>,
    /// The program's own warning that a usage window is filling
    /// ([`ScreenReader::usage_warning`]), for the record.
    pub warning: Option<crate::credits::Warning>,
}

/// A program's screen grammar. [`Self::program`] and [`Self::phase`] are
/// every reader's own; every other read defaults to what the generic reader
/// answers — no box, no wall, no survey, no context, no suggestion, no goal,
/// no last words, no interrupt, no housekeeping, never fresh, no composer, a
/// transcript down to the last drawn row, no caret, no paste guard, no busy
/// row, no credits, no usage warning, and a phase that is never evidence — so
/// a reader overrides only what its program draws.
pub trait ScreenReader: Sync {
    fn program(&self) -> Program;
    fn phase(&self, rows: &[String]) -> Phase;
    fn prompt(&self, _rows: &[String]) -> Option<PromptV2> {
        None
    }
    /// WHAT ESC DOES on `prompt`, a box [`Self::prompt`] read: the Esc its
    /// footer or options name ([`PromptV2::cancel`]), else what this
    /// program's Esc is known to do on a box that names none; `None` where
    /// nothing says (`aterm drive phase` then prints no Esc line).
    fn esc(&self, prompt: &PromptV2) -> Option<CancelEffect> {
        prompt.cancel.as_ref().map(|c| c.effect)
    }
    fn wall(&self, _rows: &[String]) -> Option<Wall> {
        None
    }
    /// A wall that says the program, not the turn, is stuck, read even while a turn runs
    /// (where [`Self::wall`] is not asked: a retry notice under a live spinner is no wall
    /// yet, but a process past saving is one). `None` by default.
    fn health_wall(&self, _rows: &[String]) -> Option<Wall> {
        None
    }
    /// Whether a rating card is open: [`Self::survey_block`] reads one.
    fn survey(&self, rows: &[String]) -> bool {
        self.survey_block(rows).is_some()
    }
    /// The open rating card ([`crate::survey::survey_block`]): its kind, rows and the
    /// `key if=` guard for its question row. `None` for a program that draws none.
    fn survey_block(&self, _rows: &[String]) -> Option<crate::survey::SurveyBlock> {
        None
    }
    /// THE AGENT'S OWN BACKGROUND WORK, AND NOTHING ELSE, KEEPS THIS SCREEN
    /// BUSY: its turn is over, the composer drawn, and what runs on is work
    /// it started (Claude Code's dynamic workflow, background agent or
    /// shell; agy's task, timer or subagent in its panel; Muse Code's
    /// backgrounded shell or monitor in its panel) — `Some(what runs)`.
    /// `None` for a live turn, an idle screen, a box, or a program with no
    /// such read (the default). The reader reads such a screen
    /// [`Phase::Busy`] — but where a limit or a wall the turn ended on
    /// stands over the work, or a question stands over work of the soft
    /// kind (a monitor: [`crate::phase::Busy::soft`]), as each reader says —
    /// so the supervisor's wait holds through it, the wall it remembers
    /// stands through it, and the host is offered its break.
    fn background_wait(&self, _rows: &[String]) -> Option<&'static str> {
        None
    }
    /// THE SESSION RESTS ON A TURN'S END THE SCREEN HIDES: the phase reads
    /// idle as evidence, nothing of the last turn's end is in sight, and
    /// nothing in sight says a turn went on past it or a person stopped it
    /// (Muse Code's cards alone over its open composer, the newest naming a
    /// failed run or none: [`crate::muse::end_hidden`]). `false` for every
    /// other screen, and for a program with no such read (the default). The
    /// wall the supervisor remembers stands over it, as over the agent's own
    /// background work ([`Self::background_wait`]): a person's look at the
    /// session is no end of the wall.
    fn end_hidden(&self, _rows: &[String]) -> bool {
        false
    }
    /// The context left before the agent auto-compacts, in percent, as its supervisor's watch
    /// reads it (`--context-warn`): `None` where this program draws no indicator.
    fn context(&self, _rows: &[String]) -> Option<u8> {
        None
    }
    /// The context left as the screen SHOWS it, for the reading's displays
    /// ([`Reading::context_left`]: the tab's hover, `ctl survey`, the session log):
    /// [`Self::context`] unless the program draws its indicator where that watch does not look
    /// (Claude Code 2.1.305's mode row, [`crate::phase::context_left_shown`]), so every display
    /// reads what aterm's footer draws.
    fn context_shown(&self, rows: &[String]) -> Option<u8> {
        self.context(rows)
    }
    fn suggestion(&self, _rows: &[String], _cursor_col: usize) -> Option<String> {
        None
    }
    fn goal_active(&self, _rows: &[String]) -> bool {
        false
    }
    fn said_tail(&self, _rows: &[String]) -> Option<String> {
        None
    }
    /// WHETHER THE MESSAGE [`Self::said_tail`] READS IS WHOLE ON `rows`: what
    /// bounds it above — its head, the message it answers — is one of them.
    /// `false` for a message taller than the rows, its tail then the rows in
    /// sight from the top (a need or an open item above them unread). Asked
    /// only where [`Self::said_tail`] reads words; `true` by default, for a
    /// program whose reader reads none.
    fn said_whole(&self, _rows: &[String]) -> bool {
        true
    }
    /// A row a read of the HISTORY ABOVE THE SCREEN stops at, for a message
    /// [`Self::said_whole`] finds cut: the nearest row that opens in column
    /// 0 with words — a message's head, a person's message, a tool's row —
    /// bounds the message under it. agy's caret row does not
    /// ([`crate::agy::said_history_stop`]).
    fn said_history_stop(&self, row: &str) -> bool {
        !row.is_empty() && !row.starts_with(char::is_whitespace)
    }
    /// THE AGENT'S OTHER WORDS IN THE TURN ITS LAST WORDS END
    /// ([`Self::said_tail`]): every message of the same turn above its last,
    /// the words it said before a tool call, say, oldest first and joined by
    /// a blank line, each read as [`Self::said_tail`] reads one. Its own rows
    /// and its tools' rows are passed over. `None` where the turn holds no
    /// other message on `rows`, where there are no last words, and by
    /// default, for a program whose reader reads none.
    fn turn_said_before(&self, _rows: &[String]) -> Option<String> {
        None
    }
    /// WHETHER THE TURN [`Self::said_tail`]'s MESSAGE ENDS IS WHOLE ON
    /// `rows`: the message is ([`Self::said_whole`]), and so is the row that
    /// opened the turn (the person's message, its band or its rule). `false`
    /// where the turn reaches above the rows: an earlier message of the turn,
    /// and a need said in it, may stand there. Asked only where
    /// [`Self::said_tail`] reads words; [`Self::said_whole`] by default, for
    /// a program whose reader reads no turn's other words
    /// ([`Self::turn_said_before`]): its turn is as whole as its message.
    fn turn_whole(&self, rows: &[String]) -> bool {
        self.said_whole(rows)
    }
    /// A row a read of the HISTORY ABOVE THE SCREEN stops at, for a turn
    /// [`Self::turn_whole`] finds cut: the row that opened the turn, read
    /// walking up. [`Self::said_history_stop`] by default.
    fn turn_history_stop(&self, row: &str) -> bool {
        self.said_history_stop(row)
    }
    /// THE MESSAGE THAT OPENED THE TURN [`Self::said_tail`]'s MESSAGE ENDS,
    /// as drawn on `rows`: the person's message over it (or a continuation
    /// typed for them), its marker stripped and the rows it wrapped onto
    /// joined by a space. What tells whether an act typed into the session
    /// opened the turn whose reply stands on the screen. `None` where that
    /// message is not on `rows`, where there are no last words, and by
    /// default, for a program whose reader reads none (Claude Code's
    /// answered prompt is read by its own grammar).
    fn turn_opened_by(&self, _rows: &[String]) -> Option<String> {
        None
    }
    /// WHETHER THIS PROGRAM'S SCREEN BOUNDS A MESSAGE BY ITS ROWS: a reply's
    /// rows are marked apart from a message's, so no row a message draws —
    /// past a blank line of it, or under its first row where that row is
    /// above the rows read — is read as the agent's reply. `true` by
    /// default: Claude Code, Gemini CLI and Muse Code take the reply from
    /// its head's mark (`⏺`, `✦`, `◆`; held to each reader by
    /// `tests::only_agy_reads_a_messages_rows_as_its_reply`), and Codex (no
    /// worker, display only) and a generic program keep the default.
    /// agy's is `false` ([`crate::agy`]'s A MESSAGE'S BLANK LINE): its
    /// message's rows past the first sit two columns in, as a reply's. So a
    /// worker's first prompt, which aterm writes to a file the agent reads,
    /// drops its blank lines for it
    /// (`aterm_agent::harness::worker::WorkerCli::blank_lines`), and a
    /// loop's first point reads a reply cut at the rows' top up to its
    /// message (`aterm_agent::supervise::policy::turn_end::TurnEndState::
    /// reads_turn_whole`).
    fn bounds_messages(&self) -> bool {
        true
    }
    /// THE ROWS WHOSE CELLS BOUND THE LAST MESSAGE where its text does not
    /// ([`Self::bounds_messages`] `false`): `(its caret row, the blank rows
    /// under it to read, top down)` — a row drawn in the caret row's style
    /// from column 0 is the message's own, and the first that is not ends
    /// it ([`agy::message_blanks`], measured). `None` by default: the text
    /// bounds every message.
    fn message_blanks(&self, _rows: &[String]) -> Option<(usize, Vec<usize>)> {
        None
    }
    /// `rows` with the blank rows `inner` that a read of the cells found to
    /// be the message's own ([`Self::message_blanks`]) taken out of the
    /// message whose caret row is `caret`, the rows under it keeping their
    /// index ([`agy::fold_message`]). `rows` as they are by default.
    fn fold_message(&self, rows: &[String], _caret: usize, _inner: &[usize]) -> Vec<String> {
        rows.to_vec()
    }
    /// A MESSAGE STANDS ON `rows`: a person's message (or one typed for
    /// them) in the transcript, answered or not, with words under it or
    /// none. What tells a screen holding nothing of a conversation — a
    /// launch's frame before its banner, a blank first screen, a launch card
    /// — from one whose last turn ended with no words under its message (a
    /// tool's box or row its last item), where [`Self::turn_opened_by`] and
    /// [`Self::said_tail`] read none. By default, any row drawn: a reader
    /// that reads no message cannot say that none stands on a screen with
    /// something on it, and a blank screen draws nothing (Muse Code's first
    /// screen, read before the loop knows the program: [`identify`] names
    /// no reader by a blank screen).
    fn message_drawn(&self, rows: &[String]) -> bool {
        rows.iter().any(|r| !r.trim().is_empty())
    }
    fn interrupted(&self, _rows: &[String]) -> bool {
        false
    }
    /// The program's OWN HOUSEKEEPING ended the last turn: nobody's message
    /// was answered and nobody worked ([`crate::turn::idle_compacted`]).
    fn housekeeping(&self, _rows: &[String]) -> bool {
        false
    }
    /// No turn yet: the program's launch card on the screen and no message under it.
    fn fresh(&self, _rows: &[String]) -> bool {
        false
    }
    fn composer(&self, _rows: &[String]) -> Option<(usize, Vec<String>)> {
        None
    }
    /// Where the TRANSCRIPT ends on `rows`, by position: the first row of the
    /// program's live zone — its box, its running turn's status row, its
    /// composer and footer — which a read of what the worker said (the
    /// since-removed `aterm drive report`) cuts. One past the last non-blank row by default: a
    /// program whose live zone no reader knows has nothing cut.
    fn transcript_end(&self, rows: &[String]) -> usize {
        rows.iter()
            .rposition(|r| !crate::text::trim(r).is_empty())
            .map_or(0, |i| i + 1)
    }
    /// Usage credits carry the session, by the program's own notice. Only a
    /// reader that knows its program's notice has one
    /// ([`crate::credits::in_use`]).
    fn credits(&self, _rows: &[String]) -> Option<crate::credits::Credits> {
        None
    }
    /// The program's own warning that a usage window is filling
    /// ([`crate::credits::usage_warning`]).
    fn usage_warning(&self, _rows: &[String]) -> Option<crate::credits::Warning> {
        None
    }
    /// The glyph the composer's caret row starts with (`❯` Claude Code and
    /// Muse Code, `>` agy and Gemini CLI): what a guard on typed
    /// text anchors to. `None`: no composer.
    fn caret(&self) -> Option<char> {
        None
    }
    /// The column [`Self::caret`] is drawn in: `0` — the caret row starts
    /// with it — for every program but Gemini CLI, whose composer pads it one
    /// column in (` > text`). A guard on typed text anchors that many blanks
    /// before the caret.
    fn caret_column(&self) -> usize {
        0
    }
    /// The glyph the composer's caret row starts with ON `rows` — what a guard
    /// on text typed into THAT screen anchors to. [`Self::caret`] by default:
    /// a reader whose program draws its input line with more than one mark
    /// reads the one drawn, so a guard spelled with the other is never
    /// pressed under.
    fn caret_on(&self, _rows: &[String]) -> Option<char> {
        self.caret()
    }
    /// Whether the composer, EMPTY, still shows text on its caret row — a
    /// dim placeholder (Claude Code's suggestion or stock hint, Muse Code's prompt hint `/export saves the
    /// conversation to a text file`) that only the cursor's column and the
    /// cell there tell from a person's draft. `false` by default: every word
    /// a composer without one holds was typed (agy draws none but its
    /// permission mode's hint, which [`AgyReader`] reads as no text), so none
    /// is ever taken for the placeholder and typed over. Gemini CLI draws one
    /// where neither the cursor's column nor a dim cell tells it
    /// ([`crate::gemini`]): its reader takes it out of the composer itself,
    /// and says `false` here.
    fn placeholder(&self) -> bool {
        false
    }
    /// The colour a [`Self::placeholder`] is drawn in where it carries no
    /// `dim` attribute (`rrggbb`, as `cell` prints it): Muse Code draws its
    /// prompt hint in its dim grey, `676c74`, a typed draft in `ccd3db`
    /// (measured on 1.4.10, its default theme). `None`: the `dim` attribute
    /// alone marks it.
    fn placeholder_fg(&self) -> Option<&'static str> {
        None
    }
    /// An Enter that arrives within a burst of typed text is taken as a
    /// NEWLINE, not a submit (a paste guard, measured): typed text is
    /// submitted by a `turn` that settles first, never by an Enter right
    /// behind the text.
    fn paste_guard(&self) -> bool {
        false
    }
    /// What the composer draws in place of `text` written into it as ONE
    /// paste (a plain `turn`), where it folds a long paste into a chip —
    /// Gemini CLI's `[Pasted Text: <n> chars]` ([`gemini::folded_paste`]):
    /// the row the cursor ends on, which a guard on the typed text anchors to.
    /// `None` by default: the composer draws the text itself.
    fn folded_paste(&self, _text: &str) -> Option<String> {
        None
    }
    /// A guard (the wire's regex: spaces as `.`) matching only the row a running turn
    /// shows: its leaving ends the turn (`await gone`). `None`: nothing says a turn runs.
    fn busy_guard(&self) -> Option<String> {
        None
    }
    /// Whether the row [`Self::busy_guard`] matches STAYS while an approval
    /// box is up, so a wait on it leaving does not end when a box is drawn.
    /// `false` by default: Claude Code's busy footer leaves for a box
    /// (measured); Muse Code's status row does not.
    fn busy_row_outlives_boxes(&self) -> bool {
        false
    }
    /// The first row a running turn's busy row can be on, the top of the live zone: a wait
    /// on [`Self::busy_guard`] is armed from here down, never over a busy row a dead
    /// process left above ([`crate::phase::busy_zone_start`]). `0` (the whole screen) by
    /// default.
    fn busy_zone_start(&self, _rows: &[String]) -> usize {
        0
    }
    /// Whether `phase`, read from `rows`, is evidence ([`Reading::phase_authoritative`]).
    /// `false` by default.
    fn phase_authoritative(&self, _rows: &[String], _phase: &Phase) -> bool {
        false
    }
    /// Whether the terminal's cursor, on row `cursor_row` of `rows` (`None`: on none),
    /// vouches for `reading`. `true` by default: the cursor says nothing of the phase.
    fn cursor_vouches(
        &self,
        _rows: &[String],
        _reading: &Reading,
        _cursor_row: Option<usize>,
    ) -> bool {
        true
    }

    /// The whole reading of a screen whose cursor is known: [`Self::read`], its phase
    /// evidence only where [`Self::cursor_vouches`] too. `cursor_row` is the cursor's row
    /// among `rows` (`None`: on none of them), `cursor_col` its column. Whatever acts on an
    /// `idle` reading of a live screen (`await agent idle`, the supervisor's idle point)
    /// reads it here.
    fn read_at(
        &self,
        rows: &[String],
        cursor_row: Option<usize>,
        cursor_col: Option<usize>,
    ) -> Reading {
        let mut reading = self.read(rows, cursor_col);
        reading.phase_authoritative &= self.cursor_vouches(rows, &reading, cursor_row);
        reading
    }

    /// The whole reading.
    fn read(&self, rows: &[String], cursor_col: Option<usize>) -> Reading {
        let phase = self.phase(rows);
        let phase_authoritative = self.phase_authoritative(rows, &phase);
        let prompt = (phase == Phase::Prompt)
            .then(|| self.prompt(rows))
            .flatten();
        let hard_busy = phase == Phase::Busy && busy_signal(rows).is_some_and(|b| !b.soft);
        // Never under a box (its answer is the human's); under a hard busy only a health
        // wall; otherwise every wall.
        let wall = if phase == Phase::Prompt {
            None
        } else if hard_busy {
            self.health_wall(rows)
        } else {
            self.wall(rows)
        };
        Reading {
            program: self.program(),
            phase,
            phase_authoritative,
            wall,
            prompt,
            survey: self.survey(rows),
            context_left: self.context_shown(rows),
            suggestion: cursor_col.and_then(|c| self.suggestion(rows, c)),
            goal_active: self.goal_active(rows),
            said_tail: self.said_tail(rows),
            interrupted: self.interrupted(rows),
            fresh: self.fresh(rows),
            composer: self.composer(rows),
            credits: self.credits(rows),
            warning: self.usage_warning(rows),
        }
    }
}

/// Claude Code: this crate's grammar.
#[derive(Debug, Clone, Copy)]
pub struct ClaudeReader;

impl ScreenReader for ClaudeReader {
    fn program(&self) -> Program {
        Program::Claude
    }
    fn phase(&self, rows: &[String]) -> Phase {
        worker_phase(rows)
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        parse_prompt_v2(rows)
    }
    /// Esc leaves every box Claude Code draws: a footed one names what it
    /// does in its `Esc to …` footer (the trust dialog's quits the program,
    /// [`CancelEffect::Exit`]), a footerless one marks its refusal ` (esc)`
    /// or answers Esc with its select's cancel.
    fn esc(&self, prompt: &PromptV2) -> Option<CancelEffect> {
        Some(
            prompt
                .cancel
                .as_ref()
                .map_or(CancelEffect::Back, |c| c.effect),
        )
    }
    fn wall(&self, rows: &[String]) -> Option<Wall> {
        wall(rows)
    }
    fn health_wall(&self, rows: &[String]) -> Option<Wall> {
        memory_wall(rows)
    }
    fn survey_block(&self, rows: &[String]) -> Option<crate::survey::SurveyBlock> {
        crate::survey::survey_block(rows)
    }
    fn background_wait(&self, rows: &[String]) -> Option<&'static str> {
        crate::phase::background_wait(rows)
    }
    fn context(&self, rows: &[String]) -> Option<u8> {
        context_left(rows)
    }
    fn context_shown(&self, rows: &[String]) -> Option<u8> {
        context_left_shown(rows)
    }
    fn suggestion(&self, rows: &[String], cursor_col: usize) -> Option<String> {
        continuation_suggestion(rows, cursor_col)
    }
    fn goal_active(&self, rows: &[String]) -> bool {
        goal_active(rows)
    }
    fn said_tail(&self, rows: &[String]) -> Option<String> {
        said_tail(rows)
    }
    fn said_whole(&self, rows: &[String]) -> bool {
        crate::turn::said_whole(rows)
    }
    fn turn_said_before(&self, rows: &[String]) -> Option<String> {
        crate::turn::turn_said_before(rows)
    }
    fn turn_whole(&self, rows: &[String]) -> bool {
        crate::turn::turn_whole(rows)
    }
    fn turn_history_stop(&self, row: &str) -> bool {
        crate::turn::turn_history_stop(row)
    }
    fn interrupted(&self, rows: &[String]) -> bool {
        interrupted(rows)
    }
    fn housekeeping(&self, rows: &[String]) -> bool {
        crate::turn::idle_compacted(rows)
    }
    fn fresh(&self, rows: &[String]) -> bool {
        crate::turn::fresh(rows)
    }
    fn message_drawn(&self, rows: &[String]) -> bool {
        crate::turn::message_drawn(rows)
    }
    /// Inside its frame only: a `❯` row with no frame around it is a user's message, and
    /// a box replaces the frame.
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        has_composer_frame(rows)
            .then(|| composer_draft(rows))
            .flatten()
    }
    fn transcript_end(&self, rows: &[String]) -> usize {
        crate::phase::transcript_end(rows)
    }
    fn credits(&self, rows: &[String]) -> Option<crate::credits::Credits> {
        crate::credits::in_use(rows)
    }
    fn usage_warning(&self, rows: &[String]) -> Option<crate::credits::Warning> {
        crate::credits::usage_warning(rows)
    }
    fn caret(&self) -> Option<char> {
        Some('❯')
    }
    fn placeholder(&self) -> bool {
        true
    }
    fn paste_guard(&self) -> bool {
        false
    }
    /// The busy footer's `esc to interrupt`, under the spinner.
    fn busy_guard(&self) -> Option<String> {
        Some(guard_regex(
            const { crate::anchors::anchor_text("busy.interrupt") },
        ))
    }
    fn busy_zone_start(&self, rows: &[String]) -> usize {
        crate::phase::busy_zone_start(rows)
    }
    /// Every reading but one: Claude Code's `idle` ("no box, no spinner, no question") is
    /// evidence only at its composer (the frame drawn, or shell mode's `!`) or on a wall,
    /// since a screen with no REPL on it says the same: the shell's rows between launch and
    /// REPL, and the alternate screen's first half-drawn frame. Measured on 2.1.283 (the
    /// `LAUNCH_*` fixtures): `idle` was published from those frames 0.1-2 s before the REPL
    /// was drawn, and text typed then was lost.
    fn phase_authoritative(&self, rows: &[String], phase: &Phase) -> bool {
        *phase != Phase::Idle
            || has_composer_frame(rows)
            || has_shell_mode_frame(rows)
            || wall(rows).is_some()
    }
    /// Claude Code's `idle` at a prompt box is evidence only where that box holds the
    /// cursor ([`crate::phase::prompt_box_holds`]); a wall and every other phase stand as
    /// read. An inline relaunch in the same tab leaves the previous run's box (footer `Press
    /// Ctrl-C again to exit`) whole above the new launch line until the new REPL is drawn,
    /// with the cursor under that line. Measured on 2.1.283 at 150x50 (the
    /// `INLINE_RELAUNCH_*` fixtures): `agent=idle` from such a frame 187-352 ms before the
    /// REPL, and a draft typed on it lost.
    fn cursor_vouches(
        &self,
        rows: &[String],
        reading: &Reading,
        cursor_row: Option<usize>,
    ) -> bool {
        reading.phase != Phase::Idle
            || reading.wall.is_some()
            || cursor_row.is_some_and(|row| prompt_box_holds(rows, row))
    }
}

/// Codex: identity only. aterm reads no Codex screen (its Codex screen lane
/// was retired on 2026-10-16, the owner: "retire the codex harness"): no
/// box, wall, composer, busy row or paste guard, every trait method but these
/// two at its default. Its own state is its OSC 7501 report, which the window
/// shows (`status program_state=`, the needs border's hover) and nothing decides on.
#[derive(Debug, Clone, Copy)]
pub struct CodexReader;

impl ScreenReader for CodexReader {
    fn program(&self) -> Program {
        Program::Codex
    }
    /// No evidence: every reading is `idle` NOT authoritatively (the trait's
    /// default [`ScreenReader::phase_authoritative`]), so the server
    /// publishes `agent=unknown`, and `aterm drive phase` and `answer` read
    /// nothing on a Codex.
    fn phase(&self, _rows: &[String]) -> Phase {
        Phase::Idle
    }
}

/// agy (Google's Antigravity CLI): [`crate::agy`]'s grammar (module header).
#[derive(Debug, Clone, Copy)]
pub struct AgyReader;

impl ScreenReader for AgyReader {
    fn program(&self) -> Program {
        Program::Agy
    }
    fn phase(&self, rows: &[String]) -> Phase {
        agy::phase(rows).0
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        agy::prompt(rows)
    }
    fn esc(&self, prompt: &PromptV2) -> Option<CancelEffect> {
        agy::esc(prompt)
    }
    fn wall(&self, rows: &[String]) -> Option<Wall> {
        agy::wall(rows)
    }
    /// agy's feedback survey, drawn in its composer's place
    /// ([`crate::agy::survey`]).
    fn survey_block(&self, rows: &[String]) -> Option<crate::survey::SurveyBlock> {
        agy::survey(rows)
    }
    fn said_tail(&self, rows: &[String]) -> Option<String> {
        agy::said_tail(rows)
    }
    fn said_whole(&self, rows: &[String]) -> bool {
        agy::said_whole(rows)
    }
    fn said_history_stop(&self, row: &str) -> bool {
        agy::said_history_stop(row)
    }
    fn turn_said_before(&self, rows: &[String]) -> Option<String> {
        agy::turn_said_before(rows)
    }
    fn turn_whole(&self, rows: &[String]) -> bool {
        agy::turn_whole(rows)
    }
    fn turn_history_stop(&self, row: &str) -> bool {
        agy::turn_history_stop(row)
    }
    fn turn_opened_by(&self, rows: &[String]) -> Option<String> {
        agy::turn_opened_by(rows)
    }
    /// agy draws a message's blank line as a blank row, the rows under it
    /// two columns in as a reply's, and every row of a message past its
    /// first two columns in: nothing in its text tells them from a reply's
    /// ([`crate::agy`]'s A MESSAGE'S BLANK LINE); its cells do.
    fn bounds_messages(&self) -> bool {
        false
    }
    /// Its cells do (measured): [`agy::message_blanks`].
    fn message_blanks(&self, rows: &[String]) -> Option<(usize, Vec<usize>)> {
        agy::message_blanks(rows)
    }
    fn fold_message(&self, rows: &[String], caret: usize, inner: &[usize]) -> Vec<String> {
        agy::fold_message(rows, caret, inner)
    }
    fn interrupted(&self, rows: &[String]) -> bool {
        agy::interrupted(rows)
    }
    /// A task, a timer or a subagent running in the panel under the
    /// composer, the turn over it ended ([`agy::background_wait`]).
    fn background_wait(&self, rows: &[String]) -> Option<&'static str> {
        agy::background_wait(rows)
    }
    fn fresh(&self, rows: &[String]) -> bool {
        agy::fresh(rows)
    }
    fn message_drawn(&self, rows: &[String]) -> bool {
        agy::message_drawn(rows)
    }
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        agy::composer_draft(rows)
    }
    fn caret(&self) -> Option<char> {
        Some('>')
    }
    /// None: agy's empty composer draws nothing after its caret (measured)
    /// but, while a permission mode is on, the mode's hint — whole or cut
    /// with `...` — which [`agy::composer_draft`] reads as the empty draft
    /// itself, so no text it reads is ever typed over.
    fn placeholder(&self) -> bool {
        false
    }
    /// An Enter sent in the same burst as the text submits it (measured on
    /// 1.4.0): no paste guard.
    fn paste_guard(&self) -> bool {
        false
    }
    /// The spinner row over the composer: the footer's `esc to cancel` is
    /// drawn under a menu and a box too, and must not read as a turn running.
    fn busy_guard(&self) -> Option<String> {
        Some(agy::busy_guard())
    }
    fn busy_zone_start(&self, rows: &[String]) -> usize {
        agy::busy_zone_start(rows)
    }
    fn phase_authoritative(&self, rows: &[String], _: &Phase) -> bool {
        agy::phase(rows).1
    }
}

/// Muse Code: [`crate::muse`]'s grammar (module header).
#[derive(Debug, Clone, Copy)]
pub struct MuseReader;

impl ScreenReader for MuseReader {
    fn program(&self) -> Program {
        Program::Muse
    }
    fn phase(&self, rows: &[String]) -> Phase {
        muse::phase(rows).0
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        muse::prompt(rows)
    }
    fn wall(&self, rows: &[String]) -> Option<Wall> {
        muse::wall(rows)
    }
    fn said_tail(&self, rows: &[String]) -> Option<String> {
        muse::said_tail(rows)
    }
    fn said_whole(&self, rows: &[String]) -> bool {
        muse::said_whole(rows)
    }
    fn turn_said_before(&self, rows: &[String]) -> Option<String> {
        muse::turn_said_before(rows)
    }
    fn turn_whole(&self, rows: &[String]) -> bool {
        muse::turn_whole(rows)
    }
    fn turn_history_stop(&self, row: &str) -> bool {
        muse::turn_history_stop(row)
    }
    fn turn_opened_by(&self, rows: &[String]) -> Option<String> {
        muse::turn_opened_by(rows)
    }
    fn interrupted(&self, rows: &[String]) -> bool {
        muse::interrupted(rows)
    }
    /// A backgrounded shell or a monitor running in the panel under the
    /// composer, the turn over it ended ([`muse::background_wait`]).
    fn background_wait(&self, rows: &[String]) -> Option<&'static str> {
        muse::background_wait(rows)
    }
    /// Cards alone over the composer whose newest `/status` card says the
    /// run `FAILED`, or a `/usage` card naming no state ([`muse::end_hidden`]).
    fn end_hidden(&self, rows: &[String]) -> bool {
        muse::end_hidden(rows)
    }
    fn fresh(&self, rows: &[String]) -> bool {
        muse::fresh(rows)
    }
    fn message_drawn(&self, rows: &[String]) -> bool {
        muse::message_drawn(rows)
    }
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        muse::composer_draft(rows)
    }
    fn caret(&self) -> Option<char> {
        Some('❯')
    }
    /// Its prompt hint (one contextual tip in the idle composer, measured on
    /// 1.4.10: `❯ /export saves the conversation to a text file`, the cursor
    /// at column 2, drawn in its dim grey with no `dim` attribute) is no
    /// draft: the cursor's column and the cell's colour tell it from one.
    fn placeholder(&self) -> bool {
        true
    }
    fn placeholder_fg(&self) -> Option<&'static str> {
        Some("676c74")
    }
    /// An Enter in the same burst as the text is taken as a newline (measured
    /// on 1.4.9-R6297.1 and 1.4.10-R6427.1, [`crate::muse`]).
    fn paste_guard(&self) -> bool {
        true
    }
    /// The status row's tail, `esc to interrupt)` (`◆ Thinking (2s · esc to
    /// interrupt)`).
    fn busy_guard(&self) -> Option<String> {
        Some(format!(
            "{}\\)",
            guard_regex(const { crate::anchors::anchor_text("muse.busy.interrupt") })
        ))
    }
    /// `◆ Calling tools (2s · esc to interrupt)` stays above an approval box's
    /// frame (measured on 1.4.10, [`crate::muse::fixtures::BOX_SHELL`]).
    fn busy_row_outlives_boxes(&self) -> bool {
        true
    }
    fn busy_zone_start(&self, rows: &[String]) -> usize {
        muse::busy_zone_start(rows)
    }
    fn phase_authoritative(&self, rows: &[String], _: &Phase) -> bool {
        muse::phase(rows).1
    }
}

/// Gemini CLI: [`crate::gemini`]'s grammar (module header).
#[derive(Debug, Clone, Copy)]
pub struct GeminiReader;

impl ScreenReader for GeminiReader {
    fn program(&self) -> Program {
        Program::Gemini
    }
    fn phase(&self, rows: &[String]) -> Phase {
        gemini::phase(rows).0
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        gemini::prompt(rows)
    }
    fn esc(&self, prompt: &PromptV2) -> Option<CancelEffect> {
        gemini::esc(prompt)
    }
    fn wall(&self, rows: &[String]) -> Option<Wall> {
        gemini::wall(rows)
    }
    fn said_tail(&self, rows: &[String]) -> Option<String> {
        gemini::said_tail(rows)
    }
    fn said_whole(&self, rows: &[String]) -> bool {
        gemini::said_whole(rows)
    }
    fn turn_said_before(&self, rows: &[String]) -> Option<String> {
        gemini::turn_said_before(rows)
    }
    fn turn_whole(&self, rows: &[String]) -> bool {
        gemini::turn_whole(rows)
    }
    fn turn_history_stop(&self, row: &str) -> bool {
        gemini::turn_history_stop(row)
    }
    fn turn_opened_by(&self, rows: &[String]) -> Option<String> {
        gemini::turn_opened_by(rows)
    }
    fn interrupted(&self, rows: &[String]) -> bool {
        gemini::interrupted(rows)
    }
    fn fresh(&self, rows: &[String]) -> bool {
        gemini::fresh(rows)
    }
    fn message_drawn(&self, rows: &[String]) -> bool {
        gemini::message_drawn(rows)
    }
    /// Its placeholder read as an empty draft ([`gemini::composer_draft`]).
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        gemini::composer_draft(rows)
    }
    fn caret(&self) -> Option<char> {
        Some('>')
    }
    /// `*` while YOLO is on; nothing in shell mode ([`gemini::caret_on`]).
    fn caret_on(&self, rows: &[String]) -> Option<char> {
        gemini::caret_on(rows)
    }
    fn caret_column(&self) -> usize {
        1
    }
    /// An Enter within 30 ms of a typed key is a newline (READ in the
    /// 0.46.0 bundle, [`crate::gemini`]).
    fn paste_guard(&self) -> bool {
        true
    }
    fn folded_paste(&self, text: &str) -> Option<String> {
        gemini::folded_paste(text)
    }
    /// The spinner on the status row: `esc to cancel` is drawn there only
    /// beside it, and the row's `? for shortcuts` stays while a turn runs.
    fn busy_guard(&self) -> Option<String> {
        Some(gemini::busy_guard())
    }
    fn phase_authoritative(&self, rows: &[String], _: &Phase) -> bool {
        gemini::phase(rows).1
    }
}

/// Any other program: never a prompt, a question or a wall; always `idle`, never evidence.
#[derive(Debug, Clone, Copy)]
pub struct GenericReader;

impl ScreenReader for GenericReader {
    fn program(&self) -> Program {
        Program::Generic
    }
    fn phase(&self, _: &[String]) -> Phase {
        Phase::Idle
    }
}

/// Shells, pagers, viewers and editors: a session whose foreground program is one of
/// these runs no agent, whatever its screen shows (a `cat` of a saved Claude screen).
const NOT_AGENTS: &[&str] = &[
    "zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "pwsh", "login", "less",
    "more", "most", "man", "vim", "nvim", "vi", "view", "emacs", "nano", "tail", "head", "cat",
    "bat", "watch",
];

/// The reader for a session. The PROGRAM NAME first — the basename of the
/// first word of `program` (a `status` `program=`/`detail=` value), a login
/// shell's leading `-` dropped: an agent's name ([`program_of`]: `claude`,
/// `codex`, `agy`, `muse`, `gemini`) → that program's reader ([`Program::reader`]), a
/// shell, a pager, a viewer or an editor ([`NOT_AGENTS`]) → Generic. Then,
/// for no name or any other one (a Claude Code started as `node`, `ssh` to a
/// host running one, a session adopted with no name), the screen, the most
/// specific test first: Muse Code's composer under its labelled rule, an
/// approval box in that frame, its trust box or its sign-in wall
/// (`muse::is_muse_screen`) → Muse — FIRST,
/// because its composer is Claude Code's frame to the rule (its caret is
/// `❯`), and Claude Code's test claims it; agy's composer over a footer it
/// draws, one of its boxes or its sign-in wall (`agy::is_agy_screen`) →
/// Agy; Gemini CLI's composer band under its rule or one of its named boxes
/// (`gemini::is_gemini_screen`; it runs as `node`, so its name is never
/// `gemini` on the wire) → Gemini; Claude Code's composer frame, one of its boxes (the trust dialog
/// replaces the frame) or its `/login` dialog → Claude; else Generic. Codex is named by its name
/// alone: aterm reads no Codex screen (its screen lane was retired on
/// 2026-10-16), so a Codex with no name — a vendor `codex` npm started as
/// `node` — reads Generic, and the window shows it by its own OSC 7501
/// report.
///
/// `program` must name the foreground process, the one drawing the screen: the root shell
/// would name every Claude Code started from it as a shell.
#[must_use]
pub fn identify(program: Option<&str>, rows: &[String]) -> &'static dyn ScreenReader {
    if let Some(named) = program.and_then(program_of) {
        return named.reader();
    }
    if program
        .map(program_word)
        .is_some_and(|name| NOT_AGENTS.iter().any(|not| name.eq_ignore_ascii_case(not)))
    {
        return &GenericReader;
    }
    // A footerless box of no kind aterm-phase names (a setup dialog) looks like any
    // program's menu (a shell script's `❯ 1. dev`), so it names Claude Code only by name.
    let names_claude = |p: PromptV2| p.kind != PromptKind::Other || p.footer.is_some();
    if muse::is_muse_screen(rows) {
        &MuseReader
    } else if agy::is_agy_screen(rows) {
        &AgyReader
    } else if gemini::is_gemini_screen(rows) {
        &GeminiReader
    } else if has_composer_frame(rows)
        || parse_prompt_v2(rows).is_some_and(names_claude)
        || names_login(rows)
    {
        &ClaudeReader
    } else {
        &GenericReader
    }
}

/// Claude Code's `/login` dialog in one of the states its own words name
/// ([`crate::login::login_screen`]: the method picker, Console's menu, the
/// browser hand-off, the success) — drawn in the composer's place, so no frame
/// names the program while it stands. A dialog titled `Login` this module
/// reads no state of ([`LoginScreen::Failed`]) is any program's.
fn names_login(rows: &[String]) -> bool {
    crate::login::login_screen(rows).is_some_and(|s| !matches!(s, LoginScreen::Failed))
}

/// [`identify`], then that reader's [`ScreenReader::read`].
#[must_use]
pub fn read(program: Option<&str>, rows: &[String], cursor_col: Option<usize>) -> Reading {
    identify(program, rows).read(rows, cursor_col)
}

/// [`read`] of a screen whose cursor is known ([`ScreenReader::read_at`]): row
/// `cursor_row` of `rows` (`None`: on none of them), column `cursor_col`. What acts on
/// an `idle` reading of a live screen reads it here.
#[must_use]
pub fn read_at(
    program: Option<&str>,
    rows: &[String],
    cursor_row: Option<usize>,
    cursor_col: Option<usize>,
) -> Reading {
    identify(program, rows).read_at(rows, cursor_row, cursor_col)
}

/// Where a screen's live zone begins: the first of its last `n` drawn rows, counted up
/// from its last row that is not blank. The blank rows under what is drawn stay in the
/// zone without counting against `n`, and a gap ([`BLANK_GAP`] blank rows or more, the
/// pane's empty middle) counts as one row however tall. A screen drawn to its last row
/// with no gap has exactly its last `n` rows as its zone. The server's agent verdict
/// reads this zone (aterm-gui's `presence::CLASSIFY_ROWS`), and so does a supervisor
/// whose tail read ends on a blank row or holds a gap ([`live_zone_reaches_above`]).
///
/// Drawn rows, because Claude Code's inline renderer draws its REPL under the launch
/// line and leaves the rows below blank: on a pane taller than `n`, the grid's last `n`
/// rows held the box's bottom rule and footer, never its caret. The gap, because the
/// fullscreen renderer pins the composer to the pane's foot: a short transcript in a
/// tall pane leaves the last thing said far above the last `n` rows.
#[must_use]
pub fn live_zone_start(rows: &[String], n: usize) -> usize {
    zone_walk(rows, n).0
}

/// Whether the live zone of a screen begins above `tail`, its last `n` rows: for a
/// supervisor that reads by `text tail=<n>` and must know when it holds less than the
/// zone. True when `tail` is drawn to its last row and holds a gap, so the walk of
/// [`live_zone_start`] reaches the tail's top with budget to spare. Measured on 2.1.284
/// at 144x50: a first turn ending on `⏺ API Error` on row 8, 35 blank rows, the composer
/// on rows 46-49; the last 40 rows alone read as an ordinary idle turn end.
///
/// A tail cannot see a gap its top row cuts (fewer than [`BLANK_GAP`] of the run's rows
/// inside): that takes `n - 3` or more drawn rows under it, a tall box at the pane's
/// foot, which is what the reader reads, so this returns `false` rather than make every
/// poll of such a pane read it twice. A tail that ends on a blank row is the supervisor's
/// `drawn_above_tail`'s to judge. A `tail` shorter than `n` is the whole screen.
#[must_use]
pub fn live_zone_reaches_above(tail: &[String], n: usize) -> bool {
    let drawn_to_the_last_row = tail
        .last()
        .is_some_and(|r| !crate::text::trim(r).is_empty());
    if tail.len() < n || !drawn_to_the_last_row {
        return false;
    }
    let (start, budget) = zone_walk(tail, n);
    start == 0 && budget > 0
}

/// [`live_zone_start`]'s walk: where it stops, and the budget it has left
/// (rows of `n` unspent when it ran out of rows).
fn zone_walk(rows: &[String], n: usize) -> (usize, usize) {
    let drawn = rows
        .iter()
        .rposition(|r| !crate::text::trim(r).is_empty())
        .map_or(0, |last| last + 1);
    let blank = |i: usize| crate::text::trim(&rows[i]).is_empty();
    let (mut start, mut budget) = (drawn, n);
    while start > 0 && budget > 0 {
        start -= 1;
        // A run of BLANK_GAP blank rows or more counts as one row against `n`; a shorter
        // run is a separator and counts row by row.
        if blank(start) {
            let mut top = start;
            while top > 0 && blank(top - 1) {
                top -= 1;
            }
            if start - top + 1 >= BLANK_GAP {
                start = top;
            }
        }
        budget -= 1;
    }
    (start, budget)
}

/// How many blank rows in a row make a gap rather than a separator ([`live_zone_start`]).
pub const BLANK_GAP: usize = 4;

/// THE ONE NAME TABLE: the agent a program name is, by its name alone —
/// the basename of its first word, a login shell's `-` dropped: `claude` and
/// `claude-code` are Claude Code, `codex` is Codex and so is `codex-old`
/// (OpenAI's own build beside codex-alab, as atpkg lays it: its entry cloned
/// under that name, so its argv0 says so), `agy` is agy, `muse` is
/// Muse Code and so is `muse-bin-<version>` (Meta's own installer's launcher
/// execs its build so named: `muse-bin-1.4.10-R6427.1`, measured), `gemini`
/// is Gemini CLI (a name its own process never carries: it runs as `node`),
/// anything else is no agent BY NAME (it may still be one by its screen:
/// [`identify`],
/// [`may_host_agent`]). The server's agent verdict and the in-GUI supervisor
/// host ask this, never a table of their own. Which programs atpkg installs
/// and shims is atpkg's roster (`atpkg::stub::AGENT_PROGRAMS`); the GUI's
/// program shim rule and `copy=` ask that.
#[must_use]
pub fn program_of(program: &str) -> Option<Program> {
    let word = program_word(program);
    let is = |name: &str| word.eq_ignore_ascii_case(name);
    if is("claude") || is("claude-code") {
        Some(Program::Claude)
    } else if is("codex") || is("codex-old") {
        Some(Program::Codex)
    } else if is("agy") {
        Some(Program::Agy)
    } else if is("muse") || is_muse_build(word) {
        Some(Program::Muse)
    } else if is("gemini") {
        Some(Program::Gemini)
    } else {
        None
    }
}

/// `muse-bin-1.4.10-R6427.1`: the build Meta's installer's launcher execs
/// (`muse-bin-` and a version that opens on a digit), case aside.
fn is_muse_build(word: &str) -> bool {
    word.get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("muse-bin-"))
        && word[9..].starts_with(|c: char| c.is_ascii_digit())
}

/// The script runtimes an agent runs AS without renaming itself — an
/// npm/bun install of Claude Code starts under `node` or `bun`: a program so
/// named may be identified as an agent by its SCREEN (Claude Code's frame, a
/// Codex, agy, Muse Code or Gemini CLI box or composer: [`identify`]); Gemini
/// CLI always runs so. A shell, a pager or `cat` showing a
/// captured screen may not.
pub const AGENT_RUNTIMES: &[&str] = &["node", "bun", "deno"];

/// Whether `program` is one of [`AGENT_RUNTIMES`].
#[must_use]
pub fn may_host_agent(program: &str) -> bool {
    let word = program_word(program);
    AGENT_RUNTIMES
        .iter()
        .any(|runtime| word.eq_ignore_ascii_case(runtime))
}

/// The program word of a `program=` / `detail=` value: the basename of its
/// first word, a login shell's `-` dropped — borrowed, and matched against
/// the tables without regard to case ([`program_of`], [`may_host_agent`],
/// [`NOT_AGENTS`]; every name in them is ASCII), never lowercased into a new
/// string: the GUI asks it of every pane's program on every frame
/// (`harness_host::agent_of`), so it allocates nothing.
fn program_word(program: &str) -> &str {
    let head = program.split_whitespace().next().unwrap_or(program);
    let base = head.rsplit('/').next().unwrap_or(head);
    base.trim_start_matches('-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::fixtures::{codex_0_155_trust, working as codex_working};
    use crate::prompt::PromptKind;
    use crate::prompt::fixtures::{
        API_ERROR_ENOTFOUND, API_RETRYING_DRAFT_MEASURED, API_RETRYING_MEASURED,
        API_RETRYING_QUEUED_80_MEASURED, API_RETRYING_QUEUED_DRAFT_MEASURED,
        API_RETRYING_QUEUED_MEASURED, BOX_BASH_TOUCH, END_529, GOAL_ACTIVE_SUGGESTION,
        LOGIN_HANDOFF, LOGIN_PICKER_WARNED, LOGIN_SUCCESS, MEMORY_BANNER_BUSY, MEMORY_BANNER_IDLE,
        TRUST, bash_one_row, composer, rows, screen,
    };
    use crate::wall::WallKind;

    /// A MESSAGE ON THE SCREEN IS DRAWN, answered or not, with words under
    /// it or none ([`ScreenReader::message_drawn`]; the review of
    /// `b9/forever-gaps`, RV-2: a turn ended on a tool's box read as no
    /// conversation where only the words were asked). Each reader: a launch
    /// (the frame before Gemini CLI's banner, a launch card, Muse Code's
    /// blank first screen and a session relaunched under its banner) holds
    /// none; a turn's end holds one, and so does a turn ended with no words
    /// (Gemini CLI's tool box last, Muse Code's `Ran` row last). NEGATIVE
    /// CONTROL: a reader that reads no message says one may stand.
    #[test]
    fn a_message_on_the_screen_is_drawn_and_a_launch_holds_none() {
        use crate::prompt::fixtures::{first_turn_under_a_table, launch_under_a_table};
        const END_OF_TURN_ANY: &str = "$ make\nok\n$ ";
        let drawn = |p: Program, rows: &[String]| p.reader().message_drawn(rows);
        {
            use crate::gemini::fixtures::{END_OF_TURN, IDLE, PRE_BANNER, YOLO_TOOL_RAN};
            assert!(!drawn(Program::Gemini, &screen(PRE_BANNER)));
            assert!(!drawn(Program::Gemini, &screen(IDLE)));
            assert!(drawn(Program::Gemini, &screen(END_OF_TURN)));
            let mut tool_last = screen(YOLO_TOOL_RAN);
            let at = tool_last
                .iter()
                .rposition(|r| r.starts_with("✦ Tool call done."))
                .expect("the reply");
            tool_last[at] = String::new();
            assert_eq!(crate::gemini::said_tail(&tool_last), None);
            assert!(drawn(Program::Gemini, &tool_last));
        }
        {
            use crate::agy::fixtures::{END_OF_TURN, IDLE};
            assert!(crate::agy::fresh(&screen(IDLE)));
            assert!(!drawn(Program::Agy, &screen(IDLE)));
            assert!(drawn(Program::Agy, &screen(END_OF_TURN)));
        }
        {
            use crate::muse::fixtures::{
                END_OF_TURN, END_OF_TURN_RAN_COMMAND, IDLE, IDLE_RELAUNCHED,
            };
            assert!(!drawn(Program::Muse, &vec![String::new(); 50]));
            assert!(!drawn(Program::Muse, &screen(IDLE)));
            assert!(!drawn(Program::Muse, &screen(IDLE_RELAUNCHED)));
            assert!(drawn(Program::Muse, &screen(END_OF_TURN)));
            assert!(drawn(Program::Muse, &screen(END_OF_TURN_RAN_COMMAND)));
        }
        let launch = launch_under_a_table("❯ Try \"refactor <filepath>\"", "  ? for shortcuts");
        assert!(!drawn(Program::Claude, &launch), "{launch:#?}");
        assert!(drawn(
            Program::Claude,
            &first_turn_under_a_table(&["❯ then add tests"])
        ));
        // NEGATIVE CONTROL: no reader of messages, which says one may stand
        // on any screen with a row drawn — and none on a blank one, as Muse
        // Code's first screen reads before the loop knows its program.
        assert!(drawn(Program::Generic, &screen(END_OF_TURN_ANY)));
        assert!(!drawn(Program::Generic, &vec![String::new(); 50]));
        assert!(!drawn(Program::Generic, &[]));
    }

    fn shell_asking() -> Vec<String> {
        rows(&[
            "% ./configure",
            "checking for gcc... gcc",
            "Overwrite the existing config?",
        ])
    }

    /// A program word is matched without regard to case, from its path and
    /// past a login shell's `-` — and borrowed, never rebuilt: the GUI asks
    /// it of every pane on every frame. `CODEX` is Codex, `/usr/local/bin/
    /// Claude --resume` Claude Code, `-ZSH` a shell, `Node` a runtime.
    ///
    /// NEGATIVE CONTROLS: a name that only starts like one (`codexx`,
    /// `claude-codes`) names nothing, and neither does a runtime's.
    #[test]
    fn program_words_match_without_case_from_a_path_and_a_login_dash() {
        for (program, want) in [
            ("codex", Some(Program::Codex)),
            ("CODEX", Some(Program::Codex)),
            ("/usr/local/bin/Claude --resume", Some(Program::Claude)),
            ("-Claude-Code", Some(Program::Claude)),
            ("Agy", Some(Program::Agy)),
            ("MUSE x", Some(Program::Muse)),
            // Meta's installer's launcher execs its build by its versioned
            // name (measured: muse 1.4.10's `program=muse-bin-1.4.10-R6427.1`);
            // a name that only begins so is no build of it.
            (
                "/Users//x/.local/bin/muse-bin-1.4.10-R6427.1",
                Some(Program::Muse),
            ),
            ("muse-bin-", None),
            ("muse-bin-latest", None),
            ("muse-binary", None),
            ("codexx", None),
            // atpkg's `codex-old` (OpenAI's own build beside codex-alab) runs
            // as that name; one that only begins or ends so is no Codex.
            ("codex-old", Some(Program::Codex)),
            (
                "/opt/pkg/store/codex-old/1000000000163000000/bin/codex-old resume",
                Some(Program::Codex),
            ),
            ("CODEX-OLD", Some(Program::Codex)),
            ("codex-older", None),
            ("xcodex-old", None),
            ("codex-", None),
            ("claude-codes", None),
            ("node", None),
            ("", None),
        ] {
            assert_eq!(program_of(program), want, "{program:?}");
        }
        assert!(may_host_agent("Node"));
        assert!(may_host_agent("/opt/homebrew/bin/BUN run x"));
        assert!(!may_host_agent("nodes"));
        assert!(!may_host_agent("codex"));
        let rows = composer("");
        assert_eq!(identify(Some("-ZSH"), &rows).program(), Program::Generic);
        assert_eq!(
            identify(Some("/bin/Less"), &rows).program(),
            Program::Generic
        );
        assert_eq!(identify(Some("node"), &rows).program(), Program::Claude);
    }

    /// [`Program::ALL`] is every variant by construction (`next_program` and
    /// the compile-time walk beside it); each name is its own and
    /// [`program_of`] reads it back.
    #[test]
    fn every_programs_name_reads_back() {
        for p in Program::ALL {
            let named = program_of(p.name());
            if p == Program::Generic {
                assert_eq!(named, None);
            } else {
                assert_eq!(named, Some(p), "{p:?}");
            }
        }
    }

    /// The program name decides first; with none (or one that names no
    /// agent), the screen does — Claude Code's frame or one of its boxes,
    /// else the generic reader. A shell's name wins over a Claude-looking
    /// screen it is merely printing. Codex is named by its name alone: its
    /// trust gate (codex 0.155.1's, measured) with no name reads Generic
    /// (aterm's Codex screen lane, retired on 2026-10-16, read it as Codex).
    #[test]
    fn identify_takes_the_program_name_then_the_screen() {
        let framed = {
            let mut r = rows(&["⏺ Done.", ""]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        let p = |name: Option<&str>, r: &[String]| identify(name, r).program();
        assert_eq!(p(Some("claude"), &shell_asking()), Program::Claude);
        assert_eq!(
            p(Some("/opt/bin/codex --full-auto"), &framed),
            Program::Codex
        );
        assert_eq!(p(Some("agy"), &framed), Program::Agy);
        assert_eq!(
            p(Some("/opt/homebrew/bin/gemini --resume x"), &framed),
            Program::Gemini
        );
        assert_eq!(
            p(Some("/usr/local/bin/muse resume x"), &framed),
            Program::Muse
        );
        assert_eq!(p(Some("-zsh"), &framed), Program::Generic);
        assert_eq!(p(Some("bash"), &bash_one_row()), Program::Generic);
        assert_eq!(p(None, &framed), Program::Claude);
        assert_eq!(p(Some("node"), &framed), Program::Claude);
        assert_eq!(p(None, &screen(TRUST)), Program::Claude, "a box, no frame");
        assert_eq!(p(None, &screen(BOX_BASH_TOUCH)), Program::Claude);
        // `/login`'s dialog stands in the frame's place: its own words name
        // it, a successor loop's first read before `status` included.
        for login in [LOGIN_HANDOFF, LOGIN_PICKER_WARNED, LOGIN_SUCCESS] {
            assert_eq!(p(None, &screen(login)), Program::Claude, "{login}");
        }
        // NEGATIVE CONTROL: a dialog titled `Login` with no state this crate
        // reads is any program's; a shell's name still wins.
        let unknown: Vec<String> = [
            "$ ./sign-in.sh",
            "",
            &"─".repeat(100),
            "  Login",
            "",
            "  Enter your user name:",
            "",
            "  Esc to cancel",
        ]
        .iter()
        .map(|r| (*r).to_string())
        .collect();
        assert_eq!(p(None, &unknown), Program::Generic);
        assert_eq!(p(Some("bash"), &screen(LOGIN_HANDOFF)), Program::Generic);
        assert_eq!(p(None, &codex_0_155_trust()), Program::Generic);
        assert_eq!(p(Some("codex"), &codex_0_155_trust()), Program::Codex);
        assert_eq!(p(None, &shell_asking()), Program::Generic);
        assert_eq!(p(Some("python3"), &shell_asking()), Program::Generic);
        // A pager or an editor showing a saved Claude screen is not Claude.
        for viewer in ["less", "/usr/bin/vim", "tail -f log", "watch"] {
            assert_eq!(
                p(Some(viewer), &bash_one_row()),
                Program::Generic,
                "{viewer}"
            );
        }
        assert_eq!(
            p(Some("ssh"), &bash_one_row()),
            Program::Claude,
            "the control"
        );
    }

    /// THE SNIFF'S ORDER, PROVED OVER EVERY CAPTURE: read with no program
    /// name, no capture of one agent is ever named another's — Claude Code's
    /// (its own named `claude-…`, and the unprefixed and hand-built ones),
    /// agy's, Muse Code's and Gemini CLI's each read as their own program or
    /// as none, and each is named at least once; Codex never is (the screen
    /// names no Codex).
    #[test]
    fn every_capture_is_identified_as_its_own_program_or_as_none() {
        let mut named = [0usize; 6];
        for (name, text) in crate::text::fixture_files() {
            let rows = screen(&text);
            let want = match name.split('-').next() {
                Some("agy") => Program::Agy,
                Some("muse") => Program::Muse,
                Some("gemini") => Program::Gemini,
                _ => Program::Claude,
            };
            let got = identify(None, &rows).program();
            assert!(
                got == want || got == Program::Generic,
                "{name}: read as {got:?}"
            );
            named[got as usize] += 1;
        }
        for agent in [
            Program::Claude,
            Program::Agy,
            Program::Muse,
            Program::Gemini,
        ] {
            assert!(
                named[agent as usize] > 0,
                "every agent named at least once: {named:?}"
            );
        }
        assert_eq!(named[Program::Codex as usize], 0, "{named:?}");
    }

    /// A reader's default is not evidence: the generic reader's `idle` and
    /// the Codex reader's every `idle` (it reads no Codex screen: a running
    /// turn's status row and its trust gate alike) are not authoritative;
    /// Claude Code's idle at its composer is (its idle with no composer:
    /// [`claude_codes_idle_is_read_at_its_composer`]).
    #[test]
    fn a_default_idle_is_not_authoritative() {
        let working = codex_working();
        for r in [working, codex_0_155_trust()] {
            let r = read(Some("codex"), &r, None);
            assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
        }
        let g = read(Some("zsh"), &shell_asking(), None);
        assert_eq!(g.phase, Phase::Idle);
        assert!(!g.phase_authoritative);
        let c = read(Some("claude"), &screen(END_529), None);
        assert_eq!(c.phase, Phase::Idle);
        assert!(c.phase_authoritative, "the control");
    }

    /// The generic reader never says prompt or question: a shell screen ending in `?`
    /// (which Claude Code's reader calls a question, the control) and a shell printing a
    /// Claude box.
    #[test]
    fn the_generic_reader_is_never_a_question_or_a_prompt() {
        let r = shell_asking();
        assert_eq!(ClaudeReader.phase(&r), Phase::Question, "the control");
        assert_eq!(GenericReader.phase(&r), Phase::Idle);
        let reading = read(Some("zsh"), &bash_one_row(), Some(2));
        assert_eq!(reading.program, Program::Generic);
        assert_eq!(reading.phase, Phase::Idle);
        assert_eq!(reading.prompt, None);
        assert_eq!(reading.wall, None);
        assert_eq!(
            ClaudeReader.phase(&bash_one_row()),
            Phase::Prompt,
            "the control"
        );
    }

    /// THE CODEX READER READS NOTHING (aterm's Codex screen lane, retired on
    /// 2026-10-16): on its trust gate and on a running turn it names no box,
    /// wall, composer, question or busy row, and has no caret or paste
    /// guard, so nothing acts on a Codex's screen and `aterm drive` waits on
    /// nothing there. NEGATIVE CONTROL: Claude Code's reader, the same
    /// machinery, reads its own trust dialog's box and its busy row.
    #[test]
    fn the_codex_reader_reads_nothing() {
        let working = codex_working();
        let codex = Program::Codex.reader();
        assert_eq!(codex.program(), Program::Codex);
        for r in [codex_0_155_trust(), working, shell_asking()] {
            let reading = read(Some("codex"), &r, None);
            assert_eq!(reading.phase, Phase::Idle);
            assert!(!reading.phase_authoritative);
            assert_eq!(
                (reading.prompt, reading.wall, reading.composer),
                (None, None, None)
            );
            assert!(!reading.interrupted && !reading.goal_active && !reading.fresh);
            assert_eq!(codex.prompt(&r), None);
            assert_eq!(codex.background_wait(&r), None);
        }
        assert_eq!(
            (codex.caret(), codex.busy_guard(), codex.paste_guard()),
            (None, None, false)
        );
        let claude = read(Some("claude"), &screen(TRUST), None);
        assert_eq!(claude.phase, Phase::Prompt, "the control");
        assert!(claude.prompt.is_some(), "the control");
        assert!(
            Program::Claude.reader().busy_guard().is_some(),
            "the control"
        );
    }

    /// What a supervisor TYPING into a program needs of its reader: the
    /// composer on the screen (Claude Code's only inside its frame — a `❯`
    /// row under a box is the transcript's) and the row whose leaving ends a
    /// turn. NEGATIVE CONTROLS: the generic reader has no caret, busy row or
    /// paste guard; `Program::reader` is each one.
    #[test]
    fn each_reader_says_how_to_type_into_its_program() {
        let claude = Program::Claude.reader();
        assert_eq!(claude.program(), Program::Claude);
        assert_eq!(Program::Generic.reader().program(), Program::Generic);
        for p in Program::ALL {
            assert_eq!(p.reader().program(), p);
        }
        let idle = {
            let mut r = rows(&["⏺ Done.", ""]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        assert!(claude.composer(&idle).is_some(), "the framed composer");
        assert_eq!(
            claude.composer(&screen(BOX_BASH_TOUCH)),
            None,
            "a box replaces the frame: no composer"
        );
        assert!(claude.busy_guard().is_some());
        let g = GenericReader;
        assert_eq!(
            (g.caret(), g.busy_guard(), g.paste_guard()),
            (None, None, false)
        );
    }

    /// One read, whole: the 529 end of turn is idle WITH its wall; the
    /// measured `/goal` screen is busy (a workflow runs), carries the armed
    /// goal and Claude Code's suggestion, and no wall.
    #[test]
    fn a_reading_carries_the_wall_and_the_turn_end_facts() {
        let r = read(None, &screen(END_529), Some(2));
        assert_eq!(r.program, Program::Claude);
        assert_eq!(r.phase, Phase::Idle);
        assert_eq!(r.wall.map(|w| w.kind), Some(WallKind::Overloaded));
        assert_eq!(
            r.said_tail.as_deref(),
            Some("Suites are running; I'll report the number when it lands.")
        );

        let g = read(Some("claude"), &screen(GOAL_ACTIVE_SUGGESTION), Some(2));
        assert_eq!(g.phase, Phase::Busy);
        assert!(g.goal_active);
        assert_eq!(g.suggestion.as_deref(), Some("keep going"));
        assert_eq!(g.wall, None);
        assert_eq!(
            read(Some("claude"), &screen(GOAL_ACTIVE_SUGGESTION), None).suggestion,
            None
        );

        // A box: the prompt is read, no wall is.
        let b = read(Some("claude"), &screen(TRUST), None);
        assert_eq!(b.phase, Phase::Prompt);
        assert_eq!(b.prompt.map(|p| p.kind), Some(PromptKind::Trust));
        assert_eq!(b.wall, None);

        // 2.1.283: the outage's `⏺ API Error` is the wall the turn ended on; while the
        // vendor still retries under it (spinner row, `esc to interrupt`), busy and no wall.
        let e = read(Some("claude"), &screen(API_ERROR_ENOTFOUND), Some(3));
        assert_eq!(e.phase, Phase::Idle);
        assert_eq!(
            e.wall.map(|w| (w.kind, w.placement)),
            Some((
                WallKind::ApiError {
                    code: None,
                    retryable: true,
                    cause: crate::wall::ApiCause::Unreachable,
                },
                crate::wall::Placement::ErrorRow,
            ))
        );
        let mut retrying = screen(API_ERROR_ENOTFOUND);
        let done = retrying
            .iter()
            .position(|r| r.starts_with("✻ Worked"))
            .expect("the done row");
        retrying[done] =
            "✻ Can't reach the API server · Retrying in 2m · attempt 7/10 (esc to interrupt)"
                .to_string();
        let last = retrying.len() - 1;
        retrying[last] =
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt".to_string();
        let busy = read(Some("claude"), &retrying, None);
        assert_eq!(busy.phase, Phase::Busy);
        assert_eq!(busy.wall, None);
        // The measured retry: the spinner row carries the whole formatted error
        // (`✻ Can't reach the API server — check your internet or DNS
        // (ENOTFOUND) · Retrying in 1s · attempt 5/10`) and no `(esc to
        // interrupt)` of its own; `esc to interrupt` is on the mode row. Busy,
        // no wall, also with a message queued and at 80 columns.
        for (name, text) in [
            ("retrying", API_RETRYING_MEASURED),
            ("queued", API_RETRYING_QUEUED_MEASURED),
            ("queued 80", API_RETRYING_QUEUED_80_MEASURED),
            // A draft typed drops `esc to interrupt` from the mode row: the
            // retry row alone says the vendor is still working.
            ("draft", API_RETRYING_DRAFT_MEASURED),
            ("queued draft", API_RETRYING_QUEUED_DRAFT_MEASURED),
        ] {
            let r = read(Some("claude"), &screen(text), None);
            assert_eq!((r.phase, r.wall), (Phase::Busy, None), "{name}");
        }
    }

    /// The reading's context is the one the screen SHOWS ([`ScreenReader::context_shown`]):
    /// Claude Code 2.1.305's indicator at the end of its mode row reaches the tab's hover,
    /// `ctl survey` and the session log as aterm's footer reads it, while the supervisor's
    /// watch ([`ScreenReader::context`]) keeps the zone above the composer, where that build
    /// draws none. An older build's indicator above the composer reads the same in both.
    /// CONTROL: a reading filled from `context` reads `None` on the mode row.
    #[test]
    fn a_reading_carries_the_context_the_screen_shows() {
        let mut on_mode_row = rows(&["⏺ Done.", ""]);
        on_mode_row.extend(composer(&crate::prompt::fixtures::right_aligned(
            "  ⏸ manual mode on · ? for shortcuts",
            "0% until auto-compact",
        )));
        let claude = Program::Claude.reader();
        assert_eq!(
            read(Some("claude"), &on_mode_row, None).context_left,
            Some(0)
        );
        assert_eq!(claude.context(&on_mode_row), None, "the watch's zone");
        let mut above = rows(&["⏺ Done.", "", &format!("{:>120}", "7% until auto-compact")]);
        above.extend(composer("  ? for shortcuts"));
        assert_eq!(read(Some("claude"), &above, None).context_left, Some(7));
        assert_eq!(claude.context(&above), Some(7));
        let mut none = rows(&["⏺ Done.", ""]);
        none.extend(composer("  ? for shortcuts"));
        assert_eq!(read(Some("claude"), &none, None).context_left, None);
    }

    /// CLAUDE CODE'S OWN RETRY OF A USAGE LIMIT IS A TURN IN PROGRESS (accounts
    /// v2 S5): the watchdog's ticking row, measured on 2.1.300 and 2.1.301 —
    /// the inline renderer with and without a statusLine (with one, the
    /// footer draws no `esc to interrupt`), the fullscreen renderer, 110, 80
    /// and 50 columns (Ink's two columns under the glyph alone on its row),
    /// the five-hour and the weekly limit, a turn's first request and its
    /// second — reads busy on its status row and no wall, and so does the
    /// same row a second later, its span ticked. It is a live turn, never the
    /// agent's background work alone, with a shell running beside it too: the
    /// stall canary and the remembered wall, which act only on background
    /// work alone ([`crate::phase::background_only`]), see it as any spinner.
    /// The rows above it are the transcript. NEGATIVE CONTROLS: the row with
    /// its attempt no count is a done row, as the reader before read every
    /// such row — the turn over, idle on the statusLine screens, the session
    /// background work alone under a shell; and its words as the model's
    /// reply or a tool's output are no status row.
    #[test]
    fn the_watchdogs_retry_row_is_a_turn_in_progress() {
        use crate::phase::{Zone, background_only, transcript_end};
        use crate::prompt::fixtures::{
            WALL_WATCHDOG_FULLSCREEN_2_1_301, WALL_WATCHDOG_MIDTURN_2_1_301,
            WALL_WATCHDOG_SESSION_2_1_300, WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300,
            WALL_WATCHDOG_WEEKLY_2_1_300, WALL_WATCHDOG_WEEKLY_50_2_1_300,
            WALL_WATCHDOG_WEEKLY_80_2_1_300,
        };
        let shell = |rows: &[String]| -> Vec<String> {
            rows.iter()
                .map(|r| {
                    r.replace(
                        "auto mode on (shift+tab to cycle)",
                        "auto mode on · 1 shell",
                    )
                })
                .collect()
        };
        let tick = |rows: &[String]| -> Vec<String> {
            rows.iter()
                .map(|r| {
                    r.replace("Retrying in 2m 28s", "Retrying in 2m 27s")
                        .replace("Retrying in 59s", "Retrying in 58s")
                        .replace("Retrying in 3m 56s", "Retrying in 3m 55s")
                        .replace("Retrying in 14m", "Retrying in 13m 59s")
                        .replace("Retrying in 5h", "Retrying in 4h 59m")
                })
                .collect()
        };
        // The cut head at 50 columns (`Weekly li…`) alone reads as a
        // spinner's words running into their ellipsis: the control drops it.
        let old = |rows: &[String]| -> Vec<String> {
            rows.iter()
                .map(|r| {
                    r.replace("attempt 1/300", "attempt one of 300")
                        .replace("li…", "li")
                })
                .collect()
        };
        for (name, text, footer_busy) in [
            ("session", WALL_WATCHDOG_SESSION_2_1_300, true),
            (
                "session statusline",
                WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300,
                false,
            ),
            ("fullscreen", WALL_WATCHDOG_FULLSCREEN_2_1_301, false),
            ("weekly", WALL_WATCHDOG_WEEKLY_2_1_300, false),
            ("weekly 80", WALL_WATCHDOG_WEEKLY_80_2_1_300, false),
            ("weekly 50", WALL_WATCHDOG_WEEKLY_50_2_1_300, false),
            ("mid-turn", WALL_WATCHDOG_MIDTURN_2_1_301, true),
        ] {
            let rows = screen(text);
            for rows in [rows.clone(), tick(&rows)] {
                let r = read(Some("claude"), &rows, None);
                assert_eq!((&r.phase, &r.wall), (&Phase::Busy, &None), "{name}");
                let busy = busy_signal(&rows).expect(name);
                assert_eq!(
                    (busy.zone, busy.rule, busy.soft),
                    (Zone::Status, "spinner", false),
                    "{name}"
                );
                assert!(!background_only(&shell(&rows)), "{name}: a live turn");
                let row = rows
                    .iter()
                    .position(|r| r.contains("Retrying in"))
                    .expect("the retry row");
                assert!(transcript_end(&rows) <= row, "{name}: the live zone");
            }
            // NEGATIVE CONTROL: the row read as a done row.
            let done = old(&rows);
            let r = read(Some("claude"), &done, None);
            if footer_busy {
                assert_eq!(
                    busy_signal(&done).map(|b| b.zone),
                    Some(Zone::Footer),
                    "{name}"
                );
            } else {
                assert_eq!(r.phase, Phase::Idle, "{name}: the old reading");
                assert!(
                    background_only(&shell(&done)),
                    "{name}: background work alone"
                );
            }
        }
        let rows = screen(WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300);
        let at = rows
            .iter()
            .position(|r| r.contains("Retrying in"))
            .expect("the retry row");
        let text = rows[at].trim_start_matches(|c: char| c != 'S').to_string();
        let with = |row: String| {
            let mut rows = rows.clone();
            rows[at] = row;
            rows
        };
        // A pane that cuts the row's tail leaves it the row.
        for cut in [
            rows[at].replace("attempt 1/300", "attempt 1/3…"),
            rows[at].replace("attempt 1/300", "att…"),
        ] {
            assert_eq!(
                read(Some("claude"), &with(cut.clone()), None).phase,
                Phase::Busy,
                "{cut}"
            );
        }
        for said in [
            format!("⏺ {text}"),
            format!("  ⎿  {text}"),
            format!("     {text}"),
        ] {
            assert_eq!(busy_signal(&with(said.clone())), None, "{said}");
        }
    }

    /// Claude Code's critical-memory banner under a running spinner: busy, with the banner
    /// as a memory wall, the one wall a hard busy keeps ([`ScreenReader::health_wall`]); idle
    /// it is the same wall. Negative controls: the banner row blanked (only the draft's
    /// quote left), a box on the screen, and every reader but Claude Code's read no wall.
    #[test]
    fn the_memory_banner_is_a_wall_even_under_a_busy_spinner() {
        let busy = screen(MEMORY_BANNER_BUSY);
        for program in [Some("claude"), None] {
            let r = read(program, &busy, None);
            assert_eq!(r.program, Program::Claude);
            assert_eq!(r.phase, Phase::Busy, "{program:?}");
            assert!(busy_signal(&busy).is_some_and(|b| !b.soft), "hard busy");
            assert_eq!(
                r.wall.map(|w| w.kind),
                Some(WallKind::Memory),
                "{program:?}"
            );
        }
        let idle = read(Some("claude"), &screen(MEMORY_BANNER_IDLE), None);
        assert_eq!(idle.phase, Phase::Idle);
        assert_eq!(idle.wall.map(|w| w.kind), Some(WallKind::Memory));

        let at = memory_wall(&busy).expect("the banner row").row;
        let mut quoted = busy.clone();
        quoted[at] = String::new();
        let r = read(Some("claude"), &quoted, None);
        assert_eq!(r.phase, Phase::Busy);
        assert_eq!(r.wall, None, "only the draft's quote is left");

        // A box with the banner placed right above the composer's rule: the
        // banner is there to read (the control), the reading carries none.
        let mut boxed = bash_one_row();
        let top = boxed
            .iter()
            .rposition(|row| row.starts_with('─'))
            .expect("rules")
            - 2;
        let width = boxed[top].chars().count();
        let banner = busy[at].trim_start();
        boxed.insert(top, format!("{banner:>w$}", w = width - 2));
        assert!(memory_wall(&boxed).is_some(), "the control");
        let r = read(Some("claude"), &boxed, None);
        assert_eq!(r.phase, Phase::Prompt);
        assert_eq!(r.wall, None);

        // The generic reader (`cat` of the screen) and Codex's have no
        // health wall.
        assert_eq!(read(Some("zsh"), &busy, None).wall, None);
        assert_eq!(CodexReader.health_wall(&busy), None);
        assert_eq!(GenericReader.health_wall(&busy), None);
        assert_eq!(ClaudeReader.health_wall(&busy).map(|w| w.row), Some(at));
    }

    /// Claude Code is idle at its composer, not before it: the 2.1.283 launch frames before
    /// the REPL or with it half drawn, and a dialog with no reading (2.1.292's
    /// classifier-billing notice), read `idle` not authoritatively, with no composer; the
    /// REPL drawn whole is authoritative. Other readings with no frame stand.
    #[test]
    fn claude_codes_idle_is_read_at_its_composer() {
        use crate::prompt::fixtures::{
            AUTO_CLASSIFIER_BILLING_NOTICE_MEASURED, LAUNCH_BEFORE_REPL, LAUNCH_REPL_HALF_DRAWN,
            LAUNCH_REPL_READY,
        };
        for (name, text) in [
            ("before the REPL", LAUNCH_BEFORE_REPL),
            ("the REPL half drawn", LAUNCH_REPL_HALF_DRAWN),
            (
                "the classifier-billing notice",
                AUTO_CLASSIFIER_BILLING_NOTICE_MEASURED,
            ),
        ] {
            let r = read(Some("claude"), &screen(text), Some(2));
            assert_eq!(r.program, Program::Claude, "{name}");
            assert_eq!(r.phase, Phase::Idle, "{name}");
            assert!(!r.phase_authoritative, "{name}: {r:?}");
            assert_eq!((r.composer, r.wall), (None, None), "{name}");
            // The live zone the server classifies (its last 40 drawn rows)
            // says the same.
            let rows = screen(text);
            let zone = &rows[live_zone_start(&rows, 40)..];
            assert!(
                !read(Some("claude"), zone, None).phase_authoritative,
                "{name}"
            );
        }
        let ready = read(Some("claude"), &screen(LAUNCH_REPL_READY), Some(2));
        assert_eq!(
            (ready.phase.clone(), ready.phase_authoritative),
            (Phase::Idle, true),
            "the control: {ready:?}"
        );
        assert!(ready.composer.is_some() && ready.fresh, "{ready:?}");
        // The other readings with no composer frame on the screen stand:
        // [`claude_codes_idle_is_the_prompt_box_that_holds_the_cursor`].
        let end = screen(END_529);
        let top = end
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the rule");
        assert_eq!(
            read(Some("claude"), &end[..top], None).wall.map(|w| w.kind),
            Some(WallKind::Overloaded),
            "the wall's reading, its composer cut"
        );
    }

    /// The tall pane (2.1.284 at 144x50, `API_ERROR_TALL_PANE_MEASURED`): a short
    /// transcript with the composer pinned at the foot leaves 35 blank rows above it and
    /// the error 41 rows above the last drawn row, outside the grid's last 40. A gap of
    /// blank rows counts as one row, so the zone reaches the error. Controls: one-row
    /// separators cut exactly at `n`, and a gap of three rows is a separator.
    #[test]
    fn a_long_blank_gap_does_not_cut_the_last_thing_said_out_of_the_zone() {
        use crate::prompt::fixtures::API_ERROR_TALL_PANE_MEASURED;
        let rows = screen(API_ERROR_TALL_PANE_MEASURED);
        let error = rows
            .iter()
            .position(|r| r.starts_with("⏺ API Error"))
            .expect("the error row");
        let cursor = rows
            .iter()
            .rposition(|r| r.starts_with('❯'))
            .expect("caret");
        // The fixture's layout (0-based, header dropped): a 50-row pane, the
        // error on row 8, the done row on 10, 35 blank rows, the composer on
        // rows 46-48 (the caret on 47), the footer on 49.
        let done = rows
            .iter()
            .position(|r| r.starts_with("✻ Worked for"))
            .expect("the done row");
        let top_rule = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the composer's top rule");
        assert_eq!(
            (rows.len(), error, done, top_rule, cursor),
            (50, 8, 10, 46, 47)
        );
        assert!(rows[done + 1..top_rule].iter().all(|r| r.trim().is_empty()));
        assert_eq!(top_rule - done - 1, 35, "35 blank rows");
        assert!(!rows[rows.len() - 1].trim().is_empty(), "drawn to the foot");
        assert_eq!(
            rows.len() - 1 - error,
            41,
            "41 rows above the last drawn row"
        );
        // The grid's last 40 rows lie below the error.
        assert!(rows.len() - 40 > error, "the old cut lies below the error");
        // The zone reaches the error and the reader finds the wall.
        let start = live_zone_start(&rows, 40);
        assert!(start <= error, "{start} > {error}");
        let zone = &rows[start..];
        let r = read_at(Some("claude"), zone, Some(cursor - start), None);
        assert!(r.phase_authoritative, "{r:?}");
        assert_eq!(
            r.wall.as_ref().map(|w| (w.kind, w.placement)),
            Some((
                crate::wall::WallKind::ApiError {
                    code: None,
                    retryable: true,
                    cause: crate::wall::ApiCause::Unreachable
                },
                crate::wall::Placement::ErrorRow
            ))
        );
        // Control: with no gap (single blanks) the zone is the last `n` rows;
        // three blank rows are a separator too.
        let full: Vec<String> = (0..62)
            .map(|i| {
                if i % 2 == 0 {
                    format!("row {i}")
                } else {
                    String::new()
                }
            })
            .collect();
        // Content on the even rows: the last drawn row is 60, one row-count each.
        assert_eq!(live_zone_start(&full, 40), 61 - 40);
        let mut sep: Vec<String> = (0..62).map(|i| format!("row {i}")).collect();
        for i in [30, 31, 32] {
            sep[i] = String::new();
        }
        assert_eq!(live_zone_start(&sep, 40), 62 - 40);
        // A gap of four counts as one: the zone reaches three rows further.
        sep[33] = String::new();
        assert_eq!(live_zone_start(&sep, 40), 62 - 40 - 3);
        // All blank: read whole.
        assert_eq!(live_zone_start(&vec![String::new(); 62], 40), 0);
    }

    /// [`live_zone_reaches_above`]: the measured tall pane's last 40 rows hold the 35-row
    /// gap and say so; a full transcript, one-to-three-row separators and a tail that ends
    /// on a blank row do not. A sweep of runs checks it against the whole screen's zone:
    /// exact for a gap of four rows or more inside the tail, blind by design to one the
    /// tail's top row cuts.
    #[test]
    fn a_tail_that_holds_a_gap_says_the_zone_reaches_above_it() {
        use crate::prompt::fixtures::API_ERROR_TALL_PANE_MEASURED;
        let tall = screen(API_ERROR_TALL_PANE_MEASURED);
        let tail_of = |rows: &[String]| rows[rows.len().saturating_sub(40)..].to_vec();
        // The measured tall pane: the gap is in the tail.
        assert!(live_zone_reaches_above(&tail_of(&tall), 40));
        // Control: the gap filled (a full transcript) leaves the zone in the
        // last 40 rows, and the tail says nothing above.
        let mut full = tall.clone();
        for row in &mut full[11..46] {
            *row = "· earlier".to_string();
        }
        assert!(!live_zone_reaches_above(&tail_of(&full), 40));
        assert_eq!(live_zone_start(&full, 40), 10);
        // Control: separators of one to three rows are counted row by row.
        for gap in 1..BLANK_GAP {
            let mut sep = full.clone();
            for row in &mut sep[30..30 + gap] {
                row.clear();
            }
            assert!(!live_zone_reaches_above(&tail_of(&sep), 40), "gap {gap}");
        }
        // The gap's own size: BLANK_GAP rows is one, the tail says so.
        let mut four = full.clone();
        for row in &mut four[30..30 + BLANK_GAP] {
            row.clear();
        }
        assert!(live_zone_reaches_above(&tail_of(&four), 40));
        assert_eq!(live_zone_start(&four, 40), 10 - (BLANK_GAP - 1));
        // Control: a tail that ends on a blank row is the supervisor's `drawn_above_tail`'s.
        let mut inline = tall.clone();
        inline.extend(std::iter::repeat_n(String::new(), 12));
        assert!(!live_zone_reaches_above(&tail_of(&inline), 40));
        assert!(!live_zone_reaches_above(&vec![String::new(); 40], 40));
        // The limit: a gap cut by the tail's top row, fewer than BLANK_GAP of
        // its rows inside, is not seen from the tail alone.
        let mut cut = full.clone();
        cut[10].clear();
        assert!(!live_zone_reaches_above(&tail_of(&cut), 40));
        // Exact over runs: a blank run of one to ten rows whose last row is 0
        // to 3 rows inside the tail is seen when four or more of its rows are
        // inside, and where the whole screen's zone begins above its last 40
        // rows unseen by the tail, the run is one of the cut ones.
        let (mut above_seen, mut cut_seen) = (0, 0);
        for run in 1..=10usize {
            for inside in 0..=3usize {
                let mut rows: Vec<String> = (0..62).map(|i| format!("row {i}")).collect();
                let last40 = rows.len() - 40;
                let bottom = last40 + inside;
                for row in &mut rows[(bottom + 1).saturating_sub(run)..=bottom] {
                    row.clear();
                }
                let in_tail = run.min(inside + 1);
                let above = live_zone_start(&rows, 40) < last40;
                let asks = live_zone_reaches_above(&tail_of(&rows), 40);
                assert_eq!(asks, in_tail >= BLANK_GAP, "run {run}, {inside} inside");
                assert_eq!(
                    above,
                    run >= BLANK_GAP,
                    "run {run}, {inside} inside: the zone is above the tail"
                );
                above_seen += usize::from(above);
                cut_seen += usize::from(above && !asks);
            }
        }
        assert!(above_seen > 10, "the sweep saw {above_seen} zones above");
        assert!(
            cut_seen > 0,
            "and some cut ones ({cut_seen}) the tail cannot see"
        );
    }

    /// The inline renderer's REPL sits above the grid's last 40 rows (2.1.283 at 150x50,
    /// the `INLINE_*` fixtures: the box on rows 8-11, 38 blank rows under it). Those rows
    /// alone are no evidence; the live zone holds the whole REPL, authoritative idle. The
    /// inline launch's other frames keep their readings through the zone, and a screen
    /// drawn to its last row with no gap has the last 40 rows as its zone.
    #[test]
    fn an_inline_repl_above_the_last_40_rows_is_read_in_the_live_zone() {
        use crate::prompt::fixtures::{
            INLINE_REPL_HALF_DRAWN, INLINE_REPL_READY, INLINE_TRUST, LAUNCH_REPL_READY,
        };
        let zone = |rows: &[String]| rows[live_zone_start(rows, 40)..].to_vec();
        let tail = |rows: &[String]| rows[rows.len().saturating_sub(40)..].to_vec();
        let ready = screen(INLINE_REPL_READY);
        assert_eq!(ready.len(), 50, "a 50-row pane");
        let caret = ready
            .iter()
            .position(|r| r.starts_with('❯'))
            .expect("the caret row");
        assert!(caret < 10, "the prompt box sits above the last 40 rows");
        // The grid's last 40 rows alone: no evidence.
        let cut = read(Some("claude"), &tail(&ready), None);
        assert_eq!(
            (cut.phase.clone(), cut.phase_authoritative),
            (Phase::Idle, false),
            "the grid's last 40 rows: {cut:?}"
        );
        // The live zone holds the whole REPL.
        assert_eq!(live_zone_start(&ready, 40), 0);
        let r = read(Some("claude"), &zone(&ready), Some(2));
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{r:?}"
        );
        assert!(r.composer.is_some() && r.fresh, "{r:?}");
        // The inline launch's other frames, through the zone.
        let dialog = read(Some("claude"), &zone(&screen(INLINE_TRUST)), None);
        assert_eq!(
            (dialog.phase.clone(), dialog.prompt.as_ref().map(|p| p.kind)),
            (Phase::Prompt, Some(PromptKind::Trust)),
            "{dialog:?}"
        );
        let half = read(Some("claude"), &zone(&screen(INLINE_REPL_HALF_DRAWN)), None);
        assert_eq!(
            (half.phase, half.phase_authoritative, half.composer),
            (Phase::Idle, false, None),
            "the REPL half drawn is no evidence"
        );
        // The fullscreen REPL at launch: the banner at the top, the composer
        // pinned at its foot, a blank gap between. The gap counts as one
        // row, so the zone holds the launch card too (`fresh`).
        let full = screen(LAUNCH_REPL_READY);
        assert_eq!(full.len(), 50);
        assert_eq!(live_zone_start(&full, 40), 0, "the banner is in the zone");
        let launch = read(Some("claude"), &zone(&full), Some(45));
        assert!(launch.fresh, "{launch:?}");
        // The control: a fullscreen screen drawn to its last row, no gap, has
        // the grid's last 40 rows as its zone.
        let drawn: Vec<String> = (0..50).map(|i| format!("row {i}")).collect();
        assert_eq!(zone(&drawn), tail(&drawn));
    }

    /// [`live_zone_start`]'s arithmetic: the last `n` rows counted up from
    /// the last row that is not blank (spaces and no-break spaces are blank),
    /// never below 0; a screen with nothing drawn starts at 0.
    #[test]
    fn the_live_zone_counts_drawn_rows() {
        let grid = |drawn: usize, blank: usize| {
            let mut r: Vec<String> = (0..drawn).map(|i| format!("row {i}")).collect();
            r.extend((0..blank).map(|i| [" ", "", "\u{a0}"][i % 3].to_string()));
            r
        };
        for (drawn, blank, n, start) in [
            (50, 0, 40, 10),
            (12, 38, 40, 0),
            (45, 5, 40, 5),
            (60, 40, 40, 20),
            (3, 0, 40, 0),
            (0, 50, 40, 0),
            (0, 0, 40, 0),
            (10, 5, 4, 6),
        ] {
            assert_eq!(
                live_zone_start(&grid(drawn, blank), n),
                start,
                "{drawn} drawn, {blank} blank, n={n}"
            );
        }
    }

    /// Claude Code's shell mode (`!` as the caret, the `SHELL_MODE*` fixtures) is its prompt
    /// box: authoritative idle, though no `❯` composer or frame. Negative controls: no
    /// bottom rule, no top rule, and a `!` that heads a word.
    #[test]
    fn claude_codes_shell_mode_is_its_prompt_box() {
        use crate::prompt::fixtures::{SHELL_MODE, SHELL_MODE_DRAFT};
        for (name, text, caret) in [
            (
                "shell mode",
                SHELL_MODE,
                "!\u{a0}Try \"write a test for <filepath>\"",
            ),
            ("shell mode, a command typed", SHELL_MODE_DRAFT, "!\u{a0}ls"),
        ] {
            let full = screen(text);
            assert!(
                full.iter().any(|r| r == caret),
                "{name}: the measured caret row"
            );
            // The whole screen, and the live zone the server classifies (its
            // last 40 drawn rows).
            for rows in [&full[..], &full[live_zone_start(&full, 40)..]] {
                let r = read(Some("claude"), rows, Some(2));
                assert_eq!(r.program, Program::Claude, "{name}");
                assert_eq!(
                    (r.phase.clone(), r.phase_authoritative),
                    (Phase::Idle, true),
                    "{name}: {r:?}"
                );
                assert_eq!((r.composer, r.wall), (None, None), "{name}");
                assert!(!has_composer_frame(rows), "{name}");
            }
            // Negative controls, each one edit of the measured screen.
            let at = full.iter().position(|r| r == caret).expect("the caret");
            let bottom = at + 1;
            assert!(crate::phase::is_rule(&full[bottom]), "{name}");
            let mut half = full.clone();
            half[bottom] = String::new();
            let mut unruled = full.clone();
            unruled[at - 1] = String::new();
            let mut word = full.clone();
            word[at] = "!important: not a caret".to_string();
            for (what, rows) in [
                ("no bottom rule", half),
                ("no top rule", unruled),
                ("a word", word),
            ] {
                let r = read(Some("claude"), &rows, None);
                assert_eq!(
                    (r.phase.clone(), r.phase_authoritative),
                    (Phase::Idle, false),
                    "{name}, {what}: {r:?}"
                );
            }
        }
    }

    /// Claude Code's idle is the prompt box that holds the cursor: an inline relaunch leaves
    /// the previous run's box whole above the new launch line, authoritative idle by the
    /// frame alone but no evidence with the cursor under the launch line. Every measured
    /// 2.1.283 launch and relaunch frame, read with its cursor, reads as the REPL it shows.
    /// Negative controls: the cursor outside the box or on no row; the cursor changes no
    /// box, wall or spinner reading, nor another program's.
    #[test]
    fn claude_codes_idle_is_the_prompt_box_that_holds_the_cursor() {
        use crate::prompt::fixtures::{
            INLINE_RELAUNCH_BEFORE_REPL, INLINE_RELAUNCH_REPL_HALF_DRAWN,
            INLINE_RELAUNCH_REPL_READY, INLINE_RELAUNCH_TRUST, INLINE_REPL_HALF_DRAWN,
            INLINE_REPL_READY, INLINE_TRUST, LAUNCH_BEFORE_REPL, LAUNCH_REPL_HALF_DRAWN,
            LAUNCH_REPL_READY, SHELL_MODE, SHELL_MODE_DRAFT, cursor,
        };
        let stale = screen(INLINE_RELAUNCH_BEFORE_REPL);
        let (row, col) = cursor(INLINE_RELAUNCH_BEFORE_REPL).expect("measured");
        assert!(
            stale[..row]
                .iter()
                .any(|r| r.contains("Press Ctrl-C again to exit"))
        );
        // The frame alone reads the old box as the REPL.
        let framed = read(Some("claude"), &stale, None);
        assert_eq!(
            (framed.phase.clone(), framed.phase_authoritative),
            (Phase::Idle, true),
            "the frame rule: {framed:?}"
        );
        // The cursor is under the new launch line, not in the box.
        let at = read_at(Some("claude"), &stale, Some(row), Some(col));
        assert_eq!(
            (at.phase.clone(), at.phase_authoritative),
            (Phase::Idle, false),
            "{at:?}"
        );
        let zone = live_zone_start(&stale, 40);
        let zoned = read_at(
            Some("claude"),
            &stale[zone..],
            row.checked_sub(zone),
            Some(col),
        );
        assert!(!zoned.phase_authoritative, "the live zone: {zoned:?}");

        // Every measured launch and relaunch frame, with its measured cursor.
        for (name, text, want) in [
            ("launch, before the REPL", LAUNCH_BEFORE_REPL, None),
            ("launch, the REPL half drawn", LAUNCH_REPL_HALF_DRAWN, None),
            ("launch, the REPL", LAUNCH_REPL_READY, Some(Phase::Idle)),
            ("shell mode", SHELL_MODE, Some(Phase::Idle)),
            (
                "shell mode, a command typed",
                SHELL_MODE_DRAFT,
                Some(Phase::Idle),
            ),
            ("inline, the dialog", INLINE_TRUST, Some(Phase::Prompt)),
            ("inline, the REPL half drawn", INLINE_REPL_HALF_DRAWN, None),
            ("inline, the REPL", INLINE_REPL_READY, Some(Phase::Idle)),
            (
                "relaunch, the dialog",
                INLINE_RELAUNCH_TRUST,
                Some(Phase::Prompt),
            ),
            (
                "relaunch, before the REPL",
                INLINE_RELAUNCH_BEFORE_REPL,
                None,
            ),
            (
                "relaunch, half drawn",
                INLINE_RELAUNCH_REPL_HALF_DRAWN,
                None,
            ),
            (
                "relaunch, the REPL",
                INLINE_RELAUNCH_REPL_READY,
                Some(Phase::Idle),
            ),
        ] {
            let rows = screen(text);
            let (row, col) = cursor(text).unwrap_or_else(|| panic!("{name}: a measured cursor"));
            assert!(row < rows.len(), "{name}");
            let zone = live_zone_start(&rows, 40);
            for (cut, r) in [
                (
                    "whole",
                    read_at(Some("claude"), &rows, Some(row), Some(col)),
                ),
                (
                    "zone",
                    read_at(
                        Some("claude"),
                        &rows[zone..],
                        row.checked_sub(zone),
                        Some(col),
                    ),
                ),
            ] {
                let got = r.phase_authoritative.then(|| r.phase.clone());
                assert_eq!(got, want, "{name} ({cut}): {r:?}");
            }
            // Negative controls: the same screen with the cursor on no row
            // read, and on every row outside a prompt box, is no idle.
            let off = read_at(Some("claude"), &rows, None, None);
            if want == Some(Phase::Idle) {
                assert!(!off.phase_authoritative, "{name}: the cursor off the rows");
                let outside =
                    (0..rows.len()).filter(|&r| !crate::phase::prompt_box_holds(&rows, r));
                for r in outside {
                    assert!(
                        !read_at(Some("claude"), &rows, Some(r), None).phase_authoritative,
                        "{name}: the cursor on row {r}, outside the box"
                    );
                }
            } else {
                assert_eq!(off.phase_authoritative, want.is_some(), "{name}");
            }
        }

        // The cursor takes nothing from any phase but idle at a box, nor
        // from a wall, nor from another program's reader.
        let spinning = rows(&["% claude", "", "✶ Deliberating… (3s · thinking)"]);
        let end = screen(END_529);
        let top = end
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the rule");
        for (name, r, phase) in [
            ("trust dialog", screen(TRUST), Phase::Prompt),
            ("spinner", spinning, Phase::Busy),
            ("question", shell_asking(), Phase::Question),
            ("wall", end[..top].to_vec(), Phase::Idle),
        ] {
            let reading = read_at(Some("claude"), &r, None, None);
            assert_eq!(
                (reading.phase, reading.phase_authoritative),
                (phase, true),
                "{name}"
            );
        }
        for program in [Some("codex"), None] {
            let codex = codex_0_155_trust();
            assert_eq!(
                read_at(program, &codex, None, None),
                read(program, &codex, None),
                "{program:?}: the cursor is Claude Code's rule"
            );
        }
    }

    /// [`crate::phase::prompt_box_holds`]: a box holds its rows from its top rule to its
    /// bottom rule, for the `❯` composer and shell mode's `!` alike; nothing else holds,
    /// and a whole box does not stretch down to a half-drawn one under it (the relaunch).
    #[test]
    fn a_prompt_box_holds_its_rows_and_nothing_else() {
        use crate::phase::prompt_box_holds as holds;
        use crate::prompt::fixtures::{INLINE_RELAUNCH_REPL_HALF_DRAWN, cursor};
        let rule = "─".repeat(40);
        for caret in ["❯ Try \"fix lint errors\"", "!\u{a0}ls", "!"] {
            let r = rows(&[
                "banner",
                "",
                &rule,
                caret,
                "  more of the draft",
                &rule,
                "  footer",
                "",
            ]);
            let held: Vec<usize> = (0..r.len() + 2).filter(|&i| holds(&r, i)).collect();
            assert_eq!(held, [2, 3, 4, 5], "{caret}");
        }
        let unruled = rows(&["banner", "", "❯ a transcript row", &rule, "  footer"]);
        assert!((0..unruled.len()).all(|i| !holds(&unruled, i)));
        let half = rows(&["", &rule, "❯ Try", "──"]);
        assert!((0..half.len()).all(|i| !holds(&half, i)));
        assert!(!holds(&[], 0) && !holds(&rows(&["❯"]), 0));
        let relaunch = screen(INLINE_RELAUNCH_REPL_HALF_DRAWN);
        let (row, _) = cursor(INLINE_RELAUNCH_REPL_HALF_DRAWN).expect("measured");
        let old: Vec<usize> = (0..relaunch.len())
            .filter(|&i| holds(&relaunch, i))
            .collect();
        assert_eq!(old, [8, 9, 10], "only the old box, whole");
        assert!(
            !holds(&relaunch, row),
            "the cursor, on the new box's half-drawn rule"
        );
    }

    /// The banner's vendor remedy (`— restart and resume with claude --continue`) is cut by
    /// [`crate::wall::memory_banner_head`]: `--continue` resumes the directory's newest
    /// conversation, which may be a sibling tab's. Negative control: the whole message
    /// still carries it.
    #[test]
    fn the_memory_banner_head_leaves_the_vendors_continue_out() {
        let banner = memory_wall(&screen(MEMORY_BANNER_BUSY)).expect("the banner");
        assert!(banner.message.contains("--continue"), "{}", banner.message);
        let head = crate::wall::memory_banner_head(&banner.message);
        assert_eq!(
            head,
            format!("{} (140.4GB)", crate::anchors::anchor_text("wall.memory"))
        );
        assert!(!head.contains("--continue"), "{head}");
    }

    /// agy ALONE CANNOT BOUND A MESSAGE THAT HOLDS A BLANK LINE
    /// ([`ScreenReader::bounds_messages`]): on the L15 worker's measured
    /// screen the first prompt's blank line, drawn as a blank row, ends the
    /// message as agy's reader reads it — the task alone — and the fact line
    /// under it, two columns in, reads as agy's reply.
    #[test]
    fn agy_alone_cannot_bound_a_message_that_holds_a_blank_line() {
        let rows = crate::prompt::fixtures::screen(crate::agy::fixtures::lab::WORKER_FIRST_PROMPT);
        let act = rows
            .iter()
            .position(|r| r.starts_with("> [aterm harness]"))
            .expect("the Land act");
        let caret = rows
            .iter()
            .rposition(|r| r.trim_end() == ">")
            .expect("the composer");
        // The screen as it stood before the act: the first turn, then the
        // composer's frame.
        let first: Vec<String> = rows[..act - 1]
            .iter()
            .chain(&rows[caret - 1..])
            .cloned()
            .collect();
        let first = &first[..];
        assert_eq!(
            crate::agy::turn_opened_by(first).as_deref(),
            Some("FOREVERLAB-WORKER. write the parser for this repository and land it")
        );
        assert!(
            crate::agy::said_tail(first)
                .is_some_and(|said| said.starts_with("(Started by `s-146719f2750f4a8f6909`")),
            "the fact line read as the reply"
        );
        assert!(!Program::Agy.reader().bounds_messages());
    }

    /// ONLY AGY READS A MESSAGE'S ROWS AS ITS REPLY, which is what
    /// [`ScreenReader::bounds_messages`] says of each worker's reader: on
    /// each one's measured end of a turn (Claude Code's drawn in its frame),
    /// rows put in the message under its first row — a blank row, then a row
    /// at the message's own indent saying `NEEDS YOU: y` (two columns in,
    /// Gemini CLI's three inside its band), agy's drawing of a message's
    /// blank line and the worst case for every reader — change the reply
    /// agy's reader reads and no other's: Claude Code's, Gemini CLI's and
    /// Muse Code's take the reply from its head's mark. The rows put in are
    /// a model of each program's continuation, not a capture of its blank
    /// line (Claude Code, Gemini CLI and Muse Code drew the L15 worker's
    /// multi-line first prompt with the reply read whole,
    /// `docs/measured/forever-lab-2026-10-20.md`).
    /// Codex (no worker) and a generic program are not drawn: no worker
    /// runs them, and they keep the default.
    #[test]
    fn only_agy_reads_a_messages_rows_as_its_reply() {
        use crate::prompt::fixtures::screen;
        let claude = {
            let mut r = rows(&["❯ Fix the parser.", "", "⏺ Fixed the parser.", ""]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        let cases = [
            (Program::Claude, claude, "❯ ", "  "),
            (
                Program::Gemini,
                screen(crate::gemini::fixtures::END_OF_TURN),
                " > ",
                "   ",
            ),
            (
                Program::Muse,
                screen(crate::muse::fixtures::END_OF_TURN),
                "❯ ",
                "  ",
            ),
            (
                Program::Agy,
                screen(crate::agy::fixtures::END_OF_TURN),
                "> ",
                "  ",
            ),
        ];
        for (program, rows, caret, indent) in cases {
            let reader = program.reader();
            let said = reader.said_tail(&rows);
            assert!(said.is_some(), "{program:?}: a reply to read");
            let at = rows
                .iter()
                .position(|r| r.starts_with(caret) && r.trim().len() > 1)
                .unwrap_or_else(|| panic!("{program:?}: the message's first row"));
            let mut more = rows.clone();
            more.splice(
                at + 1..at + 1,
                [String::new(), format!("{indent}NEEDS YOU: y")],
            );
            let read = reader.said_tail(&more);
            assert_eq!(
                read == said,
                reader.bounds_messages(),
                "{program:?}: {said:?} then {read:?}"
            );
        }
    }
}
