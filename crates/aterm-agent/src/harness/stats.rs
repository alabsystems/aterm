// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE HARNESS DID, folded from the ledgers it already writes, for
//! Settings ▸ Harness (`docs/DESIGN-harness-autopilot-2026-10-15.md` §4, item
//! 4): boxes approved, questions answered, continuations, Land's wrap-ups,
//! resumes after walls, relaunches, upgrades, escalations and the time they
//! waited on a person — over a window that starts at `since_ms` (unix ms;
//! the page asks for today and for 7 days).
//!
//! Three files are read, and each fact is counted from ONE of them, so a
//! session whose ledger and journal both record a press counts it once:
//!
//! * the session's approval ledger, `<state>/drive/<sid>.jsonl`
//!   ([`crate::supervise::approvals`]): every press, refusal and typed act;
//! * its journal beside it, `<state>/drive/<sid>.journal.jsonl`
//!   ([`crate::supervise::approvals::journal_beside`],
//!   [`crate::supervise::journal`]), which the window's host keeps for every
//!   supervised session: the escalations, their ends, and the question
//!   dialogs the loop answered;
//! * the live upgrade's ledger, `<harness state>/upgrade/ledger.jsonl`
//!   ([`super::upgrade_drive::ledger_path`]): the relaunches and upgrades.
//!
//! | File | Row | Counts as |
//! |---|---|---|
//! | ledger | `approved` under a box's rule | [`HarnessStats::approved`]; also [`HarnessStats::unproven`] where its reason says `unproven:` (full power pressed what the safe rules could not prove) |
//! | ledger | `approved` under a question dialog's rule (`answer-recommended@v1`, `answer-long-term@v1`, `answer-short-term@v1`; `land-answer@v1` once Land answers dialogs, which is deferred — design §2.2 — so no build writes it there yet) | nothing but [`HarnessStats::by_rule`]: one key of a dialog (a tab, a toggle, a submit); the journal's `chose` counts the dialog |
//! | ledger | `declined` | [`HarnessStats::declined`] |
//! | ledger | `typed` `continue@v1`, `continue-suggestion@v1`, `continue-done-check@v1`, `continue-open-items@v1` | [`HarnessStats::continued`] |
//! | ledger | `typed` `answer@v1`, `prose-long-term@v1`, `prose-short-term@v1`, `land-answer@v1` | [`HarnessStats::answered`] (a question asked in prose) |
//! | ledger | `typed` `land@v1`, `land-nudge@v1` | [`HarnessStats::landed`] |
//! | ledger | `typed` `usage-resume@v1`, `api-retry@v1`, `api-back@v1`, `api-cutoff@v1`, and `context-compact@v1`'s continuation (not its `/compact`) | [`HarnessStats::resumed`] |
//! | ledger | `typed` under any other rule (`auth-login@v1`, `consent-accept@v1`, `model-fallback@v1`, `model-restore@v1`, `account-rotate@v1`, `memory-restart@v1`, `model-wind-down@v1`, `harness-turn@v1`, `usage-probe@v1`, `context-compact@v1`'s `/compact`, …) | nothing but [`HarnessStats::by_rule`] |
//! | ledger | `waiting` `wall-wait@v1`, to the session's `wall-wait-end@v1` or its next `typed` row | [`HarnessStats::limit_wait_ms`] |
//! | ledger | `escalated`, `skipped`, `deferred`, any other `waiting` | nothing but [`HarnessStats::rows`] |
//! | journal | `chose` | [`HarnessStats::answered`] (a question dialog the loop's own keys answered, said once it is done) |
//! | journal | `escalated` whose badge went up (`attention=OK`) — not `attention=skipped: …` ([`ATTENTION_SKIPPED`]: another supervisor answers the session, or a stall withheld the ask), and not a badge set again ([`MAIL_RETRY`], [`MAIL_ADOPTED`], [`MAIL_UNPARKED`]) | [`HarnessStats::escalated`], and a person's wait opens (one open goes on) |
//! | journal | `escalated` of a badge set again: a limit episode's whose retry hit the wall again, or that was adopted from a previous loop (one an Off park took down too); the turn-end policy's an Off park took down, set again as it ends | no escalation; a person's wait opens if none is open |
//! | journal | `ADOPTED` (kind `other`): a badge a previous loop left, taken over at a loop's start | a person's wait opens if none is open |
//! | journal | `cleared` of a badge (`box`, `turn-end`, `goal`, `memory`, `stale`, `hidden`, a limit's `attention=`; a leading `seq=-` passed over) | the wait closes — [`HarnessStats::human_wait_ms`] — only on `attention=OK` or the explicit `attention=none`; refused or unknown results leave it open |
//! | journal | `FROZEN` (kind `other`, [`FROZEN`]): a stall's hold, which takes the loop's point badge down with no `CLEARED` line | the wait closes only on `attention=OK` or `attention=none`; kept, skipped, refused and unknown results leave it open |
//! | journal | `exit`, `timeout`: a loop's end | the session's open wait ends, counting nothing |
//! | journal | every other kind | nothing but [`HarnessStats::rows`] (its `approved`, `declined`, `typed`, `continued` and `unproven` lines repeat the ledger's rows) |
//! | upgrade | `relaunched` on the build it ran | [`HarnessStats::relaunched`] (a crash relaunched, a restart in place) |
//! | upgrade | `relaunched` onto another build | [`HarnessStats::upgraded`] |
//! | upgrade | every other step | nothing but [`HarnessStats::rows`] |
//!
//! ANSWERS, CONTINUATIONS AND RESUMES are three different acts. An ANSWER
//! replies to a question the worker asked (a dialog's option, or a prose
//! question's `answer_text`). A CONTINUATION is typed at an ordinary turn end
//! to keep the work moving (Cruise). A RESUME carries the work on past a WALL
//! the worker did not choose — a usage window's reset, an API error, a reply
//! the connection cut, a full context — and is the wall's own act; the walls
//! that need a login, a consent or a relaunch are counted where their act is
//! (the relaunch) or only by rule. Land's instruction and its nudges are
//! neither: they bring the work down, and count as [`HarnessStats::landed`].
//!
//! THE WAITS are pairs, matched per session in the files' order (time
//! order). A PERSON'S WAIT runs while a badge of the loop's stands: the
//! first `ESCALATED` line that raised one (or an `ADOPTED` line that took one
//! over) opens it, and the `CLEARED` of a badge that took the loop's entry
//! down closes it (its `seq=` is the read it closed on, never the escalated
//! point's, so the two are not matched by seq number) — or the `FROZEN` line
//! of a stall's hold, which takes the entry down and says so on that line
//! alone. An Off park takes the loop's badges down as it starts, each with
//! its `CLEARED` line, so a park is never counted as a person's wait (a
//! badge set again as it ends says so — [`MAIL_RETRY`], [`MAIL_ADOPTED`],
//! [`MAIL_UNPARKED`] — and opens a wait again with no second escalation),
//! and the loop's last clear of the turn-end policy's badge has its
//! `CLEARED` line before the loop's end. The server keeps ONE attention entry per
//! owner, so badges that overlap — the goal's note
//! raised while the turn-end policy's badge stands, say — are one entry, its
//! text the latest escalation's: the first of them to end leaves the other's
//! standing and says so (its reply is `kept`), and the wait goes on to the
//! clear that takes the entry down. (Counting escalations against clears
//! instead never comes back to zero where a badge is raised again before its
//! clear — the turn-end policy's, at a point the worker has not left — and
//! carries one wait to the loop's end.) The loop's end (`EXIT …`, `TIMEOUT`)
//! ends a wait still open with nothing counted, as the end of a file does:
//! its end is in no journal of this loop's — a badge handed to the next loop
//! is `ADOPTED` there and opens a wait anew, and the next escalation opens
//! its own rather than inheriting a dead loop's. A LIMIT'S WAIT: a
//! `wall-wait@v1` row opens it and its end — the `wall-wait-end@v1` row, or
//! any act the policy typed since, as
//! [`crate::supervise::approvals::open_wall`] reads it — closes it. A second
//! opening while one stands keeps the first. Only the part of a wait inside
//! the window counts; a wait still open at the end of its file counts nothing
//! (it has no end yet).
//!
//! THE WINDOW. A row counts when its own time is at or after `since_ms`. A
//! session ledger and its journal are cut back to their newest
//! [`crate::supervise::approvals::KEEP_ROWS`] rows past their bound (the
//! upgrade ledger to [`super::cli::DISK_LEDGER_KEEP_ROWS`]), and each file is
//! read through [`super::cli::tail_lines`]' ring of at most
//! [`super::cli::READ_MAX_ROWS`] rows: a file holding at least as many rows
//! as its cut keeps, whose oldest row is newer than `since_ms`, may have lost
//! the window's start, and [`HarnessStats::cut`] says so. A file shorter than
//! that was never cut: a short session that began inside the window is whole.
//!
//! The folds are pure. [`stats_under`] is the one reader: it creates nothing
//! (a missing directory is an empty result), reads only the files named
//! above, and leaves a ledger no row has been appended to since the window
//! opened unread. The window's host calls it off the event loop.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::cli::{DISK_LEDGER_KEEP_ROWS, READ_MAX_ROWS, tail_lines};
use crate::supervise::approvals::{self, Outcome, RULE_WALL_WAIT, RULE_WALL_WAIT_END};
use crate::supervise::journal::{
    ADOPTED, ATTENTION_SKIPPED, CLEARED_BADGES, FROZEN, JournalRecord, MAIL_ADOPTED, MAIL_RETRY,
    MAIL_UNPARKED,
};
use crate::supervise::policy::approval::QuestionPriority;
use crate::supervise::policy::question::is_question_rule;
// Land's rules (design §2.2) are the policy's own consts, as every other rule
// here is: a rule id is never reworded (a ledger an earlier build wrote reads
// the same), so the fold counts Land's rows whichever build wrote them.
use crate::supervise::policy::turn_end::{
    RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, RULE_COMPACT, RULE_CONTINUE, RULE_DONE_CHECK,
    RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_NUDGE, RULE_LIMIT_RESUME, RULE_OPEN_ITEMS,
    RULE_SUGGESTION,
};

/// How much older than the window's start a file's last write may be before
/// [`stats_under`] leaves it unread: a row's time is taken just before it is
/// appended, so a file written in the window was modified in it; the slack
/// absorbs a clock that stepped between the two.
const MTIME_SLACK_MS: i64 = 60_000;

/// What the harness did over one window, folded from its ledgers (the module
/// docs' table says which row counts where). Built empty by
/// [`HarnessStats::new`], filled by [`fold_ledger_line`], [`fold_journal`]
/// and [`fold_upgrade_line`], or read whole by [`stats_under`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessStats {
    /// The window's start, unix ms: rows at or after it count.
    pub since_ms: i64,
    /// The sessions with at least one row in the window: the ledgers' and
    /// journals' `sid`, the upgrade ledger's `tab`.
    pub sessions: u32,
    /// Boxes the harness pressed yes (or its focused answer) on: permission
    /// boxes, the trust dialog, plan approvals, the limit dialog's wait.
    pub approved: u64,
    /// Of [`Self::approved`], the presses full power made that the safe rules
    /// could not prove.
    pub unproven: u64,
    /// Boxes refused with a reason the worker reads.
    pub declined: u64,
    /// Questions answered: dialogs the loop's keys answered, and questions
    /// asked in prose answered with the policy's text.
    pub answered: u64,
    /// Continuations typed at an ordinary turn end (Cruise).
    pub continued: u64,
    /// Land's instruction and its nudges (`land@v1`, `land-nudge@v1`).
    pub landed: u64,
    /// The work carried on past a wall: a usage reset, an API error, a reply
    /// cut off, a full context.
    pub resumed: u64,
    /// Points handed to a person (a box, a question, a limit, a turn end).
    pub escalated: u64,
    /// How long escalations waited on a person, inside the window: while a
    /// badge of the loop's stood, from the `ESCALATED` (or `ADOPTED`) that
    /// raised it to the `CLEARED` that took it down (module docs, THE WAITS).
    pub human_wait_ms: u64,
    /// How long the harness waited at a limit's wall, inside the window:
    /// each `wall-wait@v1` row to its end.
    pub limit_wait_ms: u64,
    /// Agents started again on the build they ran: a crash relaunched, a
    /// restart in place (the memory banner, a model's fallback and return,
    /// an account move).
    pub relaunched: u64,
    /// Agents relaunched onto another build: the live upgrade.
    pub upgraded: u64,
    /// Every act of the approval ledger (`approved`, `declined`, `typed`)
    /// in the window, by its full rule id: the breakdown behind the counts
    /// above, a rule this build does not name included.
    pub by_rule: BTreeMap<String, u64>,
    /// The rows of all three files whose time is in the window.
    pub rows: u64,
    /// Lines that are no row: not JSON, no time (a line cut short by a crash,
    /// a hand edit). Never a panic, and never placed in or out of the window.
    pub bad: u64,
    /// The window may be partial: a file's cut (or this read's ring) may have
    /// dropped rows the window starts with (module docs, THE WINDOW).
    pub cut: bool,
    /// The folds' memory of what is still open.
    open: Open,
}

/// What a fold carries from one row to the next: the waits still open and
/// the sessions already counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Open {
    /// Session → when the first badge of the loop's that still stands went
    /// up, or was adopted (unix ms).
    human: BTreeMap<String, i64>,
    /// Session → when the wall it waits at was first written (unix ms).
    limit: BTreeMap<String, i64>,
    /// The sessions [`HarnessStats::sessions`] counted.
    seen: BTreeSet<String>,
}

impl HarnessStats {
    /// Nothing folded yet, over the window that starts at `since_ms`.
    #[must_use]
    pub fn new(since_ms: i64) -> Self {
        Self {
            since_ms,
            ..Self::default()
        }
    }

    /// No row of any file fell in the window: the page says the harness has
    /// done nothing there yet (a line that is no row says nothing).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// One row of `sid`'s in the window.
    fn row_of(&mut self, sid: &str) {
        self.rows = self.rows.saturating_add(1);
        if !self.open.seen.contains(sid) {
            self.open.seen.insert(sid.to_string());
            self.sessions = u32::try_from(self.open.seen.len()).unwrap_or(u32::MAX);
        }
    }

    /// One act under `rule`.
    fn act(&mut self, rule: &str) {
        let n = self.by_rule.entry(rule.to_string()).or_insert(0);
        *n = n.saturating_add(1);
    }

    /// A badge of the loop's went up on `sid` at `at` (unix ms), or was
    /// adopted: its person's wait opens, unless one is open already.
    fn badge_up(&mut self, sid: &str, at: i64) {
        self.open.human.entry(sid.to_string()).or_insert(at);
    }

    /// The loop's entry on `sid` was taken down at `at` (unix ms): the
    /// person's wait, if one is open, closes, and its part inside the window
    /// counts.
    fn badge_down(&mut self, sid: &str, at: i64) {
        if let Some(start) = self.open.human.remove(sid) {
            self.human_wait_ms =
                self.human_wait_ms
                    .saturating_add(span_ms(start, at, self.since_ms));
        }
    }
}

/// `n` plus one, saturating: a count never wraps.
fn bump(n: &mut u64) {
    *n = n.saturating_add(1);
}

/// The part of the wait from `start` to `end` (unix ms) that lies at or
/// after `since_ms`, in ms; none for a wait that ended before the window or
/// a clock that ran back.
fn span_ms(start: i64, end: i64, since_ms: i64) -> u64 {
    u64::try_from(end.saturating_sub(start.max(since_ms))).unwrap_or(0)
}

/// Whether an `approved` row under `rule` is one key of a question dialog
/// (its tab, a toggle, its submit) rather than a box's press. Land's answer
/// is among them for the day Land answers dialogs: deferred (design §2.2),
/// so today a dialog under Land is keyed under the question rules, and
/// [`RULE_LAND_ANSWER`] is only ever typed in prose.
fn is_dialog_key(rule: &str) -> bool {
    is_question_rule(rule) || rule == RULE_LAND_ANSWER
}

/// Whether a `typed` row under `rule` answers a question asked in prose:
/// `answer_text` under any priority ([`QuestionPriority::prose_rule_id`]),
/// or Land's answer.
fn is_prose_answer(rule: &str) -> bool {
    rule == RULE_LAND_ANSWER
        || QuestionPriority::ALL
            .into_iter()
            .any(|p| p.prose_rule_id() == rule)
}

/// The counter a `typed` row under `rule`, typing `command`, adds to (module
/// docs' table); `None`: only [`HarnessStats::by_rule`].
fn typed_counter<'a>(acc: &'a mut HarnessStats, rule: &str, command: &str) -> Option<&'a mut u64> {
    if matches!(
        rule,
        RULE_CONTINUE | RULE_SUGGESTION | RULE_DONE_CHECK | RULE_OPEN_ITEMS
    ) {
        Some(&mut acc.continued)
    } else if is_prose_answer(rule) {
        Some(&mut acc.answered)
    } else if matches!(rule, RULE_LAND | RULE_LAND_NUDGE) {
        Some(&mut acc.landed)
    } else if matches!(
        rule,
        RULE_LIMIT_RESUME | RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF
    ) || (rule == RULE_COMPACT && !command.starts_with('/'))
    {
        Some(&mut acc.resumed)
    } else {
        None
    }
}

/// Fold one line of a session's approval ledger
/// ([`crate::supervise::approvals::Row::to_json`]'s shape) into `acc`, over
/// its window ([`HarnessStats::since_ms`]). A line that is no row (not JSON,
/// no `ts`) is [`HarnessStats::bad`]. A row before the window counts nothing,
/// but still opens or closes a limit's wait the window may hold the rest of.
pub fn fold_ledger_line(acc: &mut HarnessStats, line: &str) {
    let Some((v, ts)) = row_and_time(line, "ts") else {
        bump(&mut acc.bad);
        return;
    };
    let text = |k: &str| v.get(k).and_then(Value::as_str);
    let sid = text("sid").unwrap_or_default();
    let rule = text("rule_id").unwrap_or("-");
    let outcome = text("decision").and_then(Outcome::from_word);
    // The limit's wait, as `approvals::open_wall` reads it: opened by the
    // wall's row, closed by its end or by any act typed since.
    match outcome {
        Some(Outcome::Waiting) if rule == RULE_WALL_WAIT => {
            acc.open.limit.entry(sid.to_string()).or_insert(ts);
        }
        Some(Outcome::Waiting) if rule == RULE_WALL_WAIT_END => close_limit(acc, sid, ts),
        Some(Outcome::Typed) => close_limit(acc, sid, ts),
        _ => {}
    }
    if ts < acc.since_ms {
        return;
    }
    acc.row_of(sid);
    match outcome {
        Some(Outcome::Approved) => {
            acc.act(rule);
            if !is_dialog_key(rule) {
                bump(&mut acc.approved);
                if text("reason").is_some_and(|r| r.starts_with("unproven:")) {
                    bump(&mut acc.unproven);
                }
            }
        }
        Some(Outcome::Declined) => {
            acc.act(rule);
            bump(&mut acc.declined);
        }
        Some(Outcome::Typed) => {
            acc.act(rule);
            if let Some(n) = typed_counter(acc, rule, text("command").unwrap_or_default()) {
                bump(n);
            }
        }
        _ => {}
    }
}

/// `sid`'s limit wait, if one is open, closed at `at` (unix ms).
fn close_limit(acc: &mut HarnessStats, sid: &str, at: i64) {
    if let Some(start) = acc.open.limit.remove(sid) {
        acc.limit_wait_ms = acc
            .limit_wait_ms
            .saturating_add(span_ms(start, at, acc.since_ms));
    }
}

/// What an `ESCALATED` line's tail (`attention=<reply> mail=<reply>`) says
/// went up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escalation {
    /// No confirmed badge: another supervisor answers the session, a stall
    /// withheld the ask, or the set was refused or answered no known result.
    /// No escalation or person's badge wait is counted.
    Withheld,
    /// A badge set again, no new ask: a limit episode's whose retry hit the
    /// wall again before the worker answered ([`MAIL_RETRY`]) — or that an
    /// Off park took down, the notice still up after it — or whose badge was
    /// adopted from a previous loop ([`MAIL_ADOPTED`]); the turn-end
    /// policy's an Off park took down, set again as it ends
    /// ([`MAIL_UNPARKED`]). No escalation; the person's wait goes on, or
    /// opens where none is open.
    Again,
    /// A point handed to a person: an escalation, and a person's wait.
    Asked,
}

/// [`Escalation`] of an `ESCALATED` line's tail.
fn escalation_of(summary: &str) -> Escalation {
    if summary.split(' ').next() != Some("attention=OK") {
        return Escalation::Withheld;
    }
    let mail = summary.split_once(" mail=").map_or("", |(_, mail)| mail);
    if [MAIL_RETRY, MAIL_ADOPTED, MAIL_UNPARKED]
        .iter()
        .any(|again| mail.starts_with(again))
    {
        Escalation::Again
    } else {
        Escalation::Asked
    }
}

/// Whether a `CLEARED` line's tail took the loop's badge down: it ended a
/// badge an escalation raised — a box's or a question's, the turn-end
/// policy's, the memory measure's, one a restarted loop found stale or
/// hidden ([`CLEARED_BADGES`]), or a limit episode's (`attention=<reply>
/// <why>`) — and its reply confirms success (`OK`) or no entry (`none`). A
/// refusal, an unknown result, or [`crate::supervise::journal::ATTENTION_KEPT`] changes no open wait.
/// Not the survey's, the usage panel's or an
/// exception's end, which raised no badge. A leading `seq=…` the record
/// could not read as a number (`seq=-`, the hidden clear's: no read was
/// made) is passed over.
fn takes_badge_down(summary: &str) -> bool {
    let rest = match summary.split_once(' ') {
        Some((seq, rest)) if seq.starts_with("seq=") => rest,
        _ => summary,
    };
    let (word, tail) = rest.split_once(' ').unwrap_or((rest, ""));
    let reply = match word.strip_prefix("attention=") {
        Some(reply) => reply,
        None if CLEARED_BADGES.contains(&word) => tail
            .split(' ')
            .next()
            .and_then(|w| w.strip_prefix("attention="))
            .unwrap_or_default(),
        None => return false,
    };
    cleared_reply(reply)
}

/// The writer's successful unset reply, or its explicit report that it
/// holds no entry. Never infer a change from a refusal or an unknown word.
fn cleared_reply(reply: &str) -> bool {
    matches!(reply.strip_suffix(':').unwrap_or(reply), "OK" | "none")
}

/// Whether a journal record is an `ADOPTED` line ([`ADOPTED`]: kind
/// `other`, the line's first word).
fn is_adopted(r: &JournalRecord) -> bool {
    r.kind == "other" && r.line.split(' ').next() == Some(ADOPTED)
}

/// Whether a journal record is a stall's hold that took the loop's badge
/// down: a `FROZEN` line ([`FROZEN`]: kind `other`, the line's first word)
/// whose `attention=<reply>` confirms success or no entry, as for CLEARED.
/// A refused, unknown, kept or skipped result preserves the open wait.
fn frozen_takes_badge_down(r: &JournalRecord) -> bool {
    if r.kind != "other" || r.line.split(' ').next() != Some(FROZEN) {
        return false;
    }
    let Some(at) = r.line.find(" attention=") else {
        return false;
    };
    let field = &r.line[at + 1..];
    if field.starts_with(ATTENTION_SKIPPED) {
        return false;
    }
    let reply = field
        .split(' ')
        .next()
        .and_then(|w| w.strip_prefix("attention="))
        .unwrap_or_default();
    cleared_reply(reply)
}

/// Fold a session journal's records ([`crate::supervise::journal`]) into
/// `acc`, over its window ([`HarnessStats::since_ms`]): the escalations and
/// the person's waits their badges opened, and the question dialogs the loop
/// answered (the module docs' table). A record before the window counts
/// nothing, but still opens or closes a wait the window may hold the rest of.
pub fn fold_journal(acc: &mut HarnessStats, recs: &[JournalRecord]) {
    for r in recs {
        let sid = r.sid.as_deref().unwrap_or_default();
        let inside = r.t >= acc.since_ms;
        match r.kind.as_str() {
            "escalated" => match escalation_of(&r.summary) {
                Escalation::Withheld => {}
                Escalation::Again => acc.badge_up(sid, r.t),
                Escalation::Asked => {
                    if inside {
                        bump(&mut acc.escalated);
                    }
                    acc.badge_up(sid, r.t);
                }
            },
            "cleared" if takes_badge_down(&r.summary) => acc.badge_down(sid, r.t),
            // The loop's end: what its badges did after it is in no journal
            // of its own — the next loop ADOPTS a badge it was handed, and
            // opens the wait anew there.
            "exit" | "timeout" => {
                acc.open.human.remove(sid);
            }
            "other" if is_adopted(r) => acc.badge_up(sid, r.t),
            "other" if frozen_takes_badge_down(r) => acc.badge_down(sid, r.t),
            "chose" if inside => bump(&mut acc.answered),
            _ => {}
        }
        if inside {
            acc.row_of(sid);
        }
    }
}

/// Fold one line of the live upgrade's ledger (`upgrade_drive::ledger`'s
/// shape: `t` in unix SECONDS, `tab`, `from`, `to` as `<version>(<source>)`,
/// `step`) into `acc`, over its window ([`HarnessStats::since_ms`], unix
/// ms). A `relaunched` row is an upgrade where both builds are known and
/// differ, else a relaunch. A line that is no row is [`HarnessStats::bad`].
pub fn fold_upgrade_line(acc: &mut HarnessStats, line: &str) {
    let Some((v, t)) = row_and_time(line, "t") else {
        bump(&mut acc.bad);
        return;
    };
    if t.saturating_mul(1000) < acc.since_ms {
        return;
    }
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
    match text("tab") {
        "" | "-" => bump(&mut acc.rows),
        tab => acc.row_of(tab),
    }
    if text("step") == "relaunched" {
        if moved_build(text("from"), text("to")) {
            bump(&mut acc.upgraded);
        } else {
            bump(&mut acc.relaunched);
        }
    }
}

/// Whether an upgrade row's `to` (`<version>(<source>)`) names a build other
/// than `from`, both known (a version unknown, `-`, is no evidence of a move).
fn moved_build(from: &str, to: &str) -> bool {
    let to = to
        .strip_suffix(')')
        .and_then(|t| t.rsplit_once('('))
        .map_or(to, |(version, _)| version);
    let known = |v: &str| !v.is_empty() && v != "-";
    known(from) && known(to) && from != to
}

/// `line` as a JSON object and its time under `key` (an integer); `None` for
/// a line that is no row.
fn row_and_time(line: &str, key: &str) -> Option<(Value, i64)> {
    let v = aterm_json::from_str::<Value>(line).ok()?;
    let t = v.get(key).and_then(Value::as_i64)?;
    Some((v, t))
}

/// Whether a file of `rows` rows (read through a ring of at most
/// [`READ_MAX_ROWS`]) whose cut keeps `keep`, its oldest row read at
/// `oldest_ms`, may be missing the start of the window at `since_ms`: it is
/// long enough to have been cut, and begins after the window does. A file
/// shorter than its cut keeps was never cut.
fn window_cut(rows: usize, keep: usize, oldest_ms: Option<i64>, since_ms: i64) -> bool {
    rows >= keep.min(READ_MAX_ROWS) && oldest_ms.is_some_and(|t| t > since_ms)
}

/// One per-session file under `<state>/drive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriveFile {
    /// `<sid>.jsonl`, the approval ledger.
    Ledger,
    /// `<sid>.journal.jsonl`, the loop's journal.
    Journal,
}

/// The ledgers and journals under `drive` (`<state>/drive`), in name order;
/// none for a directory that is missing or cannot be read. Everything else
/// there (`<sid>.goal-hold.json`, a cut's `.tmp`, a directory) is passed
/// over, and so is a file no row was appended to since the window opened at
/// `since_ms`.
fn drive_files(drive: &Path, since_ms: i64) -> Vec<(PathBuf, DriveFile)> {
    let Ok(entries) = std::fs::read_dir(drive) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, DriveFile)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            let kind = if name.ends_with(".journal.jsonl") {
                DriveFile::Journal
            } else if name.ends_with(".jsonl") {
                DriveFile::Ledger
            } else {
                return None;
            };
            Some((e.path(), kind))
        })
        .filter(|(path, _)| !untouched_since(path, since_ms))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Whether `path` was last written before the window at `since_ms` opened
/// (less [`MTIME_SLACK_MS`]): none of its rows can fall in the window. A file
/// whose time cannot be read is read.
fn untouched_since(path: &Path, since_ms: i64) -> bool {
    let Some(modified_ms) = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
    else {
        return false;
    };
    modified_ms.saturating_add(MTIME_SLACK_MS) < since_ms
}

/// The live upgrade's ledger under the harness state directory:
/// `<harness state>/upgrade/ledger.jsonl`, where `upgrade_drive::ledger_path`
/// keeps it for a sweep whose state is `harness_state`.
fn upgrade_ledger(harness_state: &Path) -> PathBuf {
    harness_state.join("upgrade").join("ledger.jsonl")
}

/// WHAT THE HARNESS DID since `since_ms` (unix ms), read from the files it
/// keeps: every session ledger and journal under `<state_dir>/drive`, and the
/// live upgrade's ledger under `harness_state` (module docs). Each file is
/// read through [`tail_lines`]' ring of at most [`READ_MAX_ROWS`] rows, never
/// whole. Nothing is created and nothing is written: a missing directory or
/// file is no rows, one that cannot be read is passed over. Blocking file
/// I/O: call it off the event loop.
#[must_use]
pub fn stats_under(state_dir: &Path, harness_state: &Path, since_ms: i64) -> HarnessStats {
    let mut acc = HarnessStats::new(since_ms);
    for (path, kind) in drive_files(&state_dir.join("drive"), since_ms) {
        let Ok(lines) = tail_lines(&path, READ_MAX_ROWS) else {
            continue;
        };
        match kind {
            DriveFile::Ledger => {
                let oldest = lines
                    .iter()
                    .find_map(|l| row_and_time(l, "ts").map(|(_, t)| t));
                acc.cut |= window_cut(lines.len(), approvals::KEEP_ROWS, oldest, since_ms);
                for line in &lines {
                    fold_ledger_line(&mut acc, line);
                }
            }
            DriveFile::Journal => {
                let mut recs = Vec::with_capacity(lines.len());
                for line in &lines {
                    match JournalRecord::from_json(line) {
                        Ok(r) => recs.push(r),
                        Err(_) => bump(&mut acc.bad),
                    }
                }
                let oldest = recs.first().map(|r| r.t);
                acc.cut |= window_cut(lines.len(), approvals::KEEP_ROWS, oldest, since_ms);
                fold_journal(&mut acc, &recs);
            }
        }
    }
    let path = upgrade_ledger(harness_state);
    if !untouched_since(&path, since_ms)
        && let Ok(lines) = tail_lines(&path, READ_MAX_ROWS)
    {
        let oldest = lines
            .iter()
            .find_map(|l| row_and_time(l, "t").map(|(_, t)| t.saturating_mul(1000)));
        acc.cut |= window_cut(lines.len(), DISK_LEDGER_KEEP_ROWS, oldest, since_ms);
        for line in &lines {
            fold_upgrade_line(&mut acc, line);
        }
    }
    acc
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
