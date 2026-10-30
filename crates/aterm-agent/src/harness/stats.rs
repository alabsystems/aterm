// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Harness activity statistics folded from the harness ledgers for Settings ▸ Harness.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::acts::{self, Act, Counter};
use super::cli::{DISK_LEDGER_KEEP_ROWS, READ_MAX_ROWS, tail_lines};
use crate::supervise::approvals::{self, Outcome, RULE_WALL_WAIT, RULE_WALL_WAIT_END};
use crate::supervise::journal::{
    ADOPTED, AGAIN_ADOPTED, AGAIN_RETRY, AGAIN_UNPARKED, ATTENTION_SKIPPED, CLEARED_BADGES, FROZEN,
    JournalRecord,
};
use crate::supervise::policy::approval::{
    RULE_ALLOW_ONCE, RULE_LIMIT_RESET, RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM, RULE_TRUST_ANY,
    SUBJECT_OPTION, WARNED_UNDER_SAFE,
};
use crate::supervise::policy::go_ahead::RULE_GO_AHEAD;
use crate::supervise::policy::question::{CHOSE_FIRST_SAFE, CHOSE_RECOMMENDED, CHOSE_UNMARKED};
use crate::supervise::policy::reset::KeepWhy;
use crate::supervise::policy::turn_end::{
    LAND, LAND_NUDGE, LAND_NUDGE_REMOTE_MAIN_BEFORE, LAND_REMOTE_MAIN_BEFORE, RULE_ANSWER,
    RULE_LAND, RULE_LAND_ANSWER, is_land_rule,
};
use crate::supervise::run::LINE_CHARS;

const MTIME_SLACK_MS: i64 = 60_000;

/// What the harness did over one window, folded from its ledgers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessStats {
    /// The window's start, unix ms: rows at or after it count.
    pub since_ms: i64,
    /// The sessions with at least one row in the window.
    pub sessions: u32,
    /// Boxes the harness pressed yes (or its focused answer) on.
    pub approved: u64,
    /// Of [`Self::approved`], the presses full power made that the safe rules could not prove.
    pub unproven: u64,
    /// Boxes refused with a reason the worker reads.
    pub declined: u64,
    /// Questions answered: dialogs the loop's keys answered, and questions asked in prose answered with the policy's text.
    pub answered: u64,
    /// Continuations typed at an ordinary turn end (Cruise).
    pub continued: u64,
    /// Land's instruction and its nudges typed (`land@v1`, `land-nudge@v1`).
    pub land_prompts: u64,
    /// Land's landings: the journal's `DONE … rule=land@v1` notes, one per landing at its first note's time, never a restatement of it.
    pub landed: u64,
    /// The work carried on past a wall.
    pub resumed: u64,
    /// Points handed to a person (a box, a question, a limit, a turn end).
    pub escalated: u64,
    /// The worker's NEEDS YOU sets put on the person's screen.
    pub needs_told: u64,
    /// How long escalations waited on a person, inside the window.
    pub human_wait_ms: u64,
    /// How long the harness waited at a limit's wall, inside the window.
    pub limit_wait_ms: u64,
    /// Agents started again on the build they ran.
    pub relaunched: u64,
    /// Agents relaunched onto another build: the live upgrade.
    pub upgraded: u64,
    /// Every act of the approval ledger (`approved`, `declined`, `typed`) in the window, by its full rule id.
    pub by_rule: BTreeMap<String, u64>,
    /// The fact lines the occupancy door typed in the window (`fact-<kind>@v1` `typed`), by their kind.
    pub facts_told: BTreeMap<String, u64>,
    /// The fact lines it would have typed under `prompt`, booked under `observe` (`fact-would@v1`), by the kind of the rule each names.
    pub facts_would: BTreeMap<String, u64>,
    /// The rows of all three files whose time is in the window.
    pub rows: u64,
    /// Lines that are no row: not JSON, no time (a line cut short by a crash, a hand edit).
    pub bad: u64,
    /// The window may be partial.
    pub cut: bool,
    /// WHAT AGENTS THAT SUPERVISE THEMSELVES DID (a proven self-claim's acts, codex-alab's), by
    /// the name each holder acts under ([`acts::claimant_name`]): never in the counts above, which
    /// are aterm's own harness's, nor in [`Self::rows`] and [`Self::sessions`].
    pub self_claimed: BTreeMap<String, SelfClaimed>,
    /// Each session's share of the counts above, by `sid`.
    pub by_session: Option<BTreeMap<String, Tally>>,
    /// The choices the harness made in the person's name inside the window.
    pub decided: Option<Decided>,
    /// The folds' memory of what is still open.
    open: Open,
}

/// One session's share of a window ([`HarnessStats::by_session`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    /// [`HarnessStats::approved`]'s share.
    pub approved: u64,
    /// [`HarnessStats::unproven`]'s share.
    pub unproven: u64,
    /// [`HarnessStats::declined`]'s share.
    pub declined: u64,
    /// [`HarnessStats::answered`]'s share.
    pub answered: u64,
    /// [`HarnessStats::continued`]'s share.
    pub continued: u64,
    /// [`HarnessStats::land_prompts`]' share.
    pub land_prompts: u64,
    /// [`HarnessStats::landed`]' share.
    pub landed: u64,
    /// [`HarnessStats::resumed`]'s share.
    pub resumed: u64,
    /// [`HarnessStats::escalated`]'s share.
    pub escalated: u64,
    /// [`HarnessStats::needs_told`]' share.
    pub needs_told: u64,
    /// [`HarnessStats::human_wait_ms`]' share.
    pub human_wait_ms: u64,
    /// [`HarnessStats::limit_wait_ms`]' share.
    pub limit_wait_ms: u64,
    /// [`HarnessStats::relaunched`]'s share (the upgrade ledger's `tab`).
    pub relaunched: u64,
    /// [`HarnessStats::upgraded`]'s share (the upgrade ledger's `tab`).
    pub upgraded: u64,
    /// When the newest escalation counted in the window was handed to a person (unix ms).
    pub last_escalated_ms: Option<i64>,
    /// [`HarnessStats::self_claimed`]'s share: what each self-claim's holder did in the session.
    pub self_claimed: BTreeMap<String, SelfClaimed>,
    /// The session's own ledger or journal may have lost the window's start.
    pub cut: bool,
}

/// What one agent that supervises itself did over a window, from the acts its proven claim
/// recorded ([`approvals::SelfClaimRow`]), each counted under the act aterm's own act of its
/// rule is, one count per act ([`acts::self_claim_sort`]): the words of aterm's own counts,
/// booked apart from them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfClaimed {
    /// Boxes it approved (`approve-all@v1`, the plan box's yes).
    pub approved: u64,
    /// Questions it answered.
    pub answered: u64,
    /// Turns it kept going.
    pub continued: u64,
    /// Work it carried past a wall.
    pub resumed: u64,
    /// Land's instruction and nudges it typed.
    pub land_prompts: u64,
    /// Its landings (`landed@v1`).
    pub landed: u64,
    /// Its re-execs onto a re-linked build (`upgrade@v1`).
    pub upgraded: u64,
    /// Every act it recorded in the window, those that count nothing included (a point left to a
    /// person, a mail turn's receipt).
    pub acts: u64,
}

impl SelfClaimed {
    fn take(&mut self, act: &str) {
        bump(&mut self.acts);
        let slot = match acts::self_claim_sort(act).counter {
            Some(Counter::Approved) => &mut self.approved,
            Some(Counter::Answered) => &mut self.answered,
            Some(Counter::Continued) => &mut self.continued,
            Some(Counter::Resumed) => &mut self.resumed,
            Some(Counter::LandPrompts) => &mut self.land_prompts,
            Some(Counter::Declined) => return,
            None if act == acts::RULE_LANDED => &mut self.landed,
            None if act == acts::RULE_UPGRADE => &mut self.upgraded,
            None => return,
        };
        bump(slot);
    }
}

/// A count a row adds to: the window's total and, per session, its share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Count {
    /// The counter THE ONE SORT names ([`acts::sort`]).
    Act(Counter),
    /// A full-power press the safe rules could not prove.
    Unproven,
    /// A point handed to a person.
    Escalated,
    /// A NEEDS YOU set put on the person's screen.
    NeedsTold,
    /// Land's landing, at its first note.
    Landed,
    /// An agent started again on the build it ran.
    Relaunched,
    /// An agent relaunched onto another build.
    Upgraded,
}

/// A wait a pair of rows measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// A person's: a badge of the loop's stood.
    Human,
    /// A limit's: the harness waited at a wall.
    Limit,
}

trait Counts {
    fn slot(&mut self, count: Count) -> &mut u64;
    fn wait_slot(&mut self, wait: Wait) -> &mut u64;
}

impl Counts for HarnessStats {
    fn slot(&mut self, count: Count) -> &mut u64 {
        match count {
            Count::Act(counter) => match counter {
                Counter::Approved => &mut self.approved,
                Counter::Declined => &mut self.declined,
                Counter::Answered => &mut self.answered,
                Counter::Continued => &mut self.continued,
                Counter::Resumed => &mut self.resumed,
                Counter::LandPrompts => &mut self.land_prompts,
            },
            Count::Unproven => &mut self.unproven,
            Count::Escalated => &mut self.escalated,
            Count::NeedsTold => &mut self.needs_told,
            Count::Landed => &mut self.landed,
            Count::Relaunched => &mut self.relaunched,
            Count::Upgraded => &mut self.upgraded,
        }
    }

    fn wait_slot(&mut self, wait: Wait) -> &mut u64 {
        match wait {
            Wait::Human => &mut self.human_wait_ms,
            Wait::Limit => &mut self.limit_wait_ms,
        }
    }
}

impl Counts for Tally {
    fn slot(&mut self, count: Count) -> &mut u64 {
        match count {
            Count::Act(counter) => match counter {
                Counter::Approved => &mut self.approved,
                Counter::Declined => &mut self.declined,
                Counter::Answered => &mut self.answered,
                Counter::Continued => &mut self.continued,
                Counter::Resumed => &mut self.resumed,
                Counter::LandPrompts => &mut self.land_prompts,
            },
            Count::Unproven => &mut self.unproven,
            Count::Escalated => &mut self.escalated,
            Count::NeedsTold => &mut self.needs_told,
            Count::Landed => &mut self.landed,
            Count::Relaunched => &mut self.relaunched,
            Count::Upgraded => &mut self.upgraded,
        }
    }

    fn wait_slot(&mut self, wait: Wait) -> &mut u64 {
        match wait {
            Wait::Human => &mut self.human_wait_ms,
            Wait::Limit => &mut self.limit_wait_ms,
        }
    }
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
    /// The sessions whose last landing note has no land act journaled
    /// after it: their next note restates that landing.
    landed_since_act: BTreeSet<String>,
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

    /// [`Self::new`], keeping each session's share of the counts ([`Self::by_session`]).
    #[must_use]
    pub fn per_session(since_ms: i64) -> Self {
        Self {
            by_session: Some(BTreeMap::new()),
            decided: Some(Decided::default()),
            ..Self::new(since_ms)
        }
    }

    /// The pass is over: the last session's join is settled into [`Self::decided`].
    pub fn end_reading(&mut self) {
        if let Some(decided) = self.decided.as_mut() {
            decided.settle_session();
            decided.keep_newest();
        }
    }

    /// No row of aterm's own harness fell in the window (a self-claim's rows are in
    /// [`Self::self_claimed`], never in [`Self::rows`]): the page says aterm's harness has done
    /// nothing there yet.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows == 0
    }

    fn tally(&mut self, sid: &str) -> Option<&mut Tally> {
        let shares = self.by_session.as_mut()?;
        if !shares.contains_key(sid) {
            shares.insert(sid.to_string(), Tally::default());
        }
        shares.get_mut(sid)
    }

    fn count(&mut self, sid: &str, count: Count) {
        bump(self.slot(count));
        if let Some(tally) = self.tally(sid) {
            bump(tally.slot(count));
        }
    }

    fn waited(&mut self, sid: &str, wait: Wait, ms: u64) {
        let total = self.wait_slot(wait);
        *total = total.saturating_add(ms);
        if let Some(tally) = self.tally(sid) {
            let share = tally.wait_slot(wait);
            *share = share.saturating_add(ms);
        }
    }

    fn escalated_at(&mut self, sid: &str, at: i64) {
        self.count(sid, Count::Escalated);
        if let Some(tally) = self.tally(sid) {
            tally.last_escalated_ms = Some(tally.last_escalated_ms.map_or(at, |last| last.max(at)));
        }
    }

    fn cut_of(&mut self, sid: &str) {
        self.cut = true;
        if let Some(tally) = self.tally(sid) {
            tally.cut = true;
        }
    }

    fn row_of(&mut self, sid: &str) {
        self.rows = self.rows.saturating_add(1);
        if !self.open.seen.contains(sid) {
            self.open.seen.insert(sid.to_string());
            self.sessions = u32::try_from(self.open.seen.len()).unwrap_or(u32::MAX);
        }
    }

    fn act(&mut self, rule: &str) {
        let n = self.by_rule.entry(rule.to_string()).or_insert(0);
        *n = n.saturating_add(1);
    }

    fn land(&mut self, sid: &str, line: LandLine, inside: bool) {
        match line {
            LandLine::Act => {
                self.open.landed_since_act.remove(sid);
            }
            LandLine::Note | LandLine::Restated => {
                if self.open.landed_since_act.contains(sid) {
                    return;
                }
                if line == LandLine::Note && inside {
                    self.count(sid, Count::Landed);
                }
                self.open.landed_since_act.insert(sid.to_string());
            }
        }
    }

    fn badge_up(&mut self, sid: &str, at: i64) {
        self.open.human.entry(sid.to_string()).or_insert(at);
    }

    fn badge_down(&mut self, sid: &str, at: i64) {
        if let Some(start) = self.open.human.remove(sid) {
            self.waited(sid, Wait::Human, span_ms(start, at, self.since_ms));
        }
    }
}

const fn bump(n: &mut u64) {
    *n = n.saturating_add(1);
}

fn span_ms(start: i64, end: i64, since_ms: i64) -> u64 {
    u64::try_from(end.saturating_sub(start.max(since_ms))).unwrap_or(0)
}

/// How many decisions a window keeps ([`Decided::rows`]): the newest.
pub const DECIDED_KEEP: usize = 64;

/// How many characters of the agent's words a decision keeps, `…` after a cut.
pub const DECIDED_CHARS: usize = 96;

/// How close (ms) an unproven press of a command comes after the last press of that command in that session to be counted in its row (`×N`) rather than a row of its own.
pub const BURST_MS: i64 = 60_000;

/// What kind of choice the harness made in the person's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionKind {
    /// A question dialog its keys answered.
    Chose,
    /// A question asked in prose, answered with the policy's text.
    Answered,
    /// A box full power answered that no safe rule proved: its yes, or another of its options.
    Unproven,
    /// A box refused, with the reason the worker was given.
    Declined,
    /// A limit reset kept unspent where the owner lets the harness spend one.
    KeptReset,
}

impl DecisionKind {
    const fn tag(self) -> u8 {
        match self {
            Self::Chose => 1,
            Self::Answered => 2,
            Self::Unproven => 3,
            Self::Declined => 4,
            Self::KeptReset => 5,
        }
    }
}

/// One choice the harness made in the person's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// When it was made, unix ms.
    pub t_ms: i64,
    /// The session its rows carry.
    pub sid: String,
    /// What kind of choice it was.
    pub kind: DecisionKind,
    /// [`DecisionKind::Chose`]: the dialog's questions, ` / ` between them.
    pub asked: Box<str>,
    /// [`DecisionKind::Answered`].
    pub said: bool,
    /// [`DecisionKind::Chose`]: the options chosen, `; ` between them.
    pub did: Box<str>,
    /// [`DecisionKind::Chose`]: why that option.
    pub why: Option<Box<str>>,
    /// How many presses the row holds.
    pub times: u32,
}

impl Decision {
    /// What tells this decision from every other one of the day, the same at every reading while its rows stand.
    #[must_use]
    pub fn id(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let bytes = self
            .sid
            .bytes()
            .chain([0, self.kind.tag()])
            .chain(self.t_ms.to_le_bytes())
            .chain(self.asked.bytes());
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }
}

/// The window's decisions ([`HarnessStats::decided`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decided {
    /// The newest [`DECIDED_KEEP`] the window held, oldest first, once the reading has ended
    /// ([`HarnessStats::end_reading`]).
    pub rows: Vec<Decision>,
    /// How many the window held, kept or not (a run of presses is one).
    pub total: u64,
    /// The join's memory of the session whose files are being folded.
    join: Join,
}

/// What the join keeps of the session being folded until its next row
/// belongs to another: its decisions, its dialogs waiting for their keys,
/// its review points and the loop runs they were read in, and its runs of
/// unproven presses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Join {
    /// The session, once a row of it has come.
    sid: Option<String>,
    /// Its decisions so far.
    rows: Vec<Decision>,
    /// Its `CHOSE` rows, in time order.
    dialogs: Vec<Dialog>,
    /// Its review points a prose answer may follow, in the journal's order.
    points: Vec<Point>,
    /// When each of its loop runs before the current one ended (unix ms),
    /// oldest first: the current run is `run_ends.len()` ([`Join::note_run`]).
    run_ends: Vec<i64>,
    /// The newest `EVENT`'s `seq` in the current run.
    last_event_seq: Option<u64>,
    /// Its runs of unproven presses: what the presses share → the last press
    /// (unix ms) and the run's row in [`Self::rows`].
    runs: BTreeMap<RunKey, (i64, usize)>,
}

type RunKey = (String, Box<str>, &'static str);

/// A review point a prose answer may have been decided at: an `EVENT` the
/// worker's turn ended on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Point {
    /// When it was journaled, unix ms.
    t: i64,
    /// Its read's `seq`.
    seq: u64,
    /// The loop run it was read in ([`Join::run_ends`]).
    run: usize,
    /// It was a question's (`EVENT question`), not a turn's end the policy
    /// read a question in (`idle`, `turn`, `idle-no-report`).
    question: bool,
    /// What the agent said there.
    words: Box<str>,
}

/// A `CHOSE` row waiting for the answer key its ledger wrote before it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dialog {
    /// When the `CHOSE` was told, unix ms.
    told_ms: i64,
    /// Its decision in [`Join::rows`].
    at: usize,
    /// Its `policy=` word.
    policy: Box<str>,
    /// What it told: `<question> → <choice>`, ` | ` between the tabs (cut
    /// at [`LINE_CHARS`] where the line was cut).
    said: String,
    /// The answer keys its session wrote at or before it and after the
    /// `CHOSE` before it, oldest first, the newest [`DIALOG_KEYS`] kept:
    /// when, its command (`<question> → <choice>`) and its reason.
    keys: Vec<AnswerKey>,
}

type AnswerKey = (i64, String, String);

const DIALOG_KEYS: usize = 16;

struct LedgerAct<'a> {
    ts: i64,
    outcome: Outcome,
    sorted: acts::Sorted,
    command: &'a str,
    reason: &'a str,
    box_seq: Option<u64>,
    rule: &'a str,
}

impl Join {
    fn note_run(&mut self, r: &JournalRecord) {
        match r.kind.as_str() {
            "exit" | "timeout" => self.end_run(r.t),
            "other" if is_adopted(r) => self.end_run(r.t),
            "event" => {
                if let Some(seq) = r.seq {
                    if self.last_event_seq.is_some_and(|last| seq < last) {
                        self.end_run(r.t);
                    }
                    self.last_event_seq = Some(seq);
                }
            }
            _ => {}
        }
    }

    fn end_run(&mut self, at: i64) {
        self.run_ends.push(at);
        self.last_event_seq = None;
    }

    fn point_at(&self, ts: i64, seq: u64) -> Option<&Point> {
        self.points.iter().rev().find(|point| {
            point.t <= ts
                && point.seq <= seq
                && self
                    .run_ends
                    .get(point.run)
                    .is_none_or(|ended| ts <= *ended)
        })
    }
}

impl Decided {
    fn session(&mut self, sid: &str) {
        if sid.is_empty() && self.join.sid.is_some() {
            return;
        }
        if self.join.sid.as_deref() != Some(sid) {
            self.settle_session();
            self.join.sid = Some(sid.to_string());
        }
    }

    fn push(&mut self, decision: Decision) -> usize {
        self.total = self.total.saturating_add(1);
        self.join.rows.push(decision);
        self.join.rows.len() - 1
    }

    fn note_journal(&mut self, sid: &str, r: &JournalRecord) {
        self.session(sid);
        self.join.note_run(r);
        match r.kind.as_str() {
            "chose" => {
                let (policy, said) = r
                    .summary
                    .strip_prefix("policy=")
                    .and_then(|rest| rest.split_once(' '))
                    .unwrap_or(("", r.summary.as_str()));
                let at = self.push(Decision {
                    t_ms: r.t,
                    sid: sid.to_string(),
                    kind: DecisionKind::Chose,
                    asked: Box::from(""),
                    said: false,
                    did: Box::from(""),
                    why: None,
                    times: 1,
                });
                self.join.dialogs.push(Dialog {
                    told_ms: r.t,
                    at,
                    policy: Box::from(policy),
                    said: said.to_string(),
                    keys: Vec::new(),
                });
            }
            "event"
                if matches!(
                    r.phase.as_str(),
                    "question" | "idle" | "turn" | "idle-no-report"
                ) =>
            {
                if let Some(seq) = r.seq {
                    let words = r.summary.trim();
                    let words = if words == "-" { "" } else { words };
                    let words = words.trim_start_matches(['⏺', '●', '•']).trim_start();
                    let run = self.join.run_ends.len();
                    self.join.points.push(Point {
                        t: r.t,
                        seq,
                        run,
                        question: r.phase == "question",
                        words: agent_words(words),
                    });
                }
            }
            _ => {}
        }
    }

    fn note_ledger(&mut self, sid: &str, row: &LedgerAct<'_>) {
        let LedgerAct {
            ts,
            outcome,
            sorted,
            command,
            reason,
            box_seq,
            rule,
        } = *row;
        match (outcome, sorted.act, sorted.counter) {
            (Outcome::Approved, Some(Act::Answered), None) => {
                if !command.contains(ARROW) {
                    return;
                }
                self.session(sid);
                if let Some(dialog) = self
                    .join
                    .dialogs
                    .iter_mut()
                    .find(|dialog| dialog.told_ms >= ts)
                {
                    if dialog.keys.len() >= DIALOG_KEYS {
                        dialog.keys.remove(0);
                    }
                    dialog
                        .keys
                        .push((ts, command.to_string(), reason.to_string()));
                }
            }
            (Outcome::Approved, _, Some(Counter::Approved)) if reason.starts_with(UNPROVEN) => {
                self.session(sid);
                let (subject, option) = box_option(rule, command);
                let (whole, head) = command_head(subject);
                let option = option.map_or_else(|| Box::from(""), agent_words);
                let switch = unproven_switch(reason);
                let key: RunKey = (whole, option.clone(), switch);
                if let Some((last, at)) = self.join.runs.get_mut(&key)
                    && ts >= *last
                    && ts.saturating_sub(*last) <= BURST_MS
                    && let Some(run) = self.join.rows.get_mut(*at)
                {
                    run.times = run.times.saturating_add(1);
                    *last = ts;
                    return;
                }
                let at = self.push(Decision {
                    t_ms: ts,
                    sid: sid.to_string(),
                    kind: DecisionKind::Unproven,
                    asked: head,
                    said: false,
                    did: option,
                    why: Some(Box::from(switch)),
                    times: 1,
                });
                self.join.runs.insert(key, (ts, at));
            }
            (Outcome::Declined, ..) if rule == RULE_LIMIT_RESET => {
                self.session(sid);
                self.push(Decision {
                    t_ms: ts,
                    sid: sid.to_string(),
                    kind: DecisionKind::KeptReset,
                    asked: Box::from(""),
                    said: false,
                    did: Box::from(""),
                    why: KeepWhy::plain_words(reason.trim()).map(|why| agent_words(&why)),
                    times: 1,
                });
            }
            (Outcome::Declined, ..) => {
                self.session(sid);
                let reason = reason.trim();
                let why = (!reason.is_empty() && reason != "-").then(|| agent_words(reason));
                self.push(Decision {
                    t_ms: ts,
                    sid: sid.to_string(),
                    kind: DecisionKind::Declined,
                    asked: command_head(command).1,
                    said: false,
                    did: Box::from(""),
                    why,
                    times: 1,
                });
            }
            (Outcome::Typed, _, Some(Counter::Answered)) => {
                self.session(sid);
                let (said, asked) = box_seq
                    .and_then(|seq| self.join.point_at(ts, seq))
                    .map_or((false, Box::from("")), |point| {
                        (!point.question, point.words.clone())
                    });
                self.push(Decision {
                    t_ms: ts,
                    sid: sid.to_string(),
                    kind: DecisionKind::Answered,
                    asked,
                    said,
                    did: prose_answer_words(rule, command),
                    why: None,
                    times: 1,
                });
            }
            _ => {}
        }
    }

    fn settle_session(&mut self) {
        let mut join = std::mem::take(&mut self.join);
        for dialog in &join.dialogs {
            if let Some(decision) = join.rows.get_mut(dialog.at) {
                dialog.settle(decision);
            }
        }
        self.rows.append(&mut join.rows);
        if self.rows.len() >= 2 * DECIDED_KEEP {
            self.keep_newest();
        }
    }

    fn keep_newest(&mut self) {
        self.rows.sort_by_key(|decision| decision.t_ms);
        let over = self.rows.len().saturating_sub(DECIDED_KEEP);
        self.rows.drain(..over);
    }
}

const ARROW: &str = " \u{2192} ";

const TABS: &str = " | ";

const UNPROVEN: &str = "unproven:";

impl Dialog {
    fn settle(&self, decision: &mut Decision) {
        let cut = self.said.chars().count() >= LINE_CHARS;
        let (tabs, keys) = told_tabs(&self.said, &self.keys, cut);
        if tabs.is_empty() {
            decision.asked = agent_words(&self.said);
        } else {
            let questions: Vec<&str> = tabs.iter().map(|(question, _)| question.trim()).collect();
            let choices: Vec<&str> = tabs
                .iter()
                .map(|(_, choice)| {
                    let choice = choice.trim();
                    choice
                        .strip_suffix("(Recommended)")
                        .map_or(choice, str::trim_end)
                })
                .collect();
            decision.asked = agent_words(&questions.join(" / "));
            decision.did = agent_words(&choices.join("; "));
        }
        let reasons: Vec<&str> = keys.iter().map(|(_, _, reason)| reason.as_str()).collect();
        decision.why = Some(Box::from(chose_why(&reasons, &self.policy)));
        if let Some((at, ..)) = keys.iter().max_by_key(|(at, ..)| *at) {
            decision.t_ms = *at;
        }
    }
}

fn told_tabs<'a>(
    said: &'a str,
    keys: &'a [AnswerKey],
    cut: bool,
) -> (Vec<(String, String)>, Vec<&'a AnswerKey>) {
    let commands: Vec<String> = keys
        .iter()
        .map(|(_, command, _)| {
            command
                .trim()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect()
        })
        .collect();
    let mut taken = vec![false; keys.len()];
    let (mut tabs, mut told) = (Vec::new(), Vec::new());
    let mut rest = said.trim_start();
    while !rest.trim().is_empty() {
        let hit = commands
            .iter()
            .enumerate()
            .rev()
            .filter(|(i, command)| !taken[*i] && !command.is_empty())
            .find_map(|(i, command)| match rest.strip_prefix(command.as_str()) {
                Some("") => Some((i, "")),
                Some(after) => after.strip_prefix(TABS).map(|next| (i, next)),
                None => (cut && command.starts_with(rest)).then_some((i, "")),
            });
        match hit {
            Some((i, next)) => {
                taken[i] = true;
                let key = &keys[i];
                if let Some((question, choice)) = key.1.rsplit_once(ARROW) {
                    tabs.push((question.to_string(), choice.to_string()));
                    told.push(key);
                }
                rest = next.trim_start();
            }
            None => {
                let (tab, next) = match rest.split_once(TABS) {
                    Some((tab, next)) => (tab, Some(next)),
                    None => (rest, None),
                };
                if let Some((question, choice)) = tab.rsplit_once(ARROW) {
                    let choice = if cut && next.is_none() {
                        format!("{}\u{2026}", choice.trim_end())
                    } else {
                        choice.to_string()
                    };
                    tabs.push((question.to_string(), choice));
                }
                rest = next.unwrap_or_default().trim_start();
            }
        }
    }
    if cut && let Some(newest) = taken.iter().rposition(|was| *was) {
        for key in &keys[newest + 1..] {
            if let Some((question, choice)) = key.1.rsplit_once(ARROW) {
                tabs.push((question.to_string(), choice.to_string()));
                told.push(key);
            }
        }
    }
    (tabs, told)
}

fn chose_why(reasons: &[&str], policy: &str) -> String {
    let (mut marked, mut unmarked, mut passed) = (false, false, false);
    for reason in reasons {
        let reason = reason.strip_prefix(UNPROVEN).unwrap_or(reason).trim_start();
        if reason.starts_with(CHOSE_RECOMMENDED) {
            marked = true;
        } else if reason.starts_with(CHOSE_UNMARKED) {
            unmarked = true;
        } else if reason.starts_with(CHOSE_FIRST_SAFE) {
            (unmarked, passed) = (true, true);
        } else {
            (marked, unmarked, passed) = (false, false, false);
            break;
        }
    }
    let first = if passed {
        "the first that names no destructive act"
    } else {
        "the first"
    };
    match (marked, unmarked) {
        (true, false) if reasons.len() > 1 => "the options marked Recommended".to_string(),
        (true, false) => "the option marked Recommended".to_string(),
        (false, true) => format!("no option was marked Recommended, so it took {first}"),
        (true, true) => format!("the option marked Recommended where one was, else {first}"),
        (false, false) => {
            let word = match policy {
                "long-term" => "Long-term",
                "short-term" => "Short-term",
                _ => "Recommended",
            };
            format!("Question priority is {word}")
        }
    }
}

fn unproven_switch(reason: &str) -> &'static str {
    if reason.contains(WARNED_UNDER_SAFE) {
        "Approve permission boxes: Safe, with the rm warning"
    } else {
        "Approve permission boxes: All"
    }
}

fn box_option<'a>(rule: &str, command: &'a str) -> (&'a str, Option<&'a str>) {
    if matches!(rule, RULE_ALLOW_ONCE | RULE_TRUST_ANY) {
        return (command, None);
    }
    match command.rsplit_once(SUBJECT_OPTION) {
        Some((subject, option)) => (subject, Some(option.trim()).filter(|o| !o.is_empty())),
        None => (command, None),
    }
}

/// What a decision row says aterm answered with the owner's standing go-ahead ([`RULE_GO_AHEAD`]).
pub const GO_AHEAD_DID: &str =
    "the owner's standing go-ahead: build, test and land its own work, nothing outward";

fn prose_answer_words(rule: &str, command: &str) -> Box<str> {
    match rule {
        RULE_LAND_ANSWER => Box::from(
            "the land answer: land all of the work on the remote main soonest, start nothing new",
        ),
        RULE_GO_AHEAD => Box::from(GO_AHEAD_DID),
        RULE_PROSE_LONG_TERM => {
            Box::from("the long-term answer: decide for long-term correctness and total value")
        }
        RULE_PROSE_SHORT_TERM => {
            Box::from("the short-term answer: get the current deliverable done soonest")
        }
        RULE_ANSWER => {
            const OPEN: &str = "the answer text \u{201c}";
            const CLOSE: &str = "\u{201d}";
            let room = DECIDED_CHARS - OPEN.chars().count() - CLOSE.chars().count();
            format!("{OPEN}{}{CLOSE}", agent_words_in(command, room)).into_boxed_str()
        }
        _ => agent_words(command),
    }
}

fn command_head(command: &str) -> (String, Box<str>) {
    let mut lines = command
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    let (head, body) = match heredoc_opened(first) {
        Some(end) => first.split_at(end),
        None => (first, ""),
    };
    let head: String = head.chars().filter(|c| !is_box_drawing(*c)).collect();
    let head = head.trim();
    let shown = if head.is_empty() {
        Box::from("")
    } else if !body.trim().is_empty() || lines.next().is_some() {
        agent_words(&format!("{head} \u{2026}"))
    } else {
        agent_words(head)
    };
    (head.to_string(), shown)
}

fn is_box_drawing(c: char) -> bool {
    ('\u{2500}'..='\u{259f}').contains(&c)
}

fn heredoc_opened(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(found) = line.get(from..)?.find("<<") {
        let at = from + found;
        let mut i = at + 2;
        if bytes.get(i) == Some(&b'-') {
            i += 1;
        }
        while bytes.get(i) == Some(&b' ') {
            i += 1;
        }
        let start = i;
        match bytes.get(i) {
            Some(&quote @ (b'\'' | b'"')) => {
                i += line.get(i + 1..)?.find(char::from(quote))? + 2;
            }
            _ => {
                while bytes
                    .get(i)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    i += 1;
                }
            }
        }
        if i > start {
            return Some(i);
        }
        from = at + 2;
    }
    None
}

/// The agent's words as the page may draw them.
pub fn agent_words(text: &str) -> Box<str> {
    agent_words_in(text, DECIDED_CHARS)
}

fn agent_words_in(text: &str, room: usize) -> Box<str> {
    let mut out = String::new();
    let mut kept = 0_usize;
    let mut space = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => skip_escape(&mut chars),
            '\u{9b}' => skip_until_final(&mut chars),
            c if c.is_whitespace() => space = !out.is_empty(),
            c if c.is_control() || is_bidi_control(c) => {}
            c => {
                if space {
                    out.push(' ');
                    kept += 1;
                    space = false;
                }
                out.push(c);
                kept += 1;
                if kept > room {
                    break;
                }
            }
        }
    }
    atpkg::progress::sanitize_for_tty(&out, room).into_boxed_str()
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        Some('[') => skip_until_final(chars),
        Some(']' | 'P' | 'X' | '^' | '_') => {
            while let Some(c) = chars.next() {
                if c == '\u{7}' || c == '\u{9c}' {
                    return;
                }
                if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                    chars.next();
                    return;
                }
            }
        }
        _ => {}
    }
}

fn skip_until_final(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for c in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&c) {
            return;
        }
    }
}

const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Fold one line of a session's approval ledger ([`crate::supervise::approvals::Row::to_json`]'s shape) into `acc`, over its window.
pub fn fold_ledger_line(acc: &mut HarnessStats, line: &str) {
    match row_and_time(line, "ts") {
        Some((v, ts)) => fold_ledger_row(acc, &v, ts),
        None => bump(&mut acc.bad),
    }
}

/// Fold one approval ledger row `v`, written at `ts` (unix ms), into `acc`, over its window.
pub fn fold_ledger_row(acc: &mut HarnessStats, v: &Value, ts: i64) {
    let text = |k: &str| v.get(k).and_then(Value::as_str);
    let sid = text("sid").unwrap_or_default();
    if let Some(row) = approvals::SelfClaimRow::of(v) {
        fold_self_claim(acc, sid, &row, ts);
        return;
    }
    let rule = text("rule_id").unwrap_or("-");
    let outcome = text("decision").and_then(Outcome::from_word);
    let fact = approvals::is_fact_rule(rule);
    match outcome {
        Some(Outcome::Waiting) if rule == RULE_WALL_WAIT => {
            acc.open.limit.entry(sid.to_string()).or_insert(ts);
        }
        Some(Outcome::Waiting) if rule == RULE_WALL_WAIT_END => close_limit(acc, sid, ts),
        Some(Outcome::Typed) if !fact => close_limit(acc, sid, ts),
        _ => {}
    }
    if ts < acc.since_ms {
        return;
    }
    acc.row_of(sid);
    if fact {
        fold_fact(acc, rule, outcome, text("reason").unwrap_or_default());
        return;
    }
    let Some(outcome @ (Outcome::Approved | Outcome::Declined | Outcome::Typed)) = outcome else {
        return;
    };
    acc.act(rule);
    let command = text("command").unwrap_or_default();
    let sorted = acts::sort(rule, outcome, command);
    if let Some(decided) = acc.decided.as_mut() {
        let row = LedgerAct {
            ts,
            outcome,
            sorted,
            command,
            reason: text("reason").unwrap_or_default(),
            box_seq: v.get("box_seq").and_then(Value::as_u64),
            rule,
        };
        decided.note_ledger(sid, &row);
    }
    let Some(kind) = sorted.counter else {
        return;
    };
    acc.count(sid, Count::Act(kind));
    if kind == Counter::Approved && text("reason").is_some_and(|r| r.starts_with("unproven:")) {
        acc.count(sid, Count::Unproven);
    }
}

/// A PROVEN SELF-CLAIM'S ACT IS ITS HOLDER'S, NEVER ATERM'S: a [`approvals::SelfClaimRow`] in the
/// window adds to its holder's [`HarnessStats::self_claimed`] and the session's share of it, and
/// to no count, row or session of aterm's own harness; one that names no holder is a line no row
/// is ([`HarnessStats::bad`]).
fn fold_self_claim(acc: &mut HarnessStats, sid: &str, row: &approvals::SelfClaimRow<'_>, ts: i64) {
    if row.holder.is_empty() {
        bump(&mut acc.bad);
        return;
    }
    if ts < acc.since_ms {
        return;
    }
    let name = acts::claimant_name(row.holder);
    acc.self_claimed
        .entry(name.to_string())
        .or_default()
        .take(row.act);
    if let Some(tally) = acc.tally(sid) {
        tally
            .self_claimed
            .entry(name.to_string())
            .or_default()
            .take(row.act);
    }
}

fn fold_fact(acc: &mut HarnessStats, rule: &str, outcome: Option<Outcome>, reason: &str) {
    use crate::supervise::facts::FactKind;
    use crate::supervise::occupancy::rules::{RuleId, WOULD_RULE};
    if crate::supervise::occupancy::door::is_task_row(reason) {
        return;
    }
    let (book, kind) = if rule == WOULD_RULE {
        let named = reason
            .split(' ')
            .find_map(|w| w.strip_prefix("rule="))
            .and_then(RuleId::of_name);
        let Some(named) = named else {
            return;
        };
        (&mut acc.facts_would, named.kind())
    } else {
        match (FactKind::of_rule(rule), outcome) {
            (Some(kind), Some(Outcome::Typed)) => (&mut acc.facts_told, kind),
            _ => return,
        }
    };
    *book.entry(kind.word().to_string()).or_insert(0) += 1;
}

fn close_limit(acc: &mut HarnessStats, sid: &str, at: i64) {
    if let Some(start) = acc.open.limit.remove(sid) {
        let ms = span_ms(start, at, acc.since_ms);
        acc.waited(sid, Wait::Limit, ms);
    }
}

/// What an `ESCALATED` line's tail (`attention=<reply> [again=<why>]`) says
/// went up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escalation {
    /// No confirmed badge: another supervisor answers the session, a stall withheld the ask, or the set was refused or answered no known result.
    Withheld,
    /// A badge set again, no new ask: a limit episode's whose retry hit the wall again before the worker answered ([`AGAIN_RETRY`]).
    Again,
    /// A point handed to a person: an escalation, and a person's wait.
    Asked,
}

fn escalation_of(summary: &str) -> Escalation {
    if summary.split(' ').next() != Some("attention=OK") {
        return Escalation::Withheld;
    }
    let again = summary
        .split_once(" again=")
        .or_else(|| summary.split_once(" mail=skipped: "))
        .map_or("", |(_, why)| why);
    if [AGAIN_RETRY, AGAIN_ADOPTED, AGAIN_UNPARKED]
        .iter()
        .any(|why| again.starts_with(why))
    {
        Escalation::Again
    } else {
        Escalation::Asked
    }
}

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

fn needs_went_up(summary: &str) -> bool {
    summary
        .split(' ')
        .find_map(|w| w.strip_prefix("attention="))
        .is_some_and(|reply| reply.strip_suffix(':').unwrap_or(reply) == "OK")
}

fn cleared_reply(reply: &str) -> bool {
    matches!(reply.strip_suffix(':').unwrap_or(reply), "OK" | "none")
}

fn is_adopted(r: &JournalRecord) -> bool {
    r.kind == "other" && r.line.split(' ').next() == Some(ADOPTED)
}

/// What a journal record says of Land's landings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LandLine {
    /// A land act typed: `CONTINUED` or `TYPED` under `land@v1`, `land-nudge@v1` or `land-answer@v1`, or a wall's continuation that typed Land's own words under the wall's rule ([`opens_with_land_words`]).
    Act,
    /// A landing's note, `DONE [seq=<n>] rule=land@v1 …` (kind `other`), that restates no landing: no `ended`.
    Note,
    /// A note that restates a landing noted before, its first time its `ended`: a restarted loop's, or the task's own wake's.
    Restated,
}

fn land_line(r: &JournalRecord) -> Option<LandLine> {
    match r.kind.as_str() {
        "continued" | "typed" => {
            (is_land_rule(&r.phase) || opens_with_land_words(&r.summary)).then_some(LandLine::Act)
        }
        "other" => {
            let mut words = r.line.split(' ');
            if words.next() != Some("DONE") {
                return None;
            }
            let mut word = words.next()?;
            if word.starts_with("seq=") {
                word = words.next()?;
            }
            if word.strip_prefix("rule=") != Some(RULE_LAND) {
                return None;
            }
            Some(if r.ended.is_some() {
                LandLine::Restated
            } else {
                LandLine::Note
            })
        }
        _ => None,
    }
}

/// Whether a journal line's `said` opens with Land's words as a line clips them: today's, or FOREVER LAND's before their no-repository sentence, which a journal an older build wrote holds.
fn opens_with_land_words(said: &str) -> bool {
    const SAID: [&[u8]; 4] = [
        clipped(LAND),
        clipped(LAND_NUDGE),
        clipped(LAND_REMOTE_MAIN_BEFORE),
        clipped(LAND_NUDGE_REMOTE_MAIN_BEFORE),
    ];
    let said = said.as_bytes();
    SAID.iter().any(|words| said.starts_with(words))
}

const fn clipped(s: &str) -> &[u8] {
    s.as_bytes().split_at(clip_len(s)).0
}

const fn clip_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut chars = 0;
    while i < bytes.len() {
        // A character starts at every byte that is no UTF-8 continuation.
        if bytes[i] & 0xC0 != 0x80 {
            if chars == LINE_CHARS {
                return i;
            }
            chars += 1;
        }
        i += 1;
    }
    bytes.len()
}

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

/// Fold a session journal's records ([`crate::supervise::journal`]) into `acc`, over its window.
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
                        acc.escalated_at(sid, r.t);
                    }
                    acc.badge_up(sid, r.t);
                }
            },
            "cleared" if takes_badge_down(&r.summary) => acc.badge_down(sid, r.t),
            "needs" if inside && needs_went_up(&r.summary) => acc.count(sid, Count::NeedsTold),
            "exit" | "timeout" => {
                acc.open.human.remove(sid);
            }
            "other" if is_adopted(r) => acc.badge_up(sid, r.t),
            "other" if frozen_takes_badge_down(r) => acc.badge_down(sid, r.t),
            "chose" if inside => acc.count(sid, Count::Act(Counter::Answered)),
            "other" | "continued" | "typed" => {
                if let Some(line) = land_line(r) {
                    acc.land(sid, line, inside);
                }
            }
            _ => {}
        }
        if inside {
            if let Some(decided) = acc.decided.as_mut() {
                decided.note_journal(sid, r);
            }
            acc.row_of(sid);
        }
    }
}

/// Fold one line of the live upgrade ledger (`upgrade_drive::ledger`) into `acc` over [`HarnessStats::since_ms`].
pub fn fold_upgrade_line(acc: &mut HarnessStats, line: &str) {
    match row_and_time(line, "t") {
        Some((v, t)) => fold_upgrade_row(acc, &v, t),
        None => bump(&mut acc.bad),
    }
}

/// Fold one live upgrade ledger row `v`, written at unix second `t`, into `acc`, over its window.
pub fn fold_upgrade_row(acc: &mut HarnessStats, v: &Value, t: i64) {
    if t.saturating_mul(1000) < acc.since_ms {
        return;
    }
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
    let tab = text("tab");
    match tab {
        "" | "-" => bump(&mut acc.rows),
        tab => acc.row_of(tab),
    }
    if text("step") == "relaunched" {
        let count = if moved_build(text("from"), text("to")) {
            Count::Upgraded
        } else {
            Count::Relaunched
        };
        acc.count(tab, count);
    }
}

fn moved_build(from: &str, to: &str) -> bool {
    let to = to
        .strip_suffix(')')
        .and_then(|t| t.rsplit_once('('))
        .map_or(to, |(version, _)| version);
    let known = |v: &str| !v.is_empty() && v != "-";
    known(from) && known(to) && from != to
}

fn row_and_time(line: &str, key: &str) -> Option<(Value, i64)> {
    let v = aterm_json::from_str::<Value>(line).ok()?;
    let t = v.get(key).and_then(Value::as_i64)?;
    Some((v, t))
}

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

impl DriveFile {
    const fn suffix(self) -> &'static str {
        match self {
            Self::Ledger => ".jsonl",
            Self::Journal => ".journal.jsonl",
        }
    }
}

/// A ledger or journal [`drive_files`] listed: its path, what it is, the
/// session its name carries, and when it was last written (unix ms; `None`
/// where that cannot be read, and the file is read).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    path: PathBuf,
    kind: DriveFile,
    sid: String,
    modified_ms: Option<i64>,
}

fn drive_files(drive: &Path, since_ms: i64) -> Vec<Listed> {
    let Ok(entries) = std::fs::read_dir(drive) else {
        return Vec::new();
    };
    let mut out: Vec<Listed> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            let kind = if name.ends_with(DriveFile::Journal.suffix()) {
                DriveFile::Journal
            } else if name.ends_with(DriveFile::Ledger.suffix()) {
                DriveFile::Ledger
            } else {
                return None;
            };
            let sid = name.strip_suffix(kind.suffix()).unwrap_or(name).to_string();
            let path = e.path();
            let modified_ms = modified_ms(&path);
            Some(Listed {
                path,
                kind,
                sid,
                modified_ms,
            })
        })
        .filter(|file| !untouched(file.modified_ms, since_ms))
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn modified_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
}

fn untouched(modified_ms: Option<i64>, since_ms: i64) -> bool {
    modified_ms.is_some_and(|ms| ms.saturating_add(MTIME_SLACK_MS) < since_ms)
}

fn untouched_since(path: &Path, since_ms: i64) -> bool {
    untouched(modified_ms(path), since_ms)
}

fn upgrade_ledger(harness_state: &Path) -> PathBuf {
    harness_state.join("upgrade").join("ledger.jsonl")
}

/// WHAT THE HARNESS DID since `since_ms` (unix ms), read from the files it keeps.
#[must_use]
pub fn stats_under(state_dir: &Path, harness_state: &Path, since_ms: i64) -> HarnessStats {
    let mut acc = HarnessStats::new(since_ms);
    for file in drive_files(&state_dir.join("drive"), since_ms) {
        let Ok(lines) = tail_lines(&file.path, READ_MAX_ROWS) else {
            continue;
        };
        match file.kind {
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

/// ONE READING, TWO WINDOWS: what the harness did since `today_ms` (each session's share kept)
/// and since `week_ms`, reading each file at most once.
#[must_use]
pub fn stats_windows(
    state_dir: &Path,
    harness_state: &Path,
    today_ms: i64,
    week_ms: i64,
) -> (HarnessStats, HarnessStats) {
    stats_windows_with(state_dir, harness_state, today_ms, week_ms, &mut |path| {
        tail_lines(path, READ_MAX_ROWS)
    })
}

fn stats_windows_with(
    state_dir: &Path,
    harness_state: &Path,
    today_ms: i64,
    week_ms: i64,
    read: &mut dyn FnMut(&Path) -> std::io::Result<Vec<String>>,
) -> (HarnessStats, HarnessStats) {
    let mut today = HarnessStats::per_session(today_ms);
    let mut week = HarnessStats::new(week_ms);
    for file in drive_files(&state_dir.join("drive"), today_ms.min(week_ms)) {
        let wants = [
            !untouched(file.modified_ms, today_ms),
            !untouched(file.modified_ms, week_ms),
        ];
        let Ok(lines) = read(&file.path) else {
            continue;
        };
        match file.kind {
            DriveFile::Ledger => {
                let mut oldest = None;
                for line in &lines {
                    let row = row_and_time(line, "ts");
                    if oldest.is_none() {
                        oldest = row.as_ref().map(|(_, t)| *t);
                    }
                    for (acc, wanted) in [(&mut today, wants[0]), (&mut week, wants[1])] {
                        match &row {
                            Some((v, ts)) if wanted => fold_ledger_row(acc, v, *ts),
                            None if wanted => bump(&mut acc.bad),
                            _ => {}
                        }
                    }
                }
                for (acc, wanted) in [(&mut today, wants[0]), (&mut week, wants[1])] {
                    if wanted && window_cut(lines.len(), approvals::KEEP_ROWS, oldest, acc.since_ms)
                    {
                        acc.cut_of(&file.sid);
                    }
                }
            }
            DriveFile::Journal => {
                let mut recs = Vec::with_capacity(lines.len());
                let mut bad = 0_u64;
                for line in &lines {
                    match JournalRecord::from_json(line) {
                        Ok(r) => recs.push(r),
                        Err(_) => bump(&mut bad),
                    }
                }
                let oldest = recs.first().map(|r| r.t);
                for (acc, wanted) in [(&mut today, wants[0]), (&mut week, wants[1])] {
                    if !wanted {
                        continue;
                    }
                    acc.bad = acc.bad.saturating_add(bad);
                    if window_cut(lines.len(), approvals::KEEP_ROWS, oldest, acc.since_ms) {
                        acc.cut_of(&file.sid);
                    }
                    fold_journal(acc, &recs);
                }
            }
        }
    }
    let path = upgrade_ledger(harness_state);
    let modified = modified_ms(&path);
    let wants = [
        !untouched(modified, today_ms),
        !untouched(modified, week_ms),
    ];
    if wants.contains(&true)
        && let Ok(lines) = read(&path)
    {
        let mut oldest = None;
        for line in &lines {
            let row = row_and_time(line, "t");
            if oldest.is_none() {
                oldest = row.as_ref().map(|(_, t)| t.saturating_mul(1000));
            }
            for (acc, wanted) in [(&mut today, wants[0]), (&mut week, wants[1])] {
                match &row {
                    Some((v, t)) if wanted => fold_upgrade_row(acc, v, *t),
                    None if wanted => bump(&mut acc.bad),
                    _ => {}
                }
            }
        }
        for (acc, wanted) in [(&mut today, wants[0]), (&mut week, wants[1])] {
            // The upgrade ledger is every session's: its cut is the window's.
            if wanted && window_cut(lines.len(), DISK_LEDGER_KEEP_ROWS, oldest, acc.since_ms) {
                acc.cut = true;
            }
        }
    }
    today.end_reading();
    week.end_reading();
    (today, week)
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
