// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-program screen readers ([`ScreenReader`]) and program identification ([`identify`]).

use crate::anchors::guard_regex;
use crate::login::LoginScreen;
use crate::phase::{
    Phase, busy_signal, composer_draft, context_left, has_composer_frame, has_shell_mode_frame,
    prompt_box_holds, worker_phase,
};
use crate::prompt::{CancelEffect, PromptKind, PromptV2, parse_prompt_v2};
use crate::turn::{continuation_suggestion, goal_active, interrupted, said_tail};
use crate::wall::{Wall, memory_wall, wall};
use crate::{agy, grok, kimi, muse};
pub use aterm_agent_names::{AGENT_RUNTIMES, may_host_agent};
use aterm_agent_names::{Agent, agent_of, program_word};

/// Program identified for a terminal screen. Every variant is in [`Program::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Program {
    Claude,
    Codex,
    /// Google's Antigravity CLI.
    Agy,
    /// Meta's Muse Code.
    Muse,
    /// xAI's Grok Build.
    Grok,
    /// Moonshot AI's Kimi Code.
    Kimi,
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
        Program::Muse => Some(Program::Grok),
        Program::Grok => Some(Program::Kimi),
        Program::Kimi => Some(Program::Generic),
        Program::Generic => None,
    }
}

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
    /// agent) iterates this, never a list of its own.
    pub const ALL: [Program; 7] = [
        Program::Claude,
        Program::Codex,
        Program::Agy,
        Program::Muse,
        Program::Grok,
        Program::Kimi,
        Program::Generic,
    ];

    /// Lowercase CLI name for this program.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Program::Claude => "claude",
            Program::Codex => "codex",
            Program::Agy => "agy",
            Program::Muse => "muse",
            Program::Grok => "grok",
            Program::Kimi => "kimi",
            Program::Generic => "generic",
        }
    }

    /// Display name used on harness surfaces: the agent's, by the name table
    /// ([`Agent::product`]).
    #[inline]
    #[must_use]
    pub const fn product(self) -> &'static str {
        match self.agent() {
            Some(agent) => agent.product(),
            None => "the program",
        }
    }

    /// The agent this program is in the name table, `None` for [`Program::Generic`].
    #[inline]
    #[must_use]
    pub const fn agent(self) -> Option<Agent> {
        match self {
            Program::Claude => Some(Agent::Claude),
            Program::Codex => Some(Agent::Codex),
            Program::Agy => Some(Agent::Agy),
            Program::Muse => Some(Agent::Muse),
            Program::Grok => Some(Agent::Grok),
            Program::Kimi => Some(Agent::Kimi),
            Program::Generic => None,
        }
    }

    /// The program `agent` is.
    const fn of(agent: Agent) -> Program {
        match agent {
            Agent::Claude => Program::Claude,
            Agent::Codex => Program::Codex,
            Agent::Agy => Program::Agy,
            Agent::Muse => Program::Muse,
            Agent::Grok => Program::Grok,
            Agent::Kimi => Program::Kimi,
        }
    }

    /// Whether the supervisor's screen policy acts on this program. Grok Build's reader is for
    /// display only; Codex carries its own supervision and a generic screen has no agent policy.
    #[inline]
    #[must_use]
    pub const fn supervisable(self) -> bool {
        match self {
            Program::Claude | Program::Agy | Program::Muse | Program::Kimi => true,
            Program::Codex | Program::Grok | Program::Generic => false,
        }
    }

    /// Whether this program has a `[harness.<agent>]` config table and Settings row.
    #[inline]
    #[must_use]
    pub const fn harness_table(self) -> bool {
        match self {
            Program::Claude | Program::Codex | Program::Agy | Program::Muse | Program::Kimi => true,
            Program::Grok | Program::Generic => false,
        }
    }

    /// Canonical [`crate::CliHarness`] entry for this program, or `None` for Codex (native controls) and [`Program::Generic`].
    #[inline]
    #[must_use]
    pub const fn cli_harness(self) -> Option<crate::CliHarness> {
        match self {
            Program::Claude => Some(crate::CliHarness::ClaudeCode),
            Program::Agy => Some(crate::CliHarness::Agy),
            Program::Muse => Some(crate::CliHarness::Muse),
            Program::Grok => Some(crate::CliHarness::Grok),
            Program::Kimi => Some(crate::CliHarness::Kimi),
            Program::Codex | Program::Generic => None,
        }
    }

    /// Structured incremental numeric [`crate::HarnessVersion`] of this program's harness.
    #[inline]
    #[must_use]
    pub const fn harness_version_num(self) -> Option<crate::HarnessVersion> {
        match self.cli_harness() {
            Some(h) => Some(h.version()),
            None => None,
        }
    }

    /// Incremental version of this program's aterm/alab harness, `"0.4"`, one per harness.
    #[inline]
    #[must_use]
    pub const fn harness_version(self) -> Option<&'static str> {
        match self.cli_harness() {
            Some(h) => Some(h.version_str()),
            None => None,
        }
    }

    /// Screen reader for this program.
    #[inline]
    #[must_use]
    pub fn reader(self) -> &'static dyn ScreenReader {
        match self {
            Program::Claude => &ClaudeReader,
            Program::Codex => &CodexReader,
            Program::Agy => &AgyReader,
            Program::Muse => &MuseReader,
            Program::Grok => &GrokReader,
            Program::Kimi => &KimiReader,
            Program::Generic => &GenericReader,
        }
    }
}

/// Complete screen reading produced by a [`ScreenReader`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub program: Program,
    pub phase: Phase,
    /// Whether [`Self::phase`] is the reader's evidence rather than its default.
    pub phase_authoritative: bool,
    /// The wall the last turn ended on: `None` under a box, and only a health wall under a hard
    /// busy ([`ScreenReader::health_wall`]).
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
    /// A person stopped the last turn: the next message is theirs
    /// ([`crate::turn::interrupted`]).
    pub interrupted: bool,
    /// No turn yet ([`ScreenReader::fresh`]): an idle point here ended nothing, since a session
    /// nobody has asked anything is no worker stopped short.
    pub fresh: bool,
    /// The composer as `(caret row, lines)`, the caret row's text first; `None` with no composer.
    pub composer: Option<(usize, Vec<String>)>,
    /// Usage credits carry the session, by the program's own notice
    /// ([`ScreenReader::credits`]).
    pub credits: Option<crate::credits::Credits>,
}

/// A caller's veto on a row [`ScreenReader::said_lines`] would join by the pane's width:
/// `apart(line, row)`, `line` the line so far and `row` the next row's text, says the row opens
/// a line of its own all the same.
pub type Apart = fn(&str, &str) -> bool;

/// What joins a turn's earlier messages into one text ([`ScreenReader::turn_said_before`]): a
/// blank line, as a message's own paragraphs are joined.
pub const MESSAGE_GAP: &str = "\n\n";

/// A turn's earlier `messages` as one text ([`MESSAGE_GAP`]).
#[must_use]
pub fn join_messages(messages: &[String]) -> String {
    messages.join(MESSAGE_GAP)
}

/// Per-program screen grammar.
pub trait ScreenReader: Sync {
    fn program(&self) -> Program;
    /// Structured version of this program's harness, or `None` for Codex (native controls) and [`Program::Generic`].
    fn harness_version_num(&self) -> Option<crate::HarnessVersion> {
        self.program().harness_version_num()
    }
    /// Incremental version of this program's harness (`"0.4"`), or `None` for Codex (native controls) and [`Program::Generic`].
    fn harness_version(&self) -> Option<&'static str> {
        self.program().harness_version()
    }
    fn phase(&self, rows: &[String]) -> Phase;
    fn prompt(&self, _rows: &[String]) -> Option<PromptV2> {
        None
    }
    /// Effect of pressing Esc on `prompt`.
    fn esc(&self, prompt: &PromptV2) -> Option<CancelEffect> {
        prompt.cancel.as_ref().map(|c| c.effect)
    }
    fn wall(&self, _rows: &[String]) -> Option<Wall> {
        None
    }
    /// Stuck-program wall detected even while a turn is running.
    fn health_wall(&self, _rows: &[String]) -> Option<Wall> {
        None
    }
    /// Whether a feedback survey card is open on `rows`.
    fn survey(&self, rows: &[String]) -> bool {
        self.survey_block(rows).is_some()
    }
    /// Open feedback survey card on `rows`, if any.
    fn survey_block(&self, _rows: &[String]) -> Option<crate::survey::SurveyBlock> {
        None
    }
    /// Description of agent-started background work keeping an idle screen busy, if any.
    fn background_wait(&self, _rows: &[String]) -> Option<&'static str> {
        None
    }
    /// The phase a [`Phase::Busy`] screen rests at when only the agent's own background work keeps
    /// it busy and its turn is over ([`crate::phase::background_rest`]), `None` where the reader
    /// reads no such turn end.
    fn background_rest(&self, _rows: &[String]) -> Option<Phase> {
        None
    }
    /// The agent's own background work this screen names with its turn over, in
    /// [`Self::background_wait`]'s words: the stall canary then holds the agent's words to it, and
    /// on `None` only the agent's title can say it rests. Claude Code's wait reaches further than
    /// this ([`crate::phase::work_left`]).
    fn work_left(&self, rows: &[String]) -> Option<&'static str> {
        self.background_wait(rows)
    }
    /// The agent's own work running under its turn's end that a restart in place would end, as
    /// this screen names it, in [`Self::background_wait`]'s words. Claude Code's reading keeps
    /// only what names live work ([`crate::phase::work_running`]): a workflow's progress row,
    /// which outlives its workflow, is none of it.
    fn work_running(&self, rows: &[String]) -> Option<&'static str> {
        self.background_wait(rows)
    }
    /// Whether the last turn's end is hidden off-screen while the session rests idle.
    fn end_hidden(&self, _rows: &[String]) -> bool {
        false
    }
    /// Remaining context percentage before the program compacts it, as the screen shows it: what
    /// the supervisor's context watch (`EVENT context`) and `aterm drive phase` (`context <n>%`)
    /// read, and what the window's displays read unless [`Self::context_shown`] says otherwise.
    fn context(&self, _rows: &[String]) -> Option<u8> {
        None
    }
    /// Remaining context percentage shown on screen for the window's displays
    /// ([`Reading::context_left`]): [`Self::context`], unless the program shows a context share
    /// the watch does not take (Kimi Code's footer).
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
    /// Whether the message read by [`Self::said_tail`] is fully visible on `rows`.
    fn said_whole(&self, _rows: &[String]) -> bool {
        true
    }
    /// Whether `row` bounds a cut message when scanning history above the screen.
    fn said_history_stop(&self, row: &str) -> bool {
        !row.is_empty() && !row.starts_with(char::is_whitespace)
    }
    /// Earlier agent messages from the same turn above [`Self::said_tail`], joined by blank lines
    /// ([`join_messages`]).
    fn turn_said_before(&self, _rows: &[String]) -> Option<String> {
        None
    }
    /// [`Self::turn_said_before`] message by message, oldest first: the texts it joins. A reader
    /// that cannot tell its messages apart answers the joined text as one message.
    fn turn_messages_before(&self, rows: &[String]) -> Option<Vec<String>> {
        self.turn_said_before(rows).map(|t| vec![t])
    }
    /// [`Self::said_tail`], each line the program wrapped across rows read as one by the pane's
    /// width, every other row, and each row `apart` sets apart ([`Apart`]), a line of its own.
    fn said_lines(&self, rows: &[String], _apart: Apart) -> Option<String> {
        self.said_tail(rows)
    }
    /// [`Self::turn_said_before`], its lines read as [`Self::said_lines`] reads them.
    fn turn_lines_before(&self, rows: &[String], _apart: Apart) -> Option<String> {
        self.turn_said_before(rows)
    }
    /// [`Self::turn_lines_before`] message by message, oldest first
    /// ([`Self::turn_messages_before`]).
    fn turn_message_lines_before(&self, rows: &[String], apart: Apart) -> Option<Vec<String>> {
        self.turn_lines_before(rows, apart).map(|t| vec![t])
    }
    /// Whether the entire turn ending at [`Self::said_tail`] is visible on `rows`.
    fn turn_whole(&self, rows: &[String]) -> bool {
        self.said_whole(rows)
    }
    /// Whether the last turn ran a tool: a tool row of the program's under the message that opened it.
    fn turn_ran_tools(&self, _rows: &[String]) -> bool {
        false
    }
    /// Whether [`Self::turn_ran_tools`] reads this program's tool rows, so a turn it finds none in ran none.
    fn reads_tool_rows(&self) -> bool {
        false
    }
    /// Whether the message that opened the last turn is above `rows`, so [`Self::turn_ran_tools`] finding none there rules none out.
    fn turn_opens_above(&self, _rows: &[String]) -> bool {
        false
    }
    /// Whether `row` bounds a cut turn when scanning history above the screen.
    fn turn_history_stop(&self, row: &str) -> bool {
        self.said_history_stop(row)
    }
    /// User message that opened the turn ending at [`Self::said_tail`], if visible on `rows`.
    fn turn_opened_by(&self, _rows: &[String]) -> Option<String> {
        None
    }
    /// Whether reply rows are marked apart from user message rows by text alone.
    fn bounds_messages(&self) -> bool {
        true
    }
    /// Caret row and candidate blank rows whose cell styles bound the last user message.
    fn message_blanks(&self, _rows: &[String]) -> Option<(usize, Vec<usize>)> {
        None
    }
    /// Return `rows` with in-message blank rows `inner` folded out of the message at `caret`.
    fn fold_message(&self, rows: &[String], _caret: usize, _inner: &[usize]) -> Vec<String> {
        rows.to_vec()
    }
    /// Whether any user message is drawn in the transcript on `rows`.
    fn message_drawn(&self, rows: &[String]) -> bool {
        rows.iter().any(|r| !r.trim().is_empty())
    }
    /// Whether the program has drawn on `rows`: its composer's frame, a box or dialog, or its
    /// launch card. A blank screen, or one holding only the shell's prompt and launch line, is
    /// not, and a supervisor reads it as the program still starting, never as idle. The default
    /// credits any screen.
    fn program_drawn(&self, _rows: &[String]) -> bool {
        true
    }
    fn interrupted(&self, _rows: &[String]) -> bool {
        false
    }
    /// Whether the program's own housekeeping (e.g. compaction) ended the last turn.
    fn housekeeping(&self, _rows: &[String]) -> bool {
        false
    }
    /// The program's own notice that it stopped the last turn, where only a person clears what it
    /// names (Muse Code's fault rows, [`muse::stopped_turn`]): no reply and no wall, so no rule
    /// types at it.
    fn stopped_turn(&self, _rows: &[String]) -> Option<String> {
        None
    }
    /// Whether the screen shows only the initial launch card with no turn yet.
    fn fresh(&self, _rows: &[String]) -> bool {
        false
    }
    /// Whether the screen shows the opening exchange whole: the launch card, then one user message.
    fn opening(&self, _rows: &[String]) -> bool {
        false
    }
    fn composer(&self, _rows: &[String]) -> Option<(usize, Vec<String>)> {
        None
    }
    /// Whether the program's prompt-history search is open over its composer on `rows`: keys then
    /// type into the search's query and Enter sends the old prompt it matched, so no row of the
    /// composer is a person's draft.
    fn history_search(&self, _rows: &[String]) -> bool {
        false
    }
    /// Row index where the transcript ends and the live zone begins.
    fn transcript_end(&self, rows: &[String]) -> usize {
        rows.iter()
            .rposition(|r| !crate::text::trim(r).is_empty())
            .map_or(0, |i| i + 1)
    }
    /// The screen's rows as this program's transcript readers read them
    /// ([`crate::phase::Transcript`]): Claude Code's, each row above its composer's top rule cut
    /// at its side panel's rule; the default, `rows` whole, for a program whose side panel no
    /// reader here reads.
    fn transcript<'a>(&self, rows: &'a [String]) -> crate::phase::Transcript<'a> {
        crate::phase::Transcript::whole(rows)
    }
    /// Usage-credits notice shown on `rows`, if any.
    fn credits(&self, _rows: &[String]) -> Option<crate::credits::Credits> {
        None
    }
    /// Usage-window warning shown on `rows`, if any.
    fn usage_warning(&self, _rows: &[String]) -> Option<crate::credits::Warning> {
        None
    }
    /// A model's week shown on `rows` (its wall or its warning), if any.
    fn model_week(&self, _rows: &[String]) -> Option<crate::credits::ModelWeek> {
        None
    }
    /// Leading glyph of the composer's caret row, if the program has a composer.
    fn caret(&self) -> Option<char> {
        None
    }
    /// Column where [`Self::caret`] is drawn.
    fn caret_column(&self) -> usize {
        0
    }
    /// Leading glyph of the composer's caret row on `rows`.
    fn caret_on(&self, _rows: &[String]) -> Option<char> {
        self.caret()
    }
    /// Whether an empty composer still displays dim placeholder text on its caret row.
    fn placeholder(&self) -> bool {
        false
    }
    /// Foreground hex color (`rrggbb`) of placeholder text when not marked `dim`.
    fn placeholder_fg(&self) -> Option<&'static str> {
        None
    }
    /// Whether an Enter arriving inside a burst of typed text is treated as a newline.
    fn paste_guard(&self) -> bool {
        false
    }
    /// Chip text drawn by the composer when `text` is pasted as a single block, if folded.
    fn folded_paste(&self, _text: &str) -> Option<String> {
        None
    }
    /// Wire regex matching only the busy row of a running turn.
    fn busy_guard(&self) -> Option<String> {
        None
    }
    /// Whether `rows` show a composer frame caught mid-redraw.
    fn frame_torn(&self, rows: &[String]) -> bool {
        crate::phase::composer_frame_torn(rows)
    }
    /// Whether the row [`Self::busy_guard`] matches stays visible while an approval box is open.
    fn busy_row_outlives_boxes(&self) -> bool {
        false
    }
    /// First row index where a running turn's busy row can appear.
    fn busy_zone_start(&self, _rows: &[String]) -> usize {
        0
    }
    /// Whether `phase` read from `rows` is authoritative evidence.
    fn phase_authoritative(&self, _rows: &[String], _phase: &Phase) -> bool {
        false
    }
    /// What the program's own window title says of its run: `Some(true)` running, `Some(false)`
    /// at rest, `None` where the reader has no reading of the program's titles (the default).
    fn title_runs(&self, _title: &str) -> Option<bool> {
        None
    }
    /// Whether `rows` draw the program's own mark that its turn ended, in a frame that holds all
    /// of its reply: an end that needs no title to vouch for it.
    fn end_drawn(&self, _rows: &[String]) -> bool {
        false
    }
    /// What `title`, the program's window title read with the screen, says of its run
    /// ([`Self::title_runs`]): `None` where there is no title (a host that sends none, or a tape
    /// recorded from one) or it is empty (one the program never set), which say nothing.
    fn runs(&self, title: Option<&str>) -> Option<bool> {
        title
            .filter(|t| !t.is_empty())
            .and_then(|t| self.title_runs(t))
    }
    /// Whether `rows` show work the agent handed off running, which runs its title with its turn
    /// over. The default shows none; Claude Code's is [`crate::phase::delegated_shown`].
    fn delegated_shown(&self, _rows: &[String]) -> bool {
        false
    }
    /// Whether the title says a turn runs that `rows` do not show: `runs` is `Some(true)` where
    /// the rows alone read Idle or a Question ([`Self::phase`]), with no end drawn
    /// ([`Self::end_drawn`]), no wall and no handed-off work shown ([`Self::delegated_shown`]).
    /// Claude Code's is [`crate::phase::title_live`].
    fn title_live(&self, rows: &[String], runs: Option<bool>) -> bool {
        runs == Some(true)
            && matches!(self.phase(rows), Phase::Idle | Phase::Question)
            && !self.end_drawn(rows)
            && self.wall(rows).is_none()
            && !self.delegated_shown(rows)
    }
    /// [`Self::phase`] read beside the title (`runs`, [`Self::runs`]): an Idle or a Question is
    /// Busy where [`Self::title_live`]. A box, a limit, a wall and every Busy stand as read.
    fn phase_seen(&self, rows: &[String], runs: Option<bool>) -> Phase {
        let phase = self.phase(rows);
        if matches!(phase, Phase::Idle | Phase::Question) && self.title_live(rows, runs) {
            Phase::Busy
        } else {
            phase
        }
    }
    /// [`Self::background_wait`] read beside the title. The default is the screen's alone;
    /// Claude Code's reads no break where its title runs a turn
    /// ([`crate::phase::background_wait_seen`]).
    fn background_wait_seen(&self, rows: &[String], _runs: Option<bool>) -> Option<&'static str> {
        self.background_wait(rows)
    }
    /// [`Self::background_rest`] read beside the title. The default is the screen's alone;
    /// Claude Code's reads no rest where its title runs a turn
    /// ([`crate::phase::background_rest_seen`]).
    fn background_rest_seen(&self, rows: &[String], _runs: Option<bool>) -> Option<Phase> {
        self.background_rest(rows)
    }
    /// [`Self::read`] with its phase read beside the title ([`Self::phase_seen`]). A turn the
    /// title holds is a hard busy, so the reading carries only a health wall, as
    /// [`Self::read`] does under a live turn.
    fn read_titled(
        &self,
        rows: &[String],
        cursor_col: Option<usize>,
        runs: Option<bool>,
    ) -> Reading {
        let phase = self.phase_seen(rows, runs);
        let held = phase == Phase::Busy && self.title_live(rows, runs);
        compose(self, rows, cursor_col, phase, held)
    }
    /// [`Self::read_at`] read beside the title: [`Self::read_titled`], then
    /// [`Self::cursor_vouches`].
    fn read_seen(
        &self,
        rows: &[String],
        cursor_row: Option<usize>,
        cursor_col: Option<usize>,
        runs: Option<bool>,
    ) -> Reading {
        let mut reading = self.read_titled(rows, cursor_col, runs);
        reading.phase_authoritative &= self.cursor_vouches(rows, &reading, cursor_row);
        reading
    }
    /// Whether cursor position `cursor_row` vouches for `reading`.
    fn cursor_vouches(
        &self,
        _rows: &[String],
        _reading: &Reading,
        _cursor_row: Option<usize>,
    ) -> bool {
        true
    }

    /// Read `rows` at `(cursor_row, cursor_col)` and apply [`Self::cursor_vouches`].
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

    /// Read `rows` at `cursor_col`.
    fn read(&self, rows: &[String], cursor_col: Option<usize>) -> Reading {
        compose(self, rows, cursor_col, self.phase(rows), false)
    }
}

/// The [`Reading`] of `rows` in `phase`: [`ScreenReader::read`]'s, and
/// [`ScreenReader::read_titled`]'s where `title_busy` says the title holds a turn the screen does
/// not show, a hard busy.
fn compose<R: ScreenReader + ?Sized>(
    reader: &R,
    rows: &[String],
    cursor_col: Option<usize>,
    phase: Phase,
    title_busy: bool,
) -> Reading {
    let phase_authoritative = reader.phase_authoritative(rows, &phase);
    let prompt = (phase == Phase::Prompt)
        .then(|| reader.prompt(rows))
        .flatten();
    let hard_busy =
        phase == Phase::Busy && (title_busy || busy_signal(rows).is_some_and(|b| !b.soft));
    let wall = if phase == Phase::Prompt {
        None
    } else if hard_busy {
        reader.health_wall(rows)
    } else {
        reader.wall(rows)
    };
    Reading {
        program: reader.program(),
        phase,
        phase_authoritative,
        wall,
        prompt,
        survey: reader.survey(rows),
        context_left: reader.context_shown(rows),
        suggestion: cursor_col.and_then(|c| reader.suggestion(rows, c)),
        goal_active: reader.goal_active(rows),
        said_tail: reader.said_tail(rows),
        interrupted: reader.interrupted(rows),
        fresh: reader.fresh(rows),
        composer: reader.composer(rows),
        credits: reader.credits(rows),
    }
}

/// Screen reader for Claude Code.
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
    fn background_rest(&self, rows: &[String]) -> Option<Phase> {
        crate::phase::background_rest(rows)
    }
    fn work_left(&self, rows: &[String]) -> Option<&'static str> {
        crate::phase::work_left(rows)
    }
    fn work_running(&self, rows: &[String]) -> Option<&'static str> {
        crate::phase::work_running(rows)
    }
    fn context(&self, rows: &[String]) -> Option<u8> {
        context_left(rows)
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
    fn turn_messages_before(&self, rows: &[String]) -> Option<Vec<String>> {
        crate::turn::turn_messages_before(rows)
    }
    fn said_lines(&self, rows: &[String], apart: Apart) -> Option<String> {
        crate::turn::said_lines(rows, apart)
    }
    fn turn_lines_before(&self, rows: &[String], apart: Apart) -> Option<String> {
        crate::turn::turn_lines_before(rows, apart)
    }
    fn turn_message_lines_before(&self, rows: &[String], apart: Apart) -> Option<Vec<String>> {
        crate::turn::turn_message_lines_before(rows, apart)
    }
    fn turn_whole(&self, rows: &[String]) -> bool {
        crate::turn::turn_whole(rows)
    }
    fn turn_ran_tools(&self, rows: &[String]) -> bool {
        crate::turn::turn_ran_tools(rows)
    }
    fn reads_tool_rows(&self) -> bool {
        true
    }
    fn turn_opens_above(&self, rows: &[String]) -> bool {
        crate::turn::turn_opens_above(rows)
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
    fn opening(&self, rows: &[String]) -> bool {
        crate::turn::opening(rows)
    }
    fn message_drawn(&self, rows: &[String]) -> bool {
        crate::turn::message_drawn(rows)
    }
    /// Its composer's frame, a box or its sign-in dialog, or its launch card.
    fn program_drawn(&self, rows: &[String]) -> bool {
        has_composer_frame(rows)
            || parse_prompt_v2(rows).is_some()
            || names_login(rows)
            || crate::turn::launch_card_scan(rows).0.is_some()
    }
    /// Inside its frame only: a `❯` row with no frame around it is a user's message, and
    /// a box replaces the frame.
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        has_composer_frame(rows)
            .then(|| composer_draft(rows))
            .flatten()
    }
    fn history_search(&self, rows: &[String]) -> bool {
        crate::phase::history_search(rows)
    }
    fn transcript_end(&self, rows: &[String]) -> usize {
        crate::phase::transcript_end(rows)
    }
    fn transcript<'a>(&self, rows: &'a [String]) -> crate::phase::Transcript<'a> {
        crate::phase::Transcript::of(rows)
    }
    fn credits(&self, rows: &[String]) -> Option<crate::credits::Credits> {
        crate::credits::in_use(rows)
    }
    fn usage_warning(&self, rows: &[String]) -> Option<crate::credits::Warning> {
        crate::credits::usage_warning(rows)
    }
    fn model_week(&self, rows: &[String]) -> Option<crate::credits::ModelWeek> {
        crate::credits::model_week(rows)
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
        Some(guard_regex(const { crate::anchor!("busy.interrupt") }))
    }
    fn busy_zone_start(&self, rows: &[String]) -> usize {
        crate::phase::busy_zone_start(rows)
    }
    /// Every reading but one: Claude Code's `idle` ("no box, no spinner, no question") is
    /// evidence only at its composer (the frame drawn, or shell mode's `!`) or on a wall,
    /// since a screen with no REPL on it says the same: the shell's rows between launch and
    /// REPL, and the alternate screen's first half-drawn frame.
    fn phase_authoritative(&self, rows: &[String], phase: &Phase) -> bool {
        *phase != Phase::Idle
            || has_composer_frame(rows)
            || has_shell_mode_frame(rows)
            || wall(rows).is_some()
    }
    /// Its title's lead glyph ([`crate::phase::title_runs`]). Under a status line a reply still
    /// being drawn shows no busy sign on the screen: the footer has no `esc to interrupt`, and
    /// the spinner row is gone once the reply's first line is drawn. Every other title says it
    /// is not running; an empty one is a title never set, which the harness takes as saying
    /// nothing.
    fn title_runs(&self, title: &str) -> Option<bool> {
        Some(crate::phase::title_runs(title))
    }
    /// Its done row under the reply ([`crate::phase::done_row_drawn`]).
    fn end_drawn(&self, rows: &[String]) -> bool {
        crate::phase::done_row_drawn(rows)
    }
    fn delegated_shown(&self, rows: &[String]) -> bool {
        crate::phase::delegated_shown(rows)
    }
    fn title_live(&self, rows: &[String], runs: Option<bool>) -> bool {
        crate::phase::title_live(rows, runs)
    }
    fn phase_seen(&self, rows: &[String], runs: Option<bool>) -> Phase {
        crate::phase::worker_phase_seen(rows, runs)
    }
    fn background_wait_seen(&self, rows: &[String], runs: Option<bool>) -> Option<&'static str> {
        crate::phase::background_wait_seen(rows, runs)
    }
    fn background_rest_seen(&self, rows: &[String], runs: Option<bool>) -> Option<Phase> {
        crate::phase::background_rest_seen(rows, runs)
    }
    /// Claude Code's `idle` at a prompt box is evidence only where that box holds the
    /// cursor ([`crate::phase::prompt_box_holds`]); a wall and every other phase stand as
    /// read.
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

/// Identity-only reader for Codex (screen reading retired).
#[derive(Debug, Clone, Copy)]
pub struct CodexReader;

impl ScreenReader for CodexReader {
    fn program(&self) -> Program {
        Program::Codex
    }
    /// No evidence: every reading is a non-authoritative `idle`, so the server publishes
    /// `agent=unknown`, and `aterm drive phase` and `answer` read nothing on a Codex.
    fn phase(&self, _rows: &[String]) -> Phase {
        Phase::Idle
    }
}

/// Screen reader for Google Antigravity CLI (`agy`).
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
    fn turn_messages_before(&self, rows: &[String]) -> Option<Vec<String>> {
        agy::turn_messages_before(rows)
    }
    fn said_lines(&self, rows: &[String], apart: Apart) -> Option<String> {
        agy::said_lines(rows, apart)
    }
    fn turn_lines_before(&self, rows: &[String], apart: Apart) -> Option<String> {
        agy::turn_lines_before(rows, apart)
    }
    fn turn_message_lines_before(&self, rows: &[String], apart: Apart) -> Option<Vec<String>> {
        agy::turn_message_lines_before(rows, apart)
    }
    fn turn_whole(&self, rows: &[String]) -> bool {
        agy::turn_whole(rows)
    }
    fn turn_ran_tools(&self, rows: &[String]) -> bool {
        agy::turn_ran_tools(rows)
    }
    fn reads_tool_rows(&self) -> bool {
        true
    }
    fn turn_opens_above(&self, rows: &[String]) -> bool {
        agy::turn_opens_above(rows)
    }
    fn turn_history_stop(&self, row: &str) -> bool {
        agy::turn_history_stop(row)
    }
    fn turn_opened_by(&self, rows: &[String]) -> Option<String> {
        agy::turn_opened_by(rows)
    }
    /// agy draws a message's blank line as a blank row and the message's rows under it two
    /// columns in, as it draws a reply's: its text cannot bound a message, its cells can
    /// ([`agy::message_blanks`]).
    fn bounds_messages(&self) -> bool {
        false
    }
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
    fn opening(&self, rows: &[String]) -> bool {
        agy::opening(rows)
    }
    fn message_drawn(&self, rows: &[String]) -> bool {
        agy::message_drawn(rows)
    }
    fn program_drawn(&self, rows: &[String]) -> bool {
        agy::program_drawn(rows)
    }
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        agy::composer_draft(rows)
    }
    fn transcript_end(&self, rows: &[String]) -> usize {
        agy::transcript_end(rows)
    }
    fn caret(&self) -> Option<char> {
        Some('>')
    }
    /// None: agy's empty composer draws nothing after its caret but a permission mode's hint
    /// (whole or cut with `...`), which [`agy::composer_draft`] reads as the empty draft.
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

/// Screen reader for Muse Code.
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
    fn turn_messages_before(&self, rows: &[String]) -> Option<Vec<String>> {
        muse::turn_messages_before(rows)
    }
    fn said_lines(&self, rows: &[String], apart: Apart) -> Option<String> {
        muse::said_lines(rows, apart)
    }
    fn turn_lines_before(&self, rows: &[String], apart: Apart) -> Option<String> {
        muse::turn_lines_before(rows, apart)
    }
    fn turn_message_lines_before(&self, rows: &[String], apart: Apart) -> Option<Vec<String>> {
        muse::turn_message_lines_before(rows, apart)
    }
    fn turn_whole(&self, rows: &[String]) -> bool {
        muse::turn_whole(rows)
    }
    fn turn_ran_tools(&self, rows: &[String]) -> bool {
        muse::turn_ran_tools(rows)
    }
    fn reads_tool_rows(&self) -> bool {
        true
    }
    fn turn_opens_above(&self, rows: &[String]) -> bool {
        muse::turn_opens_above(rows)
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
    fn stopped_turn(&self, rows: &[String]) -> Option<String> {
        muse::stopped_turn(rows)
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
    fn opening(&self, rows: &[String]) -> bool {
        muse::opening(rows)
    }
    fn message_drawn(&self, rows: &[String]) -> bool {
        muse::message_drawn(rows)
    }
    fn program_drawn(&self, rows: &[String]) -> bool {
        muse::program_drawn(rows)
    }
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        muse::composer_draft(rows)
    }
    fn history_search(&self, rows: &[String]) -> bool {
        muse::history_search(rows)
    }
    fn caret(&self) -> Option<char> {
        Some('❯')
    }
    /// The idle composer's hint (one contextual tip, `❯ /export saves the conversation to a text
    /// file`, the cursor at column 2, in dim grey with no `dim` attribute) is no draft: the
    /// cursor's column and the cell's colour tell it from one.
    fn placeholder(&self) -> bool {
        true
    }
    fn placeholder_fg(&self) -> Option<&'static str> {
        Some("676c74")
    }
    /// An Enter in the same burst as the text is taken as a newline (measured on 1.4.9 and
    /// 1.4.10).
    fn paste_guard(&self) -> bool {
        true
    }
    /// The status row's tail, `esc to interrupt)` (`◆ Thinking (2s · esc to
    /// interrupt)`).
    fn busy_guard(&self) -> Option<String> {
        Some(format!(
            "{}\\)",
            guard_regex(const { crate::anchor!("muse.busy.interrupt") })
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
    fn title_runs(&self, title: &str) -> Option<bool> {
        Some(muse::title_runs(title))
    }
}

/// Screen reader for Grok Build ([`crate::grok`]), for display: its phase, its walls and the
/// boxes that wait on the person ([`grok::prompt`]), which nothing here answers.
#[derive(Debug, Clone, Copy)]
pub struct GrokReader;

impl ScreenReader for GrokReader {
    fn program(&self) -> Program {
        Program::Grok
    }
    /// Busy or idle by the rows around its composer's box ([`grok::phase`]; the box is read for
    /// the point cut too, [`grok::composer_rows`], never for what is typed in it); a prompt for a
    /// plan approval whose key row offers `a:approve`, or a card a running turn draws where no
    /// box is ([`grok::prompt`]).
    fn phase(&self, rows: &[String]) -> Phase {
        grok::phase(rows).0
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        grok::prompt(rows)
    }
    /// Its wall ([`grok::wall`]), none while a turn runs or waits on a box: a wall row such a
    /// frame still shows in its live zone is an earlier turn's.
    fn wall(&self, rows: &[String]) -> Option<Wall> {
        if matches!(grok::phase(rows).0, Phase::Busy | Phase::Prompt) {
            return None;
        }
        grok::wall(rows)
    }
    fn phase_authoritative(&self, rows: &[String], _: &Phase) -> bool {
        grok::phase(rows).1
    }
    /// The context its header's token counts leave ([`grok::context_left`]), for the window's
    /// displays: the header names no compaction, so the watch does not take it.
    fn context_shown(&self, rows: &[String]) -> Option<u8> {
        grok::context_left(rows)
    }
}

/// Screen reader for Kimi Code ([`crate::kimi`]): its phase, prompts, composer and walls.
#[derive(Debug, Clone, Copy)]
pub struct KimiReader;

impl ScreenReader for KimiReader {
    fn program(&self) -> Program {
        Program::Kimi
    }
    /// A card by [`kimi::prompt`], else busy or idle by the rows over its composer.
    fn phase(&self, rows: &[String]) -> Phase {
        kimi::phase(rows).0
    }
    fn phase_authoritative(&self, rows: &[String], _: &Phase) -> bool {
        kimi::phase(rows).1
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        kimi::prompt(rows)
    }
    fn esc(&self, prompt: &PromptV2) -> Option<CancelEffect> {
        kimi::esc(prompt)
    }
    fn wall(&self, rows: &[String]) -> Option<Wall> {
        kimi::wall(rows)
    }
    fn said_tail(&self, rows: &[String]) -> Option<String> {
        kimi::said_tail(rows)
    }
    fn said_whole(&self, rows: &[String]) -> bool {
        kimi::said_whole(rows)
    }
    fn turn_said_before(&self, rows: &[String]) -> Option<String> {
        kimi::turn_said_before(rows)
    }
    fn turn_messages_before(&self, rows: &[String]) -> Option<Vec<String>> {
        kimi::turn_messages_before(rows)
    }
    fn turn_ran_tools(&self, rows: &[String]) -> bool {
        kimi::turn_ran_tools(rows)
    }
    fn reads_tool_rows(&self) -> bool {
        true
    }
    fn turn_opens_above(&self, rows: &[String]) -> bool {
        kimi::turn_opens_above(rows)
    }
    fn turn_opened_by(&self, rows: &[String]) -> Option<String> {
        kimi::turn_opened_by(rows)
    }
    fn interrupted(&self, rows: &[String]) -> bool {
        kimi::interrupted(rows)
    }
    fn fresh(&self, rows: &[String]) -> bool {
        kimi::fresh(rows)
    }
    fn opening(&self, rows: &[String]) -> bool {
        kimi::opening(rows)
    }
    fn message_drawn(&self, rows: &[String]) -> bool {
        kimi::message_drawn(rows)
    }
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        kimi::composer(rows)
    }
    /// What its footer's `context: N%` leaves ([`kimi::context_left`]).
    fn context_shown(&self, rows: &[String]) -> Option<u8> {
        kimi::context_left(rows)
    }
}

/// Fallback reader for unknown or non-agent programs (always `idle`, non-authoritative).
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

/// The names of Google's Gemini CLI, which aterm no longer supports: agy replaces it. A program
/// so named is no agent to any reader ([`identify`]) and no lane's ([`program_of`] names none),
/// and a spawn, config table or record naming it is refused with [`RETIRED_AGENT`].
pub const RETIRED_AGENTS: &[&str] = &["gemini", "gemini-cli"];

/// Why a name in [`RETIRED_AGENTS`] is refused, for the refusal a person reads.
pub const RETIRED_AGENT: &str = "Gemini CLI is no longer supported: agy replaces it (agent=agy)";

/// Whether `program` (a command line, a path or a bare word) names a retired agent
/// ([`RETIRED_AGENTS`]).
#[must_use]
pub fn retired_agent(program: &str) -> bool {
    let word = program_word(program);
    RETIRED_AGENTS
        .iter()
        .any(|retired| word.eq_ignore_ascii_case(retired))
}

/// Select the [`ScreenReader`] for `program` and `rows`.
#[must_use]
pub fn identify(program: Option<&str>, rows: &[String]) -> &'static dyn ScreenReader {
    if let Some(named) = program.and_then(program_of) {
        return named.reader();
    }
    if program.is_some_and(|p| {
        retired_agent(p)
            || NOT_AGENTS
                .iter()
                .any(|not| program_word(p).eq_ignore_ascii_case(not))
    }) {
        return &GenericReader;
    }
    let names_claude = |p: PromptV2| p.kind != PromptKind::Other || p.footer.is_some();
    // Claude Code's own frames (its composer under a rule at the screen's left edge, its boxes,
    // its `/login` dialog) are read before Kimi Code's and Grok Build's sniffs: those match a
    // welcome row, a sign-in screen or a composer box anywhere on the screen, which a Claude
    // Code transcript shows wherever its tool output or its reply quotes the other CLI's screen.
    if muse::is_muse_screen(rows) {
        &MuseReader
    } else if agy::is_agy_screen(rows) {
        &AgyReader
    } else if has_composer_frame(rows)
        || parse_prompt_v2(rows).is_some_and(names_claude)
        || names_login(rows)
    {
        &ClaudeReader
    } else if kimi::is_kimi_screen(rows) {
        &KimiReader
    } else if grok::is_grok_screen(rows) {
        &GrokReader
    } else {
        &GenericReader
    }
}

/// Claude Code's `/login` dialog in any state its own words name but the failure
/// ([`crate::login::login_screen`]): drawn in the composer's place, so no frame names the program
/// while it stands.
fn names_login(rows: &[String]) -> bool {
    crate::login::login_screen(rows).is_some_and(|s| !matches!(s, LoginScreen::Failed))
}

/// Identify the reader for `(program, rows)` and run [`ScreenReader::read`].
#[must_use]
pub fn read(program: Option<&str>, rows: &[String], cursor_col: Option<usize>) -> Reading {
    identify(program, rows).read(rows, cursor_col)
}

/// Identify the reader for `(program, rows)` and run [`ScreenReader::read_at`].
#[must_use]
pub fn read_at(
    program: Option<&str>,
    rows: &[String],
    cursor_row: Option<usize>,
    cursor_col: Option<usize>,
) -> Reading {
    identify(program, rows).read_at(rows, cursor_row, cursor_col)
}

/// Identify the reader for `(program, rows)` and run [`ScreenReader::read_seen`] with what
/// `title`, the program's window title read with `rows`, says of its run
/// ([`ScreenReader::runs`]).
#[must_use]
pub fn read_seen(
    program: Option<&str>,
    rows: &[String],
    cursor_row: Option<usize>,
    cursor_col: Option<usize>,
    title: Option<&str>,
) -> Reading {
    let reader = identify(program, rows);
    reader.read_seen(rows, cursor_row, cursor_col, reader.runs(title))
}

/// Where the live zone of `n` rows starts: `n` rows up from the last drawn row, a run of
/// [`BLANK_GAP`] or more blank rows counting as one.
#[must_use]
pub fn live_zone_start(rows: &[String], n: usize) -> usize {
    zone_walk(rows, n).0
}

/// Whether a live zone of `n` rows ([`live_zone_start`]) reaches above `tail`, the screen's last
/// rows: `tail` holds `n` rows or more, is drawn to its last, and runs out before the walk does.
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

/// Map a program command string or shell-wrapped command to its [`Program`], if known: the
/// agent the name table ([`agent_of`]) names.
#[must_use]
pub fn program_of(program: &str) -> Option<Program> {
    agent_of(program).map(Program::of)
}

/// Whether `rows` could be the shell's alone, before a program drew: every row blank, or the
/// non-blank rows one unbroken run (a prompt and the line it echoed).
#[must_use]
pub fn shell_alone(rows: &[String]) -> bool {
    let drawn = |row: &String| !row.trim().is_empty();
    let Some(first) = rows.iter().position(drawn) else {
        return true;
    };
    let last = rows.iter().rposition(drawn).unwrap_or(first);
    rows[first..=last].iter().all(drawn)
}

#[cfg(test)]
#[path = "title_tests.rs"]
mod title_tests;

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
