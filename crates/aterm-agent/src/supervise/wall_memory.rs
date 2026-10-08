// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wall the loop waits on is REMEMBERED.
//!
//! A limit's wait is hours long, and the wall is read off the screen: the
//! notice must be the last thing said above the composer. Two things the owner
//! does at a wall change that without ending it — a scroll back through the
//! transcript (Claude Code 2.1.28x's fullscreen view scrolls), and a local
//! command (`/model`, `/usage`: the first thing the owner did at the last two
//! walls), whose `❯ /model` and `⎿` result rows are said after the wall. The
//! screen then holds only the agent's own background work (a shell in the
//! footer, the done row counting it), reads `Busy` with no wall, and the
//! standing wait is dropped: at the reset nothing is typed, or ever (measured
//! before the wall was remembered: `WAITING until=00:09:01`, the wall's rows leave, 652 s
//! later no turn, a timeout). A shell-less tab reads `Idle` in the same state
//! and is continued at once by the policy's "the screen left a limit" rule;
//! only a tab with background work — the owner's dominant one — was blind. An
//! update at that moment lost the wait for good: the successor had nothing to
//! remember.
//!
//! What is kept is the wall the loop last read and waited on — a limit that
//! resets (`reads_limited`: a usage window, a spend limit, a model bucket, a
//! 429) — in memory and, for a loop that follows it, as a ledger row
//! ([`approvals::RULE_WALL_WAIT`]). While the screen shows nothing but the
//! agent's own background work ([`aterm_phase::background_only`]: no
//! spinner, no `esc to interrupt`, no message just submitted), the memory
//! stands for the wall the screen no longer shows; the standing `WaitUntil`
//! then fires at the reset and its grace as it would have with the rows
//! showing. A live turn (the worker worked: the owner typed, the vendor's own
//! resume), an idle screen with no wall (the policy's own rule takes over), a
//! question and another wall end it — the ledger row for the end
//! ([`approvals::RULE_WALL_WAIT_END`]) says so to the next loop; so does any
//! act the policy typed since ([`approvals::open_wall`]).
//!
//! Nothing is typed by this module: it decides what a screen READS as. The
//! act, its grace, a person's hold, the draft and the fenced write are the
//! policy's and the loop's, as at any wall.

use super::super::policy::outlook::ExceptionKind;
use super::approvals::{self, Outcome, RULE_WALL_WAIT, RULE_WALL_WAIT_END};
use super::*;

/// How long past its reset a wall stays remembered when nothing has ended it:
/// the ladder that follows the reset (10 minutes, then 30, for ever) and the
/// policy's `Futile` exception judge a wall that outlasts its reset by hours
/// (`exception.rs`); a memory older than this is history.
pub(super) const MEMORY_TTL: Duration = Duration::from_secs(24 * 3600);

/// How long a screen may read busy on the agent's own background work alone,
/// with the agent's OWN title saying idle, before the loop says so in its
/// journal ([`Session::stuck_busy_note`]). The ladder's unit: its second rung
/// is three of these and its third six.
pub(super) const STUCK_AFTER: Duration = Duration::from_secs(30 * 60);

/// How many times shorter than [`STUCK_AFTER`] the unit of the dialog flavour's
/// ladder is ([`Session::dialog_note`]): nothing runs on its screen (no
/// spinner, no busy row, a title at rest, words that stand still), so no long
/// background job can explain it, and the journal line comes at 10 minutes, the
/// host's record at 30 and the digest's line at an hour.
const DIALOG_DIVISOR: u32 = 3;

/// The glyph Claude Code puts before its terminal title while it is IDLE
/// (`✳ Claude Code`); a working agent animates a spinner glyph there instead.
const IDLE_TITLE_GLYPH: char = '\u{2733}';

/// The wall being waited on, and what was last written of it to the ledger.
#[derive(Debug, Clone)]
pub(super) struct WallMemory {
    pub(super) wall: aterm_phase::Wall,
    /// The reset placement last written to the ledger: `None` before the
    /// first write, then `Some(the placement)`.
    saved: Option<Option<i64>>,
}

/// A wall the memory is kept for: a limit that resets, whose wait the loop
/// owns. An API error's ladder rereads the screen at every rung, a login and
/// a full context are acted on and left, a memory banner is no turn's end.
fn is_kept(wall: &aterm_phase::Wall) -> bool {
    wall.kind.reads_limited()
}

impl<C: Ctl> Session<'_, C> {
    /// The remembered wall, when `rows` show nothing but the agent's own
    /// background work — Claude Code's screen, its composer frame drawn, no
    /// live turn and no message just submitted.
    pub(super) fn remembered_wall(
        &self,
        reader: &dyn ScreenReader,
        rows: &[String],
    ) -> Option<aterm_phase::Wall> {
        let memory = self.wall_memory.as_ref()?;
        (reader.program() == aterm_phase::Program::Claude && aterm_phase::background_only(rows))
            .then(|| memory.wall.clone())
    }

    /// `rows` read `Busy`: a LIVE turn on a screen with a composer (a spinner,
    /// `esc to interrupt`, a message just submitted) ends the memory. A busy
    /// read that is only background work, or a screen with no composer frame
    /// (a dialog covering it), leaves it.
    pub(super) fn end_memory_on_live_turn(
        &mut self,
        reader: &dyn ScreenReader,
        rows: &[String],
        seq: u64,
    ) {
        if self.wall_memory.is_some()
            && reader.composer(rows).is_some()
            && !aterm_phase::background_only(rows)
        {
            self.forget_wall(seq);
        }
    }

    /// The reading of a POINT, folded into the memory (called from
    /// [`Self::turn_end_reading`]): a wall that resets is remembered; another
    /// wall, an idle screen with no wall and a question end it; a busy screen
    /// with only background work is read as the remembered wall.
    pub(super) fn wall_memory_step(
        &mut self,
        reader: &dyn ScreenReader,
        rows: &[String],
        reading: &mut aterm_phase::Reading,
        seq: u64,
    ) {
        match reading.wall.clone() {
            Some(w) if is_kept(&w) => self.remember_wall(&w),
            Some(_) => self.forget_wall(seq),
            None => match reading.phase {
                aterm_phase::Phase::Busy => {
                    if let Some(w) = self.remembered_wall(reader, rows) {
                        reading.phase = aterm_phase::Phase::Limited {
                            message: w.message.clone(),
                            reset: w.reset.clone(),
                        };
                        reading.wall = Some(w);
                    } else {
                        self.end_memory_on_live_turn(reader, rows, seq);
                    }
                }
                aterm_phase::Phase::Idle | aterm_phase::Phase::Question => self.forget_wall(seq),
                aterm_phase::Phase::Prompt | aterm_phase::Phase::Limited { .. } => {}
            },
        }
    }

    /// Remember `wall`: the same wall read again (its rows shift, its footer
    /// ticks) changes nothing.
    fn remember_wall(&mut self, wall: &aterm_phase::Wall) {
        match &mut self.wall_memory {
            Some(m)
                if m.wall.kind == wall.kind
                    && m.wall.message == wall.message
                    && m.wall.reset == wall.reset => {}
            slot => {
                *slot = Some(WallMemory {
                    wall: wall.clone(),
                    saved: None,
                });
            }
        }
    }

    /// Forget the wall: the ledger says so to a loop that follows this one.
    pub(super) fn forget_wall(&mut self, seq: u64) {
        if let Some(m) = self.wall_memory.take()
            && m.saved.is_some()
        {
            self.ledger_row(
                RULE_WALL_WAIT_END,
                Outcome::Waiting,
                "",
                "the wall left the screen or the worker worked",
                seq,
            );
        }
    }

    /// Write the remembered wall to the ledger when it, or the reset the loop
    /// placed for it, is not what was last written. After the reset is placed
    /// ([`Self::wall_reset_at`]).
    pub(super) fn persist_wall(&mut self, seq: u64) {
        let unix = self.wall_reset.as_ref().and_then(|(_, u)| *u);
        let Some(m) = self.wall_memory.as_mut() else {
            return;
        };
        if m.saved == Some(unix) {
            return;
        }
        m.saved = Some(unix);
        let reason = approvals::wall_wait_reason(unix, m.wall.reset.as_deref(), &m.wall.message);
        self.ledger_row(RULE_WALL_WAIT, Outcome::Waiting, "", &reason, seq);
    }

    /// A loop that follows another on the session goes on waiting for the wall
    /// its predecessor waited on ([`approvals::open_wall`]) when the screen
    /// shows none of its rows — the update that applied while the owner's
    /// scroll hid it. Kept for [`MEMORY_TTL`] past its reset (a wall with no
    /// placed reset, past its row).
    pub(super) fn seed_wall_memory(&mut self, open: Option<approvals::OpenWall>) {
        if self.wall_memory.is_some() {
            return;
        }
        let Some(open) = open else {
            return;
        };
        let now = self.now_unix();
        let ttl = i64::try_from(MEMORY_TTL.as_secs()).unwrap_or(i64::MAX);
        let since = open.reset_unix.unwrap_or(open.at_ms / 1000);
        if now > since.saturating_add(ttl) {
            return;
        }
        let Some(kind) = aterm_phase::classify_wall(&open.message).filter(|k| k.reads_limited())
        else {
            return;
        };
        let key = format!("{}|{}", kind.name(), open.reset.as_deref().unwrap_or("-"));
        self.wall_reset = Some((key, open.reset_unix));
        self.wall_memory = Some(WallMemory {
            wall: aterm_phase::Wall {
                kind,
                message: open.message,
                reset: open.reset,
                row: 0,
                placement: aterm_phase::Placement::Gutter,
            },
            saved: Some(open.reset_unix),
        });
    }
}

/// How far up the canary's ladder a stall has climbed
/// ([`Session::stuck_busy_note`], [`Session::dialog_note`]): each rung is a
/// multiple of [`STUCK_AFTER`] (a third of it for a dialog) of the screen
/// reading as it does under an idle title, and each says more than the last.
/// The FIRST is the journal alone (30 minutes; 10 for a dialog); the SECOND is
/// the host's record and error line (90 minutes; 30: a wait or a dialog this
/// build does not read, aterm's own step stuck); the THIRD is a line of the
/// owner's digest (3 hours; an hour: nothing here explains it, and only a
/// person can look).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum StuckRung {
    /// Nothing said.
    Quiet,
    /// `STUCK` is in the journal.
    Said,
    /// `STALLED` is in the journal and the host has been told.
    Told,
    /// The exception is up.
    Raised,
}

impl StuckRung {
    /// The rung above this one and the multiple of the ladder's unit it needs
    /// ([`STUCK_AFTER`]; a third of it for a dialog), or `None` at the top.
    fn next(self) -> Option<(Self, u32)> {
        match self {
            Self::Quiet => Some((Self::Said, 1)),
            Self::Said => Some((Self::Told, 3)),
            Self::Told => Some((Self::Raised, 6)),
            Self::Raised => None,
        }
    }
}

impl<C: Ctl> Session<'_, C> {
    /// The CANARY for a screen no reader names (the review of
    /// 2026-09-29): the loop reads a screen by its words, and a build that
    /// words a wall or a turn's end differently reads `busy` for as long as a
    /// shell runs: the 4 h 44 m of 2026-09-28, silently, again. The one witness the words
    /// do not depend on is Claude Code's OWN terminal title: it animates while
    /// the agent works and rests on `✳` when it does not (measured on tab 1: `✳`
    /// from 12:19:10 to 17:03:52 — the whole stall — and an animation for the
    /// hours of workflow before it). So: the screen has read busy on background
    /// work alone for [`STUCK_AFTER`] and the title has rested throughout →
    /// ONE journal line per episode, `STUCK …`; until it has said so the
    /// title is asked for no more than once per [`STUCK_AFTER`]. It types
    /// nothing and raises nothing itself: it is the log the owner reads later.
    ///
    /// THE LADDER ABOVE IT (design-exceptions 9.3; the input is this canary, not
    /// screen age: workflow footers tick and would false-alarm): still true at
    /// 90 minutes, `STALLED` to the journal and [`IdleHost::stall_unread`] (a
    /// record and an error line at the host); still true at 3 hours, an
    /// [`Exception::stalled`] (`EXCEPTION … kind=stalled`, a line of the digest:
    /// "open the tab and type keep going"). The title asked again at each rung:
    /// one that moves is a working agent, and the ladder is over. It ends, too,
    /// when the screen stops reading background-only, at the next point and
    /// when the loop ends ([`Self::stuck_over`]).
    /// `since`: when the screen began reading background work only
    /// ([`Self::host_steps_in_background`]'s clock). A busy read, background
    /// work alone included, is also the end of a dialog's episode
    /// ([`Self::dialog_note`]), so a clock left running meets no dialog hours
    /// later.
    pub(super) fn stuck_busy_note(
        &mut self,
        screen: &Screen,
        since: Option<Instant>,
        review: &mut dyn Review,
    ) {
        if self.stuck_dialog.is_some() {
            self.stuck_over(screen.seq, "the screen moved on", review);
        }
        let reader = self.reader(&screen.rows);
        let stuck = since.is_some()
            && reader.program() == aterm_phase::Program::Claude
            && aterm_phase::background_only(&screen.rows);
        let Some(at) = since.filter(|_| stuck) else {
            self.stuck_over(screen.seq, "the screen moved on", review);
            self.stuck_asked = None;
            return;
        };
        self.stuck_climb(screen, at, self.stuck_after, ExceptionKind::Stalled, review);
    }

    /// THE CANARY'S SECOND FLAVOUR, for a DIALOG NO READER NAMES: Claude Code
    /// stops at a dialog this build has no reading of (measured 2026-10-06 on
    /// 2.1.292: the classifier-billing notice, `Enter to continue · Esc to
    /// cancel`, which has no options), and the screen reads `idle` with no
    /// composer, nothing busy and nothing said, for as long as it stands. The
    /// evidence is the screen's words, which have not changed
    /// ([`Self::dialog_words`]); the witness, asked only when a rung is due
    /// ([`Self::stuck_climb`]), is the agent's own title resting on `✳` (it
    /// animates only for a running turn, a dialog does not move it) and
    /// `status` naming Claude as the tab's program, which the screen cannot
    /// say (the loop's own reader is the generic one until it has typed
    /// something, and this screen has no frame to name it by).
    ///
    /// THE LADDER is the busy flavour's at a third of its unit
    /// ([`DIALOG_DIVISOR`]): `STUCK` in the journal at 10 minutes, `STALLED`
    /// and the host's record at 30, at an hour a line of "To look at"
    /// ([`Exception::dialog`], `kind=dialog`) asking the owner to answer it in
    /// the tab. Nothing runs here, so nothing but a person can be what it
    /// waits for: one who opened a panel (`/usage`) and walked away is told once, after
    /// an hour, in a line that ends itself when the screen moves. Nothing is
    /// typed: a key aterm cannot place answers the wrong dialog (the Enter
    /// that continues this notice accepts an option on the next).
    ///
    /// CALLED from the two places the loop visits while such a screen stands:
    /// the top of each step of its wait ([`Self::wait_for_next`], every 20 s
    /// at most, on a still screen), and each read of a screen that never holds
    /// still for the idle window ([`Self::await_turn_from`], which never
    /// reaches that wait: a footer that ticks every second). Hosted watch
    /// only, as the stall probe is.
    pub(super) fn dialog_note(&mut self, screen: &Screen, review: &mut dyn Review) {
        if !review.unattended() {
            return;
        }
        let Some(words) = self.dialog_words(screen) else {
            // Nothing a person could be left at: an episode that was open is over.
            if self.stuck_dialog.is_some() {
                self.stuck_over(screen.seq, "the screen moved on", review);
            }
            return;
        };
        match self.stuck_dialog.as_ref() {
            Some((was, at)) if *was == words => {
                let at = *at;
                let after = self.stuck_after / DIALOG_DIVISOR;
                self.stuck_climb(screen, at, after, ExceptionKind::Dialog, review);
            }
            // A screen new to the clock, or another dialog on it: its own
            // episode, from now. What the ladder still held under the screen
            // before (a busy flavour's rungs) is over with it.
            _ => {
                self.stuck_over(screen.seq, "the screen changed", review);
                self.stuck_dialog = Some((words, Instant::now()));
            }
        }
    }

    /// The words of a dialog no reader names on `screen` ([`Self::dialog_note`]),
    /// or `None`. Claude Code's grammar reads it `idle` WITHOUT evidence — no
    /// composer frame, no shell-mode frame, no wall: that grammar's `idle` is
    /// "no box, no spinner, no question", which any frameless screen
    /// satisfies — with no busy row, question, box, wall or survey card
    /// (each of those is another phase, or its own point); nothing else of
    /// the loop's owns the screen (a held stall, its own `/usage` panel or
    /// limit-reset dialog, a point already told: the host keeps ONE exception
    /// per tab); and it has words. They are the letters of its last
    /// [`KEY_ROWS`] non-blank rows ([`push_words_above`]), the digest the
    /// review key already trusts: a tick of seconds, a blinking glyph or a
    /// footer's clock is no change, and a sentence added, another dialog or a
    /// re-wrap is one.
    pub(super) fn dialog_words(&self, screen: &Screen) -> Option<String> {
        if self.stalled.is_some()
            || self.probe != UsageProbe::Idle
            || self.reset != ResetStep::Idle
            || self.exception_open.is_some()
        {
            return None;
        }
        let rows = &screen.rows;
        let reader: &dyn ScreenReader = &aterm_phase::ClaudeReader;
        let phase = reader.phase(rows);
        if phase != Phase::Idle || reader.phase_authoritative(rows, &phase) || reader.survey(rows) {
            return None;
        }
        let mut words = String::new();
        push_words_above(&mut words, rows);
        let words = words.trim();
        (!words.is_empty()).then(|| words.to_string())
    }

    /// One climb of the canary's ladder for the episode that began at `at`
    /// ([`Self::stuck_busy_note`], [`Self::dialog_note`]): when the next rung
    /// is due — its multiple of `after`, the title not asked within `after`
    /// — one `status` read is its witness, and the rung is said in `kind`'s
    /// words. A title that moves is an agent at work, and, for a dialog, a tab
    /// that runs no Claude (a shell left in a swapped tab keeps the title it
    /// had) has none to wait for: either ends the episode.
    fn stuck_climb(
        &mut self,
        screen: &Screen,
        at: Instant,
        after: Duration,
        kind: ExceptionKind,
        review: &mut dyn Review,
    ) {
        let Some((next, times)) = self.stuck_rung.next() else {
            return;
        };
        if at.elapsed() < after * times || self.stuck_asked.is_some_and(|t| t.elapsed() < after) {
            return;
        }
        self.stuck_asked = Some(Instant::now());
        let Ok(r) = self.call(&["status"]) else {
            return;
        };
        if !title_rests(&r.stdout) {
            // A title that moves is an agent at work: whatever the ladder said,
            // it is over.
            self.stuck_over(screen.seq, "the agent's title moves: it works", review);
            return;
        }
        let dialog = kind == ExceptionKind::Dialog;
        if dialog && !names_claude(&r.stdout) {
            self.stuck_over(screen.seq, "the tab runs no Claude", review);
            return;
        }
        let minutes = at.elapsed().as_secs() / 60;
        self.stuck_rung = next;
        match next {
            StuckRung::Quiet => {}
            StuckRung::Said if dialog => {
                let agent = crate::supervise::screen::status_field(&r.stdout, "agent");
                review.note(&format!(
                    "STUCK seq={} the agent's own title says idle, and the screen has stood \
                     unchanged for {minutes} min with no composer, box, wall or busy row \
                     (agent={}): no reader here names it. If the tab is stalled, this is a \
                     dialog this build does not read, and only a person can answer it. rows: {}",
                    screen.seq,
                    agent.unwrap_or("-"),
                    tail_rows(&screen.rows),
                ));
            }
            StuckRung::Said => review.note(&format!(
                "STUCK seq={} the agent's own title says idle, and the screen has read busy on \
                 background work alone (a shell, a monitor, a workflow) for {minutes} min: no \
                 wall, question or box is recognised there. If the tab is stalled, this screen \
                 is one this build does not read.",
                screen.seq,
            )),
            StuckRung::Told => {
                let what = if dialog {
                    "a dialog this build does not read"
                } else {
                    "a wait this build does not classify"
                };
                review.note(&format!(
                    "STALLED seq={} still so after {minutes} min: {what}, told to the window as \
                     a record",
                    screen.seq,
                ));
                if let Some(host) = &self.stall_host {
                    host.stall_unread(Some(minutes));
                }
            }
            StuckRung::Raised => {
                let exception = if dialog {
                    Exception::dialog()
                } else {
                    Exception::stalled()
                };
                review.note(&format!(
                    "EXCEPTION seq={} kind={} when={} {} => {}",
                    screen.seq,
                    exception.kind.word(),
                    exception.when.word(),
                    exception.why,
                    exception.action
                ));
                if let Some(host) = &self.stall_host {
                    host.exception(Some(&exception));
                }
                self.stuck_raised = Some((Instant::now(), kind));
            }
        }
    }

    /// THE STALL NO READER NAMED IS OVER (or never was): the screen moved on,
    /// the title moves, a point was reached, the loop ended. What the ladder
    /// told ends with it: the record's host is told `None`, and the raised
    /// exception journals `CLEARED seq=<n> exception=<stalled|dialog>
    /// after=<s>s <cause>` and is withdrawn from the host. Quiet where nothing
    /// was said. A dialog's clock ([`Self::dialog_note`]) ends with it, so
    /// every end the busy flavour has is the dialog's too.
    pub(super) fn stuck_over(&mut self, seq: u64, cause: &str, review: &mut dyn Review) {
        self.stuck_dialog = None;
        let rung = std::mem::replace(&mut self.stuck_rung, StuckRung::Quiet);
        let raised = self.stuck_raised.take();
        if rung >= StuckRung::Told
            && let Some(host) = &self.stall_host
        {
            host.stall_unread(None);
        }
        if let Some((since, kind)) = raised {
            review.note(&format!(
                "CLEARED seq={seq} exception={} after={}s {cause}",
                kind.word(),
                since.elapsed().as_secs(),
            ));
            // The host keeps ONE exception per tab, so the stall's end must not
            // drop a point that stood under it: the point is told again.
            if let Some(host) = &self.stall_host {
                host.exception(self.exception_open.as_ref().map(|(open, _)| open));
            }
        }
    }
}

/// Whether a `status` reply says Claude Code's own title rests: its source is
/// the program's own (`osc`, not a title the owner pinned) and it opens with
/// the idle glyph ([`IDLE_TITLE_GLYPH`]).
fn title_rests(stdout: &str) -> bool {
    let field = |f: &str| crate::supervise::screen::status_field(stdout, f);
    field("subject_source") == Some("osc")
        && field("subject")
            .map(crate::supervise::report::pct_decode)
            .is_some_and(|t| t.trim_start().starts_with(IDLE_TITLE_GLYPH))
}

/// Whether a `status` reply names Claude Code as the tab's foreground program
/// (`program=`), read fresh: the screen of a dialog has no frame to say it,
/// and a shell left in a swapped tab, Codex or a foreground `vim` are none.
fn names_claude(stdout: &str) -> bool {
    crate::supervise::screen::status_field(stdout, "program").and_then(aterm_phase::program_of)
        == Some(aterm_phase::Program::Claude)
}

/// How many characters of a screen's last rows the dialog flavour's journal
/// line carries ([`tail_rows`]).
const TAIL_ROWS_CHARS: usize = 400;

/// The last five non-blank rows of `rows`, trimmed, on one line joined by
/// ` ⏎ `, a control character a space: what a reader is taught the dialog by.
/// Cut at [`TAIL_ROWS_CHARS`] from the FRONT (a `…` marks the cut), since a
/// dialog is named by its footer and options, which are the last rows (the
/// classifier-billing notice's five rows are 403 characters, and a cut from the
/// back left `Esc to can`).
fn tail_rows(rows: &[String]) -> String {
    let said: Vec<&str> = rows
        .iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .collect();
    let line: Vec<char> = said[said.len().saturating_sub(5)..]
        .join(" ⏎ ")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cut = line.len().saturating_sub(TAIL_ROWS_CHARS);
    let tail: String = line[cut..].iter().collect();
    if cut > 0 { format!("…{tail}") } else { tail }
}
