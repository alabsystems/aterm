// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The turn-end policy in the loop: every point where a worker's turn ended
//! is decided by [`decide_turn_end`], the one turn-end decider, and the loop
//! only carries out what it says.
//!
//! Around the pure decision the loop adds what it cannot know: the screen
//! read by the reader for the session's program (`aterm-phase`), the work the
//! turn did (the first busy read since the last point, [`Session::running`]),
//! a wall's reset on the loop's clock (read once per notice text), and the
//! standing rules (the `rules_file`, cut at [`RULES_CAP`]). Nothing it types
//! goes without:
//!
//! * the session's foreground program read fresh (`status program=`), its
//!   reader a supervised agent's (Claude Code's, agy's, Muse Code's or
//!   Gemini CLI's), and every read of the composer that reader's
//!   ([`Session::reader`]);
//! * the fenced write ([`Session::type_fenced`]), where the host fences
//!   `send` on the screen generation and the original known person count:
//!   `send if-human=<the judged count> if-gen=<the judged read>
//!   if=<its composer row> -- <text>` writes only while neither has changed
//!   (otherwise `OK skipped reason=person` or `reason=changed` and the
//!   point is decided again shortly; a guard matching no row of the
//!   unchanged screen, a bare `OK skipped`, is escalated). A read must then
//!   show the composer holding that text and the same known person count
//!   before Enter goes, fenced
//!   on that read and the original count (`key if-human=… if-gen=…
//!   if=<the caret row> enter`). An absent count refuses without an unfenced
//!   retry. A program whose paste guard takes an Enter right behind typed
//!   text as a newline (Muse Code, Gemini CLI:
//!   [`aterm_phase::ScreenReader::paste_guard`]) also waits for the echo to
//!   hold still [`PASTE_GUARD_SETTLE_MS`]. Text written and
//!   not submitted is escalated;
//! * elsewhere, the guarded submit (`turn submit=guarded:<re> yield=0.2`):
//!   parked while a person types, then pasted, and Enter pressed only while
//!   the row holding the cursor (one the composer draws, never a shell's)
//!   still ends as the text does ([`composer_guard`]). The paste itself is
//!   not fenced; a miss (`skipped`) leaves the text typed, and is escalated.
//!   A host without either gets nothing typed;
//! * either one, in the window's host ([`Session::type_under_hand`]), under
//!   the harness's hand ([`Session::under_hand`]): no other connection
//!   types between the text and its Enter, and the server's ledger reads
//!   the text as the harness's turn (`history … by=harness`);
//! * for the worker's own suggestion, the vendor's accept key: `right` on an
//!   empty composer with a suggestion shown fills it (Claude Code 2.1.280),
//!   fenced on the judged read's generation and guarded on the suggestion's
//!   row, then Enter fenced on the read that shows it filled. A host without
//!   the generation fence gets the suggestion's words typed instead, and so
//!   does the window's host ([`Session::type_under_hand`]): the vendor counts
//!   a submitted text equal to its suggestion as accepted, and only a typed
//!   text gives the server's ledger the words to book as the harness's turn;
//! * for a draft left standing ([`TurnEndAction::Submit`]), Enter alone,
//!   fenced on the judged read's generation and guarded on the caret row. A
//!   host without the fence gets nothing pressed: the draft is escalated.
//!
//! THE SESSION'S AUTOPILOT ([`TurnEndReading::autopilot`]) is the loop's to
//! fill: the session's `autopilot` word, read by key off the bare `meta`
//! reply as the `questions` word is, else the table's `[harness] autopilot`
//! ([`Session::autopilot`]). Every bare `meta` the loop reads notes it; a
//! decision reads it on purpose only where Cruise would act, reusing the
//! `meta` the prose's `questions` word read if one was made
//! ([`Session::decide_to_act`]), and a host whose `meta` names no such field
//! is asked nothing. A point decided under `off` is neither reported nor
//! acted on: the loop parks ([`Session::park_while_off`]). A task Land
//! landed is said once, `DONE seq=<n> rule=land@v1 …`.
//!
//! A `WaitUntil` is the deadline of the loop's wait for the screen to move
//! ([`Session::wait_for_next`]): a change decides again at the next look,
//! the deadline on the point still showing ([`Session::turn_end_now`]).
//! Every act is a ledger row under its rule id and a printed line
//! (`CONTINUED seq=<n> rule=<id> <text>`, or `TYPED seq=<n> rule=<id>
//! <command>` for a slash command); a wait is journaled `WAITING seq=<n>
//! until=<UTC> <why>` once per deadline, an act not taken `SKIPPED seq=<n>
//! rule=<id> <why>`, and a task Land landed `DONE seq=<n> rule=land@v1 …`
//! (a clean defense at a land act's yield over a clean repository; nothing
//! ends a Cruise task), once — the moment a tasked session's
//! record is marked `done` with the result its transcript holds, and marked
//! `working` again when someone else's turn opens the task again (`TASK
//! seq=<n> …`, `task_mark`).
//!
//! THE ACCOUNTS PROBE IS NONE OF THESE ACTS
//! (`docs/SPEC-claude-accounts-probe-2026-10-01.md`). Where the session's
//! host claims one at a plain idle turn end, the LOOP types `/usage` itself,
//! reads the Settings panel it opens once it reads settled, hands the host
//! the sample and closes the panel with an Escape fenced by the panel's own
//! header row ([`Session::usage_probe`]) — a step between the host's step
//! and the point's act, which the decider never hears of: it books no act,
//! folds no panel screen into the policy's memory, and the point is decided
//! again after it ([`Session::after_probe`]). A panel the harness opened is
//! owed its close until a look sees it gone ([`Session::usage_panel_look`]),
//! across an outage and across loops. With no host that claims — no roster —
//! none of it runs, and not a request is added to what the loop sent before.
//! Where the roster says a probe is DUE and the loop does not run it, the
//! loop says which gate shut, once a point: `SKIPPED seq=<n>
//! rule=usage-probe@v1 gate=<word>` ([`Session::probe_shut`], the words
//! [`ProbeGate`]'s) — printed, so a hosted loop's host logs it.

use super::super::policy::approval::squashed;
use super::super::policy::exception::exception_for;
use super::super::policy::sleeps::InTurn;
use super::super::policy::trace::{Outcome as TraceOutcome, TraceStep};
use super::super::policy::turn_end::{
    ANSWERS_OFF, AccountAnswer, AccountAt, CREDITS_HOLD_MODEL_TOLD, CREDITS_HOLD_TOLD, Composer,
    ComposerRead, CreditsCarry, LOGIN_SHARED_LOOK, MENU_DIVERTS, OWN_WORDS_STAND, ProbeGate,
    RULE_ACCOUNT, RULE_LAND, RULE_LEFT, RULE_LOGIN, RULE_USAGE_PROBE, RecordTurn, Restart,
    TRY_ANSWER_SLACK, WAKE_KEPT, act_turn_work, decide_turn_end, decide_turn_end_traced,
    done_row_work,
};
use super::wall_confirm::{Check, LeftRead};
use super::*;
use crate::harness::accounts::Sample;
use crate::harness::usage::{
    PanelRead, PanelWindow, USAGE_COMMAND, panel_sample, usage_panel_read,
};

/// Whether `composer` holds the point `action` was decided at with nothing
/// due ([`Session::draft_held`]): aterm's own words, or a draft over a
/// completion menu, escalated for itself ([`OWN_WORDS_STAND`],
/// [`MENU_DIVERTS`]); a draft whose cells were not read, left alone until
/// they are. An escalation for another reason over the same composer (a
/// question the table hands to a person, the continue cap) is that reason's
/// point: the draft's going answers none of it, and deciding it again would
/// take its badge down and raise it a second time.
fn draft_holds(composer: &Composer, action: &TurnEndAction) -> bool {
    let told = |why: &str| matches!(action, TurnEndAction::Escalate { reason } if reason == why);
    match composer {
        Composer::Own => told(OWN_WORDS_STAND),
        Composer::Diverted => told(MENU_DIVERTS),
        Composer::Unknown => matches!(action, TurnEndAction::Nothing),
        Composer::Absent | Composer::Empty | Composer::Placeholder(_) | Composer::Typed => false,
    }
}

/// The furthest a wall's reset may jump on a REPRINT of the same words
/// before the jump is doubted: the ladder's last rung (30 minutes).
const REPRINT_JUMP_MAX_S: i64 = 30 * 60;

/// How many reprints a doubted jump is kept back for ([`REPRINT_JUMP_MAX_S`]).
pub(super) const REPRINT_JUMPS_MAX: u32 = 4;

/// How long past its placement a reset is doubted at all: two hours, the four
/// ladder tries of [`REPRINT_JUMPS_MAX`] (10, 30, 30, 30 minutes).
const REPRINT_HOLD_WINDOW_S: i64 = 2 * 3600;

/// The longest text the cursor-row guard names whole, as `^❯ <text>`: it
/// sits on the caret row of any composer 43 columns wide or more.
const GUARD_TAIL_CHARS: usize = 40;

/// The most of the text's last word a longer text's guard names: the tail
/// of a word too long for the composer's width is what its last row holds.
const GUARD_WORD_CHARS: usize = 16;

/// `yield=<floor>` on every `turn` this policy types: the verb parks before
/// typing until the target's typing momentum has fallen to the floor, so a
/// person typing is not pasted over. A yield that outlives the turn's
/// timeout types nothing (`ERR yield timeout`).
const TURN_YIELD: &str = "yield=0.2";

/// The standing rules ride in the continuation cut to this many characters
/// (at a word, `…` after): a continuation stays a line a composer holds as
/// typed text, not a pasted-text placeholder the fence cannot read back.
pub(super) const RULES_CAP: usize = 400;

/// The longest the loop waits for the worker to react to its own write (the
/// continuation's echo, [`Session::type_fenced`]; the accept key's fill,
/// [`Session::accept_suggestion`]) before it reads the composer to judge the
/// write. The wait ends the moment the screen moves, so a worker that reacts
/// pays nothing; the bound only says when one that has not reacted at all is
/// judged anyway. Judged too early, the read sees the composer as it was
/// before the write, and an accept key not yet read would get its words
/// typed after it, submitting them doubled. A worker starved past this
/// bound is still judged on the old composer, and one that stalls mid-draw
/// is read once the screen is still for [`SETTLE_MS`] or [`SETTLE_CAP`] has
/// passed (a half-drawn composer is escalated).
pub(super) const REACTION_WAIT: Duration = Duration::from_secs(10);

/// How long the echo of a fenced write must hold still before its Enter, for
/// a program with a paste guard ([`ScreenReader::paste_guard`]: Codex and
/// Muse Code take an Enter within a burst of typed text as a newline): the
/// guarded `turn`'s own settle (`idle=600`).
const PASTE_GUARD_SETTLE_MS: &str = "600";

/// The cursor-row guard for `text` typed into the composer whose caret row
/// starts with `caret` (the program's, [`ScreenReader::caret`]: `❯` Claude
/// Code and Muse Code, `›` Codex, `>` agy): `^<caret> <text>` to the row's end when the
/// text is short enough to sit on the caret row ([`GUARD_TAIL_CHARS`]); else
/// the end of its last word (at most [`GUARD_WORD_CHARS`]) to the row's end,
/// on a row the composer draws (its caret row, or a continuation row
/// indented two). Each wraps between words (agy's measured on its sent
/// prompt's echo, Muse Code's on a 301-character draft, its rows indented
/// two), so the row the cursor ends on holds the last word at any width. This
/// checks the composer's text shape; a custom shell prompt or continuation
/// prompt can share it, especially agy's `>`. The dispatch's program check and
/// the fenced path's generations and fresh composer read provide separate
/// checks, without an atomic foreground-process identity fence.
/// Whitespace-free ([`super::super::policy::row_guard`]'s escaping). A caret
/// drawn in another column than 0 is [`composer_guard_at`]'s.
pub(crate) fn composer_guard(caret: char, text: &str) -> String {
    composer_guard_at(caret, 0, text)
}

/// [`composer_guard`] for a composer whose caret is drawn `column` blanks in
/// ([`ScreenReader::caret_column`]: Gemini CLI's ` > text`, its draft
/// wrapped onto rows that start in column 3 — between words, as its bundle's
/// layout breaks them): each form anchored that many blanks before the caret,
/// or before the continuation's indent, which is one column wider. A caret
/// that is a pattern's metacharacter (Gemini CLI's YOLO `*`) stands in the
/// guard escaped, as [`super::super::policy::row_guard`] writes it.
pub(crate) fn composer_guard_at(caret: char, column: usize, text: &str) -> String {
    let lead = "\\s".repeat(column);
    if text.chars().count() <= GUARD_TAIL_CHARS {
        // The caret's space is whatever the program draws there (a no-break
        // space on Claude Code 2.1.280 and 2.1.281: `❯\u{a0}keep going`), so
        // it is `\s`, never the `\x20` a typed space is.
        let guard = super::super::policy::row_guard(&format!("{caret} {text}"));
        let lit = super::super::policy::row_guard(&caret.to_string());
        let lit = lit
            .strip_prefix('^')
            .and_then(|l| l.strip_suffix(super::super::policy::guard::ROW_END))
            .unwrap_or(&lit);
        return guard.replacen(&format!("^{lit}\\x20"), &format!("^{lead}{lit}\\s"), 1);
    }
    let word = text.split_whitespace().last().unwrap_or(text);
    let n = word.chars().count();
    let tail: String = word
        .chars()
        .skip(n.saturating_sub(GUARD_WORD_CHARS))
        .collect();
    let anchored = super::super::policy::row_guard(&tail);
    // `row_guard` anchors at the row's start: the tail ends the row, on a
    // row that opens with the caret or the composer's two-space indent.
    format!(
        "^{lead}[{caret}\\s]\\s.*{}",
        anchored.trim_start_matches('^')
    )
}

/// After a Mac's wake, nothing is typed into the agent for this long — the
/// network is coming back (Wi-Fi re-associating, a VPN reconnecting), and the
/// first turn typed into an API that cannot be reached is a wall of its own
/// — plus up to [`WAKE_SPREAD`] of the session's own, so the sessions of one
/// account, every one waking at the same instant with its reset past, do not
/// all type in the same second ([`Session::settle_wake`]). A wait of the
/// policy's that the sleep carried past is decided again then.
pub(super) const WAKE_SETTLE: Duration = Duration::from_secs(30);
/// See [`WAKE_SETTLE`].
pub(super) const WAKE_SPREAD: Duration = Duration::from_secs(60);

/// How long the session repository's last answer stands for the same done
/// words, no turn run since ([`Session::repo_read`]): a point a loop re-reads
/// at every wake asks `git` at most this often.
pub(super) const REPO_REPROBE: Duration = Duration::from_secs(20);

/// The reason of the due the wake's settle places ([`Session::woke`],
/// [`Session::turn_end_execute`]).
pub(super) const WOKE: &str = "the Mac woke";

/// Whether the `tries`-th ask of a restart the host keeps deferring is
/// journaled: the first four (the pauses grow 10 s, 1, 5, 10 min) and then
/// every sixth — an hour on the last pause — where a fallback that waits on
/// the agent's own jobs for a day would otherwise write ~290 lines of the same
/// `wait:changed`. The pause itself is not journaled away: it is `turn_end_due`.
fn restart_note_due(tries: u32) -> bool {
    tries < 4 || tries.is_multiple_of(6)
}

/// The composer's caret row as `reader` reads it (`None`: no composer).
fn caret_row(reader: &dyn ScreenReader, rows: &[String]) -> Option<usize> {
    reader.composer(rows).map(|(caret, _)| caret)
}

/// The text on the composer's caret row as `reader` reads it, the caret
/// stripped — typed, or the placeholder ([`typed_draft`] tells which).
fn caret_text(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    reader
        .composer(rows)
        .and_then(|(_, lines)| lines.into_iter().next())
}

/// A wall the host's measure of the API's reach answers: an API error that
/// never reached the API, a reply the connection cut off, or a certificate
/// or proxy refused ([`aterm_phase::ApiCause`]), the arms of the turn-end
/// policy that read [`TurnEndReading::reach`]. The server's own failure (a
/// status, an overload) is not: a handshake says nothing of the API's
/// health, and it keeps its ladder whatever the measure.
fn answered_by_the_network(kind: aterm_phase::WallKind) -> bool {
    matches!(
        kind,
        aterm_phase::WallKind::ApiError {
            cause: aterm_phase::ApiCause::Unreachable
                | aterm_phase::ApiCause::CutOff
                | aterm_phase::ApiCause::Config,
            ..
        }
    )
}

/// The walls a usage window's reset lifts.
fn resumes_at_reset(kind: aterm_phase::WallKind) -> bool {
    use aterm_phase::WallKind as K;
    matches!(
        kind,
        K::UsageSession | K::UsageWeekly | K::Spend | K::ModelBucket { .. }
    )
}

/// A session's stable share of `span`, from its id and the
/// reset (unix seconds) — FNV-1a, no clock and no randomness, so the same
/// session waits the same and the policy stays a pure function of what it is
/// handed. Zero for a zero span.
pub(super) fn resume_jitter(sid: &str, reset_unix: i64, span: Duration) -> Duration {
    let span_s = span.as_secs();
    if span_s == 0 {
        return Duration::ZERO;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sid.bytes().chain(reset_unix.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    Duration::from_secs(h % span_s)
}

/// The standing rules of `policy` (`rules_file`), read now (an edit since the
/// launch counts), as one line of at most [`RULES_CAP`] characters: what the
/// loop appends to everything it types, and what aterm's `harness policy` verb
/// composes into `continue_text=` for an agent that answers for itself.
/// `None` when it is not set, or the file cannot be read or is empty.
#[must_use]
pub fn standing_rules(policy: &crate::supervise::SupervisorConfig) -> Option<String> {
    let path = policy.rules_file.as_ref()?;
    standing_rules_in(&std::fs::read_to_string(path).ok()?)
}

/// [`standing_rules`] of the rules file's text `text`, read by the caller (the
/// loop reads it through its door, `Fx::read_text`, so a harness tape holds
/// the rules the loop typed): `None` when it is empty.
#[must_use]
pub fn standing_rules_in(text: &str) -> Option<String> {
    let line = capped_rules(&limit::one_line(text));
    (!line.is_empty()).then_some(line)
}

/// `rules` as one line of at most [`RULES_CAP`] characters: cut at the last
/// word that fits, `…` after.
pub(super) fn capped_rules(line: &str) -> String {
    if line.chars().count() <= RULES_CAP {
        return line.to_string();
    }
    let head: String = line.chars().take(RULES_CAP).collect();
    let cut = head.rfind(' ').filter(|&i| i > 0).unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end())
}

/// The unix second nearest `ahead_ms` past the unix millisecond `now_ms`: the
/// second `now_ms` falls in, moved by its own milliseconds plus `ahead_ms`,
/// rounded half away from that second. On a whole-second clock (a test's) that
/// is the second plus `ahead_ms` rounded half away from zero.
pub(super) fn nearest_s(now_ms: i64, ahead_ms: i64) -> i64 {
    let off = now_ms.rem_euclid(1000).saturating_add(ahead_ms);
    let whole = i64::try_from(off.unsigned_abs().saturating_add(500) / 1000).unwrap_or(i64::MAX);
    now_ms
        .div_euclid(1000)
        .saturating_add(off.signum().saturating_mul(whole))
}

impl<C: Ctl> Session<'_, C> {
    /// `t` on the loop's clock as unix seconds, the nearest, against the
    /// clock to the millisecond ([`Self::now_unix_ms`]).
    pub(super) fn unix_of(&self, t: Instant) -> i64 {
        let (now_ms, ahead) = self.ms_ahead(t);
        nearest_s(now_ms, ahead)
    }

    /// `t` on the loop's clock as unix MILLISECONDS ([`Self::unix_of`] to the
    /// millisecond): the wall time a deadline names ([`Due::unix_ms`]).
    pub(super) fn unix_ms_of(&self, t: Instant) -> i64 {
        let (now_ms, ahead) = self.ms_ahead(t);
        now_ms.saturating_add(ahead)
    }

    /// The wall clock now (unix milliseconds) and how many milliseconds
    /// `t` lies ahead of now on the monotonic clock (negative: behind).
    fn ms_ahead(&self, t: Instant) -> (i64, i64) {
        let now = self.fx.mono();
        let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
        let ahead = if t >= now { ms(t - now) } else { -ms(now - t) };
        (self.now_unix_ms(), ahead)
    }

    /// Stand `at` (on the loop's clock) as the policy's deadline at the point
    /// still showing ([`Self::turn_end_due`]), and why: kept on both clocks
    /// ([`Due`]), its wall time read now. No wall's next try
    /// ([`Self::set_wait`]).
    pub(super) fn set_due(&mut self, at: Instant, why: impl Into<String>) {
        self.set_wait(at, why, WaitKind::Other);
    }

    /// [`Self::set_due`], `wait` the wait's kind (a
    /// [`TurnEndAction::WaitUntil`]'s `wait`): a wall's next try and the
    /// short streak's back-off are told to the host ([`Self::tell_wait`]).
    pub(super) fn set_wait(&mut self, at: Instant, why: impl Into<String>, wait: WaitKind) {
        self.turn_end_due = Some(Due {
            at,
            unix_ms: self.unix_ms_of(at),
            why: why.into(),
            wait,
        });
    }

    /// When the policy's deadline falls ([`Due`]), on the loop's clock now:
    /// its `Instant` — unless the wall clock has run ahead of the monotonic
    /// one since it was set by more than a sleep ([`super::slept_through`]),
    /// when it is the wall time it named, now behind or nearer. The
    /// milliseconds between the two clocks' reads never move it, so an
    /// awake wait never falls a moment before its `Instant` and is decided
    /// twice. `None`: nothing waited on.
    pub(super) fn due_at(&self) -> Option<Instant> {
        self.turn_end_due.as_ref().map(|due| self.falls_at(due))
    }

    /// When `due` falls on the loop's clock now ([`Self::due_at`]'s rule):
    /// its `Instant`, or the wall time it named once the wall clock has run
    /// ahead of the monotonic one since it was set by more than a sleep.
    fn falls_at(&self, due: &Due) -> Instant {
        let wall = self.instant_of_ms(due.unix_ms);
        // How much sooner the wall clock reads the deadline than the
        // monotonic one: the sleep since it was set.
        let ahead =
            i64::try_from(due.at.saturating_duration_since(wall).as_secs()).unwrap_or(i64::MAX);
        if super::slept_through(ahead, 0) {
            wall
        } else {
            due.at
        }
    }

    /// When the wake's settle falls ([`Self::settle_wake`]), while it is
    /// still ahead; one that has fallen is let go.
    pub(super) fn settling(&mut self) -> Option<Instant> {
        let at = self
            .wake_settle
            .as_ref()
            .map(|(settle, _)| self.falls_at(settle));
        let ahead = at.filter(|&at| self.fx.mono() < at);
        if ahead.is_none() {
            self.wake_settle = None;
        }
        ahead
    }

    /// `t` on the loop's clock as a UTC stamp, for the journal (a `t` already
    /// past stamps now). A unix time made an `Instant` by
    /// [`Self::instant_of`] stamps as that unix time, whenever it is read.
    pub(super) fn stamp_in(&self, t: Instant) -> String {
        let secs = self.unix_of(t.max(self.fx.mono()));
        utc_stamp(u64::try_from(secs).unwrap_or(0))
    }

    /// `unix` (epoch seconds) on the loop's clock: placed against the clock to
    /// the millisecond ([`Self::now_unix_ms`]), so the `Instant` is `unix`
    /// itself, whatever part of a second the read fell in. The ONE way a
    /// unix time becomes an `Instant` here; [`Self::unix_of`] is its inverse.
    pub(super) fn instant_of(&self, unix: i64) -> Instant {
        self.instant_of_ms(unix.saturating_mul(1000))
    }

    /// WHETHER THE CREDITS HOLD STANDS FOR A TURN THE HOST WOULD TYPE on
    /// `rows` (`Guard::Credits`): the screen says usage credits carry the
    /// session, or the loop keeps a notice that still stands without it
    /// ([`Self::credits_carry`]), under `cfg` — the policy's own hold
    /// ([`TurnEndState::credits_hold_stands`]), so the host types no turn
    /// the policy would not.
    pub(super) fn credits_hold_here(&self, cfg: &SupervisorConfig, rows: &[String]) -> bool {
        let reader = self.reader(rows);
        let read = self.credits_carry(reader.credits(rows).as_ref());
        self.turn_end
            .credits_hold_stands(read.as_ref(), reader.program(), cfg, self.fx.mono())
    }

    /// Usage credits carry the session, a read's notice says (`drawn`), or
    /// the loop keeps one that stands without it ([`Self::note_credits`]):
    /// with the reset kept from the notice they started on, on the loop's
    /// clock, the first read of a notice, and the model one named. `None`:
    /// neither.
    pub(super) fn credits_carry(
        &self,
        drawn: Option<&aterm_phase::credits::Credits>,
    ) -> Option<CreditsCarry> {
        let kept = self.credits_kept.as_ref();
        if drawn.is_none() && kept.is_none() {
            return None;
        }
        Some(CreditsCarry {
            reset_at: kept
                .and_then(|k| k.reset.as_ref())
                .and_then(|r| r.unix)
                .map(|unix| self.instant_of(unix)),
            since: kept.map(|k| k.since),
            model: drawn
                .and_then(|c| c.model.as_deref())
                .and_then(super::super::policy::turn_end::credits_model)
                .or_else(|| kept.and_then(|k| k.model.clone())),
            kept: drawn.is_none(),
        })
    }

    /// [`Self::instant_of`] for a unix MILLISECOND ([`Self::unix_ms_of`]'s
    /// inverse). A time further back than the monotonic clock reaches is now.
    pub(super) fn instant_of_ms(&self, unix_ms: i64) -> Instant {
        let now = self.fx.mono();
        let delta = unix_ms.saturating_sub(self.now_unix_ms());
        let d = Duration::from_millis(delta.unsigned_abs());
        if delta >= 0 {
            now + d
        } else {
            now.checked_sub(d).unwrap_or(now)
        }
    }

    /// The model switch an earlier loop left open
    /// ([`approvals::open_model_switch`]), carried on by this one: its reset
    /// placed on this loop's clock by [`Self::instant_of`], so the switch back
    /// and the row that says when stand at the second the earlier row named.
    pub(super) fn carried_model_switch(&self, open: approvals::OpenSwitch) -> ModelSwitch {
        ModelSwitch {
            from: open.from,
            to: open.to,
            back_at: open.back_at_unix.map(|unix| self.instant_of(unix)),
        }
    }

    /// THE MODEL SWITCH AN EARLIER LOOP MADE, taken up from the ledger at
    /// `path` ([`approvals::open_model_switch`], `drive_loop`'s seed): switched
    /// back at its reset by this loop ([`TurnEndState::seed_model`]), and,
    /// where no turn has been asked on the fallback since its relaunch
    /// ([`approvals::OpenSwitch::asked`]), the relaunched process's redraw of
    /// the bucket's notice is no wall of this loop's either — no notice the
    /// transcript wrote after the relaunch was made
    /// ([`approvals::OpenSwitch::made_unix`],
    /// [`TurnEndState::seed_fallback_unasked`]). A relaunch asked and not
    /// made yet (its run's row, [`approvals::relaunch_waiting`]) is this
    /// loop's run of tries, made at that run's first ask. One method for the
    /// loop and its tests: written out again in each test's successor, the
    /// seed this loop runs was never tested (the verifier of b2c17b2dd, A-1).
    #[cfg(test)]
    pub(super) fn seed_model_switch(&mut self, path: &std::path::Path) {
        let body = self.fx.read_text(path).unwrap_or_default();
        self.seed_model_switch_in(&body, None);
    }

    /// [`Self::seed_model_switch`] over the ledger's text, read once by the
    /// loop through its door (`approvals::*_in`), and the journal beside it
    /// (`journal`): where an older build's rows do not say when the relaunch
    /// was made, its journal does ([`journaled_relaunch`]).
    pub(super) fn seed_model_switch_in(&mut self, body: &str, journal: Option<&str>) {
        let fallback = Restart::Model { to: String::new() };
        let journaled = journal.map_or_else(JournaledRelaunch::default, |j| {
            journaled_relaunch(j, self.setup.sid.as_deref(), fallback.word())
        });
        // A relaunch an earlier loop asked and its host still carries: this
        // loop's run, made at that run's first ask
        // ([`approvals::relaunch_waiting`], [`Self::restart_run`]).
        if let Some(made) =
            approvals::relaunch_waiting_in(body, self.setup.sid.as_deref()).or(journaled.waiting)
        {
            self.restart_run = Some((fallback.word(), made));
        }
        let Some(open) = approvals::open_model_switch_in(body, self.setup.sid.as_deref()) else {
            return;
        };
        // Never later than the journal's: a notice written after it is a
        // wall, not the relaunched process's redraw.
        let made_at = journaled
            .made
            .map_or(open.made_unix, |m| m.min(open.made_unix));
        let asked = open.asked;
        let switch = self.carried_model_switch(open);
        self.turn_end.seed_model(switch);
        if !asked {
            self.turn_end.seed_fallback_unasked(made_at);
        }
    }
    /// The standing rules, read now (an edit since the launch counts), as
    /// one line: `policy.rules_file`. `None` when it is not set, or the file
    /// cannot be read or is empty.
    fn rules_text(&self, opts: &SuperviseOpts) -> Option<String> {
        let path = opts.policy.rules_file.as_ref()?;
        standing_rules_in(&self.fx.read_text(path)?)
    }

    /// A wall's `reset=` text as the loop's clock, read once per text and
    /// kind: the same text is the same reset ([`Self::refresh_reset`]'s rule).
    /// A reset already past is that long ago. The
    /// reset is placed for the wall's kind ([`Self::reset_unix_for`]) and
    /// remembered as UNIX time, turned into the loop's monotonic clock at
    /// every reading — an `Instant` does not count the time a Mac sleeps
    /// (measured here: `CLOCK_UPTIME_RAW` runs 2.4 days behind
    /// `CLOCK_MONOTONIC`), so one cached at the first reading lands late by
    /// every sleep since. The cache is dropped with the wall
    /// ([`Self::turn_end_reading`]), so the next wall of the same words is a
    /// new reset.
    ///
    /// `undated`: the wall is a LIMIT A CANCEL LEFT that only the screen read
    /// as a turn's answer — no transcript row dated its notice since the
    /// turn ([`Session::left_read`]) — and a conversation `/resume`d in the
    /// meantime, ending on its own older cancelled limit, draws the same. Its
    /// bare clock is then placed never more than [`limit::SESSION_AHEAD_MAX`]
    /// ahead: one placed further — a reset already past read as tomorrow's
    /// on the print of the act it was taken to answer — is the day before's,
    /// PASSED, and the continuation goes at its grace. The shorter wait, not
    /// the owner's call: a wrong "passed" costs one try, which a limit that
    /// stands answers with a notice the loop reads as it arrives and places
    /// on that print; a wrong "tomorrow" is a day's wait on a session that
    /// may be free (round 17's residual (a)).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn wall_reset_at(
        &mut self,
        text: Option<&str>,
        kind: aterm_phase::WallKind,
        session_window: bool,
        live: bool,
        answered: bool,
        undated: bool,
        rows: &[String],
    ) -> Option<Instant> {
        let key = reset_key(kind, text);
        // Only a bare clock time on a Claude Code wall that is no session
        // window is placed on the print (a span counts from its first print
        // and is read once; a dated time and a session window's clock do not
        // move with it; a Codex's words keep their first placement, the
        // host's or the look's day's — [`Self::reset_unix_for`]).
        let reprint = (live || answered)
            && !session_window
            && places_resets_on_print(self.reader(rows).program())
            && matches!(
                text.and_then(limit::parse_reset),
                Some(limit::ResetSpec::At { date: None, .. })
            );
        let unix = match &self.wall_reset {
            // The same print read again (a wait's wake, a re-decision, the
            // screen changing under it): the reset it was placed at. NOT a
            // print the loop watched arrive (`live`): after an act of ours
            // the wall answered, and the notice printed again — the same
            // words, a new print — is placed afresh on what the loop saw. A
            // reset guessed for a wall found sitting was wrong once at most
            // (cached under the words, the same misplacement
            // answered every retry: 26 blind messages in a 13-hour wait, in a
            // replay of the policy).
            Some((k, u)) if *k == key && !reprint => *u,
            _ => {
                let same = self
                    .wall_reset
                    .as_ref()
                    .filter(|(k, _)| *k == key)
                    .and_then(|(_, o)| *o);
                let mut u = self.reset_unix_for(text, session_window, live, answered, rows);
                if undated {
                    u = u.map(|at| self.within_a_session_window(text, at));
                }
                // A reprint placed on
                // ITS OWN print can jump a day: `resets 4pm`, tried at 16:02
                // and 16:12, reprinted at 16:13 — placed on 16:07, that is
                // TOMORROW's 16:00, a 23-hour wait for a lift that lagged
                // twelve minutes. A jump past the ladder's last rung on the
                // same words, from a placement that passed within the last
                // two hours, is not taken at once: the ladder goes on (at
                // most [`REPRINT_JUMPS_MAX`] times — four more tries, about
                // two hours — and then the far placement is believed, since
                // the vendor prints a bare clock only for a reset within a
                // day AHEAD).
                let now_unix = self.now_unix();
                match (reprint, same, u) {
                    // Only a placement that PASSED recently is doubted: the
                    // reset was lately behind us and the wall still stands.
                    // One placed hours back was a guess (a wall found sitting
                    // with no print to go by), and its reprint is the
                    // evidence.
                    (true, Some(old), Some(new))
                        if new > old.saturating_add(REPRINT_JUMP_MAX_S)
                            && now_unix.saturating_sub(old) <= REPRINT_HOLD_WINDOW_S
                            && self.reprint_jumps < REPRINT_JUMPS_MAX =>
                    {
                        self.reprint_jumps += 1;
                        u = Some(old);
                    }
                    (_, None, _) => self.reprint_jumps = 0,
                    _ => {}
                }
                self.wall_reset = Some((key, u));
                u
            }
        };
        unix.map(|unix| self.instant_of(unix))
    }

    /// `at`, the placement of the reset `text` names, as an UNDATED one is
    /// kept ([`Self::wall_reset_at`]): a bare clock placed more than
    /// [`limit::SESSION_AHEAD_MAX`] ahead is the day before's, passed.
    fn within_a_session_window(&self, text: Option<&str>, at: i64) -> i64 {
        text.and_then(limit::parse_reset).map_or(at, |spec| {
            limit::within_a_session_window(&spec, at, self.now_unix())
        })
    }

    /// How long ago a person last typed into the session, as the policy
    /// reads it ([`TurnEndReading::person`]): the later of the server's last
    /// status (`human_ms=`) and the draft in the composer last changing.
    pub(super) fn person_ago(&self, now: Instant) -> Option<Duration> {
        self.person_at()
            .map(|typed| now.saturating_duration_since(typed))
    }

    /// When a person last typed into the session ([`Self::person_ago`]).
    pub(super) fn person_at(&self) -> Option<Instant> {
        let draft = self.draft_seen.as_ref().map(|(_, at)| *at);
        self.person.max(draft)
    }

    /// The turn-end reading of `screen`: the reader for the session's program
    /// (as last read), the work since the last point, the wall's reset, the
    /// rules, and how long ago a person typed, a draft that changed on any
    /// read included ([`Self::person_ago`]). At a wall the network answers
    /// ([`answered_by_the_network`]), and there alone, the host's measure of
    /// the API's reach ([`IdleHost::reach`]), remembered as
    /// [`Self::reach_seen`]; everywhere else nothing is asked, and it is
    /// forgotten.
    ///
    /// A DECISION's read: the screen's wall is checked against the session's
    /// transcript here ([`Self::confirm_wall`], [`Check::Decide`]). The
    /// point read again for the loop's books after its act is
    /// [`Self::turn_end_reread`].
    pub(super) fn turn_end_reading(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, worked, opts, Check::Decide, true)
    }

    /// The reading of a point ALREADY DECIDED, for the loop's books after
    /// its act — what the act was taken at, the restart made or waited for:
    /// its wall as the decision's read judged it at that screen
    /// ([`Check::AsDecided`]). The transcript, moved on by the act itself
    /// (the carry-on typed, the relaunched conversation), is not asked again:
    /// asked, it refused the wall just acted on.
    pub(super) fn turn_end_reread(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, None, opts, Check::AsDecided, true)
    }

    /// [`Self::turn_end_reread`] of the point an ACCOUNT MOVE'S RELAUNCH was
    /// made at ([`Restart::Account`]), and nothing it says of the move told
    /// ([`Self::account_tells`]): the screen is the old process's, but the
    /// host judges the process its tab runs NOW — the one under the login
    /// moved to — so a lost login told from it struck the account the
    /// session had just been moved to. What answered the move was told
    /// before the host advised it ([`Self::fold_point`]).
    fn turn_end_reread_relaunched(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, None, opts, Check::AsDecided, false)
    }

    fn turn_end_reading_as(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
        check: Check,
        tell: bool,
    ) -> TurnEndReading {
        let reader = self.reader(&screen.rows);
        let mut reading = reader.read(&screen.rows, Some(screen.cursor_col));
        // A screen busy by nothing but the wait Claude Code carries, whose
        // turn the session's transcript said closed with nothing pending:
        // the loop's idle point ([`Self::transcript_wait_over`]), idle here too.
        if reading.phase == Phase::Busy && self.transcript_over_seq == Some(screen.seq) {
            reading.phase = Phase::Idle;
        }
        // Whose countdown a limit's notice is: the agent's process as the
        // host last read it, before the point is folded in (KI-1015-1 W4).
        if check == Check::Decide
            && let Some(started) = opts.idle_host.as_ref().and_then(|h| h.agent_started())
        {
            let at = self.instant_of(started);
            self.turn_end.agent_started(at);
            self.turn_end.agent_started_unix(started);
        }
        // THE PROGRAM'S OWN HOUSEKEEPING ended the turn (Claude Code's idle
        // compaction, its notice the last thing said,
        // [`aterm_phase::ScreenReader::housekeeping`]): nobody's message was
        // answered, and the spinner it ran under is nobody's work. Read as the
        // point read again, so nothing the last point settled moves — a task
        // done stays done, a person's exchange stays theirs, a wait keeps its
        // clock — and decided on the worker's last words above it
        // ([`aterm_phase::said_tail`]) (2026-10-15: two tabs that had reported
        // ATERM-TASK-DONE were typed `keep going` an hour later).
        let worked = worked.filter(|_| !reader.housekeeping(&screen.rows));
        // THE LIMIT THAT ANSWERED A TURN, its notice taken off the screen's
        // end before this read (Esc on the vendor's countdown: the act
        // awaited, round 16's residual (a); a person's turn asked since the
        // policy last looked, round 17's residual (a)): read in the screen's place as the
        // wall it was ([`TurnEndState::left_answer`]), so that
        // everything below — the transcript's confirmation, the memory and
        // its ledger row, the reset placed on the print, the policy's fold —
        // takes it as a wall read here.
        self.read_left_answer(&mut reading, screen, check);
        // The screen's wall acted on only where the session's transcript
        // confirms it (H6-HARNESS-1): refused, the point is an ordinary turn
        // end — before anything below reads the wall.
        self.confirm_wall(&mut reading, screen, opts, check);
        // `/compact`'s own word that there was nothing to compact, read off
        // the screen's last thing said where no wall shows, before the point
        // is folded in ([`TurnEndState::compact_answered`]). Before the
        // transcript's standing limit below: the screen's own words, which a
        // limit only the transcript shows does not unsay.
        if check == Check::Decide
            && reading.wall.is_none()
            && match reading.program {
                Program::Claude => true,
                Program::Codex
                | Program::Agy
                | Program::Muse
                | Program::Gemini
                | Program::Generic => false,
            }
            && let Some(said) = aterm_phase::phase::last_said_notice(&screen.rows)
            && !said.hook_words
        {
            self.turn_end.compact_answered(&said.text);
        }
        // A limit the screen draws none of — a relaunched or restored
        // conversation, under the vendor's own paragraph — that the agent's
        // transcript stands at is the wall (KI-1015-1 LIM-1).
        self.read_standing_limit(&mut reading, screen, worked.is_some(), opts, check);
        // The wall waited on, remembered while its rows are
        // out of view; another wall or an idle screen ends it.
        let wall_was_hidden = reading.wall.is_none();
        self.wall_memory_step(reader, &screen.rows, &mut reading, screen.seq);
        let remembered_wall = wall_was_hidden && reading.wall.is_some();
        let reset_at = match &reading.wall {
            Some(w) => {
                let text = w.reset.clone();
                // A session window says so (`session limit`, `5-hour`): five
                // hours at most. A `Usage limit reached · …` that names no
                // window is placed on the print like any other wall's.
                let session_window = w.kind == aterm_phase::WallKind::UsageSession
                    && limit::names_a_session_window(&w.message);
                // A wall the loop WATCHED: one a turn it saw run ended on, or
                // one standing where an act of its own went untaken — the wall
                // answered that act, so the limit stands now
                // ([`TurnEndState::untaken_act`]), inside the act's take
                // window or past it. Without the second, a `5am` found at
                // 17:26 with no print to go by was placed by the session rule
                // in the past, and the `keep going` it was answered with —
                // held by Claude Code, no turn, no busy read — left that
                // placement standing: typed again every 30 minutes until the
                // reset. Once an act and its words: a retry typed at a wall
                // that still shows reads the old notice first, and the answer
                // naming another reset is placed as one too. A LIMIT A CANCEL
                // LEFT read as a turn's answer ([`Session::left_read`]) is
                // printed since that turn was asked where a transcript row
                // dated it; where only the screen read it, nothing dates it
                // (a `/resume`d conversation's older notice draws the same),
                // and it is placed never more than a session window out.
                let left = self.left_read.as_ref().filter(|l| l.seq == screen.seq);
                let undated = left.is_some_and(|l| !l.dated);
                let answer = left
                    .filter(|l| l.dated)
                    .map(|l| l.answer.since)
                    .or_else(|| self.turn_end.untaken_act())
                    .map(|at| (at, reset_key(w.kind, text.as_deref())))
                    .filter(|answer| self.wall_answered.as_ref() != Some(answer));
                let answered = answer.is_some();
                if answer.is_some() {
                    self.wall_answered = answer;
                }
                self.wall_reset_at(
                    text.as_deref(),
                    w.kind,
                    session_window,
                    worked.is_some(),
                    answered,
                    undated,
                    &screen.rows,
                )
            }
            None => {
                // The wall is over once the worker worked (the policy's own
                // rule, [`TurnEndState::observe`]): the next is a new reset.
                if worked.is_some() {
                    self.wall_reset = None;
                    self.reprint_jumps = 0;
                }
                None
            }
        };
        // A limit only the transcript shows is reset where its row says.
        let reset_at = self.standing_reset(screen.seq).or(reset_at);
        // The answer of a try no loop read, dated at this loop's first read
        // (a decision's: a reread follows an act) — before its wall is
        // written, which the record of that answer precedes
        // ([`Self::date_unread_try`]).
        self.date_unread_try(&reading, &screen.rows, screen.seq);
        // ... and written down once its reset is placed, for a loop that
        // follows this one: at a reading of the wall. One with none (the
        // memory kept past it for the policy's say, [`Self::observe_point`])
        // has no reset to write, and moves nothing.
        if reading.wall.is_some() {
            self.persist_wall(screen.seq);
        }
        self.reach_seen = reading
            .wall
            .as_ref()
            .filter(|w| answered_by_the_network(w.kind))
            .map(|_| {
                opts.idle_host
                    .as_ref()
                    .map_or(Reach::Unknown, |h| h.reach())
            });
        let composer = self.composer_read(&reading, screen);
        let r = TurnEndReading::of_slept(
            &reading,
            &screen.rows,
            composer,
            worked,
            self.turn_asleep(&screen.rows),
            reset_at,
            self.rules_text(opts),
        );
        let host = opts.idle_host.as_ref();
        // This session's share of the account's herd.
        let jitter = match (&reading.wall, self.setup.sid.as_deref()) {
            (Some(w), Some(sid)) if resumes_at_reset(w.kind) => resume_jitter(
                sid,
                self.wall_reset.as_ref().and_then(|(_, u)| *u).unwrap_or(0),
                self.turn_end.timing.reset_jitter,
            ),
            _ => Duration::ZERO,
        };
        // THE MEMORY CEILING ACTED ON: the host's measure of the agent's
        // memory past `[harness] memory_ceiling` WHILE the Mac is in trouble
        // and this agent holds the most (`supervise/memory.rs`,
        // `MemoryOver::acts`), remembered for the wait's edge
        // ([`Self::memory_edge`]). Past the ceiling with memory to spare, or
        // behind a larger agent, is a warning only ([`Self::memory_warn`]).
        self.memory_seen = host
            .and_then(|h| h.memory())
            .and_then(|m| super::super::memory::judge(opts.policy.memory_ceiling, &m))
            .filter(super::super::memory::MemoryOver::acts);
        // At an idle point, or one the turn ended on a wall of its own (a
        // usage or spend limit, a lost login, an API error waited out), since
        // a weekly limit or a lost login may stand for days: the restart comes
        // first; the wall, still there on the resumed agent's screen or met
        // again at its next turn, is then decided by its own rule. The banner
        // is the memory wall already.
        let measured = self.memory_seen.filter(|_| {
            r.wall != Some(aterm_phase::WallKind::Memory)
                && matches!(r.phase, Phase::Idle | Phase::Limited { .. })
        });
        self.memory_wall = measured.is_some();
        // …in place of the screen's own wall: its act is the memory's now,
        // handed back when the measure falls under the ceiling again.
        self.memory_over_wall =
            self.memory_wall && (r.wall.is_some() || matches!(r.phase, Phase::Limited { .. }));
        // The renewable login, asked only where the logins' lifecycle can act
        // on it; and, at its lost login, whether the process is still on a
        // login a renewal replaced: what that asks of the owner where nothing
        // waits for the renewal. The host's stamp is read before that answer,
        // so a renewal that lands after it is news to the point a decision's
        // read decides on it ([`Self::login_seen`]); a read for the loop's
        // books after the decision leaves that alone.
        let login_label = host
            .filter(|_| {
                opts.policy.logins
                    && match r.program {
                        Program::Claude => true,
                        Program::Codex
                        | Program::Agy
                        | Program::Muse
                        | Program::Gemini
                        | Program::Generic => false,
                    }
            })
            .and_then(|h| h.login_label());
        // …and at the `/login` dialog or `Login interrupted` of the loss this
        // loop tracks: a renewal made while that `/login` was up is followed
        // once it is left, never told as a sign-in to make again.
        let login_track = self.turn_end.login_track().is_some();
        let lost = host.filter(|_| {
            login_label.is_some()
                && (r.wall == Some(aterm_phase::WallKind::Auth)
                    || (login_track && (r.login_screen.is_some() || r.login_interrupted)))
        });
        let stamp = lost.map(|h| h.account_stamp());
        let login_superseded = lost.is_some_and(|h| h.login_superseded());
        if check == Check::Decide {
            self.login_seen = stamp.filter(|_| !login_superseded);
        }
        // The method the session's login already is, asked only where a lost
        // login or its `/login` dialog stands: what the owner's notice says
        // aterm chooses on the method picker, or leaves to them.
        let login_method = host
            .filter(|_| {
                super::super::policy::login::carried(r.program)
                    && (r.wall == Some(aterm_phase::WallKind::Auth)
                        || r.login_screen.is_some()
                        || r.login_interrupted)
            })
            .and_then(|h| h.login_method());
        // Another tab's sign-in of the same login, asked at its lost login
        // alone — its row, or the `/login` dialog or `Login interrupted` of
        // the loss this loop tracks: one standing is waited on, and the
        // login back there delivers this tab's turn again
        // ([`TurnEndReading::login_shared`]), the carrier's own left included.
        let login_shared = host
            .filter(|_| {
                super::super::policy::login::carried(r.program)
                    && (r.wall == Some(aterm_phase::WallKind::Auth)
                        || (login_track && (r.login_screen.is_some() || r.login_interrupted)))
            })
            .and_then(|h| h.login_shared());
        let mut reading = TurnEndReading {
            jitter,
            reach: self.reach_seen.unwrap_or_default(),
            // The later of the last `status`, a draft change and THIS read's
            // own stamp: every screen read carries `human_ms`, and a point is
            // folded in before any `status` is read.
            person: self
                .person_ago(self.fx.mono())
                .into_iter()
                .chain(screen.human.ago())
                .min(),
            // Never while a limit episode stands: the upgrade types nothing
            // at a limit, and the wall is the loop's to wait out
            // ([`crate::supervise::IdleHost::limited`]).
            upgrading: self.limit.is_none() && host.is_some_and(|h| h.owns_turn_end()),
            restartable: host.is_some_and(|h| h.can_restart()),
            resume: host.and_then(|h| h.resume_command()),
            login_label,
            login_superseded,
            login_method,
            login_shared,
            // The screen's launch card, or the host's record of whose turns
            // the conversation holds (the harness's own are no task).
            taskless: r.taskless || host.is_some_and(|h| h.taskless()),
            // …and whether that record says the task is finished, read at the
            // points the verdict can move ([`TurnEndState::reads_record`]).
            finished_record: self
                .turn_end
                .reads_record(&r)
                .then(|| host.and_then(|h| h.finished()))
                .flatten(),
            // The session's word as the loop last read it, else the table's:
            // what the policy's memory folds this point in under
            // ([`TurnEndState::observe`]). A decision reads it fresh where it
            // would act ([`Self::decide_to_act`]).
            autopilot: self.autopilot(opts),
            // Credits carry the session: the reset the notice they started
            // on named, kept from the read that saw it, and a notice that
            // stands without being drawn ([`Self::note_credits`]).
            credits: self.credits_carry(reader.credits(&screen.rows).as_ref()),
            ..r
        };
        // …and, where it was read, while an act of the policy's stands
        // untaken, the turn that record ends on: the act's own prompt there,
        // closed, is its answer, however fast it came
        // ([`TurnEndState::observe`]).
        if reading.finished_record.is_some()
            && (self.turn_end.untaken_act().is_some() || self.turn_end.land_act_seeded())
        {
            let now_ms = self.now_unix_ms();
            reading.record_turn = host
                .and_then(|h| h.last_turn())
                .and_then(|t| RecordTurn::of(&t, now_ms));
        }
        // …and, where those words claim the worker is done but their message
        // is taller than the rows read (its head above them, a need or an
        // open item possibly with it), the message read whole: the grid and
        // the history above it up to its head ([`Self::said_read_whole`]).
        // Where that cannot be read, the words land nothing
        // ([`TurnEndReading::said_cut`]). Every harness's message is read so,
        // by its own reader: Claude Code's, Gemini CLI's, agy's and Muse
        // Code's (the recheck of d88a7bf289 found the cut read on Claude Code
        // alone, and a tall report on the others landed on its last rows).
        //
        // THE WHOLE TURN is read so too where the row that opened it is
        // above the rows read, its last message whole on them
        // (`ScreenReader::turn_whole`, by each harness's reader: Claude
        // Code's `❯` row, Gemini CLI's band, agy's rule over the message,
        // Muse Code's `❯` row): the history up to that row, the need an
        // earlier message of the turn said with it
        // ([`TurnEndReading::turn_said`]; the recheck of d88a7bf289 found a
        // NEEDS YOU said before a tool call, above the screen, never read,
        // and that of ea564676a3 the earlier words read on Claude Code
        // alone).
        // Where it cannot be read so — its start further up than the history
        // is looked through, no history to read — the turn is as cut as a
        // message is: its words land nothing ([`TurnEndReading::said_cut`];
        // the recheck of ea564676a3 found a long landing turn, many git and
        // test calls tall, landed over a need said at its start). The point
        // gets Land's nudge, whose short reply is read whole. A cut message
        // is still read up to its own head, for the words the nudge names.
        //
        // NOT ONLY WHERE THE LAST WORDS CLAIM DONE: wherever a turn that ran
        // ends at a point that folds its needs in or may keep a landing on
        // the record ([`TurnEndState::reads_turn_whole`]).
        let reader = reading.program.reader();
        let turn_cut = reading.said_tail.is_some() && !reader.turn_whole(&screen.rows);
        if check == Check::Decide
            && (reading.said_cut || turn_cut)
            && (super::super::policy::turn_end::says_done(reading.said_tail.as_deref())
                || self.turn_end.reads_turn_whole(&reading, worked.is_some()))
        {
            let turn = self.said_read_whole(screen, reader, true);
            // Read up to the row that opened it, or to the history's first
            // row (`line` reads absolute rows: one evicted answers `ERR
            // evicted`, and the read gives up, so a read that reached row 0
            // read all there is).
            let turn_read = turn.is_some();
            let read = turn.or_else(|| {
                reading
                    .said_cut
                    .then(|| self.said_read_whole(screen, reader, false))
                    .flatten()
            });
            if let Some(rows) = read {
                reading.said_cut = !reader.said_whole(&rows);
                if !reading.said_cut {
                    reading.said_tail = reader.said_tail(&rows);
                    reading.turn_said = reader.turn_said_before(&rows);
                    // The message that opened the turn, read with it
                    // ([`TurnEndReading::opened_by`]).
                    if let Some(opened) = reader.turn_opened_by(&rows) {
                        reading.opened_by = Some(super::super::approvals::words_lead(&opened));
                    }
                }
            }
            // A turn whose start could not be read lands nothing.
            reading.said_cut |= turn_cut && !turn_read;
        }
        // …and, where the worker's words claim it is done (a done report, a
        // defense, the record's finish), what the session's repository holds
        // that no remote main has, by the host: named in the point's act, and
        // under Land no defense lands the task over it ([`Session::repo_read`]).
        if check == Check::Decide {
            reading.repo = self.repo_read(&reading, host, worked.is_some());
        }
        // …and whether a person's own exchange an earlier loop left to them
        // is still the conversation's last turn, read at those same points
        // and only while one is open (RR3-TE-2).
        // At a wall, the row an earlier loop wrote where the person's
        // exchange met it is read too (RR4-TE-2), and only there.
        if check == Check::Decide
            && !self.left_keys.is_empty()
            && self.turn_end.reads_record(&reading)
        {
            // The turn is keyed by the record the finish was just read from,
            // as far back as that read reaches: a 256 KiB tail under an idle
            // compaction's block names no turn (the merge review's XR-1).
            // The wall's row carries the exchange's work before the wall
            // (F3 of the merge review), which the retry's answer adds to.
            // A row names the turn by its last row's key or by the prompt
            // that opened it ([`crate::harness::upgrade::LastTurn::names`]).
            let walled = reading.wall.is_some();
            let turn = reading
                .finished_record
                .and(host.and_then(|h| h.last_turn()))
                .filter(|t| t.key().is_some())
                .or_else(|| Self::last_turn(opts, reading.said_tail.as_deref()));
            // A row whose retry was typed names the retry's turn, its work
            // the work before the wall and the retry's own
            // ([`approvals::LEFT_RETRIED`]): met by the wall again, the
            // exchange goes on as the loop that typed the retry holds it.
            let at_wall = turn.as_ref().filter(|_| walled).and_then(|turn| {
                self.left_keys.iter().rev().find_map(|k| {
                    approvals::parse_left_at_wall(k)
                        .filter(|(key, _)| turn.names(key))
                        .map(|(_, work)| work)
                        .or_else(|| {
                            let (_, wall) = approvals::parse_left_retried(k)?;
                            let before = approvals::parse_left_at_wall(wall)?.1;
                            turn.names(k).then(|| {
                                Some(
                                    before
                                        .unwrap_or_default()
                                        .saturating_add(turn.ended.unwrap_or_default()),
                                )
                            })
                        })
                })
            });
            reading.left_record = turn
                .as_ref()
                .is_some_and(|t| self.left_keys.iter().any(|k| t.names(k)))
                || at_wall.is_some();
            reading.left_work = at_wall.flatten();
        }
        // A remembered notice supplies the standing limit, not a new word
        // from the vendor. Its old countdown must not replace the current
        // screen's cancellation, model choice, or continuation. Include the
        // host's task history before judging whether a task is still owed.
        // A TURN SERVED SINCE A LIMIT WHOSE NOTICE LEFT (below) is read under
        // the remembered wall too, before it is judged: the screen the shell
        // keeps busy draws no notice to order the vendor's continuation by,
        // and the transcript's reply after the limit's notice ends the wait
        // as on an idle screen ([`TurnEndState::read_remembered_wall`]).
        // Measured before (the adversarial verifier of the limit's-end fix,
        // D1): the transcript was asked only with no wall on the reading, the
        // wait stood, and `keep going` went into the reply's done report.
        let reads_served = |s: &Self, reading: &TurnEndReading| {
            check == Check::Decide
                && reading.since_limit.is_none()
                && reading.worked.is_none()
                && match reading.program {
                    Program::Claude => true,
                    Program::Codex
                    | Program::Agy
                    | Program::Muse
                    | Program::Gemini
                    | Program::Generic => false,
                }
                && s.turn_end.waits_on_named_reset()
        };
        if remembered_wall {
            if reads_served(self, &reading) {
                reading.served_in_transcript = self.served_in_transcript(opts);
            }
            reading = self.turn_end.read_remembered_wall(reading);
        }
        // …read, at an idle point (a wall's own included, whose kind it takes
        // the place of) as the wall Claude Code's critical-memory banner is:
        // the same restart at the same idle point, or the same escalation
        // where no host makes one, with the measure as its fact. Never at a
        // box or a question: its answer is not held back for a restart that
        // cannot be made there.
        if let Some(over) = measured {
            reading.wall = Some(aterm_phase::WallKind::Memory);
            reading.wall_message = over.fact();
            // …which stands, whatever the memory of a limit said.
            reading.wall_remembered = false;
        }
        // A TURN SERVED SINCE A LIMIT WHOSE NOTICE LEFT (a usage window, a
        // model bucket), no read having seen it busy: where the screen draws
        // no limit's notice to order it by, the session's transcript can —
        // the limit's own notice row and a reply after it — asked at a
        // decision's read, at an idle point or a question, while the policy
        // waits on the reset the notice named (under a remembered wall, read
        // above already).
        if !remembered_wall
            && reading.wall.is_none()
            && matches!(reading.phase, Phase::Idle | Phase::Question)
            && reads_served(self, &reading)
        {
            reading.served_in_transcript = self.served_in_transcript(opts);
        }
        // …and where the screen DOES draw one, a served turn or the vendor's
        // continuation under it, naming this limit's reset clock: the
        // notice is this limit's only where the conversation the tab holds
        // now has this limit's notice row (a `/resume`d conversation's older
        // notice names the same clock with no date; the residual review's R2).
        if check == Check::Decide
            && reading.since_limit.as_ref().is_some_and(|s| {
                s.served || reading.auto_continue == Some(aterm_phase::AutoContinue::Fired)
            })
            && reading.wall.is_none()
            && reading.worked.is_none()
            && match reading.program {
                Program::Claude => true,
                Program::Codex
                | Program::Agy
                | Program::Muse
                | Program::Gemini
                | Program::Generic => false,
            }
            && matches!(reading.phase, Phase::Idle | Phase::Question)
            && self.turn_end.waits_on_named_reset()
        {
            reading.notice_elsewhere = self.notice_elsewhere(opts);
        }
        if tell {
            self.account_tells(&mut reading, screen.seq, opts);
        }
        // The usage tap's reading, at a decided idle turn end of a Claude
        // Code (`crate::harness::tap`): the host's to read and record; it
        // changes nothing of this point.
        if check == Check::Decide
            && match reading.program {
                Program::Claude => true,
                Program::Codex
                | Program::Agy
                | Program::Muse
                | Program::Gemini
                | Program::Generic => false,
            }
            && reading.phase == Phase::Idle
            && let Some(host) = host
        {
            host.usage_tap();
        }
        reading
    }

    /// The limit a cancel left, read in `reading`'s place where it answered
    /// a turn ([`TurnEndState::left_answer`]: an act awaited, or anyone's
    /// asked since the policy last looked) and the screen's own reader reads
    /// no wall: the reading's wall and its phase (`limited`, as the reader
    /// reads the same notice before the cancel). A decision's read asks the
    /// policy and keeps its answer for the screen ([`Session::left_read`]),
    /// which the transcript's confirmation then dates or refuses
    /// ([`Self::confirm_wall`]); a read for the loop's books after the
    /// decision ([`Check::AsDecided`]) keeps what the decision read there —
    /// the policy, having folded the answer in, no longer asks
    /// ([`TurnEndState::observe`]: the act is met, the limit tracked).
    fn read_left_answer(
        &mut self,
        reading: &mut aterm_phase::Reading,
        screen: &Screen,
        check: Check,
    ) {
        let decided = self
            .left_read
            .as_ref()
            .filter(|l| l.seq == screen.seq)
            .map(|l| l.answer.wall.clone());
        let left = match (check, decided) {
            (Check::AsDecided, Some(w)) => Some(w),
            _ => {
                let left = match reading.program {
                    Program::Claude => reading
                        .wall
                        .is_none()
                        .then(|| self.turn_end.left_answer(&screen.rows))
                        .flatten(),
                    Program::Codex
                    | Program::Agy
                    | Program::Muse
                    | Program::Gemini
                    | Program::Generic => None,
                };
                let wall = left.as_ref().map(|l| l.wall.clone());
                self.left_read = left.map(|answer| LeftRead {
                    seq: screen.seq,
                    answer,
                    dated: false,
                });
                wall
            }
        };
        if let Some(w) = left.filter(|_| reading.wall.is_none()) {
            reading.phase = Phase::Limited {
                message: w.message.clone(),
                reset: w.reset.clone(),
            };
            reading.wall = Some(w);
        }
    }

    /// A DECISION BEGINS ([`Self::turn_end_at_point`], [`Self::turn_end_now`]):
    /// what the host advised about
    /// the account for the decision before is forgotten, so this one asks
    /// again ([`Self::account_advise`]) — a wait that ran out is how a
    /// session with no account to run on learns that one is usable.
    fn account_round(&mut self) {
        self.account_seen = None;
        self.roster_seen = None;
    }

    /// THE SESSION'S ACCOUNT as every reading of a point carries it
    /// (`docs/DESIGN-claude-accounts-2026-10-01.md` §6), in this order:
    ///
    /// 1. THE TELLS. A move this session's host made and no point has
    ///    answered ([`TurnEndState::account_answer`], read off the reading
    ///    BEFORE it is folded in): served — the first turn asked under the
    ///    new login ran and ended with no wall ([`IdleHost::account_served`])
    ///    — or answered by a usage limit or a lost login
    ///    ([`IdleHost::account_answered`]). Each once a move — and once for
    ///    every loop on the session: the tell is written to the approval
    ///    ledger ([`approvals::account_answer_reason`]), where a loop that
    ///    starts after this one reads it back
    ///    ([`approvals::account_moves`]) and neither tells a bad answer a
    ///    second time nor forgets a move still to be served. A tell that
    ///    waited on the act awaited is read again after the fold that judges
    ///    it, before the host is asked ([`Self::fold_point`]); and the point
    ///    read again once an account move's relaunch is made tells nothing
    ///    ([`Self::turn_end_reread_relaunched`]).
    /// 2. THE ADVICE THIS DECISION WAS GIVEN ([`Self::account_advise`]),
    ///    for the readings of the same point built after it — an act typed,
    ///    a restart — each of which takes the answer the decision got
    ///    ([`Session::account_seen`], keyed by the screen and the point). A
    ///    tell forgets it: the host has struck the account since, and its
    ///    advice is the next one.
    ///
    /// 3. CLAUDE CODE'S EXPIRY NOTICE ([`TurnEndReading::login_notice`]),
    ///    each day count once ([`IdleHost::login_notice`]), under `[harness]
    ///    logins`.
    ///
    /// No host, one that keeps no roster (the trait's defaults), or
    /// `[harness] accounts = "off"` with `logins = "off"`: nothing is told
    /// and the reading's advice is `None` — the reading it was before
    /// accounts existed.
    fn account_tells(&mut self, reading: &mut TurnEndReading, seq: u64, opts: &SuperviseOpts) {
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        if opts.policy.logins
            && let Some(days) = reading.login_notice
            && self.login_notice_told != Some(days)
        {
            host.login_notice(days);
            self.login_notice_told = Some(days);
        }
        if opts.policy.accounts == crate::harness::accounts::Mode::Off && !opts.policy.logins {
            return;
        }
        if let Some(answer) = self.turn_end.account_answer(reading) {
            let to = self
                .turn_end
                .account_move()
                .map(|m| m.to.clone())
                .unwrap_or_default();
            match answer {
                AccountAnswer::Served => host.account_served(),
                AccountAnswer::Bad(bad) => host.account_answered(bad),
            }
            self.turn_end.account_told(answer);
            self.account_seen = None;
            self.roster_seen = None;
            self.ledger_row(
                RULE_ACCOUNT,
                approvals::Outcome::Waiting,
                &format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND),
                &approvals::account_answer_reason(&to, answer),
                seq,
            );
        }
        if let Some((at, asked, advice)) = &self.account_seen
            && *at == seq
            && self.turn_end.account_point(reading, &opts.policy) == Some(*asked)
        {
            reading.account.clone_from(advice);
        }
        if let Some((at, word)) = &self.roster_seen
            && *at == seq
        {
            reading.roster.clone_from(word);
        }
    }

    /// THE HOST'S ADVICE about the session's account ([`IdleHost::account`]),
    /// asked AFTER the point is folded into the policy's memory
    /// ([`TurnEndState::observe`]) — the fold is what ends a latch with its
    /// wall and opens a finished task again — only where it could decide
    /// something ([`TurnEndState::account_point`]), and ONCE A DECISION
    /// ([`Self::account_round`] begins one). The accounts' NEWS goes first
    /// ([`Self::account_news`]): the stamp is read before the host reads its
    /// roster, so a change that lands meanwhile moves it past what is kept
    /// and the wait that follows is decided again ([`Self::account_edge`]).
    /// Nothing under `[harness] accounts = "off"` with `logins = "off"`, and
    /// nothing with no host.
    pub(super) fn account_advise(
        &mut self,
        reading: &mut TurnEndReading,
        seq: u64,
        opts: &SuperviseOpts,
    ) {
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        if opts.policy.accounts == crate::harness::accounts::Mode::Off && !opts.policy.logins {
            return;
        }
        self.account_news(host.as_ref());
        // At a weekly usage limit, or a `/login` left at a usage limit, whose
        // the login is: what the owner's notice there may ask of them
        // ([`IdleHost::roster_word`], [`TurnEndState::reads_roster`]).
        if self.turn_end.reads_roster(reading) {
            let word = host.roster_word();
            self.roster_seen = Some((seq, word.clone()));
            reading.roster = word;
        }
        // …and the owner's `relaunch = false` where the host reads it live,
        // asked where a notice may name it: that limit's, and a renewed
        // login's ([`IdleHost::relaunch_off`]).
        if reading.roster.is_some() || reading.login_superseded {
            reading.relaunch_off = host.relaunch_off();
        }
        let Some(point) = self.turn_end.account_point(reading, &opts.policy) else {
            return;
        };
        let advice = host.account(point);
        self.account_seen = Some((seq, point, advice.clone()));
        reading.account = advice;
    }

    /// A TURN READ RUNNING ([`TurnEndState::took`]), and the ledger told so
    /// where it is the first since the point recorded last
    /// ([`approvals::RULE_TURN_RAN`]). ON A LANDED TASK, at the turn's first
    /// frame ([`TurnEndState::wake_may_keep_landing`]), the conversation's
    /// record is asked whether anyone asked it (`IdleHost::finished`): where
    /// it says nobody has since the landing — Claude Code's own wake for a
    /// background task — the turn is the task's own wake
    /// ([`TurnEndState::took_quiet`]), and the face stays landed until the
    /// point it ends at decides whether the landing holds.
    pub(super) fn took(&mut self, seq: u64) {
        if self.turn_end.wake_may_keep_landing()
            && self.stall_host.as_ref().and_then(|h| h.finished()) == Some(true)
        {
            self.turn_end.took_quiet();
            // …and what the repository holds as it begins: what its point
            // finds unchanged is none of the wake's
            // ([`TurnEndState::wake_began_over`]) — where this loop saw the
            // point the wake began after ([`TurnEndState::wake_start_owed`]).
            if let Some(host) = &self.stall_host
                && self.turn_end.wake_start_owed()
            {
                let found = host.repo_work();
                self.turn_end.wake_began_over(found);
            }
        } else {
            self.turn_end.took();
        }
        self.tell_ran(seq);
    }

    /// A BOX READ UP ([`TurnEndState::box_up`]): the worker asked something,
    /// and the ledger told so where it is the first work since the point
    /// recorded last ([`approvals::RULE_TURN_RAN`]), as a turn read running
    /// is.
    pub(super) fn box_up(&mut self, seq: u64) {
        self.turn_end.box_up();
        self.tell_ran(seq);
    }

    /// The ledger's record that the worker worked since the point recorded
    /// last, written once ([`TurnEndState::take_ran`]).
    fn tell_ran(&mut self, seq: u64) {
        if self.turn_end.take_ran() {
            self.ledger_row(
                approvals::RULE_TURN_RAN,
                approvals::Outcome::Waiting,
                "",
                approvals::TURN_RAN,
                seq,
            );
        }
    }

    /// THE POINT FOLDED IN, THEN THE HOST ASKED ([`Self::observe_point`],
    /// [`Self::account_advise`]): every decision's order. A tell that waited
    /// on the act awaited ([`TurnEndState::account_answer_awaited`]: the turn
    /// typed under the move had no busy read, so the wall the point shows
    /// said nothing yet) is read again between the two
    /// ([`Self::account_tells`]): the fold that judges that act makes the
    /// point the act's, and A LOST LOGIN AFTER A TURN ASKED UNDER THE LOGIN
    /// THE MOVE CHOSE IS TOLD — STRUCK ON THAT LOGIN — BEFORE ANY ADVICE FOR
    /// THAT POINT IS ASKED. Asked first, the host advised with the dead login
    /// unstruck — the next account, or the move back onto it on the `ok` it
    /// had before — and the tell landed after the relaunch, on the login the
    /// session was moved to. A fold that judges nothing leaves the act
    /// awaited, and the tell read again says nothing.
    pub(super) fn fold_point(
        &mut self,
        r: &mut TurnEndReading,
        now: Instant,
        seq: u64,
        opts: &SuperviseOpts,
    ) {
        let awaited = self.turn_end.account_answer_awaited(r);
        self.observe_point(r, now, seq);
        // A lost login ended back: news to the tabs that waited on its
        // sign-in ([`IdleHost::login_back`]).
        if self.turn_end.take_login_returned()
            && let Some(host) = opts.idle_host.as_ref()
        {
            host.login_back();
        }
        // A `/login` a loop before this one typed, taken up with its dialog
        // standing: the login's one sign-in is this tab's again
        // ([`IdleHost::login_carry`]). One another tab took meanwhile is
        // theirs (the host says so in its log), and this tab's dialog is the
        // owner's to finish or leave, as it stands.
        if self.turn_end.take_login_retaken()
            && let Some(host) = opts.idle_host.as_ref()
        {
            let _ = host.login_carry();
        }
        // The point judged: its record, where it moved what a loop that
        // follows must hold ([`approvals::RULE_POINT`]).
        if self.turn_end.take_point_moved() {
            let reason =
                approvals::point_reason(self.carried(), self.turn_end.persons_esc_stands());
            self.ledger_row(
                approvals::RULE_POINT,
                approvals::Outcome::Waiting,
                "",
                &reason,
                seq,
            );
        }
        // The point is where the task's face moves (a landing, a person's
        // own exchange, a turn that opens the task again): told here, beside
        // what the point says to the journal, and on every route a point
        // takes — one the host steps at, one parked under Off or carried past
        // included.
        self.tell_face();
        if awaited {
            self.account_tells(r, seq, opts);
        }
        self.account_advise(r, seq, opts);
    }

    /// THE ROWS THAT HOLD THE LAST MESSAGE WHOLE, for a point whose message
    /// is taller than the rows read ([`TurnEndReading::said_cut`]): the whole
    /// grid (`text --json`), and where what bounds the message is above it
    /// too (`reader`'s [`ScreenReader::said_whole`]), the history above the
    /// grid up to the row `reader` stops at
    /// ([`ScreenReader::said_history_stop`], [`Self::rows_up_to`]). With
    /// `turn`, the rows that hold its WHOLE TURN: up to the row that opened
    /// it ([`ScreenReader::turn_whole`], [`ScreenReader::turn_history_stop`]),
    /// or the history's first row. `None`
    /// where they cannot be read — the grid no longer the screen's, the head
    /// further up than the history is looked through, a request not served
    /// — and the words stay cut.
    fn said_read_whole(
        &mut self,
        screen: &Screen,
        reader: &'static dyn ScreenReader,
        turn: bool,
    ) -> Option<Vec<String>> {
        // The screen read from its first row is the whole grid already (the
        // history read checks it still is); else the grid is read.
        let whole = if screen.first == 0 {
            screen.rows.clone()
        } else {
            let whole = self.grid().ok()?;
            let same = whole
                .rows
                .get(screen.first..)
                .is_some_and(|rows| super::wall_confirm::same_rows(rows, &screen.rows));
            if !same {
                return None;
            }
            whole.rows
        };
        let read = if turn {
            reader.turn_whole(&whole)
        } else {
            reader.said_whole(&whole)
        };
        if read {
            return Some(whole);
        }
        let stop = |r: &str| {
            if turn {
                reader.turn_history_stop(r)
            } else {
                reader.said_history_stop(r)
            }
        };
        let mut joined = self.rows_up_to(&whole, &stop).ok().flatten()?;
        joined.extend(whole);
        Some(joined)
    }

    /// WHAT THE SESSION'S REPOSITORY HOLDS THAT NO REMOTE MAIN HAS, at a
    /// point `reading` reads (`TurnEndReading::repo`): asked of the host
    /// (`IdleHost::repo_work`) only where the worker's words claim it is done
    /// ([`super::super::policy::turn_end::says_done`]: a done report, a
    /// `Nothing left:` defense, a retired token) or the record says the task
    /// finished — the points whose act names what it holds (Cruise's defend
    /// prompt, Land's nudge) and the only ones a land act's clean defense can
    /// land the task at, which it does only over a clean repository (the
    /// landing's fourth condition); nothing elsewhere. The same words read again
    /// within [`REPO_REPROBE`], no turn run since (`worked`), keep the last
    /// answer ([`Session::repo_seen`]): a point re-read at each wake asks
    /// `git` once, and words that changed, a turn run or the time past ask
    /// it afresh — a commit and push the worker made since is seen. A
    /// LANDING'S OWN WORDS read again, no turn run since — the landing a loop
    /// before this one landed, or this loop's — ask nothing
    /// ([`TurnEndState::landing_words_read`]): the landing was judged over a
    /// clean repository, and what is there now is someone else's work.
    fn repo_read(
        &mut self,
        reading: &TurnEndReading,
        host: Option<&Arc<dyn IdleHost>>,
        worked: bool,
    ) -> Option<crate::harness::repo_work::RepoWork> {
        let tail = reading.said_tail.as_deref();
        if !(super::super::policy::turn_end::says_done(tail)
            || reading.finished_record == Some(true))
        {
            return None;
        }
        // A landing's own words read again, no turn run since: nothing to
        // ask — work found now is someone else's
        // ([`TurnEndState::landing_words_read`]).
        if !worked && self.turn_end.landing_words_read(tail) {
            return None;
        }
        let host = host?;
        let mark = said_mark(tail);
        let now = self.fx.mono();
        if let Some((_, _, found)) = self.repo_seen.as_ref().filter(|(m, at, _)| {
            !worked && *m == mark && now.saturating_duration_since(*at) < REPO_REPROBE
        }) {
            return found.clone();
        }
        let found = host.repo_work();
        self.repo_seen = Some((mark, now, found.clone()));
        found
    }

    /// What the loop holds that its next row marks for a loop that follows
    /// ([`approvals::Carried`]): the short streak and the retry ladder of an
    /// API error's wall it waits at, its appearance
    /// on the wall clock ([`TurnEndState::retry_track`]).
    fn carried(&self) -> approvals::Carried {
        approvals::Carried {
            short: self.turn_end.short_streak(),
            retry: self
                .turn_end
                .retry_track()
                .map(|(kind, attempts, ups, since)| approvals::RetryMark {
                    kind,
                    attempts,
                    ups,
                    since_ms: self.unix_ms_of(since),
                }),
        }
    }

    /// THE TASK'S FACE (`docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §9.4.2), told to the window's host where it moved since the loop last
    /// told it ([`TurnEndState::face_moved`]): at each point folded in
    /// ([`Self::fold_point`]) and at the first live read of someone else's
    /// turn (`Session::await_turn_from`'s live read, [`TurnEndState::took`]).
    /// A face told again is no move, and with no host (a CLI `watch` or
    /// `supervise`) nothing is told.
    pub(super) fn tell_face(&mut self) {
        if let Some(face) = self.turn_end.face_moved()
            && let Some(host) = &self.stall_host
        {
            host.task_face(face);
        }
    }

    /// THE TASK'S FACE AT THE LOOP'S START ([`TurnEndState::face_start`]),
    /// told to the window's host whatever it is — a seeded landing's with
    /// the time its end was noted ([`TurnEndState::face_start_noted`]).
    pub(super) fn tell_start_face(&mut self) {
        let face = self.turn_end.face_start();
        if let Some(host) = &self.stall_host {
            match self.turn_end.face_start_noted() {
                Some(noted_ms) => host.task_face_noted(face, noted_ms),
                None => host.task_face(face),
            }
        }
    }

    /// THE ACCOUNTS' NEWS, read off the host's stamp
    /// ([`IdleHost::account_stamp`]): `true`, once, when it has moved since
    /// this session last read it — and then what the policy remembers of a
    /// move the host refused is let go ([`TurnEndState::account_news`]), so
    /// a remedy the owner applied (`accounts login`, `accounts use`) reaches
    /// a tab whose move was refused. A refusal with no news after it stays
    /// as it was. THE FIRST READ IS NO NEWS: the stamp counts
    /// since the host started, and read as news it ended, at the first
    /// decision of every loop that started later, the latch the loop before
    /// left in the ledger ([`TurnEndState::seed_account_unmade`]).
    pub(super) fn account_news(&mut self, host: &dyn IdleHost) -> bool {
        let stamp = host.account_stamp();
        if self
            .account_stamp
            .replace(stamp)
            .is_none_or(|read| read == stamp)
        {
            return false;
        }
        self.turn_end.account_news();
        true
    }

    /// Whether the composer holds a PERSON'S draft, judged where it is DRAWN.
    /// On a program that draws no placeholder (`placeholder` false,
    /// [`ScreenReader::placeholder`]: agy, Muse Code) every word on it was
    /// typed. Elsewhere a row of text on the caret row is
    /// Claude's dim suggestion or a draft: the cursor's COLUMN says which
    /// where it can — a program parks it at column 2, after the caret, on an
    /// empty composer, and a typed draft leaves it inside the text — and at
    /// column 2 the `cell` there decides (the placeholder is drawn DIM,
    /// [`crate::supervise::screen::cell_is_dim`], or in the colour its
    /// program draws it in, `hint_fg` — Muse Code's prompt hint,
    /// [`aterm_phase::ScreenReader::placeholder_fg`]; a typed draft is not, and
    /// neither is one whose caret was moved home: ←, Home, ctrl-a, measured on
    /// Claude Code 2.1.280 and 2.1.281). The cursor's ROW is not trusted: a
    /// window that re-gridded twice in a millisecond leaves an
    /// alternate-screen program's rows one higher than it drew them and its
    /// cursor on the row below the prompt (seen live for hours, through
    /// exactly the idle stretch a wall is), and read by the row, Claude's own
    /// suggestion was a person's draft and the wall's act became a bare
    /// Enter on ghost text instead of `keep going`. A second row of text is a draft.
    /// A cell the server does not answer or whose style is malformed is UNKNOWN
    /// (`None`), regardless of the cursor row: neither typed over nor submitted.
    /// `Some(false)` is a known empty/placeholder composer (or no composer),
    /// `Some(true)` is proven typed text. `composer` is the
    /// region the reader already read off `screen` ([`aterm_phase::Reading::composer`]).
    pub(in crate::supervise) fn composer_draft(
        &mut self,
        composer: Option<&(usize, Vec<String>)>,
        placeholder: bool,
        hint_fg: Option<&str>,
        screen: &Screen,
    ) -> Option<bool> {
        let Some((caret, lines)) = composer else {
            return Some(false);
        };
        if lines.iter().skip(1).any(|l| !l.is_empty()) {
            return Some(true);
        }
        if lines.first().is_none_or(String::is_empty) {
            return Some(false);
        }
        if !placeholder || screen.cursor_col != 2 {
            return Some(true);
        }
        let row = (screen.first + caret).to_string();
        match self.call(&["cell", &row, "2"]) {
            Ok(r) if r.ok() => crate::supervise::screen::cell_is_placeholder(&r.stdout, hint_fg)
                .map(|placeholder| !placeholder),
            _ => None,
        }
    }

    /// What the loop reads of the composer's cells on `screen`, read by
    /// `reading`: whether its text was typed ([`Self::composer_draft`]) and,
    /// over Claude Code's completion menu, its selection's colours
    /// ([`Self::menu_colours`]).
    fn composer_read(&mut self, reading: &aterm_phase::Reading, screen: &Screen) -> ComposerRead {
        let typed = self.composer_draft(
            reading.composer.as_ref(),
            reading.program.reader().placeholder(),
            reading.program.reader().placeholder_fg(),
            screen,
        );
        let menu_fg = match reading.program {
            Program::Claude if typed == Some(true) => self.menu_colours(screen),
            Program::Claude
            | Program::Codex
            | Program::Agy
            | Program::Muse
            | Program::Gemini
            | Program::Generic => None,
        };
        ComposerRead { typed, menu_fg }
    }

    /// A DRAFT THAT HELD THE POINT ([`Self::draft_held`]), read again at a
    /// look at the point already handed over: its composer read otherwise —
    /// the person sent it, cleared it or typed another, or its cells read now
    /// — the point is decided again at once ([`Self::turn_end_now`]), the
    /// badge the draft raised taken down first ([`Self::close_box`]), so the
    /// act, or a new escalation, is the new composer's. The same composer, or
    /// one whose cells were not read, holds the point on. `true` when it was
    /// decided again. Only the composer is read: what else the point is made
    /// of is its review key, the same.
    pub(super) fn draft_look(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some(held) = self.draft_held.clone() else {
            return Ok(false);
        };
        let reading = self
            .reader(&seen.screen.rows)
            .read(&seen.screen.rows, Some(seen.screen.cursor_col));
        let read = self.composer_read(&reading, &seen.screen);
        let now = TurnEndReading::of(&reading, &seen.screen.rows, read, None, None, None).composer;
        if now == held || now == Composer::Unknown {
            return Ok(false);
        }
        self.draft_held = None;
        if held != Composer::Unknown {
            self.close_box(seen.screen.seq, review)?;
        }
        self.turn_end_now(seen, opts, allow, review)?;
        Ok(true)
    }

    /// THE COMPLETION MENU'S SELECTION, where Claude Code draws it in colour
    /// only ([`aterm_phase::phase::CompletionMenu::colour_only`]: 2.1.289 and
    /// before draw no pointer, and a person's arrow, or a selection the vendor
    /// kept as the draft changed, leaves the rows as they were): the colour of
    /// each entry's first glyph, read with `cell` at the rows `screen` read, in
    /// entry order — `None` for a cell not answered, malformed, or holding
    /// another glyph than the entry's (the screen moved under the read). `None`
    /// where no such menu stands. What the colours say is
    /// [`aterm_phase::phase::CompletionMenu::runs`]'s; a press made on it is
    /// fenced on the screen's generation, which a later arrow moves.
    fn menu_colours(&mut self, screen: &Screen) -> Option<Vec<Option<String>>> {
        let menu = aterm_phase::phase::completion_menu(&screen.rows).filter(|m| m.colour_only())?;
        let mut fg = Vec::with_capacity(menu.entries.len());
        for e in &menu.entries {
            let row = (screen.first + e.row).to_string();
            let col = e.col.to_string();
            fg.push(
                match (self.call(&["cell", &row, &col]), e.word.chars().next()) {
                    (Ok(r), Some(glyph)) if r.ok() => {
                        crate::supervise::screen::cell_glyph_fg(&r.stdout, glyph)
                    }
                    _ => None,
                },
            );
        }
        Some(fg)
    }

    /// The decision at a NEW point: the point folded into the policy's
    /// memory ([`TurnEndState::observe`]) — once, here — the host asked for
    /// its advice about the account on the memory so folded
    /// ([`Self::account_advise`]), then [`decide_turn_end`]; and when that is
    /// an act, decided again on a fresh `status` ([`Self::decide_to_act`]). A
    /// loop watching behind another supervisor's claim decides nothing: that
    /// one answers the session.
    pub(super) fn turn_end_at_point(
        &mut self,
        point: &Turn,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> Result<TurnEndAction, Fail> {
        if !self.held {
            self.turn_end_due = None;
        }
        self.draft_held = None;
        if self.claim.watching_behind().is_some() {
            return Ok(TurnEndAction::Nothing);
        }
        self.account_round();
        // THE END OF A TURN THE LOOP BEFORE THIS ONE READ RUNNING, this loop's
        // first point ([`TurnEndState::take_gap`]): a turn that ran, its work
        // what Claude Code's done row says (the reading takes the larger of
        // the two, as for the loop that read it running), none where no done
        // row says — never a point no read saw run.
        let worked = worked.or_else(|| self.turn_end.take_gap().then_some(Duration::ZERO));
        // THE TURN AN ACT NO READ SAW TAKEN RAN, ended here
        // ([`Self::unseen_act_work`]): its work Claude Code's done row.
        let worked = worked.or_else(|| self.unseen_act_work(&point.screen.rows));
        // A TURN ENDED OVER A LANDING (the landed task's own wake, or anyone's
        // turn): the fold may keep the landing by the session's word
        // ([`TurnEndReading::autopilot`]: the conversation's record keeps one
        // only under Land), so the word is read fresh for it — one bare
        // `meta` — never the one cached since the landing. A loop idle on
        // the landed screen reads no word, so a switch made there (Land to
        // Cruise on the chip) was unread at the wake's end: the wake kept
        // the landing under Cruise, and nothing went on.
        // AND A WORD NEVER READ ([`AutopilotWord::read`]: the loop's start-up
        // read was lost) is read here, before the point folds in under it:
        // the table's word is no session's, and a seeded landing is taken up
        // only under Land — folded under the table's Cruise, a successor
        // dropped the session's landing and typed the done check into it
        // (the final check of 2026-10-20, W1). Lost again, the point is the
        // outage's to ride out and is decided again after it.
        if !self.autopilot.read()
            || worked.is_some() && (self.turn_end.landed() || self.turn_end.landed_seed_armed())
        {
            self.autopilot.begin();
            self.autopilot_read(opts)?;
        }
        let mut r = self.turn_end_reading(&point.screen, worked, opts);
        // Decided while the host owns or withholds the turn ends: decided
        // again once it does neither ([`Self::host_held`]).
        self.host_held = r.upgrading || host_withholds(opts);
        let now = self.policy_now();
        self.fold_point(&mut r, now, point.screen.seq, opts);
        // The task record, once a loop: a manager waiting, or a done this
        // loop takes on ([`Self::task_seed`]).
        self.task_seed(opts)?;
        self.decide_to_act(r, opts, now, &point.screen)
    }

    /// THE WORK OF THE TURN THE ACT AWAITED BEGAN, no read since it was typed
    /// having seen that turn run ([`TurnEndState::untaken_act`]): the done
    /// row Claude Code drew at this point, where the turn it closes ended
    /// after the act and began at it ([`act_turn_work`]). A gap no read
    /// covered (a handoff with no loop in it) is read as the loop that
    /// watched the turn read it, the act's work its turn's (ATERM-KI-1016-1).
    /// The act's time is the one the wall clock read when it was typed
    /// ([`TurnEndState::untaken_act_typed`]), a sleep since it moved back
    /// with every stamp of the policy's: read off the take window's instant,
    /// an act typed at 10:00 with a 30-minute sleep after it read as typed at
    /// 10:30, and the 42 minutes its turn's done row said were read as a turn
    /// running before it. A sleep of the machine inside the turn is no work
    /// of the turn's, though the done row counts it ([`Self::turn_asleep`]).
    /// `None` where no done row says so, and on another program's screen.
    fn unseen_act_work(&mut self, rows: &[String]) -> Option<Duration> {
        let at = self.turn_end.untaken_act_typed()?;
        match self.reader(rows).program() {
            Program::Claude => {}
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                return None;
            }
        }
        let work = done_row_work(rows)?;
        let now = self.now_unix();
        let offset = self.local_offset_s();
        let ended = limit::done_row_ended(rows, now, offset)?;
        act_turn_work(
            self.unix_of(at),
            ended,
            work,
            self.turn_asleep(rows).most,
            self.turn_end.timing.min_work,
        )
    }

    /// THE SLEEP OF THE MACHINE INSIDE THE TURN Claude Code's done row on
    /// `rows` closes, as this loop knows its sleeps ([`Self::sleeps`],
    /// `SleepBook::in_turn`): the turn's span is the done row's (`✻ Baked for
    /// 30m 3s`, timed on the wall clock, which a sleep does not stop), ending
    /// in the minute its clock names (`· done 10:30 AM`), no later than now
    /// — or at this read, with no clock drawn. Any turn's: the act's, a
    /// person's, one a read saw run before the sleep. The most that can lie
    /// inside the span and the least that must ([`InTurn`]): that minute
    /// reaches back before the turn began, and a sleep the book cannot place
    /// may lie before it. Zero with no sleep known (nothing is read then) or
    /// no done row. A span reaching back past this loop's first look where
    /// the books could not say how long the machine slept before it
    /// ([`Self::sleep_unknown`]) is read so all the same, and the journal
    /// says so once ([`Self::say_sleep_unknown`]).
    pub(super) fn turn_asleep(&mut self, rows: &[String]) -> InTurn {
        if self.sleeps.is_empty() && self.sleep_unknown.is_none() {
            return InTurn::default();
        }
        let Some(work) = done_row_work(rows) else {
            return InTurn::default();
        };
        let now_ms = self.now_unix_ms();
        let now = now_ms.div_euclid(1000);
        let offset = self.local_offset_s();
        let (lo, hi) = limit::done_row_ended(rows, now, offset)
            .map_or((now_ms, now_ms), |minute| {
                (minute * 1000, (minute * 1000 + 59_999).min(now_ms))
            });
        let span = i64::try_from(work.as_millis()).unwrap_or(i64::MAX);
        if self
            .sleep_look
            .as_ref()
            .is_some_and(|look| lo.saturating_sub(span) < look.at_ms)
            && let Some(why) = self.sleep_unknown.take()
        {
            self.sleep_unsaid = Some(why);
        }
        self.sleeps.in_turn(work, lo, hi)
    }

    /// THE ANSWER OF A LIMIT'S TRY NO LOOP READ, DATED at this loop's first
    /// decision read ([`TurnEndState::date_unread_try`], ATERM-KI-1016-1, "a
    /// loop that follows another"), AT THE LATEST its sign allows: where the
    /// session's transcript ends on a notice written within
    /// [`TRY_ANSWER_SLACK`] of the try ([`TurnEndState::notice_written`]:
    /// this read's), by that second and the slack; where Claude Code's done
    /// row on `rows` says a turn of no more than that ended in a minute that
    /// reaches the try (its start no more than the slack past it), by that
    /// minute's end; both, the earlier. The loop that typed the try first
    /// read the screen no later than the answer drew, so a follower dating it
    /// there is never sooner than that loop; dated at the try, an answer that
    /// drew 30 s after it in the same minute had the follower type the next
    /// try 20 s sooner. The read is past the take window that first read
    /// would have opened, or nothing is dated: inside it (a bound past this
    /// read among it), the window from this loop's own read stands, which
    /// types no sooner. Nothing else dates it: a line Claude Code held (no
    /// turn, nothing written) leaves the done row and the notice of the turn
    /// before, the try's own turn that worked and met the limit again a span
    /// or a time past it. The try taken up met, the ledger is told the first
    /// read it stands for ([`approvals::RULE_TRY_ANSWERED`]), so a loop that
    /// follows takes it up as this one did.
    fn date_unread_try(&mut self, reading: &aterm_phase::Reading, rows: &[String], seq: u64) {
        let Some(at) = self.turn_end.unread_try() else {
            return;
        };
        let secs = |d: Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
        let tried = self.unix_of(at);
        let now = self.now_unix();
        let slack = secs(TRY_ANSWER_SLACK);
        let by_notice = self
            .turn_end
            .notice_written()
            .filter(|t| (tried - slack..=tried + slack).contains(t))
            .map(|t| t.saturating_add(slack));
        let claude = match reading.program {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        let by_done = if claude && done_row_work(rows).is_some_and(|w| w <= TRY_ANSWER_SLACK) {
            limit::done_row_ended(rows, now, self.local_offset_s())
                .filter(|&minute| minute <= tried + slack && minute + 59 >= tried)
                .map(|minute| minute + 59)
        } else {
            None
        };
        let latest = match (by_notice, by_done) {
            (Some(n), Some(d)) => Some(n.min(d)),
            (n, d) => n.or(d),
        };
        let take = secs(self.turn_end.timing.take_within);
        let answered = latest
            .filter(|&t| now >= t.saturating_add(take))
            .map(|t| Duration::from_secs(u64::try_from(t - tried).unwrap_or(0)));
        let wall = reading.wall.as_ref().map(|w| w.kind);
        if let Some(read) = self.turn_end.date_unread_try(wall, answered) {
            let read_ms = self.unix_ms_of(read);
            self.ledger_row(
                approvals::RULE_TRY_ANSWERED,
                approvals::Outcome::Waiting,
                "",
                &approvals::try_answered_reason(read_ms),
                seq,
            );
        }
    }

    /// [`decide_turn_end`] on `r`; when it says to type, the session's program
    /// and a person's last keystroke are read fresh (`status`,
    /// [`Self::foreground_program`]) and the point decided again on them, so
    /// a status read is paid only where there is something to type.
    ///
    /// A question asked in prose ([`TurnEndReading::asked`]) is decided by the
    /// session's priority word where it has one ([`Self::prose_policy`]):
    /// `recommended`, `long-term` or `short-term` answers it by that priority,
    /// over the table's `question_priority` and over its `answer_questions =
    /// false` (contract decision 7: the prose path and the dialog path resolve
    /// the same word). The word is read (one bare `meta`) only where it can
    /// decide: a point the table would answer, or hold only because the owner
    /// switched the answers off ([`ANSWERS_OFF`]); never at a wait, a cap or
    /// a wall. The session's `ask`, and a word this build does not know, hand
    /// over its dialogs only (`aterm drive answer` answers them, `meta unset
    /// questions` hands them back): a question in prose has neither, so under
    /// those words it is the table's.
    ///
    /// THE AUTOPILOT ([`TurnEndReading::autopilot`]) is decided the same way:
    /// the point is decided first at [`Autopilot::Cruise`], the most any word
    /// gives — Land and Off only take acts away, and wait where Cruise waits —
    /// and only where that decision would act (type, restart or escalate) is
    /// the session's word read ([`Self::autopilot_read`]: the bare `meta` the
    /// prose's word read, or one more where the host keeps the word; none
    /// where it keeps none) and the point decided again under it. A point
    /// Cruise waits at, or leaves alone, reads nothing; its wait's deadline
    /// decides it again, the word read then. Under Off the decision is
    /// nothing, the exceptions engine raises nothing, and the loop parks
    /// ([`Self::park_while_off`]).
    ///
    /// ON A HARNESS TAPE the decision is noted first — the fold and the
    /// reading it is made on, for a reader's `why` — and marked after: the
    /// screen it judged (`judged`), its rule and its outcome
    /// ([`turn_end_outcome`]).
    fn decide_to_act(
        &mut self,
        r: TurnEndReading,
        opts: &SuperviseOpts,
        now: Instant,
        judged: &Screen,
    ) -> Result<TurnEndAction, Fail> {
        self.fx.note(super::super::tape::core::NOTE_TURN_END, || {
            format!("{:?}\n{r:?}", self.turn_end)
        });
        let composer = r.composer.clone();
        // The decision's RULE TRACE, where a replay asks for one: kept here
        // and handed over with the mark alone, so a decision cut short (a
        // read lost inside it, ridden out) leaves no steps to be told as the
        // next mark's.
        let mut steps = self.fx.tracing().then(Vec::new);
        // A SHADOW replay's candidate policy (`tape::shadow`; none anywhere
        // else): the point is decided again under it, on the fold and the
        // reading the recorded decision was made on, and handed over with the
        // mark — the loop goes on with its own decision, unchanged.
        let shadow = self.fx.shadow_policy();
        let mut point = shadow.as_ref().map(|_| ShadowPoint::default());
        let action = self.decide_to_act_now(r, opts, now, &mut steps, point.as_mut())?;
        if let (Some(policy), Some(point)) = (shadow, point) {
            let candidate = point.decide(&self.turn_end, policy, now);
            self.fx
                .shadowed(candidate.rule_id(), |at| turn_end_outcome(&candidate, at));
        }
        // The credits hold's escalation, decided — alone, or as the
        // otherwise of the move ahead that goes first: told once an episode
        // ([`TurnEndState::credits_told`]).
        let tells = |a: &TurnEndAction| {
            matches!(a, TurnEndAction::Escalate { reason }
                if reason == CREDITS_HOLD_TOLD || reason == CREDITS_HOLD_MODEL_TOLD)
        };
        if tells(&action)
            || matches!(&action, TurnEndAction::Restart { otherwise, .. } if tells(otherwise))
        {
            self.turn_end.credits_told();
        }
        // A draft that holds the point with nothing due: what moves it is
        // the person's send or clear ([`Self::draft_look`]).
        self.draft_held = draft_holds(&composer, &action).then_some(composer);
        if let Some(steps) = steps {
            self.fx.traced(steps);
        }
        self.fx.decide(
            super::super::tape::doors::DECIDE_TURN_END,
            judged,
            action.rule_id(),
            |at| turn_end_outcome(&action, at),
            None,
        );
        Ok(action)
    }

    /// [`Self::decide_to_act`]'s decision, each pass of it traced into
    /// `trace` where it is `Some` (a replay that asked for the point's trace).
    fn decide_to_act_now(
        &mut self,
        mut r: TurnEndReading,
        opts: &SuperviseOpts,
        now: Instant,
        trace: &mut Option<Vec<TraceStep>>,
        mut shadow: Option<&mut ShadowPoint>,
    ) -> Result<TurnEndAction, Fail> {
        // The host withholds the continuations ([`IdleHost::withholds_turn_end`]):
        // nothing is typed, and the point is decided again once it lifts.
        if host_withholds(opts) {
            if let Some(point) = shadow {
                point.withheld = true;
            }
            if let Some(steps) = trace.as_mut() {
                steps.push(TraceStep {
                    check: "host-withholds",
                    rule: None,
                    outcome: TraceOutcome::Decided(format!("{:?}", TurnEndAction::Nothing)),
                });
            }
            return Ok(TurnEndAction::Nothing);
        }
        self.autopilot.begin();
        r.autopilot = Autopilot::Cruise;
        let mut action = self.turn_end_decision(&r, &opts.policy, now, "pass:cruise", trace);
        let mut cfg = std::borrow::Cow::Borrowed(&opts.policy);
        if r.asked().is_some()
            && session_word_can_decide(&action)
            && let Some(policy) = self.prose_policy(opts)?
        {
            if let Some(point) = shadow.as_deref_mut() {
                point.prose = Some(policy.question_priority);
            }
            cfg = std::borrow::Cow::Owned(policy);
            action = self.turn_end_decision(&r, &cfg, now, "pass:prose-word", trace);
        }
        r.autopilot = if matches!(
            action,
            TurnEndAction::Nothing | TurnEndAction::WaitUntil { .. }
        ) {
            self.autopilot(opts)
        } else {
            self.autopilot_read(opts)?
        };
        if r.autopilot != Autopilot::Cruise {
            action = self.turn_end_decision(&r, &cfg, now, "pass:autopilot", trace);
        }
        if action.rule_id().is_some() {
            self.program = self.foreground_program()?;
            r.person = self.person_ago(now);
            action = self.turn_end_decision(&r, &cfg, now, "pass:final", trace);
        }
        // The exceptions engine reads the same policy and the same reading
        // the loop's final decision was made on — a lost login's notice says
        // what that decision does ([`exception_for`]); with the autopilot off
        // it raises nothing.
        self.exception_now = (r.autopilot != Autopilot::Off)
            .then(|| exception_for(&self.turn_end, &r, &cfg, now))
            .flatten();
        if let Some(point) = shadow {
            point.reading = Some(r);
        }
        Ok(action)
    }

    /// [`decide_turn_end`] on `r` under `cfg` at `now` — and, where a replay
    /// asks for the point's RULE TRACE (`trace` is `Some`, as `Fx::tracing`
    /// says: `aterm drive replay --why` at a turn end, strictly replaying the
    /// tape to it), the same decision made with its trace kept
    /// ([`decide_turn_end_traced`]), its steps added to `trace` after a step
    /// naming this `pass` of the point; [`Self::decide_to_act`] hands them to
    /// the replay with the point's mark. The shipped loop and a recording one
    /// never keep a trace: a turn end's is computed only in a replay, which
    /// alone rebuilds the fold it decides on.
    fn turn_end_decision(
        &self,
        r: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
        pass: &'static str,
        trace: &mut Option<Vec<TraceStep>>,
    ) -> TurnEndAction {
        let Some(steps) = trace.as_mut() else {
            return decide_turn_end(&self.turn_end, r, cfg, now);
        };
        steps.push(TraceStep {
            check: pass,
            rule: None,
            outcome: TraceOutcome::Passed,
        });
        decide_turn_end_traced(&self.turn_end, r, cfg, now, steps)
    }

    /// The policy a question asked in prose is decided under when the
    /// session's own `questions` word is a priority word
    /// ([`Self::question_policy`], the dialog's one resolver): the table's,
    /// answering, by that word's priority. `None` (no session word, `ask`, or
    /// an unknown word): the table decides.
    fn prose_policy(&mut self, opts: &SuperviseOpts) -> Result<Option<SupervisorConfig>, Fail> {
        let policy = self.question_policy(opts)?;
        Ok(match policy.answer {
            Some(word) if policy.by_session => {
                let mut cfg = opts.policy.clone();
                cfg.answer_questions = true;
                cfg.question_priority = word;
                Some(cfg)
            }
            _ => None,
        })
    }

    /// The policy's `WaitUntil` ran out on the point still showing: read the
    /// screen again, and when it still shows the same point (its review key)
    /// fold it in again with no work ([`TurnEndState::observe`]: the same
    /// point read again changes nothing, except an act whose point has shown
    /// past [`TurnEndTiming::take_within`] with no busy read, which is judged
    /// now), decide again, and carry it out; an escalation is the point's
    /// ([`Self::escalate_point`]). A screen that shows another point is left to
    /// the next look.
    pub(super) fn turn_end_now(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.turn_end_due = None;
        // The wait the host was told (an API wall's next try) is over: the
        // point is decided again now, and the try is typed below.
        self.end_told_wait(opts);
        if self.claim.watching_behind().is_some() {
            return Ok(());
        }
        // An act held for a person: the next look judges the point again.
        if std::mem::take(&mut self.held) {
            self.rejudge = true;
            return Ok(());
        }
        let screen = self.screen()?;
        let mut turn = self.point_of(screen);
        if self.review_key(&turn) != self.review_key(seen) {
            return Ok(());
        }
        self.account_round();
        let mut r = self.turn_end_reading(&turn.screen, None, opts);
        self.point_as_decided(&mut turn);
        self.host_held = r.upgrading || host_withholds(opts);
        let now = self.policy_now();
        self.fold_point(&mut r, now, turn.screen.seq, opts);
        self.task_seed(opts)?;
        // A probe the roster says is due at a point decided AGAIN is not
        // run — the turn behind it was not read live by this look — and says
        // so, unless the point has had its line ([`Self::probe_shut`]).
        self.probe_shut(&turn, ProbeGate::NotNew, None, review)?;
        let action = self.decide_to_act(r, opts, now, &turn.screen)?;
        if let TurnEndAction::Escalate { reason } = &action
            && self.escalates_here(&turn.phase)
        {
            self.escalate_point(&turn, Some(reason), review)?;
            // The memory wall's escalation took the point's own act: handed
            // back when the measure is no longer acted on
            // ([`Self::memory_edge`]).
            if self.memory_wall {
                self.mark_memory_badge();
            }
        }
        self.turn_end_execute(&turn, action, opts, allow, review)
    }

    /// THE TURN THE CONVERSATION ENDS ON, by every key that names it: the
    /// host's transcript ([`crate::harness::upgrade::LastTurn::of`]), else
    /// the screen's last words `said`, hashed. What a person's own exchange
    /// left to them is ledgered under and matched by (the review's RR3-TE-2).
    fn last_turn(
        opts: &SuperviseOpts,
        said: Option<&str>,
    ) -> Option<crate::harness::upgrade::LastTurn> {
        opts.idle_host
            .as_ref()
            .and_then(|h| h.transcript_tail())
            .map(|tail| crate::harness::upgrade::LastTurn::of(&tail))
            .filter(|t| t.key().is_some())
            .or_else(|| {
                said.map(|said| crate::harness::upgrade::LastTurn {
                    last: Some(Self::said_key(said)),
                    ..crate::harness::upgrade::LastTurn::default()
                })
            })
    }

    /// The key of a turn no transcript names: the screen's last words
    /// `said`, hashed ([`Self::last_turn`]).
    fn said_key(said: &str) -> String {
        format!("said:{:016x}", aterm_hash::fnv1a_64(said.as_bytes()))
    }

    /// A PERSON'S OWN EXCHANGE LEFT TO THEM at `point`, made durable (the
    /// review's RR3-TE-2): a `skipped` row under [`RULE_LEFT`] carrying the
    /// key of the turn it left ([`Self::last_turn`]), once per turn. Kept
    /// only in this loop's memory, a new loop (a seamless update, a new
    /// window, a fault, a cold restore's tab) read the greeting by its words
    /// and typed the done check or `Nobody is here to answer…` into the
    /// person's exchange, and a relaunch typed its carry-on there. A typed
    /// act closes it ([`Self::ledger_row`], [`approvals::open_left`]).
    ///
    /// THE KEY IS THE PROMPT THAT OPENED THE EXCHANGE
    /// ([`crate::harness::upgrade::LastTurn::key`], ATERM-KI-1016-1): the
    /// loop decides the moment the busy row leaves, and the reply's rows
    /// reach the transcript a flush or more later, so the tail read here can
    /// still end on the person's prompt, or on a tool's call before the
    /// reply's last words. Keyed by that last row, the row matched no later
    /// reading (each names the reply's last row), and a new loop or a
    /// relaunch typed into the person's exchange; re-reading the tail for a
    /// second (the review's RR4-TE-3) left it so for a transcript later than
    /// that. The prompt's row is on the record whichever of the reply's rows
    /// are, and every later reading names it while the exchange is the
    /// conversation's last turn ([`crate::harness::upgrade::LastTurn::names`]).
    fn ledger_left(&mut self, screen: &Screen, opts: &SuperviseOpts, seq: u64) {
        let said = self
            .reader(&screen.rows)
            .read(&screen.rows, Some(screen.cursor_col))
            .said_tail;
        let Some(key) = Self::last_turn(opts, said.as_deref()).and_then(|turn| turn.key()) else {
            return;
        };
        if self.left_keys.contains(&key) {
            return;
        }
        self.ledger_row(
            RULE_LEFT,
            approvals::Outcome::Skipped,
            &key,
            approvals::LEFT_REASON,
            seq,
        );
        self.left_keys.push(key);
    }

    /// A PERSON'S OWN EXCHANGE AT A WALL, made durable (the review's
    /// RR4-TE-2, [`TurnEndState::exchange_at_wall`]): a `skipped` row under
    /// [`RULE_LEFT`] keyed by the turn the wall answered — the person's
    /// prompt (`crate::harness::upgrade::last_turn_key` passes over the
    /// wall's own error row) — marked [`approvals::LEFT_AT_WALL`], once per
    /// standing row: the retry typed keeps it open as the retry's
    /// ([`approvals::left_after_typed`]), and the retry the wall answers
    /// again writes the next. Kept only in this loop's memory, a new
    /// loop whose first point was the wall (a seamless update, a new window,
    /// a fault inside an outage) typed the retry and then `Nobody is here to
    /// answer…` or the done check into the person's greeting. The key carries
    /// the exchange's work so far ([`approvals::left_at_wall_key`]): with
    /// none, that new loop measured the retry's answer alone, and a job of
    /// 6 min, a 529 and 6 more was left to the person (the merge review's
    /// F3, against main's 7fffc8156).
    fn ledger_left_at_wall(&mut self, screen: &Screen, opts: &SuperviseOpts, seq: u64) {
        if self.left_at_wall_seq.replace(seq) == Some(seq) {
            return;
        }
        let said = self
            .reader(&screen.rows)
            .read(&screen.rows, Some(screen.cursor_col))
            .said_tail;
        let Some(key) = Self::last_turn(opts, said.as_deref()).and_then(|turn| turn.key()) else {
            return;
        };
        let key = approvals::left_at_wall_key(&key, self.turn_end.exchange_work());
        if self.left_keys.contains(&key) {
            return;
        }
        self.ledger_row(
            RULE_LEFT,
            approvals::Outcome::Skipped,
            &key,
            approvals::LEFT_AT_WALL_REASON,
            seq,
        );
        self.left_keys.push(key);
    }

    /// Whether the policy's escalation at a point of `phase` is the point's
    /// own ([`Self::escalate_point`]): at an idle point and a question, and at
    /// a limit notice only when the reading took it as the memory wall
    /// ([`Self::memory_wall`]). A limit's own escalation is its episode's
    /// (`limit_after`), which then raises nothing over the memory's
    /// (`handled`).
    pub(super) fn escalates_here(&self, phase: &Phase) -> bool {
        match phase {
            Phase::Idle | Phase::Question => true,
            Phase::Limited { .. } => self.memory_wall,
            _ => false,
        }
    }

    /// THE TASK'S EDGE AT A POINT, said once a point: as the point is folded
    /// in — before the host's step or the accounts probe can take it, and
    /// type into it — and again before its act ([`Self::turn_end_execute`];
    /// the second time changes nothing). The task ended — by Land's landing
    /// (a clean defense at a land act's yield), or as a person's own
    /// exchange; nothing else ends one (FOREVER CRUISE / FOREVER LAND) — is
    /// said once, to the journal alone — nothing is
    /// typed from here until someone else's turn — and to the ledger, so a
    /// loop that follows reads the act awaited before it as answered
    /// ([`approvals::RULE_TASK_DONE`]; a person's own exchange as theirs,
    /// [`Self::ledger_left`]); and the session's task record hears it: `done`
    /// with its result at a done point, `working` again where someone else's
    /// turn ended it (`task_mark`); and the worker's NEEDS YOU is put up or
    /// taken down (`escalate.rs`'s `sync_needs`). `screen` is the point's.
    pub(super) fn task_edge(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.task_edge_with(screen, opts, review, true)
    }

    /// [`Self::task_edge`] at the point's fold, before the point's own line
    /// is journaled: published and recorded as there, its `DONE …`/`LEFT …`
    /// note owed ([`Self::task_note`]) until the point's line is written —
    /// the order a loop that follows reads a landing by (`ended_before`: the
    /// landing's point, then its note) — and written right after it
    /// ([`Self::say_task_note`]), before the host's step, the accounts probe
    /// or the act can take the point, and before the loop ends on a request
    /// that failed ([`Self::said_before_fail`]) or a print of the point's
    /// line that failed. Owed to the point's act instead, the note was never written
    /// where the host's step took the point and the loop was stopped before
    /// another point's act: the books a loop that follows read held the
    /// end's point and its ledger rows and no note, and that loop started
    /// `open`, stamped the end with its own clock, and — a landing said in
    /// words, its Land episode over with the Off the loop was stopped under
    /// — typed the land instruction (Land) or the done check (Cruise) into
    /// the finished work.
    pub(super) fn task_edge_at_fold(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.task_edge_with(screen, opts, review, false)
    }

    /// THE TASK'S END NOTE OWED ([`Self::task_note`]), written now, with the
    /// time the end it restates was first noted: right after the point's own
    /// line is journaled (`look`), and at the point's act for an end no fold
    /// owed (a point decided again, a done read on a busy break). Nothing
    /// owed writes nothing.
    pub(super) fn say_task_note(&mut self, review: &mut dyn Review) {
        if let Some((note, ended)) = self.task_note.take() {
            review.note_ended(&note, ended);
        }
    }

    /// A NEW POINT'S OWN LINE, journaled with the mark of the worker's last
    /// words there ([`said_mark`]; a box says none) — what a later loop
    /// reads an end's point by (`ended_before`) — and the task's end its
    /// fold took right after it ([`Self::say_task_note`]): before the host's
    /// step, the accounts probe or the point's act can take the point, and
    /// whether the review stops the loop with it or its print fails (the
    /// line is journaled before it is printed: a CLI `watch` whose reader
    /// went away). Whether the loop goes on ([`Review::review_marked`]).
    pub(super) fn say_point(
        &mut self,
        point: &Turn,
        review: &mut dyn Review,
    ) -> Result<bool, String> {
        let mark = (point.phase != Phase::Prompt)
            .then(|| {
                let rows = &point.screen.rows;
                said_mark(self.reader(rows).said_tail(rows).as_deref())
            })
            .flatten();
        // The line is journaled before it is printed: the note goes after it
        // even where the print fails and the loop ends on it.
        let go_on = review.review_marked(point, mark);
        self.say_task_note(review);
        go_on
    }

    /// A STEP BETWEEN A POINT'S FOLD AND ITS OWN LINE (`step`: the fold's
    /// own publish, a limit's episode closed, the point's badge raised),
    /// handed back as it went — and where it failed, a request the loop ends
    /// on (the window's host hangs a loop's connection up to stop it at
    /// once), with the point's line and the task's end the fold took said
    /// first ([`Self::say_point`]). Not said there, the end's ledger rows
    /// stood with no point line and no note: a loop that follows read no end
    /// (`ended_before`), started `open` and stamped the end with its own
    /// clock, and — a landing said in words, Off lifted — typed the land
    /// instruction or the done check into the finished work. Nothing owed
    /// says nothing: the failure alone, as before.
    pub(super) fn said_before_fail<T>(
        &mut self,
        point: &Turn,
        step: Result<T, Fail>,
        review: &mut dyn Review,
    ) -> Result<T, Fail> {
        if step.is_err() && self.task_note.is_some() {
            let _ = self.say_point(point, review);
        }
        step
    }

    /// [`Self::task_edge`]; `say`: the note is written now (and any owed
    /// before it), else owed.
    fn task_edge_with(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
        say: bool,
    ) -> Result<(), Fail> {
        let seq = screen.seq;
        // A PERSON'S OWN EXCHANGE left to them is said first: it is theirs
        // under every Autopilot mode, Land's included, and no task's end (the
        // policy never reads one as landed). Otherwise a task is done only by
        // Land's landing (FOREVER CRUISE / FOREVER LAND): a clean defense at a
        // land act's yield.
        let done = self.turn_end.task_done();
        // A LANDING THE TASK'S OWN WAKE KEPT ([`TurnEndState::take_wake_restated`]):
        // its note is restated after the wake's point, which the journal
        // keeps with the wake's mark, so a loop that follows reads the
        // landing as the last word on the screen and seeds it. Unsaid, a
        // restart after the kept wake typed Land's nudge into the landed work.
        let restates_wake = self.turn_end.take_wake_restated() && done && self.task_done_said;
        if done && (!self.task_done_said || restates_wake) {
            let note = if self.turn_end.left_to_person() {
                let note = format!(
                    "LEFT seq={seq} rule={RULE_LEFT} a person's own exchange (their message, \
                     answered within 10 min): nothing is typed until the next turn ends"
                );
                self.ledger_left(screen, opts, seq);
                note
            } else if self.turn_end.landed_by_wake() {
                format!(
                    "DONE seq={seq} rule={RULE_LAND} {WAKE_KEPT} (autopilot land): nothing \
                     more is typed until a person or an orchestrator types"
                )
            } else {
                format!(
                    "DONE seq={seq} rule={RULE_LAND} the worker defended that everything is \
                     landed on the remote main branch (autopilot land): nothing more is typed \
                     until a person or an orchestrator types"
                )
            };
            // A landing a loop before this one noted, restated here: its
            // note carries the landing's first time forward
            // ([`TurnEndState::restated_noted_ms`]); a person's exchange
            // (`LEFT`) is no end restated.
            // A wake's restatement carries the landing's own first time.
            let ended = if self.turn_end.left_to_person() {
                None
            } else if restates_wake {
                self.landed_noted_ms
                    .or_else(|| self.turn_end.restated_noted_ms())
            } else {
                self.turn_end.restated_noted_ms()
            };
            if !self.turn_end.left_to_person() {
                self.landed_noted_ms = Some(ended.unwrap_or_else(|| self.now_unix_ms()));
            }
            self.task_note = Some((note, ended));
            // ...and to the ledger: a loop that follows reads the act awaited
            // before it as answered ([`approvals::RULE_TASK_DONE`]) — once
            // for the end, not again for a wake's restatement of it.
            if !self.turn_end.left_to_person() && !restates_wake {
                self.ledger_row(
                    approvals::RULE_TASK_DONE,
                    approvals::Outcome::Waiting,
                    "",
                    approvals::TASK_DONE,
                    seq,
                );
            }
        } else if self.turn_end.left_judged() {
            // A person's own exchange after the task's end was said: theirs
            // too, and a loop that follows reads it so
            // ([`TurnEndState::left_judged`]).
            self.ledger_left(screen, opts, seq);
        }
        if say {
            self.say_task_note(review);
        }
        // The worker's NEEDS YOU, put up (or taken down) at the point itself.
        self.sync_needs(screen, opts, review)?;
        let was_done = std::mem::replace(&mut self.task_done_said, done);
        self.task_at_point(seq, was_done, opts, review)
    }

    /// THE LAND EPISODE IN HAND ENDED by a reading or a decision in another
    /// mode (`had`: one was in hand before it, [`TurnEndState::land_episode`]):
    /// the ledger says so ([`approvals::RULE_LAND_OVER`]), so a loop that
    /// follows begins a Land after it as a new episode, as this one does
    /// ([`approvals::land_episode`]).
    pub(super) fn land_over(&mut self, had: bool, seq: u64) {
        if had && !self.turn_end.land_episode().0 {
            self.ledger_row(
                approvals::RULE_LAND_OVER,
                approvals::Outcome::Waiting,
                "",
                approvals::LAND_OVER,
                seq,
            );
        }
    }

    /// Carry out what the policy decided at `point` (module docs). An act the
    /// server took is recorded in the policy's memory
    /// ([`TurnEndState::acted`]); one it did not take is not.
    pub(super) fn turn_end_execute(
        &mut self,
        point: &Turn,
        action: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        // A wall the transcript did not confirm, journaled once.
        self.say_wall_refused(review);
        // A turn read where the books could not count the sleep before this
        // loop's first look, journaled once.
        self.say_sleep_unknown(seq, review);
        // The point a person alone can answer, said once per change.
        self.sync_exception(seq, review);
        // The task's end said and published, once a point ([`Self::task_edge`]).
        self.task_edge(&point.screen, opts, review)?;
        // A person's own exchange that met a wall, made durable for a new
        // loop whose first point is that wall (RR4-TE-2).
        if self.turn_end.exchange_at_wall() {
            self.ledger_left_at_wall(&point.screen, opts, seq);
        }
        // A restart being waited for is one wall's; whatever else the point
        // decides ends the run of tries, and the next restart starts its
        // pauses over.
        if !matches!(action, TurnEndAction::Restart { .. }) {
            self.restart_waits = 0;
            self.restart_run = None;
        }
        // THE WAKE'S SETTLE ([`Self::settle_wake`]): an act decided inside it
        // — whatever decided it: the due falling, a repaint, an edge of the
        // host's, a new point — is not typed into an API the network may not
        // reach yet. The point is decided again when the settle falls. The
        // watch placed the settle where it measured the sleep, and the
        // decision read the watch ([`Self::policy_now`]).
        if action.rule_id().is_some()
            && let Some(at) = self.settling()
        {
            self.settle_said(seq, at, review);
            self.set_due(at, WOKE);
            return Ok(());
        }
        let (text, rule) = match &action {
            TurnEndAction::Nothing | TurnEndAction::Escalate { .. } => return Ok(()),
            TurnEndAction::Restart {
                why,
                rule_id,
                otherwise,
            } => {
                return self.restart_agent(
                    point,
                    why,
                    rule_id,
                    otherwise.as_ref().clone(),
                    opts,
                    allow,
                    review,
                );
            }
            TurnEndAction::WaitUntil { until, why, wait } => {
                let said = self.turn_end_due.as_ref().is_some_and(|d| d.at == *until);
                if !said {
                    review.note(&format!(
                        "WAITING seq={seq} until={} {why}",
                        self.stamp_in(*until)
                    ));
                }
                self.set_wait(*until, why.clone(), *wait);
                return Ok(());
            }
            TurnEndAction::Type { text, rule_id } | TurnEndAction::Accept { text, rule_id } => {
                (text.clone(), *rule_id)
            }
            TurnEndAction::TypeCommand {
                command, rule_id, ..
            } => (command.clone(), *rule_id),
            TurnEndAction::Submit { rule_id } => (
                self.reader(&point.screen.rows)
                    .composer(&point.screen.rows)
                    .map(|(_, lines)| lines.join(" "))
                    .unwrap_or_default(),
                *rule_id,
            ),
            TurnEndAction::Confirm { rule_id, .. } => (String::new(), *rule_id),
        };
        if !self.speaks_to_its_agent(seq, rule, review) {
            return Ok(());
        }
        // The `status` that named the program says the worker is not reading
        // its input: nothing is typed; the wait holds first (`stall.rs`).
        if self.stall_in_hand(review) {
            return Ok(());
        }
        if let TurnEndAction::Confirm { rule_id, row } = &action {
            return self.confirm_dialog(point, &action, rule_id, row, opts, review);
        }
        // The draft this act would send in its place is the accounts probe's
        // own `/usage`, left typed: its Enter is the probe's, and no act.
        if matches!(action, TurnEndAction::Submit { .. }) && self.probe_left_stands(&point.screen) {
            return self.probe_left_enter(point, review);
        }
        // ONE SIGN-IN PER LOGIN: `/login` goes from the tab that takes the
        // login's sign-in ([`IdleHost::login_carry`]). One another tab took
        // first is waited on: the point is decided again, and reads it.
        if rule == RULE_LOGIN
            && let Some(host) = opts.idle_host.as_ref()
            && let Err(by) = host.login_carry()
        {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the login's sign-in is {by}'s"
            ));
            let at = self.fx.mono() + LOGIN_SHARED_LOOK;
            self.set_due(at, format!("the login's sign-in is {by}'s"));
            return Ok(());
        }
        let typed = match &action {
            TurnEndAction::Accept { .. } => self.accept_suggestion(point, &text, rule, review),
            TurnEndAction::Submit { .. } => self.submit_draft(point, &text, rule, review),
            _ => self.type_text(point, &text, rule, review),
        };
        if rule == RULE_LOGIN
            && !matches!(&typed, Ok(true))
            && let Some(host) = opts.idle_host.as_ref()
        {
            host.login_abandon();
        }
        let typed = typed?;
        if !typed {
            return Ok(());
        }
        let r = self.turn_end_reread(&point.screen, opts);
        let land = self.turn_end.land_episode().0;
        self.turn_end.acted(&action, &r, self.fx.mono());
        self.land_over(land, seq);
        let word = if matches!(action, TurnEndAction::TypeCommand { .. }) {
            "TYPED"
        } else {
            "CONTINUED"
        };
        review.say(&format!("{word} seq={seq} rule={rule} {}", clip(&text)))?;
        // An act's row carries the short streak it was typed after, so a
        // later loop backs its yield off as this one does
        // ([`approvals::short_streak`]), and the limit's try (the
        // continuation, or an act typed in its place) names its limit, so a
        // later loop works that limit from it ([`approvals::usage_attempt`]).
        let reason = approvals::act_reason_after(self.carried(), self.turn_end.limit_try());
        self.ledger_row(rule, approvals::Outcome::Typed, &text, &reason, seq);
        Ok(())
    }

    /// THE POINT ONLY A PERSON CAN ANSWER, said once per change
    /// ([`exception_for`]): `EXCEPTION seq=<n> kind=<word> when=<later|now>
    /// [notice=<id>] <why> => <action>` to the journal and the host's
    /// [`IdleHost::exception`] when one appears or its words change, and, when
    /// the point no longer holds it, its end ([`Self::exception_ended`]).
    /// Says nothing where the exception stands as told, and raises nothing
    /// of its own (no attention, no badge, no mail): one the point's act
    /// escalated ([`Exception::escalated`]: another program's lost login)
    /// has that escalation, and the rest is the host's to surface — a
    /// `Later` one in its digest, a `Now` one as the owner's interrupt
    /// (Claude Code's lost login, its notice: [`super::policy::notice`]) —
    /// or the journal's. While a notice stands the loop raises no badge of
    /// its own for the point ([`Self::escalate_point`]): one surface.
    fn sync_exception(&mut self, seq: u64, review: &mut dyn Review) {
        // A point is the screen moving on: a stall no reader named is over.
        self.stuck_over(seq, "the screen reached a point", review);
        let Some(now) = self.exception_now.clone() else {
            self.exception_ended(seq, None, review);
            return;
        };
        match &self.exception_open {
            Some((open, _)) if *open == now => return,
            Some((open, _)) if open.kind != now.kind => {
                self.exception_ended(seq, Some("another takes its place"), review);
            }
            _ => {}
        }
        let since = self
            .exception_open
            .as_ref()
            .map_or_else(|| self.fx.mono(), |(_, t)| *t);
        // A notice's id names which of the owner's notices stands
        // ([`super::policy::notice`]), so the journal tells them apart.
        let notice = now
            .notice
            .as_ref()
            .map_or_else(String::new, |n| format!(" notice={}", n.id));
        review.note(&format!(
            "EXCEPTION seq={seq} kind={} when={}{notice} {} => {}",
            now.kind.word(),
            now.when.word(),
            now.why,
            now.action
        ));
        if let Some(host) = &self.stall_host {
            host.exception(Some(&now));
        }
        self.exception_open = Some((now, since));
    }

    /// The exception told and standing is over: `CLEARED seq=<n>
    /// exception=<word> after=<s>s <what cleared it>` to the journal, and the
    /// host's [`IdleHost::exception`] told `None`. Nothing stood: nothing is
    /// said. `cause` names the end where the exception's own words for it
    /// (`clears`) would not be true — the loop ending, a replacement.
    pub(super) fn exception_ended(
        &mut self,
        seq: u64,
        cause: Option<&str>,
        review: &mut dyn Review,
    ) {
        self.exception_now = None;
        let Some((open, since)) = self.exception_open.take() else {
            return;
        };
        review.note(&format!(
            "CLEARED seq={seq} exception={} after={}s {}",
            open.kind.word(),
            self.fx.since(since).as_secs(),
            cause.unwrap_or(open.clears)
        ));
        if let Some(host) = &self.stall_host {
            host.exception(None);
        }
    }

    /// THE RESTART the policy asked for ([`TurnEndAction::Restart`]: the
    /// memory banner, D3; a model bucket's fallback and its reset, D7): the
    /// session's host's ([`IdleHost::restart`]), journaled `HOST seq=<n>
    /// restart:<why> step=<word>`, and what the loop does with its word. The
    /// agent relaunched (`adopted`) is the loop's again at once (the host asks
    /// for the next idle point to carry it on), said `RESTARTED seq=<n>
    /// rule=<id> <what>`, remembered by the policy
    /// ([`TurnEndState::restarted`]: a bucket's switch, undone at its reset)
    /// and, for a model, ledgered as `relaunch --model <m>`, the row a later
    /// loop reads the open switch from ([`approvals::open_model_switch`]). One
    /// that must wait (`wait:…`, `busy:another-sweep`) is decided again after
    /// a pause that grows ([`TurnEndTiming::restart_backoff`]), never
    /// escalated for the wait. One that is never made (no host restarts here,
    /// a launch that cannot be carried, a signal refused) gets the point's
    /// `otherwise`: the memory banner's escalation (naming why), a bucket's
    /// reset waited out, the continuation a reset owed. Nothing while the
    /// worker's stall is in hand (`stall.rs`).
    #[allow(clippy::too_many_arguments)]
    fn restart_agent(
        &mut self,
        point: &Turn,
        why: &Restart,
        rule: &'static str,
        otherwise: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        if self.stall_in_hand(review) {
            return Ok(());
        }
        // The measure the point was decided on, if the memory wall was the
        // host's measure rather than the banner (read again below).
        let measured = self.memory_seen;
        // …and whether it stood in for the screen's own wall (read again
        // below, perhaps under the ceiling by then), or for any idle point's
        // own act: an escalation in the restart's place is the memory's,
        // handed back when the measure is no longer acted on.
        let over_wall = self.memory_over_wall && matches!(why, Restart::Memory);
        let took = self.memory_wall && matches!(why, Restart::Memory);
        // THE ROW SAYS THE RESTART BEFORE IT IS MADE: the point's own reading
        // ([`Self::turn_end_reading`]) may be the first to find the measure
        // acted on — the host's cached measure expired since the turn's last
        // busy read, which said `a warning only` (or nothing, in a loop that
        // began at the point) — and no wait step comes between it and this
        // ask to say the new verdict ([`Self::memory_warn`] is said from the
        // waits). A refusal below says the person's; one standing from an
        // earlier point keeps it until a restart is made (said below).
        if let (Restart::Memory, Some(over)) = (why, measured) {
            self.memory_warn(&point.screen, over, opts, review);
        }
        // THE RUN OF TRIES BEGINS AT ITS FIRST ASK: the relaunch line an ask
        // of it types is typed no sooner (the host's `wait:resume` says it
        // was, the new process not found yet, and a later ask adopts it). A
        // restart of another kind than the run's begins its own, its pauses
        // from the first ([`Self::restart_run`]).
        let asked_unix = match self.restart_run {
            Some((kind, at)) if kind == why.word() => at,
            _ => {
                let at = self.now_unix();
                self.restart_waits = 0;
                self.restart_run = Some((why.word(), at));
                at
            }
        };
        let step = opts.idle_host.as_ref().and_then(|h| h.restart(why));
        if let Some(step) = &step
            && restart_note_due(self.restart_waits)
        {
            review.note(&format!(
                "HOST seq={seq} restart:{} step={step}",
                why.word()
            ));
        }
        let word = step
            .as_deref()
            .map(|s| s.split_once(':').map_or(s, |(head, _)| head));
        match (word, step.as_deref()) {
            (Some("adopted" | "relaunched" | "continued" | "done"), _) => {
                self.restart_waits = 0;
                self.restart_run = None;
                // A restart made: a refusal before it no longer stands, and
                // the next warning names aterm's restart again
                // ([`Self::memory_refused`]).
                self.memory_refused = false;
                let r = if matches!(why, Restart::Account { .. }) {
                    self.turn_end_reread_relaunched(&point.screen, opts)
                } else {
                    self.turn_end_reread(&point.screen, opts)
                };
                let now = self.fx.mono();
                self.turn_end.restarted(why, true, &r, now);
                if matches!(why, Restart::Model { .. }) {
                    self.turn_end.fallback_made_at(asked_unix);
                    // A model's usage credits were the process's left behind
                    // ([`Self::note_credits`]), whatever the host's start
                    // says yet.
                    if let Some(kept) = &mut self.credits_kept {
                        kept.model = None;
                    }
                }
                // The fallback's relaunch, unasked till its own row says it
                // was ([`Self::fallback_asked`], below).
                let unasked = self.turn_end.fallback_unasked();
                // A host that typed the carry-on inside its restart typed a
                // turn of its own: its answer is awaited as the harness's,
                // never a short turn of the worker's — and, being the
                // carry-on, an answer that says done, short or long, is a
                // done report; being the carry-on that asks the done check
                // (`continued:check`), its answer is the check's yield
                // (LIVE-RL-3). Which, the step's own word says
                // (`upgrade_drive::host_turn`).
                if word == Some("continued") {
                    let kind = step
                        .as_deref()
                        .and_then(crate::harness::upgrade_drive::host_turn)
                        .unwrap_or(super::super::policy::turn_end::HostTurn::CarryOn);
                    self.turn_end.host_typed(now, kind);
                }
                let what = match why {
                    Restart::Memory => match measured {
                        Some(over) => format!("the agent, for its memory: {}", over.fact()),
                        None => "the agent, for its critical-memory banner".to_string(),
                    },
                    Restart::Model { to } => format!("relaunch --model {to}"),
                    Restart::ModelBack { to } => {
                        format!(
                            "relaunch --model {}",
                            to.as_deref().unwrap_or("(its launch's)")
                        )
                    }
                    // The LABEL, and nothing else of the account, in this
                    // loop's journal and its approval ledger. (The UPGRADE's
                    // ledger is the relaunch's own record and keeps the line
                    // it typed as its `detail`, which names the login
                    // directory beside the `cd` it always held: a path,
                    // never a credential.)
                    Restart::Account { to, .. } => {
                        format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND)
                    }
                };
                // The wall the move left is the OLD account's: what the
                // loop remembers of it (the reading above read it off the
                // point's screen once more) is not the new process's, nor is
                // the advice asked for that point — nor the LIMIT EPISODE,
                // which held every step of the host's back until that
                // account's reset plus an hour ([`Guard::LimitEpisode`]): the
                // wall's resume owed at the relaunched agent's first point
                // with them, so a move resumed later than no roster would
                // have.
                if matches!(why, Restart::Account { .. }) {
                    self.forget_wall(seq);
                    self.wall_reset = None;
                    self.reprint_jumps = 0;
                    self.account_seen = None;
                    self.roster_seen = None;
                    self.retry = None;
                    self.close_episode(
                        seq,
                        "relaunched under another account, off the limited one",
                        review,
                    )?;
                }
                let switch = self
                    .turn_end
                    .model_switch()
                    .filter(|_| matches!(why, Restart::Model { .. }))
                    .cloned();
                let back = match (&switch, why) {
                    (Some(m), _) => format!(
                        " (from {}, {}; session-only)",
                        m.from.as_deref().unwrap_or("the launch's model"),
                        // No reset named: no return is scheduled, and none is
                        // guessed.
                        m.back_at.map_or_else(
                            || "no return scheduled: the notice names no reset".to_string(),
                            |at| format!("back at {}", self.stamp_in(at))
                        )
                    ),
                    (
                        None,
                        Restart::Account {
                            at: AccountAt::Wall,
                            ..
                        },
                    ) => " (from the limit its turn ended on; the same conversation)".to_string(),
                    (
                        None,
                        Restart::Account {
                            at: AccountAt::Ahead,
                            ..
                        },
                    ) => " (at a turn end; the same conversation)".to_string(),
                    (None, _) => String::new(),
                };
                review.say(&format!("RESTARTED seq={seq} rule={rule} {what}{back}"))?;
                // A restart made over a refusal that stood (the row the
                // person's until here) names aterm's at once
                // ([`Self::memory_say`]).
                if let (Restart::Memory, Some(over)) = (why, measured) {
                    self.memory_say(&point.screen, over, true, opts, review);
                }
                if !matches!(why, Restart::Memory) {
                    // A switch's row carries how to undo it, for a loop that
                    // starts before its reset.
                    let reason = match (&switch, why) {
                        (Some(m), _) => approvals::model_switch_reason(
                            m.from.as_deref(),
                            m.back_at.map(|at| self.unix_of(at)),
                            asked_unix,
                        ),
                        // The move's row: a loop that starts after it counts
                        // it and answers it ([`approvals::account_moves`]) —
                        // and, a `typed` row, it ends the wall the earlier
                        // rows waited on ([`approvals::open_wall`]), which is
                        // right: the session left that wall's account.
                        (None, Restart::Account { to, at }) => {
                            approvals::account_switch_reason(to, *at)
                        }
                        (None, _) => "the turn-end policy".to_string(),
                    };
                    self.ledger_row(rule, approvals::Outcome::Typed, &what, &reason, seq);
                }
                // A carry-on the host typed inside the restart asked a turn
                // on the fallback: written after the relaunch's own row.
                self.fallback_asked(unasked, seq);
                Ok(())
            }
            // The RETURN to the
            // bucket's model is optional — the session works on the fallback —
            // so a return the host cannot make yet (`wait:changed` while the
            // agent's own background work runs: the relaunch would end it)
            // never holds the point back. The point's own act goes ahead
            // (`otherwise`: the continuation), the switch stays open, and the
            // return is asked again at the next idle point after the reset —
            // the first quiet one makes it. Waiting for it, as for a
            // fallback, typed nothing at any point of a tab that always has a
            // job running (69 asks in 1.5 s, no continuation, in a replay
            // of the policy).
            //
            // A MOVE TO ANOTHER ACCOUNT AHEAD OF A WALL is optional in the
            // same way — the session works where it is — and deferred the
            // same way (the accounts design, §6): a tab that always has a
            // job running would otherwise never be continued again.
            (Some("wait"), Some(step)) | (Some("busy"), Some(step))
                if matches!(
                    why,
                    Restart::ModelBack { .. }
                        | Restart::Account {
                            at: AccountAt::Ahead,
                            ..
                        }
                ) && (word == Some("wait") || step == "busy:another-sweep") =>
            {
                review.note(&format!(
                    "DEFERRED seq={seq} restart:{} waits for a quiet moment ({step}); the point goes on",
                    why.word()
                ));
                self.turn_end_execute(point, otherwise, opts, allow, review)
            }
            // A FALLBACK, a move to another account AT A WALL, and the memory
            // banner's restart, the host cannot make
            // yet is asked again on [`TurnEndTiming::restart_backoff`], the
            // last pause for every try after it — cut short at the bucket's
            // reset ([`Self::restart_later`]), where the continuation is
            // tried first ([`wall_action`]): nothing is given up for a
            // count, and nothing is typed blind at a wall a relaunch on the
            // fallback answers as soon as the agent's own jobs end.
            (Some("wait"), Some(step)) => {
                // The fallback's run, written at its first wait for a loop
                // that follows before its relaunch's own row
                // ([`approvals::relaunch_waits_reason`]).
                if self.restart_waits == 0 && matches!(why, Restart::Model { .. }) {
                    self.ledger_row(
                        rule,
                        approvals::Outcome::Waiting,
                        "",
                        &approvals::relaunch_waits_reason(asked_unix),
                        seq,
                    );
                }
                let r = self.turn_end_reread(&point.screen, opts);
                let due = r
                    .reset_at
                    .map(|at| at + self.turn_end.timing.grace_for(r.program) + r.jitter);
                self.restart_later(seq, why, step, due, review)
            }
            (Some("busy"), Some(step)) if step == "busy:another-sweep" => {
                self.restart_later(seq, why, step, None, review)
            }
            _ => {
                self.restart_waits = 0;
                self.restart_run = None;
                let r = self.turn_end_reread(&point.screen, opts);
                // The memory's `otherwise` still replaces the wall's own act
                // even when this read is no longer acted on: the next measure
                // not acted on hands the point back ([`Self::memory_edge`]).
                self.memory_over_wall |= over_wall;
                self.turn_end.restarted(why, false, &r, self.fx.mono());
                // An account move that could not be made: written down, so a
                // loop that starts at the same wall does not ask again
                // ([`approvals::account_moves`]). `skipped`, never `typed`:
                // the wall the earlier rows waited on still stands.
                if let Restart::Account { to, at } = why {
                    self.ledger_row(
                        rule,
                        approvals::Outcome::Skipped,
                        &format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND),
                        &approvals::account_unmade_reason(to, *at, step.as_deref()),
                        seq,
                    );
                }
                // THE WARNING AGREES WITH THE BADGE: nothing asks the host
                // again while this point stands, and the next point's ask is
                // mostly refused the same (a launch-bound refusal stands for
                // the agent's life; a `busy:` or a relaunch switched off live
                // may not), so the row's verdict is the person's restart until
                // the next point asks again or a restart is made, said again
                // at once ([`Self::memory_refused`],
                // [`Self::memory_warn`]) — never `aterm restarts it` beside
                // `the restart could not be made`.
                if matches!(why, Restart::Memory) {
                    self.memory_refused = true;
                    if let Some(over) = self.memory_measure(opts) {
                        self.memory_warn(&point.screen, over, opts, review);
                    }
                }
                match otherwise {
                    TurnEndAction::Escalate { reason } => {
                        let reason = match &step {
                            Some(step) => {
                                format!("the restart could not be made ({step}): {reason}")
                            }
                            None => reason,
                        };
                        self.escalate_point(point, Some(&reason), review)?;
                        if took {
                            self.mark_memory_badge();
                        }
                        Ok(())
                    }
                    other => self.turn_end_execute(point, other, opts, allow, review),
                }
            }
        }
    }

    /// THE MAC WOKE, as the sleep watch measured it at `at` on the monotonic
    /// clock and `wall_now` (unix seconds) on the wall clock
    /// ([`Self::watch_sleep`]). The network is coming back, so the wake's
    /// SETTLE is placed, [`WAKE_SETTLE`] (and the session's share of
    /// [`WAKE_SPREAD`], so the sessions of one account do not all type in the
    /// same second) from that read: from the wake, never from whatever the
    /// loop does next — a busy turn that runs on past it leaves nothing held.
    /// Until it falls ([`Self::settling`]) no turn is typed into the agent,
    /// whatever event reaches the loop first (the due falling, a repaint, an
    /// edge of the host's, a new point): no act of the policy's
    /// ([`Self::turn_end_execute`]), no step of the host's
    /// ([`Guard::WakeSettle`]), no accounts probe ([`ProbeGate::WakeSettle`]).
    /// A box the agent put up is still answered: its keys are no turn.
    pub(super) fn settle_wake(&mut self, wall_now: i64, at: Instant) {
        let share = self.setup.sid.as_deref().map_or(Duration::ZERO, |sid| {
            resume_jitter(sid, wall_now.div_euclid(60), WAKE_SPREAD)
        });
        let at = at + WAKE_SETTLE + share;
        let settle = Due {
            at,
            unix_ms: self.unix_ms_of(at),
            why: WOKE.to_string(),
            wait: WaitKind::Other,
        };
        self.wake_settle = Some((settle, false));
    }

    /// The wake the watch measured at `wall_now` (unix seconds), at a step of
    /// the wait on `seq` ([`Self::check_slept`]). A deadline the sleep carried
    /// past ([`Due::unix_ms`], its wall time) would fall at once
    /// ([`Self::due_at`]); while the wake's settle stands it is moved to it,
    /// and decided then on the reset the unix clock reads afresh
    /// ([`Self::wall_reset_at`]) and the policy's pauses the sleep counted on
    /// ([`TurnEndState::slept`]). A settle that has passed holds nothing: the
    /// deadline falls at once. A deadline still ahead stands at its wall time.
    /// Nothing is decided here: a point with nothing waited on (an escalated
    /// one, a question) is decided again by nothing, or its ask would be
    /// posted twice ([`Self::reach_edge`]'s rule).
    pub(super) fn woke(&mut self, seq: u64, wall_now: i64, review: &mut dyn Review) {
        let carried = self
            .turn_end_due
            .as_ref()
            .is_some_and(|d| d.unix_ms <= wall_now.saturating_mul(1000));
        if !carried {
            return;
        }
        if let Some(at) = self.settling() {
            self.settle_said(seq, at, review);
            self.set_due(at, WOKE);
        }
    }

    /// `WAITING seq=<seq> until=<the settle> the Mac woke: deciding again`,
    /// once a settle ([`Self::settle_wake`]): said by whichever comes first,
    /// a due the sleep carried past or an act held inside it.
    fn settle_said(&mut self, seq: u64, at: Instant, review: &mut dyn Review) {
        if let Some((_, said)) = &mut self.wake_settle
            && !*said
        {
            *said = true;
            review.note(&format!(
                "WAITING seq={seq} until={} {WOKE}: deciding again",
                self.stamp_in(at)
            ));
        }
    }

    /// A restart that must wait: decided again after the next pause of
    /// [`TurnEndTiming::restart_backoff`], `WAITING … until=<t>
    /// restart:<why> waits: <step>`.
    fn restart_later(
        &mut self,
        seq: u64,
        why: &Restart,
        step: &str,
        cut_at: Option<Instant>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let pause = crate::supervise::ladder::Ladder(&self.turn_end.timing.restart_backoff)
            .step(usize::try_from(self.restart_waits).unwrap_or(usize::MAX));
        self.restart_waits = self.restart_waits.saturating_add(1);
        let now = self.fx.mono();
        let mut until = now + pause;
        // The bucket's reset, when it comes before the next try: decided
        // then, the continuation first.
        if let Some(at) = cut_at.filter(|&at| at > now) {
            until = until.min(at);
        }
        let why = format!("restart:{} waits: {step}", why.word());
        if restart_note_due(self.restart_waits.saturating_sub(1)) {
            review.note(&format!(
                "WAITING seq={seq} until={} {why}",
                self.stamp_in(until)
            ));
        }
        self.set_due(until, why);
        Ok(())
    }

    /// Whether the session's foreground program, read fresh for this decision
    /// ([`Self::turn_end_at_point`], [`Self::turn_end_now`]), is an agent this
    /// policy types into: one whose reader is supervisable
    /// ([`aterm_phase::Program::supervisable`]: Claude Code, agy, Muse Code,
    /// Gemini CLI by its screen under `node`), never a shell, a Codex (whose
    /// screen aterm reads no more) or a program nothing reads. Otherwise journaled `SKIPPED`, nothing typed.
    fn speaks_to_its_agent(&mut self, seq: u64, rule: &str, review: &mut dyn Review) -> bool {
        let rows = self
            .last
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default();
        if self.reader(&rows).program().supervisable() {
            return true;
        }
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the session runs {}, no agent this policy types into",
            self.program.as_deref().unwrap_or("-")
        ));
        false
    }

    /// `text` typed and submitted under the guarded submit (module header):
    /// `true` when the server says it submitted it (`submitted=1`), or wrote
    /// the Enter unverified (`pressed=<n>`, journaled so). A guard that missed
    /// (`skipped`), a host without the guarded submit, a hold and a refusal
    /// are `false`, journaled; the hold is waited out and the refusal backed
    /// off, and the next look decides again.
    fn type_guarded(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        if self.caps.guarded_submit == Some(false) {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit"
            ));
            return Ok(false);
        }
        // The mark the composer is drawn with on this screen: Codex 0.158.0
        // draws its input line `»` where 0.157 drew `›`, and a guard spelled
        // with the other would never be pressed under.
        let reader = self.reader(&point.screen.rows);
        let Some(caret) = reader.caret_on(&point.screen.rows) else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} no composer this policy types into"
            ));
            return Ok(false);
        };
        let submit = format!(
            "submit=guarded:{}",
            composer_guard_at(caret, reader.caret_column(), text)
        );
        let r = self.call(&["turn", &submit, TURN_IDLE, TURN_TIMEOUT, TURN_YIELD, text])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!(
                "turn (the {rule} text) failed: {}",
                r.stderr.trim()
            )));
        }
        let why = one_line(r.err_text());
        if r.is_err("halted") || r.is_err("busy") || r.is_err("rate") {
            self.write_refused(r.is_err("halted"), &why, seq, review)?;
            return Ok(false);
        }
        // A person typing outlived the yield: nothing was typed. Decided
        // again shortly, where the draft (if one is left) types nothing.
        if r.is_err("yield") {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a person is typing: {why}"
            ));
            self.redecide_soon("a person was typing");
            return Ok(false);
        }
        if r.unknown_form() {
            self.caps.guarded_submit = Some(false);
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit: {why}"
            ));
            return Ok(false);
        }
        self.caps.guarded_submit = Some(true);
        if r.submitted() {
            return Ok(true);
        }
        let verdict = r.turn_verdict().unwrap_or("").to_string();
        if r.pressed_unverified() {
            review.note(&format!(
                "UNVERIFIED seq={seq} rule={rule} Enter written, the submit not seen: {verdict}"
            ));
            return Ok(true);
        }
        let verdict = if verdict.is_empty() { why } else { verdict };
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} not submitted: {verdict}"
        ));
        // `turn` pastes before its guard is checked: a miss leaves the text
        // typed (`reason=guard`), in the composer or wherever the cursor was.
        // Said, never left for the next look to read as a person's draft.
        if r.skipped() || verdict.contains("reason=guard") {
            self.left_typed(
                point,
                text,
                &format!("the guard missed ({verdict})"),
                review,
            )?;
        }
        Ok(false)
    }

    /// A fenced write the server skipped on the unchanged screen (a bare `OK
    /// skipped`: the generation held and the guard matched no row): nothing
    /// written, and a retry would miss the same way, so it is journaled
    /// `SKIPPED … the guard matched no row …` and escalated.
    fn not_written(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        guard: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the guard matched no row of the judged screen \
             ({guard}); nothing written"
        ));
        let reason = format!(
            "not typed: aterm could not find the prompt line: `{}`",
            clip(text)
        );
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &reason);
        self.escalate(
            seq,
            &attention,
            None,
            super::escalate::Escalation::Ask,
            review,
        )?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// The point decided again a fenced write's skip later
    /// ([`crate::harness::Timings::redecide_after_skip`]: the screen moved
    /// between the judged read and the write — a person's keystroke, a box)
    /// from now ([`Self::turn_end_now`], on the deadline of the loop's own
    /// wait).
    fn redecide_soon(&mut self, why: &str) {
        self.set_due(self.fx.mono() + self.setup.redecide_after_skip, why);
    }

    /// A write of the policy's act the server REFUSED FOR NOW wrote nothing:
    /// a hold (`ERR halted`) is parked on until it lifts, `ERR busy …` and
    /// `ERR rate` are backed off from ([`Self::refused`]), and then the point
    /// is decided again ([`Self::redecide_soon`]) as after a skipped write —
    /// an idle point that stands unchanged moves nothing the loop's wait
    /// latches on, so without the due the act was dropped: a continuation a
    /// hold refused was never typed after the hold lifted, and nobody was
    /// told (measured 16 min idle, the sweep of 2026-10-09).
    fn write_refused(
        &mut self,
        halted: bool,
        why: &str,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.refused(halted, why, seq, review)?;
        self.redecide_soon(if halted {
            "the hold that refused the write lifted"
        } else {
            "the server turned the write away"
        });
        Ok(())
    }

    /// Text this policy wrote and did not submit: escalated (the worker's
    /// `attention`, one ask), the badge cleared when the worker works. The
    /// accounts probe's own `/usage` is the one text of the loop's this is
    /// NOT called for: left typed, it is a record ([`Self::probe_type`]).
    fn left_typed(
        &mut self,
        point: &Turn,
        text: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        // What happened first: the badge is cut to fit.
        let reason = format!("typed, not submitted ({why}): `{}`", clip(text));
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &reason);
        self.escalate(
            point.screen.seq,
            &attention,
            None,
            super::escalate::Escalation::Ask,
            review,
        )?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// `text` typed and submitted: [`Self::type_fenced`] where the host fences
    /// `send`, else [`Self::type_guarded`] (module docs), either one
    /// [`Self::under_hand`]. A program that takes an Enter right behind typed
    /// text as a newline (Codex's and Muse Code's paste guard,
    /// [`ScreenReader::paste_guard`]) is written fenced too, its Enter held
    /// until the echo has been still [`PASTE_GUARD_SETTLE_MS`], so text never
    /// lands in a shell an agent exited to.
    fn type_text(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        self.under_hand(|s| match s.type_fenced(point, text, rule, review)? {
            Some(typed) => Ok(typed),
            None => s.type_guarded(point, text, rule, review),
        })
    }

    /// ONE TYPED ACT UNDER THIS PROCESS'S HAND, where the host asked for it
    /// ([`Session::type_under_hand`]): the hand is taken for the act
    /// ([`crate::harness::upgrade_drive::hand_take_here_request`]) and given
    /// back after it, whatever the act came to
    /// ([`crate::harness::upgrade_drive::hand_give_back_here_request`]).
    /// Under it no other connection's write lands between the text and its
    /// Enter (the server refuses them `ERR busy lease=…`), the window shows
    /// no row for it (aterm's own hand, ruling 313), and the server records
    /// the act as the harness's turn (`history … by=harness`: a `turn` typed
    /// under it, or a fenced `send` and the `key enter` behind it,
    /// `status=pressed`).
    ///
    /// Nothing else changes: every fence, refusal and latch of the act is its
    /// own. A tab some lease already holds is not taken — another driver's,
    /// a `turn`'s, or this process's own over another connection (a
    /// relaunch's or the upgrade's hand, which the acquire's `here` never
    /// moves to this one) — and the act goes out as it did before the hand,
    /// its writes refused or passed as the server decides. A hand this
    /// connection still holds (an earlier give-back that never arrived) is
    /// renewed, used and given back; one whose connection ended, a stop's
    /// cut included, ended with it (`lease acquire … here`). Nor is a hand
    /// taken for writes this loop refuses before they leave (stopped, a
    /// claim pending or behind another holder), or kept from a host that
    /// answers the acquire with anything but `OK lease acquired`.
    fn under_hand<T>(&mut self, act: impl FnOnce(&mut Self) -> Result<T, Fail>) -> Result<T, Fail> {
        let held = self.take_hand()?;
        let out = act(self);
        if held {
            let back =
                crate::harness::upgrade_drive::hand_give_back_here_request_as(self.setup.holder());
            let words: Vec<&str> = back.split(' ').collect();
            // Best effort: a hand taken `here` is its connection's, and the
            // server frees it as that connection ends — one lost, or hung up
            // by a stop (whose cut lets no `lease` out) — and on a connection
            // that lives on, the next act's acquire renews it and gives it
            // back.
            let _ = self.call(&words);
        }
        out
    }

    /// [`Self::under_hand`]'s take: whether the hand is now held.
    fn take_hand(&mut self) -> Result<bool, Fail> {
        if !self.setup.types_under_hand || self.stopped() || self.claim_refuses(&["send"]).is_some()
        {
            return Ok(false);
        }
        // Under the setup's holder — the window host's hand name, which its
        // loops are named by — never this process's own pid read again: a
        // replay runs in another process, and asks as the recorded run did.
        let take = crate::harness::upgrade_drive::hand_take_here_request_as(self.setup.holder());
        let words: Vec<&str> = take.split(' ').collect();
        let r = self.call(&words)?;
        Ok(r.ok() && r.stdout.starts_with("OK lease acquired"))
    }

    /// Whether the host's `send` takes the `if-gen=` fence (`help send` names
    /// it; an older `send` types a leading `if-gen=` as text, so it is never
    /// sent unprobed).
    pub(super) fn send_fence_known(&mut self) -> Result<bool, Fail> {
        if let Some(known) = self.caps.send_gen {
            return Ok(known);
        }
        let r = self.call(&["help", "send"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("help send failed: {}", r.stderr.trim())));
        }
        let known = r.ok() && super::super::policy::server_fences_send(&r.stdout);
        self.caps.send_gen = Some(known);
        Ok(known)
    }

    /// The fenced write (module docs): `None` when the host or the read cannot
    /// fence it (no generation on the read, no composer, `key`/`send` without
    /// `if-gen=`), and the caller falls back to [`Self::type_guarded`]; else
    /// whether the text was submitted. Once eligible, an absent person count
    /// refuses the write without falling back; both writes retain the original
    /// count and the read-back must still carry it.
    fn type_fenced(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Option<bool>, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            return Ok(None);
        };
        if !self.gen_fence_known()? || !self.send_fence_known()? {
            return Ok(None);
        }
        let Some(human) = point.screen.human_seq else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the judged read carries no person count; \
                 nothing written"
            ));
            return Ok(Some(false));
        };
        let person = format!("if-human={human}");
        let fence = format!("if-gen={generation}");
        let guard = format!("if={}", super::super::policy::row_guard(&rows[caret]));
        let r = self.call(&["send", &person, &fence, &guard, "--", text])?;
        match self.refused_press(&r, &format!("{person} {fence} {guard} (send)"))? {
            None => {}
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.write_refused(true, &why, seq, review)?;
                return Ok(Some(false));
            }
            Some(Pressing::Refused { why }) => {
                self.write_refused(false, &why, seq, review)?;
                return Ok(Some(false));
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                // Refused outright (a usage line): nothing written; never
                // retried unfenced. The help already proved the generation
                // form: this may be refusal of the person guard. Marking
                // generation unsupported would take the legacy fallback on
                // the next decision instead of retaining this strict form.
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                return Ok(Some(false));
            }
            Some(_) => return Ok(Some(false)),
        }
        if r.skipped_person() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a person keyed before the write; nothing written"
            ));
            self.redecide_soon("a person keyed before the write");
            return Ok(Some(false));
        }
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the write; nothing \
                 written"
            ));
            self.redecide_soon("the screen moved before the write");
            return Ok(Some(false));
        }
        if r.skipped() {
            // The screen was the one judged and the composer's guard matched
            // no row of it: a retry misses the same way. Nothing was written;
            // the point is the owner's, said.
            self.not_written(point, text, rule, &format!("{fence} {guard}"), review)?;
            return Ok(Some(false));
        }
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        let settle = if self.reader(rows).paste_guard() {
            PASTE_GUARD_SETTLE_MS
        } else {
            SETTLE_MS
        };
        self.wait(&["idle", settle], SETTLE_CAP)?;
        let now = self.screen()?;
        if now.human_seq != Some(human) {
            let turn = self.turn_of(now);
            self.left_typed(
                &turn,
                text,
                "the original person count changed or is unavailable",
                review,
            )?;
            return Ok(Some(false));
        }
        let reader = self.reader(&now.rows);
        let held = reader
            .composer(&now.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(text))
            && typed_draft(reader, &now)
            && reader.prompt(&now.rows).is_none();
        let (true, Some(generation), Some(caret)) =
            (held, now.generation.clone(), caret_row(reader, &now.rows))
        else {
            let turn = self.turn_of(now);
            self.left_typed(
                &turn,
                text,
                "the composer does not show it as written",
                review,
            )?;
            return Ok(Some(false));
        };
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let (r, args) = self.fenced_key(&guard, "enter", &generation, Some(&person))?;
        let turn = self.turn_of(now);
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(Some(true)),
            None if r.skipped_person() => {
                self.left_typed(&turn, text, "a person keyed before Enter", review)?;
                Ok(Some(false))
            }
            None => {
                self.left_typed(&turn, text, "the screen moved before Enter", review)?;
                Ok(Some(false))
            }
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                self.left_typed(&turn, text, "the session is halted", review)?;
                Ok(Some(false))
            }
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why, .. }) => {
                self.left_typed(&turn, text, &why, review)?;
                Ok(Some(false))
            }
            Some(_) => Ok(Some(false)),
        }
    }

    /// The draft standing in the composer submitted as it is (module docs):
    /// Enter, fenced on the judged read's generation and guarded on its caret
    /// row. `text` is the draft as read, for what is said. A host or a read
    /// without the fence gets nothing pressed: the draft is escalated.
    fn submit_draft(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let fenced = match (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) {
            (Some(generation), Some(caret)) if self.gen_fence_known()? => Some((generation, caret)),
            _ => None,
        };
        let Some((generation, caret)) = fenced else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a draft stands in the composer and this host \
                 has no fenced Enter"
            ));
            let reason = format!(
                "a draft stands in the composer, not submitted: `{}`",
                clip(text)
            );
            let attention = escalate::attention_text(self.reader(rows), point, &reason);
            self.escalate(
                seq,
                &attention,
                None,
                super::escalate::Escalation::Ask,
                review,
            )?;
            self.turn_end_badge = true;
            return Ok(false);
        };
        let Some((r, args)) =
            self.press_fenced(&rows[caret], "enter", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
        if r.skipped_changed() {
            // A keystroke since the read: decided again on the screen as it
            // is now.
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before Enter; the draft stays"
            ));
            self.redecide_soon("the screen moved before the draft's Enter");
            return Ok(false);
        }
        if r.skipped() {
            // The judged screen, and its caret row matched no row of it
            // ([`Self::type_fenced`]'s bare skip).
            self.not_written(point, text, rule, &args, review)?;
            return Ok(false);
        }
        Ok(true)
    }

    /// `key if-gen=<generation> if=<row's guard> <key>`, the fenced press
    /// [`Self::submit_draft`] and [`Self::accept_suggestion`] make, with a
    /// refusal answered as both answer one: a lost connection is the error; a
    /// hold is parked on, a busy server backed off from, an unconfirmed press
    /// noted, each `None`, nothing written. `Some` is the server's `OK …`
    /// (pressed or skipped) and the arguments as sent.
    fn press_fenced(
        &mut self,
        row: &str,
        key: &str,
        generation: &str,
        (seq, rule): (u64, &str),
        review: &mut dyn Review,
    ) -> Result<Option<(CtlReply, String)>, Fail> {
        let guard = super::super::policy::row_guard(row);
        let args = super::super::policy::key_args(&guard, key, Some(generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None => return Ok(Some((r, args))),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.write_refused(true, &why, seq, review)?,
            Some(Pressing::Refused { why }) => self.write_refused(false, &why, seq, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
            }
            Some(_) => {}
        }
        Ok(None)
    }

    /// The worker's own suggestion accepted (module docs): `right`, fenced on
    /// the judged read's generation and guarded on the suggestion's row; the
    /// screen read once it has settled; then, when it shows the composer
    /// holding exactly the suggestion, typed now (the cursor off the
    /// placeholder's column), Enter, fenced on that read and guarded on that
    /// row. A host or a read without the generation fence types the
    /// suggestion's words instead ([`Self::type_guarded`]), as does a
    /// suggestion the accept key did not fill.
    ///
    /// THE WINDOW'S HOST TYPES THEM TOO ([`Session::type_under_hand`],
    /// [`Self::type_text`] under its hand): the accept key leaves the server
    /// a `right` and an Enter and no words, so the ledger could not book
    /// the submitted suggestion as the harness's turn (`history …
    /// by=harness`), and a reader of the worker's transcript would take it
    /// for a person's prompt. An allowed suggestion is made of the
    /// continuation's own phrases
    /// ([`suggestion_allowed`](super::super::policy::turn_end::suggestion_allowed)),
    /// and the vendor counts a submitted text equal to its suggestion as
    /// accepted.
    ///
    /// A READ TORN MID-REDRAW is not judged. The worker clears its composer
    /// and draws it again as it takes the key; a read between the two (the
    /// screen still moving when [`SETTLE_CAP`] ran out) shows no caret row at
    /// all. That read returned here with the suggestion already FILLED —
    /// nothing pressed, nothing journaled, nothing escalated — and the fill
    /// waited out the draft path's grace, minutes on. It is read once more
    /// after one more settle — a second bounded read on the
    /// [`REACTION_WAIT`] design, not a loop — and a
    /// composer still missing then is journaled `SKIPPED` and nothing more is
    /// pressed (no read showed the fill, so none is claimed).
    fn accept_suggestion(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        if self.setup.types_under_hand {
            return self.type_text(point, text, rule, review);
        }
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            return self.type_text(point, text, rule, review);
        };
        if !self.gen_fence_known()? {
            return self.type_text(point, text, rule, review);
        }
        let Some((r, args)) =
            self.press_fenced(&rows[caret], "right", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the accept key"
            ));
            self.redecide_soon("the screen moved before the accept key");
            return Ok(false);
        }
        if r.skipped() {
            // The judged screen, and its suggestion's row guard matched no
            // row of it ([`Self::type_fenced`]'s bare skip).
            self.not_written(point, text, rule, &args, review)?;
            return Ok(false);
        }
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let mut now = self.screen()?;
        if caret_row(self.reader(&now.rows), &now.rows).is_none() {
            // Torn mid-redraw (the doc): one more settle, one more read.
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
        let reader = self.reader(&now.rows);
        let shows = caret_text(reader, &now.rows).as_deref() == Some(text);
        let filled = shows && typed_draft(reader, &now) && reader.prompt(&now.rows).is_none();
        let (generation, caret) = match (now.generation.clone(), caret_row(reader, &now.rows)) {
            (Some(generation), Some(caret)) => (generation, caret),
            (_, None) => {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} no composer after the accept key on two \
                     settled reads; nothing more is pressed"
                ));
                return Ok(false);
            }
            (None, Some(_)) => {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the read after the accept key carries no \
                     generation to fence Enter on; nothing more is pressed"
                ));
                return Ok(false);
            }
        };
        if !filled {
            // The key did not fill it (a build without the accept key, the
            // suggestion gone): its words, typed — once, guarded.
            if !(shows && !typed_draft(reader, &now)) {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the composer no longer shows the suggestion"
                ));
                return Ok(false);
            }
            let turn = self.turn_of(now);
            return self.type_text(&turn, text, rule, review);
        }
        let Some((r, _)) =
            self.press_fenced(&now.rows[caret], "enter", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
        if r.skipped() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the composer moved before Enter; the suggestion \
                 stays filled"
            ));
            return Ok(false);
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// The accounts probe (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §3)
// ---------------------------------------------------------------------------

/// What the host is told, once, when the panel's Escape has failed
/// [`PROBE_STUCK_AT`] times ([`IdleHost::inform`]: a record, no badge): the
/// state on its first line, the one action that ends it on the second.
const PANEL_STUCK: &str = "The /usage panel did not close\nPress Esc in this tab.";

/// What the host is told, once, when the probe's `/usage` was written into
/// the composer and its Enter was not ([`IdleHost::inform`]: a record — no
/// badge, no attention, no ask: the probe's own words left typed are
/// no state a person is needed for NOW, so they are taken out of the
/// escalation every other typed act's leftover gets,
/// [`Session::left_typed`]). The state on its first line; beneath it, what
/// becomes of the text: sent where the policy sends a draft in an act's
/// place ([`Session::probe_left_enter`]); at a point with no act, standing
/// until a person sends or clears it — the loop types nothing more into a
/// composer a person may be at.
const USAGE_LEFT: &str = "/usage was typed and not sent\nThe supervisor's own check of the \
                          account: it is sent with the session's next act, or stands until you \
                          send or clear it; its panel is closed either way.";

/// How many reads in a row must say the same thing of the open panel before
/// the probe acts on it: two `Settled` reads with equal windows are a sample,
/// two folded ones a fold, two that are not the Usage tab another panel. One
/// read alone is a frame that may be half drawn.
const PROBE_READS: u32 = 2;

/// How long the screen must hold still before the probe's next read of it
/// ([`Session::probe_pace`]), in the server's `await idle` milliseconds.
const PROBE_STILL_MS: &str = "50";

/// The host told what a claimed probe read ([`IdleHost::account_probed`]),
/// AT MOST ONCE however the step ends: the first word is the one that
/// counts, and [`Session::usage_probe`] says [`ProbeMiss::Lost`] for a step
/// that ended on an error with nothing told.
struct Telling<'h> {
    host: &'h dyn IdleHost,
    told: bool,
}

impl Telling<'_> {
    fn tell(&mut self, read: Result<&Sample, ProbeMiss>) {
        if !std::mem::replace(&mut self.told, true) {
            self.host.account_probed(read);
        }
    }
}

/// What the probe's fenced write did ([`Session::probe_type`]).
enum ProbeTyped {
    /// `/usage` is in the composer and its Enter was written: `at` is the
    /// server's stamp on the Enter, `human` the person count it was fenced
    /// on — a read that carries another saw a person key since.
    Entered { at: u64, human: u64 },
    /// It was not submitted: what the host is told, why (the journal's
    /// words), and what the look does next. `left`: the text WAS written and
    /// stands in the composer — a record for a person ([`USAGE_LEFT`]),
    /// never an escalation.
    Not {
        miss: ProbeMiss,
        why: String,
        then: Probed,
        left: bool,
    },
}

/// A percentage as the journal's `PROBED` line carries it: `7%`, or `?%`
/// for a figure nobody read.
fn pct_word(pct: Option<u32>) -> String {
    pct.map_or_else(|| "?%".to_string(), |p| format!("{p}%"))
}

impl<C: Ctl> Session<'_, C> {
    /// ONE `/usage` PROBE, AS A STEP OF THE LOOP
    /// (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §3; the model is
    /// aterm-spec's `supervisor_usage_probe_model`): where the host claims
    /// one for this point, type `/usage`, read the Settings panel it opens
    /// until its Usage tab reads SETTLED, hand the host the sample, and close
    /// the panel with an Escape. It is a step between the host's own
    /// ([`Self::host_steps_here`]) and the point's act
    /// ([`Self::turn_end_execute`]), and NO ACT OF THE TURN-END POLICY'S:
    /// [`TurnEndState::acted`] is never called for it, no panel screen is
    /// folded into the policy's memory, and its ledger row is left out of the
    /// typing budget ([`approvals::typed_at`]).
    ///
    /// WHAT WAS MEASURED, and what is only read. MEASURED on Claude Code
    /// 2.1.287, 2026-10-01 (a private headless aterm, six instances, nine
    /// opens, the fullscreen renderer at 150x50, 129x47, 110x32, 60x50 and
    /// 150x80): Enter after `/usage` ran `usage` every time (the typeahead
    /// lists it first, `/usage-credits` second); the panel was on the first
    /// read after the Enter; `key if=<header row> esc` closed it eight times
    /// of eight, the header gone within 35 ms, the composer back and the
    /// transcript not one row longer. READ in the 2.1.287 bundle, never seen:
    /// the loading and failed states, an enrich error, what Escape does with
    /// the panel over a running turn (never pressed there, below). NOT
    /// EXERCISED AT ALL: a second account, and this step against a real
    /// session — the measurement was taken by hand, with the harness off.
    ///
    /// THE GATES ([`Self::probe_gates`]), any one shut and nothing is asked
    /// of the host or written. Then the host's claim
    /// ([`IdleHost::account_probe`]), and from there the host is told what
    /// came of it exactly once ([`IdleHost::account_probed`]). A DUE PROBE
    /// THAT IS NOT RUN SAYS WHICH GATE SHUT, once a point
    /// ([`Self::probe_shut`]); where the roster did not say one is due —
    /// every point of every session with no roster — nothing is asked and
    /// nothing said.
    ///
    /// EVERY WRITE IS FENCED THREE WAYS, and there is no unfenced fallback
    /// ([`Self::probe_type`]): the person count (`if-human=`), the screen
    /// generation (`if-gen=`) and the composer's caret row (`if=`) on the
    /// text and on its Enter. The Escape is fenced by the person count and
    /// by THE PANEL'S OWN HEADER ROW, checked by the server at the press
    /// ([`Self::panel_escape`]) — it cannot be written unless the panel is on
    /// the grid at that instant — and never while a busy signal shows:
    /// Escape stops a running turn.
    ///
    /// THE READ ([`usage_panel_read`]) is taken every [`ProbeWaits::step`] at
    /// the latest ([`Self::probe_pace`]), NOT once the screen has held still
    /// half a second (the spec's `await idle 500`, capped at 1.5 s): the
    /// measurement contradicts the spec's "unsettled, then settled or
    /// folded". On an ordinary grid the
    /// tab reads unsettled for a tenth to a quarter of a second (the home's
    /// last-known figures under a refresh line), SETTLED for about a second
    /// while its "contributing" block is still scanning, and FOLDED for good
    /// once that block fills in and pushes the tail off the grid. So:
    ///
    /// * two reads in a row `Settled` with EQUAL windows are the sample
    ///   (measured: the windows of the short panel equal the final ones in
    ///   every run);
    /// * two in a row folded are [`ProbeMiss::Fold`] — and the grid's size
    ///   is latched off for this loop ([`Session::probe_fold`]) ONLY where
    ///   EVERY read of this probe was the fold: a size whose tail never
    ///   shows (110x32, measured: folded from the first read, the refresh
    ///   line never on the grid) is not typed into again, while a race lost
    ///   on a loaded machine (129x47, the owner's own size: unsettled, then
    ///   settled for 1.1 s) is tried again at the roster's next due read.
    ///   The spec latched both; the measurement says the second is not a
    ///   property of the size;
    /// * two in a row that are not the Usage tab under the fullscreen
    ///   renderer's rule are [`ProbeMiss::Renderer`], and this loop probes
    ///   no more;
    /// * a person's key since the Enter is [`ProbeMiss::Person`]: nothing
    ///   is sampled from a panel a person may be driving, and no Escape goes
    ///   until their grace has passed ([`Self::usage_panel_look`]);
    /// * the panel gone by itself is [`ProbeMiss::Closed`]; the bound run
    ///   out, [`ProbeMiss::Unsettled`].
    ///
    /// THE PANEL IS OWED ITS CLOSE FROM THE WRITE ON ([`UsageProbe`]), in
    /// memory and in the session ledger, whatever happens to this step — a
    /// reply that never comes, the loop's stop. A stopped loop writes
    /// nothing ([`Session::call`] refuses it), so a stop mid-probe presses
    /// no Escape: the loop that follows reads the ledger and closes it (the
    /// spec had one Escape go on a stop; the loop's own rule is older and
    /// stricter).
    pub(super) fn usage_probe(
        &mut self,
        point: &Turn,
        decided: &TurnEndAction,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Probed, Fail> {
        let Some(host) = opts.idle_host.clone() else {
            return Ok(Probed::No);
        };
        // The roster's word first: with none, no gate is asked and nothing
        // is said.
        if !self.probe_advised(&point.screen) {
            return Ok(Probed::No);
        }
        let shut = match self.probe_gates(point, decided, host.as_ref(), opts)? {
            Some(gate) => Some((gate, None)),
            None => host
                .account_probe()
                .err()
                .map(|why| (ProbeGate::Claim, Some(why.word()))),
        };
        if let Some((gate, why)) = shut {
            self.probe_shut(point, gate, why, review)?;
            return Ok(Probed::No);
        }
        // Claimed: the point has its probe, and no line about one not run.
        self.probe_said = Some(self.review_key(point));
        let mut tell = Telling {
            host: host.as_ref(),
            told: false,
        };
        let done = self.probe_step(point, &mut tell, opts, review);
        // A step that ended on an error with nothing told: its claim is let
        // go, and what it did is not known.
        if !tell.told {
            tell.tell(Err(ProbeMiss::Lost));
            review.note(&format!(
                "PROBED seq={} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} no sample ({})",
                point.screen.seq,
                ProbeMiss::Lost.word()
            ));
        }
        done
    }

    /// Whether the advice this point's decision was given
    /// ([`Self::account_advise`], kept for the screen it was asked on) is the
    /// probe: the roster says the account's `/usage` read is due here. The
    /// spec's fourth gate, asked FIRST: with no roster — no host, a host that
    /// keeps none, `accounts = "off"` — it is `false` before any other gate
    /// is looked at, before a request is made, and before anything is said.
    fn probe_advised(&self, screen: &Screen) -> bool {
        matches!(
            &self.account_seen,
            Some((at, AccountPoint::TurnEnd, Some(AccountAdvice::Probe { .. })))
                if *at == screen.seq
        )
    }

    /// A DUE PROBE WAS NOT RUN AT `point`: say which gate shut, ONCE A POINT
    /// — `SKIPPED seq=<n> rule=usage-probe@v1 gate=<word>` ([`ProbeGate`]),
    /// and for a claim the host refused its own word after it (`gate=claim
    /// host=<word>`). SAID, not only journaled: the line is in the session's
    /// journal and, for a hosted loop, in its host's log beside the advice
    /// that asked for the probe — where a person looking for why no `/usage`
    /// was typed reads, and a journal-only line was found by nobody.
    ///
    /// Nothing where the roster did not say a probe is due at this screen
    /// ([`Self::probe_advised`]), and nothing a second time at one point
    /// ([`Session::probe_said`]: its review key): an idle session is decided
    /// again at every wait that runs out, for hours, and its one point has
    /// one line — the first gate that shut. A point where a probe WAS
    /// claimed has none at all. A point taken after a turn ran is another
    /// point ([`Session::look`] forgets the last), though it reads the same.
    pub(super) fn probe_shut(
        &mut self,
        point: &Turn,
        gate: ProbeGate,
        host: Option<&str>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if !self.probe_advised(&point.screen) {
            return Ok(());
        }
        let key = self.review_key(point);
        if self.probe_said.as_deref() == Some(key.as_str()) {
            return Ok(());
        }
        self.probe_said = Some(key);
        let host = host.map_or_else(String::new, |word| format!(" host={word}"));
        review.say(&format!(
            "SKIPPED seq={} rule={RULE_USAGE_PROBE} gate={}{host}",
            point.screen.seq,
            gate.word()
        ))?;
        Ok(())
    }

    /// THE PROBE'S GATES, in the spec's order (§3 G) but for its fourth —
    /// the roster's advice, which [`Self::usage_probe`] asks before these
    /// ([`Self::probe_advised`]): `None` with every one open, else THE FIRST
    /// THAT SHUT ([`ProbeGate`]), for the one line the loop says of a due
    /// probe it did not run. Nothing is written by any of them, and the
    /// requests the later ones make (`help key`, `help send`, `status`,
    /// `modes`) are made only where the roster advised the probe: with no
    /// roster none of this is reached.
    fn probe_gates(
        &mut self,
        point: &Turn,
        decided: &TurnEndAction,
        host: &dyn IdleHost,
        opts: &SuperviseOpts,
    ) -> Result<Option<ProbeGate>, Fail> {
        // 1. A loop that acts on the session and keeps a ledger for it, with
        //    no panel of its own still owed and nothing that ended its
        //    probes.
        if self.claim.watching_behind().is_some() {
            return Ok(Some(ProbeGate::Watching));
        }
        if self.setup.ledger_path.is_none() {
            return Ok(Some(ProbeGate::NoLedger));
        }
        if self.probe_off {
            return Ok(Some(ProbeGate::ProbesOff));
        }
        if self.probe != UsageProbe::Idle {
            return Ok(Some(ProbeGate::PanelOwed));
        }
        // 2. The turn that ended here was read live.
        if !self.turn_live {
            return Ok(Some(ProbeGate::NotLive));
        }
        // 3. The point is not a person's.
        if matches!(decided, TurnEndAction::Escalate { .. }) {
            return Ok(Some(ProbeGate::Escalated));
        }
        // 5. Not a grid whose tail never showed.
        let screen = &point.screen;
        if self.probe_fold == Some((screen.cols, screen.pane_rows)) {
            return Ok(Some(ProbeGate::FoldLatch));
        }
        // 6. The host asks for no point of its own, no stall is held or
        //    suspected, and the Mac's wake has settled
        //    ([`Self::settle_wake`]).
        if host.wants() {
            return Ok(Some(ProbeGate::HostWants));
        }
        if self.stalled.is_some() || self.stall_suspect {
            return Ok(Some(ProbeGate::Stalled));
        }
        if self.settling().is_some() {
            return Ok(Some(ProbeGate::WakeSettle));
        }
        // 7. Every fence the three writes carry is one this host has: a read
        //    that names its generation and its person count, `key` and
        //    `send` that take `if-gen=`.
        if screen.generation.is_none()
            || screen.human_seq.is_none()
            || !self.gen_fence_known()?
            || !self.send_fence_known()?
        {
            return Ok(Some(ProbeGate::NoFence));
        }
        // 8. One fresh `status` — the program and a person's last key — and
        //    the reading built again on it: the policy's own gate. The
        //    program is Claude Code as the server names it NOW.
        self.program = self.foreground_program()?;
        if self.stall_suspect {
            return Ok(Some(ProbeGate::Stalled));
        }
        match self.reader(&screen.rows).program() {
            Program::Claude => {}
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                return Ok(Some(ProbeGate::NotClaude));
            }
        }
        // The point was decided: its wall as the decision judged it.
        let reading = self.turn_end_reread(screen, opts);
        if let Some(gate) = self
            .turn_end
            .probe_gate(&reading, &opts.policy, self.fx.mono())
        {
            return Ok(Some(gate));
        }
        // 9. The fullscreen renderer.
        Ok((!self.alt_screen()?).then_some(ProbeGate::NotFullscreen))
    }

    /// Whether the session's program draws on the ALTERNATE screen (`modes`'
    /// `alt_screen=true` row — MEASURED 2026-10-01: the server prints the
    /// word, `true` or `false`, where the spec wrote `1`): Claude Code's
    /// fullscreen renderer, the one the probe types into. Its inline
    /// renderer is the main screen, and there `/usage` leaves two rows in
    /// the transcript and, once the panel has grown, scrolls its own header
    /// off the grid, where no header guard can fence an Escape (both
    /// measured). A host without the verb, or a reply with no such row, is
    /// `false`: not probed.
    fn alt_screen(&mut self) -> Result<bool, Fail> {
        let r = self.call(&["modes"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("modes failed: {}", r.stderr.trim())));
        }
        Ok(r.ok() && r.stdout.lines().any(|l| l.trim() == "alt_screen=true"))
    }

    /// Whether the composer on `screen` holds exactly the probe's command as
    /// typed text: the loop's own words, not yet submitted.
    fn holds_probe_draft(&self, screen: &Screen) -> bool {
        let reader = self.reader(&screen.rows);
        reader
            .composer(&screen.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(USAGE_COMMAND))
            && typed_draft(reader, screen)
    }

    /// A PANEL OF THE HARNESS'S MAY COME: `/usage` was written into the
    /// composer (or its write's answer never came). Owed from here, in
    /// memory and in the ledger — the `typed` row a later loop reads
    /// ([`approvals::probe_owed`]) — until a look sees the session with no
    /// panel and none of the text left ([`Self::probe_closed`]). The spec
    /// wrote the row at the Enter; it is written at the text, because text
    /// of ours that stands unsubmitted may still be sent — by a person's
    /// Enter, or by the loop where the policy sends a draft left standing
    /// ([`Self::probe_left_enter`]) — and the panel that opens then must be
    /// one the loop closes.
    fn probe_owes(&mut self, seq: u64) {
        if self.probe != UsageProbe::Idle {
            return;
        }
        self.probe = UsageProbe::Owed {
            own: true,
            since: self.fx.mono(),
        };
        self.ledger_row(
            RULE_USAGE_PROBE,
            approvals::Outcome::Typed,
            USAGE_COMMAND,
            approvals::PROBE_TYPED_REASON,
            seq,
        );
    }

    /// THE SESSION WAS SEEN WITH NO PANEL OF THE HARNESS'S LEFT ON IT: the
    /// ledger's `waiting` row, `CLEARED` where the journal said the panel
    /// was stuck, and the point it stood over decided again shortly
    /// ([`Self::turn_end_now`]: the act decided there may never have been
    /// carried out). Nothing where nothing was owed.
    fn probe_closed(&mut self, seq: u64, review: &mut dyn Review) {
        let was = std::mem::replace(&mut self.probe, UsageProbe::Idle);
        self.probe_hold = None;
        if was == UsageProbe::Idle {
            return;
        }
        self.ledger_row(
            RULE_USAGE_PROBE,
            approvals::Outcome::Waiting,
            USAGE_COMMAND,
            approvals::PROBE_CLOSED_REASON,
            seq,
        );
        if matches!(was, UsageProbe::Closing { .. }) {
            review.note(&format!("CLEARED seq={seq} usage panel"));
        }
        self.redecide_soon("the usage panel closed");
    }

    /// THE COMPOSER IS DRAWN on `screen` (`true`): no panel stands in its
    /// place, so a panel owed is closed ([`Self::probe_closed`]) — unless the
    /// composer still holds the probe's own command unsubmitted
    /// ([`Self::holds_probe_draft`]), which may yet be sent: owed still.
    /// `false`, and nothing done, where no composer is drawn.
    fn composer_settles(&mut self, screen: &Screen, review: &mut dyn Review) -> bool {
        if self.reader(&screen.rows).composer(&screen.rows).is_none() {
            return false;
        }
        if !self.holds_probe_draft(screen) {
            self.probe_closed(screen.seq, review);
        }
        true
    }

    /// THE PAUSE BETWEEN TWO READS OF THE PROBE'S: the screen holding still
    /// [`PROBE_STILL_MS`], or [`ProbeWaits::step`] at most, never past
    /// `until` — the server's own wait, no sleep here. A panel that sits is
    /// read twenty times a second and one that repaints without a pause four
    /// times: the cadences the live measurement read it at, either of which
    /// puts two reads inside the second the whole tab is on the grid — and
    /// never a read per repaint of a screen that animates.
    fn probe_pace(&mut self, until: Instant) -> Result<(), Fail> {
        let left = until.saturating_duration_since(self.fx.mono());
        self.wait(
            &["idle", PROBE_STILL_MS],
            left.min(self.setup.probe_waits.step),
        )?;
        Ok(())
    }

    /// The step past the claim ([`Self::usage_probe`]): type, await the
    /// panel, read it settled, tell the host, close.
    fn probe_step(
        &mut self,
        point: &Turn,
        tell: &mut Telling<'_>,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Probed, Fail> {
        let seq = point.screen.seq;
        // T. Type.
        let (entered, human) = match self.under_hand(|s| s.probe_type(point, review))? {
            ProbeTyped::Entered { at, human } => (at, human),
            ProbeTyped::Not {
                miss,
                why,
                then,
                left,
            } => {
                tell.tell(Err(miss));
                review.note(&format!("SKIPPED seq={seq} rule={RULE_USAGE_PROBE} {why}"));
                // The text stands typed and unsent: this journal line and one
                // record for a person, once — nothing is raised for it.
                if left {
                    tell.host.inform(USAGE_LEFT);
                }
                // Nothing of the point's is carried out at this look: it is
                // decided again shortly, unless a wait is set already.
                if then == Probed::Again && self.turn_end_due.is_none() {
                    self.redecide_soon("the accounts probe was not written");
                }
                return Ok(then);
            }
        };
        let no_sample = |miss: ProbeMiss, tell: &mut Telling<'_>, review: &mut dyn Review| {
            tell.tell(Err(miss));
            review.note(&format!(
                "PROBED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} no sample ({})",
                miss.word()
            ));
        };
        let waits = self.setup.probe_waits;
        let busy = |s: &Self, grid: &Screen| s.reader(&grid.rows).phase(&grid.rows) == Phase::Busy;
        // O. Await the panel: whole reads, each after the screen's next
        // move. A turn that started instead, or no panel within the bound,
        // is the end of it — and NO ESCAPE: nothing says a panel is there to
        // take one. It stays owed, so one that comes late is still closed.
        let open_by = self.fx.mono() + waits.open;
        self.wait(&["seq", &entered.to_string()], waits.step.min(waits.open))?;
        let mut grid = loop {
            let grid = self.grid()?;
            if aterm_phase::settings_panel(&grid.rows).is_some() {
                break grid;
            }
            if busy(self, &grid) || self.fx.mono() >= open_by {
                no_sample(ProbeMiss::NotOpened, tell, review);
                return Ok(Probed::Done);
            }
            self.probe_pace(open_by)?;
        };
        // S. Read it settled.
        let settle_by = self.fx.mono() + waits.settle;
        let mut prior: Option<Vec<PanelWindow>> = None;
        let (mut folds, mut strange, mut tail_seen) = (0u32, 0u32, false);
        let (read, panel) = loop {
            let Some(panel) = aterm_phase::settings_panel(&grid.rows) else {
                no_sample(ProbeMiss::Closed, tell, review);
                return Ok(Probed::Done);
            };
            // A person keyed since the Enter: the panel may be theirs to
            // drive now. Nothing is sampled, and its Escape waits out their
            // grace ([`Self::usage_panel_look`]).
            if grid.human_seq != Some(human) {
                no_sample(ProbeMiss::Person, tell, review);
                return Ok(Probed::Open);
            }
            // A turn runs under the panel: no sample, and no Escape while
            // it does.
            if busy(self, &grid) {
                no_sample(ProbeMiss::Unsettled, tell, review);
                return Ok(Probed::Open);
            }
            let said = if panel.renderer == aterm_phase::PanelRenderer::Fullscreen {
                usage_panel_read(&grid.rows, panel.header)
            } else {
                PanelRead::NotUsage
            };
            folds = if said == PanelRead::Fold {
                folds + 1
            } else {
                0
            };
            strange = if said == PanelRead::NotUsage {
                strange + 1
            } else {
                0
            };
            // Any reading but the fold's saw the panel's tail region on the
            // grid: the refresh line and the cancel hint are drawn there.
            tail_seen |= matches!(said, PanelRead::Settled(_) | PanelRead::Unsettled);
            match said {
                PanelRead::Settled(windows) => {
                    if prior.as_ref() == Some(&windows) {
                        let now = self.now_unix();
                        let offset = self.local_offset_s();
                        let zone = self.zone_lookup();
                        break (
                            panel_sample(&windows, now, offset, &zone).ok_or(ProbeMiss::Unsettled),
                            panel,
                        );
                    }
                    prior = Some(windows);
                }
                PanelRead::Unsettled => prior = None,
                PanelRead::Fold => {
                    prior = None;
                    if folds >= PROBE_READS {
                        // Latched only where the tail never showed: every
                        // read of this probe was the fold.
                        if !tail_seen {
                            self.probe_fold = Some((point.screen.cols, point.screen.pane_rows));
                        }
                        break (Err(ProbeMiss::Fold), panel);
                    }
                }
                PanelRead::NotUsage => {
                    prior = None;
                    if strange >= PROBE_READS {
                        self.probe_off = true;
                        break (Err(ProbeMiss::Renderer), panel);
                    }
                }
            }
            if self.fx.mono() >= settle_by {
                break (Err(ProbeMiss::Unsettled), panel);
            }
            self.probe_pace(settle_by)?;
            grid = self.grid()?;
        };
        // The host is told BEFORE the Escape: a close that drags on never
        // costs the sample.
        match &read {
            Ok(sample) => {
                tell.tell(Ok(sample));
                review.say(&format!(
                    "PROBED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} session={} week={}",
                    pct_word(sample.session_pct),
                    pct_word(sample.week_pct)
                ))?;
            }
            Err(miss) => no_sample(*miss, tell, review),
        }
        // C. Close (a probe is typed under Cruise or Land only: an Off
        // loop is parked).
        self.panel_escape(&grid, panel, false, opts, review)?;
        Ok(if self.probe == UsageProbe::Idle {
            Probed::Done
        } else {
            Probed::Open
        })
    }

    /// THE PROBE'S FENCED WRITE, a twin of [`Self::type_fenced`] with the
    /// person fence added and NO fallback: `send if-human=<n> if-gen=<the
    /// judged read> if=<its caret row> -- /usage`, the echo waited for and
    /// read as the fenced write reads it, the composer required to hold
    /// exactly the command as typed text with no box up, then `key
    /// if-human=<n'> if-gen=<that read> if=<its caret row> enter`. Nothing
    /// here ever falls back to the guarded `turn`, which pastes before its
    /// guard is checked.
    ///
    /// A write the server skipped or turned away wrote nothing
    /// ([`ProbeTyped::Not`]): for a person or a moved screen the point is
    /// decided again shortly; a guard that matched no row of the unchanged
    /// screen leaves the point's own act to go on (and to miss, and say so,
    /// the same way); a hold is parked on and a busy server backed off
    /// from.
    ///
    /// TEXT WRITTEN AND NOT SUBMITTED IS A RECORD, NEVER AN ESCALATION
    /// ([`ProbeTyped::Not`]'s `left`; alerts have a class, so this is not
    /// escalated as every other fenced write's leftover is,
    /// [`Self::left_typed`] — a ruling against the spec's row T for this exit
    /// in `docs/SPEC-claude-accounts-probe-2026-10-01.md`, which still says
    /// escalate):
    /// the journal's `SKIPPED … typed, not submitted: <why>` and one record
    /// through [`IdleHost::inform`] ([`USAGE_LEFT`]). No attention is set,
    /// no ask posted, no badge kept: nothing here is a state a person is
    /// needed for now. The panel stays owed ([`Self::probe_owes`]), the
    /// probe is not typed again over its own text (the first of
    /// [`Self::probe_gates`], and the policy's own gate on a composer that
    /// is not empty), and what becomes of the text is
    /// [`Self::probe_left_enter`]'s.
    fn probe_type(&mut self, point: &Turn, review: &mut dyn Review) -> Result<ProbeTyped, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let not = |miss: ProbeMiss, why: &str, then: Probed| {
            Ok(ProbeTyped::Not {
                miss,
                why: why.to_string(),
                then,
                left: false,
            })
        };
        // Written and not submitted: the point is decided again shortly.
        let left = |miss: ProbeMiss, why: &str| {
            Ok(ProbeTyped::Not {
                miss,
                why: format!("typed, not submitted: {why}"),
                then: Probed::Again,
                left: true,
            })
        };
        let (Some(generation), Some(human), Some(caret)) = (
            point.screen.generation.clone(),
            point.screen.human_seq,
            caret_row(self.reader(rows), rows),
        ) else {
            return not(
                ProbeMiss::NotWritten,
                "the judged read carries no fence; nothing written",
                Probed::No,
            );
        };
        let person = format!("if-human={human}");
        let fence = format!("if-gen={generation}");
        let guard = format!("if={}", super::super::policy::row_guard(&rows[caret]));
        let r = self.call(&["send", &person, &fence, &guard, "--", USAGE_COMMAND])?;
        match self.refused_press(&r, &format!("{person} {fence} {guard} (send)"))? {
            None => {}
            Some(Pressing::Lost { why, .. }) => {
                // Cut mid-exchange, the text may be in the composer.
                if !r.untouched() {
                    self.probe_owes(seq);
                }
                return Err(Fail::Lost(why));
            }
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                return not(
                    ProbeMiss::NotWritten,
                    "the session is halted; nothing written",
                    Probed::Again,
                );
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                return not(
                    ProbeMiss::NotWritten,
                    "the server turned the write away; nothing written",
                    Probed::Again,
                );
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                // Refused outright (a usage line): this host has no fenced
                // `send` after all, and nothing was written.
                self.caps.send_gen = Some(false);
                return not(ProbeMiss::NotWritten, &why, Probed::No);
            }
            Some(_) => {
                return not(ProbeMiss::NotWritten, "nothing written", Probed::No);
            }
        }
        if r.skipped_person() {
            return not(
                ProbeMiss::Person,
                "a person keyed before the write; nothing written",
                Probed::Again,
            );
        }
        if r.skipped_changed() {
            return not(
                ProbeMiss::NotWritten,
                "the screen moved before the write; nothing written",
                Probed::Again,
            );
        }
        if r.skipped() {
            return not(
                ProbeMiss::NotWritten,
                "the guard matched no row of the judged screen; nothing written",
                Probed::No,
            );
        }
        // Written: from here a panel of the harness's may come.
        self.probe_owes(seq);
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let now = self.screen()?;
        let reader = self.reader(&now.rows);
        let held = self.holds_probe_draft(&now) && reader.prompt(&now.rows).is_none();
        let (true, Some(generation), Some(human), Some(caret)) = (
            held,
            now.generation.clone(),
            now.human_seq,
            caret_row(reader, &now.rows),
        ) else {
            return left(
                ProbeMiss::NotWritten,
                "the composer does not show it as written",
            );
        };
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let person = format!("if-human={human}");
        let (r, args) = self.fenced_key(&guard, "enter", &generation, Some(&person))?;
        let stamp = r.seq().unwrap_or(now.seq);
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(ProbeTyped::Entered { at: stamp, human }),
            None if r.skipped_person() => left(ProbeMiss::Person, "a person keyed before Enter"),
            None => left(ProbeMiss::NotWritten, "the screen moved before Enter"),
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                left(ProbeMiss::NotWritten, "the session is halted")
            }
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why, .. }) => {
                left(ProbeMiss::NotWritten, &one_line(&why))
            }
            Some(_) => left(ProbeMiss::NotWritten, "its Enter was not written"),
        }
    }

    /// WHETHER THE PROBE'S OWN `/usage` STANDS TYPED AND UNSENT on `screen`:
    /// a panel is owed — this loop wrote the command, or the ledger says a
    /// loop before it did and none has seen its panel gone — and the
    /// composer holds exactly the command as typed text
    /// ([`Self::holds_probe_draft`]). With nothing owed a `/usage` in the
    /// composer is a person's draft, as any is.
    fn probe_left_stands(&self, screen: &Screen) -> bool {
        self.probe != UsageProbe::Idle && self.holds_probe_draft(screen)
    }

    /// THE PROBE'S OWN `/usage`, LEFT TYPED ([`Self::probe_type`]), SENT —
    /// where the turn-end policy, reading it as the draft it is, sends the
    /// draft in an act's place ([`TurnEndAction::Submit`]: past a person's
    /// grace since the text last changed, at a point that has an act).
    /// [`Self::turn_end_execute`] hands that Enter here, and it is the
    /// PROBE'S, not the act's: fenced as the probe's Enter is (`key
    /// [if-human=<n>] if-gen=<the judged read> if=<its caret row> enter`),
    /// journaled under the probe's rule, and BOOKED NOWHERE in the policy —
    /// no [`TurnEndState::acted`], no `CONTINUED`, no `typed` row under the
    /// act's rule. Booked as the act, it would spend one of the hour's
    /// continuations, and the worker — which does no work for a `/usage` —
    /// would read as a short turn not taken, so its real continuation would
    /// wait out [`TurnEndTiming::take_within`] and a step of the back-off.
    ///
    /// The panel the Enter opens is the harness's and owed
    /// ([`Self::probe_owes`], from the text): the next look closes it with
    /// its one guarded Escape and samples nothing
    /// ([`Self::usage_panel_look`]), and the point is then decided again
    /// ([`Self::probe_closed`]) — on an empty composer, where its own act is
    /// typed. An Enter the server wrote that did NOT take (the text still
    /// stands, the screen as it was) is found at the re-decision set here,
    /// [`TurnEndTiming::take_within`] on — the wait a typed act is given —
    /// and written again there: never a text left standing with no look to
    /// come.
    ///
    /// An Enter the server skipped (a person keyed, the screen moved) leaves
    /// the text and decides the point again shortly, as does a hold (parked
    /// on first) and a busy server (backed off from first); one refused
    /// outright, or a read with no generation to fence on, presses nothing.
    /// None of them raises anything: the text stands, a person's Enter sends
    /// it as well, and the panel is closed whoever sent it.
    fn probe_left_enter(&mut self, point: &Turn, review: &mut dyn Review) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let skipped = |why: &str, review: &mut dyn Review| {
            review.note(&format!(
                "SKIPPED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} stays typed: {why}"
            ));
        };
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            skipped("the judged read carries no fence for its Enter", review);
            return Ok(());
        };
        let guard = super::super::policy::row_guard(&rows[caret]);
        let person = point.screen.human_seq.map(|n| format!("if-human={n}"));
        let (r, args) = self.fenced_key(&guard, "enter", &generation, person.as_deref())?;
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => {
                review.note(&format!(
                    "TYPED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} left typed, its \
                     Enter written now: no act of the policy's, and the panel it opens is closed \
                     unsampled"
                ));
                // An Enter that did not take leaves the text standing, and
                // no look says so: the point is decided again after the wait
                // a typed act is given to be taken. One that took moves the
                // screen first, and the close sets its own.
                let due = self.fx.mono() + self.turn_end.timing.take_within;
                self.set_due(due, "the accounts probe's text was sent");
            }
            None if r.skipped_person() || r.skipped_changed() => {
                skipped("a person keyed or the screen moved before Enter", review);
                self.redecide_soon("the accounts probe's text was not sent");
            }
            None => skipped("its caret row matched no row of the judged screen", review),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.write_refused(true, &why, seq, review)?,
            Some(Pressing::Refused { why }) => self.write_refused(false, &why, seq, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => skipped(&one_line(&why), review),
            Some(_) => {}
        }
        Ok(())
    }

    /// THE ONE ENTER A DIALOG'S STATE ASKS FOR ([`TurnEndAction::Confirm`]:
    /// Claude Code's `Login successful. Press Enter to continue…`), fenced as
    /// the probe's Enter is — `key if-human=<n> if-gen=<the judged read>
    /// if=<its row> enter` — journaled `TYPED … rule=<rule> enter`, booked in
    /// the ledger as typed, and told to the policy
    /// ([`TurnEndState::acted`], which spends nothing of the hour's and awaits
    /// nothing: the dialog's leaving is the answer). A press the server
    /// skipped (a person keyed, the screen moved) decides the point again
    /// shortly; a read with no generation or person count to fence on presses nothing.
    fn confirm_dialog(
        &mut self,
        point: &Turn,
        action: &TurnEndAction,
        rule: &'static str,
        row: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let skipped = |why: &str, review: &mut dyn Review| {
            review.note(&format!("SKIPPED seq={seq} rule={rule} enter: {why}"));
        };
        let Some(generation) = point.screen.generation.clone() else {
            skipped("the judged read carries no fence for its Enter", review);
            return Ok(());
        };
        let Some(human_seq) = point.screen.human_seq else {
            skipped(
                "the judged read carries no person count for its Enter",
                review,
            );
            return Ok(());
        };
        let guard = super::super::policy::row_guard(row);
        let person = format!("if-human={human_seq}");
        let (r, args) = self.fenced_key(&guard, "enter", &generation, Some(&person))?;
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => {
                let reread = self.turn_end_reread(&point.screen, opts);
                self.turn_end.acted(action, &reread, self.fx.mono());
                review.say(&format!(
                    "TYPED seq={seq} rule={rule} enter at {}",
                    clip(row)
                ))?;
                self.ledger_row(rule, approvals::Outcome::Typed, "enter", row.trim(), seq);
            }
            None if r.skipped_person() || r.skipped_changed() => {
                skipped("a person keyed or the screen moved before Enter", review);
                self.redecide_soon("the dialog's Enter was not pressed");
            }
            None => skipped("its row matched no row of the judged screen", review),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.write_refused(true, &why, seq, review)?,
            Some(Pressing::Refused { why }) => self.write_refused(false, &why, seq, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => skipped(&one_line(&why), review),
            Some(_) => {}
        }
        Ok(())
    }

    /// ONE GUARDED ESCAPE AT THE PANEL ON `grid`, and what came of it: `key
    /// if-human=<the read's person count> if=<the panel's header row> esc`
    /// ([`Self::guarded_key`]; no `if-gen=`: a panel that refreshes moves
    /// the generation), then the header row's leaving waited for
    /// ([`ProbeWaits::close`]) and the grid read again.
    ///
    /// * The panel gone and the composer back: closed
    ///   ([`Self::probe_closed`]). Gone with no composer drawn yet: left
    ///   owed, for the next look to settle.
    /// * Still open: a failed try — the next one a pause of the survey's
    ///   ladder away ([`SURVEY_RETRY`]: 5 s first, far outside the vendor's
    ///   800 ms double-Escape window, the last pause for ever), `WAITING`
    ///   journaled, and at the [`PROBE_STUCK_AT`]-th the host told once
    ///   ([`IdleHost::inform`]) while the tries go on.
    /// * Skipped because no row matched: judged by the same read — the
    ///   panel left first (closed), or it stands under a row the guard does
    ///   not match, which is a try that failed. Skipped for a person's key:
    ///   no try spent. Turned away (`ERR halted`, `ERR busy`, `ERR rate`):
    ///   parked or backed off, no try spent. The next look reads again.
    /// * A read with no person count: NOTHING is pressed — never an
    ///   unfenced Escape — and it counts as a try that failed, so a person
    ///   is told.
    ///
    /// UNDER AUTOPILOT OFF (`off`) this is the panel's ONE PLAIN CLOSE
    /// ([`Self::usage_panel_look`]), and the park follows it whatever came
    /// of it, so nothing here promises or waits for another: a try that
    /// failed is journaled `WAITING … owed until the word turns` (its
    /// ladder is Cruise's and Land's, the host told nothing), and a refusal
    /// — `ERR halted` included — is journaled alone (`HALTED`/`REFUSED seq=<n>
    /// <why>: … owed until the word turns`), never parked on
    /// ([`Self::refused`]: a hold's wait is unbounded, with the loop's
    /// badges and claim up) or backed off from. `ERR busy input-unread`
    /// still suspects a stall, for the look after the word turns.
    ///
    /// The caller has checked what the server cannot: that no busy signal
    /// shows, that no person is within their grace, that a panel taken up
    /// from the ledger has stood one grace.
    pub(super) fn panel_escape(
        &mut self,
        grid: &Screen,
        panel: aterm_phase::SettingsPanel,
        off: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = grid.seq;
        let guard = panel.header_guard(&grid.rows);
        if !guard.is_empty() && grid.human_seq.is_some() {
            match self.guarded_key(seq, &guard, "esc", grid.human_seq)? {
                Dismiss::Pressed { .. } => {
                    self.wait_row_gone(&guard, seq, self.setup.probe_waits.close)?;
                }
                // No row matched at the press: the panel left first — or it
                // stands and its row is not the one read, which a read says.
                Dismiss::Skipped => {}
                Dismiss::Yielded => return Ok(()),
                Dismiss::Refused { halted, why } if off => {
                    if why.contains("ERR busy input-unread") {
                        self.stall_suspect = true;
                    }
                    review.note(&format!(
                        "{} seq={seq} {}: the {USAGE_COMMAND} panel is owed until the word turns \
                         (autopilot off: no wait, no second Escape)",
                        if halted { "HALTED" } else { "REFUSED" },
                        clip(&one_line(&why))
                    ));
                    return Ok(());
                }
                Dismiss::Refused { halted, why } => {
                    return self.refused(halted, &why, seq, review);
                }
            }
            let after = self.grid()?;
            if aterm_phase::settings_panel(&after.rows).is_none() {
                self.composer_settles(&after, review);
                return Ok(());
            }
        }
        let (own, since, failed, told) = match self.probe {
            UsageProbe::Idle => return Ok(()),
            UsageProbe::Owed { own, since } => (own, since, 0, false),
            UsageProbe::Closing {
                own,
                since,
                tries,
                told,
                ..
            } => (own, since, tries, told),
        };
        let tries = failed.saturating_add(1);
        let pause = crate::supervise::ladder::Ladder(&self.setup.survey_retry)
            .step(usize::try_from(tries - 1).unwrap_or(usize::MAX));
        let stuck = !off && tries >= PROBE_STUCK_AT && !told;
        self.probe = UsageProbe::Closing {
            own,
            since,
            tries,
            at: self.fx.mono() + pause,
            told: told || stuck,
        };
        let next = if off {
            "owed until the word turns (autopilot off: no Escape again)".to_string()
        } else {
            format!("Escape again in {} s", pause.as_secs())
        };
        review.note(&format!(
            "WAITING seq={seq} the {USAGE_COMMAND} panel is open ({tries} tries): {next}"
        ));
        if stuck && let Some(host) = opts.idle_host.as_ref() {
            host.inform(PANEL_STUCK);
        }
        Ok(())
    }

    /// THE PANEL OWED ITS CLOSE, at the top of every look
    /// ([`Session::look`], before anything of the look is decided; the
    /// spec's §3.1). Nothing while none is owed. `true`: a panel was handled
    /// — waited on, or its Escape pressed — and the loop looks again, so no
    /// decision is ever made on a panel screen.
    ///
    /// * The turn shows the composer: no panel stands in its place. Closed
    ///   ([`Self::composer_settles`]) — unless the composer still holds the
    ///   probe's own command unsubmitted, which may yet be sent: owed still,
    ///   and the look goes on as any other.
    /// * Else one whole read. No Settings panel on it: closed, and the loop
    ///   reads again (the turn was handed over without waiting for the
    ///   screen to hold still, [`Session::await_turn_from`]) unless it is a
    ///   box, which is judged as read.
    /// * A panel. In order, each a wait that ends the moment the header
    ///   row leaves: a stall of the worker's is held, never pressed into; a
    ///   busy signal — no Escape while one shows, and the vendor retires
    ///   the panel at the turn's end (READ, not measured); a loop that only
    ///   watches behind another supervisor's claim — the panel is the
    ///   holder's to close, and an Escape of this loop's would be refused
    ///   here ([`Session::claim_refuses`]); a panel taken up
    ///   from the ledger stands one person's grace in this loop's sight
    ///   first; a person within their grace on a fresh `status` (`HELD`,
    ///   said once a hold); a failed Escape's pause not yet over. Then the
    ///   guarded Escape ([`Self::panel_escape`]).
    ///
    /// What is NOT closed: a Settings panel on a session whose loop wrote no
    /// `/usage` and whose ledger holds no unanswered row of one — a
    /// person's own. The residual the spec states: a person who opens
    /// `/usage` right after a loop died mid-probe has it closed after a
    /// grace with nobody typing.
    ///
    /// UNDER AUTOPILOT OFF (`off`: the word the loop last read, which its
    /// park is decided by — this look runs ahead of the park) the panel gets
    /// ONE PLAIN CLOSE and nothing more: the composer back (closed, as
    /// above), or its one guarded Escape where every gate above is open.
    /// A panel taken up from the ledger inside its grace has that grace run
    /// first, as under Cruise — the same bounded wait on its header row,
    /// nothing pressed, `true` — so the park that follows is not reached
    /// with the grace still running and the panel never looked at again
    /// (the grace is `human_grace_s`, 120 s by default, and starts as the
    /// loop takes the panel up). Whatever else would wait, retry or escalate
    /// answers `false` instead, so the look goes on to the park and the
    /// panel stays owed until the word turns: a stall held or suspected —
    /// never [`Self::stall_look`], whose hold mails a `kind=ask`, journals
    /// `FROZEN` and may restart the worker — a retry pending
    /// (`UsageProbe::Closing`: an Escape already failed; the ladder that
    /// tells the host the panel is stuck is Cruise's and Land's), a busy
    /// signal, another supervisor's claim, a person's hold, and a stall the
    /// fresh `status` reports. The one Escape answers `false` too, whatever
    /// came of it ([`Self::panel_escape`] under `off`): one refused — a hold
    /// included, which is journaled and never parked on — is not tried
    /// again under Off.
    pub(super) fn usage_panel_look(
        &mut self,
        turn: &Turn,
        state: &mut Looking,
        deadline: Instant,
        off: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let (own, since) = match self.probe {
            UsageProbe::Idle => return Ok(false),
            UsageProbe::Owed { own, since } | UsageProbe::Closing { own, since, .. } => {
                (own, since)
            }
        };
        let seq = turn.screen.seq;
        if self.composer_settles(&turn.screen, review) {
            return Ok(false);
        }
        let grid = self.grid()?;
        let Some(panel) = aterm_phase::settings_panel(&grid.rows) else {
            self.probe_closed(seq, review);
            return Ok(!off && turn.phase != Phase::Prompt);
        };
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        // Off: what would wait, retry or escalate parks at once — but a
        // panel taken up from the ledger has its grace run below, as under
        // Cruise (it presses nothing), so its one close still comes.
        if off
            && (self.stalled.is_some()
                || self.stall_suspect
                || matches!(self.probe, UsageProbe::Closing { .. })
                || self.reader(&grid.rows).phase(&grid.rows) == Phase::Busy
                || self.claim.watching_behind().is_some()
                || self.probe_hold.is_some_and(|until| until > self.fx.mono()))
        {
            return Ok(false);
        }
        let guard = panel.header_guard(&grid.rows);
        // One wait on the header row leaving, to `until` at most: a step of
        // the loop's and the budget bound it.
        // On the door's clocks, cloned: the step is read between the loop's
        // own calls on `self`.
        let fx = self.fx.clone();
        let step = |until: Option<Instant>| {
            let now = fx.mono();
            until
                .map_or(WAIT_STEP, |t| t.saturating_duration_since(now))
                .min(WAIT_STEP)
                .min(deadline.saturating_duration_since(now))
        };
        if self.stall_look(seq, state, review)?
            || self.reader(&grid.rows).phase(&grid.rows) == Phase::Busy
            || self.claim.watching_behind().is_some()
        {
            self.wait_row_gone(&guard, grid.seq, step(None))?;
            return Ok(true);
        }
        if !own && self.fx.since(since) < grace {
            self.wait_row_gone(&guard, grid.seq, step(since.checked_add(grace)))?;
            return Ok(true);
        }
        if let Some(until) = self.probe_hold.filter(|until| *until > self.fx.mono()) {
            self.wait_row_gone(&guard, grid.seq, step(Some(until)))?;
            return Ok(true);
        }
        self.program = self.foreground_program()?;
        let hold = self.hands_off_until(opts);
        // Off: a person's hold, or a stall the fresh `status` reports
        // ([`Self::note_status`]), is no plain close either.
        if off && (hold.is_some() || self.stall_suspect) {
            return Ok(false);
        }
        self.probe_hold = hold;
        if let Some(until) = self.probe_hold {
            review.note(&self.held_line(seq, until, "the usage panel's Escape"));
            self.wait_row_gone(&guard, grid.seq, step(Some(until)))?;
            return Ok(true);
        }
        if let UsageProbe::Closing { at, .. } = self.probe
            && self.fx.mono() < at
        {
            self.wait_row_gone(&guard, grid.seq, step(Some(at)))?;
            return Ok(true);
        }
        self.panel_escape(&grid, panel, off, opts, review)?;
        Ok(!off)
    }

    /// D. THE POINT DECIDED AGAIN AFTER A PROBE ([`Probed::Done`]; the
    /// spec's P21): one ordinary read — which settles a panel still owed
    /// when it shows the composer — and, where it shows the SAME point (its
    /// review key), [`Self::turn_end_now`]: the host is asked afresh, so a
    /// sample that shows the account low moves the session here, and
    /// otherwise the point's own act is decided and carried out as it would
    /// have been. `turn_end_now` never probes: the turn it decides on was
    /// not read live ([`Session::turn_live`] is this look's, and the step is
    /// not called from there).
    ///
    /// A screen with NO COMPOSER while the panel is still owed is no point
    /// at all: a close not yet painted (a panel closed under the read,
    /// caught half erased), or a panel that came late. Nothing is carried
    /// out on it, and the point is decided again shortly on a fresh read;
    /// the panel stays owed until a look sees it gone, and one that stands
    /// gets its guarded Escape there ([`Self::usage_panel_look`]).
    ///
    /// A screen WITH its composer that shows ANOTHER point is the probe
    /// having left something on it — rows in the transcript, a turn
    /// started: nothing is carried out, this loop probes no more, and the
    /// next look takes the screen as the new point it is.
    pub(super) fn after_probe(
        &mut self,
        point: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let screen = self.screen()?;
        let turn = self.point_of(screen);
        if !self.composer_settles(&turn.screen, review) && self.probe != UsageProbe::Idle {
            self.redecide_soon("the usage panel is closing");
            return Ok(());
        }
        if self.review_key(&turn) != self.review_key(point) {
            self.probe_off = true;
            review.note(&format!(
                "SKIPPED seq={} rule={RULE_USAGE_PROBE} the point changed across the probe: \
                 nothing is carried out on it, and this loop probes no more",
                turn.screen.seq
            ));
            return Ok(());
        }
        self.turn_end_now(point, opts, allow, review)
    }
}

/// What a SHADOW replay (`tape::shadow`) decides a turn end again on, under
/// its candidate policy: the reading the recorded decision's last pass was
/// made on (its autopilot word and the person's last key as the recorded
/// passes read them — a candidate's own passes would make no read the
/// recording did not), the session's question word that pass applied, or
/// that the host withheld the point (nothing is decided under any policy).
#[derive(Debug, Default)]
pub(super) struct ShadowPoint {
    reading: Option<TurnEndReading>,
    prose: Option<super::super::policy::approval::QuestionPriority>,
    withheld: bool,
}

impl ShadowPoint {
    /// The candidate's decision, under `policy`, on the fold `state` at `now`.
    fn decide(
        self,
        state: &super::super::policy::turn_end::TurnEndState,
        mut policy: SupervisorConfig,
        now: Instant,
    ) -> TurnEndAction {
        let Some(reading) = self.reading.filter(|_| !self.withheld) else {
            return TurnEndAction::Nothing;
        };
        if let Some(word) = self.prose {
            policy.answer_questions = true;
            policy.question_priority = word;
        }
        decide_turn_end(state, &reading, &policy, now)
    }
}

/// A turn-end decision as its mark on a harness tape writes it
/// ([`Session::decide_to_act`]): its `Debug`, but for the one `Instant` an
/// action can hold — a wait's `until`, written by `at` as an offset from the
/// tape's origin, so a tape never holds this process's raw clock and two
/// runs that decided the same thing write the same mark.
pub(super) fn turn_end_outcome(action: &TurnEndAction, at: &dyn Fn(Instant) -> String) -> String {
    match action {
        TurnEndAction::WaitUntil { until, why, wait } => format!(
            "WaitUntil {{ until: {}, why: {why:?}, wait: {wait:?} }}",
            at(*until)
        ),
        TurnEndAction::Restart {
            why,
            rule_id,
            otherwise,
        } => format!(
            "Restart {{ why: {why:?}, rule_id: {rule_id:?}, otherwise: {} }}",
            turn_end_outcome(otherwise, at)
        ),
        TurnEndAction::Nothing
        | TurnEndAction::Type { .. }
        | TurnEndAction::Accept { .. }
        | TurnEndAction::TypeCommand { .. }
        | TurnEndAction::Submit { .. }
        | TurnEndAction::Confirm { .. }
        | TurnEndAction::Escalate { .. } => format!("{action:?}"),
    }
}

/// Whether the table's `action` at a point that asks a question in prose is
/// one the session's priority word can change ([`Session::decide_to_act`]):
/// an answer (the word picks its priority), or the escalation
/// `answer_questions = false` makes ([`ANSWERS_OFF`]). A wait, a cap's or a
/// wall's escalation is the table's alone, and reads no word.
fn session_word_can_decide(action: &TurnEndAction) -> bool {
    match action {
        TurnEndAction::Escalate { reason } => reason.ends_with(ANSWERS_OFF),
        action => action
            .rule_id()
            .is_some_and(super::super::policy::turn_end::is_answer_rule),
    }
}

/// The words a wall's reset is placed and remembered under
/// ([`Session::wall_reset_at`]): its kind and its `reset=` text. The same
/// words are the same reset; an act's answer is placed once under each
/// ([`Session::wall_answered`]).
fn reset_key(kind: aterm_phase::WallKind, text: Option<&str>) -> String {
    format!("{}|{}", kind.name(), text.unwrap_or("-"))
}

/// Whether the loop's host withholds the session's continuations now
/// ([`IdleHost::withholds_turn_end`]).
fn host_withholds(opts: &SuperviseOpts) -> bool {
    opts.idle_host
        .as_ref()
        .is_some_and(|h| h.withholds_turn_end())
}

/// THE FALLBACK'S RELAUNCH AS A LOOP JOURNALED IT ([`journaled_relaunch`]),
/// in unix seconds.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct JournaledRelaunch {
    /// When the last relaunch made was first asked: its run's first host
    /// step, or its own line where none was journaled.
    pub(super) made: Option<i64>,
    /// When the run of a relaunch asked and not made yet was first asked.
    pub(super) waiting: Option<i64>,
}

/// THE FALLBACK'S RELAUNCH, read from the journal beside `sid`'s ledger
/// (`journal`): each ask's host step (`HOST seq=<n> restart:<word> step=…`,
/// `word` the fallback's restart's, [`Restart::word`]), the relaunch's own
/// `RESTARTED … rule=model-fallback@v1` line, and the acts that end a run.
/// A build through 0.106.0 wrote no time of the run's first ask in its rows
/// (`made=`, [`approvals::model_switch_reason`]; nor the row of a run that
/// waits, [`approvals::relaunch_waits_reason`]): read from the row's own
/// time, written once the host said the relaunch was adopted (90 s and more
/// after the line was typed), a bucket's notice that answered a person's
/// prompt in between read as the relaunched process's redraw, and a loop
/// that followed typed `continue@v1` into the standing bucket two days
/// before its reset (ATERM-KI-1016-1, follow-5; the shape of the verifier
/// of 5b72b1613, M7-4).
pub(super) fn journaled_relaunch(
    journal: &str,
    sid: Option<&str>,
    word: &str,
) -> JournaledRelaunch {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let step = format!(" restart:{word} step=");
    let mut run: Option<i64> = None;
    let mut made = None;
    for rec in journal
        .lines()
        .filter_map(|text| crate::supervise::journal::JournalRecord::from_json(text).ok())
        .filter(|rec| rec.sid.as_deref() == sid)
    {
        match rec.kind.as_str() {
            "restarted" if rec.phase == crate::supervise::policy::turn_end::RULE_MODEL_FALLBACK => {
                made = Some(run.take().unwrap_or(rec.t));
            }
            "continued" | "typed" => run = None,
            _ if rec.summary.starts_with("HOST ") && rec.summary.contains(&step) => {
                run.get_or_insert(rec.t);
            }
            _ => {}
        }
    }
    JournaledRelaunch {
        made: made.map(|ms: i64| ms.div_euclid(1000)),
        waiting: run.map(|ms| ms.div_euclid(1000)),
    }
}
