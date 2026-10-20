// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure turn-end policy for a supervised worker. [`decide_turn_end`] and
//! [`decide_turn_end_traced`] choose an action from [`TurnEndReading`],
//! [`TurnEndState`], [`SupervisorConfig`] (`[harness]`) and the supplied clock.
//! `supervise/turn_end_loop.rs` executes it and reports observations, typed
//! acts and host outcomes back to the state; this module performs no I/O.
//!
//! Decision order:
//! * Autopilot Off leaves the session alone. Notices belonging to a login or
//!   model the session already left are removed before deciding again.
//! * Upgrade ownership, the critical-memory restart exception, an act still
//!   awaiting its answer and login handling precede the ordinary turn end.
//! * The ordinary point must be authoritative, belong to an unfinished task
//!   and pass the person's grace. A wall decides first; otherwise owed acts,
//!   reset continuations and retained wall episodes precede normal policy.
//! * Normal policy continues, answers, checks completion or lands the task.
//!   Eligible account moves and model returns can wrap that result. Typing
//!   budgets, back-off and credits holds constrain acts; a standing draft is
//!   submitted only through the final composer-safe conversion.
//!
//! [`TurnEndState`] owns the outstanding act, work/back-off history, task and
//! Land completion, wall/credits episodes, and account/model/reset debts.
//! A person's exchange is left to them only with observed or recorded turn
//! ownership. Host notices and carry-on turns remain distinct. A completed
//! task stays complete until the conversation records a new external turn
//! (with a screen fallback when no record is available). Done reports get a
//! check; open work or repository work blocks completion under Cruise and
//! Land. Answers to irreversible requests retain the mode's reversible-only
//! restriction.
//!
//! The loop's `supervise/wall_confirm.rs` owns the wall trust boundary:
//! Claude notices must satisfy its transcript/no-transcript confirmation
//! rules before reaching this decider. Wall responses use aterm-authored
//! text, never screen text typed back with the person's authority. Retry,
//! login, context, usage and model episodes keep their own recovery rules;
//! a notice disappearing alone does not establish recovery.
//!
//! Usage credits are opt-in (`usage_credits = false` by default). Reported
//! credits episodes hold the policy's model turns, including retries and
//! compaction, across reads without the notice. Account/model moves may
//! avoid the hold; login and memory recovery and the person's own turns are
//! not held. **This is no billing cap:** unreported credits are invisible,
//! and a probe after the named reset or hold bound can still spend credits
//! if another usage window binds. See [`credits_hold`] and the `credits-hold`
//! contract for the model-specific process lifetime and probe rules.
//!
//! Exact contracts and test bindings live in `crates/aterm-agent/SPECS.md`
//! (`turn-end-decision`, `host-turn-end-protocol`, `wall-confirm-gate`,
//! `api-wall-acts`, `login-wall-acts`, `usage-wall-wait`, `credits-hold`).
//! `turn_end_tests.rs` covers pure decisions, `supervise/turn_end_loop_tests.rs`
//! covers execution, and `tests/supervise_conformance_*.rs` binds the models.
//! Vendor readings use the captures in `aterm-phase/src/fixtures` and
//! transcript captures in `supervise/fixtures`; those are evidence for the
//! reader, not proof that a host can perform every requested recovery.
//! Account rotation's unmeasured second-login boundary is documented in
//! `docs/DESIGN-claude-accounts-2026-10-01.md` §9. Autopilot and reset ownership
//! are specified in `docs/DESIGN-harness-autopilot-2026-10-15.md` and
//! `docs/SPEC-claude-accounts-resets-2026-10-02.md`.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use aterm_phase::phase::{Phase, is_done_row, last_said_row, status_line};
use aterm_phase::wall::WallKind;
use aterm_phase::{ApiCause, Program};

use super::super::config::{Autopilot, SupervisorConfig};
use super::super::limit::ResetClock;
use super::approval::QuestionPriority;
use super::login::LoginShared;
use super::reset::{RESET_CONTINUES_WAIT, ResetOutcome};
use super::sleeps::InTurn;
use super::trace::{NoTrace, Outcome, Trace, note};
use crate::harness::acts::TaskFace;
use crate::harness::harness_mark;

/// A continuation typed at an ordinary turn end ([`decide_turn_end`]).
pub const RULE_CONTINUE: &str = "continue@v1";
/// The [`DONE_CHECK`] typed where the worker said it is done: its yield
/// saying done again ends the task ([`TurnEndState::task_done`]).
pub const RULE_DONE_CHECK: &str = "continue-done-check@v1";
/// The challenge ([`open_items_text`]) typed where the worker's own word
/// would end the task — its [`FINISHED`], or a done reply to the check or a
/// land act — while its report's own words name open work of any kind
/// ([`open_work`]), at EVERY such word: never once a job (the owner's rule of
/// 2026-10-19). Its yield saying done, or ending with [`FINISHED`], ends the
/// task only where its words name no open work either.
pub const RULE_OPEN_ITEMS: &str = "continue-open-items@v1";
/// The challenge ([`uncommitted_text`]) typed where the worker's own word
/// would end the task while the session's repository holds work no remote
/// has ([`TurnEndReading::repo`]: uncommitted, untracked, stashed or
/// unpushed), at every such word while the probe finds it. Its yield ends
/// the task as the open-items challenge's does, the repository found clean.
pub const RULE_UNCOMMITTED: &str = "continue-uncommitted@v1";

/// Whether `rule` is a challenge of a done that is no finish
/// ([`RULE_OPEN_ITEMS`], [`RULE_UNCOMMITTED`]): the done check's kin, whose
/// done reply ends the task as the check's does and which a wall's retry
/// delivers again.
#[must_use]
pub fn is_challenge_rule(rule: &str) -> bool {
    rule == RULE_OPEN_ITEMS || rule == RULE_UNCOMMITTED
}
/// The journal's name for a task the worker ended with [`FINISHED`].
pub const RULE_FINISHED: &str = "finished@v1";
/// The journal's name for a person's own exchange left to them
/// ([`TurnEndState::left_to_person`]); never typed.
pub const RULE_LEFT: &str = "left-to-person@v1";
/// Typed in place of the continuation when the worker says it is done
/// ([`says_done`]). The reply it asks for is itself a done report. It
/// defines finished as the primer does (`aterm-primer`'s PRIMER_BODY):
/// nothing of the job is left — every change verified, committed and pushed,
/// nothing uncommitted, untracked, stashed or unpushed, and no open item,
/// follow-up or next step in the report, one with a blocker included (the
/// owner's rule of 2026-10-19). The worker reports the outcome in ordinary
/// language; no completion token or special output format is asked for
/// (`69a8f68178`), though a reply that ends on the legacy [`FINISHED`] is
/// read as one still.
pub const DONE_CHECK: &str = "Before you stop: do every open item, skipped check, follow-up and \
     next step, and verify it (build, test, measure where numbers were asked for), then commit \
     and push it (merged to main where this repository lands work). Report whether the task is \
     complete, the outcome and its verification in ordinary language. It is complete only when \
     nothing of the job is left: nothing uncommitted, untracked, stashed or unpushed, and no \
     open item, follow-up or next step in your report, a blocked one included. Otherwise \
     continue.";

/// The done check from `69a8f68178` (ordinary language) until the owner's
/// rule of 2026-10-19 (an item left that names its blocker ended the job),
/// retained only for reading existing conversations and their task results.
pub(crate) const DONE_CHECK_ORDINARY_BEFORE: &str = "Before you stop: do every open item, \
     skipped check, follow-up and next step you can still do yourself now, and verify it \
     (build, test, measure where numbers were asked for, commit and land where this repository \
     lands work). Report whether the task is complete, the outcome, verification, and any \
     remaining blockers in ordinary language. Stop only when everything left needs a person (a \
     credential, a decision, physical access) or cannot be done on this host, each item named \
     with its blocker; a slow check is not a blocker. Otherwise continue.";

/// The last completion check that asked for the token
/// ([`DONE_CHECK_ORDINARY_BEFORE`]'s predecessor), retained only for reading
/// existing conversations and their task results. New prompts use
/// [`DONE_CHECK`].
pub(crate) const DONE_CHECK_MARKER_BEFORE: &str = "Before you stop: do every open item, skipped \
     check, follow-up and next step you can still do yourself now, and verify it (build, test, \
     measure where numbers were asked for, commit and land where this repository lands \
     work). End your reply with ATERM-TASK-DONE on a line by itself only when everything \
     left needs a person (a credential, a decision, physical access) or cannot be done on \
     this host, each item named with its blocker; a slow check is not a blocker. Otherwise \
     continue.";

/// What follows [`DONE_CHECK`] where the done report also names an
/// irreversible act ([`Said::Irreversible`], D1): the check's `otherwise
/// continue` alone would read as consent to it. aterm's own fixed words.
pub const DONE_CHECK_REVERSIBLE: &str =
    "Approve nothing irreversible: delete, overwrite and force-push nothing.";

/// The worker's explicit done token: a last line that is exactly this
/// (Markdown punctuation allowed). Ends the task at once with no
/// [`DONE_CHECK`] ([`TurnEndState::task_done`]) unless the report names open
/// work ([`open_work`], [`RULE_OPEN_ITEMS`]) or the session's repository
/// holds work no remote has ([`RULE_UNCOMMITTED`]). Retained for old
/// transcripts and workers taught it; new prompts and primers do not ask
/// workers to emit it.
pub const FINISHED: &str = "ATERM-TASK-DONE";

/// A WORKER's done check and its answers go without the short back-off
/// ([`TurnEndState::set_managed`]) while its short streak is at most this
/// many turns: past it, the back-off as for anyone, so a worker that asks
/// after every short turn is answered at the back-off's pace (2, 4, 8 min
/// skipped; 16 min waited at the fourth).
pub const MANAGED_SHORT_TURNS: u32 = 3;

/// Whether the worker's last words end with [`FINISHED`] on a line of its
/// own: the explicit, unambiguous done. A token quoted inside a sentence, or
/// followed by more text, is not it.
#[must_use]
pub fn says_finished(tail: Option<&str>) -> bool {
    tail.and_then(|t| t.lines().rev().find(|l| !l.trim().is_empty()))
        .is_some_and(is_finished_line)
}

/// THE MARK OF THE WORKER'S LAST WORDS (`tail`, [`TurnEndReading::said_tail`]
/// as its program's reader reads them): FNV-1a/64 over their letters and
/// digits alone, so the same words mark the same at any width — a resize
/// wraps them onto other rows, a relaunched agent draws its conversation
/// again — under any footer, status row or banner, and after the vendor's
/// own idle compaction, which the tail reads past. `None` with no words.
///
/// What it tells apart is A LATER REPLY: a turn anyone takes after a landing
/// ends on words of its own, though they end on [`FINISHED`] too. So the
/// journal keeps the mark of each point it records (`"mark"`), and a landing
/// a loop before this one landed holds for the next loop only while the
/// screen's last words are still the landing's own
/// ([`TurnEndState::seed_landed`]; the Autopilot design's §9.4.8).
#[must_use]
pub fn said_mark(tail: Option<&str>) -> Option<u64> {
    let mut letters = String::new();
    letters.extend(tail?.chars().filter(|c| c.is_alphanumeric()));
    (!letters.is_empty()).then(|| aterm_hash::fnv1a_64(letters.as_bytes()))
}

/// Whether one line of a report is [`FINISHED`] alone (Markdown punctuation allowed): the
/// last line [`says_finished`] reads as the done, and a line what a worker said is shown
/// without ([`without_finished_lines`]). A quoted line (`> ATERM-TASK-DONE`, or the gutter
/// Claude Code draws a quote in) is someone else's report the worker passed on, not its own
/// done.
#[must_use]
pub fn is_finished_line(line: &str) -> bool {
    !line.trim_start().starts_with(['>', '▎', '│', '┃'])
        && line.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) == FINISHED
}

/// `text` with its [`is_finished_line`] lines dropped, trimmed: what a worker
/// said, without the token that told the harness it was done — the task's
/// result (`harness::result`) and the survey's `said`
/// (`harness::excerpt`) read it this one way.
#[must_use]
pub fn without_finished_lines(text: &str) -> String {
    let kept: Vec<&str> = text.lines().filter(|l| !is_finished_line(l)).collect();
    kept.join("\n").trim().to_string()
}

/// The heads a report lists open work under, lowercased ([`open_work`]): a
/// sentence that begins with one (Markdown's list, heading, quote and
/// emphasis marks dropped) and ends there, or goes on with `:`, `(`, ` -`,
/// ` –` or ` —`, after at most one of [`OPEN_NOUNS`] (`Remaining work:`,
/// `Not done yet:`), or with a short tail that a colon closes, after `,`,
/// `from` or `for` (`Still open from AU-1:`, `Open items, all out of
/// scope:`, `Follow-ups for the owner:`). A Markdown heading that begins with
/// one is a head whatever follows a comma (`## Not measured, and why`).
/// `Left the config as it was` and `Remaining tests pass` are no head. The
/// challenge names what matched in these words.
pub const OPEN_HEADS: &[&str] = &[
    "still open",
    "still to do",
    "left open",
    "open items",
    "open issues",
    "open work",
    "open tasks",
    "open",
    "to do",
    "todo",
    "todos",
    "left",
    "not done",
    "not finished",
    "unfinished",
    "remaining",
    "what remains",
    "what's left",
    "what is left",
    "follow-up",
    "follow-ups",
    "follow up",
    "follow ups",
    "followup",
    "followups",
    "next step",
    "next steps",
    "outstanding",
    "pending",
    "deferred",
    "skipped",
    "not run",
    "not verified",
    "not tested",
    "untested",
    "not measured",
    "unmeasured",
];

/// The longest tail, in bytes, a head may carry before its colon
/// ([`OPEN_HEADS`]): `Still open from the implementer, and still true:`.
const HEAD_TAIL: usize = 60;

/// The nouns a head may carry before its colon ([`OPEN_HEADS`]).
const OPEN_NOUNS: &[&str] = &[
    "items", "item", "issues", "work", "tasks", "steps", "points", "to do", "yet",
];

/// The words that leave work open wherever they stand in a sentence
/// ([`open_work`]): `the e2e suite was not run`, `Tests: skipped`, `The
/// lexer is still open`, `I did not find the cause`, `Observed, not
/// explained`. A report's `didn't` reads as `did not`, and `not yet run` or
/// `not been run` as `not run`. Not after a word of [`OPEN_NEGATIONS`] in
/// the same clause (`nothing was skipped`), nor `not …` after a modal
/// ([`OPEN_MODALS`]: `do not run it twice`) or `skipped` after `is`, `are`
/// or `be` (`a warm cache is skipped`): those describe; they report nothing
/// undone. Nor inside quotation marks (`the "window still open" check`),
/// where a phrase is mentioned, not reported; nor before a reason that says
/// there was nothing to do (`not run, because no contract was added`).
/// Its `still fails` and kin report a check that is red now (`but one test
/// still fails`), not one that failed and was fixed.
pub const OPEN_STATUS: &[&str] = &[
    "not run",
    "skipped",
    "not done",
    "not verified",
    "not tested",
    "untested",
    "not measured",
    "unmeasured",
    "not re-measured",
    "not re-run",
    "not rerun",
    "not checked",
    "unchecked",
    "not compiled",
    "uncompiled",
    "not reproduced",
    "not validated",
    "not observed",
    "unobserved",
    "not exercised",
    "not explained",
    "unexplained",
    "not fixed",
    "unfixed",
    "not confirmed",
    "unconfirmed",
    "not closed",
    "not identified",
    "unidentified",
    "not passed",
    "still open",
    "remains open",
    "left open",
    "left for",
    "follow-up",
    "follow-ups",
    "needs a re-run",
    "need a re-run",
    "needs re-running",
    "did not measure",
    "did not test",
    "did not verify",
    "did not check",
    "did not look",
    "did not find",
    "did not track",
    "did not investigate",
    "did not fix",
    "did not confirm",
    "still fails",
    "still fail",
    "still failing",
];

/// The verbs whose `n't` a report's words spell out ([`ReportLine::of`]):
/// `didn't fix` reads as `did not fix`.
const CONTRACTED: &[&str] = &["did", "was", "were", "has", "have", "had", "is", "are"];

/// What negates an open item, as its first word, or a status word, before it
/// in its clause ([`open_work`]): `Left: none`, `no check was skipped`.
const OPEN_NEGATIONS: &[&str] = &[
    "none", "nothing", "no", "n/a", "nil", "never", "zero", "0", "without", "neither", "nor",
];

/// The words before `not` that make a status word a description
/// ([`OPEN_STATUS`]). `could` is none of them: `could not run` reports a run
/// that did not happen.
const OPEN_MODALS: &[&str] = &[
    "do", "does", "don't", "doesn't", "must", "should", "shall", "will", "would", "can", "may",
    "might", "need", "needs",
];

/// What a report's own words leave open, in [`OPEN_HEADS`]' and
/// [`OPEN_STATUS`]' spelling, each once, in the order found: a head with an
/// item that is not negated ([`OPEN_NEGATIONS`]), and a status word outside
/// any head's items that is neither negated, nor a description, nor before a
/// reason that leaves nothing to do ([`open_status`]). WHATEVER BLOCKS IT: an
/// item that names its blocker or a decision is open work all the same (the
/// owner's rule of 2026-10-19: `ATERM-TASK-DONE` never ends a job while any
/// follow-up is left), so `Still open (needs the owner's credential): the
/// release` reads `still open`. Empty where the report names no open work:
/// `Nothing left.`, `No open items.`, `Left: none`, `no check was skipped`,
/// `not run: no contract was added`.
///
/// The report is the worker's last words ([`TurnEndReading::said_tail`]),
/// one screen row a line: a row that is no list item and does not begin with
/// a capital continues the row above (its wrap), and the [`FINISHED`] line is
/// no part of it. A head's inline item ends at its sentence; later sentences
/// are read independently, including after a negated item. Where nothing but
/// a colon follows the head, its items are the lines under it: the first,
/// then each list item, up to the next head or a line that is no list item;
/// the head is open where any of them is not negated. A qualifier in
/// brackets after the head (`Left (none):`) that negates it clears it. Text
/// in backticks and fenced code is never a word of the report; a code block
/// also separates the prose on either side.
///
/// Conservative: a miss costs the harness as it was (the token ends the
/// task), a false find one challenge at each token over those words, at the
/// short streak's back-off. Measured against the labelled reports of three
/// rounds of this repository's own work (`fixtures/open-work-corpus.txt`
/// beside this file), which names each excerpt this reading still gets
/// wrong, and why.
#[must_use]
pub fn open_work(tail: Option<&str>) -> Vec<&'static str> {
    let Some(tail) = tail else {
        return Vec::new();
    };
    let lines = report_lines(tail);
    let mut found: Vec<&'static str> = Vec::new();
    let mut i = 0;
    while let Some(line) = lines.get(i) {
        let words = line.words.as_str();
        let starts = sentence_starts(words);
        let end = |k: usize| starts.get(k + 1).copied().unwrap_or(words.len());
        let mut next = i + 1;
        for k in 0..starts.len() {
            let sentence = &words[starts[k]..end(k)];
            let head = open_head(sentence).or_else(|| {
                (k == 0 && line.heading)
                    .then(|| heading_head(sentence))
                    .flatten()
            });
            let Some((head, rest)) = head else {
                if let Some(word) = open_status(&words[starts[k]..], end(k) - starts[k]) {
                    found.push(word);
                }
                continue;
            };
            let (qualifier, inline) = qualified(rest);
            let cleared = negated(qualifier);
            let one_sentence = sentence_starts(inline).len() == 1;
            if inline.is_empty() || (one_sentence && inline.ends_with(':')) {
                // A bare head: the lines under it are its items.
                let mut j = i + 1;
                let mut items: Vec<&str> = Vec::new();
                while let Some(under) = lines.get(j) {
                    if under.separated
                        || open_head(&under.words).is_some()
                        || !(under.item || items.is_empty())
                    {
                        break;
                    }
                    items.push(&under.words);
                    j += 1;
                }
                next = j;
                if !cleared && items.iter().any(|item| !negated(item)) {
                    found.push(head);
                }
                break;
            }
            if !cleared && !negated(inline) {
                found.push(head);
            }
        }
        i = next;
    }
    let mut once: Vec<&'static str> = Vec::with_capacity(found.len());
    for word in found {
        if !once.contains(&word) {
            once.push(word);
        }
    }
    once
}

/// One logical line of a report ([`report_lines`]).
struct ReportLine {
    /// A code block separates this row from the preceding prose.
    separated: bool,
    /// It began with a list marker (`- `, `* `, `• `, `1. `, `1) `).
    item: bool,
    /// It is a Markdown heading (`## Not measured`).
    heading: bool,
    /// Its first letter, past the marks, is a capital.
    capital: bool,
    /// Its words, lowercased, with the marks, emphasis and code dropped, a
    /// [`CONTRACTED`] verb's `n't` spelled out, and `yet` or `been` after
    /// `not` dropped.
    words: String,
}

impl ReportLine {
    fn of(raw: &str) -> Self {
        let heading = raw.trim_start().starts_with('#');
        let t = raw
            .trim()
            .trim_start_matches(|c: char| c == '>' || c == '#' || c.is_whitespace());
        let marked = ["- ", "* ", "+ ", "• ", "◦ ", "‣ "]
            .iter()
            .find_map(|m| t.strip_prefix(m))
            .or_else(|| {
                let digits = t.bytes().take_while(u8::is_ascii_digit).count();
                let rest = t.get(digits..).filter(|_| (1..=3).contains(&digits))?;
                rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
            });
        let (item, t) = marked.map_or((false, t), |r| (true, r.trim_start()));
        let capital = t
            .chars()
            .find(|&c| c != '*' && c != '_')
            .is_some_and(char::is_uppercase);
        let mut flat = String::with_capacity(t.len());
        let mut code = false;
        for c in t.chars() {
            match c {
                '`' => {
                    code = !code;
                    flat.push(' ');
                }
                _ if code => {}
                '*' => {}
                '’' => flat.push('\''),
                c => flat.extend(c.to_lowercase()),
            }
        }
        let mut words = String::with_capacity(flat.len());
        let mut after_not = false;
        for token in without_underscore_emphasis(&flat).split_whitespace() {
            if after_not && matches!(token, "yet" | "been") {
                continue;
            }
            if !words.is_empty() {
                words.push(' ');
            }
            let spelled = token.split_once("n't").filter(|(verb, rest)| {
                CONTRACTED.contains(verb) && !rest.contains(char::is_alphanumeric)
            });
            if let Some((verb, rest)) = spelled {
                words.push_str(verb);
                words.push_str(" not");
                words.push_str(rest);
                after_not = rest.is_empty();
            } else {
                words.push_str(token);
                after_not = token == "not";
            }
        }
        Self {
            separated: false,
            item,
            heading,
            capital,
            words,
        }
    }
}

/// Drop paired underscore emphasis at word boundaries, preserving underscores
/// within identifiers and every unmatched mark.
fn without_underscore_emphasis(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut kept = 0;
    let mut cursor = 0;
    let mut open = None;
    while let Some(offset) = text[cursor..].find('_') {
        let at = cursor + offset;
        let width = text[at..].bytes().take_while(|&b| b == b'_').count();
        cursor = at + width;
        let before = text[..at].chars().next_back();
        let after = text[cursor..].chars().next();
        let closes =
            before.is_some_and(|c| !c.is_whitespace()) && after.is_none_or(|c| !word_char(c));
        if let Some((start, marks)) = open
            && marks == width
            && closes
        {
            out.push_str(&text[kept..start]);
            out.push_str(&text[start + marks..at]);
            kept = cursor;
            open = None;
        } else if open.is_none()
            && before.is_none_or(|c| !word_char(c))
            && after.is_some_and(|c| !c.is_whitespace())
        {
            open = Some((at, width));
        }
    }
    out.push_str(&text[kept..]);
    out
}

/// A Markdown code-fence run and the text after it. The closing run must use
/// the same mark, be at least as wide, and carry no further text.
fn code_fence(line: &str) -> Option<(u8, usize, &str)> {
    let line = line.trim();
    let mark = *line.as_bytes().first()?;
    if !matches!(mark, b'`' | b'~') {
        return None;
    }
    let width = line.bytes().take_while(|&b| b == mark).count();
    (width >= 3).then(|| (mark, width, &line[width..]))
}

/// A report's logical lines ([`open_work`]): its rows, the [`FINISHED`] line,
/// blank rows and fenced code left out. A noncapital row continues the row
/// above unless a code block separates them; a block also ends a head's items.
fn report_lines(tail: &str) -> Vec<ReportLine> {
    let mut out: Vec<ReportLine> = Vec::new();
    let mut fence = None;
    let mut separated = false;
    for raw in tail.lines() {
        if let Some((mark, width)) = fence {
            if code_fence(raw)
                .is_some_and(|(m, n, rest)| m == mark && n >= width && rest.trim().is_empty())
            {
                fence = None;
            }
            continue;
        }
        if let Some((mark, width, _)) = code_fence(raw) {
            fence = Some((mark, width));
            separated = true;
            continue;
        }
        if is_finished_line(raw) {
            continue;
        }
        let mut line = ReportLine::of(raw);
        if line.words.is_empty() {
            continue;
        }
        line.separated = std::mem::take(&mut separated);
        match out.last_mut() {
            Some(above) if !line.separated && !line.item && !line.capital => {
                above.words.push(' ');
                above.words.push_str(&line.words);
            }
            _ => out.push(line),
        }
    }
    out
}

/// Where each sentence of `words` begins: `0`, then past each `.`, `;`, `!`
/// or `?` a space follows.
fn sentence_starts(words: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (at, c) in words.char_indices() {
        if matches!(c, '.' | ';' | '!' | '?') && words[at + 1..].starts_with(' ') {
            starts.push(at + 2);
        }
    }
    starts.retain(|&s| s < words.len() || s == 0);
    starts
}

/// The head `text` begins with ([`OPEN_HEADS`], the longest that fits) and
/// what follows it: from its colon where a tail stands before that.
fn open_head(text: &str) -> Option<(&'static str, &str)> {
    let ends = |rest: &str| {
        rest.is_empty()
            || rest.starts_with([':', '('])
            || [" -", " –", " —", " ("].iter().any(|p| rest.starts_with(p))
    };
    OPEN_HEADS
        .iter()
        .filter_map(|&head| {
            let rest = text.strip_prefix(head)?;
            let rest = OPEN_NOUNS
                .iter()
                .find_map(|noun| {
                    rest.strip_prefix(' ')?
                        .strip_prefix(noun)
                        .filter(|r| ends(r))
                })
                .unwrap_or(rest);
            if ends(rest) {
                Some((head, rest))
            } else {
                head_tail(rest).map(|rest| (head, rest))
            }
        })
        .max_by_key(|(head, _)| head.len())
}

/// What follows a head's tail ([`HEAD_TAIL`]), from its colon: `rest` opens
/// with `, `, ` from ` or ` for `.
fn head_tail(rest: &str) -> Option<&str> {
    let tail = [", ", " from ", " for "]
        .iter()
        .find_map(|p| rest.strip_prefix(p))?;
    let colon = tail.find(':').filter(|&at| at <= HEAD_TAIL)?;
    Some(&tail[colon..])
}

/// The head a Markdown heading begins with before a comma, bare whatever
/// follows it (`## Not measured, and why`): the lines under it are its items.
fn heading_head(text: &str) -> Option<(&'static str, &str)> {
    OPEN_HEADS
        .iter()
        .filter(|&&head| text.strip_prefix(head).is_some_and(|r| r.starts_with(',')))
        .max_by_key(|head| head.len())
        .map(|&head| (head, ""))
}

/// A head's rest split into its bracketed qualifier, if any, and the item
/// after the colon or dash.
fn qualified(rest: &str) -> (&str, &str) {
    let rest = rest.trim_start();
    let (qualifier, after) = rest
        .strip_prefix('(')
        .and_then(|r| r.split_once(')'))
        .unwrap_or(("", rest));
    let item = after
        .trim_start_matches(|c: char| matches!(c, ':' | '-' | '–' | '—') || c.is_whitespace())
        .trim_end();
    (qualifier, item)
}

/// A word character for [`phrases_in`]: `skipped-test` and `--skipped` hold
/// no `skipped`.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// A table of phrases and the bytes they begin with ([`phrases_in`]).
struct Phrases {
    table: &'static [&'static str],
    first: [bool; 256],
}

impl Phrases {
    const fn of(table: &'static [&'static str]) -> Self {
        let mut first = [false; 256];
        let mut i = 0;
        while i < table.len() {
            first[table[i].as_bytes()[0] as usize] = true;
            i += 1;
        }
        Self { table, first }
    }
}

/// [`OPEN_STATUS`] for [`phrases_in`].
const STATUS_PHRASES: Phrases = Phrases::of(OPEN_STATUS);

/// Each phrase of `phrases` (each begins with a [`word_char`]) that stands in
/// `text` as whole words, with where it begins: one pass over the words of
/// `text`, a word whose first byte begins no phrase passed at one look, and
/// never a substring search per phrase, which cost five times the whole
/// reading on a 15 KB report once the tables grew.
fn phrases_in<'t>(
    text: &'t str,
    phrases: &'t Phrases,
) -> impl Iterator<Item = (usize, &'static str)> + 't {
    let mut before = None;
    text.char_indices()
        .filter_map(move |(at, c)| {
            let starts = word_char(c)
                && !before.is_some_and(word_char)
                && phrases.first[usize::from(text.as_bytes()[at])];
            before = Some(c);
            starts.then_some(at)
        })
        .flat_map(move |at| {
            phrases
                .table
                .iter()
                .copied()
                .filter(move |phrase| {
                    text.as_bytes().get(at) == phrase.as_bytes().first()
                        && text[at..].starts_with(phrase)
                        && !text[at + phrase.len()..]
                            .chars()
                            .next()
                            .is_some_and(word_char)
                })
                .map(move |phrase| (at, phrase))
        })
}

/// Whether an item's first word negates it ([`OPEN_NEGATIONS`]): `none`,
/// `nothing left`, `no open items`.
fn negated(item: &str) -> bool {
    item.trim_start_matches(|c: char| !c.is_alphanumeric())
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '!' | ')'))
        .next()
        .is_some_and(|first| OPEN_NEGATIONS.contains(&first))
        || item.starts_with("n/a")
}

/// The status word the sentence of its first `len` bytes reports open work
/// with ([`OPEN_STATUS`]), if one stands there undescribed, unnegated,
/// unquoted and not before a reason that leaves nothing to do; `line` runs on
/// to the end of the sentence's line, where that reason may stand.
fn open_status(line: &str, len: usize) -> Option<&'static str> {
    let sentence = &line[..len];
    let stands: Vec<(usize, &'static str)> = phrases_in(sentence, &STATUS_PHRASES).collect();
    OPEN_STATUS.iter().copied().find(|&word| {
        stands.iter().filter(|&&(_, w)| w == word).any(|&(at, _)| {
            let clause = sentence[..at]
                .rsplit([',', ';', ':', '(', ')', '—', '–'])
                .next()
                .unwrap_or("");
            let before: Vec<&str> = clause.split_whitespace().collect();
            let prev = before.last().copied();
            !before.iter().any(|w| OPEN_NEGATIONS.contains(w))
                && !(word.starts_with("not ") && prev.is_some_and(|p| OPEN_MODALS.contains(&p)))
                && !(word == "skipped" && prev.is_some_and(|p| ["is", "are", "be"].contains(&p)))
                && !quoted(sentence, at)
                && !nothing_to_do(&line[at + word.len()..])
        })
    })
}

/// Whether `at` stands inside quotation marks in `text`.
fn quoted(text: &str, at: usize) -> bool {
    let before = &text[..at];
    before.matches('"').count() % 2 == 1 || before.rfind('“') > before.rfind('”')
}

/// Whether what follows a status word gives a reason that leaves nothing to
/// do: one that opens with a negation after a colon, semicolon, bracket or
/// full stop (`not run: no contract was added`, `not run. No contract was
/// added.`), or a `because`, `since` or `as` clause that holds one or
/// `already` (`not run, because the diff adds no contract`, `not done,
/// because main already has it`). Never one that says only that the work
/// takes long ([`ONLY_SLOW`]: `not run, because there was no time`), nor
/// one that names what is lacking ([`LACKING`]: `not reproduced: no mount is
/// available`, `not measured (no windows allowed)`): a negated blocker is
/// still a blocker, and the work it blocks is open (the owner's rule of
/// 2026-10-19).
fn nothing_to_do(after: &str) -> bool {
    let after = after.trim_start();
    let explained = after.starts_with([':', ';', '(', '.']);
    let reason = after.trim_start_matches(|c: char| {
        matches!(c, ',' | ':' | ';' | '(' | '.') || c.is_whitespace()
    });
    let because = ["because ", "since ", "as "]
        .iter()
        .find_map(|w| reason.strip_prefix(w));
    let clause = because
        .unwrap_or(reason)
        .split([',', ';', '.'])
        .next()
        .unwrap_or("");
    if reason_words(clause).any(|w| ONLY_SLOW.contains(&w) || LACKING.contains(&w)) {
        return false;
    }
    if because.is_some() {
        return reason_words(clause).any(|w| OPEN_NEGATIONS.contains(&w))
            || clause
                .split(|c: char| !word_char(c))
                .any(|w| w == "already");
    }
    explained
        && reason_words(reason)
            .next()
            .is_some_and(|w| OPEN_NEGATIONS.contains(&w))
}

/// The words of a reason ([`nothing_to_do`]), `n/a` one of them.
fn reason_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | '(' | ')'))
        .filter(|w| !w.is_empty())
}

/// What a reason says when it says only that the work takes long
/// ([`nothing_to_do`]): the owner's rule takes no slowness for a blocker, so
/// `not run, because there was no time` leaves the run to do.
const ONLY_SLOW: &[&str] = &["time", "slow", "long", "minutes", "hours"];

/// What a negated reason names when it names a lack, not an absence of work
/// ([`nothing_to_do`]): `no mount is available`, `no windows allowed`, `no
/// GPU on this host`. `no contract was added` names none, and leaves
/// nothing to do.
const LACKING: &[&str] = &[
    "available",
    "allowed",
    "permitted",
    "installed",
    "here",
    "host",
    "machine",
    "access",
    "window",
    "windows",
    "mount",
    "gpu",
    "credential",
    "credentials",
    "memory",
    "ram",
    "disk",
];

/// AUTOPILOT LAND's instruction ([`LAND`]), typed once a Land episode where
/// the continuation or the done check would go ([`continue_policy`]).
pub const RULE_LAND: &str = "land@v1";
/// A nudge after the land instruction ([`LAND_NUDGE`]), at most
/// [`LAND_NUDGES`] an episode.
pub const RULE_LAND_NUDGE: &str = "land-nudge@v1";
/// The answer a question gets under Land ([`LAND_ANSWER`]).
pub const RULE_LAND_ANSWER: &str = "land-answer@v1";
/// The most land nudges one Land episode types after its instruction: the
/// bound that keeps Land from becoming a second Cruise (the design's §7).
pub const LAND_NUDGES: u32 = 2;

/// What Land types once an episode ([`RULE_LAND`]): wrap up, start nothing
/// new, leave it verified, committed and pushed, write what is left down in
/// the repository rather than in the report, then summarize the outcome. It
/// doubles as the done check: a done or finished reply to it ends the task
/// where it leaves nothing open — none of its words naming open work,
/// nothing in the repository no remote has (the challenges,
/// [`TurnEndState::observe`]). Marked as the harness's own turn
/// ([`crate::harness::upgrade::HARNESS_MARK`]), never a task of anyone's.
pub const LAND: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed and pushed \
     (merged to main where this repository lands work), with nothing uncommitted, untracked or \
     stashed. Write down what is left in the repository and commit that too, so your summary \
     names nothing left. Then write a short summary of whether the work is complete and what \
     is done."
);

/// [`LAND`]'s words from `69a8f68178` (ordinary language) until the owner's
/// rule of 2026-10-19 (it asked for what is left in the summary), which the
/// records of conversations landed then still hold
/// (`crate::harness::upgrade::FinishedScan`, the ledger's `/compact`
/// redelivery).
pub const LAND_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of whether the work is \
     complete, what is done and what is left."
);

/// What a Land nudge types ([`RULE_LAND_NUDGE`]) where the yield after the
/// instruction said no done.
pub const LAND_NUDGE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, write down what is left in the \
     repository and commit it, then summarize whether the work is complete and what is done."
);

/// [`LAND_NUDGE`]'s words from `69a8f68178` until the owner's rule of
/// 2026-10-19, as [`LAND_BEFORE`].
pub const LAND_NUDGE_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, then summarize whether the work \
     is complete, what is done and what is left."
);

/// What a question gets under Land ([`RULE_LAND_ANSWER`]): the option that
/// finishes cleanly soonest, never one that starts more or cannot be undone.
pub const LAND_ANSWER: &str = "Nobody is here to choose: take the option that finishes this work \
                               cleanly soonest, start nothing new, and change nothing irreversible.";

/// [`LAND`], with the standing rules when there are any.
#[must_use]
pub fn land_text(rules: Option<&str>) -> String {
    with_rules(LAND, rules)
}

/// [`LAND_NUDGE`], with the standing rules when there are any.
#[must_use]
pub fn land_nudge_text(rules: Option<&str>) -> String {
    with_rules(LAND_NUDGE, rules)
}

/// [`LAND_ANSWER`], with the standing rules when there are any.
#[must_use]
pub fn land_answer_text(rules: Option<&str>) -> String {
    with_rules(LAND_ANSWER, rules)
}

/// Whether `rule` is one of Land's three acts ([`RULE_LAND`],
/// [`RULE_LAND_NUDGE`], [`RULE_LAND_ANSWER`]).
#[must_use]
pub fn is_land_rule(rule: &str) -> bool {
    matches!(rule, RULE_LAND | RULE_LAND_NUDGE | RULE_LAND_ANSWER)
}

/// The worker's own suggestion accepted.
pub const RULE_SUGGESTION: &str = "continue-suggestion@v1";
/// A continuation after an overload or an API error's wait (and the one
/// try at the ceiling while the network is measured down).
pub const RULE_API_RETRY: &str = "api-retry@v1";
/// A continuation once the host measures the API reachable again, after
/// an API error that never reached it.
pub const RULE_API_BACK: &str = "api-back@v1";
/// A continuation after a reply the connection cut off.
pub const RULE_API_CUTOFF: &str = "api-cutoff@v1";
/// A LOST LOGIN'S TURN DELIVERED AGAIN once the login came back in another
/// tab of the host ([`super::login::LoginShared::Back`]): the work the wall
/// cut, or the person's message it swallowed, with nothing of this tab's own
/// sign-in. Once a loss: met by the wall again, this tab's own `/login`
/// follows.
pub const RULE_LOGIN_BACK: &str = "login-back@v1";

/// How often a tab waiting on another tab's sign-in of its login reads that
/// sign-in again ([`super::login::LoginShared::Carried`]): one lock of the
/// host's, and the news of the login back is this late at most.
pub const LOGIN_SHARED_LOOK: Duration = Duration::from_secs(15);

/// [`RULE_LOGIN_BACK`]'s words, the agent `program`'s.
#[must_use]
pub fn login_back_text(program: Program) -> String {
    format!(
        "{} reported that its login had expired, and it is signed in again now. {CARRY_ON}",
        program.product()
    )
}

/// What every wall act ends with: where to carry on, and not to repeat a
/// step whose result the failure may have eaten.
const CARRY_ON: &str = "Carry on from where you stopped; if the result of your last step is \
missing, check whether it ran before you repeat it.";

/// How a person signs back in to `program`, its lost login's wall reading
/// `message`: Claude Code's `/login`; Codex and Muse Code sign in outside
/// their TUIs, with `codex login` or `muse login` in a shell; agy has no
/// sign-in command and signs in only as it starts — or, run under the Gemini
/// API provider, answers a 401 or a key that is no key (`API_KEY_INVALID`)
/// with the same lost login, which only a valid key lifts (measured in the
/// lab, `aterm_phase::agy::fixtures::lab`). A Muse Code key wall names its
/// own remedy ([`aterm_phase::muse::remedy`]: a rejected `META_API_KEY` is
/// updated or unset, since it beats `muse login`), and that is the step.
/// Gemini CLI names its own, `/auth`, on its banner (`Authenticated with
/// gemini-api-key /auth`, measured on 0.46.0), whose menu a person answers —
/// one of its sign-ins opens a browser — and its walls of that kind are an
/// API key it was refused (a 401, or Google's `API_KEY_INVALID`), which it
/// reads only as it starts: `/auth` in its tab, or a relaunch with a key
/// Google takes. The login exception's step too
/// ([`super::exception::exception_for`]).
pub(super) fn sign_in_how(program: Program, message: &str) -> String {
    match program {
        Program::Claude | Program::Generic => "sign in with /login".to_string(),
        Program::Codex => "sign in to Codex again with codex login".to_string(),
        Program::Agy => {
            "quit agy and run it again to sign in, or give it a valid Gemini API key".to_string()
        }
        Program::Muse => aterm_phase::muse::remedy(message).map_or_else(
            || "sign in to Muse Code again with muse login in a shell".to_string(),
            |r| format!("as Muse Code says, {r}"),
        ),
        Program::Gemini => {
            "sign Gemini CLI in with /auth in its tab, or run it again with a valid GEMINI_API_KEY"
                .to_string()
        }
    }
}

/// Whether a lost login's escalation says how `program` signs in AHEAD of
/// the wall's words: Gemini CLI's refused key quotes Google's JSON whole
/// (measured on 0.46.0), and an escalation's reason is cut after 120 cells
/// (`escalate::REASON_CELLS`), so its sign-in step would be cut off behind
/// it. Every other program's escalation keeps the wall's words first.
fn how_leads(program: Program) -> bool {
    match program {
        Program::Gemini => true,
        Program::Claude | Program::Codex | Program::Agy | Program::Muse | Program::Generic => false,
    }
}

/// The command a lost login's rule types into `program` to sign it back in:
/// Claude Code's `/login`, whose page finishes in the browser. `None` for a
/// program that signs in outside its TUI (Codex, agy, Muse Code,
/// [`sign_in_how`]), and for Gemini CLI, whose `/auth` opens a menu a person
/// answers and whose lost login is a refused key: nothing is typed, and the
/// point is escalated with how it signs in.
fn login_command(program: Program) -> Option<&'static str> {
    match program {
        Program::Claude => Some("/login"),
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => None,
    }
}

/// The command a full context's rule types into `program` to compact it:
/// `/compact`, Claude Code's and Muse Code's alike. `None` (Codex is never
/// supervised; agy has
/// none; Gemini CLI's is `/compress`, which its reader never reaches: no
/// full context of it is measured): nothing is typed, and the point is
/// escalated.
fn compact_command(program: Program) -> Option<&'static str> {
    match program {
        Program::Claude | Program::Muse => Some("/compact"),
        Program::Codex | Program::Agy | Program::Gemini | Program::Generic => None,
    }
}

/// Text typed at an API wall's act: the wall's kind named in aterm's own
/// words ([`api_wall_words`]), plus what follows from it. `rule` is the
/// act's; `kind` the wall's; `program` the one whose screen drew it.
///
/// Never a row of the screen (module header, ruling 2026-10-08): what
/// [`TurnEndReading::wall_message`] holds is whatever the reader took for
/// the wall, and text typed here is submitted as the person's own turn.
#[must_use]
pub fn wall_retry_text(rule: &str, kind: WallKind, program: Program) -> String {
    let who = program.product();
    let what = api_wall_words(kind);
    let cause = match kind {
        WallKind::ApiError { cause, .. } => cause,
        _ => ApiCause::Server,
    };
    match (rule, cause) {
        (RULE_API_BACK, _) => {
            format!("{who} reported {what}, and the API is reachable again now. {CARRY_ON}")
        }
        (_, ApiCause::CutOff) => {
            format!("{who} reported {what}, so your last reply may be incomplete. {CARRY_ON}")
        }
        (_, ApiCause::Unreachable | ApiCause::Config) => {
            format!("{who} reported {what}; trying again. {CARRY_ON}")
        }
        // The reply ran past its output token maximum: written again the same
        // way it is cut off again. The words ask for smaller pieces (Claude
        // Code asked so three times already); what bounds the cost is the
        // ladder ([`wall_own_act`]).
        (_, ApiCause::Overlong) => format!(
            "{who} reported {what}, so your last reply was cut off. Write long files and \
             outputs across several smaller replies. {CARRY_ON}"
        ),
        (_, ApiCause::Server | ApiCause::Request) => format!("{who} reported {what}. {CARRY_ON}"),
    }
}

/// An API wall in aterm's words, from its kind alone: `an API error
/// (overloaded)`, `an API error (500)`, `an API error (the API could not be
/// reached)`. The status is the reader's number, never its text.
#[must_use]
pub fn api_wall_words(kind: WallKind) -> String {
    let (code, why) = match kind {
        WallKind::Overloaded => (None, Some("overloaded")),
        WallKind::ApiError { code, cause, .. } => (
            code,
            match cause {
                ApiCause::Server => None,
                ApiCause::Unreachable => Some("the API could not be reached"),
                ApiCause::CutOff => Some("the reply was cut off"),
                ApiCause::Config => Some("a certificate or proxy refusal"),
                ApiCause::Request => Some("the request was refused"),
                ApiCause::Overlong => Some("the reply was over its output token maximum"),
            },
        ),
        other => return format!("{} wall", super::a_or_an(other.name())),
    };
    match (code, why) {
        (Some(code), Some(why)) => format!("an API error ({code}, {why})"),
        (Some(code), None) => format!("an API error ({code})"),
        (None, Some(why)) => format!("an API error ({why})"),
        (None, None) => "an API error".to_string(),
    }
}

/// What the session's host last measured of the agent's route to its API
/// ([`crate::supervise::IdleHost::reach`]), never read off the screen.
/// `since` is when the measure last changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    /// Nobody measures it here (a loop with no host), or the route is one the
    /// host cannot reproduce (a base URL, a cloud provider, a proxy).
    #[default]
    Unknown,
    Down {
        since: Instant,
    },
    Up {
        since: Instant,
    },
}

/// The most acts of ONE wall rule this policy types in its window
/// ([`TurnEndTiming::window`], an hour), the last line against a hot loop
/// (a full context behind a shell, seen since the reader looks under
/// background work, would have been `/compact` 720 times an hour). Every
/// ladder of
/// this policy stays under it — the densest, the network's 5-minute rung, is
/// twelve in the hour, and the others one or two — so it slows no real resume:
/// the act past it waits for the oldest to leave the window, and says so
/// (`WAITING … a breaker`).
pub const WALL_ACTS_PER_WINDOW: usize = 12;

/// A rule of this policy's wall acts (the ones [`WALL_ACTS_PER_WINDOW`]
/// counts, and [`TurnEndState::acted`] counts as a wall's attempts).
fn is_wall_act(rule: &str) -> bool {
    matches!(
        rule,
        RULE_API_RETRY
            | RULE_API_BACK
            | RULE_API_CUTOFF
            | RULE_LIMIT_RESUME
            | RULE_CONSENT
            | RULE_COMPACT
            | RULE_LOGIN
    )
}

/// A wall's own continuation: its next appearance is the act's to count.
pub(crate) fn is_wall_retry(rule: &str) -> bool {
    matches!(
        rule,
        RULE_API_RETRY
            | RULE_API_BACK
            | RULE_API_CUTOFF
            | RULE_LIMIT_RESUME
            | RULE_COMPACT
            | RULE_LOGIN_BACK
    )
}
/// A continuation past a usage window's reset.
pub const RULE_LIMIT_RESUME: &str = "usage-resume@v1";
/// The agent relaunched on the fallback model on a model bucket.
pub const RULE_MODEL_FALLBACK: &str = "model-fallback@v1";
/// The agent relaunched on the bucket's model again at the bucket's reset.
pub const RULE_MODEL_RESTORE: &str = "model-restore@v1";
/// The agent relaunched under another of the owner's Claude Code accounts
/// ([`Restart::Account`]; `docs/DESIGN-claude-accounts-2026-10-01.md` §6).
pub const RULE_ACCOUNT: &str = "account-rotate@v1";
/// THE ACCOUNTS PROBE: `/usage` typed to read the session's account's
/// windows off the Settings panel, and the Escape that closes it
/// (`docs/SPEC-claude-accounts-probe-2026-10-01.md`). A step of the LOOP's,
/// not an act of this policy's: no [`TurnEndAction`] carries the rule, it is
/// no wall act and no continue rule, and [`TurnEndState::acted`] is never
/// called for it — the probe moves none of this policy's memory (no budget,
/// no awaited act, no back-off). Here for the one name the session ledger's
/// rows and the journal's lines share, and for the gate
/// ([`TurnEndState::probe_point`]).
pub const RULE_USAGE_PROBE: &str = "usage-probe@v1";

/// THE GATE THAT SHUT ON A DUE ACCOUNTS PROBE: the roster said the account's
/// `/usage` read is due ([`AccountAdvice::Probe`]) and nothing was typed for
/// it — each reason by ONE STABLE WORD ([`Self::word`]), the one the loop's
/// `SKIPPED seq=<n> rule=usage-probe@v1 gate=<word>` line carries, said once
/// a point. Before these words a due probe that was not run said nothing
/// anywhere, and which of a dozen conditions had shut could only be inferred
/// from timings (the live check of 2026-10-02).
///
/// The first twelve are THIS policy's, in the order [`TurnEndState::probe_gate`]
/// asks them; the rest are the loop's and its host's, which only they can
/// know (the probe's spec, §3 G). Reporting only: nothing decides by a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeGate {
    /// The reading carries no probe advice: the roster did not say the read
    /// is due (a move, a wait, nothing).
    NotDue,
    /// The phase is not `Idle`: a question, a box, a turn running, a limit.
    NotIdle,
    /// The reader's phase is a guess, not evidence.
    NotAuthoritative,
    /// A survey card is up.
    Survey,
    /// A wall is on the screen, or one is being worked off it (a wall
    /// track: a limit waited out, a lost login).
    Wall,
    /// The composer is not drawn, or holds something typed.
    Composer,
    /// A submitted row waits unanswered.
    PendingInput,
    /// A person stopped the turn with Esc.
    Interrupted,
    /// The worker asked something of a person.
    Question,
    /// No plain turn end the host is asked about
    /// ([`TurnEndState::account_point`]): no task yet or the task done, the
    /// upgrade's point, no relaunch under another account askable at all,
    /// or the session's own last move not answered.
    NotTurnEnd,
    /// Something of this policy's is awaited or owed: an act awaited, a
    /// command's continuation, a wall's resume after a move.
    ActOwed,
    /// A person typed within the grace (`human_grace_s`).
    Person,
    /// The loop watches behind another supervisor's claim: it acts on
    /// nothing.
    Watching,
    /// The loop keeps no session ledger: a panel it opened could not be
    /// put on record.
    NoLedger,
    /// This loop probes no more: what an earlier probe brought up was not
    /// the Usage tab, or it left the point changed.
    ProbesOff,
    /// A `/usage` panel of the harness's is still owed its close.
    PanelOwed,
    /// The turn that ended here was not read LIVE by this loop: no read
    /// showed the worker's own busy row, so no response of this process is
    /// known to be newer than the home's usage snapshot.
    NotLive,
    /// The point is not new: it is decided AGAIN (a wait that ran out, the
    /// accounts' news), and a probe goes only at the look that first takes
    /// a point.
    NotNew,
    /// The point is a person's: the policy escalated it.
    Escalated,
    /// This grid size showed only the panel's fold at an earlier probe, and
    /// is not typed into again by this loop.
    FoldLatch,
    /// The host wants the point for a step of its own (the live upgrade, a
    /// relaunched agent's carry-on), or took it.
    HostWants,
    /// A stall is held or suspected: the worker may not be reading input.
    Stalled,
    /// The Mac woke less than its settle ago (30 s and the session's share
    /// of 60 s): the panel would read the account's windows from an API the
    /// network may not reach yet.
    WakeSettle,
    /// The server lacks a fence the probe's writes carry: the read's
    /// generation or person count, `if-gen=` on `key` or `send`, the row
    /// guard, the wait on a row leaving.
    NoFence,
    /// The session's program, read fresh, is not Claude Code.
    NotClaude,
    /// The agent does not draw on the alternate screen: Claude Code's
    /// inline renderer, where `/usage` leaves rows in the transcript.
    NotFullscreen,
    /// The host refused the claim; its own word for why follows on the line
    /// (`host=<word>`).
    Claim,
}

impl ProbeGate {
    /// Every gate, for a reader of the words.
    pub const ALL: [ProbeGate; 27] = [
        ProbeGate::NotDue,
        ProbeGate::NotIdle,
        ProbeGate::NotAuthoritative,
        ProbeGate::Survey,
        ProbeGate::Wall,
        ProbeGate::Composer,
        ProbeGate::PendingInput,
        ProbeGate::Interrupted,
        ProbeGate::Question,
        ProbeGate::NotTurnEnd,
        ProbeGate::ActOwed,
        ProbeGate::Person,
        ProbeGate::Watching,
        ProbeGate::NoLedger,
        ProbeGate::ProbesOff,
        ProbeGate::PanelOwed,
        ProbeGate::NotLive,
        ProbeGate::NotNew,
        ProbeGate::Escalated,
        ProbeGate::FoldLatch,
        ProbeGate::HostWants,
        ProbeGate::Stalled,
        ProbeGate::WakeSettle,
        ProbeGate::NoFence,
        ProbeGate::NotClaude,
        ProbeGate::NotFullscreen,
        ProbeGate::Claim,
    ];

    /// The word the journal's `SKIPPED … gate=<word>` line carries. STABLE:
    /// a reader greps for it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            ProbeGate::NotDue => "not-due",
            ProbeGate::NotIdle => "not-idle",
            ProbeGate::NotAuthoritative => "not-authoritative",
            ProbeGate::Survey => "survey",
            ProbeGate::Wall => "wall",
            ProbeGate::Composer => "composer",
            ProbeGate::PendingInput => "pending-input",
            ProbeGate::Interrupted => "interrupted",
            ProbeGate::Question => "question",
            ProbeGate::NotTurnEnd => "not-turn-end",
            ProbeGate::ActOwed => "act-owed",
            ProbeGate::Person => "person",
            ProbeGate::Watching => "watching",
            ProbeGate::NoLedger => "no-ledger",
            ProbeGate::ProbesOff => "probes-off",
            ProbeGate::PanelOwed => "panel-owed",
            ProbeGate::NotLive => "not-live",
            ProbeGate::NotNew => "not-new",
            ProbeGate::Escalated => "escalated",
            ProbeGate::FoldLatch => "fold-latch",
            ProbeGate::HostWants => "host-wants",
            ProbeGate::Stalled => "stalled",
            ProbeGate::WakeSettle => "wake-settle",
            ProbeGate::NoFence => "no-fence",
            ProbeGate::NotClaude => "not-claude",
            ProbeGate::NotFullscreen => "not-fullscreen",
            ProbeGate::Claim => "claim",
        }
    }
}
/// `/compact` on a full context, and the continuation after it.
pub const RULE_COMPACT: &str = "context-compact@v1";
/// `/login` on a lost login.
pub const RULE_LOGIN: &str = "auth-login@v1";
/// `cfg.answer_text`, typed where a worker asked a person.
pub const RULE_ANSWER: &str = "answer@v1";
/// A model bucket's consent to go on on usage credits, accepted — only under
/// `[harness] usage_credits` (off by default: the subscription kept).
pub const RULE_CONSENT: &str = "consent-accept@v1";
/// THE CREDITS HOLD ([`credits_hold`]): nothing of the policy's own typed
/// while usage credits carry the session and `[harness] usage_credits` is
/// off. It types nothing, so it is named in its wait's words and its
/// escalation, never on an act.
pub const RULE_CREDITS_HOLD: &str = "credits-hold@v1";
/// The credits hold's escalation where no reset is named ([`credits_hold`]),
/// told once an episode ([`TurnEndState::credits_told`]).
pub const CREDITS_HOLD_TOLD: &str = "usage credits carry the session and name no reset: \
     nothing is typed on them (credits-hold@v1) but one turn when the five-hour window must \
     have reset; `[harness] usage_credits = true` continues on credits";
/// The same where the credits carry a MODEL's requests, none named
/// ([`CreditsEpisode::model`]): no clock ends that, the session leaving the
/// model does.
pub const CREDITS_HOLD_MODEL_TOLD: &str = "usage credits carry the requests of the model \
     this session runs, with no reset named: nothing is typed on them (credits-hold@v1) until \
     it runs another (`[harness] model_fallback`, or a relaunch); `[harness] usage_credits = \
     true` continues on credits";
/// The agent restarted in its tab for Claude Code's critical-memory banner.
pub const RULE_MEMORY_RESTART: &str = "memory-restart@v1";
/// A turn the session's host typed (upgrade notice, carry-on;
/// [`TurnEndState::host_typed`]): awaited like this policy's acts; its short
/// answer is no short turn of the worker's.
pub const RULE_HOST_TURN: &str = "harness-turn@v1";

/// Which turn the session's host typed ([`TurnEndState::host_typed`]), told
/// by the host from its own step (`upgrade_drive::host_turn`), never read off
/// the screen or the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostTurn {
    /// The upgrade's notice (`announced:<n>`): it asks the worker to get to a
    /// stopping point, so its answer reads as done and is never a done
    /// report. So is a quiet restart's one line that something did not come
    /// back (`continued:restore`, `upgrade::restore_prompt`): it asks only
    /// for that to be restored, and `nothing else` in its answer is that
    /// line's, not the task's. So too the notice typed at a break of
    /// background work (`IdleHost::at_background` names the kind it typed).
    Notice,
    /// The worker's work resumed: the carry-on after a restart (`continued`;
    /// `upgrade::CARRY_ON`), and the release that takes a notice back at an
    /// idle point or at a break of background work (`released:<why>`;
    /// `upgrade::release_prompt`: carry on as you would have without it).
    /// Its answer that says done, short or long, is a done report, as a
    /// continuation's yield is.
    CarryOn,
    /// The carry-on after a restart that IS the done check
    /// (`continued:check`, `upgrade::checking`): typed where the
    /// conversation's last words were a done report, it asks what
    /// [`DONE_CHECK`] asks, and its answer is read as the check's own yield —
    /// the worker's own word that it is done ([`says_done_outright`]), or an
    /// idle report alone as a short turn's whole answer, ends the task
    /// ([`check_answer_ends`]); the check's words echoed back, or an idle
    /// report closing real work, is its work going on. Read as a plain
    /// carry-on, a done answer was a first done
    /// report, and the check was typed a second time (the live lab's
    /// LIVE-RL-3, the review's RR4-RL-5).
    Check,
}

/// Why the session's host restarts the agent in its tab
/// ([`TurnEndAction::Restart`]): a point nothing typed can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// Claude Code's critical-memory banner: nothing typed into the
    /// process is read, and no reset ends it.
    Memory,
    /// A model bucket's limit: relaunched on `to`, the `model_fallback`, via
    /// session-only `--model` (Claude's `/model` also rewrites the person's
    /// default).
    Model { to: String },
    /// The bucket's reset: relaunched on `to`, or with `None` on the launch's
    /// own model, else Claude's default.
    ModelBack { to: Option<String> },
    /// ANOTHER ACCOUNT (the accounts design, §6 "The move"): relaunched on
    /// its own conversation under the login the roster's label `to` names —
    /// the label is the only name an account has here; the directory is the
    /// roster's, resolved where the relaunch is made
    /// (`crate::harness::relaunch::restart_under`). `at` says which of the
    /// two moves it is, because the loop treats a host that cannot make it
    /// yet differently for each ([`AccountAt`]).
    Account { to: String, at: AccountAt },
}

/// WHERE an account move is asked ([`Restart::Account`]). One variant would
/// not do: the loop decides by it whether a relaunch the host cannot make
/// yet (`wait:changed` while the agent's own jobs run) HOLDS the point, and
/// the two moves need opposite answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAt {
    /// The turn ended on a limit: nothing typed there is answered, so the
    /// point is held and the move asked again on
    /// [`TurnEndTiming::restart_backoff`], as a bucket's fallback is.
    Wall,
    /// A plain turn end, the wall still ahead: the session works where it
    /// is, so the move is optional — deferred as the model's return is, the
    /// turn's own act going ahead and the move asked again at the next idle
    /// point.
    Ahead,
}

impl Restart {
    /// The one word the journal names it by (`restart:<word>`).
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Restart::Memory => "memory",
            Restart::Model { .. } => "model",
            Restart::ModelBack { .. } => "model-back",
            Restart::Account {
                at: AccountAt::Wall,
                ..
            } => "account",
            Restart::Account {
                at: AccountAt::Ahead,
                ..
            } => "account-ahead",
        }
    }
}

/// WHAT THE SESSION'S HOST ADVISES ABOUT ITS ACCOUNT at one point
/// ([`TurnEndReading::account`], from `crate::supervise::IdleHost::account`):
/// the roster's judgment (`crate::harness::accounts::advise`) as DATA, so the
/// decider stays pure — it reads no roster, no state file and no clock of its
/// own. `None` on the reading is "stay": no roster, a session the roster does
/// not list, `[harness] accounts = "off"` — today's behaviour, byte for byte.
///
/// THE DECIDER IS TRANSPARENT TO A PROBE. [`AccountAdvice::Probe`] is the
/// roster's word that the account's `/usage` read is due; whether anybody
/// makes it is the loop's and its host's (the claim, the fences, the
/// renderer), so [`decide_turn_end`] and [`TurnEndState::acted`] never act
/// on the word itself: every site that reads the advice reads it through
/// [`AccountAdvice::past_probe`] — the probe's `otherwise` — and a probe
/// nobody makes never masks a move. The one thing here that reads the word
/// is the pure gate [`TurnEndState::probe_point`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccountAdvice {
    /// THE ACCOUNT'S `/usage` READ IS DUE
    /// (`crate::harness::accounts::Advice::Probe`), at a plain turn end.
    /// `otherwise` is what the roster advises PAST the probe
    /// (`crate::harness::accounts::advise_past_probe`, mapped as any advice
    /// is): the move ahead of a wall the probe stands in front of, or
    /// `None` — stay. It is what this file decides on.
    Probe {
        otherwise: Option<Box<AccountAdvice>>,
    },
    /// Relaunch under the account labelled `to`.
    Move { to: String },
    /// No account can be run on before `until`: the wall is waited out as it
    /// would be, and decided again no later than then.
    Wait { until: Instant },
    /// THE OWNER LIFTED THIS WALL (`crate::harness::accounts::Advice::Resume`:
    /// `accounts use` cleared the session's own account after the limit row
    /// it stands at was written): continue here, now — the wall's own
    /// continuation is the probe. `cleared_at` is the clearing's instant
    /// (unix seconds), the key of "once per clearing"
    /// ([`TurnEndState::acted`]): a second advice for the same clearing types
    /// nothing more, and the wall's own ladder takes over.
    Resume { cleared_at: i64 },
}

impl AccountAdvice {
    /// The advice the decider acts on: a probe's `otherwise`, else itself.
    /// `None` for a probe with nothing behind it — stay, exactly as with no
    /// advice at all. Never a [`AccountAdvice::Probe`]: one nested in
    /// another's `otherwise` (no mapping builds that) is read through too.
    #[must_use]
    pub fn past_probe(&self) -> Option<&AccountAdvice> {
        let mut advice = self;
        while let AccountAdvice::Probe { otherwise } = advice {
            advice = otherwise.as_deref()?;
        }
        Some(advice)
    }

    /// THE ROSTER'S ADVICE AS THE POLICY TAKES IT
    /// (`crate::harness::accounts::Advice`, what `accounts::advise` answers a
    /// host with, to [`AccountAdvice`]), `clock` being the `Instant` of the
    /// unix second `now`. The ONE mapping: the window's host advises through
    /// it, and the Tier-1 walk of the rotation's model
    /// (`tests/supervise_conformance_account_rotation.rs`) feeds
    /// [`decide_turn_end`] through it at every state of that model.
    ///
    /// `past`: what `crate::harness::accounts::advise_past_probe` answered
    /// on the same state and second, handed where `advice` is `Probe` (the
    /// roster answers `Probe` AHEAD of its leave check, so the move a due
    /// probe stands in front of is asked for separately); ignored for every
    /// other advice.
    ///
    /// * `Move` is a move, whichever of the three it is (the host's commit
    ///   reads that again under its lock).
    /// * `Wait` is a wait until its instant.
    /// * `Resume` is a resume, for the clearing it names.
    /// * `Stay` for the machine's rotation bound is a wait until the bound has
    ///   room again (the accounts design, §6: "a breaker is a wait").
    /// * `Stay` for an account that has not served a turn yet, AT A WALL, is a
    ///   wait until its canary's hold ends
    ///   (`crate::harness::accounts::CANARY_HOLD_S`): a canary that answers
    ///   wakes the session sooner
    ///   ([`crate::supervise::IdleHost::account_stamp`]), and one that never
    ///   does — its tab idle with nothing to send — must not leave this
    ///   session waiting for its own limit's reset before it takes the
    ///   canary's place.
    /// * THE WAIT OF A SESSION HELD AT ITS WALL ENDS AT WHATEVER COMES FIRST
    ///   (`crate::harness::accounts::hold_until`): the hold's end or the
    ///   hour's, or `opening` — the roster's next opening
    ///   (`crate::harness::accounts::next_opening`, on the same state and
    ///   second; the caller's, which has the roster) — where that is sooner.
    ///   A reset that makes the active account followable, or the session's
    ///   own usable, then decides the point again, where it sat out the rest
    ///   of ten minutes or of the hour beside an account it could run on.
    /// * `Probe`, AT A TURN END, is [`AccountAdvice::Probe`] with `past`
    ///   mapped by these same rules as its `otherwise` (`None` where `past`
    ///   is, or maps to nothing — and a `past` that is itself `Probe`, which
    ///   `advise_past_probe` never answers, is nothing). AT A WALL (the
    ///   screen shows one the roster was asked about as a plain turn end:
    ///   a limit another process wrote) nothing is probed: `past` alone,
    ///   mapped, exactly what the point got before the probe existed.
    /// * Every other `Stay`: nothing — the point is decided as it was before
    ///   accounts existed.
    ///
    /// PURE: no clock but the two it is handed.
    #[must_use]
    pub fn of_roster_at(
        advice: &crate::harness::accounts::Advice,
        past: Option<&crate::harness::accounts::Advice>,
        state: &crate::harness::accounts::State,
        opening: Option<i64>,
        at_wall: bool,
        now: i64,
        clock: Instant,
    ) -> Option<AccountAdvice> {
        use crate::harness::accounts::{Advice, StayWhy, hold_until};
        let at = |until: i64| {
            clock + Duration::from_secs(u64::try_from(until.saturating_sub(now)).unwrap_or(0))
        };
        // Every advice but the probe, which has no word of its own here.
        let plain = |advice: &Advice| match advice {
            Advice::Move { to, .. } => Some(AccountAdvice::Move { to: to.clone() }),
            Advice::Wait { until } => Some(AccountAdvice::Wait { until: at(*until) }),
            Advice::Resume { cleared_at } => Some(AccountAdvice::Resume {
                cleared_at: *cleared_at,
            }),
            // Ahead of a wall a breaker's instant decides nothing (the turn's
            // own act goes on); it is handed over as it is.
            Advice::Stay(StayWhy::Breaker { until }) if !at_wall => {
                Some(AccountAdvice::Wait { until: at(*until) })
            }
            Advice::Stay(why @ (StayWhy::Canary | StayWhy::Breaker { .. })) if at_wall => {
                hold_until(*why, state, opening, now)
                    .map(|until| AccountAdvice::Wait { until: at(until) })
            }
            Advice::Stay(_) | Advice::Probe => None,
        };
        match advice {
            Advice::Probe => {
                let otherwise = past.and_then(plain);
                if at_wall {
                    otherwise
                } else {
                    Some(AccountAdvice::Probe {
                        otherwise: otherwise.map(Box::new),
                    })
                }
            }
            other => plain(other),
        }
    }

    /// [`Self::of_roster_at`] for a caller that has no roster's opening and
    /// no advice past a probe to hand over: a session held at its wall waits
    /// out its hold, or the hour, whole, and a due probe has nothing behind
    /// it.
    #[must_use]
    pub fn of_roster(
        advice: &crate::harness::accounts::Advice,
        state: &crate::harness::accounts::State,
        at_wall: bool,
        now: i64,
        clock: Instant,
    ) -> Option<AccountAdvice> {
        Self::of_roster_at(advice, None, state, None, at_wall, now, clock)
    }
}

/// THE POINT the host is asked about ([`crate::supervise::IdleHost::account`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPoint {
    /// The turn ended on a wall of `kind`, as the SCREEN reads it. Whether
    /// the limit is this process's own (a `quotaLimits` row it wrote, the
    /// accounts design §5) is the host's to tell: the loop reads no
    /// transcript. A lost login ([`WallKind::Auth`]) is asked about so only
    /// while this session's own move stands
    /// ([`TurnEndState::account_point`]).
    Wall { kind: WallKind },
    /// A LOST LOGIN WITH NO MOVE OF THIS SESSION'S OWN BEHIND IT, in a
    /// session on a renewable login ([`TurnEndReading::login_label`]): the
    /// login's own end (`crate::harness::accounts::Point::LoginEnded`), which
    /// only a renewal answers. The policy names which lost login it is, not
    /// the host: the policy holds its move for as long as the move stands —
    /// across the relaunch of a process that left before it answered, and
    /// from the ledger into a loop started later
    /// ([`TurnEndState::seed_account`]) — where what a host remembers of it
    /// ends with that process.
    LoginEnded,
    /// A turn that ended with none.
    TurnEnd,
}

impl AccountPoint {
    /// Whether the session stands at a wall here (a lost login included),
    /// not at a plain turn end.
    #[must_use]
    pub fn at_wall(self) -> bool {
        !matches!(self, AccountPoint::TurnEnd)
    }
}

/// WHAT THE FIRST TURN AFTER THIS SESSION'S OWN ACCOUNT MOVE WAS ANSWERED
/// WITH, when that is no served response
/// ([`crate::supervise::IdleHost::account_answered`]): the host strikes the
/// account, so the advice it gives next is the next account, or the way back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountBad {
    /// A usage limit.
    Wall,
    /// A lost login.
    SignedOut,
}

/// [`TurnEndState::account_answer`]: how the move was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAnswer {
    /// A turn asked under the new login ran and ended with no wall.
    Served,
    Bad(AccountBad),
}

/// An account move this session's host MADE and no point has answered yet
/// ([`TurnEndState::account_move`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMove {
    /// The label moved to.
    pub to: String,
    /// When the relaunch was made.
    pub at: Instant,
    /// Made AT A WALL ([`AccountAt::Wall`]): the conversation the relaunch
    /// resumed may end on the old process's notice, a lost login's
    /// included ([`TurnEndState::login_left`]). A move made ahead of a wall
    /// left the model's reply there.
    pub wall: bool,
}

/// The most account moves ONE session makes in its window
/// ([`TurnEndTiming::window`], an hour); past it the wall is waited out as
/// it would be with no roster, and said so (`WAITING … a breaker`). WHY A
/// COUNT AT ALL: the bucket's guessed return relaunched one session twice an
/// hour for five days, 240 times ([`TurnEndState::restarted`]), and a restart
/// is counted by no other breaker (it is no typed act). WHY FOUR: the design
/// bounds a session at its roster's size plus one, and this decider is pure —
/// it never sees the roster — so the figure is the design's own three-row
/// roster plus one, which a healthy hour never reaches (a move is made only
/// onto an account that can be run on, and each costs a relaunch and an
/// uncached read of the whole context). The roster-sized bound is the
/// machine's, kept by the host (`crate::harness::accounts::rotations_spent`);
/// this one is the session's last line under a host that advises wrongly.
pub const ACCOUNT_MOVES_PER_WINDOW: usize = 4;

/// The nearest a wall's wait is cut to for an account that becomes usable
/// ([`AccountAdvice::Wait`]): an instant nearer than this is decided at this
/// distance, so a host whose instant is always "a moment from now" costs one
/// decision a minute and not a hot loop.
const ACCOUNT_WAIT_FLOOR: Duration = Duration::from_secs(60);

/// The words a worker says it is DONE with, lowercased, matched as whole
/// words of its own anywhere in its last words, never inside another word or
/// a code span ([`says_done`], [`whole_words_at`], [`without_code`]): a
/// continuation's yield that ends so is
/// short, whatever it worked (module header). What the live E2E of
/// 2026-09-25 read after its nudges (`The workflow is finished`, `nothing
/// left to do`, `what would you like me to work on?`) and their kin.
pub const DONE_PHRASES: &[&str] = &[
    "nothing left to do",
    "nothing else to do",
    "nothing more to do",
    "nothing further to do",
    "nothing left to work on",
    "no further work",
    "no remaining work",
    "no more work",
    "all done",
    "all tasks are complete",
    "all tasks complete",
    "everything is done",
    "everything is already done",
    "everything is complete",
    "already complete",
    "work is complete",
    "work is done",
    "task is complete",
    "workflow is finished",
    "workflow is complete",
    // The same reports with `completed`: a phrase reads as whole words
    // ([`whole_words_at`]), so `complete` no longer matches inside them.
    // `already completed` only after a subject of the whole task and a form
    // of `be` (`Everything was already completed.`): bare, `had already
    // completed` reports steps of work still in flight (`The build had
    // already completed step 3 when…`), which names no such subject. A
    // `task …` or `work …` subject is the whole one only with nothing but
    // its determiners before it ([`partial_subject_at`]: `the first task
    // was completed` reports progress).
    "all tasks are completed",
    "all tasks completed",
    "everything is completed",
    "work is completed",
    "task is completed",
    "workflow is completed",
    "everything was completed",
    "everything is already completed",
    "everything was already completed",
    "everything has already been completed",
    "everything had already been completed",
    "everything was already done",
    "all tasks were completed",
    "all tasks are already completed",
    "all tasks were already completed",
    "all tasks have already been completed",
    "all tasks had already been completed",
    "work was completed",
    "work is already completed",
    "work was already completed",
    "work has already been completed",
    "work had already been completed",
    "task was completed",
    "task is already completed",
    "task was already completed",
    "task has already been completed",
    "task had already been completed",
    "completed successfully",
    "what would you like me to work on",
    "what would you like me to do",
    "what should i work on",
    "what else would you like",
    "let me know if you need anything",
    "let me know if there's anything else",
    // A finished report that asks only what comes next.
    "what's next?",
    "anything else?",
    // By family, from the live sessions of 2026-10-02 to 10-15, each
    // continued with `keep going`: the worker says it stops…
    "i'm stopping here",
    "i am stopping here",
    "i'll stop here",
    "i will stop here",
    "i've stopped.",
    "i have stopped.",
    // …hands off to the next session…
    "so the next session can",
    // …or has nothing left that it can do. Never in a sentence that names a
    // wait or work in flight ([`said_done_phrase`]: `everything I can do
    // until the build finishes`, `I'll stop here until CI reports back` are
    // a wait on its own work, whose wake it answers). An empty queue (`all
    // queued work has landed`) is an idle report ([`says_idle`]), and the
    // check's own words echoed back are [`CHECK_ECHO`]: neither ends the task
    // at the check, save an idle report alone as a short turn's whole answer
    // ([`check_answer_ends`]).
    "everything i can do is",
    "everything this machine can do",
    "nothing more i can do",
    "nothing else i can do",
];

/// Whether the worker's last words say it is done ([`DONE_PHRASES`]), that
/// nothing of its own is running or queued ([`says_idle`]), echo the check
/// ([`CHECK_ECHO`]), carry [`FINISHED`] on a line of its own anywhere, or
/// are exactly `DONE` (punctuation allowed). Title-case `Done.` is a step
/// report. The token followed by more words is a done report — the check, or
/// after the check the end — never the end at once ([`says_finished`] reads
/// only the last line). The worker's own word ([`says_done_outright`]) ends
/// the task at the check, and so does an idle report alone as a short turn's
/// whole answer ([`check_answer_ends`]); an echo alone, or an idle report that
/// closes a turn of real work, is a first done report.
///
/// Remembered for the last tail read on this thread (`LastTail`).
#[must_use]
pub fn says_done(tail: Option<&str>) -> bool {
    let Some(t) = tail else {
        return false;
    };
    thread_local! {
        static LAST: LastTail<bool> = const { LastTail::new() };
    }
    LAST.with(|last| last.get_or(t, || says_done_read(t)))
}

/// The check's own words echoed back: a FIRST done report only, never the
/// end at the check's yield, where a worker that is not done echoes them as
/// readily as one that is (`Phase 1 is finished and verified; phase 2 is
/// next`, `Not everything is finished and verified`, `Is the task finished
/// and verified? Not yet`). [`DONE_CHECK`]'s own (`nothing of the job is
/// left`, the owner's rule of 2026-10-19), and the words the checks before
/// it asked with (`everything left needs a person`, the primer's definition
/// of finished the check took in the merge of `f09ef3b15`; `If the task is
/// finished and verified, …`), which workers taught by them and the records
/// they wrote still echo. A worker that is done answers the check with its
/// own completion statement instead of merely repeating the prompt.
const CHECK_ECHO: [&str; 3] = [
    "nothing of the job is left",
    "everything left needs a person",
    "finished and verified",
];

/// [`says_done`] on a tail, read afresh.
fn says_done_read(t: &str) -> bool {
    if says_done_outright(t) {
        return true;
    }
    let flat = done_flat(t);
    says_idle(&flat) || CHECK_ECHO.iter().any(|echo| said_as_report(&flat, echo))
}

/// Whether `flat` ([`done_flat`]) says the check's words `echo` as a report
/// of the worker's own state ([`CHECK_ECHO`]): not negated just before them
/// ([`negated_before`]), and not in a clause that makes them a condition
/// ([`conditional_at`]: `I'll tag it once it is finished and verified`, `If
/// the task is finished and verified, …`).
fn said_as_report(flat: &str, echo: &str) -> bool {
    flat.match_indices(echo).any(|(at, _)| {
        !negated_before(flat, at)
            && !conditional_at(flat, at, echo)
            && !partial_subject_at(flat, at, echo)
    })
}

/// What ends a clause for [`conditional_at`] and [`partial_subject_at`].
const CLAUSE_ENDS: &[char] = &[',', ';', ':', '.', '!', '?', '—', '–', '(', ')'];

/// The words of the clause before `at` in `flat`, back to the nearest
/// [`CLAUSE_ENDS`] mark.
fn clause_before(flat: &str, at: usize) -> Vec<&str> {
    flat[..at]
        .rsplit(CLAUSE_ENDS)
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect()
}

/// Whether the clause `phrase` at `at` in `flat` stands in makes it a
/// CONDITION or a time to come, not a report (the clause runs to the nearest
/// `,` `;` `:` `.` `!` `?` `—` `–` `(` `)` on either side, [`CLAUSE_ENDS`]):
/// - `if`, `when`, `whenever`, `unless` or `once` (not as a count: `once
///   more`, `once again`, `at once`, `more than once`), and `provided`,
///   `providing`, `assuming`, `supposing`, `as soon as` or `as long as`,
///   anywhere among the clause's words outside the phrase itself: `If all
///   tasks are complete, I'll tag the release` and `Everything is done when
///   CI is green` (or `as soon as CI is green`) say nothing is done;
/// - `after`, `before`, `by the time`, `in case` or `whether` only where
///   they LEAD the phrase, before it in its clause (`After all tasks are
///   complete, I'll tag it`, `I'll tag it after it is finished and
///   verified`): after the phrase they date or hedge a report (`All tasks
///   are complete after the rebase`, `… in case you want to ship`) — the
///   review of ATERM-KI-1016-1, KR-DONE-1;
/// - `until` or `till` before the phrase where it waits on work
///   ([`waits_at`]: `the release waits until it is finished and verified`),
///   never before a stop for a person (`Until you review PR #42 there is
///   nothing left to do`); after the phrase [`waits_in`] reads it.
///
/// `When I checked, all tasks were complete` says it in a clause of its own.
/// A phrase whose own words carry one (`let me know if you need anything`)
/// is read past them. A subordinator read where it only dates a report that
/// opens its clause (`Right after the rebase all tasks were complete`) costs
/// one continuation; one missed ends a task whose work goes on.
fn conditional_at(flat: &str, at: usize, phrase: &str) -> bool {
    const IF: &[&str] = &["if", "when", "whenever", "unless", "once"];
    const STATES: &[&[&str]] = &[
        &["provided"],
        &["providing"],
        &["assuming"],
        &["supposing"],
        &["as", "soon", "as"],
        &["as", "long", "as"],
    ];
    const LEADS: &[&[&str]] = &[
        &["after"],
        &["before"],
        &["by", "the", "time"],
        &["in", "case"],
        &["whether"],
    ];
    let after = if phrase.ends_with(CLAUSE_ENDS) {
        ""
    } else {
        flat[at + phrase.len()..]
            .split(CLAUSE_ENDS)
            .next()
            .unwrap_or("")
    };
    // The phrase stands as one word (`\0`, at `p`) between its clause's two
    // halves, so a count's neighbours are read where they stand.
    let mut words = clause_before(flat, at);
    let p = words.len();
    words.push("\0");
    words.extend(after.split_whitespace());
    let word = |j: Option<usize>| j.and_then(|j| words.get(j)).copied().unwrap_or("");
    (0..words.len()).any(|i| {
        let w = words[i];
        let (next, prev, prev2) = (
            word(i.checked_add(1)),
            word(i.checked_sub(1)),
            word(i.checked_sub(2)),
        );
        let count = w == "once"
            && (matches!(next, "more" | "again")
                || prev == "at"
                || (prev == "than" && prev2 == "more"));
        let rest = &words[i..];
        let leads = i < p
            && (LEADS.iter().any(|s| words[i..p].starts_with(s))
                || (matches!(w, "until" | "till") && waits_at(&words, i)));
        (IF.contains(&w) && !count) || STATES.iter().any(|s| rest.starts_with(s)) || leads
    })
}

/// Whether the subject done phrase `phrase` at `at` in `flat` opens with
/// names only PART of the task (the review of ATERM-KI-1016-1, KR-DONE-2):
/// a `task …` or `work …` phrase with anything but determiners of the whole
/// (`the`, `this`, `that`, `my`, `your`, `our`, `all`, `whole`, `entire`)
/// just before it in its clause (`the first task was completed`, `the
/// migration task`, `phase 1's work is done`), or a part named before those
/// (`half the work`, `most of the work`; `all of the work` is the whole);
/// and an `all …` or `everything …` phrase with a part's word just before it
/// (`almost everything is done`, `nearly all tasks are complete`). Other
/// words before the determiners open the clause (`I re-ran the suites and
/// the work is done`). A whole task read as a part costs one continuation; a
/// part read as the whole ends a task whose work goes on.
fn partial_subject_at(flat: &str, at: usize, phrase: &str) -> bool {
    const WHOLE: &[&str] = &[
        "the", "this", "that", "my", "your", "our", "all", "whole", "entire",
    ];
    const PART: &[&str] = &[
        "half",
        "most",
        "some",
        "part",
        "much",
        "almost",
        "nearly",
        "mostly",
        "partly",
        "largely",
        "practically",
        "virtually",
        "of",
    ];
    let noun = phrase.starts_with("task ") || phrase.starts_with("work ");
    if !(noun || phrase.starts_with("all ") || phrase.starts_with("everything ")) {
        return false;
    }
    let before = clause_before(flat, at);
    let mut i = before.len();
    if noun {
        while i > 0 && WHOLE.contains(&before[i - 1]) {
            i -= 1;
        }
        if i == before.len() && i > 0 {
            // A modifier, not a determiner, just before the noun.
            return true;
        }
    }
    match i.checked_sub(1).map(|j| before[j]) {
        Some("of") => i < 2 || before[i - 2] != "all",
        Some(w) => PART.contains(&w),
        None => false,
    }
}

/// The worker's own word that it is done: [`FINISHED`] on a line of its own,
/// exactly `DONE`, or a [`DONE_PHRASES`] phrase said as the end
/// ([`said_done_phrase`]: as whole words outside code ([`whole_words_at`],
/// [`without_code`]), not negated just before it — `This isn't all done`
/// says nothing done — nor in a sentence that names a wait or work in
/// flight, a sentence ending at a list line or heading too
/// ([`done_blocks`]); `what's next?` and `anything else?` only as the last
/// words) —
/// every done report but a first-only one (an idle report, [`says_idle`];
/// the check's words echoed, [`CHECK_ECHO`]). What the check's own yield
/// ends the task on, on the screen and in the conversation's record alike
/// (`crate::harness::upgrade::FinishedScan` reads the check's answer by it),
/// so the record never ends at the check a task the screen goes on with —
/// and never ends a task whose own work's wake it would read as nobody's
/// (the cross-lane review's F4: `I'll stop here until CI reports back` at
/// the check ended the task for good).
#[must_use]
pub(crate) fn says_done_outright(t: &str) -> bool {
    if t.lines().any(is_finished_line) || t.trim_matches(|c: char| !c.is_alphanumeric()) == "DONE" {
        return true;
    }
    let blocks: Vec<String> = done_blocks(&without_code(t))
        .iter()
        .map(|b| done_flat(b))
        .collect();
    let last = blocks.len().saturating_sub(1);
    DONE_PHRASES.iter().any(|p| {
        blocks
            .iter()
            .enumerate()
            .any(|(i, b)| said_done_phrase(b, p, i == last))
    })
}

/// AN IDLE REPORT ALONE ([`says_idle`]: `No lander is running`, `All queued
/// work has landed`), with no word of the worker's own that it is done
/// ([`says_done_outright`]), read outside code as the worker's own words are
/// ([`without_code`]): at the done check's yield the end of the task where
/// the check's turn was short ([`check_answer_ends`]), and the conversation's
/// record reads it the same (`crate::harness::upgrade::FinishedScan`).
/// ALONE: nothing else in the answer names work left or to come — no open
/// work ([`open_work`]: `Remaining: OD-1 phase 2`) and no word of
/// [`AHEAD_WORDS`] ([`names_work_ahead`]: `I'll start it now`, `phase 2 still
/// needs doing`, `two tests still fail`). Read beside such words, the idle
/// line is a step's report and the answer is the work going on, continued
/// as before (the review's DR-1: six short answers that named the next step
/// ended the task at the check). State beside it (`Main is aligned`) and
/// work negated (`nothing is left to land`) leave it alone.
#[must_use]
pub(crate) fn says_idle_report(t: &str) -> bool {
    let flat = done_flat(&without_code(t));
    !says_done_outright(t)
        && says_idle(&flat)
        && !names_work_ahead(&flat)
        && open_work(Some(t)).is_empty()
}

/// The words that, beside an idle line, name work still to come or left
/// ([`names_work_ahead`]): what is next or planned (`I'll`, `will`, `next`,
/// `then`, `now`), work taken up (`starting`, `fixing`, `continuing`), and
/// work left or red (`left`, `remaining`, `still`, `needs`, `pending`,
/// `fail`). Generous on purpose: a miss ends a task whose work goes on, a
/// false find costs the idle report one more `keep going` and check, the
/// rule as it was before an idle report could end the task.
const AHEAD_WORDS: &[&str] = &[
    "i'll",
    "we'll",
    "it'll",
    "will",
    "going",
    "gonna",
    "let",
    "plan",
    "planned",
    "next",
    "then",
    "later",
    "afterwards",
    "tomorrow",
    "now",
    "start",
    "starting",
    "begin",
    "beginning",
    "fix",
    "fixing",
    "continue",
    "continuing",
    "resume",
    "resuming",
    "proceed",
    "proceeding",
    "working",
    "doing",
    "retry",
    "retrying",
    "left",
    "remaining",
    "remains",
    "remain",
    "still",
    "need",
    "needs",
    "needed",
    "pending",
    "todo",
    "to-do",
    "outstanding",
    "open",
    "unfinished",
    "incomplete",
    "fail",
    "fails",
    "failing",
    "failed",
    "broken",
    "blocked",
    "waiting",
];

/// Whether `flat` ([`done_flat`]) says a word of [`AHEAD_WORDS`] that no
/// negation in its clause takes back: among the three words before it, a
/// negation of [`OPEN_NEGATIONS`], `not` or a `…n't` word (`nothing is left
/// to land`, `no tests fail`, `I won't start anything`). A hyphenated word
/// is one word, and an apostrophe is a word's own (`i'll`).
fn names_work_ahead(flat: &str) -> bool {
    let words: Vec<(&str, bool)> = flat
        .split(' ')
        .map(|w| {
            let end = &w[w.trim_end_matches(|c: char| !c.is_alphanumeric()).len()..];
            (
                w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-')),
                end.contains(['.', ',', ':', ';', '!', '?']),
            )
        })
        .collect();
    (0..words.len()).any(|i| {
        AHEAD_WORDS.contains(&words[i].0)
            && !words[..i]
                .iter()
                .rev()
                .take(3)
                .take_while(|&&(_, ends)| !ends)
                .any(|&(w, _)| OPEN_NEGATIONS.contains(&w) || w == "not" || w.ends_with("n't"))
    })
}

/// WHETHER THE ANSWER AT THE DONE CHECK'S YIELD ENDS THE TASK (or at its
/// kin's, the open-items challenge): the worker's own word that it is done
/// ([`says_done_outright`]) unless the record's closed answer says it is
/// none ([`TurnEndReading::check_record`]: a tool's words quoted in code) —
/// or AN IDLE REPORT ALONE ([`says_idle_report`]) as the whole answer of a
/// SHORT turn: `worked`, the check's turn's busy work in all (across a
/// wall's retry that delivered it again), less than `min`
/// ([`TurnEndTiming::min_work`]). Asked to do every open item and otherwise
/// continue, the worker did no real work and said nothing of its own runs:
/// read as progress, a worker that closes every report with its idle line
/// alternated between the check and `keep going` for good (ATERM-KI-1016-1,
/// done-1). A long turn of work that ends on the idle line is the work going
/// on, and is continued (2026-10-05: fifteen hours after the check, `Both
/// branches have landed and no lander is running`, and OD-1 phase 2 came
/// next). Where the record has closed the check's turn its word decides,
/// by the same rule measured by Claude Code's own work (`FinishedScan`: the
/// `durationMs` of each `turn_duration` row since the check, a wall's turn
/// and its retry's summed, so the retry's wait is no work); a turn no read
/// saw run (`worked` `None`) answered nothing of its own.
fn check_answer_ends(reading: &TurnEndReading, worked: Option<Duration>, min: Duration) -> bool {
    let Some(t) = reading.said_tail.as_deref() else {
        return false;
    };
    if says_done_outright(t) {
        return reading.check_record != Some(false);
    }
    says_idle_report(t)
        && reading
            .check_record
            .unwrap_or_else(|| worked.is_some_and(|d| d < min))
}

/// `t` without its code: each line's inline code spans (`` `…` ``, an
/// unpaired tick's tail kept) and every line of a fenced block (between
/// lines opening with ```` ``` ```` or `~~~`). A quoted log line (`` `Build
/// completed successfully in 0:59:34` ``) is a tool's words, never the
/// worker's own (the decision review's RR3-TE-1). The screen draws no ticks;
/// the conversation's record keeps them.
fn without_code(t: &str) -> String {
    let mut fenced = false;
    let mut out: Vec<String> = Vec::new();
    for line in t.lines() {
        let l = line.trim_start();
        if l.starts_with("```") || l.starts_with("~~~") {
            fenced = !fenced;
            out.push(String::new());
            continue;
        }
        if fenced {
            out.push(String::new());
            continue;
        }
        let parts: Vec<&str> = line.split('`').collect();
        let last = parts.len().saturating_sub(1);
        let kept: Vec<&str> = parts
            .iter()
            .enumerate()
            .filter(|&(i, _)| i % 2 == 0 || (last % 2 == 1 && i == last))
            .map(|(_, p)| *p)
            .collect();
        out.push(kept.join(" "));
    }
    out.join("\n")
}

/// A tail cut where a sentence ends with no full stop: before every line
/// that opens a list item or a heading (`-`, `*`, `•`, `#`, `1.`, `1)`, each
/// followed by a space) and at every blank line. A heading or a list line
/// carries no `. `, so read as one text with the next it would make one
/// sentence of both: `All tasks complete` over `- The release workflow is
/// still running in CI` is a finish beside a fact, never a wait (the
/// decision review's RR-DEC-1). But a list the block's own sentence
/// introduces (a line ending `:`) is that sentence's: `All tasks are
/// complete except:` over `- the release workflow, still running in CI`
/// names its wait in its items (the decision review's RR3-TE-3).
fn done_blocks(t: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut open = false;
    let mut introduced = false;
    for line in t.lines() {
        let l = line.trim();
        if l.is_empty() {
            open = false;
            continue;
        }
        if !open || (opens_item(l) && !introduced) {
            blocks.push(String::new());
            open = true;
            introduced = false;
        }
        let b = blocks.last_mut().expect("a block was opened");
        b.push(' ');
        b.push_str(l);
        introduced |= l.ends_with(':');
    }
    blocks
}

/// Whether trimmed line `l` opens a list item or a heading: `-`, `*`, `•`,
/// one or more `#`, or a number and `.` or `)`, each followed by a space.
fn opens_item(l: &str) -> bool {
    let rest = if let Some(r) = l.strip_prefix(['-', '*', '•']) {
        r
    } else if l.starts_with('#') {
        l.trim_start_matches('#')
    } else {
        let digits = l.trim_start_matches(|c: char| c.is_ascii_digit());
        if digits.len() == l.len() {
            return false;
        }
        match digits.strip_prefix(['.', ')']) {
            Some(r) => r,
            None => return false,
        }
    };
    rest.starts_with(char::is_whitespace)
}

/// What says work is still in flight or next, read beside a report that
/// would say done: an idle report beside one anywhere in the last words
/// ([`says_idle`]), a done phrase beside one in its own sentence
/// ([`said_done_phrase`]).
const BUSY: &[&str] = &[
    "still running",
    "still going",
    "still working",
    "still in progress",
    "is running:",
    "are running:",
    "next step",
    "is next",
];

/// Words that, in a done phrase's own sentence, make it a WAIT on work in
/// flight (`I'll stop here until CI reports back`), never the end: whole
/// words ([`WAIT_WORDS`]) and phrases ([`WAIT_PHRASES`], and [`BUSY`]).
const WAIT_WORDS: &[&str] = &["until", "till", "once"];

/// See [`WAIT_WORDS`].
const WAIT_PHRASES: &[&str] = &[
    "will notify",
    "notify me",
    "notifies me",
    "waiting on",
    "waiting for",
    "reports back",
    "report back",
];

/// The [`DONE_PHRASES`] that end a report only as its LAST words: `Phase 2
/// is next. What's next? Starting phase 2.` asks nothing.
const ASKS_NEXT: &[&str] = &["what's next?", "anything else?"];

/// Whether `flat` (one of the tail's [`done_blocks`], read by [`done_flat`];
/// `last` when it is the tail's last) says done phrase `phrase` as the END:
/// not negated just before it ([`negated_before`]), not in a sentence that
/// names a wait or work in flight ([`waits_in`]; the sentence only — a
/// stop beside `The worktree removal is still running in the background.`
/// is a stop), and an [`ASKS_NEXT`] phrase only as the tail's last words.
fn said_done_phrase(flat: &str, phrase: &str, last: bool) -> bool {
    flat.match_indices(phrase).any(|(at, _)| {
        if !whole_words_at(flat, at, phrase) {
            return false;
        }
        if negated_before(flat, at)
            || conditional_at(flat, at, phrase)
            || partial_subject_at(flat, at, phrase)
        {
            return false;
        }
        let (start, end) = sentence_of(flat, at, phrase);
        if waits_in(&flat[start..end]) {
            return false;
        }
        !(ASKS_NEXT.contains(&phrase)
            && (!last || flat[at + phrase.len()..].chars().any(char::is_alphanumeric)))
    })
}

/// Whether `phrase` at `at` in `flat` stands as whole words: no letter or
/// digit just before it, nor just after it where it ends in one (the
/// decision review's RR3-TE-1: `already completed` says no `already
/// complete`, `subtask is complete` no `task is complete`, `install done` no
/// `all done`, `no more worktrees` no `no more work`). A phrase that ends in
/// its own mark (`i've stopped.`, `what's next?`) is read up to it.
fn whole_words_at(flat: &str, at: usize, phrase: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let end = at + phrase.len();
    !word(flat[..at].chars().next_back())
        && !(phrase.ends_with(char::is_alphanumeric) && word(flat[end..].chars().next()))
}

/// The sentence of `flat` the phrase at `at` stands in: from just after the
/// last `. `, `! ` or `? ` before it to the first such end after it (its own
/// closing mark aside), or the text's end. Never split at `;` or `,`
/// (`Everything I can do is queued behind the build, which is still
/// running.`), nor inside `5.9 TiB`.
fn sentence_of(flat: &str, at: usize, phrase: &str) -> (usize, usize) {
    const ENDS: [&str; 3] = [". ", "! ", "? "];
    let start = ENDS
        .iter()
        .filter_map(|e| flat[..at].rfind(e).map(|i| i + e.len()))
        .max()
        .unwrap_or(0);
    let from = at + phrase.trim_end_matches(['.', '?', '!']).len();
    let end = ENDS
        .iter()
        .filter_map(|e| flat[from..].find(e).map(|i| from + i + 1))
        .min()
        .unwrap_or(flat.len());
    (start, end)
}

/// Whether `sentence` names a wait or work in flight ([`WAIT_PHRASES`],
/// [`BUSY`], a [`WAIT_WORDS`] word that waits on work: [`waits_at`]).
fn waits_in(sentence: &str) -> bool {
    if WAIT_PHRASES
        .iter()
        .chain(BUSY)
        .any(|w| sentence.contains(w))
    {
        return true;
    }
    // A hyphenated word is one word (`until the user-space daemon` waits on
    // a daemon, the decision review's RR3-TE-3).
    let words: Vec<&str> = sentence
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    (0..words.len()).any(|i| WAIT_WORDS.contains(&words[i]) && waits_at(&words, i))
}

/// Whether the [`WAIT_WORDS`] word `words[i]` waits on work in flight. Not
/// where a PERSON comes next (`until you review PR #42`, `until you're
/// back`, `until the owner provides the release credential`): that is a
/// stop for someone, the worker's own word that it is done (the decision
/// review's RR-DEC-1), `the owner's` included. Nor `once` as a count (`once
/// more`, `once again`, `at once`, `more than once`). A hyphenated word is
/// one (`until the user-space daemon` waits on a daemon). `your` is a person
/// only before a person's own act, a hyphenated one by its first part
/// (`until your review`, `until your go-ahead`, `until your sign-off`):
/// `until your CI run finishes` waits on work, and a wrong wait costs one
/// continuation under the back-off where a wrong end strands the work.
fn waits_at(words: &[&str], i: usize) -> bool {
    const PERSON: &[&str] = &["you", "you're", "you've", "someone", "somebody"];
    const PERSON_NOUN: &[&str] = &["owner", "user", "person", "owner's", "user's", "person's"];
    const PERSONS_ACT: &[&str] = &[
        "review",
        "approval",
        "go",
        "decision",
        "reply",
        "answer",
        "input",
        "feedback",
        "sign",
        "confirmation",
        "ok",
        "okay",
        "say",
        "permission",
        "instructions",
        "response",
        "call",
    ];
    let word = |j: Option<usize>| j.and_then(|j| words.get(j)).copied().unwrap_or("");
    let (next, next2) = (word(i.checked_add(1)), word(i.checked_add(2)));
    let (prev, prev2) = (word(i.checked_sub(1)), word(i.checked_sub(2)));
    let person = PERSON.contains(&next)
        || (matches!(next, "the" | "a") && PERSON_NOUN.contains(&next2))
        || (next == "your"
            && next2
                .split('-')
                .next()
                .is_some_and(|w| PERSONS_ACT.contains(&w)));
    let count = words[i] == "once"
        && (matches!(next, "more" | "again")
            || prev == "at"
            || (prev == "than" && prev2 == "more"));
    !(person || count)
}

/// Whether `flat` ([`done_flat`]) says `phrase` with no negation
/// ([`negated_before`]): `did not fail again; all done` says it, `not yet
/// finished and verified` does not.
fn said_unnegated(flat: &str, phrase: &str) -> bool {
    flat.match_indices(phrase)
        .any(|(at, _)| !negated_before(flat, at))
}

/// Whether a negation (`not`, `cannot`, a `…n't` word) stands among the two
/// words before `at` in `flat`, in the same clause.
fn negated_before(flat: &str, at: usize) -> bool {
    flat[..at]
        .split(' ')
        .rev()
        .filter(|w| !w.is_empty())
        .take(2)
        .take_while(|w| !w.ends_with(['.', ',', ':', ';', '!', '?']))
        .any(|w| w == "not" || w == "cannot" || w.ends_with("n't"))
}

/// A tail as the done readers read it: lowercased, `’` as `'`, one space
/// between words.
fn done_flat(t: &str) -> String {
    t.to_lowercase()
        .replace('’', "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the worker's last words (`flat`: lowercased, one space between
/// words, as [`says_done`] reads them) say nothing of its own is in flight:
/// `no <word> is` (or `are`) `running` (or `queued`) — `No lander is
/// running, so I'm not re-arming the watch` — and `nothing (else) is
/// queued`, each inside one clause (`Did it pass? No. CI is running again`
/// and `No, it is running the last suite` are an answer and a step), and an
/// empty queue in words (`All queued work has landed`: 2026-10-04 02:08, it
/// was followed by six hours of work). Not where they go on to name what
/// still runs (`still running`, `still going`, `still working`, `still in
/// progress`, a list under `… is running:`) or what comes next (`OD-1 phase
/// 1 is the next step`): that is a wait on work in flight, or work ahead,
/// continued as any (2026-10-07: `nothing is queued to land` over a `Still
/// running:` list of agents). Nor where they
/// stop for a person ([`STOP_PHRASES`]: `What's left needs your decisions:
/// …`): that is an ask, answered as the module header rules (D1), not a done
/// report. An idle report is a FIRST done report: at the check it ends the
/// task only as the whole answer of a short turn ([`check_answer_ends`]),
/// never as the end of a turn of real work (2026-10-05: the landing reports
/// `OD-1 phase 1 has landed and no lander is running` and, fifteen hours on,
/// `Both branches have landed and no lander is running` came before OD-1
/// phase 2).
fn says_idle(flat: &str) -> bool {
    const EMPTY_QUEUE: &[&str] = &["all queued work has landed", "all queued work is landed"];
    if BUSY.iter().chain(STOP_PHRASES).any(|b| flat.contains(b)) {
        return false;
    }
    if EMPTY_QUEUE.iter().any(|p| said_unnegated(flat, p)) {
        return true;
    }
    // Each word, and whether a clause ends with it (`no.`, `no,`).
    let words: Vec<(&str, bool)> = flat
        .split(' ')
        .map(|w| {
            let end = &w[w.trim_end_matches(|c: char| !c.is_alphanumeric()).len()..];
            (
                w.trim_matches(|c: char| !c.is_alphanumeric()),
                end.contains(['.', ',', ':', ';', '!', '?']),
            )
        })
        .collect();
    let one_clause = |w: &[(&str, bool)]| w[..w.len() - 1].iter().all(|&(_, ends)| !ends);
    let be = |w: &str| matches!(w, "is" | "are");
    let idle = |w: &str| matches!(w, "running" | "queued");
    let none_of = |w: &[(&str, bool)]| w[0].0 == "no" || (w[0].0 == "nothing" && w[1].0 == "else");
    // `no <word> is running`, `nothing else is queued`…
    let of_one = words
        .windows(4)
        .any(|w| one_clause(w) && none_of(w) && be(w[2].0) && idle(w[3].0));
    // …and `nothing is running`.
    of_one
        || words
            .windows(3)
            .any(|w| one_clause(w) && w[0].0 == "nothing" && be(w[1].0) && idle(w[2].0))
}

/// The rules whose act's yield counts toward the short streak: an answer
/// under any priority among them ([`is_answer_rule`]), and Land's three acts
/// ([`is_land_rule`]: each is the continuation, the done check or the answer
/// it stands in for, so a worker that answers them short is backed off as it
/// would be there) — never [`RULE_HOST_TURN`], the harness's own turn.
pub(crate) fn is_continue_rule(rule: &str) -> bool {
    rule == RULE_CONTINUE
        || rule == RULE_SUGGESTION
        || is_answer_rule(rule)
        || rule == RULE_DONE_CHECK
        || is_challenge_rule(rule)
        || is_land_rule(rule)
}

/// Whether `rule` answers a question the worker asked in prose — under any
/// [`QuestionPriority`] ([`QuestionPriority::prose_rule_id`]: [`RULE_ANSWER`],
/// `prose-long-term@v1`, `prose-short-term@v1`).
#[must_use]
pub fn is_answer_rule(rule: &str) -> bool {
    QuestionPriority::ALL
        .into_iter()
        .any(|p| p.prose_rule_id() == rule)
}

/// Claude Code's own `/model` aliases (2.1.280's binary: `["sonnet", "opus",
/// "haiku", "fable"]`): the only names a switch back is typed with.
pub const MODEL_ALIASES: &[&str] = &["sonnet", "opus", "haiku", "fable"];

/// Pieces a suggestion may be made of to be accepted as a continuation,
/// joined by `,`, `;`, `.`, ` and ` or ` then `.
pub const SUGGESTION_ALLOW: &[&str] = &[
    "keep going",
    "continue",
    "keep fixing forward",
    "push it when green",
    "go on",
    "carry on",
    "keep at it",
    "proceed",
    "go ahead",
];

/// Phrases a worker asks a person for a decision with: answered
/// ([`RULE_ANSWER`]), never continued; [`REVERSIBLE_ANSWER`] when a
/// destructive act ([`DESTRUCTIVE_WORDS`]) is named. Lowercased, anywhere.
pub const STOP_PHRASES: &[&str] = &[
    "need your decision",
    "needs your decision",
    "need a decision",
    "your decision on",
    "blocked on",
    "i'm blocked",
    "i am blocked",
    "waiting on you",
    "waiting for you",
    "waiting on your",
    "waiting for your",
    // Names the person's authority rather than listing options: a stop, not
    // a choice ([`CHOICE_PHRASES`]).
    "your call",
    "need your input",
    "need your approval",
    "need you to",
    "please confirm",
    "can you confirm",
    "please advise",
    "how would you like me to proceed",
    "cannot proceed without",
    "can't proceed without",
    // The go-ahead, sign-off and review asks, and the
    // ways a worker says it will wait for one.
    "your go-ahead",
    "your go ahead",
    "the go-ahead",
    "a go-ahead",
    "sign off",
    "sign-off",
    "signoff",
    "for your review",
    "ready for review",
    "ready for your review",
    "please review",
    "your approval",
    "needs approval",
    "need approval",
    "how you'd like",
    "how you would like",
    "how would you like",
    "what you'd like me to",
    "let me know how",
    "let me know whether",
    "i'll wait",
    "i will wait",
    "i'll hold off",
    "holding off until",
    "before i proceed",
    "before i continue",
    "before i go ahead",
    "before i run",
    "before i push",
    "before i force",
    "before i delete",
    "before i merge",
    "before i deploy",
    "before i release",
];

/// Phrases a worker asks a person to choose with: a stop, answered
/// ([`RULE_ANSWER`]), only where the phrase asks ([`phrase_asks`]), never in
/// a report; with the act named when a destructive word follows. Matched as
/// [`STOP_PHRASES`] are.
pub const CHOICE_PHRASES: &[&str] = &[
    "which option",
    "which would you prefer",
    "which do you prefer",
    "let me know which",
];

/// Words that make an offer, stop, choice or question one a person must
/// answer: answered with [`REVERSIBLE_ANSWER`], never continued (`keep going`
/// would read as consent). Whole words after the offer's phrase. Plain `push`
/// is excluded: `push it when green` is an accepted continuation. Force-push
/// spellings include `--force`, `force-push`, a short `-f` option after
/// `push` (reordered or clustered, as in `push origin main -vf`), and
/// `--force-with-lease`, which overwrites the remote ref as `--force` does
/// when the lease holds.
pub const DESTRUCTIVE_WORDS: &[&str] = &[
    "force",
    "force-push",
    "--force",
    "push -f",
    "force-with-lease",
    "delete",
    "deleting",
    "deletion",
    "drop",
    "dropping",
    "remove",
    "removing",
    "removal",
    "rm",
    "deploy",
    "deploying",
    "migrate",
    "migrating",
    "publish",
    "publishing",
    "release",
    "releasing",
    "wipe",
    "purge",
    "reset",
    "overwrite",
    "truncate",
    "destroy",
    "uninstall",
    "revert",
    "rewrite history",
];

/// Words that make a question an offer to go on (it counts as a
/// continuation): `want me to take that on?`.
pub const OFFER_PHRASES: &[&str] = &[
    "want me to",
    "shall i",
    "should i continue",
    "should i proceed",
    "should i keep going",
    "should i go ahead",
    // An offer of more work, not a yes/no about the work done.
    "should i also",
    "would you like me to",
    "next steps:",
    "next step:",
    "i can also",
    "if you'd like, i can",
    "let me know if you'd like",
    "happy to",
];

/// Words that recommend one option: a choice with a recommendation is the
/// worker's to take.
pub const RECOMMEND_PHRASES: &[&str] = &[
    "i recommend",
    "i'd recommend",
    "i would recommend",
    "recommended",
    "my recommendation",
    "i suggest",
    "i'd suggest",
    "i'd go with",
    "i'll go with",
    "i'd pick",
    "i lean",
];

/// The busy work that makes a turn real work by default
/// ([`TurnEndTiming::min_work`]): what the conversation's record measures a
/// done check's turn by too (`crate::harness::upgrade::FinishedScan`,
/// [`check_answer_ends`]).
pub const MIN_WORK: Duration = Duration::from_secs(2 * 60);

/// The policy's timings — the defaults are the plan's; tests shorten them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndTiming {
    /// The busy work that makes a turn real work: a turn that did less is
    /// short, and backs the next act off ([`Self::short_backoff`]).
    pub min_work: Duration,
    /// The wait, from the point, before the act after ONE short turn; each
    /// short turn more in a row doubles it, up to [`Self::short_backoff_max`].
    pub short_backoff: Duration,
    pub short_backoff_max: Duration,
    /// The wait before each retry of the server's own failure (a status,
    /// an overload), counted from the wall's appearance; the last is the
    /// wait before every retry after it.
    pub retry_backoff: Vec<Duration>,
    /// The same for an API never reached, a refused certificate or proxy, and
    /// a cut-off reply while reach is not measured up: short, since a blind
    /// try costs only the vendor's own retries.
    pub net_backoff: Vec<Duration>,
    /// How long nothing is typed into an API measured down before one try
    /// anyway: the most a wrong measure holds a worker.
    pub down_hold: Duration,
    /// Busy work between a wall's act and its next appearance that ends the
    /// episode (longer than the vendor's own retries); a later wall starts a
    /// new ladder.
    pub progress: Duration,
    /// How long past a usage window's reset the continuation waits: 150 s,
    /// past the vendor's own auto-continue (Claude Code arms its resume at
    /// the reset plus 30-90 s and checks every 30 s — measured +58 s and
    /// +105 s in the owner's history), so a wall whose armed notice this
    /// build cannot read (2.1.284 draws it as two notifications whose layout
    /// is unmeasured) is still not answered twice: the continuation is the
    /// probe, sent only if the wall still stands then.
    pub reset_grace: Duration,
    /// Up to this much more, per SESSION — the loop derives
    /// it from the session and the reset ([`TurnEndReading::jitter`]) — so
    /// the sessions of one account, whose walls all lift at the same
    /// instant, do not all type at the same second (Claude Code spreads its
    /// own auto-continue over 30-90 s).
    pub reset_jitter: Duration,
    /// How long past the time a notice that goes on BY ITSELF named
    /// (`continuing automatically at 1:50pm`) the vendor is given before the
    /// policy continues the worker itself: the vendor's own retry is waited
    /// for, never doubled — this is the net under it.
    pub auto_resume_grace: Duration,
    /// After a continuation at a usage wall that hit the wall again: the
    /// next is this far off (the last entry for every one after).
    pub limit_backoff: Vec<Duration>,
    /// The same for a wall no reset ends and
    /// none names — a model bucket that "requires usage credits", a spend or
    /// credit limit, an account the admin switched off. Such a wall is an
    /// entitlement, not a window: a `keep going` every half hour at one that
    /// lasts days was about 48 blind messages a day (each a turn of its own
    /// and a synthetic error row). The first try waits the first entry from
    /// the appearance, each later one the entry after the last try, the last
    /// entry for every one after it: the safety net for an account whose
    /// owner added credits and touched nothing else.
    pub entitlement_backoff: Vec<Duration>,
    /// The waits before each retry of a REFUSED REQUEST
    /// ([`aterm_phase::ApiCause::Request`]: a 4xx the vendor does not retry, a
    /// safeguards refusal, a model not found), counted from the wall's
    /// appearance; the last is the wait before every retry after it. One
    /// cheap try in case it was a race the next request repairs, then the
    /// entitlement's slow pace: sent again unchanged it is refused again, and
    /// only a person's change ends it (a person is told once,
    /// `policy/exception.rs`).
    pub refused_backoff: Vec<Duration>,
    /// The window the typing budget counts over.
    pub window: Duration,
    /// How long an act's point may show with no busy read before it is
    /// judged anyway ([`TurnEndState::observe`]): a reply inside the `turn`
    /// verb's settle, or an Enter that did not take, is never seen busy.
    pub take_within: Duration,
    /// Pauses between tries of a restart the host cannot make yet
    /// ([`TurnEndAction::Restart`]); the last repeats. Never given up.
    pub restart_backoff: Vec<Duration>,
    /// How long the CREDITS HOLD stands where no reset is named, counted from
    /// the episode's start ([`credits_hold`]): the five-hour window credits
    /// carry an account past, however late in it they started — then one act
    /// goes as the probe. Unbounded, the hold never lifted (the review of
    /// 2026-10-19, P6): the notice is the newest answer's, and only a new
    /// request clears it.
    pub credits_bound: Duration,
}

/// The longest grace past a reset that a program with no auto-continue of its
/// own waits ([`TurnEndTiming::grace_for`]).
const NO_AUTO_CONTINUE_GRACE: Duration = Duration::from_secs(60);

impl TurnEndTiming {
    /// The grace past a wall's reset for the program the wall is in. Claude
    /// Code arms its own resume 30 to 90 seconds after a reset and checks
    /// every 30, so its first continuation stays past that window
    /// ([`Self::reset_grace`], 150 s). A program with no auto-continue has
    /// none to stay clear of: the grace is capped there.
    #[must_use]
    pub fn grace_for(&self, program: Program) -> Duration {
        if auto_continues(program) {
            self.reset_grace
        } else {
            self.reset_grace.min(NO_AUTO_CONTINUE_GRACE)
        }
    }
}

/// Whether `program` goes on by itself past a limit's reset — its own
/// auto-continue, armed with a countdown it draws
/// ([`aterm_phase::AutoContinue::Armed`]): Claude Code's. Codex has none,
/// and a program no reader identifies is read for none.
fn auto_continues(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => false,
    }
}

impl Default for TurnEndTiming {
    fn default() -> Self {
        let min = |m: u64| Duration::from_secs(m * 60);
        Self {
            min_work: MIN_WORK,
            short_backoff: min(2),
            short_backoff_max: min(60),
            retry_backoff: vec![min(1), min(5), min(15), min(30), min(60)],
            net_backoff: vec![min(1), min(2), min(5)],
            down_hold: min(15),
            progress: min(5),
            reset_grace: Duration::from_secs(150),
            reset_jitter: Duration::from_secs(60),
            auto_resume_grace: min(10),
            limit_backoff: vec![min(10), min(30)],
            entitlement_backoff: vec![min(30), min(120), min(360)],
            refused_backoff: vec![min(1), min(30), min(120), min(360)],
            window: min(60),
            take_within: Duration::from_secs(30),
            restart_backoff: vec![Duration::from_secs(10), min(1), min(5), min(10)],
            credits_bound: min(300),
        }
    }
}

/// The composer as the reading found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composer {
    /// No composer frame on the screen (a dialog, a shell).
    Absent,
    /// Text stands where the placeholder may be, but its cell style could not be read;
    /// or a draft stands over Claude Code's completion menu whose selection is drawn in
    /// colour only and a cell of it was not read, so that Enter may send it or not
    /// ([`aterm_phase::phase::CompletionMenu::runs`]). This is no free point and no
    /// draft to submit; the next screen's read decides again.
    Unknown,
    Empty,
    /// The worker's own suggestion, drawn DIM with nothing typed.
    Placeholder(String),
    /// Text is typed: a draft, never typed over — submitted as it stands
    /// once nobody has touched it for the grace ([`TurnEndAction::Submit`]).
    Typed,
    /// Aterm's own words are in the composer: the upgrade's notice, whole or
    /// partial ([`crate::harness::upgrade::own_draft`]), left when a press
    /// stopped part-way. Never typed over and never sent; the lane never takes
    /// it back (a person's key may be in flight), so the point is escalated
    /// once ([`OWN_WORDS_STAND`]).
    Own,
    /// A draft stands over Claude Code's open completion menu, whose
    /// selected entry is not the draft's command, or whose every entry Enter
    /// may run is not ([`aterm_phase::phase::CompletionMenu::runs`]): Enter would run that entry
    /// (`/clear` for a person's `/cl` left standing), not what the composer
    /// shows; or over the `@` menu with an entry selected, which Enter would
    /// complete into the draft, sending nothing; or over a menu whose
    /// selection the screen itself leaves open, every cell it is read by read
    /// (an `@` menu of one entry, a `/` menu of two in two colours: 2.1.289
    /// and before), where Enter may do either. Never sent and never typed
    /// over; the point is escalated once ([`MENU_DIVERTS`]).
    Diverted,
}

impl Composer {
    /// Whether text stands in the composer: a draft, or aterm's own words.
    #[must_use]
    pub fn drafted(&self) -> bool {
        matches!(self, Composer::Typed | Composer::Own | Composer::Diverted)
    }
}

/// Escalation reason while aterm's own words stand unsent in the prompt
/// ([`Composer::Own`]): the worker is not continued until a person clears
/// them.
pub const OWN_WORDS_STAND: &str =
    "aterm's own words stand unsent in the prompt; clear them and the session goes on";

/// Escalation reason while a draft stands over Claude Code's open completion
/// menu ([`Composer::Diverted`]): Enter must send what the composer shows, and
/// there it would (or, where the screen does not tell the selection, may) run
/// the menu's highlighted entry, or complete it.
pub const MENU_DIVERTS: &str = "a draft stands with Claude Code's completion menu open: Enter \
     may run or complete the menu's highlighted entry, not send the draft; send or clear it \
     and the session goes on";

/// Suffix of an escalated question's reason when `answer_questions` holds it
/// (`<why>; answer_questions is off`). The loop then reads the session's own
/// `questions` word, whose priority may answer it.
pub const ANSWERS_OFF: &str = "answer_questions is off";

/// What the loop read of the composer beyond its rows, for
/// [`TurnEndReading::of`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerRead {
    /// Whether the composer's text was typed: `Some(false)` for an empty
    /// composer or the worker's placeholder, `None` when its cell could not
    /// be read (`Session::composer_draft`).
    pub typed: Option<bool>,
    /// The colour of each entry's first glyph of a completion menu that
    /// draws its selection in colour only
    /// ([`aterm_phase::phase::CompletionMenu::colour_only`]), in entry order,
    /// `None` for one the `cell` did not read; `None` when none was read (a
    /// pure reader: every entry of such a menu may be the one Enter runs).
    pub menu_fg: Option<Vec<Option<String>>>,
}

/// USAGE CREDITS CARRY THE SESSION, by Claude Code's own notice
/// ([`TurnEndReading::credits`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreditsCarry {
    /// The reset of the window they carry past, on the loop's clock: named by
    /// the notice they started on (`You're now using usage credits · Your
    /// session limit resets 9:43am (…)`, or a model's `Now using usage
    /// credits for Opus · Your Opus limit resets 4pm`), drawn for some ten
    /// seconds as they begin — mostly under the running turn — and kept by
    /// the loop from the read that saw it until that reset has come, whatever
    /// the reads since draw (`Session::note_credits`). `None`: none named or
    /// none read (the lasting `Now using usage credits` alone; a pure
    /// reading, [`TurnEndReading::of`]).
    pub reset_at: Option<Instant>,
    /// The loop's first read of the episode's notice, on its clock — the
    /// running turn's, mostly ([`TurnEndState::fold_credits`] counts the hold
    /// with no reset named from it). `None`: the point's own (a pure
    /// reading).
    pub since: Option<Instant>,
    /// The MODEL whose requests they carry, as a word of its family
    /// (`fable`, [`credits_model`]): a per-model notice (`Fable 5.1 is drawing
    /// from usage credits`, `Now using usage credits for Fable · …`,
    /// [`aterm_phase::credits::Credits::model`]). `None`: the account's
    /// window.
    pub model: Option<String>,
    /// THE SCREEN NO LONGER DRAWS THE NOTICE: this is the loop's memory of
    /// one the process drew — a reset still ahead (Claude Code draws Team and
    /// Enterprise accounts no lasting notice: their started notice stands
    /// some ten seconds, and a member with no billing access sees none), or
    /// a model's (its `is drawing from usage credits` is drawn once a
    /// process). `false`: drawn on this read.
    pub kept: bool,
}

/// The word of a model's family a credits notice names ([`CreditsCarry::model`]):
/// the first word of its name, lowercased (`Fable 5.1` → `fable`).
#[must_use]
pub fn credits_model(name: &str) -> Option<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .find(|w| !w.is_empty())
        .map(str::to_lowercase)
}

impl From<Option<bool>> for ComposerRead {
    fn from(typed: Option<bool>) -> Self {
        Self {
            typed,
            menu_fg: None,
        }
    }
}

impl From<bool> for ComposerRead {
    fn from(typed: bool) -> Self {
        Some(typed).into()
    }
}

/// What one screen says, as the turn-end policy reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndReading {
    pub phase: Phase,
    /// The reader's phase is evidence ([`aterm_phase::Reading::phase_authoritative`]).
    pub authoritative: bool,
    pub survey: bool,
    /// The wall the turn ended on.
    pub wall: Option<WallKind>,
    /// The wall's notice as said (for the model a bucket names, and the
    /// escalations a person reads). SCREEN TEXT: never typed into the worker
    /// ([`wall_retry_text`]).
    pub wall_message: String,
    /// The wall's reset as the loop's clock (the loop resolves the notice's
    /// `reset=` text with its zone and clock).
    pub reset_at: Option<Instant>,
    /// The wall's reset as its notice's clock names it (the reader's `reset=`
    /// text, [`ResetClock`]): what ties a notice the screen draws later to
    /// this limit ([`Self::since_limit`]).
    pub reset_clock: Option<ResetClock>,
    /// This session's share of
    /// [`TurnEndTiming::reset_jitter`] for this reset, on top of
    /// [`TurnEndTiming::reset_grace`] (zero: from the screen, in tests).
    pub jitter: Duration,
    /// The notice says Claude Code goes on by itself (`continuing
    /// automatically at …`) — and is no transcript `⏺` row with no
    /// countdown drawn under the composer ([`aterm_phase::AutoContinue::Armed`]):
    /// that is the countdown's record in a redrawn history.
    pub resumes_by_itself: bool,
    pub composer: Composer,
    /// The worker's last words ([`aterm_phase::said_tail`]).
    pub said_tail: Option<String>,
    /// For [`TurnEndState::observe`] only: the busy work since the previous
    /// point, `None` when no read saw the worker busy since (the same point
    /// read again).
    pub worked: Option<Duration>,
    /// How much longer than [`Self::worked`] the turn may have worked: the
    /// sleep of the machine its done row's span may hold but need not
    /// (`policy::sleeps::InTurn`, its most less its least, as far as it
    /// moves the reading: [`Self::of_slept`]). `worked` is the turn at its
    /// least, for the work it is judged by; whether a person's keystroke is
    /// inside it reads it at its most ([`Self::person_in_turn`]). Zero with
    /// no sleep in doubt.
    pub worked_doubt: Duration,
    /// The standing rules, one line, when a rules file is set and readable.
    pub rules: Option<String>,
    /// The last row is a user's `❯` row not yet answered (a continuation just
    /// submitted, before its spinner): no turn has ended.
    pub pending_input: bool,
    /// A BUSY screen whose turn is OVER, kept busy only by a shell or a
    /// monitor it left running — no live turn drawn, and no turn still
    /// waiting on a workflow or a background agent it started, which goes on
    /// by itself (Claude Code: [`aterm_phase::turn_over_background`]; another
    /// program: its reader's
    /// [`aterm_phase::ScreenReader::background_wait`]). The worker's
    /// [`FINISHED`] read there ends its task
    /// ([`TurnEndState::finished_over_background`]); nothing else is decided
    /// on it.
    pub background: bool,
    /// A person stopped the turn with Esc ([`aterm_phase::interrupted`]):
    /// nothing is typed here — the next message is theirs.
    pub interrupted: bool,
    /// A host can restart the agent here
    /// ([`crate::supervise::IdleHost::can_restart`]). Set by the loop.
    pub restartable: bool,
    /// The command a person runs to resume the agent's own conversation
    /// ([`crate::supervise::IdleHost::resume_command`], `claude --resume
    /// <id>`), named by the memory wall's escalation. `None` names no command
    /// (`claude --continue` would pick the directory's newest conversation).
    /// Set by the loop.
    pub resume: Option<String>,
    /// The live upgrade owns the session's turn ends
    /// ([`crate::supervise::IdleHost::owns_turn_end`]: the agent answered READY
    /// and the restart is imminent), read from the upgrade's state, never the
    /// screen: nothing is typed. Never set during the loop's limit episode; a
    /// wall is still decided by its own rule, with only the memory banner
    /// asking for a relaunch ([`decide_turn_end`]).
    pub upgrading: bool,
    /// No task yet: nobody but the harness has asked the session anything
    /// (the launch card and no message, [`aterm_phase::Reading::fresh`], or the
    /// host's [`crate::supervise::IdleHost::taskless`]). Nothing is typed.
    pub taskless: bool,
    /// WHAT THE CONVERSATION'S OWN RECORD SAYS OF ITS TASK
    /// ([`crate::supervise::IdleHost::finished`],
    /// `crate::harness::upgrade::FinishedScan`), set by the loop at every
    /// point a turn ran (the vendor's own usage-limit continuation included)
    /// and at a fresh state's first point: `Some(true)`, the worker ended it
    /// and nobody has asked anything since, ends the task whatever the
    /// screen's last words are ([`TurnEndState::observe`]) — a turn Claude
    /// Code woke itself for, a relaunched agent's start-up, a token said
    /// under a running shell, a new loop — a wall that turn ended on
    /// included: its ladder owes a wake nobody asked for no retry. Not under
    /// an unanswered prompt. `Some(false)` and `None`
    /// (no record: a Codex, none written yet) leave the screen's rules.
    pub finished_record: Option<bool>,
    /// THAT FINISH IS A DONE REPLY IN WORDS TO THE DONE CHECK, not the
    /// worker's [`FINISHED`] ([`crate::supervise::IdleHost::finished_at_check`]),
    /// read beside [`Self::finished_record`]: the task ends the same, and
    /// [`TurnEndState::finished_by_worker`] stays the token's alone — the
    /// journal's `finished@v1` says the worker reported [`FINISHED`], the
    /// check's end is [`RULE_DONE_CHECK`]'s (F2 of the live lab of
    /// 2026-10-16). `false`: a finish read as the token's, as before.
    pub finished_at_check: bool,
    /// WHAT THE DONE CHECK'S CLOSED ANSWER SAYS BY THE RECORD
    /// ([`crate::supervise::IdleHost::check_answer`]), read beside
    /// [`Self::finished_record`]: `Some(false)` where the check's turn is
    /// closed in the record and its last words are no done word of the
    /// worker's own — a tool's words quoted in inline code or a fenced block,
    /// which the screen draws as prose. At the check's yield it overrules the
    /// screen's done word ([`TurnEndState::observe`]; the review's RR4-TE-1:
    /// the task ended while a push or an audit still ran). `None` — no record,
    /// no check, or its turn not closed yet (the record lags the screen by a
    /// flush) — leaves the screen's word.
    pub check_record: Option<bool>,
    /// A PERSON'S OWN EXCHANGE AN EARLIER LOOP LEFT TO THEM is still the
    /// conversation's last turn (the review's RR3-TE-2): the approval
    /// ledger's open [`RULE_LEFT`] row — this tab's, or an earlier tab's a
    /// cold restore noted — names the turn the conversation ends on now
    /// (`crate::harness::upgrade::last_turn_key`; the screen's last words
    /// where no transcript is read), and no act of the loop's was typed
    /// since. Read where [`TurnEndState::reads_record`] says the verdict can
    /// move: a new loop's first point (a seamless update, a new window, a
    /// fault, the window's host starting one as the session's Autopilot Off
    /// lifts), a relaunched agent's start-up. `true` keeps the exchange the
    /// person's ([`TurnEndState::observe`]), as the loop that left it did:
    /// the row is that loop's judgement of a turn it SAW run, where a first
    /// point's own read never says whose turn it was (a turn of the person's
    /// under Off is another turn, and matches no row).
    /// AT A WALL, the row an earlier loop wrote where the person's exchange
    /// met that wall ([`TurnEndState::exchange_at_wall`], RR4-TE-2; its key
    /// marked as the wall's, so it is read at a wall alone): its ladder runs,
    /// and the retry's answer is theirs while the exchange's work stays
    /// within [`EXCHANGE_WORK`].
    pub left_record: bool,
    /// THE WORK THAT WALL'S ROW CARRIES ([`Self::left_record`] at a wall):
    /// the person's exchange's work in all when the earlier loop wrote it
    /// ([`TurnEndState::exchange_work`]), which the retry's answer adds to,
    /// as in the loop that saw the turn (main's 7fffc8156: a wall splits a
    /// job, it never makes it an exchange; the merge review's F3). `None`:
    /// no such row, or one written before the work was carried — the retry's
    /// answer measured alone, as before.
    pub left_work: Option<Duration>,
    /// The screen says the login is back ([`aterm_phase::login_restored`]),
    /// which lifts the login hold ([`decide_turn_end`]).
    pub login_back: bool,
    /// How Claude Code's own auto-continue of a usage limit ended, by the
    /// vendor's word under it ([`aterm_phase::auto_continue_end`]): read at a
    /// point the limit's notice has left ([`TurnEndState::observe`]). Its
    /// `Fired`, the footer's notification or the transcript's row, ends a
    /// wait only under the notice of the limit waited on
    /// ([`Self::since_limit`]): one under an older limit's notice is a
    /// `/resume`d conversation's history (W1 of ATERM-KI-1015-1). Its `Armed`,
    /// a countdown still drawn, is what says the vendor still goes on by
    /// itself where the notice has left (W4).
    pub auto_continue: Option<aterm_phase::AutoContinue>,
    /// THE SCREEN'S NEWEST USAGE LIMIT NOTICE and whether a turn was served
    /// since it ([`aterm_phase::since_limit`]: a person's prompt below it with
    /// the model's reply under the prompt). Read at a point the limit's notice
    /// has left, where no read saw the worker busy ([`TurnEndState::observe`]):
    /// a served turn there under THE NOTICE OF THE LIMIT WAITED ON — the
    /// reset it names is that limit's ([`ResetClock`]) — is the
    /// person's work at the tab, and ends the wait as one seen busy does; so
    /// is the vendor's own continuation under it ([`Self::auto_continue`]'s
    /// `Fired`). Under another limit's notice (a `/resume`d conversation's
    /// history) neither says anything of this one, nor where the
    /// conversation holds no row of this limit's notice
    /// ([`Self::notice_elsewhere`]).
    pub since_limit: Option<aterm_phase::SinceLimit>,
    /// A TURN WAS SERVED SINCE THE LIMIT'S OWN NOTICE ROW, by the session's
    /// transcript — set by the loop at a decision's read where the screen
    /// draws no limit's notice to order the turn by ([`Self::since_limit`]
    /// `None`) and the host hands it the transcript
    /// (`supervise/wall_confirm.rs`, [`TurnEndState::observe`]).
    pub served_in_transcript: bool,
    /// THE SCREEN'S LIMIT NOTICE IS ANOTHER CONVERSATION'S: the transcript of
    /// the conversation the tab holds now has no row of THIS limit's notice
    /// (`Session::limit_notice_row`, the row that confirmed it) — set by the
    /// loop at a decision's read where a served turn or the vendor's
    /// continuation under the screen's newest limit notice
    /// ([`Self::since_limit`]) would end the wait on the reset it names
    /// (`supervise/wall_confirm.rs`). The reset's clock names no date, so a
    /// `/resume`d conversation's older notice naming the same clock read as
    /// this limit's, and the continuation was typed into the standing limit
    /// (the residual review's R2). `false` with no host, no transcript or no
    /// row kept: the clock tie decides, as before.
    pub notice_elsewhere: bool,
    /// THE WALL IS THE LOOP'S MEMORY OF A LIMIT'S NOTICE, not a reading of
    /// the screen: the screen read busy with only the agent's own background
    /// work (a shell the worker started, still running), and the limit the
    /// loop waits on stood in for it (`supervise/wall_memory.rs`), kept by
    /// [`TurnEndState::read_remembered_wall`]. Where the screen no longer
    /// draws a limit's notice ([`Self::since_limit`] `None`) that wall is the
    /// loop's wait alone, and the words under it are read as an idle
    /// screen's ([`Self::wall_drawn`]).
    pub wall_remembered: bool,
    /// The host's measure of the API's reach, set by the loop at an
    /// unreachable or cut-off wall; [`Reach::Unknown`] from the screen.
    pub reach: Reach,
    /// Time since a person last typed into the session through its window
    /// (`status human_ms=`; control-socket writes excluded) or the composer's
    /// draft last changed, whichever is later. `None`: no person.
    pub person: Option<Duration>,
    /// The program whose reader read the screen.
    pub program: Program,
    /// WHAT THE HOST ADVISES ABOUT THE SESSION'S ACCOUNT at this point
    /// ([`AccountAdvice`]; the loop's to set, asked of its host ONCE a
    /// decision: `crate::supervise::IdleHost::account`). `None` — no roster,
    /// no host that keeps one, or "stay": every decision is the one made
    /// before accounts existed. What is decided on is [`Self::advised`]: a
    /// due probe's `otherwise`, never the probe.
    pub account: Option<AccountAdvice>,
    /// THE SESSION'S AUTOPILOT ([`Autopilot`]): its own `autopilot` word, else
    /// the table's `[harness] autopilot`
    /// ([`SupervisorConfig::autopilot_for`]) — the loop's to set, never read
    /// off the screen ([`Autopilot::Cruise`] from [`Self::of`]). `Off`: nothing
    /// is decided ([`decide_turn_end`]); `Land`: the continue policy lands the
    /// work ([`LAND`]); a reading in any other mode than Land ends the Land
    /// episode's memory ([`TurnEndState::observe`], [`TurnEndState::acted`]).
    pub autopilot: Autopilot,
    /// THE RENEWABLE LOGIN THIS SESSION RUNS ON: the roster label of a row
    /// with an email that the session's process's login is — its renewed
    /// one, or one a renewal superseded
    /// (`crate::harness::accounts::Me::label`) — set by the loop from its
    /// host (`crate::supervise::IdleHost::login_label`). `None`: no such
    /// row, no host that keeps a roster, or `[harness] logins = "off"`. A
    /// lost login on it is signed in where it stands, as any other (`/login`
    /// once, the owner's notice naming the row: [`super::notice::
    /// LOGIN_SIGN_IN_AS`]); only one a renewal already replaced
    /// ([`Self::login_superseded`]) is waited at, for its relaunch onto the
    /// renewed login ([`login_wall_waits`], [`login_wall_moves`]).
    pub login_label: Option<String>,
    /// THE SESSION'S PROCESS HOLDS A LOGIN A RENEWAL REPLACED: its row
    /// ([`Self::login_label`]) is bound to the renewed login, and this
    /// process is still on the one before it
    /// (`crate::harness::accounts::Me::left`) — set by the loop from its host
    /// (`crate::supervise::IdleHost::login_superseded`), asked only at a lost
    /// login on the screen of a session on a renewable login. No renewal
    /// reaches such a process: only its relaunch does, so where the wall is
    /// not waited at, what the owner is told is what holds that relaunch
    /// ([`renew_step`]), never the renewal again.
    pub login_superseded: bool,
    /// THE DAYS CLAUDE CODE SAYS THE STORED LOGIN HAS LEFT, off the screen
    /// ([`aterm_phase::login_expires_in`]): the loop hands it to its host for
    /// the login's forecast (`crate::supervise::IdleHost::login_notice`).
    /// Read on Claude Code only; nothing here decides on it.
    pub login_notice: Option<u32>,
    /// CLAUDE CODE'S `/login` DIALOG, the state it stands in
    /// ([`aterm_phase::login_screen`]): what [`super::login::dialog_act`]
    /// answers at a turn end, ahead of every gate a typed act waits on — its
    /// one key is the success state's Enter. Read on Claude Code only.
    pub login_screen: Option<aterm_phase::LoginScreen>,
    /// `⎿  Login interrupted` is the last thing said
    /// ([`aterm_phase::login_interrupted`]): the `/login` typed at the wall
    /// left by Esc, its login still gone. Nothing types `/login` again into
    /// it; the owner is told. Read on Claude Code only.
    pub login_interrupted: bool,
    /// THE METHOD THIS SESSION'S LOGIN ALREADY IS, as the host reads it
    /// (`crate::supervise::IdleHost::login_method`), asked only where a lost
    /// login or its `/login` dialog is on the screen: what the owner's
    /// notice says aterm can choose for them (a subscription), or that the
    /// method is theirs to pick ([`super::notice::LOGIN_CHOOSE`]). `None`:
    /// not asked, or nothing says.
    pub login_method: Option<aterm_phase::LoginMethod>,
    /// ANOTHER TAB'S SIGN-IN OF THIS LOGIN, as the host's tabs share it
    /// (`crate::supervise::IdleHost::login_shared`), asked at each reading of
    /// a lost login of a carried agent: one that carries it is waited on,
    /// nothing typed, and a login back there delivers this tab's turn again
    /// ([`RULE_LOGIN_BACK`]). `None`: not asked, or nothing shares it — this
    /// tab's `/login` is the login's.
    pub login_shared: Option<super::login::LoginShared>,
    /// THE TURN THE SCREEN'S LAST WORDS CLOSE, by the `❯` row that opened it
    /// ([`aterm_phase::answered_prompt`], [`ScreenAnswer`]): one of the
    /// signs an act was taken and answered where no read saw its turn run
    /// ([`TurnEndState::observe`]). Read on Claude Code only.
    pub answered: Option<ScreenAnswer>,
    /// THE TURN THE CONVERSATION'S RECORD ENDS ON ([`RecordTurn`]), set by
    /// the loop where it read that record
    /// ([`TurnEndState::reads_record`]) while an act of the policy's stands
    /// untaken ([`TurnEndState::untaken_act`]): the record's sign of the
    /// same. `None`: no record, none read, or no prompt opened that turn.
    pub record_turn: Option<RecordTurn>,
    /// USAGE CREDITS CARRY THE SESSION by Claude Code's own notice
    /// ([`aterm_phase::Reading::credits`]: the lasting `Now using usage
    /// credits`, or the notice they started on), with the reset of the
    /// window they carry past where one was named ([`CreditsCarry`]). No
    /// wall: what the credits hold reads under `[harness] usage_credits =
    /// false` ([`credits_hold`]). `None`: no notice says so, or no composer
    /// is drawn to say it under.
    pub credits: Option<CreditsCarry>,
    /// WHAT THE SESSION'S REPOSITORY HOLDS THAT NO REMOTE HAS
    /// ([`crate::harness::repo_work::RepoWork`]: tracked changes, untracked
    /// files, stash entries, commits no remote-tracking ref contains), by the
    /// host's probe of the agent's working directory
    /// (`crate::supervise::IdleHost::repo_work`) — set by the loop at a
    /// decision's read whose words could end the task. Found, the worker's
    /// own word ends nothing: the point owes [`RULE_UNCOMMITTED`]
    /// ([`TurnEndState::observe`]). `None`: nothing found, no host that
    /// probes, a probe that could not run, or the screen's (the inert arm
    /// every test reading starts from).
    pub repo: Option<crate::harness::repo_work::RepoWork>,
}

/// THE TURN A SCREEN'S LAST WORDS CLOSE ([`TurnEndReading::answered`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenAnswer {
    /// The words of the `❯` row that opened it, hashed
    /// ([`crate::supervise::approvals::words_hash`]).
    pub prompt: u64,
    /// Its work by the done row Claude Code drew under it
    /// ([`done_row_work`]: whole seconds, `✻ Sautéed for 0s`); `None` where
    /// none is drawn.
    pub work: Option<Duration>,
}

impl ScreenAnswer {
    /// The turn Claude Code's `rows` close ([`aterm_phase::answered_prompt`]).
    #[must_use]
    pub fn of(rows: &[String]) -> Option<Self> {
        aterm_phase::answered_prompt(rows).map(|said| Self {
            prompt: crate::supervise::approvals::words_hash(&said),
            work: done_row_work(rows),
        })
    }
}

/// THE TURN THE CONVERSATION'S RECORD ENDS ON
/// ([`TurnEndReading::record_turn`], `crate::harness::upgrade::LastTurn`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordTurn {
    /// The words of the prompt that opened it, hashed
    /// ([`crate::supervise::approvals::words_hash`]).
    pub prompt: u64,
    /// How long before the read that prompt was recorded, on the wall clock
    /// (its row's whole second: `LastTurn::opened_ms`).
    pub age: Duration,
    /// Its work by the `turn_duration` row that closed it; `None` while none
    /// has.
    pub ended: Option<Duration>,
}

impl RecordTurn {
    /// The record's last turn `t`, read at `now_ms` (unix milliseconds):
    /// `None` where no prompt with its words and its time opened it.
    #[must_use]
    pub fn of(t: &crate::harness::upgrade::LastTurn, now_ms: i64) -> Option<Self> {
        let at = i64::try_from(t.opened_ms?).ok()?;
        Some(Self {
            prompt: t.opened_said?,
            age: Duration::from_millis(now_ms.saturating_sub(at).max(0).unsigned_abs()),
            ended: t.ended,
        })
    }
}

/// How much earlier than the act's own stamp the record may date the prompt
/// that act typed ([`TurnEndState::observe`]'s fast answer): the record's
/// whole second, and the guarded submit the act's stamp follows (its
/// `timeout=2500`), Claude Code having recorded the prompt at its Enter.
pub const RECORD_SLACK: Duration = Duration::from_secs(5);

impl TurnEndReading {
    /// THE ADVICE THE DECIDER ACTS ON ([`AccountAdvice::past_probe`] of
    /// [`Self::account`]): the one read of it every deciding site makes, so
    /// a `Probe { otherwise: X }` is decided exactly as `X` is, and a probe
    /// with nothing behind it exactly as no advice.
    #[must_use]
    pub fn advised(&self) -> Option<&AccountAdvice> {
        self.account.as_ref().and_then(AccountAdvice::past_probe)
    }

    /// A person's hand in the turn that ended here: a keystroke
    /// ([`Self::person`]) no older than the turn's busy work
    /// ([`RunningTurn::point`]) at its most ([`Self::worked_doubt`]) plus
    /// [`PERSON_TURN_SLACK`]. Decides whether a turn nobody here typed is a
    /// person's ([`TurnEndState::observe`]). Both are awake time: the
    /// keystroke's age is read on the host's awake clock (`status
    /// human_ms=`), and the work is the done row's span less the sleep that
    /// must lie inside it ([`Self::of_slept`]). Read against the turn's work
    /// at its least — an hour the ledger counts somewhere between the act
    /// and the night taken off a person's 328 s exchange that ran before
    /// both — the keystroke fell outside the turn, the exchange was nobody's,
    /// and a loop that followed typed `keep going` into it.
    #[must_use]
    pub fn person_in_turn(&self) -> bool {
        self.person.is_some_and(|ago| {
            ago <= self
                .worked
                .unwrap_or_default()
                .saturating_add(self.worked_doubt)
                .saturating_add(PERSON_TURN_SLACK)
        })
    }

    /// THE SCREEN SHOWS A STANDING WALL: one read off it, or the loop's memory
    /// of a limit ([`Self::wall_remembered`]) while the screen still draws a
    /// limit's notice ([`Self::since_limit`]). A remembered limit whose notice
    /// the screen draws no more — the reply under the vendor's continuation
    /// scrolled it off while a shell the worker started kept the screen busy
    /// — is the loop's wait, and nothing on the screen: the words under it
    /// are the worker's, read as an idle screen's are (its words since the
    /// limit, [`Self::said_since_limit`]; its [`FINISHED`] and a person's
    /// exchange left to them, [`TurnEndState::observe`]). Measured before
    /// (the adversarial verifier of the limit's-end fix, D1): the memory's
    /// wall hid them, and at the reset `keep going` went under
    /// `usage-resume@v1` into the done report.
    #[must_use]
    pub fn wall_drawn(&self) -> bool {
        self.wall.is_some() && !(self.wall_remembered && self.since_limit.is_none())
    }

    /// THE WORKER'S WORDS SINCE A USAGE LIMIT, where the screen orders them
    /// after it ([`limit_end`]): its last words ([`Self::said_tail`]) on
    /// Claude Code's screen with no standing wall shown ([`Self::wall_drawn`]:
    /// a standing limit's notice, read off the screen or the loop's memory of
    /// it while its notice is drawn, is no word of the worker's), no prompt
    /// unanswered and no turn stopped — and the newest
    /// limit notice the screen still draws ([`Self::since_limit`]) with a turn
    /// served or the vendor's own continuation under it, or none drawn at all
    /// (the reply pushed it off the screen; a relaunched or `/resume`d
    /// conversation redraws its notice). A newest notice with nothing served
    /// or continued under it is the standing limit: what is said under it —
    /// the vendor's countdown or its cancel, a person's local command — is
    /// none of the worker's since. `None` on Codex's screen, which draws
    /// neither that order nor a continuation of the vendor's.
    #[must_use]
    pub fn said_since_limit(&self) -> Option<&str> {
        let claude = match self.program {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        let after = self
            .since_limit
            .as_ref()
            .is_none_or(|s| s.served || s.fired);
        if !claude || self.wall_drawn() || self.pending_input || self.interrupted || !after {
            return None;
        }
        self.said_tail.as_deref()
    }

    /// What the worker asks of a person here and whether it names an
    /// irreversible act ([`Said::Irreversible`]); `None` when nothing is asked
    /// (including after a person's Esc). An unreadable question is never
    /// "nothing asked". [`decide_turn_end`] answers by this reading.
    #[must_use]
    pub fn asked(&self) -> Option<(String, bool)> {
        if self.interrupted {
            None
        } else if self.phase == Phase::Question && self.said_tail.is_none() {
            Some((
                "the worker asked something its last words do not show".to_string(),
                false,
            ))
        } else {
            asked_by(self.said_tail.as_deref())
        }
    }

    /// Reads one screen: `aterm-phase`'s [`aterm_phase::Reading`] of `rows`
    /// by the session program's reader, plus what the loop read of the
    /// composer's cells ([`ComposerRead`]; a bare `bool` is whether its text
    /// was typed), work since the last point, the wall's reset on the loop's
    /// clock, and the rules.
    /// Claude-Code-only signals (an unanswered `❯` row, `Worked for`) are read
    /// only on Claude Code. [`Self::upgrading`] is set by the loop.
    #[must_use]
    pub fn of(
        reading: &aterm_phase::Reading,
        rows: &[String],
        typed: impl Into<ComposerRead>,
        worked: Option<Duration>,
        reset_at: Option<Instant>,
        rules: Option<String>,
    ) -> Self {
        Self::of_slept(
            reading,
            rows,
            typed,
            worked,
            InTurn::default(),
            reset_at,
            rules,
        )
    }

    /// [`Self::of`] where the machine slept `asleep` inside the turn the
    /// screen closes, as the loop knows its sleeps
    /// (`policy::sleeps::SleepBook::in_turn`, any turn's: the act's, a
    /// person's, one a read saw run before the sleep): the work is the larger
    /// of the loop's own (`worked`, on a clock a sleep stops) and the done
    /// row's span less the most of the sleep that can lie inside it, and
    /// [`Self::worked_doubt`] what the least that must would add to that
    /// (for [`Self::person_in_turn`]). Claude Code times a turn on the wall
    /// clock, which runs on through a sleep: whole, a try's `✻ Baked for 32m
    /// 30s` around a 30-minute sleep read as 32 minutes of work and started
    /// a spend limit's ladder over, where the same two and a half minutes
    /// with no sleep kept the try's two-hour rung (limits-5 live).
    #[must_use]
    pub fn of_slept(
        reading: &aterm_phase::Reading,
        rows: &[String],
        typed: impl Into<ComposerRead>,
        worked: Option<Duration>,
        asleep: InTurn,
        reset_at: Option<Instant>,
        rules: Option<String>,
    ) -> Self {
        let claude = match reading.program {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        let own = reading
            .composer
            .as_ref()
            .is_some_and(|(_, lines)| crate::harness::upgrade::own_draft(&lines.concat()));
        let read = typed.into();
        // Enter must send what the composer shows: over Claude Code's open
        // completion menu it runs the selected entry, so a draft whose
        // command is not the entry Enter runs is diverted (`/cl` over `❯
        // /clear`), one whose command it is (the probe's `/usage` over `❯
        // /usage`) is not; over the `@` menu it completes the selected entry
        // and sends only with none selected. A draft over a menu whose
        // selection the screen ITSELF does not tell — every cell its
        // selection is read by read ([`ComposerRead::menu_fg`]), or none to
        // read (an `@` menu of one entry, MEASURED on 2.1.268: drawn alike
        // selected or not, but for a colour no other entry's tells against)
        // — may be diverted, and no later read of that screen says more: it
        // is diverted too, never sent, and told once. A cell not read leaves
        // it unknown until a read answers it.
        let over_menu = reading.composer.as_ref().and_then(|(_, lines)| {
            let menu = claude
                .then(|| aterm_phase::phase::completion_menu(rows))
                .flatten()?;
            let fg = read.menu_fg.as_deref();
            let runs = menu.runs(fg);
            let command = lines.first().and_then(|l| l.split_whitespace().next());
            let sends = |enter: &aterm_phase::phase::MenuEnter<'_>| match *enter {
                aterm_phase::phase::MenuEnter::Run(entry) => Some(entry) == command,
                aterm_phase::phase::MenuEnter::Complete(_) => false,
                aterm_phase::phase::MenuEnter::Draft => true,
            };
            let selection_read = !menu.colour_only()
                || fg.is_some_and(|fg| {
                    fg.len() == menu.entries.len() && fg.iter().all(Option::is_some)
                });
            if runs.iter().all(sends) {
                None
            } else if selection_read || !runs.iter().any(sends) {
                Some(Composer::Diverted)
            } else {
                Some(Composer::Unknown)
            }
        });
        // Existing pure readers pass a proven bool; a live cell read may be unknown.
        // Match it explicitly rather than defaulting unknown to empty or typed.
        let composer = match (reading.composer.as_ref(), read.typed) {
            (None, _) => Composer::Absent,
            (Some(_), None) => Composer::Unknown,
            (Some(_), Some(true)) if own => Composer::Own,
            (Some(_), Some(true)) => over_menu.unwrap_or(Composer::Typed),
            (Some(_), Some(false)) => match &reading.suggestion {
                Some(s) => Composer::Placeholder(s.clone()),
                None => Composer::Empty,
            },
        };
        let message = reading
            .wall
            .as_ref()
            .map(|w| w.message.clone())
            .unwrap_or_default();
        // The vendor's word, wherever it is drawn (the footer's notification,
        // the transcript's row): tied to the limit it ends by the notice it
        // stands under ([`TurnEndState::observe`], `NamedReset::continued_under`).
        let auto_continue = claude
            .then(|| aterm_phase::auto_continue_end(rows))
            .flatten();
        // A COUNTDOWN'S RECORD IS NO COUNTDOWN. On Claude Code's screen a
        // live countdown is drawn as a row of its own (the footer's
        // `Continuing automatically at … · esc to cancel` under its banner
        // from 2.1.294 on, a notification over the composer in 2.1.284:
        // [`aterm_phase::AutoContinue::Armed`]), and the vendor's word that it
        // fires is [`aterm_phase::AutoContinue::Fired`]. A wall read off the
        // transcript's `⏺` row (`⏺ Usage limit reached · continuing
        // automatically at … · esc to cancel`, [`aterm_phase::Placement::ErrorRow`])
        // with neither drawn is the countdown's RECORD, which a `/resume` back
        // into the conversation redraws after its countdown was dropped:
        // nobody counts down there (measured on 2.1.301, the review of
        // ATERM-KI-1015-1's W4 fix).
        let recorded = claude
            && reading.composer.is_some()
            && !matches!(
                auto_continue,
                Some(aterm_phase::AutoContinue::Armed | aterm_phase::AutoContinue::Fired)
            )
            && reading
                .wall
                .as_ref()
                .is_some_and(|w| w.placement == aterm_phase::Placement::ErrorRow);
        // The turn's work: the loop's own, or the done row's span less the
        // sleep inside it, whichever is more.
        let done = claude
            .then(|| done_row_work(rows))
            .flatten()
            .unwrap_or_default();
        let less = |w: Duration, asleep: Duration| w.max(done.saturating_sub(asleep));
        Self {
            phase: reading.phase.clone(),
            authoritative: reading.phase_authoritative,
            survey: reading.survey,
            wall: reading.wall.as_ref().map(|w| w.kind),
            // ...and has not said since that it will not (the limit a cancel
            // left, read as its wall: `TurnEndState::left_answer`), and the
            // words are no countdown's record in a redrawn history.
            resumes_by_itself: super::super::limit::resumes_by_itself(&message)
                && auto_continue != Some(aterm_phase::AutoContinue::Unkept)
                && !recorded,
            wall_message: message,
            reset_at,
            reset_clock: reading
                .wall
                .as_ref()
                .and_then(|w| ResetClock::of(w.reset.as_deref())),
            jitter: Duration::ZERO,
            composer,
            said_tail: reading.said_tail.clone(),
            worked: worked.map(|w| less(w, asleep.most)),
            worked_doubt: worked.map_or(Duration::ZERO, |w| {
                less(w, asleep.least).saturating_sub(less(w, asleep.most))
            }),
            rules,
            pending_input: claude
                && last_said_row(rows).is_some_and(|r| r.trim_start().starts_with('❯')),
            background: reading.phase == Phase::Busy
                && if claude {
                    aterm_phase::turn_over_background(rows)
                } else {
                    reading.program.reader().background_wait(rows).is_some()
                },
            interrupted: reading.interrupted,
            restartable: false,
            resume: None,
            upgrading: false,
            taskless: reading.fresh,
            finished_record: None,
            finished_at_check: false,
            check_record: None,
            left_record: false,
            left_work: None,
            person: None,
            login_back: claude && aterm_phase::login_restored(rows),
            auto_continue,
            since_limit: claude.then(|| aterm_phase::since_limit(rows)).flatten(),
            served_in_transcript: false,
            notice_elsewhere: false,
            wall_remembered: false,
            reach: Reach::Unknown,
            program: reading.program,
            account: None,
            autopilot: Autopilot::Cruise,
            login_label: None,
            login_superseded: false,
            login_notice: claude
                .then(|| aterm_phase::login_expires_in(rows))
                .flatten(),
            login_screen: claude.then(|| aterm_phase::login_screen(rows)).flatten(),
            login_interrupted: claude && aterm_phase::login_interrupted(rows),
            login_method: None,
            login_shared: None,
            answered: claude.then(|| ScreenAnswer::of(rows)).flatten(),
            record_turn: None,
            // The reset the notice they started on named is the loop's to
            // place ([`CreditsCarry::reset_at`]).
            credits: reading.credits.as_ref().map(|c| CreditsCarry {
                model: c.model.as_deref().and_then(credits_model),
                ..CreditsCarry::default()
            }),
            repo: None,
        }
    }

    /// A turn has ended and nothing blocks acting: an idle, question or limit
    /// phase from an authoritative reader, no box or survey, a composer drawn,
    /// and no unanswered input except one judged not taken (`stale_input`).
    fn at_point(&self, stale_input: bool) -> bool {
        matches!(
            self.phase,
            Phase::Idle | Phase::Question | Phase::Limited { .. }
        ) && self.authoritative
            && !self.survey
            && !matches!(self.composer, Composer::Absent | Composer::Unknown)
            && (!self.pending_input || stale_input)
    }
}

/// The turn running now, as the loop's busy reads track it (the busy-read
/// twin of [`TurnEndReading::person_in_turn`]). Its span starts at the first
/// busy read after the last point ([`Self::point`]). Kept by the loop's
/// `Session`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunningTurn {
    /// The first busy read of the turn running now.
    since: Option<Instant>,
}

impl RunningTurn {
    /// A busy read of the worker's own turn at `now`: the span begins at
    /// the first.
    pub fn busy(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    /// The point ending the turn, at `now`: returns the turn's work (its
    /// span) and starts a fresh span.
    pub fn point(&mut self, now: Instant) -> Option<Duration> {
        self.since.take().map(|t| now.saturating_duration_since(t))
    }
}

/// What a typed command leaves owed for the next point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    /// Nothing more.
    Nothing,
    /// A continuation, under the command's rule, once it has run.
    Continue,
}

/// WHAT KIND OF WAIT A [`TurnEndAction::WaitUntil`] IS
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §10.5.3, part A): what the
/// loop tells its host of it (`IdleHost::waiting`), and nothing else — no
/// decision reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitKind {
    /// A wall's retry wait (a ladder rung or the Down hold): the wait a tab
    /// shows as the next try (`→ 14:05`).
    Retry,
    /// The short streak's back-off ([`TurnEndState::short_wait`]): `short`
    /// short turns in a row (the first point seen is one), at most
    /// `u8::MAX`. The window shows it as a pause (`pausing until 18:41`).
    BackOff {
        /// The streak's length.
        short: u8,
    },
    /// Any other wait (the take check, a person typing, a reset, a model
    /// picker, an account's): not told.
    Other,
}

impl WaitKind {
    /// Whether the loop tells the host of a wait of this kind: a retry and
    /// a back-off are; any other wait is not.
    #[must_use]
    pub const fn told(self) -> bool {
        !matches!(self, WaitKind::Other)
    }
}

/// What [`decide_turn_end`] says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndAction {
    Nothing,
    /// Decide again at `until` (a screen change first also decides again).
    WaitUntil {
        until: Instant,
        why: String,
        /// What kind of wait it is ([`WaitKind`]): a wall's retry and the
        /// short streak's back-off are told to the window
        /// (`IdleHost::waiting`); every other wait is not.
        wait: WaitKind,
    },
    /// Type `text` into the composer and submit it, guarded.
    Type {
        text: String,
        rule_id: &'static str,
    },
    /// Accept the worker's own suggestion (`text`): the vendor's accept key
    /// fills it, a guarded Enter submits it.
    Accept {
        text: String,
        rule_id: &'static str,
    },
    /// Type a slash command and submit it, guarded.
    TypeCommand {
        command: String,
        rule_id: &'static str,
        then: Then,
    },
    /// Submit the draft standing in the composer, as it is, in place of the
    /// act `rule_id` names (a guarded Enter): the draft is the next message.
    Submit {
        rule_id: &'static str,
    },
    /// Press Enter once on the dialog whose row `row` stands (guarded on that
    /// row, fenced on the judged read and a person's key): Claude Code's
    /// `Login successful. Press Enter to continue…`
    /// ([`super::login::dialog_act`]). Nothing is typed, and it is no
    /// continuation: no act of the hour's is spent, nothing is awaited.
    Confirm {
        rule_id: &'static str,
        row: String,
    },
    /// Hand the point to a person.
    Escalate {
        reason: String,
    },
    /// Have the host end the agent, relaunch it on its own conversation and
    /// carry it on ([`Restart`]). `otherwise` applies where no restart is made:
    /// the memory banner's escalation, a bucket's reset waited out, or the
    /// continuation a reset was owed.
    Restart {
        why: Restart,
        rule_id: &'static str,
        otherwise: Box<TurnEndAction>,
    },
}

impl TurnEndAction {
    /// The rule an act is taken under (`None` for no act).
    #[must_use]
    pub fn rule_id(&self) -> Option<&'static str> {
        match self {
            TurnEndAction::Type { rule_id, .. }
            | TurnEndAction::Accept { rule_id, .. }
            | TurnEndAction::TypeCommand { rule_id, .. }
            | TurnEndAction::Submit { rule_id }
            | TurnEndAction::Confirm { rule_id, .. }
            | TurnEndAction::Restart { rule_id, .. } => Some(rule_id),
            _ => None,
        }
    }
}

/// Which wall a track follows (a new kind is a new track).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallClass {
    Retry,
    Usage,
    Model,
    Context,
    Auth,
    Memory,
}

fn class_of(kind: WallKind) -> WallClass {
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => WallClass::Retry,
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => WallClass::Usage,
        WallKind::ModelBucket { .. } => WallClass::Model,
        WallKind::Context => WallClass::Context,
        WallKind::Auth => WallClass::Auth,
        WallKind::Memory => WallClass::Memory,
    }
}

/// THE LIMITS A RESET ENDS, whose notice's reset ([`NamedReset`]) binds the
/// wait wherever the notice has gone ([`still_shown`],
/// [`TurnEndState::waits_on_named_reset`]), and which the vendor's own word
/// ends under that notice ([`TurnEndState::limit_went_on`]): a usage window
/// (or spend), and a model's bucket. A bucket is one limit like the other
/// until the session leaves the model it limits — the fallback's relaunch
/// made ([`TurnEndState::restarted`]), a person's `/model` read since its
/// notice ([`NamedReset::moved_off_under`]) — and then its reset binds
/// nothing (its [`WallTrack::named`] is dropped). Measured on Claude Code
/// 2.1.300 (the review of ATERM-KI-1015-1's W3 fix): a Sonnet bucket under
/// the generic banner, its countdown armed and no relaunch made, read only
/// as a usage limit was waited on when its notice left; read as the bucket
/// it is, a `/resume` or Esc on the countdown had `usage-resume@v1 keep
/// going` typed into it one minute later, two days before its reset.
fn named_class(class: WallClass) -> bool {
    matches!(class, WallClass::Usage | WallClass::Model)
}

/// THE LIMIT'S TRY [`TurnEndState::acted`] last recorded
/// ([`TurnEndState::limit_try`]): `usage-resume@v1`, or the done check, an
/// answer ([`is_answer_rule`]) or under Autopilot Land a land act ([`is_land_rule`])
/// typed in its place at a limit's end, at the track of a usage window or a
/// model bucket ([`named_class`]) — the track as the act left it. One rule:
/// the try follows the act's place, whichever act it is. Its ledger row
/// carries it ([`crate::supervise::approvals::act_reason`]), and a loop that
/// starts after it seeds that track ([`TurnEndState::seed_usage_attempt`]):
/// the same limit, so the same next act at the same time as the loop that
/// typed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitTry {
    /// The limit is a model's bucket; otherwise a usage window (or a spend
    /// limit). A loop that seeded a usage window's track for a try at a
    /// bucket saw it replaced by the bucket's own, its attempt lost, where
    /// the bucket met again stood past a reset placed early: `usage-resume@v1`
    /// into it at once (the verifier of the successor's seed, D2).
    pub bucket: bool,
    /// Typed in the continuation's place (the done check, the reversible
    /// answer, or under Land a land act: [`limit_end`]), not the continuation
    /// itself.
    pub in_place: bool,
    /// The wall's attempts the act left, itself counted: the rung the next
    /// try is on. A loop that seeded every try as the first, after the done
    /// check in the continuation's place and the continuation that met the
    /// limit again (attempt 2), typed its next continuation the first rung
    /// after the act, 20 minutes before the loop that typed it would have
    /// (the verifier of the successor's seed, D3).
    pub attempts: usize,
    /// The try was on the ENTITLEMENT ladder ([`WallTrack::entitlement`]: a
    /// wall no reset ends and none names, retried 30 min, 2 h, then every
    /// 6 h). A successor's seed stands until that ladder's next rung, not the
    /// usage windows' last one ([`TurnEndState::seed_usage_attempt`]). Cut at
    /// 30 minutes, every restart that long after a try put the ladder back to
    /// its first rung: `usage-resume@v1` half an hour after the loop
    /// attached, hours before the loop that typed the try (the verifier of
    /// the successor's seed's refix, V2-2).
    pub entitlement: bool,
}

/// HOW FAR APART A LIMIT'S TRY AND ITS ANSWER STAND, at most, where the
/// limit answered it at once ([`TurnEndState::date_unread_try`]): the
/// notice's row may be stamped before the loop's record of the try (the
/// record follows the server's taking of the turn; the stub's 429 is written
/// 19 ms after the prompt's own row, 2.1.299), the screen draws it after
/// the row is written, and the answer's done row says a few seconds of work
/// at most. Ten seconds, as for the transcript's answer to an act
/// (`supervise/wall_confirm.rs`, `LEFT_ANSWER_SLACK_S`). The answer is dated
/// at the latest its sign allows, not at the try and this slack: a loop that
/// typed the try and first read the screen when the answer drew reads it
/// then, and a follower dating it sooner types sooner.
pub const TRY_ANSWER_SLACK: Duration = Duration::from_secs(10);

/// A LIMIT'S TRY NO LOOP HAS READ ([`TurnEndState::seed_usage_attempt`]): when
/// it was typed, on the policy's clock (moved by a sleep,
/// [`TurnEndState::slept`]), and at which limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnreadTry {
    at: Instant,
    limit: LimitTry,
}

/// A wall being worked: since when it shows (this appearance), and how many
/// acts it has had.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WallTrack {
    class: WallClass,
    since: Instant,
    attempts: usize,
    /// The acts made because the host measured the API reachable again
    /// ([`RULE_API_BACK`]): the next is spaced on the ladder by their count.
    ups: usize,
    /// A full context's track has met the vendor's own word that a `/compact`
    /// could not lower it (`Compaction failed · …`, [`compaction_exhausted_row`]).
    /// Latched for the appearance: a request error on a later rung
    /// (`Error during compaction: …`) refutes nothing the vendor already said.
    exhausted: bool,
    /// What a full context's own notice said fills it beyond the conversation's
    /// words ([`ContextFill::of`]), latched for the appearance: by the third
    /// `/compact` the vendor's `Compaction failed` row stands where the notice
    /// stood, and the way back the owner is told is the notice's
    /// ([`TurnEndState::context_fill`]).
    fill: Option<ContextFill>,
    /// The reset the wall's notice last named while it showed: what a point
    /// the wall has LEFT is decided by ([`still_shown`]).
    named: Option<NamedReset>,
    /// The wall has shown at a POINT, where its own rule decides it — not
    /// only at readings that were no point yet (an act's take window, anyone's
    /// unanswered row; [`TurnEndState::observe`]). A lost login no point saw
    /// was never decided: no `/login` typed, nobody told. Where its row has
    /// left the screen it is decided as the wall it left ([`still_shown`]),
    /// never held as one the owner was told of ([`act_at_point`]).
    pointed: bool,
    /// The wall's words as its notice last read ([`TurnEndReading::wall_message`]):
    /// what a decision about a wall that has left the screen says of it.
    said: String,
    /// The wall's kind as its notice last read (`None`: a track a loop
    /// before this one left, no notice read since): what a retry's wall
    /// whose notice has left the screen is decided as ([`left_retry`]).
    kind: Option<WallKind>,
    /// A loop before this one's usage act is all the track knows
    /// ([`TurnEndState::seed_usage_attempt`]): no notice of its limit read
    /// since ([`TurnEndState::track_wall`]), nor by a loop before this one
    /// ([`TurnEndState::seed_named_reset`], [`TurnEndState::seed_met_attempt`]).
    /// It works the limit that act meets again once one is read; until then
    /// nothing stands in for that limit's continuation
    /// ([`TurnEndState::acted`]), and the worker's work it sees is the
    /// try's, which starts the wall's episode again ([`TurnEndState::track_wall`]).
    seeded: bool,
    /// The wall's notice, as last read, names no reset that ends it and is
    /// retried on the entitlement ladder ([`entitlement_ladder`]); for a
    /// seed, the ladder its try was on ([`LimitTry::entitlement`]). The
    /// limit's try carries it ([`TurnEndState::acted`]).
    entitlement: bool,
    /// A LOOP BEFORE THIS ONE'S RETRY AT THIS WALL IS OWED ITS ANSWER
    /// ([`TurnEndState::seed_retry`]): typed last, no point judged since it,
    /// and its take window over. The first reading of the wall is the
    /// retry's next appearance, as the act awaited is for the loop that typed
    /// it, and a point without the wall is the retry taken.
    owed: bool,
    /// A LOST LOGIN'S OWN BOOKS ([`WallClass::Auth`]; nothing for any other
    /// wall): who it answered, when, and what the harness did about it.
    login: LoginBooks,
}

/// WHAT A LOST LOGIN'S TRACK KEEPS ([`WallTrack::login`]): the read it was
/// seen answering a prompt at, whose hand that prompt was, the `/login` the
/// harness typed into it and whether a `/login` was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LoginBooks {
    /// A `/login` WAS LEFT BEFORE IT SIGNED IN (`⎿  Login interrupted` read
    /// while the track stood), whoever typed it: nothing types `/login` into
    /// this loss again ([`wall_own_act`]), and what the owner is told for the
    /// rest of it is that the sign-in was cancelled
    /// ([`TurnEndState::login_cancelled`]), the wall's row back or not.
    cancelled: bool,
    /// THE WALL ANSWERED A PERSON'S OWN PROMPT (their hand opened the turn it
    /// ended, [`TurnEndState::hand`]): the exchange it swallowed is theirs,
    /// and the turn Claude Code sends it again in once the login is back
    /// delivers it — whoever pressed the success Enter
    /// ([`TurnEndState::observe`]).
    hand: bool,
    /// THE READ AT WHICH THIS LOOP SAW THE WALL ANSWER A PROMPT, last: the
    /// track opened at a read after a point this loop judged (or after a
    /// turn it read running), or a later read whose answered prompt
    /// ([`TurnEndReading::answered`]) is another. `None` at a loop's first
    /// point with no turn read: the wall may have stood there for hours, and
    /// nothing says a person's key came before it ([`login_quiet`]).
    prompt_at: Option<Instant>,
    /// That prompt's words, hashed ([`ScreenAnswer::prompt`]).
    prompt: Option<u64>,
    /// When THIS LOOP typed `/login` into the loss: a person's key no later
    /// than it is the one that sent the prompt, which holds nothing of the
    /// dialog's own steps ([`TurnEndState::login_typed_at`]).
    typed_at: Option<Instant>,
    /// THE TURN WAS DELIVERED AGAIN once the login came back in another tab
    /// ([`RULE_LOGIN_BACK`]): met by the wall again, this process did not
    /// take the login back, and its own `/login` follows.
    back_typed: bool,
}

/// HOW A WALL READ AT AN OBSERVATION CAME ([`TurnEndState::track_wall`]): a
/// person's hand opened the turn it ended ([`TurnEndState::hand`]), and the
/// loop could see it arrive ([`LoginBooks::prompt_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arrival {
    hand: bool,
    seen: bool,
}

/// A limit's reset as its notice named it, kept while the notice shows
/// ([`TurnEndState::observe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NamedReset {
    kind: WallKind,
    /// The reset on the loop's clock ([`TurnEndReading::reset_at`]).
    at: Instant,
    /// This session's share of the herd ([`TurnEndReading::jitter`]).
    jitter: Duration,
    /// The vendor said it goes on by itself ([`TurnEndReading::resumes_by_itself`]),
    /// and has not said since that it will not
    /// ([`aterm_phase::AutoContinue::Unkept`]: Esc on its countdown,
    /// `Automatic continue cancelled`), nor stopped drawing its countdown on
    /// Claude Code's screen where the notice has left it (a `/resume`, a
    /// `/clear`: the countdown dropped without a word, W4 of
    /// ATERM-KI-1015-1), nor been read before the agent's process started
    /// ([`TurnEndState::agent_started`]: a relaunched process holds no
    /// countdown). Either way the reset is then placed as a cancelled
    /// countdown's ([`TurnEndState::observe`]).
    by_itself: bool,
    /// The reset as the notice's clock said it: what ties a notice the screen
    /// draws to THIS limit ([`Self::served_under`],
    /// [`Self::continued_under`]); `None` where it named none the clock reads
    /// (a span), and so no notice is this one's.
    said: Option<ResetClock>,
    /// When this limit was FIRST read — kept while its notice is read again
    /// naming the same reset ([`ResetClock`]), and a loop before this one's
    /// read for a seeded one ([`TurnEndState::seed_named_reset`]): the
    /// vendor's countdown belongs to the process that drew it then, and an
    /// agent process started since holds none ([`TurnEndState::agent_started`]).
    first: Instant,
    /// When the session's transcript says THIS limit's notice row was written
    /// (unix seconds, [`TurnEndState::notice_written_at`] at the read that
    /// first read it, kept as [`Self::first`] is; `None`: no transcript, a
    /// row that names no time, a seed). A notice written before the agent's
    /// process started is a gone process's, whenever this loop first read it
    /// ([`TurnEndState::agent_started_unix`]): a cold restore's first loop
    /// has no earlier read of it (ATERM-KI-1016-1).
    written: Option<i64>,
}

impl NamedReset {
    /// A turn served since THIS limit's own notice ([`notice_of`]): the limit
    /// lifted before its reset, and the turn is work since it.
    fn served_under(&self, since: Option<&aterm_phase::SinceLimit>) -> bool {
        since.is_some_and(|s| s.served) && notice_of(since, self.kind, self.said)
    }

    /// The vendor's word that it went on ([`aterm_phase::AutoContinue::Fired`],
    /// read under the screen's newest limit notice `since`) is THIS limit's:
    /// said under this limit's own notice ([`notice_of`]), as a served turn
    /// is — never under an older limit's that a `/resume`d conversation
    /// redraws (W1 of ATERM-KI-1015-1) — and read no sooner than
    /// [`FIRED_EARLY`] before this limit's reset, where the read's time `now`
    /// is known: the vendor's countdown goes on AT the reset, so its word
    /// read before it is another day's under the same clock, which names no
    /// date (ATERM-KI-1015-1 LP-1: a `/resume`d conversation's older notice
    /// where no row, host or tail says it is another conversation's,
    /// [`TurnEndReading::notice_elsewhere`]). A notice that named no clock (a
    /// span) has nothing to tie a word to, and the vendor's is taken as said.
    fn continued_under(
        &self,
        since: Option<&aterm_phase::SinceLimit>,
        now: Option<Instant>,
    ) -> bool {
        self.said.is_none()
            || (notice_of(since, self.kind, self.said)
                && now.is_none_or(|now| later(now, FIRED_EARLY) >= self.at))
    }

    /// A person's `/model` since THIS limit's own notice
    /// ([`aterm_phase::SinceLimit::chosen_model`], under the notice
    /// [`notice_of`] ties to it): only a choice of another known family
    /// leaves the model a bucket limits. A same-family or unknown choice
    /// keeps its reset binding. Never under
    /// another limit's notice (a `/resume`d conversation's history), nor
    /// where the notice named no clock to tie it by.
    fn moved_off_under(&self, since: Option<&aterm_phase::SinceLimit>, limited: &str) -> bool {
        notice_of(since, self.kind, self.said)
            && since.is_some_and(|s| {
                s.model_set
                    && s.chosen_model
                        .as_deref()
                        .and_then(bucket_model)
                        .zip(bucket_model(limited))
                        .is_some_and(|(chosen, limited)| chosen != limited)
            })
    }
}

/// How long before a limit's reset, as this policy placed it, the vendor's
/// word that it went on is read as this limit's at all
/// ([`NamedReset::continued_under`]): the countdown goes on at the reset, and
/// the placed reset is the notice's minute, at or before the vendor's
/// second; the minute is room for the two clocks' reads.
pub const FIRED_EARLY: Duration = Duration::from_secs(60);

/// Whether the screen's newest limit notice (`since`,
/// [`aterm_phase::since_limit`]) is THE NOTICE OF THE LIMIT `kind` whose
/// reset read `said`: of its class, naming the same reset. Another limit's
/// notice — an older one a `/resume`d conversation redraws — says nothing of
/// this one, nor does any notice where `said` is `None` (a reset no clock
/// names). What ties the screen's evidence of a limit's end to THE limit
/// waited on: a turn served under it ([`NamedReset::served_under`]) and the
/// vendor's word that it went on ([`NamedReset::continued_under`]) — and
/// only in the conversation that holds this limit's notice row
/// ([`TurnEndReading::notice_elsewhere`], [`TurnEndState::observe`]).
fn notice_of(
    since: Option<&aterm_phase::SinceLimit>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    since.is_some_and(|s| names_reset(s.kind, s.reset.as_deref(), kind, said))
}

/// Whether a limit notice of kind `notice` whose words name the reset
/// `reset` (its reset text) is the notice of the limit `kind` whose reset
/// read `said`: of its class, naming the same clock ([`notice_of`] for the
/// screen's newest notice, [`WaitedLimit::named_by`] for a transcript's
/// row). Nothing ties to a reset no clock names.
fn names_reset(
    notice: WallKind,
    reset: Option<&str>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    class_of(notice) == class_of(kind) && said.is_some() && ResetClock::of(reset) == said
}

/// THE LIMIT THIS POLICY WAITS ON, by the reset its notice named
/// ([`TurnEndState::waited_limit`]): what a transcript's notice row is tied
/// to it by where the loop remembers no row id of its own
/// (`Session::served_in_transcript`: a loop that took the wait over from its
/// predecessor's ledger row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitedLimit {
    kind: WallKind,
    /// The reset, on the loop's clock.
    pub at: Instant,
    said: Option<ResetClock>,
}

impl WaitedLimit {
    /// Whether a limit notice of `kind` whose words name the reset `reset`
    /// (its reset text, [`aterm_phase::notice_reset`]) names THIS limit's
    /// reset: of its class, the same clock ([`names_reset`]).
    #[must_use]
    pub fn named_by(&self, kind: WallKind, reset: Option<&str>) -> bool {
        names_reset(kind, reset, self.kind, self.said)
    }
}

/// Whether the screen's newest limit notice (`since`,
/// [`aterm_phase::since_limit`]) is THE NOTICE OF THE LIMIT `kind` whose
/// reset read `said` ([`notice_of`]) and the limit ENDED under it: a turn was
/// served since it (the limit lifted before its reset, and the turn is work
/// since it), or the vendor's own continuation is drawn under it
/// ([`aterm_phase::SinceLimit::fired`]). What ends a REMEMBERED wall, its
/// rows gone from the screen (`supervise/wall_memory.rs`); the policy's own
/// fold reads the same two under the same tie ([`NamedReset::served_under`],
/// [`NamedReset::continued_under`]). Another limit's notice — an older one a
/// `/resume`d conversation redraws with turns served or the vendor's
/// continuation under it (the review of 2026-10-14 and W1 of
/// ATERM-KI-1015-1, the lab on 2.1.300: read as this one's, the
/// continuation was typed into the standing limit) — says nothing of this
/// one.
#[must_use]
pub fn ended_under(
    since: Option<&aterm_phase::SinceLimit>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    since.is_some_and(|s| s.served || s.fired) && notice_of(since, kind, said)
}

/// Whether the work a wall's retry led to is what the wall cuts off — a reply
/// past its output token maximum ([`ApiCause::Overlong`]), the long reply
/// itself — so that work does not end its episode ([`TurnEndState::observe`]).
fn work_is_its_cause(kind: WallKind) -> bool {
    matches!(
        kind,
        WallKind::ApiError {
            cause: ApiCause::Overlong,
            ..
        }
    )
}

/// Whether `message`, the notice a full context's wall stands on, is the vendor's own word that a `/compact` could not
/// lower the context (its head, `Compaction failed`, from the anchor table the canary checks against the binary).
/// `Error during compaction: <detail>` is a request that failed, and no word on the conversation. So too, from
/// 2.1.300, the vendor's word that there is nothing to compact: the full context's own notice for a conversation of
/// one exchange (`Prompt is too long · … A single-exchange conversation cannot be compacted; …`), and `/compact`'s
/// own answer on one, which is no wall (`Not enough messages to compact.`, [`TurnEndState::compact_answered`]).
fn compaction_exhausted_row(message: &str) -> bool {
    let lower = message
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let anchor = |id: &str| aterm_phase::anchor(id).to_lowercase();
    lower.starts_with(&anchor("wall.compaction_failed"))
        || lower.starts_with(&anchor("wall.not_enough_to_compact"))
        || lower.contains(&anchor("wall.cannot_be_compacted"))
}

/// WHAT FILLS A FULL CONTEXT beyond the conversation's words, by the vendor's
/// own notice (2.1.300): an image past the many-image request's size
/// (`wall.compact_images`), or images and attachments that pushed the request
/// past the API's size (`wall.compact_attachments`). Its notice is the full
/// context's wall, which `/compact` answers; where compaction cannot help,
/// the way back the owner is told is to before what fills it
/// ([`super::exception::Exception::context_of`]), not a few messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFill {
    /// An image past the many-image request's size.
    Images,
    /// Images and attachments past the API's request size.
    Attachments,
}

impl ContextFill {
    /// The fill notice `message` names, by the anchor table's words; `None`
    /// for every other notice.
    #[must_use]
    pub fn of(message: &str) -> Option<Self> {
        let lower = message.to_lowercase();
        let names = |id: &str| lower.contains(&aterm_phase::anchor(id).to_lowercase());
        if names("wall.compact_attachments") {
            Some(Self::Attachments)
        } else if names("wall.compact_images") {
            Some(Self::Images)
        } else {
            None
        }
    }
}

/// A model switch this policy made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSwitch {
    /// The bucket's model, as a `/model` alias (`None`: the notice named
    /// none this policy knows, and nothing is switched back).
    pub from: Option<String>,
    pub to: String,
    /// The bucket's reset (`None`: it named none).
    pub back_at: Option<Instant>,
}

/// How far past a turn's busy work a person's keystroke still counts as their
/// hand in it (the message sent just before the turn began).
pub const PERSON_TURN_SLACK: Duration = Duration::from_secs(30);

/// The longest answer that is still A PERSON'S OWN EXCHANGE
/// ([`TurnEndState::left_to_person`]): a question or a message of theirs,
/// answered within it — a thoughtful answer, one that looked things up — is
/// theirs to answer; a turn they opened that works longer is a job, and is
/// continued (the owner, 2026-10-15: "these auto prompts are for
/// continuously running jobs").
pub const EXCHANGE_WORK: Duration = Duration::from_secs(10 * 60);

/// An act typed and its point not judged yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Awaited {
    rule: &'static str,
    /// A slash command, whose effect needs no turn.
    command: bool,
    /// When it was typed, on the worker's clock: what the take window counts
    /// from ([`Self::due`]), which a sleep does not move (the worker slept
    /// too: [`TurnEndState::slept`]).
    at: Instant,
    /// When it was typed, on the policy's clock — moved by a sleep, as every
    /// stamp the wall clock reads is ([`TurnEndState::slept`]): what a time
    /// on the wall clock is set against (a done row's,
    /// [`TurnEndState::untaken_act_typed`]). Read off [`Self::at`] after a
    /// sleep of 30 minutes, an act typed at 10:00 read as typed at 10:30.
    typed: Instant,
    /// When a point after it first showed with no busy read since
    /// ([`TurnEndState::observe`]).
    unseen_since: Option<Instant>,
    /// A read since it was typed saw the agent's live turn (spinner or busy
    /// footer, not a background-work break; [`TurnEndState::took`]): the act
    /// was taken, judged or not ([`TurnEndState::act_untaken`]).
    taken: bool,
    /// A WALL ANSWERED IT: a reading since it was typed, no point yet (its
    /// take window open), showed a wall, and no read has seen it taken since
    /// ([`TurnEndState::observe`]). The point that judges it judges a retry
    /// the wall answered MET, whatever that point shows: its notice gone by
    /// the window's end (Esc on the vendor's new countdown) left the wall
    /// standing, and never says the retry got past it.
    met: bool,
    /// THE LIMIT A CANCEL LEFT on the screen the act was typed at
    /// ([`left_key`]), if one stood there: a notice the screen draws after
    /// the act is its answer only where it is not that one
    /// ([`TurnEndState::left_answer`]).
    left: Option<LeftKey>,
    /// The host's turn ([`RULE_HOST_TURN`]): which one it typed.
    host: Option<HostTurn>,
    /// OFF STOOD SINCE IT WAS TYPED ([`TurnEndState::park_off`]): the person
    /// drove the session meanwhile, so a point after it that no read saw
    /// busy says nothing of whose turn ended there ([`TurnEndState::observe`]).
    off_stood: bool,
    /// THE WALL'S OWN RETRY ([`is_wall_retry`], or an answer typed in a
    /// limit's place — the done check, the reversible answer, or under Land a
    /// land act: [`TurnEndState::acted`]): the wall's next appearance is the
    /// act's to count ([`TurnEndState::observe`]).
    wall_retry: bool,
    /// THE WORDS IT TYPED, hashed ([`crate::supervise::approvals::words_hash`]):
    /// what names its own `❯` row on the screen ([`ScreenAnswer::prompt`])
    /// and its prompt in the record ([`RecordTurn::prompt`]). `None` for an
    /// act whose words this state does not hold (a draft submitted, a
    /// command, the host's turn, one a loop before this one typed).
    said: Option<u64>,
    /// THE TURN THE SCREEN IT WAS TYPED AT CLOSED ([`ScreenAnswer::prompt`]
    /// there): the act's own row over an answer is news only where that turn
    /// was another prompt's. `None`: no answered prompt drawn there (a launch
    /// card, a notice, a tool's call), where nothing on the screen tells a
    /// turn drawn since from one redrawn.
    over: Option<u64>,
}

/// What tells one left limit's notice from another's: its class and the
/// reset its clock names ([`ResetClock`]).
type LeftKey = (WallClass, Option<ResetClock>);

/// THE LIMIT A CANCEL LEFT, by what the screen draws of it: the vendor's word
/// that its auto-continue will not go on (`auto`,
/// [`aterm_phase::AutoContinue::Unkept`]) under the newest notice of a limit
/// a reset ends (`since`, [`aterm_phase::since_limit`]: a usage window's or a
/// model bucket's, [`named_class`]) that names its reset and that no turn
/// has been served since. `None` for anything else. A bucket is such a limit
/// as a window is: read as a usage window's alone, a Sonnet bucket's notice
/// answering the loop's own try and cancelled before the loop's first read
/// was no answer, the try was judged a short turn, and `continue@v1 keep
/// going` went into the standing bucket at its two-minute back-off, two days
/// before its reset (the verifier of the successor's seed's refix, V21-1).
fn left_key(
    since: Option<&aterm_phase::SinceLimit>,
    auto: Option<aterm_phase::AutoContinue>,
) -> Option<LeftKey> {
    since
        .filter(|s| {
            auto == Some(aterm_phase::AutoContinue::Unkept)
                && !s.served
                && s.reset.is_some()
                && named_class(class_of(s.kind))
        })
        .map(|s| (class_of(s.kind), ResetClock::of(s.reset.as_deref())))
}

/// A LIMIT A CANCEL LEFT, read as the answer to a turn asked since the policy
/// last saw the screen ([`TurnEndState::left_answer`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftAnswer {
    /// The wall its notice was before the cancel ([`aterm_phase::left_wall`]).
    pub wall: aterm_phase::Wall,
    /// When the turn it answered was asked, at the earliest: the act
    /// awaited, else the policy's last look. Its notice, to be that turn's
    /// answer, was written no earlier.
    pub since: Instant,
    /// The SCREEN tells it from what the policy already saw: the notice it
    /// draws is not the one the act was typed over (else the one the last
    /// look drew). Where it is — the same limit, the same reset — a screen
    /// that has not moved since draws the same, and only a transcript row
    /// written since `since` tells an answer from it.
    pub screen_tells: bool,
}

impl Awaited {
    /// When its point is judged with no busy read, or it is escalated as
    /// not taken: [`TurnEndTiming::take_within`] from the first point that
    /// showed no busy read, else from the act.
    fn due(&self, timing: &TurnEndTiming) -> Instant {
        later(self.unseen_since.unwrap_or(self.at), timing.take_within)
    }
}

/// THE FIRST READ AFTER AN ACT A LOOP BEFORE THIS ONE TYPED, as its ledger
/// shows it ([`TurnEndState::seed_awaited`]): where that loop's take window
/// for the act began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRead {
    /// That loop read it so long ago (its wall's row,
    /// [`crate::supervise::approvals::AwaitedTry::read_ms`]).
    Read(Duration),
    /// Nothing has read it yet
    /// ([`crate::supervise::approvals::AwaitedTry::unread`]): this loop's
    /// first point is its first read, however late after the act it comes.
    Unread,
    /// The ledger does not say: the window counts from this loop's first
    /// point, the act typed under the window ago.
    Unknown,
}

/// The continuation a limit reset left owed ([`TurnEndState::reset_told`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetResume {
    /// When the outcome was told: work that began at or after it pays the
    /// continuation, work that began before it is the turn that met the wall.
    told: Instant,
    /// From when the continuation is typed.
    due: Instant,
}

/// What the policy remembers of a Land episode ([`TurnEndState::land`]): from
/// the first reading under [`Autopilot::Land`] to the first under any other
/// mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LandEpisode {
    /// [`LAND`] was typed by this policy ([`TurnEndAction::Type`] under
    /// [`RULE_LAND`]; a draft submitted in its place is someone's own words).
    typed: bool,
    /// [`LAND_NUDGE`]s typed since ([`LAND_NUDGES`] at most).
    nudges: u32,
    /// THE EPISODE ENDED QUIET: its nudges spent ([`TurnEndState::land_spent`])
    /// and its last land act answered with no done reply — the harness types
    /// nothing more of it but a question's [`LAND_ANSWER`]. Set at the point
    /// that answers a land act no wall cut ([`TurnEndState::observe`]), or
    /// where the host's own line took the place of the awaited answer
    /// ([`TurnEndState::host_typed`]); cleared where a land act's done reply
    /// LANDS the task, which says the opposite. Read by the task's face alone
    /// ([`TaskFace::of`], the Autopilot design's §9.4.2): nothing the policy
    /// types turns on it.
    quiet: bool,
}

/// AN EPISODE OF USAGE CREDITS CARRYING THE SESSION, as the policy's points
/// read it ([`TurnEndState::fold_credits`]): what the credits hold waits on
/// ([`credits_hold`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CreditsEpisode {
    /// The reset of the window they carry past, as a reading last named it
    /// ([`CreditsCarry::reset_at`]); `None`: none named.
    reset: Option<Instant>,
    /// When it began ([`CreditsCarry::since`], else its first point; a turn
    /// served past its end on credits starts it again): the hold with no
    /// reset named lifts [`TurnEndState::credits_bound`] past it.
    since: Option<Instant>,
    /// The model whose requests they carry ([`CreditsCarry::model`]): an
    /// episode of that model's, which no read without the notice ends — it
    /// is drawn once a process — but the session leaving the process
    /// ([`TurnEndState::agent_started`], [`TurnEndState::restarted`]).
    /// `None`: the account's window.
    model: Option<String>,
    /// A reset this episode waited out, with a turn served past it under the
    /// notice still drawn: the notice is that turn's answer's (Claude Code
    /// draws it from the newest answer's headers), so credits carry the
    /// session past the window that reset, and a reading that names that
    /// reset again (the loop keeps the started notice's while the screen
    /// says credits) names nothing new.
    passed: Option<Instant>,
    /// The hold's escalation was decided ([`TurnEndState::credits_told`]):
    /// once an episode.
    told: bool,
}

/// How far past a reset an episode waited out a reading's reset must be to
/// name another window's ([`CreditsEpisode::passed`]): the same notice's
/// reset placed again moves by the loop's clocks at most.
const SAME_CREDITS_RESET: Duration = Duration::from_secs(60);

/// What the credits hold holds at a point ([`TurnEndState::credits_held`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct CreditsHeld {
    /// The reset they wait for; `None`: none named.
    reset: Option<Instant>,
    /// The model whose requests they carry; `None`: the account's window.
    model: Option<String>,
    /// When the hold lifts; `None`: by no clock (a model's, none named).
    lifts: Option<Instant>,
    /// The episode's owner was told ([`TurnEndState::credits_told`]).
    told: bool,
}

/// The act a `/compact` owes its continuation ([`TurnEndState::compact_owes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactOwes {
    /// The done check or the challenge ([`TurnEndState::checking`]).
    check: bool,
    /// A land act ([`TurnEndState::landing`]).
    land: bool,
}

/// What the policy remembers of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndState {
    pub timing: TurnEndTiming,
    /// THE SUCCESS STATE OF `/login` WAS ANSWERED: its one Enter
    /// ([`TurnEndAction::Confirm`]) went, and the dialog has not left since
    /// ([`Self::observe`] clears it at any other screen). Read again with it
    /// standing, the success is the owner's to look at: never a second Enter.
    login_confirmed: bool,
    /// When each typed act went, within the window.
    acts: VecDeque<Instant>,
    /// The wall acts typed within the window, by rule
    /// ([`WALL_ACTS_PER_WINDOW`]).
    wall_acts: VecDeque<(Instant, &'static str)>,
    /// The act typed last — this policy's, or a turn its host typed
    /// ([`Self::host_typed`]) — until the next point shows what it did.
    awaiting: Option<Awaited>,
    /// The act the current point follows (`None`: someone else's turn),
    /// retaining a check's origin when a wall's retry delivers it again.
    followed: Option<&'static str>,
    /// The busy work of the turn that ended at the current point.
    point_worked: Duration,
    /// Short turns in a row ([`TurnEndTiming::min_work`]): this policy's
    /// acts' yields and anyone else's turns alike.
    short: u32,
    /// When the current point first showed: the back-off counts from it.
    point_at: Option<Instant>,
    /// How many points this policy has judged: bumped wherever
    /// [`Self::point_at`] is set ([`Self::observe`]), and moved by nothing
    /// else — the point's identity ([`Self::point_number`]), which a sleep
    /// that moves `point_at` back ([`Self::slept`]) leaves as it is.
    points: u64,
    /// WHEN THE DONE STANDING NOW WAS DECIDED, on the loop's clock: the read
    /// that first showed it ([`Self::done_shown`]); `None` while no done
    /// stands.
    done_at: Option<Instant>,
    /// WHEN THE POLICY FIRST READ THE PERSON'S ESC that stopped the turn now
    /// showing ([`TurnEndReading::interrupted`], no Esc of the harness's own,
    /// [`Self::observe`]): their grace counts from it ([`decide_turn_end`]),
    /// never from the point's first showing. A box that came up with no busy
    /// read before it (the stub answers at once; a tool call asked in a
    /// moment) moves no point, so a person's refusal of it was held from a
    /// point minutes old: `keep going` typed 0.6 s after the Esc (the Gemini
    /// CLI review of 2026-10-16). A turn read run since, or a box read up
    /// since ([`Self::box_up`]), makes the next Esc read a new one.
    esc_at: Option<Instant>,
    wall: Option<WallTrack>,
    /// The typed kind Claude Code wrote with the notice its transcript ended
    /// on at the last decision's read (its `apiError`, else its `error`;
    /// [`Self::notice_typed_was`]): what names a refused request's remedy
    /// (`policy/exception.rs`). `None`: no transcript read, or no notice.
    notice_typed: Option<String>,
    /// When the agent's process started, on this policy's clock, as the loop
    /// last read it from its host ([`Self::agent_started`]; `None`: unread):
    /// a countdown armed before it is no countdown of this process's.
    agent_start: Option<Instant>,
    /// The same start in unix seconds ([`Self::agent_started_unix`]): what a
    /// notice row's own written time is compared with ([`NamedReset::written`]).
    agent_start_unix: Option<i64>,
    /// A command's rule whose continuation is owed at the next point.
    after: Option<&'static str>,
    model: Option<ModelSwitch>,
    /// The fallback relaunch for the bucket being worked could not be made
    /// ([`TurnEndState::restarted`]): its reset is waited out instead.
    fallback_unmade: bool,
    /// THE FALLBACK'S RELAUNCH WAS MADE and no turn has been asked since
    /// ([`Self::bucket_left`]): no act of this policy's, no turn of the
    /// host's, and every point since the relaunched process's redraw of the
    /// bucket's notice.
    fallback_unasked: bool,
    /// When the fallback's relaunch was made, unix seconds (the loop's
    /// restart, [`Self::fallback_made_at`]; a successor's seed: the
    /// relaunch's row, [`Self::seed_fallback_unasked`]).
    fallback_unix: Option<i64>,
    /// When the RELAUNCHED PROCESS started, unix seconds: the first start
    /// the host names at or after [`Self::fallback_unix`]
    /// ([`Self::agent_started_unix`]), kept against a later relaunch's.
    /// `None` until one is named.
    fallback_started: Option<i64>,
    /// When the transcript's notice this decision read was written, unix
    /// seconds (`supervise/wall_confirm.rs`, [`Self::notice_written_at`]):
    /// `None` where no transcript was read, it ends on no notice, or its row
    /// names no time.
    notice_unix: Option<i64>,
    /// THE ACCOUNT MOVE THIS SESSION MADE and no point has answered yet
    /// ([`Self::restarted`], [`Self::account_answer`]): from the relaunch
    /// until a turn asked under the new login is served, or — after a wall or
    /// a lost login answered it — until the worker works again. While it
    /// stands no move is asked AHEAD of a wall (the move made is answered
    /// first), and a lost login is the move's, not the person's
    /// ([`login_wall_moves`]).
    account: Option<AccountMove>,
    /// A turn has been asked of the agent since that move — an act of this
    /// policy's, a turn the host typed, a turn of anyone's that ran, whatever
    /// it ended on: only then does a usage limit or a lost login on the
    /// screen say anything about the NEW account (a relaunch resumes the
    /// conversation, and what it repaints of the old process's notice was
    /// never measured: [`Self::login_left`]).
    account_asked: bool,
    /// The host was told what answered the move ([`Self::account_told`]):
    /// never told twice.
    account_told: bool,
    /// The relaunch of that move — or one made while it stands (the memory
    /// banner's, a model's fallback or its return), or the agent a loop
    /// begun on a told move takes ([`Self::seed_account`]) — has shown no
    /// point yet: the FIRST point after it is the resumed process's own
    /// start-up, whatever work it shows, and no turn anyone asked
    /// ([`Self::observe`]).
    account_startup: bool,
    /// THE WALL'S RESUME IS OWED: the move was made AT A WALL
    /// ([`AccountAt::Wall`]) — not over a move of the session's own that
    /// owed none ([`Self::restarted`]) — and no turn has been asked under the
    /// new login since. The continuation the wall would have had at its
    /// reset is typed at the relaunched session's first point
    /// ([`act_at_point`], [`wall_action`]) — under `resume_limits`, by
    /// [`RULE_LIMIT_RESUME`], whatever `continue_policy` says. An act of this
    /// policy's or a turn of anyone's pays it.
    account_resume: bool,
    /// That resume was SEEDED from the ledger ([`Self::seed_account`]) and
    /// no point has been folded in since: owed only if the first point this
    /// state sees is the notice the session left ([`Self::observe`]).
    account_resume_seed: bool,
    /// The owner's clearing ([`AccountAdvice::Resume`]'s `cleared_at`) this
    /// policy last typed the wall's continuation for — or the unix second a
    /// loop before this one typed it ([`Self::seed_account_resumed`]): every
    /// clearing at or before it is answered, one continuation a clearing
    /// ([`account_first`]).
    account_resumed: Option<i64>,
    /// The act [`Self::acted`] last recorded was the limit's try
    /// ([`Self::limit_try`]).
    limit_try: Option<LimitTry>,
    /// A LIMIT'S TRACK ENDED at the last fold, by no act of this policy's
    /// ([`Self::observe`], [`Self::limit_ended`]).
    limit_ended: bool,
    /// A LIMIT RESET'S CONTINUATION IS OWED ([`Self::reset_told`]): the
    /// step the loop made under the usage-limit dialog ended, and the session
    /// is to be continued once — by [`RULE_LIMIT_RESUME`] under
    /// `resume_limits`, whatever `continue_policy` says ([`act_at_point`],
    /// [`wall_action`]) — from the instant it names. Paid by any act typed, a
    /// turn the host typed, and any work seen since the outcome was told
    /// ([`Self::took`], [`Self::observe`]).
    reset_resume: Option<ResetResume>,
    /// An account move asked for could not be made — the host refused it, or
    /// the relaunch failed ([`Self::restarted`]): not asked again while the
    /// wall being worked stands. THE LATCH IS THAT WALL'S: cleared where
    /// [`Self::fallback_unmade`] is (a new wall, the wall gone with the
    /// worker working or a lost login's `Login successful` read), where a new
    /// episode of the same class begins after real work ([`Self::observe`]),
    /// by a move made, and by the accounts' news ([`Self::account_news`]).
    account_unmade: bool,
    /// That latch was SEEDED from the ledger ([`Self::seed_account_unmade`])
    /// and no point has been folded in since: the first wall this state sees
    /// is the wall the earlier loop left standing, and takes the latch up
    /// where a wall of this loop's own finding would clear it.
    account_unmade_seed: bool,
    /// The label of a move asked AHEAD of a wall that could not be made, and
    /// when: not asked again for that label — the refusal is the launch's (a
    /// flag the relaunch cannot carry, a line that cannot hold the login) or
    /// the roster's, and neither changes from one turn end to the next,
    /// where the wall's latch would be cleared by the very turn that
    /// follows. A move made, or advice naming another account, is a new ask
    /// — and so is the same account once the window has passed
    /// ([`Self::account_ahead_refused`]): no wall stands here whose end
    /// would end the latch, and a refusal of the moment (a store that could
    /// not be read, a restart of another kind that was in flight) kept for
    /// the life of the loop left the owner's `accounts use` unheard by this
    /// one tab.
    account_ahead_unmade: Option<(String, Instant)>,
    /// When each account move this session MADE went, within the window
    /// ([`ACCOUNT_MOVES_PER_WINDOW`]).
    account_moves: VecDeque<Instant>,
    /// The `❯` row on the screen is an act of this policy's the worker never
    /// took (judged so at its deadline): a point, though input waits.
    stale_input: bool,
    /// Done reports in a row ([`says_done`]): `1` gets the [`DONE_CHECK`];
    /// `2` means the task is done ([`Self::task_done`]) until someone else's
    /// turn.
    done: u8,
    /// The act awaited is the [`DONE_CHECK`] as this policy typed it (a draft
    /// submitted in its place is someone's own words, never the check), or
    /// a wall's retry that delivers it again ([`Self::cut_check`]).
    checking: bool,
    /// THE CHECK A LOOP BEFORE THIS ONE TYPED, TAKEN UP BY THIS ONE
    /// ([`Self::seed_done_check`]): the done check or the open-items
    /// challenge, whose done reply ends the task under its own rule — the
    /// act its point follows ([`Self::done_after_challenge`]) — as for the
    /// loop that typed it. Read as someone else's turn's end, a done reply
    /// to the challenge was journaled under the done check's rule
    /// (ATERM-KI-1016-1). Cleared by any act of this loop's but the wall's
    /// retry that delivers it again, and by a turn the host types.
    seeded_check: Option<&'static str>,
    /// The task ended by the worker's own [`FINISHED`] token (not the check).
    finished: bool,
    /// THE OPEN WORK A DONE REPORT NAMES at the current point
    /// ([`open_work`]): the [`RULE_OPEN_ITEMS`] challenge is owed there, in
    /// place of the end the worker's word would make ([`Self::observe`]).
    /// Judged at a point a read saw run (an act's answer, a turn of
    /// anyone's) and at a first point no loop judged, kept while that point
    /// is read again, and cleared by the next such point and by any act typed
    /// ([`Self::acted`], [`Self::host_typed`]). At every such point: no job
    /// is challenged once and let go.
    open: Vec<&'static str>,
    /// WHAT THE SESSION'S REPOSITORY HELD THAT NO REMOTE HAS at the current
    /// point ([`TurnEndReading::repo`]): the [`RULE_UNCOMMITTED`] challenge
    /// is owed there, as [`Self::open`]'s is, judged where a read saw the
    /// turn run, and cleared as it is.
    repo: Option<crate::harness::repo_work::RepoWork>,
    /// THE LAND EPISODE in hand ([`Autopilot::Land`]): whether its
    /// instruction was typed and how many nudges followed. Forgotten by any
    /// reading in another mode ([`Self::observe`], [`Self::acted`]).
    land: LandEpisode,
    /// The act awaited is one of Land's, typed by this policy once the
    /// episode's instruction was ([`Self::acted`]), or a wall's retry that
    /// delivers one again ([`Self::cut_land`]): its yield saying done ends
    /// the task, as the done check's second done report does.
    landing: bool,
    /// The task ended on a done reply to a land act ([`Self::landed`]).
    landed: bool,
    /// THE ACT A WALL CUT ([`Self::observe`]): the done check
    /// ([`Self::checking`], or the restart's carry-on that asks it,
    /// [`HostTurn::Check`]) was awaited and its point showed a wall. The
    /// wall's retry typed next delivers it again ([`Self::acted`]), so the
    /// retry's done reply is the check's. Kept until an act is typed or
    /// someone's turn ends; the same point read again keeps it.
    cut_check: bool,
    /// As [`Self::cut_check`], for a land act ([`Self::landing`]): the
    /// retry's done reply is landed, so a wall never costs a landing a nudge
    /// (or, on the last nudge's turn, the landing itself).
    cut_land: bool,
    /// THE TEXT OF THE DONE CHECK, THE CHALLENGE OR THE LAND ACT THIS POLICY
    /// TYPED LAST, kept while a wall's retry delivers it again
    /// ([`Self::cut_check`], [`Self::cut_land`]) and through a `/compact` that
    /// owes it ([`Self::compact_owes`]): what the compaction's continuation
    /// types again. `None` after any other act, and for a check the host
    /// typed ([`HostTurn::Check`]).
    cut_text: Option<String>,
    /// A `/COMPACT` TYPED WHERE A FULL CONTEXT CUT THE DONE CHECK (or its
    /// kin the challenge) OR A LAND ACT ([`RULE_COMPACT`]): the continuation
    /// owed after it types that act again ([`Self::cut_text`]) in place of
    /// `keep going`, under the compaction's rule, and its answer is that
    /// act's (ATERM-KI-1016-1, done-3). The compaction's summary may drop the
    /// check, so a `keep going` after it delivered none, and its done reply
    /// was a first done report — one check more. Kept through a `/compact`
    /// typed again on the context's ladder; any other act drops it.
    compact_owes: Option<CompactOwes>,
    /// THE DONE CHECK'S TURN'S WORK SO FAR, where a wall cut it
    /// ([`Self::cut_check`]): the wall's retry delivers the check again, and
    /// its answer's work adds to this, so a check's turn a wall split is
    /// measured whole where its answer is an idle report alone
    /// ([`check_answer_ends`]). Zero at every other point.
    check_work: Duration,
    /// That token was read OVER THE AGENT'S OWN BACKGROUND WORK
    /// ([`Self::finished_over_background`]) and no turn of anyone's has run
    /// since ([`Self::took`]): the point it made stands while the work runs,
    /// through the agent's own wake when the work ends ([`Self::live_turn`]),
    /// and the screen the work or the wake leaves is that point still
    /// ([`Self::observe`]).
    background_done: bool,
    /// A live turn since that point was the agent's OWN WAKE
    /// ([`Self::live_turn`]): later reads of it with no opener on the screen
    /// are the wake's still.
    woken: bool,
    /// A MANAGER WAITS ON THIS SESSION: it is a worker, its task record read
    /// by the loop ([`Self::set_managed`]). Its done check and its answers
    /// go without the short back-off for its first [`MANAGED_SHORT_TURNS`]
    /// short turns in a row ([`continue_policy`]).
    managed: bool,
    /// THE CURRENT POINT'S REPLY IS A WALL'S ([`Self::observe`]): the act
    /// awaited, or someone's turn, ended at a wall, so its answer was never
    /// read and the done reports stand as the point before left them. Set
    /// afresh where [`Self::cut_check`] is, and kept by the same point read
    /// again and by a relaunched agent's first point no read saw run (a
    /// restart made at the wall). No act typed clears it: the next point
    /// that answers one does. A reply a wall cut never said done
    /// ([`Self::said_done`]).
    cut_reply: bool,
    /// The current point ends A PERSON'S OWN EXCHANGE ([`Self::observe`]):
    /// their message, answered within [`EXCHANGE_WORK`], on a task this
    /// policy was not carrying — or a wall's act that delivered it again,
    /// the answer's work in all still within it ([`Self::left_to_person`]).
    exchange: bool,
    /// The work of the person's exchange so far: their turn's, plus each
    /// wall's act that delivered it again ([`Self::exchange`]).
    exchange_work: Duration,
    /// THIS POLICY IS CARRYING THE TASK: an act of its own (or a previous
    /// loop's, [`Self::seed_acts`]) has been answered since the task began,
    /// and the task is not done. A person's turn meanwhile, however short, is
    /// a check-in, not an exchange of their own. Ended by the task's end, by a
    /// person's Esc on the act, by someone else's turn where the policy
    /// was not carrying it, and where the policy stopped driving it: a
    /// landing spent with no done ([`Self::land_spent`], under Land), and
    /// Off standing between two points ([`Self::park_off`]).
    carrying: bool,
    /// THE CARRY A LOOP STARTED AS ITS SESSION'S OFF LIFTED HOLDS FOR A TURN
    /// ALREADY RUNNING AS IT ATTACHES ([`Self::start_lifted_off`]): the acts
    /// seeded from the ledger carried the task, and a live read before any
    /// point ([`Self::took`]) gives it back for that turn; the first point
    /// judged ends it.
    carry_at_attach: bool,
    /// THE POLICY'S LAST LOOK: when it last folded a reading in
    /// ([`Self::observe`]), and the limit a cancel left on that screen if one
    /// stood there ([`left_key`]). A cancelled limit the screen draws after
    /// it answered a turn asked since — a person's, the host's, the one an
    /// act of ours led to ([`Self::left_answer`]).
    looked: Option<(Instant, Option<LeftKey>)>,
    /// A LIVE TURN OF SOMEONE ELSE'S HAS BEEN READ SINCE THE LAST POINT
    /// ([`Self::took`] with no act of this policy's awaited — the host's own
    /// line counts as the policy's): the task's face is open again, or still
    /// spent, however the last point ended ([`TaskFace::of`]; the Autopilot
    /// design's §9.4.2). The policy changes `done` and `landed` only when a
    /// turn ENDS, so without it `✓ landed` stood through a person's new task
    /// for as long as it ran. Cleared at the next point. Read by nothing the
    /// policy decides.
    other_turn: bool,
    /// THE LANDING A LOOP BEFORE THIS ONE LANDED ([`Self::seed_landed`]),
    /// held for this state's first point: `Some(episode)` while armed, where
    /// `episode` is whether the land episode's instruction counts as typed
    /// once the seed holds (an Off since takes it away, [`Self::park_off`]).
    landed_seed: Option<bool>,
    /// THE MARK OF THE LANDING'S WORDS ([`said_mark`]), as the journal kept
    /// it at the landing's point, beside an armed [`Self::landed_seed`]:
    /// the seed holds only at a first point whose last words still mark
    /// the same. `None`: a landing a build before the mark wrote, held as
    /// it was.
    landed_words: Option<u64>,
    /// THE DONE A LOOP BEFORE THIS ONE READ ([`Self::seed_done`]), held for
    /// this state's first point: `Some(words)` while armed, `words` the mark
    /// of that done's last words ([`said_mark`]) as the journal kept it at
    /// its point (`None`: a done a build before the mark wrote). Read by the
    /// face alone ([`TaskFace::of`]): nothing the policy decides reads it.
    done_seed: Option<Option<u64>>,
    /// THE SEEDED DONE WAS WITHDRAWN at this state's first point: its last
    /// words were another reply's ([`Self::seed_done`]), so a done read
    /// there is a new one ([`Self::face_withdrawn`]).
    done_withdrawn: bool,
    /// THE WALL TIME (unix ms) THE JOURNAL NOTED THE SEEDED END AT
    /// ([`Self::seed_noted_at`]), beside an armed [`Self::landed_seed`] or
    /// [`Self::done_seed`]: told with the start face where that face is the
    /// seed's ([`Self::face_start_noted`]).
    seed_noted_ms: Option<i64>,
    /// THE END THIS STATE HOLDS RESTATES A SEED, and when that end was first
    /// noted ([`Self::restated_noted_ms`]): set at the first point that
    /// takes a landing's seed up or keeps a done's, from
    /// [`Self::seed_noted_ms`]; gone at the first face told after it
    /// ([`Self::face_moved`], [`Self::face_withdrawn`]) and at a live read
    /// ([`Self::took`]).
    restating: Option<(TaskFace, i64)>,
    /// The task's face as the loop last told its host
    /// ([`Self::face_moved`], [`Self::face_start`]).
    face_told: Option<TaskFace>,
    /// THE LIMIT'S TRY A LOOP BEFORE THIS ONE MADE IS OVER for this one
    /// ([`Self::seed_usage_attempt`]: a try nobody read, its appearance
    /// older than its next rung; one a loop read met again is taken up
    /// however old, [`Self::seed_met_attempt`]), and no act of this one's
    /// has followed: the ledger is told so before this loop's first wall
    /// row ([`Self::take_try_over`]).
    try_over: bool,
    /// THE LIMIT'S TRY A LOOP BEFORE THIS ONE MADE THAT NO LOOP HAS READ
    /// ([`Self::seed_usage_attempt`]), its answer this loop's first decision
    /// read dates ([`Self::date_unread_try`]); `None` once that read is made,
    /// or for a try met again.
    unread_try: Option<UnreadTry>,
    /// THE ACT A LOOP BEFORE THIS ONE TYPED WAS ANSWERED ALREADY: the done
    /// check, or a land act, whose answer that loop judged the task's end
    /// by ([`Self::seed_done_check`], [`Self::seed_land`]). Its answer's
    /// screen still ends the task at this loop's first point, and a turn read
    /// busy since is a later one ([`Self::took`]).
    seed_answered: bool,
    /// THE POINT A LOOP BEFORE THIS ONE JUDGED LAST, when (this loop's clock):
    /// its record is the ledger's last word on the streak
    /// ([`Self::seed_point`]). Taken at this loop's first judged point.
    point_seed: Option<Instant>,
    /// THE TURN A LOOP BEFORE THIS ONE READ RUNNING after the point it
    /// recorded last, and stopped before its end: when it was read running
    /// and when that point was (this loop's clock; [`Self::seed_ran_turn`]).
    /// Taken at this loop's first point, dropped by a busy read of its own.
    ran_turn_seed: Option<(Instant, Instant)>,
    /// A PERSON'S ESC STOOD AT [`Self::point_seed`] as the loop that judged
    /// it recorded ([`Self::seed_esc`]): that point read again, the Esc's
    /// first read is the point's, not this loop's attach ([`Self::esc_at`]).
    esc_seed: bool,
    /// The point judged last moved what its record holds ([`Self::take_point_moved`]).
    point_moved: bool,
    /// A LOST LOGIN ENDED BACK at the reading folded last: its track gone
    /// (`Login successful`, the worker's work past it, its retry taken), to
    /// be told to the host once ([`Self::take_login_returned`]).
    login_returned: bool,
    /// THE ACT A LOOP BEFORE THIS ONE TYPED LAST, ITS YIELD STILL TO BE
    /// JUDGED ([`Self::seed_yield`]): whether it is a continuation
    /// ([`is_continue_rule`]). Taken at this loop's first judged point.
    yield_seed: Option<bool>,
    /// The point judged last ENDED A PERSON'S OWN EXCHANGE, left to them
    /// ([`Self::left_judged`]).
    left_judged: bool,
    /// THE LEDGER'S LAST WORD ON THE STREAK IS A POINT'S RECORD
    /// ([`crate::supervise::approvals::RULE_POINT`]), or it holds no word on
    /// the streak yet (a loop's start on books with none), and no turn was
    /// read running since ([`Self::took`]).
    point_last: bool,
    /// The busy read just folded in ([`Self::took`]) was the first since the
    /// point recorded last ([`Self::take_ran`]).
    ran: bool,
    /// THE END OF A TURN THE LOOP BEFORE THIS ONE READ RUNNING is this loop's
    /// first point ([`Self::seed_ran`], [`Self::seed_try_taken`]): taken
    /// there ([`Self::take_gap`]).
    gap: bool,
    /// USAGE CREDITS CARRY THE SESSION ([`CreditsEpisode`]): from the first
    /// point a notice says so to the first that draws the composer with no
    /// such notice ([`Self::fold_credits`]).
    credits: Option<CreditsEpisode>,
}

impl Default for TurnEndState {
    fn default() -> Self {
        Self::new(TurnEndTiming::default())
    }
}

impl TaskFace {
    /// THE TASK'S FACE, a pure function of the policy's memory (the Autopilot
    /// design's §9.4.2), taken in order:
    ///
    /// 1. `other_turn` — a live turn this policy did not start has been read
    ///    since its last point ([`TurnEndState::other_turn`]): [`Self::Spent`]
    ///    if the land episode ended quiet, else [`Self::Open`];
    /// 2. a person's own exchange ([`TurnEndState::left_to_person`]):
    ///    [`Self::Theirs`];
    /// 3. a done reply to a land act ([`TurnEndState::landed`]), or a landing
    ///    a loop before this one landed, armed for this loop's first point
    ///    ([`TurnEndState::seed_landed`]): [`Self::Landed`];
    /// 4. the task done otherwise — the done check, the worker's own
    ///    [`FINISHED`] — or a done a loop before this one read, armed for
    ///    this loop's first point ([`TurnEndState::seed_done`]):
    ///    [`Self::Done`];
    /// 5. the land episode ended quiet — its nudges spent and its last land
    ///    act answered with no done: [`Self::Spent`];
    /// 6. else [`Self::Open`].
    ///
    /// Landed and done are apart: a task the done
    /// check or `FINISHED` ended was never asked to verify, commit and push.
    /// The episode ends quiet once its last nudge's turn ENDED with no done —
    /// while that turn runs, or a wall's retry owes it again, its reply may
    /// still land it — and a done reply that lands it after all (a question's
    /// land answer) ends the quiet: someone else's turn after a landing is
    /// open, whichever land act landed it. The seed is the landing told at
    /// the loop's start: the first point takes it up (no move) or drops it
    /// (open), so a restart restates the landing rather than announcing it
    /// again; a done's seed restates the done the same way, so the window's
    /// clock keeps the time the task was done, not the restart's.
    #[must_use]
    pub fn of(state: &TurnEndState, other_turn: bool) -> TaskFace {
        let spent = state.landing_spent();
        if other_turn {
            return if spent {
                TaskFace::Spent
            } else {
                TaskFace::Open
            };
        }
        if state.left_to_person() {
            return TaskFace::Theirs;
        }
        if state.landed() || state.landed_seed.is_some() {
            TaskFace::Landed
        } else if state.task_done() || state.done_seed.is_some() {
            TaskFace::Done
        } else if spent {
            TaskFace::Spent
        } else {
            TaskFace::Open
        }
    }
}

impl TurnEndState {
    #[must_use]
    pub fn new(timing: TurnEndTiming) -> Self {
        Self {
            timing,
            login_confirmed: false,
            acts: VecDeque::new(),
            wall_acts: VecDeque::new(),
            awaiting: None,
            followed: None,
            point_worked: Duration::ZERO,
            short: 0,
            point_at: None,
            points: 0,
            done_at: None,
            esc_at: None,
            wall: None,
            notice_typed: None,
            agent_start: None,
            agent_start_unix: None,
            after: None,
            model: None,
            fallback_unmade: false,
            fallback_unasked: false,
            fallback_unix: None,
            fallback_started: None,
            notice_unix: None,
            account: None,
            account_asked: false,
            account_told: false,
            account_startup: false,
            account_resume: false,
            account_resume_seed: false,
            account_resumed: None,
            limit_try: None,
            limit_ended: false,
            reset_resume: None,
            account_unmade: false,
            account_unmade_seed: false,
            account_ahead_unmade: None,
            account_moves: VecDeque::new(),
            stale_input: false,
            done: 0,
            checking: false,
            seeded_check: None,
            finished: false,
            open: Vec::new(),
            repo: None,
            land: LandEpisode::default(),
            landing: false,
            landed: false,
            cut_check: false,
            cut_land: false,
            check_work: Duration::ZERO,
            cut_text: None,
            compact_owes: None,
            background_done: false,
            woken: false,
            managed: false,
            cut_reply: false,
            exchange: false,
            exchange_work: Duration::ZERO,
            carrying: false,
            carry_at_attach: false,
            looked: None,
            other_turn: false,
            landed_seed: None,
            landed_words: None,
            done_seed: None,
            done_withdrawn: false,
            seed_noted_ms: None,
            restating: None,
            face_told: None,
            try_over: false,
            unread_try: None,
            seed_answered: false,
            point_seed: None,
            ran_turn_seed: None,
            esc_seed: false,
            point_moved: false,
            login_returned: false,
            yield_seed: None,
            left_judged: false,
            point_last: true,
            ran: false,
            gap: false,
            credits: None,
        }
    }

    /// A model switch a previous loop on the session made and did not undo
    /// (the approval ledger's open `/model` row): switched back at its reset
    /// as if this loop had made it. A switch of this loop's own wins.
    pub fn seed_model(&mut self, switch: ModelSwitch) {
        if self.model.is_none() {
            self.model = Some(switch);
        }
    }

    /// The account moves a previous loop on the session MADE, `ages` ago
    /// (the approval ledger's `typed` rows of [`RULE_ACCOUNT`]): counted by
    /// [`ACCOUNT_MOVES_PER_WINDOW`] like this loop's own while they are
    /// within the window — a loop its host starts again (each seamless
    /// update, each fault) is no fresh four.
    pub fn seed_account_moves(&mut self, ages: &[Duration], now: Instant) {
        let mut at: Vec<Instant> = ages
            .iter()
            .filter(|&&a| a < self.timing.window)
            .filter_map(|&a| now.checked_sub(a))
            .collect();
        at.extend(self.account_moves.drain(..));
        at.sort_unstable();
        self.account_moves = at.into();
        while self.account_moves.len() > 4 * ACCOUNT_MOVES_PER_WINDOW {
            self.account_moves.pop_front();
        }
    }

    /// An account move a previous loop on the session made, `ago` ago, that
    /// its ledger does not show served (the approval ledger's open `relaunch
    /// under account …` row): still to be answered, as if this loop had made
    /// it — `asked` where a turn has been asked under the new login since
    /// (a `typed` row follows it), `told` where the host was told of a wall
    /// or a lost login that answered it (never told twice, by any loop). A
    /// move of this loop's own wins, and one older than the window is
    /// history — WHERE NO BAD ANSWER OF IT WAS TOLD BY A BUILD THAT TELLS
    /// ITS SERVICE (`told_closes`: the told row says so,
    /// [`crate::supervise::approvals::account_answer_reason`]). A build
    /// before that one closed a told move silently, so its ledger never
    /// shows one served: such a told row is history past the window as an
    /// untold one is — held, it may be a move the worker went past days ago.
    /// A told move stands in one loop until the worker works past it
    /// ([`Self::observe`]), however long the wall it met stands, and the
    /// wait at a lost login has no end until the owner signs in: a loop
    /// started on it hours on (the agent relaunched after it crashed there,
    /// a seamless update, a fault) holds it as well — and waits at that lost
    /// login, asking the host, where with no move standing it typed `/login`
    /// into a tab nobody is at.
    ///
    /// THE WALL'S RESUME ([`Self::account_resume_owed`]) of a move made AT A
    /// WALL (`at`) that the ledger shows neither asked nor told is taken up
    /// AS FAR AS THE NOTICE GOES: owed if the first point this state sees
    /// draws the notice the session left with no turn served under it
    /// ([`Self::observe`]) — the usage limit read as the wall; the session's
    /// newest usage-limit notice with nothing served below it, though no
    /// wall is read (the vendor's `Automatic continue cancelled` under it is
    /// the last thing said); a lost login no turn has been asked under
    /// ([`Self::login_left`]). The loop started right after the relaunch, or
    /// takes the agent a host's carry landed (the relaunch typed nothing),
    /// and the resumed conversation repaints that notice: not owed there,
    /// the session waited out THAT account's reset, or under `continue_policy
    /// = false` was never resumed at all. At a first point that draws a turn
    /// served since the notice — a person's, between the loops — or no
    /// notice, it is not taken up: a person's turn leaves no row to say
    /// whether it was the resume's.
    ///
    /// A MOVE THE LEDGER SHOWS NEITHER ASKED NOR TOLD is taken up as this
    /// loop's own relaunch is ([`Self::restarted`]): the first point this
    /// state sees is the resumed process's start-up
    /// ([`Self::account_startup`]) — this loop may have begun while it drew
    /// its conversation, read busy — so work shown there is no turn that ran
    /// under the new login, and the notice it repaints strikes nothing. A
    /// TOLD answer was a turn that ran into the wall it told of, so the move
    /// counts as asked ([`Self::account_asked`]): a person's turn writes no
    /// `typed` row, and one loop holds asked after that tell. ITS FIRST
    /// POINT IS A START-UP TOO: the loop begun on the agent relaunched at
    /// the told wall (it crashed there, the way back) may see it draw its
    /// conversation, read busy — work there is no worker working past the
    /// wall, and told `served` the move ended under a login that answered
    /// nothing, so the 401 its continuation met had no move to wait on and
    /// `/login` was typed. At most that one point's work is passed over.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_account(
        &mut self,
        to: String,
        at: AccountAt,
        ago: Duration,
        asked: bool,
        told: bool,
        told_closes: bool,
        now: Instant,
    ) {
        if self.account.is_none() && ((told && told_closes) || ago < self.timing.window) {
            self.account = Some(AccountMove {
                to,
                at: now.checked_sub(ago).unwrap_or(now),
                wall: at == AccountAt::Wall,
            });
            self.account_asked = asked || told;
            self.account_told = told;
            self.account_startup = told || !asked;
            self.account_resume = at == AccountAt::Wall && !asked && !told;
            self.account_resume_seed = self.account_resume;
        }
    }

    /// An account move a previous loop asked for could not be made, and
    /// nothing has ended the wall it was asked at since (the approval
    /// ledger's `skipped` row of [`RULE_ACCOUNT`], no act and no wall's end
    /// after it): not asked again by this loop either, while that wall
    /// stands.
    pub fn seed_account_unmade(&mut self) {
        self.account_unmade = true;
        self.account_unmade_seed = true;
    }

    /// Acts a previous loop on the session typed, `ages` ago (the approval
    /// ledger's `typed` rows): counted by the budget like this loop's own
    /// while they are within the window, and marking the session's task as
    /// carried ([`Self::carrying`]). The short streak ([`Self::seed_short`])
    /// and a wall's attempts ([`Self::seed_usage_attempt`]) are seeds of
    /// their own.
    pub fn seed_acts(&mut self, ages: &[Duration], now: Instant) {
        let mut at: Vec<Instant> = ages
            .iter()
            .filter(|&&a| a < self.timing.window)
            .filter_map(|&a| now.checked_sub(a))
            .collect();
        at.extend(self.acts.drain(..));
        at.sort_unstable();
        self.acts = at.into();
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
        // An act typed within the window: the session's task was being carried.
        self.carrying |= !self.acts.is_empty();
    }

    /// THE CARRY THE SEEDED ACTS GAVE IS OVER
    /// (`crate::supervise::approvals::carry_ended_in`): the loop that typed
    /// them ended the task, left a person's exchange to them, or parked
    /// under Off since their last answer, and carries no task — so neither
    /// does this one ([`Self::seed_acts`] gave the carry back). A person's
    /// own short turn after it is theirs, as for that loop.
    pub fn seed_carry_over(&mut self) {
        self.carrying = false;
    }

    /// A usage-resume a previous loop on the session typed
    /// `ago` ago (the approval ledger's `typed` row of [`RULE_LIMIT_RESUME`],
    /// or of an act typed in its place at a limit's end:
    /// [`Self::acted_in_limits_place`]) at the limit its row names (`limit`,
    /// [`Self::limit_try`]; a usage window's first try for a row that names
    /// none, as 0.104.0 wrote them; on the ladder the wall that met it puts
    /// it), with the attempts it left, and NO LOOP HAS READ ITS ANSWER: this
    /// loop's first read after it is the try's first read
    /// ([`WallTrack::seeded`]; one a loop read met again is
    /// [`Self::seed_met_attempt`]'s). The wall it meets is worked as this
    /// loop's own retry would be — its next try the back-off from that act —
    /// so a loop restarted at a wall (each seamless update, each fault) does
    /// not type its continuation again the moment it attaches. ITS ANSWER IS
    /// DATED where the screen or the transcript dates it, at this loop's
    /// first decision read ([`Self::date_unread_try`]): the try met at once,
    /// taken up as the loop that typed it read it however old it is, as one
    /// a loop read met again is. Where nothing dates it, this loop's first
    /// read is the try's answer, as for the loop that typed it; and nothing
    /// is restored for a try older than the rung after it on the ladder it
    /// was on: the usage windows' last (30 min), or the entitlement ladder's
    /// next for a try at a wall no reset ends ([`LimitTry::entitlement`]: 2 h
    /// after the first, 6 h after every later one). Taken up, its first read
    /// would move the try's answer to this loop's attach, and the rung would
    /// count from there (up to 6 h past the attach); so that wall is read as
    /// a new appearance, its first rung counted from this loop's attach, as a
    /// loop attached to it fresh counts it — which is right where the try's
    /// own turn worked and met the limit anew in the gap, and nothing on the
    /// screen says which. A track the screen contradicts (another kind of
    /// wall — another limit too —, work) is replaced by [`Self::observe`] as
    /// any other.
    pub fn seed_usage_attempt(&mut self, ago: Duration, limit: LimitTry, now: Instant) {
        self.seed_try(ago, limit, false, now);
    }

    /// THE TRY A LOOP BEFORE THIS ONE MADE, MET AGAIN: [`Self::seed_usage_attempt`]'s
    /// try, whose limit a loop read after it, `ago` ago the track's appearance
    /// from then on ([`crate::supervise::approvals::UsageAttempt::met_ms`]:
    /// open still or closed since). Its limit has been read since its try
    /// ([`WallTrack::seeded`] clear), as at a named reset: work seen since is
    /// no longer that try's, and the loop that typed it keeps its rung
    /// through a person's turn ([`Self::track_wall`]). TAKEN UP HOWEVER OLD
    /// IT IS (ATERM-KI-1016-1, "a loop that follows another"): the rung that
    /// came due in the gap is owed, and this loop's first point at the wall
    /// types it, the ladder going on from that act — as the loop that
    /// watched typed it at its time. A spend limit's second try, then 6 h
    /// 1 min with no loop: the watcher types at the answer plus 6 h and 12 h,
    /// this loop at its attach and 6 h after it — never sooner, never more —
    /// and a limit that lifted unseen in the gap (credits added overnight) is
    /// served by that first try. Read as a new appearance (the rule before),
    /// the follower typed 30 min and 2 h 30 min after its attach, a try more
    /// than the watcher by 12 h, and a lifted limit sat idle its first rung.
    pub fn seed_met_attempt(&mut self, ago: Duration, limit: LimitTry, now: Instant) {
        self.seed_try(ago, limit, true, now);
    }

    /// [`Self::seed_usage_attempt`] (`met` false) and
    /// [`Self::seed_met_attempt`] (`met` true).
    fn seed_try(&mut self, ago: Duration, limit: LimitTry, met: bool, now: Instant) {
        if self.wall.is_some() {
            return;
        }
        let at = now.checked_sub(ago).unwrap_or(now);
        // A try nobody read: this loop's first read dates its answer, or not
        // ([`Self::date_unread_try`]).
        if !met {
            self.unread_try = Some(UnreadTry { at, limit });
        }
        // The rung after the try, on the ladder it was on: past it, a loop
        // that watched would have typed its next. A try nobody read is a new
        // appearance past it; one met again is owed that next.
        let next = if limit.entitlement {
            crate::supervise::ladder::Ladder(&self.timing.entitlement_backoff).at(limit.attempts)
        } else {
            self.timing.limit_backoff.last().copied()
        };
        if !met && next.is_none_or(|next| ago >= next) {
            // That wall is a new appearance, and the ledger is told so
            // ([`Self::take_try_over`]).
            self.try_over = next.is_some();
            return;
        }
        self.wall = Some(WallTrack {
            // The limit the act was typed at ([`LimitTry::bucket`]).
            class: if limit.bucket {
                WallClass::Model
            } else {
                WallClass::Usage
            },
            since: at,
            // The rung the act left the wall on ([`LimitTry::attempts`]): an
            // act typed is at least the first try.
            attempts: limit.attempts.max(1),
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            // A loop before this one acted at it.
            pointed: true,
            said: String::new(),
            kind: None,
            seeded: !met,
            entitlement: limit.entitlement,
            owed: false,
            login: LoginBooks::default(),
        });
    }

    /// THE RETRY LADDER OF THE API ERROR'S WALL THIS LOOP WAITS AT — an
    /// overload, a server's failure, a network never reached, a reply cut off
    /// ([`WallClass::Retry`]) — as the loop's next row marks it for a loop
    /// that follows ([`crate::supervise::approvals::RetryMark`]): its kind as
    /// last read, the retries typed this episode, those the API's measured
    /// return led to, and the appearance its ladder counts from. `None`
    /// where no such wall stands, or its kind was never read.
    #[must_use]
    pub fn retry_track(&self) -> Option<(WallKind, usize, usize, Instant)> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Retry)
            .and_then(|t| Some((t.kind?, t.attempts, t.ups, t.since)))
    }

    /// THE RETRY LADDER A LOOP BEFORE THIS ONE WAS ON at the API error's
    /// wall `kind` ([`Self::retry_track`], its last row's mark): `attempts`
    /// retries typed, `ups` of them on the API's measured return, the
    /// appearance `ago` ago. Worked as this loop's own track would be — its
    /// next retry on the rung the loop before was on, counted from that
    /// appearance. Taken up from nothing, a loop that follows retried on the
    /// ladder's first rung (a minute) from its own read, whatever rung the
    /// loop before was on: up to an hour sooner (ATERM-KI-1016-1, "a loop
    /// that follows another"). Where that loop's last row is the retry itself
    /// and its take window is over (`owed`: [`WallTrack::owed`]), the wall's
    /// first reading is the retry's next appearance and a point without it
    /// the retry taken, as for the loop that typed it; a retry still inside
    /// its window is the act awaited ([`Self::seed_awaited`]). A track the
    /// screen contradicts (another kind of wall, or work) is replaced or
    /// ended by [`Self::observe`] as any other.
    pub fn seed_retry(
        &mut self,
        kind: WallKind,
        attempts: usize,
        ups: usize,
        ago: Duration,
        owed: bool,
        now: Instant,
    ) {
        if self.wall.is_some() || class_of(kind) != WallClass::Retry {
            return;
        }
        self.wall = Some(WallTrack {
            class: WallClass::Retry,
            since: now.checked_sub(ago).unwrap_or(now),
            attempts,
            ups,
            exhausted: false,
            fill: None,
            named: None,
            // A loop before this one decided it.
            pointed: true,
            said: String::new(),
            kind: Some(kind),
            seeded: false,
            entitlement: false,
            owed,
            login: LoginBooks::default(),
        });
    }

    /// A usage limit or a model bucket a previous loop on the session waited
    /// on and no loop has ended (the approval ledger's open `wall-wait@v1`
    /// row, [`crate::supervise::approvals::open_wall`], written at `since` on
    /// this loop's clock): its notice (`message`, as written) named `kind`'s
    /// reset at `at` — its clock `said` ([`ResetClock`]) — this session's
    /// `jitter` on top, and said the vendor goes on by itself, or not
    /// (`by_itself`). Waited on as this loop's own would be, its notice on
    /// the screen or not ([`still_shown`]): a loop restarted while it is off
    /// the screen (Esc on the vendor's countdown leaves an idle screen with no
    /// wall) waits for that reset still, where with nothing seeded the
    /// continue policy typed into the standing limit. The bucket's words are
    /// kept with it, so the model it limits is read as at the wall
    /// ([`bucket_model`]). A track the screen contradicts is
    /// [`Self::observe`]'s as ever.
    ///
    /// THE OPEN WALL IS NEWER THAN THE USAGE ACT a previous loop made
    /// ([`Self::seed_usage_attempt`]): the ledger's wall is open only with no
    /// `typed` row after it ([`crate::supervise::approvals::open_wall`]), and
    /// the act is a `typed` row. It is the limit that stands. A seed of its
    /// class keeps its attempt and the act's time — the act met this wall
    /// again, as the loop that typed it worked it — and takes the wall's
    /// reset and words. A seed of another class is replaced, as a wall of
    /// another class opens its own track at the screen
    /// ([`Self::track_wall`]). Measured before (the verifier of the
    /// successor's seed, D1): a done check typed in a Sonnet bucket's place
    /// seeded a usage window's track, the bucket's reset was dropped beside
    /// it, and at Esc on the countdown the continue policy typed `keep going`
    /// into the bucket two minutes in, two days before its reset.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_named_reset(
        &mut self,
        kind: WallKind,
        message: &str,
        at: Instant,
        jitter: Duration,
        by_itself: bool,
        said: Option<ResetClock>,
        since: Instant,
    ) {
        let class = class_of(kind);
        if !named_class(class) {
            return;
        }
        let named = NamedReset {
            kind,
            at,
            jitter,
            by_itself: by_itself && !self.countdown_gone(since),
            said,
            first: since,
            written: None,
        };
        let (since, attempts, named) = match &self.wall {
            Some(t) if t.class == class => (t.since, t.attempts, t.named.unwrap_or(named)),
            _ => (since, 0, named),
        };
        self.wall = Some(WallTrack {
            class,
            since,
            attempts,
            ups: 0,
            exhausted: false,
            fill: None,
            named: Some(named),
            // A loop before this one waited on it.
            pointed: true,
            said: message.to_string(),
            kind: None,
            seeded: false,
            // A reset named: the short ladder after it.
            entitlement: false,
            owed: false,
            login: LoginBooks::default(),
        });
    }

    /// A LIMIT WHOSE NOTICE PLACED NO RESET a previous loop on the session
    /// waited on and no loop has ended, before any try of its (the ledger's
    /// open `wall-wait@v1` row, [`crate::supervise::approvals::open_wall`]:
    /// a spend limit, a bucket that names no reset, a usage window whose
    /// reset did not read), its notice `message` first read at `since` on
    /// this loop's clock: its track from that appearance, so the first try
    /// comes on the rung the loop before counted from it — on a spend limit
    /// 30 minutes after the wall, not after this loop's attach (ATERM-KI-1016-1,
    /// "a loop that follows another": a successor attached 10 minutes into
    /// that rung tried 10 minutes later than the loop before would have). A
    /// try a loop made wins — its track seeded ([`Self::seed_usage_attempt`]),
    /// or aged out, the wall a new appearance ([`Self::take_try_over`]):
    /// seeded from its old appearance, the aged-out try's wall was tried
    /// again at once, on the ladder's first rung.
    pub fn seed_unnamed_wall(&mut self, kind: WallKind, message: &str, since: Instant) {
        let class = class_of(kind);
        if self.wall.is_some() || self.try_over || !named_class(class) {
            return;
        }
        self.wall = Some(WallTrack {
            class,
            since,
            attempts: 0,
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            // A loop before this one waited on it.
            pointed: true,
            said: message.to_string(),
            kind: Some(kind),
            seeded: false,
            entitlement: entitlement_ladder(false, kind),
            owed: false,
            login: LoginBooks::default(),
        });
    }

    /// AN ACT A LOOP BEFORE THIS ONE TYPED IS STILL INSIDE ITS TAKE WINDOW
    /// (typed `ago` ago, under [`TurnEndTiming::take_within`]): the limit's
    /// try ([`Self::seed_usage_attempt`]) or the done check
    /// ([`Self::seed_done_check`]), `rule`, the wall's own retry or not
    /// (`wall_retry`: the try, or a check typed in a limit's place). The loop
    /// that typed it awaits it still, and so does this one. A point inside
    /// the window is no point yet, a wall read there answers it (`met`: one a
    /// loop before this one read, its ledger row), and a live turn takes it
    /// ([`Self::took`], [`Self::retry_met_untaken`]): the work its turn does
    /// is the act's own — a try's starts the episode again at its point, a
    /// check's answer is the check's ([`Self::observe`]). The window counts
    /// from the first point after the act ([`FirstRead`]): the one the loop
    /// that typed it read, where its ledger shows it — awaited while that
    /// window is open, however late that read came after the act — else this
    /// loop's own first point after attaching: for an act nothing has read,
    /// however long ago it was typed, as for the loop that typed it; where
    /// the ledger does not say, the act typed under the window ago. Seeded as a track
    /// alone, a successor attached inside a try's window read the screen the
    /// try was typed at as the try met again ([`WallTrack::seeded`]
    /// cleared), its busy read took nothing, and the try's own three-hour
    /// turn kept the try's rung: `usage-resume@v1` the moment that turn
    /// ended on the spend limit, 30 minutes before the loop that typed it
    /// (the verifier of 5b72b1613, M7-1). Counted from the act alone, or from
    /// this loop's attach, a successor attached after the first loop's read
    /// two seconds on (or 35 s on) judged the try met at the screen it was
    /// typed at before the first loop's window ended, and the try's own turn
    /// kept its rung the same way (the verifier of 341e1fa81, RS7-1).
    pub fn seed_awaited(
        &mut self,
        rule: &'static str,
        ago: Duration,
        read: FirstRead,
        wall_retry: bool,
        met: bool,
        now: Instant,
    ) -> bool {
        let take = self.timing.take_within;
        // The window the ledger shows began at that loop's first read, however
        // late after the act it came (the Mac asleep, a refusal's pause, an
        // outage ridden out): capped at twice the window, a successor attached
        // inside the window a read 70 s on opened judged the try met at the
        // screen it was typed at, and the try's own turn kept its two-hour
        // rung, 30 minutes sooner (the verifier of 97daeeace, RS8-F4). An act
        // nothing has read is this loop's first read, however late after the
        // act it comes, as the loop that typed it opens its window at its own
        // first read ([`Self::point_judged`]): every read of that loop's after
        // the act writes the wall's row ([`crate::supervise::approvals::AwaitedTry::unread`]),
        // so no loop has read the act since. Capped at twice the window, a
        // successor attached 66 s after a try whose first read came 70 s on
        // judged the try met at the screen it was typed at, and typed into
        // the standing limit 30 minutes sooner (ATERM-KI-1016-1, follow-2).
        let (open, read_ago) = match read {
            FirstRead::Read(r) => (r < take, Some(r)),
            FirstRead::Unread => (true, None),
            FirstRead::Unknown => (ago < take, None),
        };
        if !open || self.awaiting.is_some() {
            return false;
        }
        let at = now.checked_sub(ago).unwrap_or(now);
        self.awaiting = Some(Awaited {
            rule,
            command: false,
            at,
            typed: at,
            unseen_since: read_ago.map(|r| now.checked_sub(r).unwrap_or(now)),
            taken: false,
            met,
            left: None,
            host: None,
            off_stood: false,
            wall_retry,
            said: None,
            over: None,
        });
        true
    }

    /// THE DONE CHECK A LOOP BEFORE THIS ONE ASKED IS OWED ITS ANSWER
    /// ([`crate::supervise::approvals::done_check_owed`]): its answer's done
    /// report is the check's ([`Self::checking`]), or, a wall having answered
    /// it (`cut`), the wall's retry delivers it again ([`Self::cut_check`]).
    /// Its answer is read as the check's at this loop's first point that
    /// judges one, whoever's turn that point reads as ([`Self::observe`]): a
    /// done report of the worker's own then ends the task, as for the loop
    /// that asked it. Read as a first one, the check was typed again after
    /// the task had ended (the verifier of 5b72b1613, M7-5). A check whose
    /// answer a loop already judged the task's end by (`answered`): a turn
    /// read busy since is a later one ([`Self::seed_answered`]).
    /// `rule`: which check it is, the done check's or the open-items
    /// challenge's ([`Self::seeded_check`]).
    pub fn seed_done_check(&mut self, rule: &'static str, cut: bool, answered: bool) {
        self.seeded_check = Some(rule);
        if cut {
            self.cut_check = true;
        } else {
            self.checking = true;
            self.seed_answered |= answered;
        }
    }

    /// A TURN WAS READ RUNNING after the point a loop before this one
    /// recorded last ([`crate::supervise::approvals::StreakSeed::ran`]): a
    /// later turn than the one whose answer it judged, as for that loop's
    /// own busy read ([`Self::took`]). Its done report is a first one.
    pub fn seed_ran(&mut self) {
        if std::mem::take(&mut self.seed_answered) {
            self.checking = false;
            self.landing = false;
        }
        // That loop stopped before the turn's end: this loop's first point
        // is it ([`Self::take_gap`]).
        self.gap = true;
    }

    /// THE LIMIT'S TRY A LOOP BEFORE THIS ONE TYPED LAST WAS TAKEN: its own
    /// turn read running (the first read after it the wall's end, no wall
    /// read since, no point judged: [`crate::supervise::approvals::AwaitedTry`])
    /// and its take window over. That loop stopped before the turn's end,
    /// and this loop's first point is it ([`Self::take_gap`]): the try's
    /// answer, the work its turn did the try's own, as for the loop that
    /// typed it. Read as a point no read saw run, a successor counted the
    /// try's next rung from the try rather than from the wall its own turn
    /// met (272 s sooner), and kept that rung over its turn's 90 minutes of
    /// work where the loop that typed it started the ladder over (ATERM-KI-1016-1).
    pub fn seed_try_taken(&mut self) {
        self.gap = true;
    }

    /// THE GAP'S POINT, asked once by the loop at its first point
    /// ([`Self::seed_ran`], [`Self::seed_try_taken`]): the end of a turn the
    /// loop before this one read running and stopped before seeing end, with
    /// no act of this loop's awaited and no turn read running since. The
    /// loop reads it as the end of a turn that ran, its work Claude Code's
    /// own done row ([`done_row_work`]) — the measure the loop that read it
    /// running takes too — and none where no done row says (each rule that
    /// asks how long a turn ran then takes its later branch: a short turn,
    /// the same episode, a person's exchange left to them). Read as a point
    /// no read saw run, a FINISHED that names open work ended the task
    /// unchallenged, a person's turn that left a limit's notice behind left
    /// the limit's track standing, and a limit's try met by its own turn
    /// kept the rung of the try's appearance (ATERM-KI-1016-1).
    pub(crate) fn take_gap(&mut self) -> bool {
        std::mem::take(&mut self.gap) && self.awaiting.is_none()
    }

    /// …AND WHEN (ATERM-KI-1016-1): that turn was read running `ran_ago`
    /// ago, after the point recorded `point_ago` ago. This loop's first
    /// point, no busy read of its own before it and no act awaited, is that
    /// turn's end, its work the time since it was read running: where a
    /// person's hand opened it — their keystroke after that point and the
    /// last act, within the turn ([`TurnEndReading::person_in_turn`]) — it is
    /// judged as the loop that read it running judges it ([`Self::observe`]):
    /// a person's own exchange is left to them. Judged as a first point no
    /// read saw run, which is never left to a person, a loop that stopped
    /// inside a person's exchange had its successor type `keep going` into
    /// it after the back-off, where the loop before left it to them (the
    /// seed matrix's gap cells with every read stamped with the person).
    pub fn seed_ran_turn(&mut self, ran_ago: Duration, point_ago: Duration, now: Instant) {
        self.ran_turn_seed = Some((
            now.checked_sub(ran_ago).unwrap_or(now),
            now.checked_sub(point_ago).unwrap_or(now),
        ));
    }

    /// The reading of this loop's first point as the end of the turn a loop
    /// before it read running ([`Self::seed_ran_turn`]), where a person's
    /// hand opened that turn: `reading` with that turn's work. `None` where
    /// no such turn is seeded, this loop read a turn or awaits an act of its
    /// own, or no person's hand is in it.
    fn ran_turn_reading(
        &mut self,
        reading: &TurnEndReading,
        now: Instant,
    ) -> Option<TurnEndReading> {
        let (ran_at, point_at) = self.ran_turn_seed.take()?;
        if reading.worked.is_some() || self.awaiting.is_some() || self.point_at.is_some() {
            return None;
        }
        let worked = now.saturating_duration_since(ran_at);
        let at = reading.person.and_then(|ago| now.checked_sub(ago))?;
        let since = self
            .acts
            .back()
            .copied()
            .map_or(point_at, |a| a.max(point_at));
        let turn = TurnEndReading {
            worked: Some(worked),
            ..reading.clone()
        };
        (at > since && turn.person_in_turn()).then_some(turn)
    }

    /// THE LAND EPISODE A LOOP BEFORE THIS ONE HAD IN HAND
    /// ([`crate::supervise::approvals::land_episode`]): its instruction typed
    /// and `nudges` nudges since, so Land's next act is a nudge (or, the
    /// landing spent, nothing), as for that loop; the last act one of
    /// Land's (`landing`): its done reply lands the task, read wherever this
    /// loop judges it ([`Self::observe`]) — unless that loop already judged
    /// its answer (`answered`, [`Self::seed_answered`]) — or, a wall having
    /// answered it (`cut`), the wall's retry delivers it again
    /// ([`Self::cut_land`]). A reading in another mode ends it, as any
    /// episode. `quiet`: the episode ended quiet, its nudges spent and its
    /// last land act answered with no done ([`LandEpisode::quiet`]), so the
    /// task's face is spent from this loop's start, as for that loop.
    pub fn seed_land(
        &mut self,
        nudges: u32,
        landing: bool,
        cut: bool,
        answered: bool,
        quiet: bool,
    ) {
        self.land = LandEpisode {
            typed: true,
            nudges,
            quiet,
        };
        self.landing = landing;
        self.cut_land = cut;
        self.seed_answered |= landing && answered;
    }

    /// THE SHORT STREAK THE LAST ACT A LOOP BEFORE THIS ONE TYPED WAS TYPED
    /// AFTER (`short` short turns in a row: its row's mark,
    /// [`crate::supervise::approvals::short_streak`], an act within the
    /// window): this loop's first point lengthens it, as that act's short
    /// yield lengthens the streak of the loop that typed it, and real work
    /// seen ends it ([`Self::observe`]). Read from the screen alone, every
    /// restart backed a worker that yields at once off the first rung (2
    /// min) where the loop that typed the act backed it off 4, 8 or 16 —
    /// and the done report a wall's retry was answered with got the done
    /// check at once where that loop backed it off first (the verifier of
    /// 341e1fa81, RS7-5).
    pub fn seed_short(&mut self, short: u32) {
        self.short = short;
        // The books hold a word on the streak: a point's record is the last
        // only where [`Self::seed_point`] says so.
        self.point_last = false;
    }

    /// THE STREAK SEEDED WAS RECORDED AT A POINT a loop before this one judged
    /// `ago` ago ([`crate::supervise::approvals::RULE_POINT`]): the streak
    /// after that point, not before an act's yield. This loop's first point,
    /// no busy read before it, is that point read again — as for the loop
    /// that judged it, a turn no read saw run is no new turn
    /// ([`Self::observe`]) — and keeps the streak and the back-off counted
    /// from that point, as that loop does. Counted from this loop's attach
    /// and lengthened once more, a successor in a back-off waited up to the
    /// ladder's rung longer, and one after a point no act followed (someone
    /// else's short turn) backed off half as long, sooner (the verifier of
    /// 97daeeace, RS8-R3, RS8-F3).
    pub fn seed_point(&mut self, ago: Duration, now: Instant) {
        self.point_seed = Some(now.checked_sub(ago).unwrap_or(now));
        self.point_last = true;
    }

    /// A PERSON'S ESC STOOD AT THE POINT SEEDED ([`Self::seed_point`]), as
    /// its record says (`approvals::StreakSeed::esc`): this loop's first
    /// read of that point again — no busy read, no box read since — times
    /// the Esc from the point, as the loop that judged it does
    /// ([`Self::esc_at`]), so its hold ends where that loop's would. Timed
    /// from this loop's attach, a loop attached inside the hold typed up to
    /// a whole grace later than the first loop (RS9-F2 over main's
    /// 28b288273). A turn or a box read since makes the next Esc a new one.
    pub fn seed_esc(&mut self) {
        self.esc_seed = true;
    }

    /// Whether a person's Esc stands at the point now ([`Self::esc_at`]):
    /// what the point's record marks for a loop that follows
    /// (`approvals::point_reason`).
    #[must_use]
    pub fn persons_esc_stands(&self) -> bool {
        self.esc_at.is_some()
    }

    /// THE STREAK SEEDED IS THE ONE THE ACT `rule` A LOOP BEFORE THIS ONE TYPED
    /// LAST WAS TYPED AFTER, no point judged since it (every point that
    /// judges an act's answer is recorded,
    /// [`crate::supervise::approvals::RULE_POINT`]): a turn this loop sees run
    /// and end first is that act's yield, and moves the streak as that act's
    /// own answer does for the loop that typed it ([`Self::observe`]) — a
    /// continuation's done reply is a short turn, a wall's try moves none.
    /// Judged as someone else's turn, a land act's ten-minute done reply
    /// ended the streak a loop before had lengthened, and a spend limit's try
    /// whose work kept it was read as ending it: the next act came 2 to 8
    /// minutes sooner (the verifier of 97daeeace, RS8-F2).
    pub fn seed_yield(&mut self, rule: &str) {
        self.yield_seed = Some(is_continue_rule(rule));
    }

    /// THE POINT JUDGED LAST MOVED WHAT A LOOP THAT FOLLOWS MUST HOLD and no
    /// act's row says, asked once by the loop after the fold: the short
    /// streak, an act's answer (the act awaited, or the done check's or a
    /// land act's a loop before this one asked) judged, or the point's time
    /// (this loop's first point among them, from which its ledger holds
    /// every act's answer: [`crate::supervise::approvals::StreakSeed::recorded`]).
    /// The loop writes the point's record
    /// ([`crate::supervise::approvals::RULE_POINT`]): a loop that follows
    /// awaits that act no more, reads the check's verdict, and takes the
    /// streak up from that point ([`Self::seed_point`]), or, none written
    /// since the last act, that act's yield as still to come
    /// ([`Self::seed_yield`]). The first turn read running after it is
    /// recorded too ([`Self::take_ran`]).
    pub(crate) fn take_point_moved(&mut self) -> bool {
        let moved = std::mem::take(&mut self.point_moved);
        self.point_last |= moved;
        moved
    }

    /// A LOST LOGIN ENDED BACK at the reading folded last
    /// ([`Self::observe`]), asked once by the loop after it folds a point: the
    /// host hands the news to every tab that waited on this login's sign-in
    /// (`crate::supervise::IdleHost::login_back`).
    pub(crate) fn take_login_returned(&mut self) -> bool {
        std::mem::take(&mut self.login_returned)
    }

    /// A TURN WAS READ RUNNING, the first since the point recorded last
    /// ([`crate::supervise::approvals::RULE_POINT`]), asked once by the loop
    /// after the busy read: the loop writes so
    /// ([`crate::supervise::approvals::RULE_TURN_RAN`]). A loop that follows
    /// reads its first idle point as that turn's end, not the point recorded
    /// again ([`Self::seed_point`]): read again, a turn the loop before saw
    /// start and never saw end (it stopped meanwhile) left the streak and its
    /// back-off where that point left them, and the successor typed up to a
    /// rung sooner than the loop that saw it run, into a standing limit too
    /// (the verifier of 75b03ac6d, RS9-F1).
    pub(crate) fn take_ran(&mut self) -> bool {
        std::mem::take(&mut self.ran)
    }

    /// THE POINT JUDGED LAST ENDED A PERSON'S OWN EXCHANGE, left to them
    /// ([`Self::left_to_person`]): a turn read here ended (an act's answer
    /// or someone else's turn), not the same point read again. The loop
    /// writes its row whenever the exchange is left, the task's end said
    /// before it or not: written only where the task's end was first said,
    /// a person's exchange after the worker's FINISHED (or the check's done
    /// answer) left nothing a loop that follows could read, and that loop
    /// typed `keep going` into the finished task at its first point, where
    /// the loop that judged the exchange types nothing (the verifier of
    /// 75b03ac6d, RS9-F3).
    #[must_use]
    pub(crate) fn left_judged(&self) -> bool {
        self.left_judged
    }

    /// THE TRY NOBODY READ THAT A SEED DECLINED AS AGED OUT IS OVER
    /// ([`Self::try_over`]; a try met again never ages out,
    /// [`Self::seed_met_attempt`], nor one whose answer this loop's first
    /// read dated, [`Self::date_unread_try`]),
    /// asked once, by the loop about to write its first wall: it writes the
    /// try over ([`crate::supervise::approvals::RULE_TRY_OVER`]: the wall
    /// stands) before that wall, as the loop that typed the try ended its track, so
    /// a loop that follows reads no try met again at this loop's wall
    /// ([`crate::supervise::approvals::usage_attempt`]). Unwritten, a third
    /// loop read the first loop's try met again at the second's new
    /// appearance and held that try's two-hour rung from it, 90 minutes
    /// after the first loop typed (the verifier of 341e1fa81, RS7-6). An act
    /// typed first is the ledger's last try itself.
    pub(crate) fn take_try_over(&mut self) -> bool {
        std::mem::take(&mut self.try_over)
    }

    /// WHEN THE LIMIT'S TRY NO LOOP HAS READ WAS TYPED (on the policy's clock,
    /// [`UnreadTry::at`]), while this loop has not read it either — the try is
    /// the act it awaits, no point read since it, no wall answering it, no
    /// turn taking it: its answer is [`Self::date_unread_try`]'s to date, at
    /// this loop's first decision read. `None` otherwise.
    #[must_use]
    pub fn unread_try(&self) -> Option<Instant> {
        self.unread_try
            .map(|u| u.at)
            .filter(|&at| self.awaits_unread(at))
    }

    /// The act this loop awaits is the try typed at `at`, and nothing has
    /// read it ([`Self::unread_try`]).
    fn awaits_unread(&self, at: Instant) -> bool {
        self.awaiting.is_some_and(|a| {
            a.wall_retry && !a.taken && !a.met && a.unseen_since.is_none() && a.typed == at
        })
    }

    /// THE ANSWER OF A TRY NO LOOP READ, DATED at this loop's first decision
    /// read (ATERM-KI-1016-1, "a loop that follows another"; asked once, by
    /// the loop: `Session::date_unread_try`). `answered`: how long after the
    /// try the limit's answer came AT THE LATEST, where the screen's done row
    /// (`Worked for 0s`, the try's minute: by that minute's end) or the
    /// transcript's notice row ([`Self::notice_written_at`]: by its second
    /// and [`TRY_ANSWER_SLACK`]) says the limit answered the try at once, the
    /// read past the take window a first read then would have opened, and
    /// nothing asked since — Claude Code answers a line typed at a standing
    /// limit at once (measured: the 2.1.294 stub's 429 22 ms after the
    /// prompt's row; 2.1.302's `Worked for 0s · done` the prompt's minute).
    /// Where the read shows that try's limit (`wall`), the try is taken up
    /// MET as the loop that typed it read it: that loop's first read after
    /// the try came no later than the answer, so it is dated there, and the
    /// appearance that read's take window's end, however old it is
    /// ([`Self::seed_met_attempt`]); never earlier, so this loop's next try
    /// is never sooner than that loop's (a done row's minute leaves it later
    /// by under a minute). The act is awaited no more, and a rung that came
    /// due in the gap is owed at this point. A spend limit's second try, a
    /// loop 6 h 1 min after it: this loop types the third try at once and
    /// the fourth 6 h on, as the loop that typed it would have, where a new
    /// appearance typed 30 min and 2 h 30 min on, a try more than that loop
    /// by 12 h; an hour after it, the third try comes 6 h after the try, not
    /// 6 h after this loop's read. Returns that first read, which the loop
    /// writes for a loop that follows
    /// ([`crate::supervise::approvals::RULE_TRY_ANSWERED`]). `None` —
    /// nothing dates the answer, the read shows no wall of that limit, or the
    /// try is not unread ([`Self::unread_try`]) — and the try is read by
    /// [`Self::seed_usage_attempt`]'s rule.
    pub fn date_unread_try(
        &mut self,
        wall: Option<WallKind>,
        answered: Option<Duration>,
    ) -> Option<Instant> {
        let u = self.unread_try.take()?;
        let answered = answered?;
        if !self.awaits_unread(u.at) {
            return None;
        }
        let class = if u.limit.bucket {
            WallClass::Model
        } else {
            WallClass::Usage
        };
        if wall.map(class_of) != Some(class) {
            return None;
        }
        self.awaiting = None;
        self.try_over = false;
        let read = later(u.at, answered);
        self.wall = Some(WallTrack {
            class,
            since: later(read, self.timing.take_within),
            attempts: u.limit.attempts.max(1),
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            // The loop that typed it acted at it.
            pointed: true,
            said: String::new(),
            kind: None,
            // Its answer read: met.
            seeded: false,
            entitlement: u.limit.entitlement,
            owed: false,
            login: LoginBooks::default(),
        });
        Some(read)
    }

    /// Whether a usage limit or a model bucket whose notice named its reset
    /// is waited on ([`named_class`]): its track stands — the worker has not
    /// worked since, the vendor has not gone on, no relaunch or person's
    /// `/model` moved the session off the bucket's model — whether its notice
    /// shows or has left the screen ([`still_shown`]). While it is, the loop
    /// keeps the wall open for a loop that follows
    /// ([`Self::seed_named_reset`]).
    #[must_use]
    pub fn waits_on_named_reset(&self) -> bool {
        self.waited_limit().is_some()
    }

    /// WHETHER THE SCREEN SHOWS ANOTHER CONVERSATION than the one the waited
    /// limit cut off ([`limit_end`]; ATERM-KI-1015-1 LP-1): the screen's
    /// newest limit notice ([`TurnEndReading::since_limit`]) is another
    /// limit's — of another class, or naming another clock ([`notice_of`]):
    /// a `/resume`d conversation's older notice, since a conversation that
    /// met this limit after it draws this one newer — or the conversation
    /// the tab holds has no row of this limit's notice
    /// ([`TurnEndReading::notice_elsewhere`]: the same clock a day apart).
    /// No notice drawn (the reply pushed it off), or a limit whose notice
    /// named no clock: not known to be elsewhere, so `false` — the limit's
    /// own resume, as before.
    fn limit_elsewhere(&self, reading: &TurnEndReading) -> bool {
        let Some(n) = self.waited_limit() else {
            return false;
        };
        reading.notice_elsewhere
            || (n.said.is_some()
                && reading.since_limit.is_some()
                && !notice_of(reading.since_limit.as_ref(), n.kind, n.said))
    }

    /// The limit [`Self::waits_on_named_reset`] waits on: its kind, its reset
    /// and the clock its notice named ([`WaitedLimit`]).
    #[must_use]
    pub fn waited_limit(&self) -> Option<WaitedLimit> {
        let n = self.wall.as_ref().filter(|t| named_class(t.class))?.named?;
        Some(WaitedLimit {
            kind: n.kind,
            at: n.at,
            said: n.said,
        })
    }

    /// A wall supplied by the loop's memory while the screen shows only
    /// background work. The notice preserves its limit, but the current
    /// screen decides whether its countdown still runs or its model was
    /// left. An end tied to this limit is the wall-less point it would be
    /// without the shell, so `observe` and the ledger fold it normally. A
    /// wall kept is marked the memory's ([`TurnEndReading::wall_remembered`]):
    /// where the screen draws no notice, the words under it are the worker's.
    /// A screen of ANOTHER conversation ([`Self::limit_elsewhere`]: a
    /// `/resume`d history whose older limit notice is drawn) is the wall-less
    /// point too: the limit stands in this state's track, waited on to its
    /// reset as on the same screen with no shell, and its words then get the
    /// continue policy's act ([`limit_end`]) — where the memory's wall hid them
    /// and `keep going` went into its done report (ATERM-KI-1016-1).
    #[must_use]
    pub(crate) fn read_remembered_wall(&self, mut reading: TurnEndReading) -> TurnEndReading {
        let plain = TurnEndReading {
            phase: Phase::Idle,
            wall: None,
            wall_message: String::new(),
            reset_at: None,
            reset_clock: None,
            resumes_by_itself: false,
            ..reading.clone()
        };
        let moved = self.wall.as_ref().is_some_and(|t| {
            t.class == WallClass::Model
                && t.named
                    .is_some_and(|n| n.moved_off_under(reading.since_limit.as_ref(), &t.said))
        });
        if reading.taskless
            || moved
            || self.limit_went_on(&plain, None)
            || self.limit_elsewhere(&plain)
        {
            return plain;
        }
        reading.resumes_by_itself &=
            reading.auto_continue == Some(aterm_phase::AutoContinue::Armed);
        reading.wall_remembered = true;
        reading
    }

    /// A usage-resume a previous loop on the session typed at the unix
    /// second `typed_unix` (the approval ledger's last `typed` row of
    /// [`RULE_LIMIT_RESUME`], or of an act typed in its place:
    /// [`Self::acted_in_limits_place`]): every clearing of the owner's made at or
    /// before it has had its continuation ([`AccountAdvice::Resume`]: one a
    /// clearing, whichever loop typed it). A clearing made after it is a new
    /// one. Which clearing was answered is this state's memory alone, and a
    /// loop restarted at the same notice typed that continuation again.
    pub fn seed_account_resumed(&mut self, typed_unix: i64) {
        self.account_resumed = Some(
            self.account_resumed
                .map_or(typed_unix, |typed| typed.max(typed_unix)),
        );
    }

    /// The typed acts within the window ending at `now`.
    #[must_use]
    pub fn acts_in_window(&self, now: Instant) -> usize {
        self.acts
            .iter()
            .filter(|&&t| now.saturating_duration_since(t) < self.timing.window)
            .count()
    }

    /// How many acts of wall rule `rule` this policy typed within the window
    /// ending at `now`, and when the oldest of them leaves it
    /// ([`WALL_ACTS_PER_WINDOW`]).
    #[must_use]
    pub fn wall_acts_of(&self, rule: &str, now: Instant) -> (usize, Option<Instant>) {
        let window = self.timing.window;
        let mut n = 0;
        let mut oldest = None;
        for &(t, r) in &self.wall_acts {
            if r == rule && now.saturating_duration_since(t) < window {
                n += 1;
                oldest = oldest.or(Some(t + window));
            }
        }
        (n, oldest)
    }

    /// Short turns in a row (the conformance walk's projection, and what an
    /// act's row carries: [`crate::supervise::approvals::act_reason_after`]).
    #[must_use]
    pub fn short_streak(&self) -> u32 {
        self.short
    }

    /// Done reports in a row, the check's yield included (`0`, `1`, `2`: the
    /// conformance walk's projection).
    #[must_use]
    pub fn done_reports(&self) -> u8 {
        self.done
    }

    /// Whether the conversation's record is read at `r`
    /// ([`TurnEndReading::finished_record`]): where a turn ran since the last
    /// point (anyone's, Claude Code's own wake included — a token said while
    /// the screen read busy is read here too), where the vendor's own
    /// usage-limit continuation fired (folded in as a turn no read saw,
    /// [`Self::observe`]), wherever a usage limit or a model bucket this
    /// policy waits on ([`named_class`]) stands with no wall on the screen —
    /// a turn SERVED since its notice is folded in there too, and a finished
    /// task's own wake after the limit lifted is such a turn (read as
    /// anyone's, it reopened the task and `keep going` was typed) — and at
    /// this state's first point (a new loop's): the only points the verdict
    /// can move.
    #[must_use]
    pub fn reads_record(&self, r: &TurnEndReading) -> bool {
        r.worked.is_some()
            || r.auto_continue == Some(aterm_phase::AutoContinue::Fired)
            || self.point_at.is_none()
            || (r.wall.is_none() && self.wall.as_ref().is_some_and(|t| named_class(t.class)))
            // …and while an act of this policy's stands untaken: its answer,
            // drawn before any read saw its turn run, ends that turn at the
            // read the record shows it at ([`Self::answered_unseen`]).
            || self.untaken().is_some()
    }

    /// The task is done: the worker said so, got the [`DONE_CHECK`], and said
    /// so again. Nothing is typed ([`decide_turn_end`]) until someone else's
    /// turn ends at a point this state sees.
    #[must_use]
    pub fn task_done(&self) -> bool {
        self.done >= 2
    }

    /// A LOST LOGIN'S `/login` WAS LEFT BEFORE IT SIGNED IN (`⎿  Login
    /// interrupted` read while its track stands, [`WallTrack::cancelled`]):
    /// for the rest of that loss nothing types `/login` again, and the owner
    /// is told the sign-in was cancelled.
    #[must_use]
    pub fn login_cancelled(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Auth && t.login.cancelled)
    }

    /// WHEN THIS LOOP TYPED `/login` INTO THE LOST LOGIN STANDING
    /// ([`LoginBooks::typed_at`]), its sign-in not left: the `/login` the
    /// dialog's steps belong to — the method picker the approval loop
    /// answers, the success Enter — which a person's key no later than it
    /// does not hold. `None` with no such `/login`, a person's own included.
    #[must_use]
    pub fn login_typed_at(&self) -> Option<Instant> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Auth && t.attempts > 0 && !t.login.cancelled)
            .and_then(|t| t.login.typed_at)
    }

    /// UNTIL WHEN THE `/login` THIS LOOP TYPED MAY STILL BE OPENING ITS
    /// DIALOG ([`TurnEndTiming::take_within`] from the keys): a read of the
    /// lost login with no dialog inside it is no sign-in left, and the point
    /// waits it out ([`login_opening_wait`]), decided again at its end. `None`
    /// past it, and with no such `/login`.
    #[must_use]
    pub fn login_opening(&self, now: Instant) -> Option<Instant> {
        self.login_typed_at()
            .map(|at| later(at, self.timing.take_within))
            .filter(|until| now < *until)
    }

    /// `/login`'s success state was answered with its Enter and has not left
    /// since ([`Self::login_confirmed`]'s field).
    #[must_use]
    pub fn login_confirmed(&self) -> bool {
        self.login_confirmed
    }

    /// The task is done because the worker ended its report with
    /// [`FINISHED`] (rather than by answering the [`DONE_CHECK`]).
    #[must_use]
    pub fn finished_by_worker(&self) -> bool {
        self.task_done() && self.finished
    }

    /// The challenge the task is done after, where the worker said so in
    /// words (not its own [`FINISHED`]) in reply to one
    /// ([`is_challenge_rule`]: [`RULE_OPEN_ITEMS`], [`RULE_UNCOMMITTED`]), as
    /// a done reply to the [`DONE_CHECK`] ends one: the journal's word for
    /// that end.
    #[must_use]
    pub fn done_after_challenge(&self) -> Option<&'static str> {
        self.followed
            .filter(|&rule| self.task_done() && !self.finished && is_challenge_rule(rule))
    }

    /// The open work the current point's done report names, whose challenge
    /// is owed ([`RULE_OPEN_ITEMS`]; empty: none owed): a projection for a
    /// walk or a test.
    #[must_use]
    pub fn open_items(&self) -> &[&'static str] {
        &self.open
    }

    /// What the session's repository held that no remote has at the current
    /// point, whose challenge is owed ([`RULE_UNCOMMITTED`]; `None`: none
    /// owed): a projection for a walk or a test.
    #[must_use]
    pub fn repo_owed(&self) -> Option<&crate::harness::repo_work::RepoWork> {
        self.repo.as_ref()
    }

    /// The task is done because the worker said so in reply to a land act
    /// ([`LAND`], a [`LAND_NUDGE`], or a [`LAND_ANSWER`] after the
    /// instruction) — its own [`FINISHED`] among the ways it can.
    #[must_use]
    pub fn landed(&self) -> bool {
        self.task_done() && self.landed
    }

    /// The Land episode in hand has had its instruction ([`LAND`]) typed, and
    /// how many nudges ([`LAND_NUDGE`]) since: a projection for a walk or a
    /// test. `(false, 0)` outside Land.
    #[must_use]
    pub fn land_episode(&self) -> (bool, u32) {
        (self.land.typed, self.land.nudges)
    }

    /// The Land episode in hand is spent: its instruction typed and its
    /// [`LAND_NUDGES`] nudges after it. No instruction or nudge is typed for
    /// it again, only a question's [`LAND_ANSWER`] ([`land_policy`]), and its
    /// point is wrapped in no relaunch, whatever the point gets
    /// ([`act_at_point`]).
    pub(crate) fn land_spent(&self) -> bool {
        self.land.typed && self.land.nudges >= LAND_NUDGES
    }

    /// THE LANDING ENDED QUIET ([`LandEpisode::quiet`]): its nudges spent
    /// ([`Self::land_spent`]) and its last land act answered with no done.
    /// The task's face reads it ([`TaskFace::of`]): the second nudge, once
    /// typed, spends the episode, but its reply may still say done and land
    /// it — the face is spent only once that turn ended with no done, never
    /// while it runs or a wall's retry owes it again. A question's land answer
    /// in a quiet episode leaves it quiet while its turn runs (no move), and
    /// its done reply lands the task; a landing at the last nudge leaves the
    /// nudges spent but the episode NOT quiet — the agent did reply done.
    fn landing_spent(&self) -> bool {
        self.land.quiet
    }

    /// AUTOPILOT OFF STOOD HERE, whatever the next reading says: the loop
    /// parked under Off (`lifecycle.rs`'s `park_while_off`), where the
    /// readings that would end anything ([`Self::observe`], [`Self::acted`])
    /// are never folded in — the point the word turned off at was read under
    /// the word before it — or the window's host started this state's loop as
    /// the session's Off lifted ([`Self::start_lifted_off`]). The person
    /// drove the session meanwhile, so:
    ///
    /// * THE LAND EPISODE IS OVER: a Land chosen after is a new episode, its
    ///   instruction typed again ([`LAND`]);
    /// * THE TASK IS CARRIED NO MORE ([`Self::carrying`]): a person's turn a
    ///   busy read sees after it, answered within [`EXCHANGE_WORK`], is their
    ///   own exchange until an act of this policy's is answered — acts seeded
    ///   before it still count toward the budget.
    ///
    /// The point after it is judged as before: a turn no read saw run under
    /// Off is never left to anyone ([`Self::observe`]), so it is backed off
    /// and acted on, never a stall. A task already done stays done. A
    /// person's exchange a wall's act was delivering again stays theirs on
    /// the same terms as without the park — the answer's work in all, before
    /// the park and after it, within [`EXCHANGE_WORK`] — where a busy read
    /// after the park saw that answer run; the act's first point after the
    /// park with no busy read ([`Awaited::off_stood`]) ends the exchange
    /// instead, backed off and acted on as any first point is: the person
    /// drove meanwhile, and nothing there says the retry's answer is what
    /// ended. A wall's cut act is delivered again as itself no more
    /// ([`Self::cut_check`]): its done reply after the park is a first one.
    pub(crate) fn park_off(&mut self) {
        self.land = LandEpisode::default();
        self.landing = false;
        self.cut_check = false;
        self.cut_land = false;
        self.compact_owes = None;
        self.carrying = false;
        self.carry_at_attach = false;
        // A landing seeded from a loop before this one still holds at the
        // first point (a done task stays done), its episode over.
        if let Some(episode) = &mut self.landed_seed {
            *episode = false;
        }
        if let Some(a) = &mut self.awaiting {
            a.off_stood = true;
        }
    }

    /// THE WINDOW'S HOST STARTED THIS STATE'S LOOP AS THE SESSION'S OFF
    /// LIFTED (`IdleHost::lifted_off`: the host stops the worker under Off,
    /// and the new loop's acts are seeded from the ledger, [`Self::seed_acts`]):
    /// Off stood ([`Self::park_off`]). But a turn ALREADY RUNNING as the loop
    /// attaches — a live read before its first point ([`Self::took`]) — may
    /// be the one the harness's last act asked (the chip's way from Land back
    /// to Cruise is through Off, the agent mid-turn): it keeps the carry those
    /// acts gave, so the scroll of a person reading its tail does not make
    /// the harness's own continuation an exchange of theirs, left for good.
    /// A first point no live read came before drops it, as the park does
    /// (that point itself is backed off and acted on, [`Self::observe`]).
    pub(crate) fn start_lifted_off(&mut self) {
        let seeded = self.carrying;
        self.park_off();
        self.carry_at_attach = seeded;
    }

    /// THE LANDING A LOOP BEFORE THIS ONE LANDED (the Autopilot design's
    /// §9.4.5): the session's last turn-end note is `DONE … rule=land@v1`
    /// and its ledger holds no `typed` row after it, and no record of a later
    /// point judged or of a turn read running since
    /// ([`crate::supervise::approvals::RULE_POINT`],
    /// [`crate::supervise::approvals::RULE_TURN_RAN`]; the loop reads both at
    /// its start). A new loop restores only its acts ([`Self::seed_acts`]),
    /// so a landed session got the land instruction typed into finished work
    /// after every restart and every seamless update.
    ///
    /// The seed holds only at a first point no live read came before, whose
    /// reply still says done ([`says_done`], or the worker's [`FINISHED`],
    /// with no wall or unanswered input over it): the task is then landed —
    /// done twice, [`Self::landed`], the episode's instruction typed (under
    /// Land, and unless an Off stood since, [`Self::park_off`]) — and nothing
    /// is typed. A live read before that point ([`Self::took`]), or a reply
    /// that no longer says done, drops it, and the point is decided as any
    /// first point is ([`Self::observe`]). A loop started as Off lifted seeds
    /// first, then [`Self::start_lifted_off`] runs: a done task stays done.
    ///
    /// `words` is THE MARK OF THE LANDING'S LAST WORDS ([`said_mark`]) the
    /// journal kept at its point: the first point's last words must mark the
    /// same, or the seed is dropped. A turn no loop watched (typed while Off
    /// had stopped the loop, say) ended on words of its own, though they end
    /// on [`FINISHED`] too: the task is THEIRS done ([`FINISHED`] reads
    /// `done`), never landed, and no land instruction preceded that reply.
    /// `None` (a journal a build before the mark wrote): no words compared.
    pub fn seed_landed(&mut self, words: Option<u64>) {
        self.landed_seed = Some(true);
        self.landed_words = words;
    }

    /// A landing seeded from a loop before this one is armed, not yet taken
    /// up or dropped ([`Self::seed_landed`]): a projection for a test.
    #[must_use]
    pub fn landed_seed_armed(&self) -> bool {
        self.landed_seed.is_some()
    }

    /// THE DONE A LOOP BEFORE THIS ONE READ (the Autopilot design's §9.4.2,
    /// §10.2.1): the session's last turn-end note is a `DONE` other than the
    /// land's (`rule=finished@v1`, the done check's, the open-items
    /// challenge's), under the landing's rules ([`Self::seed_landed`], read by
    /// the loop at its start): no `typed` row after it, no later point judged
    /// nor turn read running, every point the journal holds after it the
    /// done's own by its words. A restarted loop (an agent's switch toggled, a
    /// SUPERVISION setting saved, an Off lifted) told `open` at its start
    /// and the done again at its first point, so the window, which keeps a
    /// face's first time only where the face told after "unknown" is the one
    /// it held, stamped the done with the restart's time: `done 13:27` for
    /// work finished at 13:04 (owner-eye-4 / behaviour-5 of the post-landing
    /// check of Autopilot v3 wave 1).
    ///
    /// Seeded, the start face is [`TaskFace::Done`] — the done restated —
    /// and the window gives back its first time. The seed is the FACE's
    /// alone: nothing the policy decides reads it, and its first point is
    /// decided as any first point is, which reads the same done again (the
    /// token, the record, a done check answered) and tells nothing, or moves
    /// the face and tells the move. A live read before that point
    /// ([`Self::took`]) drops it (someone else's turn: open). `words` is THE
    /// MARK OF THE DONE'S LAST WORDS ([`said_mark`]): where the first point's
    /// last words mark another reply (a turn no loop watched, typed while an
    /// Off had stopped the loop), the restated done is WITHDRAWN, and a done
    /// read there is a new one, told as one ([`Self::face_withdrawn`]).
    /// `None` (a journal a build before the mark wrote): no words compared.
    pub fn seed_done(&mut self, words: Option<u64>) {
        self.done_seed = Some(words);
    }

    /// THE TIME THE SEEDED END WAS NOTED (§10.2.7): the wall time (unix ms)
    /// of the journal's note of the landing or the done a loop before this
    /// one read ([`Self::seed_landed`], [`Self::seed_done`]), told with the
    /// start face that restates it ([`Self::face_start_noted`]). A window
    /// that held the face over the host's "unknown" gives back its own first
    /// time; one that held none — a seamless update's new window, which no
    /// face of the session's crossed — took the start's clock, so `done
    /// 13:04` read `done 13:27` after an update at 13:27. Read by nothing
    /// the policy decides.
    pub fn seed_noted_at(&mut self, noted_ms: i64) {
        self.seed_noted_ms = Some(noted_ms);
    }

    /// The time to tell with the face at the loop's start
    /// ([`Self::face_start`]): the seeded end's note ([`Self::seed_noted_at`])
    /// where that face is the end a seed restates — landed with a landing
    /// seeded, done with a done seeded — and `None` for any other start face
    /// (open, a done the task's record says, no note's time).
    #[must_use]
    pub fn face_start_noted(&self) -> Option<i64> {
        let restated = match self.face_told? {
            TaskFace::Landed => self.landed_seed.is_some(),
            TaskFace::Done => self.done_seed.is_some(),
            TaskFace::Open | TaskFace::Spent | TaskFace::Theirs => false,
        };
        self.seed_noted_ms.filter(|_| restated)
    }

    /// THE END THIS POLICY HOLDS IS A SEED RESTATED (§10.2.7): where the
    /// loop's first point took a landing's seed up ([`Self::seed_landed`])
    /// or kept a done's ([`Self::seed_done`]: the same words) and the face
    /// is still that end, the wall time (unix ms) the end was FIRST noted
    /// ([`Self::seed_noted_at`]) — written with the note this loop journals
    /// for it, so the next loop's seed carries the same time however many
    /// restarts and seamless updates restate it; each restated note is
    /// newer than the point records its loop wrote, which is what lets the
    /// next loop seed at all. `None` for an end of this loop's own: no seed,
    /// one dropped or withdrawn, a face moved since, a live read since.
    #[must_use]
    pub fn restated_noted_ms(&self) -> Option<i64> {
        self.restating
            .filter(|(face, _)| *face == self.face())
            .map(|(_, at)| at)
    }

    /// A done seeded from a loop before this one is armed, not yet resolved
    /// at a first point nor dropped ([`Self::seed_done`]): a projection for a
    /// test.
    #[must_use]
    pub fn done_seed_armed(&self) -> bool {
        self.done_seed.is_some()
    }

    /// THE DONE RESTATED AT THIS LOOP'S START, WITHDRAWN AT ITS FIRST POINT
    /// ([`Self::seed_done`]), where that point reads done again — a new
    /// done, another reply's: `open` to tell the host BEFORE the point's
    /// face ([`Self::face_moved`], which then tells the done as a move), so
    /// the window stamps the new done with its own time rather than keeping
    /// the restated one's. Once, and `None` otherwise: no seed withdrawn, or
    /// the face moved off done there anyway (its move is told, and stamped).
    pub fn face_withdrawn(&mut self) -> Option<TaskFace> {
        if !std::mem::take(&mut self.done_withdrawn)
            || self.face_told != Some(TaskFace::Done)
            || self.face() != TaskFace::Done
        {
            return None;
        }
        self.face_told = Some(TaskFace::Open);
        self.restating = None;
        Some(TaskFace::Open)
    }

    /// THE LAST REPLY SAID DONE, as this policy read it at its last point: a
    /// done report ([`says_done`], or the worker's [`FINISHED`]) — a first
    /// one, or the task done or landed — or a landing a loop before this one
    /// landed, armed for its first point ([`Self::seed_landed`]). Never a
    /// reply a wall cut ([`Self::cut_reply`]: the land instruction's turn
    /// ended at the memory banner, say, though the done report before it
    /// stands), never a person's own exchange ([`Self::left_to_person`]:
    /// theirs to answer), and never once someone else's turn has been read
    /// live since ([`Self::other_turn`]). Read by the loop for its host's step at an
    /// idle point (`IdlePoint::done_under_land`): under Land, a relaunched
    /// agent whose last reply said done is carried on with nothing typed,
    /// since the relaunch's carry-on would start again the work the person
    /// asked to wrap up (the Autopilot design's §9.4.8).
    #[must_use]
    pub fn said_done(&self) -> bool {
        !self.other_turn
            && !self.cut_reply
            && !self.left_to_person()
            && (self.done >= 1 || self.landed_seed.is_some())
    }

    /// A live turn of someone else's has been read since the last point
    /// ([`Self::other_turn`]): the conformance walk's projection.
    #[must_use]
    pub fn other_turn(&self) -> bool {
        self.other_turn
    }

    /// THE TASK'S FACE NOW ([`TaskFace::of`] over this state and
    /// [`Self::other_turn`]).
    #[must_use]
    pub fn face(&self) -> TaskFace {
        TaskFace::of(self, self.other_turn)
    }

    /// THE FACE TO TELL THE HOST (`IdleHost::task_face`, design §9.4.2), where
    /// it moved since the loop last told one: the loop asks at each point it
    /// folds in and at each live read of someone else's turn, and tells what
    /// this hands back. `None`: nothing moved, nothing is told.
    pub fn face_moved(&mut self) -> Option<TaskFace> {
        let face = self.face();
        let moved = (self.face_told.replace(face) != Some(face)).then_some(face);
        // A face told after the start's: the end held is no seed restated
        // ([`Self::restated_noted_ms`]).
        if moved.is_some() {
            self.restating = None;
        }
        moved
    }

    /// The face at the loop's start, told whatever it is — open, landed
    /// where a landing is seeded ([`Self::seed_landed`]), done where a done
    /// is ([`Self::seed_done`]): the host told the window "unknown" when the
    /// loop before this one ended.
    pub fn face_start(&mut self) -> TaskFace {
        let face = self.face();
        self.face_told = Some(face);
        face
    }

    /// The task is done because the turn was A PERSON'S OWN EXCHANGE: their
    /// message, answered within [`EXCHANGE_WORK`], on a task this policy was
    /// not carrying ([`Self::observe`]). Nothing is typed until the next turn
    /// ends: the reply is theirs to answer, whatever it says.
    #[must_use]
    pub fn left_to_person(&self) -> bool {
        self.task_done() && self.exchange
    }

    /// The act the point last judged followed: this policy's rule
    /// ([`RULE_DONE_CHECK`] for a done check it typed), [`RULE_HOST_TURN`]
    /// for a turn its host typed — `None` for SOMEONE ELSE'S TURN (a
    /// person's, an orchestrator's) or a loop's first point. A projection,
    /// for the reader of the task record's edges (`supervise::task_mark`);
    /// it decides nothing.
    #[must_use]
    pub fn followed(&self) -> Option<&'static str> {
        self.followed
    }

    /// Which point this policy judged last ([`Self::observe`]), counted from
    /// `1`; `None` before the first: the point's identity for a reader that
    /// acts once per point (the task record's `done`, `supervise::task_mark`).
    /// A point decided again keeps it, and so does a short turn its host
    /// typed. A COUNT, NOT AN INSTANT: a sleep moves the point's first
    /// showing back ([`Self::slept`]), and an identity read off that stamp
    /// made the same point a new one at the wake.
    #[must_use]
    pub fn point_number(&self) -> Option<u64> {
        (self.points > 0).then_some(self.points)
    }

    /// WHEN THE DONE STANDING NOW WAS DECIDED, on the loop's clock: the
    /// instant of the read that first showed it ([`Self::observe`],
    /// [`Self::finished_over_background`]; for a done a loop took on from
    /// its record, the point it took it on at, [`Self::seed_task_done`]),
    /// moved back with the wall clock across a sleep ([`Self::slept`]);
    /// `None` while no done stands. What the done knew, for the result
    /// reader (`supervise::task_mark`): a transcript row stamped after it
    /// was written after the harness read the done.
    ///
    /// NOT THE POINT'S FIRST SHOWING (`point_at`): a turn the host
    /// typed (a relaunched agent's carry-on, the live upgrade's notice, the
    /// background notice) answered with [`FINISHED`] ends the task with no
    /// new point, so that point showed before the turn the done ends began.
    #[must_use]
    pub fn done_shown(&self) -> Option<Instant> {
        self.done_at
    }

    /// A MANAGER WAITS ON THIS SESSION (`managed`): the loop found a task
    /// record on it (`supervise::task_mark`) — a worker, started by `spawn
    /// agent=` for a manager waiting on its `await task`. Its done check and
    /// its answers then go at once, without the short back-off that saves an
    /// idle person's tokens ([`continue_policy`]), for its first
    /// [`MANAGED_SHORT_TURNS`] short turns in a row — past them, a worker
    /// that asks after every short turn is answered at the back-off's pace,
    /// not the model's. A continuation keeps the back-off.
    pub fn set_managed(&mut self, managed: bool) {
        self.managed = managed;
    }

    /// Whether a manager waits on this session ([`Self::set_managed`]).
    #[must_use]
    pub fn managed(&self) -> bool {
        self.managed
    }

    /// The point standing now is the one the worker's [`FINISHED`] made over
    /// its own background work ([`Self::finished_over_background`]), that
    /// work still running and no live turn since.
    #[must_use]
    pub fn done_over_background(&self) -> bool {
        self.background_done
    }

    /// THE TASK IS DONE BY ITS RECORD: a loop that starts while its session's
    /// task record reads `done` — published by a loop before it (the window
    /// harness's fault restart, a seamless update's successor) — takes that
    /// done on at its first point (`supervise::task_mark`), so neither the
    /// done check nor a continuation is typed into the finished worker, and
    /// someone else's turn after it opens the task again as after any done
    /// ([`Self::observe`]). A done this state reached itself stands as it is.
    ///
    /// NOT WHERE THE POINT COULD NOT BE A DONE: a wall's track standing (the
    /// point shows one, or is decided as the one it left) or an act of this
    /// policy's owed. A done point never shows a wall, so a walled first
    /// point ended a turn after the published done — someone else's `send`,
    /// paste or keystrokes, which the server does not reopen on, met a 529 or
    /// a limit — and taken on as done, its retry, its resume or its `/login`
    /// was never typed. `false` there: the state is as the point left it,
    /// and the wall's ladder acts.
    ///
    /// The job is over with it, as at any task's end ([`Self::observe`]): a
    /// challenge owed is dropped — the loop before this one judged the job
    /// done.
    pub fn seed_task_done(&mut self) -> bool {
        if self.task_done() {
            return true;
        }
        if self.wall.is_some() || self.awaiting.is_some() || self.after.is_some() {
            return false;
        }
        self.done = 2;
        self.done_at = self.point_at;
        self.finished = false;
        self.exchange = false;
        self.carrying = false;
        self.checking = false;
        self.open.clear();
        self.repo = None;
        true
    }

    /// A LIVE TURN READ — the loop's busy read with no break of background
    /// work in it — and who opened it, as the screen says
    /// ([`aterm_phase::opened_by_own_wake`]: `Some(true)` Claude Code's own
    /// wake at a background command's end or a monitor's event, `Some(false)`
    /// a person's message, `None` neither drawn). Where the point the
    /// worker's [`FINISHED`] made over its background work stands
    /// ([`Self::finished_over_background`]), a turn ITS OWN WAKE opened is
    /// PART OF THAT TASK: Claude Code wakes itself when the work it left
    /// ends, and goes on by itself. Read as someone else's turn, the task was
    /// opened again — and published again with the wake's words for its
    /// result, or the done check and `keep going` typed into the finished
    /// worker. The point stands (the screen the wake leaves is that point,
    /// [`Self::observe`]), nothing is typed, and `true` is returned the first
    /// time (the loop says so once); a later read of the same wake with no
    /// opener on the screen is still the wake's. Anything else — a person's
    /// message, an act of ours awaited, no done over background work — is
    /// [`Self::took`]: the worker works.
    pub fn live_turn(&mut self, opened_by_wake: Option<bool>) -> bool {
        if !self.own_wake(opened_by_wake) {
            self.took();
            return false;
        }
        !std::mem::replace(&mut self.woken, true)
    }

    /// Whether a live turn opened as `opened_by_wake` says
    /// ([`Self::live_turn`]) is the agent's OWN WAKE past the done its
    /// [`FINISHED`] made over its background work: that done stands, no act
    /// of ours is awaited, and the screen names the wake as the opener (or,
    /// naming none, a wake was read since). It changes nothing: a loop that
    /// does more at someone else's turn than [`Self::took`] asks it first.
    #[must_use]
    pub fn own_wake(&self, opened_by_wake: Option<bool>) -> bool {
        self.background_done && self.awaiting.is_none() && opened_by_wake.unwrap_or(self.woken)
    }

    /// THE WORKER'S [`FINISHED`] OVER ITS OWN BACKGROUND WORK: a busy screen
    /// whose turn is over and only a shell or a monitor it left keeps busy
    /// ([`TurnEndReading::background`]) whose last words end with the token
    /// ends the task at once, as the token does at any point
    /// ([`Self::observe`]) — the worker's own verdict, whatever it left
    /// running. Before it, a shell that ran for hours (or a dev server that
    /// never ends) held the done back for as long, and a manager's `await
    /// task` with it. The turn's end is folded in as a point of its own (a
    /// new [`Self::point_number`]; the act it answered, if any, judged by
    /// it), and nothing else of the screen is decided: no act goes at a busy
    /// screen, and the host's step there types only what ends nothing. That
    /// point stands while the work runs — the screen read again changes
    /// nothing — through the agent's own wake when the work ends
    /// ([`Self::live_turn`]), and the idle screen the work or the wake leaves
    /// is the same point ([`Self::observe`]); a turn someone else opened
    /// since ([`Self::took`]: a message typed, an act of ours) is a turn of
    /// its own, judged at its point as any. Not while an act is awaited that
    /// no live turn has taken: the words on the screen are from before it.
    /// Nor while anything of the job is left — the report names open work
    /// ([`open_work`]) or the session's repository holds work no remote has
    /// ([`TurnEndReading::repo`]): the token ends no job then (the owner's
    /// rule of 2026-10-19), and the turn's end is judged at its idle point,
    /// where the challenge goes. `true` where the task ended here.
    pub fn finished_over_background(&mut self, reading: &TurnEndReading, now: Instant) -> bool {
        let untaken = self.awaiting.is_some_and(|a| !a.taken) || self.after.is_some();
        if reading.phase != Phase::Busy
            || !reading.background
            || self.background_done
            || untaken
            || reading.wall.is_some()
            || reading.pending_input
            || !says_finished(reading.said_tail.as_deref())
            || !open_work(reading.said_tail.as_deref()).is_empty()
            || reading.repo.is_some()
        {
            return false;
        }
        self.checking = false;
        self.followed = self.awaiting.take().map(|a| a.rule);
        self.point_worked = Duration::ZERO;
        self.new_point(now);
        self.done = 2;
        self.done_at = Some(now);
        self.finished = true;
        self.exchange = false;
        self.carrying = false;
        self.background_done = true;
        true
    }

    /// The work of the person's exchange in all so far: their turn's, plus
    /// each wall's act that delivered it again ([`Self::exchange_at_wall`]'s
    /// row carries it to a new loop, [`TurnEndReading::left_work`]).
    #[must_use]
    pub fn exchange_work(&self) -> Duration {
        self.exchange_work
    }

    /// A PERSON'S OWN EXCHANGE STANDS AT A WALL: their message, answered
    /// within [`EXCHANGE_WORK`], met a wall whose ladder runs
    /// ([`Self::observe`]); its retry is typed, and the retry's answer is
    /// theirs while the exchange's work in all stays within it (a wall splits
    /// a job, it never makes it an exchange). The loop makes it
    /// durable (a `skipped` row under [`RULE_LEFT`], the review's RR4-TE-2):
    /// kept only here, a new loop whose first point was the wall typed the
    /// retry and then `Nobody is here to answer…` or the done check into the
    /// person's greeting ([`TurnEndReading::left_record`] at a wall).
    #[must_use]
    pub fn exchange_at_wall(&self) -> bool {
        self.exchange && self.wall.is_some() && !self.task_done()
    }

    /// A PERSON'S HAND OPENED THE TURN ENDING AT `reading`: their last window
    /// gesture ([`TurnEndReading::person`]) is inside the turn
    /// ([`TurnEndReading::person_in_turn`]) and after this policy's last
    /// point and its last act.
    fn hand(&self, reading: &TurnEndReading, now: Instant) -> bool {
        let Some(at) = reading.person.and_then(|ago| now.checked_sub(ago)) else {
            return false;
        };
        reading.person_in_turn()
            && self
                .point_at
                .max(self.acts.back().copied())
                .is_none_or(|since| at > since)
    }

    /// The back-off before the next act after the short streak: none after
    /// real work, else [`TurnEndTiming::short_backoff`] doubled for each
    /// short turn after the first, at most [`TurnEndTiming::
    /// short_backoff_max`].
    #[must_use]
    pub fn short_wait(&self) -> Option<Duration> {
        let n = self.short.checked_sub(1)?;
        let t = &self.timing;
        Some(crate::supervise::ladder::doubling(
            t.short_backoff,
            n,
            t.short_backoff_max,
        ))
    }

    /// A RETRY'S WALL STANDS WHERE ITS NOTICE LEFT the screen with no work
    /// since ([`left_retry`]: a server's, an overload's, a network never
    /// reached, a cut-off or an overlong reply; never a refused request): a
    /// point is decided as that wall, its rung held. The host's step at such
    /// a point is withheld as at the wall (the decision review's RR-DEC-3:
    /// the live upgrade's notice turn, or a relaunch and its carry-on, was
    /// typed into the outage the policy was waiting out). Work ends the
    /// track ([`Self::observe`]).
    #[must_use]
    pub fn holds_left_retry(&self) -> bool {
        self.wall.as_ref().and_then(left_retry_kind).is_some()
    }

    /// An act of this policy's is in flight: typed and its point not judged,
    /// or a command's continuation owed. A host-typed turn
    /// ([`Self::host_typed`]) is not: the host's next step reads its own
    /// evidence.
    #[must_use]
    pub fn act_in_flight(&self) -> bool {
        self.awaiting.is_some_and(|a| a.rule != RULE_HOST_TURN) || self.after.is_some()
    }

    /// WHAT `reading` SAYS OF THE ACCOUNT MOVE WAITS ON THE ACT AWAITED: a
    /// move stands untold, and a turn typed under it (the wall's resume, the
    /// host's own) has had no busy read since — so [`Self::account_answer`]
    /// says nothing of a wall here until [`Self::observe`] judges that act.
    /// The loop reads the answer again right after that fold, before the
    /// host is asked about the point: a lost login after a turn asked under
    /// the login the move chose is struck on THAT login before any advice
    /// for its point.
    #[must_use]
    pub fn account_answer_awaited(&self, reading: &TurnEndReading) -> bool {
        self.account.is_some()
            && !self.account_told
            && self.awaiting.is_some()
            && reading.worked.is_none()
    }

    /// An act of this policy's the agent has not taken yet: typed with no
    /// live turn seen since ([`Self::took`]), or a command's continuation
    /// owed. Holds back the host's step at a break of the agent's background
    /// work (`Session::host_steps_in_background`), so no host line is stacked
    /// on an unread continuation; once a live turn is seen, the break is the
    /// agent's own. The following point still judges the act
    /// ([`Self::observe`]); a host line typed at the break
    /// ([`Self::host_typed`]) takes its place.
    #[must_use]
    pub fn act_untaken(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.rule != RULE_HOST_TURN && !a.taken)
            || self.after.is_some()
    }

    /// THE TEXT A `/COMPACT`'S CONTINUATION TYPES IN PLACE OF `keep going`
    /// ([`Self::compact_owes`]): the act the full context cut, as this policy
    /// typed it ([`Self::cut_text`]) — or, for one it did not type (the
    /// restart's check, a check a loop before this one asked), the done
    /// check with [`DONE_CHECK_REVERSIBLE`] after it (nothing says the report
    /// named no irreversible act), or a land nudge. `None` where nothing is
    /// owed, or `rule` is no compaction's.
    fn compact_owed_text(&self, rule: &str, rules: Option<&str>) -> Option<String> {
        let owes = self.compact_owes.filter(|_| rule == RULE_COMPACT)?;
        Some(self.cut_text.clone().unwrap_or_else(|| {
            if owes.land && !owes.check {
                land_nudge_text(rules)
            } else {
                done_check_text(rules, true)
            }
        }))
    }

    /// A `/compact` of this policy's is running: typed, and its point not
    /// judged yet. The spinner a busy read shows meanwhile is the compaction's
    /// own run, no sign that the agent works ([`Self::observe`] counts it no
    /// work either), so it ends no exception that stands over the wall
    /// ([`exception_for`](super::exception::exception_for)). The continuation
    /// that follows it is a text act, and its spinner is the agent's.
    #[must_use]
    pub fn compacting(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.command && a.rule == RULE_COMPACT)
    }

    /// THE WALL'S RETRY AWAITED ([`Awaited::wall_retry`]: the limit's try
    /// among them) WAS ANSWERED BY A WALL inside its take window, and no read
    /// has seen it taken yet ([`Awaited::met`]): a live turn read now takes
    /// it ([`Self::took`]), and that wall was the screen it was typed at, not
    /// its answer — the work since is the retry's own turn, which starts its
    /// wall's episode again at its point ([`Self::track_wall`]). The loop
    /// writes so where it ends the remembered wall
    /// (`Session::end_memory_on_live_turn`,
    /// [`crate::supervise::approvals::WALL_TAKEN`]): the wall's row is no try
    /// met again for a loop that follows either
    /// ([`crate::supervise::approvals::UsageAttempt::met_ms`]).
    #[must_use]
    pub fn retry_met_untaken(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.wall_retry && a.met && !a.taken)
    }

    /// When the act [`Self::untaken_act`] names first showed a point with no
    /// busy read since ([`Awaited::unseen_since`]): its take window's start,
    /// if one showed.
    #[cfg(test)]
    pub(crate) fn untaken_act_seen(&self) -> Option<Instant> {
        self.awaiting
            .as_ref()
            .filter(|a| !a.taken)
            .and_then(|a| a.unseen_since)
    }

    /// When the act this policy typed last went, while it stands UNTAKEN: no
    /// read since saw the agent's live turn ([`Self::took`]) — inside its
    /// take window or past it. A slash command (no turn of its own) and a
    /// turn the host typed are not such an act. `None` otherwise.
    ///
    /// A usage wall on the screen at such a read ANSWERED the act: Claude Code
    /// holds a line typed into a standing limit and runs no turn for it, and
    /// takes one typed after the limit lifted (a busy read). The limit stands
    /// NOW, which is what the loop's placement of the wall's reset reads it as
    /// (a print it watched, `Session::turn_end_reading`): a bare clock is the
    /// next one, never a reset the session rule guessed passed. Inside the
    /// window as past it (round 16's review): the stub's 429 answers the line
    /// at once, and the wall read there is tracked ([`Self::observe`]), so a
    /// placement made there is the one its notice leaves behind.
    #[must_use]
    pub fn untaken_act(&self) -> Option<Instant> {
        self.untaken().map(|a| a.at)
    }

    /// When [`Self::untaken_act`]'s act was typed on the policy's clock
    /// ([`Awaited::typed`]): what a time on the wall clock — a done row's —
    /// is set against, across a sleep as before it.
    #[must_use]
    pub fn untaken_act_typed(&self) -> Option<Instant> {
        self.untaken().map(|a| a.typed)
    }

    fn untaken(&self) -> Option<&Awaited> {
        self.awaiting
            .as_ref()
            .filter(|a| a.rule != RULE_HOST_TURN && !a.command && !a.taken)
    }

    /// THE LIMIT A CANCEL LEFT, ANSWERING A TURN NO READ SAW ANSWERED (round
    /// 16's residual (a), the lab's `tw16after` on 2.1.300; round 17's residual (a)): a
    /// limit answered a turn at once, and a person's Esc on the vendor's
    /// countdown came before the loop's first read after it — so that read
    /// shows only `⏺ Automatic continue cancelled` under the notice, no wall,
    /// and the limit was never tracked: the continue policy typed `keep
    /// going` into it. The limit a cancel left on `rows`
    /// ([`aterm_phase::left_wall`]: the cancel the last thing said, right
    /// under the newest notice, which names its reset and has no turn served
    /// under it) answered
    ///
    /// * an ACT OF THIS POLICY'S OWN standing untaken ([`Self::untaken_act`])
    ///   that NO WALL HAS ANSWERED yet ([`Awaited::met`]: from then on the
    ///   notice is the track's to decide, [`still_shown`]) — asked when it
    ///   was typed; the notice is new where it is not the one the act was
    ///   typed over ([`Awaited::left`]);
    /// * else ANYONE'S TURN — a person's prompt, the host's carry-on, the
    ///   turn an act of ours led to — asked since the policy LAST LOOKED
    ///   ([`Self::observe`]), while it waits on no limit of its own (whose
    ///   notice is the track's); the notice is new where that look drew no
    ///   such notice. The FIRST look says nothing: a cancelled limit found
    ///   standing is no answer of anyone's (the left-wall rule of 7057c6acf:
    ///   a notice says something of the limit it names, and of no other).
    ///
    /// Handed back: that wall, which the loop reads in the screen's place
    /// (`Session::turn_end_reading`) so that it is confirmed, placed,
    /// remembered and folded in ([`Self::observe`]) as a wall read there is;
    /// WHEN its turn was asked, at the earliest — its notice, to be its
    /// answer, was written no earlier, and a conversation `/resume`d since
    /// ends on its own older cancelled limit of the same class, which only
    /// the session's transcript dates; and whether the SCREEN tells it from
    /// what the policy already saw ([`LeftAnswer::screen_tells`]). `None`
    /// everywhere else.
    #[must_use]
    pub fn left_answer(&self, rows: &[String]) -> Option<LeftAnswer> {
        let wall = aterm_phase::left_wall(rows)?;
        let key = left_key(
            aterm_phase::since_limit(rows).as_ref(),
            aterm_phase::auto_continue_end(rows),
        )?;
        let own = self
            .awaiting
            .as_ref()
            .filter(|a| a.rule != RULE_HOST_TURN && !a.command && !a.taken);
        let (since, seen) = match own {
            Some(a) if a.met => return None,
            Some(a) => (a.at, a.left),
            None if self.waits_on_named_reset() => return None,
            None => self.looked?,
        };
        Some(LeftAnswer {
            wall,
            since,
            screen_tells: seen != Some(key),
        })
    }

    /// A read saw the agent's LIVE TURN (its spinner, its busy footer — never
    /// a break of its own background work): the act awaited, if any, was
    /// taken ([`Self::act_untaken`]).
    ///
    /// The worker works: a limit reset's continuation, if one was owed
    /// ([`Self::reset_told`]), is paid — the vendor's own queued one started,
    /// or someone asked a turn — and a point its [`FINISHED`] made over its
    /// background work is behind it ([`Self::finished_over_background`]):
    /// the point this turn ends at is its own. (The agent's own wake at that
    /// work's end is no such turn: [`Self::live_turn`].)
    ///
    /// A live turn with NO act of this policy's awaited (nor a command's
    /// continuation owed) is SOMEONE ELSE'S ([`Self::other_turn`]): a
    /// person's, an orchestrator's — the host's own line is awaited as the
    /// policy's acts are ([`Self::host_typed`]). The task's face is open again
    /// from this read ([`TaskFace::of`]). And a landing or a done seeded
    /// from a loop before this one ([`Self::seed_landed`],
    /// [`Self::seed_done`]) holds no more: a live read came before its first
    /// point.
    pub fn took(&mut self) {
        if self.awaiting.is_none() && self.after.is_none() {
            self.other_turn = true;
        }
        // A turn this loop reads running itself ([`Self::seed_ran_turn`]).
        self.ran_turn_seed = None;
        self.landed_seed = None;
        self.landed_words = None;
        self.done_seed = None;
        self.seed_noted_ms = None;
        self.restating = None;
        // The first turn read running since the point recorded last
        // ([`Self::take_ran`]).
        self.ran |= std::mem::take(&mut self.point_last);
        // This loop reads the turn itself: its own measure is the work.
        self.gap = false;
        // The worker works: whatever Esc is read next stopped this turn.
        self.esc_at = None;
        self.esc_seed = false;
        if let Some(a) = &mut self.awaiting {
            a.taken = true;
            // A wall read before it was the screen the act was typed at, not
            // the act's answer.
            a.met = false;
        }
        // An act a loop before this one typed, its answer judged already:
        // this turn is a later one, its done report a first one.
        if std::mem::take(&mut self.seed_answered) {
            self.checking = false;
            self.landing = false;
        }
        self.reset_resume = None;
        // A turn running as a loop started as Off lifted attached: the
        // carry its seeded acts gave holds for it ([`Self::start_lifted_off`]).
        if std::mem::take(&mut self.carry_at_attach) {
            self.carrying = true;
        }
        self.background_done = false;
        self.woken = false;
    }

    /// A new point judged at `now`: its first showing, and its identity
    /// ([`Self::point_number`]).
    fn new_point(&mut self, now: Instant) {
        self.point_at = Some(now);
        self.points = self.points.saturating_add(1);
    }

    /// A BOX WAS READ UP (a permission box, a dialog): the worker asked
    /// something since the point before it, so a person's Esc read after it
    /// — their refusal of the box — is a new one, its grace counted from
    /// that read ([`Self::esc_at`]), whether or not a busy read came between.
    ///
    /// For a loop that follows, a box read up after the point recorded last
    /// is the worker's work since it, as a turn read running is
    /// ([`Self::take_ran`]): that loop reads its first idle point as a later
    /// one, never the point recorded read again with the Esc it stood at
    /// (a refusal of the box would be held from that old Esc).
    pub fn box_up(&mut self) {
        self.esc_at = None;
        self.esc_seed = false;
        self.ran |= std::mem::take(&mut self.point_last);
        self.gap = false;
    }

    /// When the person's Esc standing now was first read ([`Self::esc_at`]).
    #[cfg(test)]
    pub(crate) fn esc_read(&self) -> Option<Instant> {
        self.esc_at
    }

    /// THE MAC SLEPT `by`, as the loop measured it (the wall clock ran ahead
    /// of the monotonic one by that much: `Instant` stops while the Mac
    /// sleeps, the wall clock does not). Every stamp kept across the sleep
    /// reads `by` too recent, so a pause counted from one would be stretched
    /// by the night: the stamps this policy's pauses and windows count from,
    /// and the resets it keeps as `Instant`s, move `by` earlier, and a pause
    /// a sleep went through is over at the wake, as on the wall clock:
    ///
    /// * the wall being worked: its appearance (every ladder's rung, the down
    ///   hold, the exceptions' hours) and the reset its notice named
    ///   ([`still_shown`]);
    /// * the point's first showing (the short-turn back-off) — not the
    ///   point's identity, a count no clock moves ([`Self::point_number`]) —
    ///   the done's ([`Self::done_shown`]), and a person's Esc's first reading
    ///   (their grace, [`Self::esc_at`]);
    /// * the owner's hour: the typing budget (`continue_per_hour`), and a
    ///   move refused ahead of a wall, asked again an hour on;
    /// * the reset the model switch goes back at;
    /// * when the act awaited was typed, as a wall clock's time is set
    ///   against it ([`Awaited::typed`]), and a seeded try nobody read
    ///   ([`UnreadTry::at`]).
    ///
    /// NOT the worker's own clocks, which the sleep stopped too: the time an
    /// act has to be taken ([`TurnEndTiming::take_within`]). NOR a stamp a
    /// person's keystroke is set against — the reset continuation's `told`:
    /// a keystroke's age is read on an awake clock, and moving one side of
    /// the comparison would make an old keystroke new. NOR the last lines against a
    /// hot loop — the wall breaker ([`WALL_ACTS_PER_WINDOW`]) and the account
    /// moves ([`ACCOUNT_MOVES_PER_WINDOW`]): no healthy hour reaches them
    /// (every ladder paces itself under them), so they count the hours the
    /// harness ran, and a wall clock STEPPED forward — which reads as a sleep
    /// here on a machine that keeps no count of its sleep (where it keeps one,
    /// a step is none: `Session::watch_sleep`) — never reopens them. A stamp
    /// the clock cannot hold that far back is left as it is.
    pub fn slept(&mut self, by: Duration) {
        let back = |t: &mut Instant| {
            if let Some(earlier) = t.checked_sub(by) {
                *t = earlier;
            }
        };
        for t in &mut self.acts {
            back(t);
        }
        if let Some((_, t)) = &mut self.account_ahead_unmade {
            back(t);
        }
        if let Some(t) = &mut self.point_at {
            back(t);
        }
        if let Some(t) = &mut self.done_at {
            back(t);
        }
        if let Some(t) = &mut self.point_seed {
            back(t);
        }
        if let Some(t) = &mut self.esc_at {
            back(t);
        }
        // The act awaited's time on the policy's clock, not its take window's
        // ([`Awaited::typed`]), and the unread try's ([`UnreadTry::at`]).
        if let Some(a) = &mut self.awaiting {
            back(&mut a.typed);
        }
        if let Some(u) = &mut self.unread_try {
            back(&mut u.at);
        }
        if let Some(w) = &mut self.wall {
            back(&mut w.since);
            if let Some(n) = &mut w.named {
                back(&mut n.at);
            }
            // A lost login's own stamps: the read that saw it answer a
            // prompt, and this loop's `/login` (its dialog's window).
            for t in [&mut w.login.prompt_at, &mut w.login.typed_at]
                .into_iter()
                .flatten()
            {
                back(t);
            }
        }
        if let Some(t) = self.model.as_mut().and_then(|m| m.back_at.as_mut()) {
            back(t);
        }
        // The credits' window's reset, a wall-clock instant as a bucket's is,
        // and the episode's start the bound with no reset counts from.
        if let Some(c) = &mut self.credits {
            for t in [&mut c.reset, &mut c.passed, &mut c.since]
                .into_iter()
                .flatten()
            {
                back(t);
            }
        }
    }

    /// THE LOOP'S LIMIT-RESET STEP ENDED, and its host read `outcome`
    /// (`crate::supervise::IdleHost::reset_told`): what the session is owed
    /// now is ONE continuation, typed by the limit-resume rule under
    /// `resume_limits` at the next free point ([`act_at_point`],
    /// [`wall_action`]):
    ///
    /// * [`ResetOutcome::Spent`] and [`ResetOutcome::Lifted`] whose line ASKS
    ///   for a message, [`ResetOutcome::Kept`], [`ResetOutcome::Failed`] and
    ///   [`ResetOutcome::Unread`]: owed from `now`. After a reset kept, failed
    ///   or unread the wall may still stand: the continuation is the probe
    ///   that costs no relaunch, and a wall that answers it is rotated from
    ///   or waited out by its own rule;
    /// * `Spent` and `Lifted` whose line asks for nothing (the vendor queued
    ///   its own continuation): owed from [`RESET_CONTINUES_WAIT`] past
    ///   `now`, so the vendor's is never doubled;
    /// * [`ResetOutcome::Pending`] is no end: nothing is owed.
    ///
    /// `Spent` and `Lifted` cleared the claimed wall (`cleared_at`), which
    /// the host then advises as it does the owner's own clearing
    /// ([`AccountAdvice::Resume`]): that clearing is answered by this
    /// continuation, so the advice types nothing more for it
    /// ([`account_first`]).
    ///
    /// While it is owed the host is not asked about the account
    /// ([`Self::account_point`]), and so no probe is made
    /// ([`Self::probe_gate`]). Any act typed ([`Self::acted`]), a turn the
    /// host typed ([`Self::host_typed`]) and work seen since `now`
    /// ([`Self::took`], [`Self::observe`]) pay it.
    pub fn reset_told(&mut self, outcome: &ResetOutcome, now: Instant) {
        let (asks, cleared_at) = match *outcome {
            ResetOutcome::Pending => return,
            ResetOutcome::Spent {
                asks, cleared_at, ..
            }
            | ResetOutcome::Lifted { asks, cleared_at } => (asks, Some(cleared_at)),
            ResetOutcome::Kept | ResetOutcome::Failed { .. } | ResetOutcome::Unread => (true, None),
        };
        self.reset_resume = Some(ResetResume {
            told: now,
            due: if asks {
                now
            } else {
                later(now, RESET_CONTINUES_WAIT)
            },
        });
        if let Some(cleared_at) = cleared_at {
            self.account_resumed = Some(
                self.account_resumed
                    .map_or(cleared_at, |typed| typed.max(cleared_at)),
            );
        }
    }

    /// From when the continuation a limit reset left is typed, while one is
    /// owed ([`Self::reset_told`]).
    #[must_use]
    pub fn reset_resume_owed(&self) -> Option<Instant> {
        self.reset_resume.map(|owed| owed.due)
    }

    /// The model switch in force, if any.
    #[must_use]
    pub fn model_switch(&self) -> Option<&ModelSwitch> {
        self.model.as_ref()
    }

    /// Whether the act [`Self::acted`] last recorded (a typed act; a restart
    /// is [`Self::restarted`]'s) was typed IN A LIMIT'S
    /// PLACE: the done check, the reversible answer, or under Land a land
    /// act at a limit's end ([`limit_end`]), counted as the wall's attempt as
    /// the continuation it stands in for would be ([`LimitTry::in_place`]).
    #[must_use]
    pub fn acted_in_limits_place(&self) -> bool {
        self.limit_try.is_some_and(|l| l.in_place)
    }

    /// THE LIMIT'S TRY the act [`Self::acted`] last recorded was, if it was
    /// one ([`LimitTry`]): `usage-resume@v1`, or an act typed in its place,
    /// at a usage window's or a model bucket's track — `None` for any other
    /// act, a restart, or a continuation no such track stood behind. Its
    /// ledger row carries it ([`crate::supervise::approvals::act_reason`]),
    /// so a loop that starts after it works the limit from that act
    /// ([`crate::supervise::approvals::usage_attempt`],
    /// [`Self::seed_usage_attempt`]) as the loop that typed it does.
    #[must_use]
    pub fn limit_try(&self) -> Option<LimitTry> {
        self.limit_try
    }

    /// A LIMIT'S TRACK ENDED at the last fold ([`Self::observe`]), by no act
    /// of this policy's: a usage window's or a model bucket's
    /// ([`named_class`]) dropped where the worker worked, a retry was taken
    /// or the vendor went on, or the conversation holds no task (a `/clear`);
    /// begun again as a new episode by the work a try led to; or another
    /// class's wall opened its own track in its place ([`Self::track_wall`]).
    /// The episode its tries belong to is over for this loop — the limit met
    /// again is a new appearance, on its first rung — and the loop writes so
    /// for one that follows ([`crate::supervise::approvals::LIMIT_ENDED`],
    /// [`crate::supervise::approvals::WALL_LEFT_NO_TASK`]): a try such an end
    /// follows is no seed ([`crate::supervise::approvals::usage_attempt`]).
    /// Measured before (the verifier of ed6efadb0, V22-A): only an account
    /// move voided a try, so a successor took up a try whose episode the
    /// worker's work had ended and worked a new wall of the same class as
    /// that try's rung — a spend limit's try, two hours of work, then a
    /// five-hour window whose reset was unread, and `usage-resume@v1` the
    /// moment the successor attached, where the first loop waited 30 min. So
    /// too where a `/clear` or a 529 in the try's own turn dropped the track
    /// with no work row (the verifier of e09018898, B5-1): the successor
    /// held the try's rung on the next spend limit, and typed into it 15
    /// minutes before the first loop.
    #[must_use]
    pub fn limit_ended(&self) -> bool {
        self.limit_ended
    }

    /// The acts made at the wall being worked, if it is of
    /// `kind`'s class ([`exception_for`]'s and the conformance walks'
    /// projection).
    #[must_use]
    pub fn wall_attempts(&self, kind: WallKind) -> Option<usize> {
        self.wall
            .as_ref()
            .filter(|t| t.class == class_of(kind))
            .map(|t| t.attempts)
    }

    /// Since when the wall being worked shows (this
    /// appearance; a retry moves it), if it is of `kind`'s class.
    #[must_use]
    pub fn wall_since(&self, kind: WallKind) -> Option<Instant> {
        self.wall
            .as_ref()
            .filter(|t| t.class == class_of(kind))
            .map(|t| t.since)
    }

    /// Whether the full context being worked has met the vendor's own word
    /// that a `/compact` could not lower it: the wall's attempts say how often
    /// the loop tried, this says Claude Code answered that it cannot be done.
    #[must_use]
    pub fn compaction_exhausted(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Context && t.exhausted)
    }

    /// What the full context being worked was said to be filled by, by its
    /// own notice this appearance ([`ContextFill::of`]); `None` for a full
    /// context of the conversation's words, or no full context worked.
    #[must_use]
    pub fn context_fill(&self) -> Option<ContextFill> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Context)
            .and_then(|t| t.fill)
    }

    /// THE TYPED KIND OF THE TRANSCRIPT'S NOTICE at this decision's read,
    /// told by the loop (`supervise/wall_confirm.rs`): `typed` is the
    /// `apiError` Claude Code wrote with the notice its transcript ends on
    /// (since 2.1.295), else its `error` word; `None` where no transcript was
    /// read or it ends on no notice. Read by [`Self::notice_typed`]. Never
    /// typed into the session.
    pub fn notice_typed_was(&mut self, typed: Option<String>) {
        self.notice_typed = typed;
    }

    /// The typed kind of the notice the transcript ended on at the last
    /// decision's read ([`Self::notice_typed_was`]).
    #[must_use]
    pub fn notice_typed(&self) -> Option<&str> {
        self.notice_typed.as_deref()
    }

    /// `/compact`'S OWN ANSWER, read by the loop off the screen's last thing
    /// said (`said`, `aterm_phase::phase::last_said_notice`) at a decision's
    /// read where no wall shows, BEFORE the point is folded in
    /// ([`Self::observe`]): after this policy's own `/compact`
    /// ([`RULE_COMPACT`]) on the full context it works, the vendor's word that
    /// there was nothing to compact (`Not enough messages to compact.`, plain
    /// text with no error level, so no wall: 2.1.300's too-few-groups answer)
    /// latches the track as exhausted, as `Compaction failed · …` does
    /// ([`compaction_exhausted_row`]). Anything else, or any other act
    /// awaited, changes nothing.
    pub fn compact_answered(&mut self, said: &str) {
        if self
            .awaiting
            .is_some_and(|a| a.rule == RULE_COMPACT && a.command)
            && compaction_exhausted_row(said)
            && let Some(t) = self.wall.as_mut().filter(|t| t.class == WallClass::Context)
        {
            t.exhausted = true;
        }
    }

    /// Whether the fallback relaunch for the bucket being worked was
    /// refused for good ([`Self::restarted`] with `made` false): its reset,
    /// or the entitlement ladder, is what is waited on.
    #[must_use]
    pub fn fallback_refused(&self) -> bool {
        self.fallback_unmade
    }

    /// The account move made and not answered yet, if any.
    #[must_use]
    pub fn account_move(&self) -> Option<&AccountMove> {
        self.account.as_ref()
    }

    /// Whether an account move asked for at the wall being worked could not
    /// be made ([`Self::restarted`] with `made` false): the wall is waited
    /// out as it would be with no roster.
    #[must_use]
    pub fn account_refused(&self) -> bool {
        self.account_unmade
    }

    /// THE ACCOUNTS' NEWS REACHED THIS SESSION (the host's stamp moved:
    /// `crate::supervise::IdleHost::account_stamp` — the owner signed an
    /// account in and checked it, said `accounts use`, another session proved
    /// or struck one): what the host refused may be possible now, so both
    /// latches end — the wall's ([`Self::account_refused`]) and the one ahead
    /// of a wall — and the next point asks again. A refusal repeated with no
    /// news is still not asked again.
    pub fn account_news(&mut self) {
        self.account_unmade = false;
        self.account_unmade_seed = false;
        self.account_ahead_unmade = None;
    }

    /// Whether the wall's resume is owed after a move this session's host
    /// made at a wall ([`Self::account_resume`]): until then a usage limit
    /// on the screen is the notice of the account it LEFT.
    #[must_use]
    pub fn account_resume_owed(&self) -> bool {
        self.account_resume
    }

    /// Whether a move AHEAD of a wall to the account labelled `to` was asked
    /// and could not be made within the window ending at `now`
    /// ([`Self::restarted`] with `made` false): not asked again for that
    /// account until the window has passed — one refused relaunch an hour at
    /// the most, where a refusal that stands for the launch's life would
    /// otherwise be asked at every turn end.
    fn account_ahead_refused(&self, to: &str, now: Instant) -> bool {
        self.account_ahead_unmade
            .as_ref()
            .is_some_and(|(label, at)| {
                label == to && now.saturating_duration_since(*at) < self.timing.window
            })
    }

    /// How many account moves this session made within the window ending at
    /// `now`, and when the oldest of them leaves it
    /// ([`ACCOUNT_MOVES_PER_WINDOW`]).
    #[must_use]
    pub fn account_moves_of(&self, now: Instant) -> (usize, Option<Instant>) {
        let window = self.timing.window;
        let mut n = 0;
        let mut oldest = None;
        for &t in &self.account_moves {
            if now.saturating_duration_since(t) < window {
                n += 1;
                oldest = oldest.or(Some(later(t, window)));
            }
        }
        (n, oldest)
    }

    /// WHAT THIS POINT SAYS OF THE ACCOUNT MOVE NOT ANSWERED YET, read BEFORE
    /// the point is folded in ([`Self::observe`]: the fold is what marks a
    /// turn of someone else's as asked) and before the host is asked for its
    /// advice — so a host told of a bad answer has struck the account by the
    /// time it advises. Where the answer waited on the act awaited (below) it
    /// is read again right after the fold that judges that act, still before
    /// the host is asked ([`Self::account_answer_awaited`]). Pure; `None`
    /// where the point says nothing.
    ///
    /// * SERVED: a turn asked under the new login (`account_asked`) ran — a
    ///   read saw the agent busy — and ended at an idle point or a question,
    ///   no wall and no person's Esc, and not at a relaunch's start-up
    ///   ([`Self::account_startup`]). A reply that came and went inside one
    ///   read is not counted: the next turn proves the account.
    /// * SERVED, TOO, AFTER A BAD ANSWER TOLD, exactly where the fold ends
    ///   the move ([`Self::observe`]): work, no wall, no unanswered row — the
    ///   limit lifted, the login back. Said, so the host forgets the move it
    ///   kept (an exit of that process after hours of service is relaunched
    ///   under the login it had, not the one before the move) and the ledger
    ///   closes it; never told twice, the move being over once told. Never
    ///   at a relaunch's start-up ([`Self::account_startup`]): its work is
    ///   the new process drawing its conversation, and `served` there proved
    ///   a login that had answered nothing.
    /// * A LOST LOGIN: once a turn has been asked under the new login, or a
    ///   turn of anyone's ran into it at this point, or the move was made
    ///   ahead of a wall — never at a lost login the session left
    ///   ([`Self::login_left`]: the notice is the old process's, repainted by
    ///   the conversation the relaunch resumed, and the host has no
    ///   transcript check for a lost login to tell it from the new process's
    ///   own) — nor while the turn asked is still awaited with no busy read
    ///   since, as at a usage limit (below). READ AS [`Self::account_point`]
    ///   ASKS ABOUT IT: where its row has left the screen with no work since
    ///   (a person's `/status` drawn over it inside the act's take window)
    ///   and the policy only waited at it ([`left_waited_login`]), the
    ///   tracked lost login is the point's still — told, so the host that is
    ///   asked about that wall next has struck the login first.
    /// * A USAGE LIMIT (session, week, model bucket): only after a turn was
    ///   asked. A relaunch RESUMES the conversation, whose last row may be
    ///   the old account's limit; whether the new process repaints it was
    ///   never measured (the design's §9), so a limit on the screen before
    ///   any turn says nothing about the new account. THE SCREEN IS ALL THIS
    ///   READS: that the limit is the new process's own is the host's to
    ///   check (the transcript row's time against the process's start) before
    ///   it strikes anything. NOR WHILE THE TURN ASKED IS STILL AWAITED with
    ///   no busy read since: the continuation is typed AT the notice the old
    ///   account left (the wall's resume), and that notice stands until the
    ///   agent takes it — the point is the act's only once [`Self::observe`]
    ///   has judged it so.
    ///
    /// A spend or credit wall is no account's window: neither.
    #[must_use]
    pub fn account_answer(&self, reading: &TurnEndReading) -> Option<AccountAnswer> {
        self.account.as_ref()?;
        if !reading.at_point(self.stale_input) {
            return None;
        }
        if self.account_told {
            let worked = reading.worked.is_some() || self.limit_went_on(reading, None);
            return (reading.wall.is_none()
                && worked
                && !reading.pending_input
                && !self.account_startup)
                .then_some(AccountAnswer::Served);
        }
        let wall = reading.wall.or_else(|| {
            self.wall
                .as_ref()
                .filter(|t| reading.worked.is_none() && left_waited_login(self, t, reading))
                .map(|_| WallKind::Auth)
        });
        match wall {
            Some(WallKind::Auth)
                if (!self.login_left(reading) || self.ran_under_move(reading))
                    && !(self.awaiting.is_some() && reading.worked.is_none()) =>
            {
                Some(AccountAnswer::Bad(AccountBad::SignedOut))
            }
            Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
                if self.account_asked && !(self.awaiting.is_some() && reading.worked.is_none()) =>
            {
                Some(AccountAnswer::Bad(AccountBad::Wall))
            }
            None if self.account_asked
                && reading.worked.is_some()
                && !reading.interrupted
                && !self.account_startup
                && matches!(reading.phase, Phase::Idle | Phase::Question) =>
            {
                Some(AccountAnswer::Served)
            }
            _ => None,
        }
    }

    /// A LOST LOGIN THE SESSION LEFT: the login wall `reading` shows, read
    /// while this session's own move MADE AT A WALL ([`AccountMove::wall`])
    /// stands with no turn asked under the login it chose
    /// ([`Self::account_asked`]) and no bad answer told. The relaunch RESUMED
    /// a conversation whose last row may be the old process's
    /// `Please run /login` (a move made at a lost login leaves exactly that),
    /// and whether the new process repaints it was never measured — so, as a
    /// usage limit's notice is before a turn is asked, it says nothing about
    /// the new login: nothing is struck ([`Self::account_answer`]), the host
    /// is not asked ([`Self::account_point`]), no wall is tracked
    /// ([`Self::observe`]), and no exception stands. Where the move owes the
    /// wall's resume ([`Self::account_resume`], under `resume_limits`) that
    /// resume is typed ([`wall_action`]) — the request that tests the new
    /// login, a lost login after it being the new process's own. Where
    /// nothing is owed (a chain of moves begun ahead of a wall, no turn asked
    /// under either: [`Self::restarted`]) the point is decided as the plain
    /// turn end it would be with no such notice ([`decide_turn_end`]). NEVER
    /// AFTER A MOVE MADE AHEAD OF A WALL: its old process left the model's
    /// reply on the conversation, no notice, so a lost login on that first
    /// screen is the new process's own — struck at once, and the host asked.
    /// Read after the point is folded in: a turn that ran there is asked by
    /// then.
    #[must_use]
    pub(super) fn login_left(&self, reading: &TurnEndReading) -> bool {
        reading.wall == Some(WallKind::Auth)
            && self.account.as_ref().is_some_and(|m| m.wall)
            && !self.account_asked
            && !self.account_told
    }

    /// A BUCKET THE SESSION LEFT: the model bucket `reading` shows is the
    /// one the fallback's relaunch moved the session off (its notice names
    /// the switch's `from`, [`bucket_model`]), read while no turn has been
    /// asked since that relaunch was made ([`Self::fallback_unasked`]). The
    /// relaunch resumes the conversation (`claude --resume --model …`), and
    /// Claude Code redraws its history, whose last thing said is that notice
    /// (a `/resume` redraws an older limit's notice: the lab's 2.1.300
    /// screens). It is the old process's notice, of a limit on a model the
    /// session no longer runs: as a lost login the session left
    /// ([`Self::login_left`]), no wall is tracked ([`Self::observe`]), the
    /// point is decided as the plain turn end it would be with no such
    /// notice ([`decide_turn_end`]: the continue policy's, as at a relaunch
    /// that redraws none), the host is not asked about it
    /// ([`Self::account_point`]), no exception stands, and the loop keeps no
    /// wall of it for one that follows (`Session::wall_memory_step`). Read
    /// as the wall, the session on the fallback waited out the bucket's reset
    /// (two days for the lab's Sonnet bucket) beside a model that could work,
    /// and the wall written to the ledger held every loop after it there too
    /// (the verifier of 49f07f50e, V21-2). Once a turn is asked, a notice of
    /// that bucket is a wall again: the relaunch did not take, and its reset
    /// is waited out.
    #[must_use]
    pub fn bucket_left(&self, reading: &TurnEndReading) -> bool {
        reading
            .wall
            .is_some_and(|kind| self.left_bucket(kind, &reading.wall_message))
    }

    /// [`Self::bucket_left`] for a wall of `kind` in `message`'s words. Not a
    /// notice the session's transcript says was WRITTEN AFTER THE RELAUNCH
    /// ([`Self::notice_written_at`] past [`Self::fallback_made`]): the
    /// relaunched process's redraw writes no row (a resumed conversation's
    /// rows are its own, appended to), so that notice answered a turn asked
    /// on the fallback — a person's prompt, with no point of the loop's
    /// between it and the redraw, the relaunch not taken or the model chosen
    /// back. Read as the redraw, `continue@v1` went into the standing bucket
    /// at the short turn's back-off (the verifier of fde9e066a, A2).
    #[must_use]
    pub fn left_bucket(&self, kind: WallKind, message: &str) -> bool {
        self.fallback_unasked
            && matches!(kind, WallKind::ModelBucket { .. })
            && self
                .model
                .as_ref()
                .and_then(|m| m.from.as_deref())
                .is_some_and(|from| bucket_model(message).as_deref() == Some(from))
            && !self
                .notice_unix
                .zip(self.fallback_made())
                .is_some_and(|(written, made)| written > made)
    }

    /// WHEN THE FALLBACK'S RELAUNCH WAS MADE, as late as the evidence allows:
    /// the relaunched process's start where the host named it
    /// ([`Self::fallback_started`]), else the run's first ask
    /// ([`Self::fallback_made_at`]). The first ask can come long before the
    /// line: an ask answered `wait:held` before the old process was signalled
    /// types nothing, and that process writes on — a person's prompt
    /// answered by the bucket. Stamped at the ask, its notice, redrawn by the
    /// relaunched process, read as written after the relaunch: the wall, the
    /// bucket's reset waited out two days beside the fallback
    /// (ATERM-KI-1016-1). Nothing the relaunched process writes is older than
    /// its start, and a resumed conversation's older rows keep their times
    /// (measured: Claude Code 2.1.302's `--resume` appended to its
    /// predecessor's file, every row of 2.1.300's as written).
    fn fallback_made(&self) -> Option<i64> {
        self.fallback_started.or(self.fallback_unix)
    }

    /// THE FALLBACK'S RELAUNCH WAS MADE at `unix` (seconds), told by the loop
    /// after [`Self::restarted`] made it: what a notice of the bucket it left
    /// is newer than where a turn asked since met it ([`Self::left_bucket`]).
    /// The loop tells the first ask of the run of tries that made it, no later
    /// than its line was typed: the step that adopts the relaunched process
    /// can come long after (`wait:resume`, then `adopted`). The relaunched
    /// process's start, once the host names it, places it later
    /// ([`Self::fallback_made`]).
    pub fn fallback_made_at(&mut self, unix: i64) {
        self.fallback_unix = Some(unix);
        self.fallback_started = None;
    }

    /// WHEN THE TRANSCRIPT'S NOTICE WAS WRITTEN, at this decision's read, told
    /// by the loop (`supervise/wall_confirm.rs`): `unix` seconds, `None` where
    /// no transcript was read, it ends on no notice, or its row names no time.
    /// Read by [`Self::left_bucket`], and by the loop where it dates a try's
    /// answer ([`Self::date_unread_try`]).
    pub fn notice_written_at(&mut self, unix: Option<i64>) {
        self.notice_unix = unix;
    }

    /// When the transcript's notice the last decision read was written
    /// ([`Self::notice_written_at`]).
    #[must_use]
    pub fn notice_written(&self) -> Option<i64> {
        self.notice_unix
    }

    /// THE FALLBACK'S RELAUNCH WAS MADE AND NO TURN ASKED since
    /// ([`Self::bucket_left`]'s latch): the loop writes where it ends with no
    /// act of its own (`Session::fallback_asked`).
    #[must_use]
    pub fn fallback_unasked(&self) -> bool {
        self.fallback_unasked
    }

    /// A loop before this one made the fallback's relaunch and no turn was
    /// asked after it (the ledger: no `typed` row after the relaunch's, nor
    /// the row of a turn asked that typed none,
    /// [`crate::supervise::approvals::OpenSwitch::asked`]), read with the
    /// switch it carried ([`Self::seed_model`]): the relaunched process's
    /// redraw of the bucket's notice is no wall of this loop's either
    /// ([`Self::bucket_left`]). `made_at`: when that relaunch was made, unix
    /// seconds ([`Self::fallback_made_at`]).
    pub fn seed_fallback_unasked(&mut self, made_at: i64) {
        self.fallback_unasked = self.model.is_some();
        self.fallback_unix = Some(made_at);
        self.fallback_started = None;
    }

    /// A TURN RAN AT THIS POINT under the login this session's own move
    /// chose, read BEFORE the point is folded in (the fold marks it asked,
    /// [`Self::observe`]): work, no unanswered row, and not the relaunch's
    /// own start-up ([`Self::account_startup`]).
    fn ran_under_move(&self, reading: &TurnEndReading) -> bool {
        self.account.is_some()
            && reading.worked.is_some()
            && !reading.pending_input
            && !self.account_startup
    }

    /// THE POINT THE HOST IS ASKED ABOUT ([`AccountPoint`]), or `None` where
    /// its advice could decide nothing — so a host that reads a roster and a
    /// state file to advise is asked only where [`decide_turn_end`] would act
    /// on the answer: a turn's end nothing blocks, in a session with a task,
    /// where a relaunch under another account may be asked at all
    /// ([`account_rotates`]) and the upgrade does not own the point; at a
    /// session or weekly limit or a model bucket; at a lost login only while
    /// the session's own move is unanswered; at a plain turn end only once
    /// it is answered. Pure.
    ///
    /// READ AFTER THE POINT IS FOLDED IN ([`Self::observe`]; the loop's
    /// order, and the Tier-1 walk's): what this memory says of the task, of
    /// the latch and of the move not answered is then THIS point's. The fold
    /// is what opens a finished task again (someone's turn, with work, is the
    /// task theirs) and what ends a latch with its wall; read before it, a
    /// finished session a person gave new work to would not be asked about at
    /// the limit that work ends on, and a session whose refused move's wall
    /// has ended would be passed over for one more point — a whole second
    /// wall.
    ///
    /// Nor at a usage limit while the wall's resume is owed
    /// ([`Self::account_resume`]): that notice is the account's the session
    /// left, and the point's answer is the continuation, whatever is advised.
    /// Nor at a lost login read before any turn was asked under the new
    /// login ([`Self::login_left`]): the old process's notice, which no advice
    /// answers. Nor at a lost login while the turn asked under the move is
    /// still awaited, no busy read since ([`Self::account_answer_awaited`]):
    /// the point waits for that act, and the host is asked once it is
    /// judged, the lost login told first — never about a login nobody has
    /// struck.
    ///
    /// Nor anywhere while a limit reset's continuation is owed
    /// ([`Self::reset_told`]): the point's answer is that continuation, and
    /// an account whose reset just ended is not moved from before it has
    /// answered.
    ///
    /// A usage limit whose notice LEFT the screen with no work since (Esc on
    /// the vendor's countdown) is that limit's point still, decided as if its
    /// notice showed ([`still_shown`]): asked about as the wall, never as a
    /// turn's end. So is a lost login the policy only WAITED at, no `/login`
    /// typed, whose row left the screen (a person's `/status` or `/usage`,
    /// Esc out of `/login`) while the session's own move stands
    /// ([`left_waited_login`]): its wait ends at the host's word.
    ///
    /// THE LOGIN LIFECYCLE asks too, under its own switch ([`login_follows`],
    /// `[harness] logins`, whatever `accounts` says): at a lost login with no
    /// move of the rotation's behind it in a session on a renewable login
    /// ([`TurnEndReading::login_label`]) — as [`AccountPoint::LoginEnded`],
    /// the login's own end — and at a plain turn end; never at a usage
    /// limit, which is the rotation's alone.
    #[must_use]
    pub fn account_point(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
    ) -> Option<AccountPoint> {
        let rotates = account_rotates(self, reading, cfg);
        let follows = login_follows(self, reading, cfg);
        if !reading.at_point(self.stale_input)
            || reading.taskless
            || reading.upgrading
            || self.task_done()
            || self.reset_resume.is_some()
            || !(rotates || follows)
        {
            return None;
        }
        // A bucket the session left is no wall ([`Self::bucket_left`]).
        let shown = reading.wall.filter(|_| !self.bucket_left(reading));
        let wall = shown.or_else(|| {
            let t = self.wall.as_ref()?;
            left_named(t, reading)
                .map(|n| n.kind)
                .or_else(|| left_waited_login(self, t, reading).then_some(WallKind::Auth))
        });
        match wall {
            Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
                if self.account_resume =>
            {
                None
            }
            Some(
                kind @ (WallKind::UsageSession
                | WallKind::UsageWeekly
                | WallKind::ModelBucket { .. }),
            ) if rotates => Some(AccountPoint::Wall { kind }),
            Some(WallKind::Auth)
                if self.login_left(reading) || self.account_answer_awaited(reading) =>
            {
                None
            }
            Some(kind @ WallKind::Auth) if self.account.is_some() && rotates => {
                Some(AccountPoint::Wall { kind })
            }
            Some(WallKind::Auth) if reading.login_label.is_some() && follows => {
                Some(AccountPoint::LoginEnded)
            }
            None if self.account.is_none()
                && matches!(reading.phase, Phase::Idle | Phase::Question) =>
            {
                Some(AccountPoint::TurnEnd)
            }
            _ => None,
        }
    }

    /// WHETHER THIS POINT MAY TAKE THE ACCOUNTS PROBE ([`RULE_USAGE_PROBE`];
    /// `docs/SPEC-claude-accounts-probe-2026-10-01.md`, P6, P8): the ONE
    /// pure gate, so each condition can be flipped alone. `true` only when
    /// ALL hold:
    ///
    /// * the reading's advice is [`AccountAdvice::Probe`] — the roster says
    ///   the read is due — at a plain turn end the host is asked about
    ///   ([`Self::account_point`] is [`AccountPoint::TurnEnd`]: a task, no
    ///   upgrade, a relaunch under another account askable at all, the
    ///   session's own last move answered, no limit reset's continuation
    ///   owed);
    /// * the phase is `Idle`, read by a reader whose word is evidence, with
    ///   no survey card up. NOT a question, although the host is asked
    ///   there: nothing is typed over what the worker asked a person;
    /// * the composer is drawn and holds nothing typed (empty, or the
    ///   worker's own dim suggestion), and no submitted row waits unanswered
    ///   — not even one this policy judged not taken;
    /// * no wall is on the screen and none is being worked (no wall track: a
    ///   limit waited out off the screen, a lost login);
    /// * nobody stopped the turn with Esc, and the worker asked nothing of a
    ///   person ([`TurnEndReading::asked`]);
    /// * nothing of this policy's is awaited or owed: no act awaited, no
    ///   command's continuation, no wall's resume owed after a move;
    /// * no person typed within `cfg.human_grace_s`.
    ///
    /// It is the loop's gate and decides nothing here: [`decide_turn_end`]
    /// reads the advice past the probe whatever this says, and a probe the
    /// loop makes moves none of this memory ([`Self::acted`] is not called
    /// for it). What only the loop knows is the loop's to add — that the
    /// turn was read live, the renderer, the fences, the host's claim.
    /// PURE: `now` is taken so a rule about this memory's own clocks can be
    /// stated here without a new signature, and none is today.
    #[must_use]
    pub fn probe_point(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> bool {
        self.probe_gate(reading, cfg, now).is_none()
    }

    /// [`Self::probe_point`], BY THE GATE THAT SHUT: `None` where the point
    /// may take the probe, else the FIRST of its conditions that does not
    /// hold ([`ProbeGate`]), for the loop's one line about a due probe it
    /// did not run. The conditions are a conjunction, so the order decides
    /// only which word is said, and it is the order a reader wants: the
    /// narrow condition before the wide one that would hide it — the phase,
    /// a wall and the composer before "no turn end the host is asked about",
    /// which is false at each of them too. PURE, as [`Self::probe_point`]
    /// is.
    #[must_use]
    pub fn probe_gate(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> Option<ProbeGate> {
        let _ = now;
        let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
        let owed = self.awaiting.is_some() || self.after.is_some() || self.account_resume;
        let checks = [
            (
                !matches!(reading.account, Some(AccountAdvice::Probe { .. })),
                ProbeGate::NotDue,
            ),
            (reading.phase != Phase::Idle, ProbeGate::NotIdle),
            (!reading.authoritative, ProbeGate::NotAuthoritative),
            (reading.survey, ProbeGate::Survey),
            (
                reading.wall.is_some() || self.wall.is_some(),
                ProbeGate::Wall,
            ),
            (
                !matches!(reading.composer, Composer::Empty | Composer::Placeholder(_)),
                ProbeGate::Composer,
            ),
            (reading.pending_input, ProbeGate::PendingInput),
            (reading.interrupted, ProbeGate::Interrupted),
            (reading.asked().is_some(), ProbeGate::Question),
            (
                self.account_point(reading, cfg) != Some(AccountPoint::TurnEnd),
                ProbeGate::NotTurnEnd,
            ),
            (owed, ProbeGate::ActOwed),
            (
                reading.person.is_some_and(|ago| ago < grace),
                ProbeGate::Person,
            ),
        ];
        checks
            .into_iter()
            .find_map(|(shut, gate)| shut.then_some(gate))
    }

    /// The loop told its host what answered the move ([`Self::account_answer`]):
    /// served, the move is over; answered by a wall or a lost login, it
    /// stands — told, so that bad answer is never told again — until the
    /// worker works past it, which is told as served there and ends it (as
    /// [`Self::observe`] ends it where nothing told), or another move is
    /// made.
    pub fn account_told(&mut self, answer: AccountAnswer) {
        match answer {
            AccountAnswer::Served => self.account_over(),
            AccountAnswer::Bad(_) => self.account_told = true,
        }
    }

    /// The move made is behind the session.
    fn account_over(&mut self) {
        self.account = None;
        self.account_asked = false;
        self.account_told = false;
        self.account_startup = false;
        self.account_resume = false;
    }

    /// The wall's words a decision says: the screen's, else — the notice
    /// gone from it — the track's, as its notice last read
    /// ([`WallTrack::said`]).
    fn wall_words<'a>(&'a self, reading: &'a TurnEndReading) -> &'a str {
        match &self.wall {
            Some(t) if reading.wall.is_none() => &t.said,
            _ => &reading.wall_message,
        }
    }

    /// The lost login's track, if one stands — the wall seen and the worker
    /// not working since — and how many `/login`s it has had (the
    /// conformance walk's projection).
    #[must_use]
    pub fn login_track(&self) -> Option<usize> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Auth)
            .map(|t| t.attempts)
    }

    /// USAGE CREDITS CARRYING THE SESSION, folded in at a point (a busy or a
    /// box reading folds nothing, [`Self::observe`]): the episode the credits
    /// hold waits on ([`credits_hold`]).
    ///
    /// * A notice — drawn, or the loop's memory of one this process drew
    ///   ([`CreditsCarry::kept`]) — opens it, or keeps it, with the reset the
    ///   reading names ([`CreditsCarry::reset_at`]; none for the lasting `Now
    ///   using usage credits` alone) and the model it names.
    /// * A reading with a composer drawn and no notice keeps the episode
    ///   until its reset and grace, or its bound where no reset was placed
    ///   ([`Self::credits_bound`]): absence alone says nothing of subscription
    ///   restoration. A model's notice is drawn once a process, so its
    ///   episode stands while that process does ([`CreditsEpisode::model`]).
    ///   A reading with no composer, which cannot draw the notice, says
    ///   nothing of them.
    /// * ITS END PASSED, A TURN SERVED PAST IT, the notice still drawn — its
    ///   reset waited out, or with none named its bound
    ///   ([`Self::credits_bound`]): Claude Code draws the notice from the
    ///   newest answer, so that turn's answer came on credits too — another
    ///   window carries them past (the week's under the session's), and no
    ///   reset is known. The episode starts over with none
    ///   ([`CreditsEpisode::passed`]), from that point, its owner told once
    ///   more where a reset had been named. A notice drawn before the end and
    ///   left at rest holds nothing past it: only a turn served since says
    ///   anything of now.
    fn fold_credits(&mut self, reading: &TurnEndReading, now: Instant) {
        let grace = self.timing.grace_for(reading.program);
        let bound = self.credits_bound();
        let Some(read) = &reading.credits else {
            let stands = self.credits.as_ref().is_some_and(|c| {
                c.model.is_some()
                    || match c.reset {
                        Some(at) => now < later(at, grace),
                        None => c
                            .since
                            .is_some_and(|since| now.saturating_duration_since(since) < bound),
                    }
            });
            if reading.composer != Composer::Absent && !stands {
                self.credits = None;
            }
            return;
        };
        let episode = self.credits.get_or_insert_with(CreditsEpisode::default);
        if episode.since.is_none() {
            episode.since = Some(read.since.unwrap_or(now));
        }
        if read.model.is_some() {
            episode.model.clone_from(&read.model);
        }
        let ended = match episode.reset {
            Some(at) => now >= later(at, grace),
            None => {
                episode.model.is_none()
                    && episode
                        .since
                        .is_some_and(|since| now >= later(since, bound))
            }
        };
        if ended && !read.kept && reading.worked.is_some() {
            *episode = CreditsEpisode {
                reset: None,
                since: Some(now),
                model: read.model.clone(),
                passed: episode.reset.or(episode.passed),
                told: episode.reset.is_none() && episode.told,
            };
        }
        if let Some(at) = read.reset_at
            && episode
                .passed
                .is_none_or(|passed| at > later(passed, SAME_CREDITS_RESET))
        {
            episode.reset = Some(at);
        }
    }

    /// The bound of the credits hold with no reset named
    /// ([`TurnEndTiming::credits_bound`]).
    fn credits_bound(&self) -> Duration {
        self.timing.credits_bound
    }

    /// THE CREDITS HOLD'S ESCALATION WAS DECIDED ([`CREDITS_HOLD_TOLD`],
    /// [`CREDITS_HOLD_MODEL_TOLD`]): the loop's word, once it decided it, so
    /// the episode tells its owner once ([`credits_hold`]). A new episode —
    /// a notice back after the hold ended, or a reset waited out under it
    /// ([`Self::fold_credits`]) — tells again.
    pub fn credits_told(&mut self) {
        self.credits
            .get_or_insert_with(CreditsEpisode::default)
            .told = true;
    }

    /// WHETHER THE CREDITS HOLD STANDS at `now` under `cfg`, for a read
    /// that says credits carry the session (`read`, `None` for one that does
    /// not): what holds the HOST's own turns too (`Session::host_steps_here`'s
    /// `Guard::Credits`), as [`credits_hold`] holds the policy's. `true`
    /// while the key is off and the episode's end is ahead
    /// ([`Self::credits_held`]).
    #[must_use]
    pub fn credits_hold_stands(
        &self,
        read: Option<&CreditsCarry>,
        program: Program,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> bool {
        !cfg.usage_credits && self.credits_held(read, program, now).is_some()
    }

    /// WHAT THE CREDITS HOLD HOLDS at `now`, for a read that says credits
    /// carry the session (`read`) or none: the reset they wait for
    /// ([`Self::credits_reset`]), the model whose requests they carry, and
    /// when it lifts — that reset and the grace; with none named, the bound
    /// past the episode's start ([`Self::credits_bound`]); never by itself
    /// for a model's with none, which the session leaving the model ends
    /// ([`CreditsEpisode::model`]). A read that names no credits holds only
    /// by an episode that stands without its notice ([`Self::fold_credits`]).
    /// `None`: nothing held.
    fn credits_held(
        &self,
        read: Option<&CreditsCarry>,
        program: Program,
        now: Instant,
    ) -> Option<CreditsHeld> {
        let episode = self.credits.as_ref();
        let reset = match read {
            Some(read) => self.credits_reset(read),
            None => episode?.reset,
        };
        let model = episode
            .and_then(|c| c.model.clone())
            .or_else(|| read.and_then(|r| r.model.clone()));
        if read.is_none()
            && reset.is_none()
            && model.is_none()
            && episode.and_then(|c| c.since).is_none()
        {
            return None;
        }
        let lifts = match (reset, &model) {
            (Some(at), _) => Some(later(at, self.timing.grace_for(program))),
            (None, Some(_)) => None,
            (None, None) => {
                let since = episode
                    .and_then(|c| c.since)
                    .or_else(|| read.and_then(|r| r.since))
                    .unwrap_or(now);
                Some(later(since, self.credits_bound()))
            }
        };
        if lifts.is_some_and(|at| now >= at) {
            return None;
        }
        Some(CreditsHeld {
            reset,
            model,
            lifts,
            told: episode.is_some_and(|c| c.told),
        })
    }

    /// The reset usage credits wait for, a read saying they carry the
    /// session: the episode's ([`Self::fold_credits`]), else the read's,
    /// unless the episode waited that one out already
    /// ([`CreditsEpisode::passed`]). `None`: none named.
    fn credits_reset(&self, read: &CreditsCarry) -> Option<Instant> {
        match &self.credits {
            Some(c) => c.reset.or(read
                .reset_at
                .filter(|&at| c.passed.is_none_or(|p| at > later(p, SAME_CREDITS_RESET)))),
            None => read.reset_at,
        }
    }

    /// Folds what a point shows into the state, before [`decide_turn_end`]:
    ///
    /// * the last act's yield: under `min_work` lengthens the short streak,
    ///   more ends it; a host-typed turn ([`Self::host_typed`]) never
    ///   lengthens it and ends it with real work;
    /// * someone else's turn in between (`worked` with nothing awaited);
    /// * the first point seen (work unknown: short);
    /// * the wall on screen (a new kind opens a track; the same kind after a
    ///   retry is its next appearance);
    /// * usage credits carrying the session, and the reset they wait for
    ///   ([`Self::fold_credits`]).
    ///
    /// Reading the same point again changes nothing. After a text act with no
    /// busy read since, the point is not the act's yet (Claude Code draws a
    /// submitted message before its spinner) until
    /// [`TurnEndTiming::take_within`] from the first such point; past that it
    /// is judged a short yield with no work, and an unanswered `❯` row is
    /// judged not taken and becomes a point the next act may go at. A slash
    /// command's point needs no busy read. Anyone else's unanswered row is
    /// never a point.
    ///
    /// A WALL IS TRACKED WHEREVER IT IS READ, a point or not
    /// ([`Self::track_wall`]): a reading that is no point yet — the act's take
    /// window still open, anyone's unanswered row — leaves the act's own
    /// bookkeeping (taken or not, its yield, the streak) to the point that
    /// judges it, but the wall it shows is folded in at once. Folded only at
    /// that point, a limit that answered the act and whose notice left the
    /// screen inside the window (Esc on the vendor's countdown) was never
    /// seen: the window's end read the act's short turn, and the continue
    /// policy typed into the standing limit at its back-off (round 15's run,
    /// 2026-10-14). The reading's wall ANSWERED the act awaited
    /// ([`Awaited::met`]), and the point that judges it keeps that: a retry
    /// of ours the wall answered is the wall's next appearance, never taken
    /// because the notice left before the window's end (round 16's review).
    /// Nor is a wall read there one a point DECIDED ([`WallTrack::pointed`]):
    /// a lost login no point saw is decided as the wall it left, never held
    /// as one the owner was told of. A BUSY or BOX reading is no point and folds nothing: the
    /// reader hands a wall under a live turn only for the memory banner,
    /// which [`decide_turn_end`] acts on by the reading itself, and the turn
    /// running ends whatever track stands by its work at its own point.
    pub fn observe(&mut self, reading: &TurnEndReading, now: Instant) {
        // A wall first read here ARRIVED where this loop could see it: after
        // a point it judged, or at the end of a turn it read running — not at
        // its first point, where the wall may have stood for hours
        // ([`LoginBooks::prompt_at`]).
        let arrived = self.points > 0 || reading.worked.is_some();
        // THE TURN A LOOP BEFORE THIS ONE READ RUNNING, a person's hand in
        // it, ends at this first point ([`Self::seed_ran_turn`]): its work
        // is the time since it was read running.
        let ran_turn = self.ran_turn_reading(reading, now);
        let reading = ran_turn.as_ref().unwrap_or(reading);
        // `/login`'s success answered stays answered only while it stands.
        if !matches!(
            reading.login_screen,
            Some(aterm_phase::LoginScreen::Success { .. })
        ) {
            self.login_confirmed = false;
        }
        self.limit_ended = false;
        self.point_moved = false;
        self.left_judged = false;
        // The policy LOOKED at this screen: a limit a cancel left that a
        // later screen draws, and this one did not, answered a turn asked
        // since ([`Self::left_answer`]).
        self.looked = Some((
            now,
            left_key(reading.since_limit.as_ref(), reading.auto_continue),
        ));
        // A reading in another mode than Land has left the Land episode: its
        // memory goes, and a later Land is a new episode, its instruction
        // typed again.
        if reading.autopilot != Autopilot::Land {
            self.land = LandEpisode::default();
        }
        // A PERSON'S ESC is timed from the first read that shows it: kept
        // while the same point is read again, new where a turn was read run
        // since (its work), gone where no person's Esc stands.
        // (The harness presses no Esc of its own: every interrupt is a person's.)
        let persons_esc =
            reading.interrupted && !matches!(reading.phase, Phase::Busy | Phase::Prompt);
        // The point a loop before this one judged, read again, times an Esc
        // its record says stood there from that point ([`Self::seed_esc`]).
        let seeded_esc = self.point_seed.filter(|_| self.esc_seed);
        self.esc_at = match (persons_esc, reading.worked) {
            (false, _) => None,
            (true, Some(_)) => Some(now),
            (true, None) => Some(self.esc_at.or(seeded_esc).unwrap_or(now)),
        };
        if matches!(reading.phase, Phase::Busy | Phase::Prompt) {
            return;
        }
        self.fold_credits(reading, now);
        // A CONVERSATION WITH NO TASK OWES NO CONTINUATION: a limit's wait
        // ends where the screen shows no wall and the conversation holds no
        // task ([`TurnEndReading::taskless`]: the launch card with nothing
        // said but the `/clear` that drew it, or the host's record of whose
        // turns the conversation holds). Nothing is typed into it
        // ([`decide_turn_end`]), so nothing waits on the reset. Measured on
        // 2.1.300 (W4 of ATERM-KI-1015-1): a `/clear` at an armed limit was
        // never a point (`❯ /clear` read as a prompt not answered yet), the
        // limit of the conversation it cleared stayed waited on, and the
        // ledger's `wall-wait@v1` stood open for good — for every loop that
        // followed. Ended before the point is judged, so the cleared screen
        // ends it. A turn asked there later meets the limit again, if it
        // still stands: a wall of its own.
        if reading.taskless
            && reading.wall.is_none()
            && self
                .wall
                .as_ref()
                .is_some_and(|t| matches!(t.class, WallClass::Usage | WallClass::Model))
        {
            self.wall = None;
            self.fallback_unmade = false;
            // ...and the limit's tries with it ([`Self::limit_ended`]).
            self.limit_ended = true;
        }
        // THE POINT THE WORKER'S FINISHED MADE OVER ITS BACKGROUND WORK, that
        // work over with no turn of anyone's since — the agent's own wake at
        // its end included ([`Self::finished_over_background`],
        // [`Self::live_turn`]): the screen it leaves is that point still, its
        // task done — read as a new turn of the work's length, it was someone
        // else's, and the done was published again.
        if std::mem::take(&mut self.background_done) {
            self.woken = false;
            return;
        }
        // A LOST LOGIN THE SESSION LEFT ([`Self::login_left`], no turn run
        // here either) is the old process's notice: folded in as no wall, so
        // no track holds the point and no act typed at it counts as one of
        // the wall's.
        let left_login = self.login_left(reading) && !self.ran_under_move(reading);
        // A BUCKET THE SESSION LEFT ([`Self::bucket_left`]) likewise; any
        // other point since the fallback's relaunch says a turn was asked,
        // or the conversation moved on.
        let left_bucket = self.bucket_left(reading);
        self.fallback_unasked = left_bucket;
        let unwalled;
        let reading = if left_login || left_bucket {
            unwalled = TurnEndReading {
                wall: None,
                wall_message: String::new(),
                ..reading.clone()
            };
            &unwalled
        } else {
            reading
        };
        // A TURN NO READ SAW BUSY, ended where a usage limit this policy
        // waits on has left the screen: the vendor's own continuation of
        // THIS limit (`Usage limit reset · continuing automatically`,
        // [`aterm_phase::AutoContinue::Fired`], under the notice that names
        // this limit's reset: [`NamedReset::continued_under`]), or a turn
        // SERVED since THIS limit's notice — a person's, the limit lifted
        // before its reset (the ruling of 2026-10-14, a person at the tab who
        // works drops the wait): its prompt and the model's reply drawn under
        // the notice that names this limit's reset
        // ([`NamedReset::served_under`]), or, with no limit's notice on the
        // screen to order it by, the model's reply written after this
        // limit's own notice row in the session's transcript
        // ([`TurnEndReading::served_in_transcript`]) — its reply come and
        // gone between two reads. Each is TIED TO THIS LIMIT by the reset its
        // notice names: a `/resume`d conversation redraws its own history,
        // an older limit with the vendor's continuation of it or turns served
        // under it, and read as this one's the wait ended and the continue
        // policy typed into the standing limit (W1 of ATERM-KI-1015-1, the
        // lab on 2.1.300: the stub's 429, 51 minutes before the reset) — and
        // a notice the conversation the tab holds now has no row of
        // ([`TurnEndReading::notice_elsewhere`]) ties nothing, though it
        // names this limit's clock (the residual review's R2: a reset's clock
        // names no date). Folded in as a turn of someone else's that ended
        // here, its work unmeasured: the limit is over, as any work ends it,
        // and the point is the continue policy's, its back-off counted from
        // here. News once: the track it ends is what makes it so. Not while a
        // prompt is unanswered (its turn may meet the limit again): the
        // unanswered input's arm below returns before the wall is looked at
        // whenever work is read, this fold's included.
        let fired;
        let reading = if self.limit_went_on(reading, Some(now)) {
            fired = TurnEndReading {
                worked: Some(Duration::ZERO),
                ..reading.clone()
            };
            &fired
        } else {
            reading
        };
        // A LOST LOGIN THAT ANSWERED A TURN NO READ SAW RUN, OVER A TASK DONE
        // (the owner's incident of 2026-10-19 on v0.106.0): Claude Code draws
        // its synthetic `authentication_failed` row some 70 ms after the
        // prompt, so a person's prompt to a finished task is never read busy;
        // read as the same point the done stood at, the wall was never
        // decided — no `/login` typed, nobody told, two prompts swallowed. A
        // lost login is no word of the done task's: somebody's turn met it.
        // Folded in as a turn of someone else's that ended on the wall, its
        // work unmeasured: the task reopens (a person's hand in it makes it
        // their exchange, which the wall's rule delivers again), and the
        // wall's own rule decides the point. Only where no lost login's track
        // stands already: one read again is that wall, not a new turn.
        let walled;
        let reading = if self.task_done()
            && reading.wall == Some(WallKind::Auth)
            && reading.worked.is_none()
            && self.awaiting.is_none()
            && !self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Auth)
        {
            walled = TurnEndReading {
                worked: Some(Duration::ZERO),
                ..reading.clone()
            };
            &walled
        } else {
            reading
        };
        // THE ACT AWAITED, TAKEN AND ANSWERED BEFORE ANY READ SAW ITS TURN RUN
        // ([`Self::answered_unseen`]): folded in as the turn a read saw run,
        // its work what the signs measure — judged here, not at its take
        // window's end as a turn that answered nothing.
        let unseen;
        let reading = match self.answered_unseen(reading, now) {
            Some(work) => {
                unseen = TurnEndReading {
                    worked: Some(work),
                    ..reading.clone()
                };
                &unseen
            }
            None => reading,
        };
        if !reading.pending_input || reading.worked.is_some() {
            self.stale_input = false;
        }
        let awaited_command = self.awaiting.is_some_and(|a| a.command);
        // Real work before the wall showed: the worker's, after an act of
        // ours or a message of anyone's. Not when the act was a COMMAND
        // (`/compact`): the spinner of its own run is no work of the
        // worker's (a compaction that ran six minutes and ended on the same
        // wall started the ladder over, 40 `/compact` in four hours).
        let worked_to_wall =
            !awaited_command && reading.worked.is_some_and(|w| w >= self.timing.progress);
        if !self.point_judged(reading, now) {
            // No point yet, and no act judged: the wall it shows is tracked
            // all the same, taking up a latch a loop before this one left as
            // the first wall this state sees does. An act awaited is ANSWERED
            // by it ([`Awaited::met`]): the first wall read since a retry is
            // the retry's next appearance, as at its point.
            if let Some(kind) = reading.wall {
                let unmade_seeded = std::mem::take(&mut self.account_unmade_seed);
                let retried = self.awaiting.as_mut().is_some_and(|a| {
                    let first = !std::mem::replace(&mut a.met, true);
                    first && a.wall_retry
                });
                let at_point = false;
                self.track_wall(
                    reading,
                    kind,
                    retried,
                    worked_to_wall,
                    unmade_seeded,
                    at_point,
                    Arrival {
                        hand: false,
                        seen: arrived,
                    },
                    now,
                );
            }
            return;
        }
        // A POINT: someone else's turn read live since the last one has
        // ended here ([`Self::other_turn`]), and a landing seeded from a loop
        // before this one is this point's to take up, or to drop
        // ([`Self::seed_landed`]).
        self.other_turn = false;
        let seeded = self.landed_seed.take();
        let landing_words = self.landed_words.take();
        // The seeded end's first time, for the note this point writes where
        // it restates that end ([`Self::restated_noted_ms`]): a landing's seed
        // is restated where it is taken up (below), and whether it was is
        // read by the face it leaves.
        let noted = self.seed_noted_ms.take();
        self.restating = noted
            .filter(|_| seeded.is_some())
            .map(|at| (TaskFace::Landed, at));
        // A DONE A LOOP BEFORE THIS ONE READ, restated at this loop's start
        // ([`Self::seed_done`]): kept where the screen's last words are still
        // that done's own (their mark), withdrawn where they are another
        // reply's. Either way this point decides the face as any first point
        // does; a withdrawn one's done is a new done ([`Self::face_withdrawn`]).
        if let Some(words) = self.done_seed.take() {
            self.done_withdrawn =
                words.is_some_and(|w| said_mark(reading.said_tail.as_deref()) != Some(w));
            if !self.done_withdrawn {
                self.restating = noted.map(|at| (TaskFace::Done, at));
            }
        }
        let min = self.timing.min_work;
        let short_before = self.short;
        let point_at_before = self.point_at;
        // THE POINT A LOOP BEFORE THIS ONE JUDGED LAST, read again here
        // ([`Self::seed_point`]): this loop's first judged point, no busy
        // read since it attached and no act of its own awaited.
        let seeded_point = self.point_seed.take();
        self.esc_seed = false;
        let seeded_yield = self.yield_seed.take();
        let same_point =
            seeded_point.is_some() && reading.worked.is_none() && self.awaiting.is_none();
        let mut checked = std::mem::take(&mut self.checking);
        let mut landing = std::mem::take(&mut self.landing);
        self.seed_answered = false;
        let met = self.awaiting.is_some_and(|a| a.met);
        let host = self.awaiting.and_then(|a| a.host);
        // THE ACT'S FIRST POINT AFTER OFF STOOD, ITS TURN NEVER SEEN RUN
        // ([`Awaited::off_stood`]).
        let unseen_after_off =
            self.awaiting.is_some_and(|a| a.off_stood) && reading.worked.is_none();
        let wall_retry = self.awaiting.is_some_and(|a| a.wall_retry);
        let awaited = self.awaiting.take().map(|a| a.rule);
        // A WALL CUT THE DONE CHECK OR A LAND ACT: kept for the wall's retry
        // that delivers it again ([`Self::cut_check`]). An act's point or
        // someone's turn sets it afresh; the same point read again keeps it.
        // The check is the check whoever typed it: the restart's carry-on
        // that asks it ([`HostTurn::Check`]) is cut as this policy's is.
        // THE CHECK'S TURN'S WORK IN ALL ([`Self::check_work`]): what a wall
        // cut of it adds to its retry's answer.
        let check_worked = reading.worked.map(|d| self.check_work.saturating_add(d));
        if awaited.is_some() || reading.worked.is_some() {
            // THE VENDOR'S OWN CONTINUATION of the limit that cut it (Claude
            // Code's `Usage limit reset · continuing automatically`, no `❯`
            // row) delivers it again, as the wall's retry does: the turn
            // that ran here, no act of this policy's awaited, is its answer
            // (ATERM-KI-1016-1: read as a turn nobody asked, its done reply
            // was a first one — the check typed a second time, under Land a
            // nudge).
            if awaited.is_none()
                && reading.wall.is_none()
                && reading.auto_continue == Some(aterm_phase::AutoContinue::Fired)
            {
                checked |= self.cut_check;
                landing |= self.cut_land;
            }
            let cut = reading.wall.is_some();
            self.cut_check = cut && (checked || host == Some(HostTurn::Check));
            self.cut_land = cut && landing;
            self.cut_reply = cut;
            self.check_work = if self.cut_check {
                check_worked.unwrap_or(self.check_work)
            } else {
                Duration::ZERO
            };
        }
        let done = says_done(reading.said_tail.as_deref());
        // A DONE CHECK A LOOP BEFORE THIS ONE ASKED, its answer read here
        // with no act of this loop's awaited ([`Self::seed_done_check`]): the
        // worker's own word ends the task, as at the check's yield below.
        let done_outright = done
            && reading.said_tail.as_deref().is_some_and(says_done_outright)
            && reading.check_record != Some(false);
        // …and what ends the task at the yield of a check or a land act
        // awaited: at the check, the worker's own word or an idle report
        // alone as a short turn's whole answer ([`check_answer_ends`]); at a
        // land act, the worker's own word alone (Land's instruction asks for
        // the token, and its nudges are bounded).
        let check_ends = |checked: bool, landing: bool| {
            if landing {
                done_outright
            } else {
                checked && done && check_answer_ends(reading, check_worked, min)
            }
        };
        // Not at a standing wall: a remembered limit whose notice the screen
        // draws no more is none ([`TurnEndReading::wall_drawn`]).
        let finished = says_finished(reading.said_tail.as_deref())
            && !reading.wall_drawn()
            && !reading.pending_input;
        let streak = |short: u32, d: Duration, done: bool| {
            if d < min || done {
                short.saturating_add(1)
            } else {
                0
            }
        };
        // A PERSON'S HAND opened the turn ending here ([`Self::hand`]), on a
        // task this policy was not carrying (or whose worker already said it
        // is done), and they did not stop it: the turn is theirs. Read before
        // the arms move the point.
        let carried = self.carrying && self.done == 0;
        // A TURN THAT RAN WHILE A LOST LOGIN A PERSON'S PROMPT MET STOOD is
        // that prompt delivered again (Claude Code sends it once `Login
        // successful` closes): theirs, whoever pressed the success Enter —
        // their key came before the `/login`, so by it alone the turn read
        // as nobody's, and the continue policy typed `keep going` after the
        // answer to their question ([`LoginBooks::hand`]).
        let redelivers = reading.worked.is_some()
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Auth && t.login.hand);
        let theirs = !carried && !reading.interrupted && (self.hand(reading, now) || redelivers);
        let ok_to_leave = reading.wall.is_none() && !reading.pending_input;
        let (points_before, done_before) = (self.points, self.done);
        // THE YIELD OF THE ACT A LOOP BEFORE THIS ONE TYPED LAST
        // ([`Self::seed_yield`]) WAS STOPPED BY AN ESC (a person's: the
        // harness presses none): the carry ends, as at that act's own arm
        // below for the loop that typed it, so a person's short turn after it
        // is theirs —
        // whether a read of this loop's saw the answer run or this point is
        // the first it sees (which is backed off, never left to anyone).
        // Kept, a successor attached while the act's answer ran, or after a
        // loop that stopped before it, typed `keep going` into the person's
        // exchange the loop before left to them (the review's KR-P6-ESC).
        let yield_stopped = seeded_yield.is_some() && reading.interrupted;
        let first_point = self.point_at.is_none();
        match (awaited, reading.worked) {
            // The harness's own turn: a short answer leaves the streak and
            // its back-off as they were; real work ends the streak (a
            // carry-on's answer is the worker's work resumed). A carry-on's
            // answer that says done, after real work or in seconds (a
            // relaunched worker whose task was already finished), is a done
            // report (a first one, or a done task still done), leaving the
            // streak as it was (N1); a notice's never is: it asked for the
            // stopping point it reads.
            // The restart's carry-on that is the done check: its answer is
            // the check's yield ([`HostTurn::Check`]). The worker's own word
            // ends the task; anything else is the work going on. As the
            // harness's own turn, a short answer moves no streak, and real
            // work ends it.
            (Some(RULE_HOST_TURN), worked) if host == Some(HostTurn::Check) => {
                let d = worked.unwrap_or_default();
                let own_word = check_ends(true, false);
                if d >= min {
                    self.followed = Some(RULE_HOST_TURN);
                    self.point_worked = d;
                    self.new_point(now);
                    self.short = 0;
                } else if self.point_at.is_none() {
                    self.new_point(now);
                }
                self.exchange = false;
                self.carrying = true;
                if ok_to_leave {
                    self.done = if own_word { 2 } else { 0 };
                }
            }
            (Some(RULE_HOST_TURN), worked) => {
                let d = worked.unwrap_or_default();
                let carry_on = host == Some(HostTurn::CarryOn);
                if d >= min && (!done || carry_on) {
                    self.followed = Some(RULE_HOST_TURN);
                    self.point_worked = d;
                    self.new_point(now);
                    self.exchange = false;
                    self.carrying = true;
                    if !done {
                        self.short = 0;
                        self.done = 0;
                    } else if ok_to_leave {
                        self.done = self.done.max(1);
                    }
                } else if carry_on && done && ok_to_leave {
                    self.done = self.done.max(1);
                    if self.point_at.is_none() {
                        self.new_point(now);
                    }
                } else if self.point_at.is_none() {
                    self.new_point(now);
                }
            }
            (Some(rule), worked) => {
                let d = worked.unwrap_or_default();
                // A TURN NO READ SAW RUN, FIRST SEEN AFTER OFF STOOD, leaves
                // nothing to anyone: the person drove the session meanwhile,
                // so nothing says it is the act's answer. A person's exchange
                // the act was delivering again is over, and the point is
                // backed off, then acted on, as any first point is.
                if unseen_after_off && self.exchange {
                    self.exchange = false;
                    self.short = self.short.max(1);
                }
                // A wall's act that delivered a person's own exchange again
                // (a retry under their message) leaves it theirs while the
                // answer's work IN ALL is within `EXCHANGE_WORK` — a wall
                // splits a job, never makes it an exchange; a continuation, a
                // command or that much work makes the task the policy's.
                self.exchange_work = self.exchange_work.saturating_add(d);
                // `/login` does no work of the task's: the exchange its wall
                // swallowed stays the person's through it.
                self.exchange &= self.exchange_work < EXCHANGE_WORK
                    && !is_continue_rule(rule)
                    && (!awaited_command || rule == RULE_LOGIN);
                // The act answered makes the task the policy's — unless it only
                // delivered a person's exchange again, or they stopped it.
                // `/login` makes nothing the policy's: it does no work of the
                // task's, and the prompt its wall swallowed, delivered again
                // once the login is back, is whoever's it was
                // ([`LoginBooks::hand`]). Made the policy's, a person's
                // question delivered after the loop's own `/login` read as
                // the carried task's turn, and `keep going` went in after the
                // answer to it (the review's LW-1).
                if rule != RULE_LOGIN {
                    self.carrying = !self.exchange && !reading.interrupted;
                }
                // A `/compact` that owes the check its continuation delivers
                // it again too ([`Self::compact_owes`]).
                let delivers = checked || self.compact_owes.is_some_and(|o| o.check);
                if !(delivers && is_wall_retry(rule)) {
                    self.followed = Some(rule);
                } else if self.followed.is_none() {
                    // The check a loop before this one typed, a wall having
                    // cut it: its retry, typed here, delivers it again.
                    self.followed = self.seeded_check;
                }
                self.point_worked = d;
                self.new_point(now);
                // At the check only the worker's own word is a done report
                // ([`says_done_outright`]), or an idle report alone as a short
                // turn's whole answer ([`check_answer_ends`]): any other
                // first-only one (the check's words echoed, an idle report
                // closing real work) is its work going on, as the check's
                // `otherwise continue` asked, and is read as progress is —
                // its real work ends the streak. Nor is a done
                // word the record's closed answer says is none
                // ([`TurnEndReading::check_record`]): a tool's words the
                // worker quoted in code, which the screen draws as prose
                // (RR4-TE-1). So too at a land act's yield once the land
                // instruction was typed (`landing`): the instruction stands in
                // for the check, so the done reply that lands the task is the
                // worker's own word too — an idle report or an echo there is
                // no landing, and Land's nudges go on (Autopilot Land).
                let done = if checked || landing {
                    check_ends(checked, landing)
                } else {
                    done
                };
                if is_continue_rule(rule) {
                    self.short = streak(self.short, d, done);
                }
                // A PERSON'S OWN EXCHANGE delivered again by a wall's act is
                // left to them, whatever it says. Else a done report after the
                // check ends the task, and so does one after a land act once
                // the land instruction was typed (the instruction is Land's
                // done check); after any other act it is the first and gets
                // the check. An act never taken said nothing.
                if ok_to_leave {
                    self.done = match (self.exchange, done, checked || landing) {
                        (true, ..) => 2,
                        (_, false, _) => 0,
                        (_, true, true) => 2,
                        (_, true, false) => 1,
                    };
                    // Landed, never left: a land act is a continuation, which
                    // makes the task the policy's (`exchange` is false here).
                    self.landed = landing && !self.exchange && self.done == 2;
                }
            }
            // Someone else's turn makes the task theirs again: its done
            // report is a first one; one ending on a wall is left to the
            // wall's ladder; the streak stands. A PERSON'S OWN EXCHANGE —
            // their message, answered within `EXCHANGE_WORK` — is theirs to
            // answer, whatever it says: nothing is typed until the next turn
            // ends. Not a turn the conversation's record says nobody asked
            // (Claude Code's own wake of a task its worker finished,
            // [`TurnEndReading::finished_record`]): the record's finish below
            // keeps it ended, a wall it ended on included.
            (None, Some(d)) => {
                self.exchange_work = d;
                self.exchange = theirs && d < EXCHANGE_WORK;
                self.carrying &= carried;
                // A landing a land act's reply ended is over: whatever ends
                // this turn (left to the person, a FINISHED) is not Land's.
                self.landed = false;
                let left = self.exchange && ok_to_leave;
                // At a check a loop before this one typed, its answer: the
                // point follows that check ([`Self::seeded_check`]).
                self.followed = if checked { self.seeded_check } else { None };
                self.point_worked = d;
                self.new_point(now);
                // A check or a land act a loop before this one typed, its
                // answer this turn ([`Self::seed_land`]).
                self.done = if left || (check_ends(checked, landing) && ok_to_leave) {
                    2
                } else {
                    u8::from(done && reading.wall.is_none())
                };
                self.landed = landing && !left && self.done == 2;
                match seeded_yield {
                    // The yield of the act a loop before this one typed
                    // last ([`Self::seed_yield`]), as that act's own arm
                    // above moves it — its carry ended where someone
                    // stopped it (`yield_stopped`).
                    Some(continues) => {
                        if yield_stopped {
                            self.carrying = false;
                        }
                        if continues {
                            let done = if checked || landing {
                                check_ends(checked, landing)
                            } else {
                                done
                            };
                            self.short = streak(self.short, d, done);
                        }
                    }
                    None if reading.wall.is_none() && !left => {
                        self.short = streak(self.short, d, false);
                    }
                    None => {}
                }
            }
            // The first point seen, its work unknown: a supervisor that
            // attaches to an idle worker backs off before it types. A
            // session with no task yet ended nothing: no streak begins.
            //
            // NEVER LEFT TO A PERSON, whatever hand is read in it: no read
            // saw the turn run, so nothing here says who opened it. The
            // person stamp is any key, click or scroll; the `❯` row above
            // the reply is the harness's own continuation (`keep going`, the
            // done check, the land instruction) as often as a person's
            // message; and a person's check-in on the run, the chip then
            // taken round through Off, reads as their own greeting. A turn
            // misread as theirs here stalls the work with nothing escalated,
            // where the back-off and the act cost at most a visible act over
            // a greeting.
            (None, None) if self.point_at.is_none() && !reading.taskless => {
                if yield_stopped {
                    self.carrying = false;
                }
                if let Some(at) = seeded_point.filter(|_| same_point) {
                    // The point a loop before this one judged: its streak
                    // and its time ([`Self::seed_point`]).
                    self.new_point(at);
                } else {
                    self.new_point(now);
                    // A streak a loop before this one left is lengthened
                    // ([`Self::seed_short`]): this point is its act's yield,
                    // or a turn after the point it recorded.
                    self.short = self.short.saturating_add(1);
                }
                self.done = if check_ends(checked, landing) && ok_to_leave {
                    2
                } else {
                    u8::from(done && reading.wall.is_none())
                };
                self.landed = landing && self.done == 2;
                self.exchange = false;
                // THE LANDING A LOOP BEFORE THIS ONE LANDED, taken up where
                // the screen's last reply still says done (its done row or
                // the worker's own token, nothing standing over it) and is
                // still the landing's own words (their mark, [`said_mark`]):
                // the task is landed as that loop left it — done twice,
                // landed, the episode's instruction typed — and nothing is
                // typed into finished work. A reply that no longer says done,
                // or another reply (a turn no loop watched), drops it, and
                // the point is decided as any first point is.
                if let Some(episode) = seeded
                    && ok_to_leave
                    && landing_words
                        .is_none_or(|w| said_mark(reading.said_tail.as_deref()) == Some(w))
                    && (done || says_finished(reading.said_tail.as_deref()))
                {
                    self.done = 2;
                    self.landed = true;
                    self.land.typed |= episode && reading.autopilot == Autopilot::Land;
                }
            }
            (None, None) => {}
        }
        // What the point's record holds, moved here ([`Self::take_point_moved`]):
        // the streak, an act's answer judged, or the point's time — not at
        // the point a loop before this one judged, read again — this loop's
        // first point among them (its ledger records every act's answer from
        // here on). The time moves at every turn's end this state judges,
        // whether or not the streak does: someone else's real work with the
        // streak at 0, a turn ending on a wall, a person's exchange left to
        // them. Recorded only where the streak moved, a loop that follows
        // counted the Esc hold from the point before and typed through it
        // (the verifier of 75b03ac6d, RS9-F2).
        self.point_moved = self.short != short_before
            || ((checked || landing) && !same_point)
            || awaited.is_some()
            || (self.point_at != point_at_before && !same_point);
        // THE CONVERSATION'S OWN RECORD SAYS THE TASK IS FINISHED
        // ([`TurnEndReading::finished_record`]): its worker ended it and no
        // prompt of a person's or an orchestrator's has come since. The turn
        // that ended here was nobody's — Claude Code's own wake, a relaunched
        // agent's start-up, the harness's own — or the screen never showed the
        // token (said while a shell kept it busy, scrolled off by the vendor's
        // rows after a resume, a new loop): the task stays finished, as the
        // done ruling of 2026-09-27 says, until someone's turn. AT A WALL
        // TOO, unlike the screen's own token: the screen cannot say whose
        // turn met the wall, the record can — a prompt of someone's would
        // have opened it — so the wall answered a turn nobody asked, and its
        // ladder owes that wake no retry, whether this state saw the finish
        // before (a wake of a task it ended) or never did (a new loop's
        // first point, a token said under a busy screen). Not under an
        // unanswered prompt, which the record may not hold yet.
        let by_record = reading.finished_record == Some(true) && !reading.pending_input;
        // …and whether the end is the worker's TOKEN: the screen's, or the
        // record's where its finish is no done reply in words to the done
        // check ([`TurnEndReading::finished_at_check`]). The record ends the
        // task on either; only the token is the worker's FINISHED (F2 of the
        // live lab of 2026-10-16: a reply in words to the check, its turn
        // seen run, was journalled as the worker having reported the token).
        let token = finished || (by_record && !reading.finished_at_check);
        // The screen's own FINISHED, which alone the challenges read
        // ([`open_work`], [`TurnEndReading::repo`]): a finish the record
        // carries over a wake is the task its worker already ended.
        let said_finished = finished;
        let finished = finished || by_record;
        // A PERSON'S OWN EXCHANGE AN EARLIER LOOP LEFT TO THEM, its turn still
        // the conversation's last ([`TurnEndReading::left_record`]; the
        // review's RR3-TE-2): theirs still, whatever brought this state here
        // — a new loop's first point, a relaunched agent's start-up, a wake
        // that wrote no answer — and nothing is typed until the next turn
        // ends. Kept only in memory, a new loop read the greeting by its
        // words (the done check, `Nobody is here to answer…`), and a
        // relaunch's start-up as someone's turn, continued. Not under an
        // unanswered prompt, at a standing wall ([`TurnEndReading::wall_drawn`]),
        // or where the worker finished. The one way a point no read of this
        // state saw run is left to a person (the first-point arm above leaves
        // none): the row is the judgement of the loop that saw it run, never
        // this read's.
        if reading.left_record && !reading.pending_input && !reading.wall_drawn() && !finished {
            self.exchange = true;
            self.done = 2;
        }
        // …AND ONE AN EARLIER LOOP SAW MEET A WALL (the review's RR4-TE-2;
        // [`Self::exchange_at_wall`]), at this state's first point, the
        // wall still on the screen: the person's exchange still, the wall's
        // ladder its own — the retry is typed, and its short answer is
        // theirs, as in the loop that saw the turn: the work before the wall,
        // which the row carries, counts with the answer's (a wall splits a
        // job, it never makes it an exchange).
        if reading.left_record && reading.wall.is_some() && first_point && !finished {
            self.exchange = true;
            self.exchange_work = self
                .exchange_work
                .max(reading.left_work.unwrap_or_default());
        }
        // The worker's own FINISHED ends the task at once, whoever's turn it
        // answered: no check round trip — unless its report's own words name
        // open work of ANY kind ([`open_work`], a blocked item among it), or
        // the session's repository holds work no remote has
        // ([`TurnEndReading::repo`]): then the point owes the challenge
        // ([`RULE_OPEN_ITEMS`], [`RULE_UNCOMMITTED`]) and is a first done
        // report — at EVERY such token, never once a job (the owner's rule of
        // 2026-10-19: `ATERM-TASK-DONE` ends nothing while anything is left).
        // So too where the worker's word ends the task at the check, at a
        // land act or at the restart's check (the arms above): a done reply
        // in words over open work, or over a dirty repository, ends nothing.
        // Judged where a read saw the turn run (an act's answer, a turn of
        // anyone's), and kept while the point is read again; at this state's
        // first point its words and the repository are judged too, whether a
        // loop before this one judged that point ([`Self::seed_point`]: what
        // it found is on no record, so it is found again) or none did (a
        // relaunch's, an upgrade's, a new window's): a token ends no job with
        // work left whichever loop reads it first. Under
        // Cruise and Land alike (Land's instruction asks for nothing left);
        // never over a person's own exchange, their Esc or Off. Only the
        // screen's own token is read; a finish the record carries over a
        // wake is the task its worker already ended.
        // AT THE CHECK'S YIELD the token on a line of its own ANYWHERE is the
        // check's answer, the words after it the report's own: they are read
        // for open work as the last line's token's are, so `ATERM-TASK-DONE`
        // over `but one test still fails` gets the challenge, as the same
        // words over the token would. Outside the check such a reply is a
        // first done report, and the check asks the same.
        let token_at_check = (checked || host == Some(HostTurn::Check))
            && reading
                .said_tail
                .as_deref()
                .is_some_and(|t| t.lines().any(is_finished_line))
            && !reading.wall_drawn()
            && !reading.pending_input;
        let word_ended = (checked || landing || host == Some(HostTurn::Check))
            && self.done == 2
            && !self.exchange
            && ok_to_leave;
        let judged = awaited.is_some() || reading.worked.is_some();
        if judged || (first_point && !reading.taskless) {
            let judge = (said_finished || token_at_check || word_ended)
                && !self.exchange
                && !reading.interrupted
                && reading.autopilot != Autopilot::Off;
            self.open = if judge {
                open_work(reading.said_tail.as_deref())
            } else {
                Vec::new()
            };
            self.repo = reading.repo.clone().filter(|_| judge);
        }
        let owed = !self.open.is_empty() || self.repo.is_some();
        if finished && !owed {
            self.done = 2;
            self.finished = token;
        } else {
            if owed {
                self.done = 1;
            }
            if self.done < 2 {
                self.finished = false;
            }
        }
        // The done's instant ([`Self::done_shown`]): this read, where it
        // decided a done — at a new point, or with none (a turn the host
        // typed, answered with FINISHED, makes none) — and the same point
        // read again keeps the instant it first showed at.
        if self.done < 2 {
            self.done_at = None;
        } else if done_before < 2 || self.points != points_before {
            self.done_at = Some(now);
        }
        if self.done < 2 {
            self.landed = false;
        }
        // THE LAND EPISODE'S END, where this point answered a land act no
        // wall cut: quiet where its nudges are spent and the reply landed
        // nothing; a reply that landed the task ends any quiet before it
        // ([`LandEpisode::quiet`]). Nothing typed turns on it.
        if landing && !self.cut_land {
            self.land.quiet = self.land_spent() && !self.landed;
        }
        // The policy carries no task it stopped driving: a landing spent —
        // its instruction and nudges typed, the last yield no done — types
        // nothing more of its own, so a person's turn after it, answered
        // within `EXCHANGE_WORK`, is theirs (a question with nobody's hand,
        // or a person's job past that bound, still gets the land answer).
        if self.done == 2 || (reading.autopilot == Autopilot::Land && self.land_spent()) {
            self.carrying = false;
        }
        self.carry_at_attach = false;
        // A turn read here ended in a person's own exchange left to them
        // ([`Self::left_judged`]).
        self.left_judged = (awaited.is_some() || reading.worked.is_some()) && self.left_to_person();
        let retried = wall_retry;
        // A `/compact` OF THIS POLICY'S THAT COMPACTED NOTHING — no busy read
        // since it (a compaction shows its spinner, then `Compacted`) and no
        // wall at its point (`Not enough messages to compact.`, a PreCompact
        // hook that blocked it, `/compact` switched off) — left the context
        // as full as it was: the full context's own answer, MET, not a retry
        // taken. Its track stands, and the continuation owed after it meets
        // the wall again as the next rung (2026-10-15: read as taken, the
        // track was dropped, the wall the continuation met was a new episode
        // compacted at once, and a single-exchange context looped `/compact`
        // and its continuation twelve acts an hour for good).
        let compacted_nothing = awaited == Some(RULE_COMPACT)
            && awaited_command
            && reading.worked.is_none()
            && reading.wall.is_none()
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Context);
        // A retry was TAKEN where no wall answered it: none on the screen at
        // its point, and none read inside its window ([`Awaited::met`]).
        let retry_taken = retried && !met && !compacted_nothing;
        // A latch a previous loop left ([`Self::seed_account_unmade`]) is
        // the first wall's, and no later one's.
        let unmade_seeded = std::mem::take(&mut self.account_unmade_seed);
        match reading.wall {
            Some(kind) => {
                let at_point = true;
                self.track_wall(
                    reading,
                    kind,
                    retried,
                    worked_to_wall,
                    unmade_seeded,
                    at_point,
                    Arrival {
                        hand: theirs,
                        seen: arrived,
                    },
                    now,
                );
            }
            // The wall has left the screen: gone for good once the worker
            // worked (the vendor's own continuation included, above) or our
            // retry was taken. With no work — Esc on the vendor's own
            // auto-continue, a person's `/usage`, `/login` or `/model` output
            // under it — the track stays, and the point is decided by the
            // reset it named ([`still_shown`]). So too after a retry the wall
            // answered inside its window, its notice gone by the window's end
            // (round 16's review: read as taken, the track was dropped and the
            // continue policy typed into the standing limit). Where the vendor
            // says its auto-continue will not go on (`Automatic continue
            // cancelled`), nobody goes on by itself now — and nobody does
            // where its COUNTDOWN is no longer drawn: Claude Code's screen,
            // its composer drawn, no notice, no countdown
            // ([`aterm_phase::AutoContinue::Armed`]) and no word that it went
            // on (its continuation of THIS limit ended the track above). A
            // `/resume` or a `/clear` drops the countdown without a word (W4
            // of ATERM-KI-1015-1, the lab on 2.1.300): kept as one that goes
            // on by itself, the limit was continued only at the reset plus
            // [`TurnEndTiming::auto_resume_grace`]; placed as a cancelled one,
            // at the reset plus the grace and the jitter. A lost login is gone
            // for good once `Login successful` was read
            // ([`TurnEndReading::login_back`]): a person's `/status` after it
            // draws over that row, and is no login lost again.
            None => {
                let login_back = reading.login_back
                    && self
                        .wall
                        .as_ref()
                        .is_some_and(|t| t.class == WallClass::Auth);
                // A loop before this one's retry owed its answer
                // ([`WallTrack::owed`]) and no wall here: taken, as an act
                // awaited is.
                let owed = self.wall.as_ref().is_some_and(|t| t.owed);
                // `⎿  Login interrupted` is no work past a lost login: the
                // `/login` dialog's own spinner (`Opening browser to sign
                // in…`) reads busy, and read as the worker's work, it dropped
                // the track — the continue policy typed `keep going` into the
                // standing wall and `/login` went a second time (MEASURED on
                // origin/main b3946413c4 in the real-Claude lab, 2026-10-19:
                // `claude-2.1.303-login-relooped.txt`).
                let interrupted = reading.login_interrupted
                    && self
                        .wall
                        .as_ref()
                        .is_some_and(|t| t.class == WallClass::Auth);
                let gone =
                    (reading.worked.is_some() && !interrupted) || retry_taken || owed || login_back;
                if gone || unmade_seeded {
                    self.account_unmade = false;
                }
                let dropped = auto_continues(reading.program)
                    && reading.composer != Composer::Absent
                    && reading.auto_continue != Some(aterm_phase::AutoContinue::Armed);
                let since = reading.since_limit.as_ref();
                if gone {
                    self.limit_ended |= self.wall.as_ref().is_some_and(|t| named_class(t.class));
                    // A lost login gone with no wall at the point: the login
                    // is back, news to the tabs that waited on it.
                    self.login_returned |= self
                        .wall
                        .as_ref()
                        .is_some_and(|t| t.class == WallClass::Auth);
                    self.wall = None;
                    self.fallback_unmade = false;
                } else if interrupted && let Some(t) = self.wall.as_mut() {
                    // The `/login` left: the rest of this loss is the
                    // owner's, told as a sign-in cancelled.
                    t.login.cancelled = true;
                } else if let Some(t) = self.wall.as_mut().filter(|t| {
                    t.class == WallClass::Model
                        && t.named.is_some_and(|n| n.moved_off_under(since, &t.said))
                }) {
                    // A person's `/model` since the bucket's notice: the
                    // session left the model the bucket limits, and its reset
                    // binds nothing ([`named_class`]); the point goes at
                    // once ([`decide_turn_end`]).
                    t.named = None;
                } else if (dropped
                    || reading.auto_continue == Some(aterm_phase::AutoContinue::Unkept))
                    && let Some(n) = self.wall.as_mut().and_then(|t| t.named.as_mut())
                {
                    n.by_itself = false;
                }
            }
        }
        // THE ACCOUNT MOVE NOT ANSWERED YET ([`Self::account_answer`], which
        // the loop asks BEFORE this fold): a turn that ran. After a bad
        // answer the host was told of, the worker works again with no wall —
        // the limit lifted, the login is back — and the move is behind the
        // session (told the host as served, where the tell read this point).
        // Before any answer, the turn was someone's (no act of the
        // harness's asked it): a turn HAS run under the new login now,
        // whatever it ended on, so the next one that is served proves it, a
        // lost login it met is the new process's own ([`Self::login_left`]),
        // and a move made from the wall it met owes the resume of the turn
        // that wall cut short ([`Self::restarted`]). NOT THE FIRST POINT
        // AFTER THE RELAUNCH: work shown there is the resumed process's own
        // start-up (drawing its conversation, read busy), a turn nobody asked
        // — read as one, it armed the tell, and the next busy read told the
        // host `served` for an account that had answered nothing. Nor is it
        // the worker working past a told wall: that move stands.
        let startup = std::mem::take(&mut self.account_startup);
        // A resume a loop before this one left owed ([`Self::seed_account`])
        // is owed AT THE NOTICE THE SESSION LEFT, and nowhere else: the usage
        // limit read as the wall; the session's newest usage-limit notice
        // drawn with no turn served under it, though no wall is read (the
        // vendor's `Automatic continue cancelled` under it is the last thing
        // said); the lost login no turn has been asked under. A person's turn
        // leaves no row, so at a point that draws a turn served since the
        // notice, or no notice, the ledger cannot say whether a turn went
        // since — and that point is the continue policy's, as with no move.
        if std::mem::take(&mut self.account_resume_seed)
            && !left_login
            && !matches!(
                reading.wall,
                Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
            )
            && !reading.since_limit.as_ref().is_some_and(|s| !s.served)
        {
            self.account_resume = false;
        }
        // ANY turn that ran since the relaunch pays the wall's resume,
        // whatever it ended on: a limit a turn of someone's worked up to is
        // the NEW account's wall, the host's to be asked about — kept owed,
        // the continuation was typed into it at once.
        if self.account.is_some() && reading.worked.is_some() && !reading.pending_input && !startup
        {
            self.account_resume = false;
            self.account_asked = true;
        }
        if self.account.is_some()
            && self.account_told
            && reading.wall.is_none()
            && reading.worked.is_some()
            && !reading.pending_input
            && !startup
        {
            self.account_over();
        }
        // A LIMIT RESET'S CONTINUATION ([`Self::reset_told`]) is paid by a
        // turn that ran SINCE its outcome was told, whatever it ended on. Not
        // by the work of the first point after the dialog where no busy read
        // came since: that span began before the dialog, and is the turn
        // that met the wall — read as work since, the continuation a kept or
        // failed reset owes was dropped at the very point it is typed at.
        if let Some(owed) = self.reset_resume
            && reading
                .worked
                .and_then(|worked| now.checked_sub(worked))
                .is_some_and(|began| began >= owed.told)
        {
            self.reset_resume = None;
        }
    }

    /// A TURN NO READ SAW BUSY ENDED THE LIMIT THIS POLICY WAITS ON — a usage
    /// window's or a model bucket's ([`named_class`]; [`Self::observe`], which
    /// folds it in as work): its notice gone, no work read, and the vendor's
    /// own continuation of THIS limit fired ([`NamedReset::continued_under`])
    /// or a turn served since this limit's notice
    /// ([`NamedReset::served_under`]), on the screen or in the transcript.
    /// The screen's notice ties nothing where the conversation the tab holds
    /// now has no row of this limit's notice
    /// ([`TurnEndReading::notice_elsewhere`], the residual review's R2): an
    /// older notice of a `/resume`d conversation naming the same clock — nor
    /// the vendor's word read before this limit's reset, where `now` is known
    /// ([`NamedReset::continued_under`]).
    fn limit_went_on(&self, reading: &TurnEndReading, now: Option<Instant>) -> bool {
        let since = reading
            .since_limit
            .as_ref()
            .filter(|_| !reading.notice_elsewhere);
        let named = self.wall.as_ref().map(|t| t.named);
        let served =
            reading.served_in_transcript || named.flatten().is_some_and(|n| n.served_under(since));
        let continued = reading.auto_continue == Some(aterm_phase::AutoContinue::Fired)
            && named.is_some_and(|n| n.is_none_or(|n| n.continued_under(since, now)));
        reading.wall.is_none()
            && reading.worked.is_none()
            && (continued || served)
            && self.wall.as_ref().is_some_and(|t| named_class(t.class))
    }

    /// THE ACT AWAITED WAS TAKEN AND ANSWERED, though no read saw its turn
    /// run: the work of that turn, from the signs the reading carries, or
    /// `None` where nothing says so — the act's point is then judged at its
    /// take window's end, as before ([`Self::point_judged`]).
    ///
    /// A turn of a fraction of a second starts and ends between two of the
    /// loop's looks: the lab's stub answered the done check in 41 ms with the
    /// idle line the worker had said before, the same words drawn 0.6 s after
    /// the Enter, and the screen's only news was the check's own row
    /// (the lab's done-check scenario `d1l` on 2.1.303). Taken only by a busy
    /// read, the act stood untaken to its 30 s window, its point was then a
    /// turn that answered nothing — no done report of its own — and the
    /// check was continued with `keep going` where its idle answer in a short
    /// turn ends the task. So an act is TAKEN AND ANSWERED where:
    ///
    /// * THE RECORD says so: the conversation's last turn was opened by a
    ///   prompt of the act's own words ([`Awaited::said`]), recorded no
    ///   earlier than the act was typed (less [`RECORD_SLACK`]), and closed —
    ///   its work Claude Code's own `turn_duration` ([`RecordTurn`]);
    /// * or THE SCREEN does: the worker's last words close the turn the
    ///   act's own `❯` row opened, where the screen it was typed at closed
    ///   another prompt's ([`Awaited::over`]) — its work the done row drawn
    ///   under it, else at most the time since the act.
    ///
    /// Either, only at an answered point: idle or a question, authoritative,
    /// no box, survey or wall (a wall's answer is the wall's track's,
    /// [`Self::track_wall`]), no unanswered row, no person's Esc and an empty
    /// composer — a draft is still a draft. Never for an act a wall answered
    /// already ([`Awaited::met`]), nor across Off ([`Awaited::off_stood`]),
    /// nor one whose words this state does not hold. So the act's words still
    /// in the composer, a paste Claude Code refused (the screen is the one it
    /// was typed at), a starting Claude Code's redrawn history (typed
    /// over no answered prompt, its record's rows older than the act), and
    /// the act's own row over Claude Code's label for a hook that stopped it
    /// (`⏺ Operation stopped by hook: <stopReason>`: no words of the worker's
    /// and no answered prompt, [`aterm_phase::said_tail`]; its record's turn
    /// never closed, Claude Code queried nothing) are never read as the act's
    /// answer.
    fn answered_unseen(&self, reading: &TurnEndReading, now: Instant) -> Option<Duration> {
        let a = self.untaken().filter(|a| !a.met && !a.off_stood)?;
        let said = a.said?;
        let answered_point = reading.worked.is_none()
            && matches!(reading.phase, Phase::Idle | Phase::Question)
            && reading.authoritative
            && !reading.survey
            && reading.wall.is_none()
            && !reading.pending_input
            && !reading.interrupted
            && matches!(reading.composer, Composer::Empty | Composer::Placeholder(_));
        if !answered_point {
            return None;
        }
        let since = now.saturating_duration_since(a.at);
        let by_record = reading
            .record_turn
            .filter(|t| {
                t.prompt == said
                    && now
                        .checked_sub(t.age)
                        .is_some_and(|recorded| recorded + RECORD_SLACK >= a.typed)
            })
            .and_then(|t| t.ended);
        let by_screen = reading
            .answered
            .filter(|s| s.prompt == said && a.over.is_some_and(|o| o != said))
            .map(|s| s.work.unwrap_or(since));
        by_record.or(by_screen).map(|work| work.min(since))
    }

    /// Whether the reading is a POINT the awaited act is judged at
    /// ([`Self::observe`]): not anyone's unanswered `❯` row but the act's own
    /// past its window (then judged not taken, [`Self::stale_input`]), and
    /// not a point after a text act no busy read has followed, until
    /// [`TurnEndTiming::take_within`] from the first such point. Stamps that
    /// first point; judges nothing.
    fn point_judged(&mut self, reading: &TurnEndReading, now: Instant) -> bool {
        if reading.pending_input {
            let Some(a) = self.awaiting.as_mut().filter(|_| reading.worked.is_none()) else {
                return false;
            };
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return false;
            }
            self.stale_input = true;
        } else if let Some(a) = &mut self.awaiting
            && !a.command
            && reading.worked.is_none()
        {
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return false;
            }
        }
        true
    }

    /// THE WALL A READING SHOWS, folded into its track ([`Self::observe`],
    /// at a point or not — `at_point`, [`WallTrack::pointed`]): another class
    /// opens a track — its appearance `now`, the fallback's and the account
    /// move's latches its own (the latter `unmade_seeded`, a latch a loop
    /// before this one left for the first wall this state sees) — and the
    /// same class after a retry of ours (`retried`) is its next appearance, a
    /// new episode with its ladder from the start where real work came first
    /// (`worked`). The reset the notice names, and its words, are kept for
    /// when it leaves ([`still_shown`]).
    #[allow(clippy::too_many_arguments)]
    fn track_wall(
        &mut self,
        reading: &TurnEndReading,
        kind: WallKind,
        retried: bool,
        worked: bool,
        unmade_seeded: bool,
        at_point: bool,
        arrival: Arrival,
        now: Instant,
    ) {
        let class = class_of(kind);
        // The ladder this reading puts the wall on, as [`wall_own_act`]
        // decides it: the entitlement ladder for a wall no reset ends
        // ([`entitlement_ladder`]), unless the vendor goes on by itself
        // there, which is waited on the short ladder's last rung.
        let entitlement = entitlement_ladder(reading.reset_at.is_some(), kind)
            && !goes_on_by_itself(
                self,
                self.wall.as_ref().filter(|t| t.class == class),
                reading,
            );
        match &mut self.wall {
            Some(t) if t.class == class => {
                // A loop before this one's retry, owed its answer
                // ([`WallTrack::owed`]): this reading is it.
                let retried = retried || std::mem::take(&mut t.owed);
                t.pointed |= at_point;
                // The act led to that work: a new episode, its ladder from
                // the start — but for a reply past its output token maximum,
                // which that work IS (a reply long enough to overrun streams
                // for minutes): its ladder climbs on. So too THE WORK A
                // SEED'S TRY LED TO: a track only a seed made
                // ([`WallTrack::seeded`]: a loop before this one typed the
                // try, and no limit has been read since it, by that loop or
                // this one), met with the worker's work since — the try's
                // turn, which the loop that typed it would have seen as its
                // retry's work (the verifier of ed6efadb0, V22-A: a successor
                // that saw a spend try's seventy minutes of work waited out
                // the try's six-hour rung where the first loop waited 30 min
                // from the wall). A try a loop before this one read met again
                // is no seed only ([`Self::seed_met_attempt`]): work after it
                // is a person's turn, after which the loop that typed the try
                // keeps its rung.
                let anew = worked && !work_is_its_cause(kind) && (retried || t.seeded);
                // The try's answer: the appearance moves to it, at a retry of
                // ours — and where a seed's try met its limit at the end of
                // a turn this loop saw run (its work read, short or long):
                // that turn was the try's own (no limit read since the try,
                // [`WallTrack::seeded`]), and the loop that typed it moved
                // the appearance there, its retry answered (the verifier of
                // 5b72b1613, R5-2: kept at the try, a successor attached into
                // the try's own short turn counted the next rung from the
                // try, and typed into the standing limit as much sooner as
                // that turn ran). With no work read, the limit answered the
                // try when it was typed, as Claude Code answers a line typed
                // at a standing limit: the appearance stays the try's.
                if retried || (t.seeded && reading.worked.is_some()) {
                    t.since = now;
                }
                if anew {
                    t.since = now;
                    t.attempts = 0;
                    t.ups = 0;
                    t.exhausted = false;
                    t.fill = None;
                    self.limit_ended |= named_class(class);
                }
                // A LATCH IS ITS WALL'S: the move refused at the wall before
                // this work is asked again at this one. Kept, a session whose
                // continuation worked half an hour and met its limit again
                // waited that whole second wall out beside an account it
                // could run on.
                if worked {
                    self.account_unmade = false;
                }
                // A LOST LOGIN THAT ANSWERED ANOTHER PROMPT (a person's
                // second message, met by the wall in milliseconds): read
                // arriving here, its key the one that sent it.
                let prompt = reading.answered.map(|a| a.prompt);
                if class == WallClass::Auth && prompt.is_some() && prompt != t.login.prompt {
                    t.login.prompt = prompt;
                    t.login.prompt_at = Some(now);
                    t.login.hand |= arrival.hand;
                }
            }
            _ => {
                // Another class's wall in a limit's place: that limit's
                // track ends, and its tries ([`Self::limit_ended`]).
                self.limit_ended |= self.wall.as_ref().is_some_and(|t| named_class(t.class));
                self.wall = Some(WallTrack {
                    class,
                    since: now,
                    attempts: 0,
                    ups: 0,
                    exhausted: false,
                    fill: None,
                    named: None,
                    pointed: at_point,
                    said: String::new(),
                    kind: Some(kind),
                    seeded: false,
                    entitlement: false,
                    owed: false,
                    login: if class == WallClass::Auth {
                        LoginBooks {
                            hand: arrival.hand,
                            prompt_at: arrival.seen.then_some(now),
                            prompt: reading.answered.map(|a| a.prompt),
                            ..LoginBooks::default()
                        }
                    } else {
                        LoginBooks::default()
                    },
                });
                self.fallback_unmade = false;
                self.account_unmade = unmade_seeded;
            }
        }
        if let Some(t) = &mut self.wall {
            if class == WallClass::Context && compaction_exhausted_row(&reading.wall_message) {
                t.exhausted = true;
            }
            if class == WallClass::Context
                && let Some(fill) = ContextFill::of(&reading.wall_message)
            {
                t.fill = Some(fill);
            }
            t.said.clone_from(&reading.wall_message);
            t.kind = Some(kind);
            // Its notice read: a seed is the limit's track now.
            t.seeded = false;
            // ... on the ladder this reading puts it.
            t.entitlement = entitlement;
            // The reset the notice names, kept for when it leaves — and when
            // it was first read, kept while the notice names the same reset:
            // a countdown a relaunched process redraws from its conversation
            // is the gone process's (KI-1015-1 W4).
            if let Some(at) = reading.reset_at {
                let kept = t
                    .named
                    .filter(|n| n.said.is_some() && n.said == reading.reset_clock);
                let first = kept.map_or(now, |n| n.first);
                // ...and when its row was written, by the transcript that
                // confirmed it: a notice older than the agent's process is
                // the gone process's countdown, whenever this loop first read
                // it (a cold restore's first loop).
                let written = kept.and_then(|n| n.written).or(self.notice_unix);
                let gone = self.countdown_gone(first) || self.written_before_start(written);
                if let Some(t) = &mut self.wall {
                    t.named = Some(NamedReset {
                        kind,
                        at,
                        jitter: reading.jitter,
                        by_itself: reading.resumes_by_itself && !gone,
                        said: reading.reset_clock,
                        first,
                        written,
                    });
                }
            }
        }
    }

    /// Records an act the loop typed (not a skipped or refused one): the
    /// budget, the awaited act, what it leaves owed, a wall track's attempts,
    /// and the Land episode's instruction and nudges (`reading` is the one
    /// the act was decided on: its [`TurnEndReading::autopilot`] says whether
    /// the episode still stands). Restarts go through [`Self::restarted`].
    pub fn acted(&mut self, action: &TurnEndAction, reading: &TurnEndReading, now: Instant) {
        self.limit_try = None;
        if matches!(action, TurnEndAction::Restart { .. }) {
            return;
        }
        // `/login`'s success answered: one Enter, no act of the hour's, and
        // nothing awaited — the dialog's leaving is the answer.
        if matches!(action, TurnEndAction::Confirm { .. }) {
            self.login_confirmed = true;
            return;
        }
        let Some(rule) = action.rule_id() else {
            return;
        };
        // The act is the ledger's last word on the streak now.
        self.point_last = false;
        // A turn asked on the fallback ([`Self::bucket_left`]).
        self.fallback_unasked = false;
        // A turn asked under the login an account move chose
        // ([`Self::account_answer`]) — and the wall's resume, if one was
        // owed, is paid by it. The process that takes it has drawn its
        // start-up ([`Self::account_startup`]).
        self.account_asked |= self.account.is_some();
        self.account_startup = false;
        self.account_resume = false;
        // …and so is the continuation a limit reset left owed
        // ([`Self::reset_told`]): any act typed is a turn asked.
        self.reset_resume = None;
        // AN ANSWER TYPED IN A LIMIT'S PLACE — the done check to a done
        // report since the limit, or the reversible answer to words since it
        // that name an irreversible act, or the priority's answer to any other
        // question or stop ([`is_answer_rule`]), at its end ([`limit_end`]), or under
        // Land the land act that stands in for either ([`is_land_rule`]), or
        // the open-items challenge, the done check's kin, wherever the
        // continue policy types it (the merge review's MG7-A4: no rung of
        // its own) — while the track of a usage window or a model bucket
        // stands — is the wall's act as the continuation it stands in for
        // would be: its attempt, its next appearance the act's retry met, the
        // owner's clearing answered. Met again, its point keeps it as the
        // act the wall cut ([`Self::cut_check`], [`Self::cut_land`]): the
        // wall's retry delivers it again, and its done reply is that act's.
        // Measured before (the adversarial verifier of the limit's-end fix,
        // D2): where the limit had not lifted, the check met it with the
        // track's attempts at 0, and `keep going` went under
        // `usage-resume@v1` into the standing limit as soon as the check's
        // take window ended — the ladder a rung late.
        // Not at a track only a loop before this one's usage act seeded, no
        // limit read since ([`WallTrack::seeded`]): no limit's continuation
        // is decided there ([`limit_end`] answers one), and the done check
        // (or the land act) the continue policy types after the worker's
        // work is an ordinary one (the verifier of the successor's seed, D4:
        // counted and marked, it was the wall's attempt 2, and every loop
        // after it kept a phantom limit alive).
        let in_limits_place = (rule == RULE_DONE_CHECK
            || is_challenge_rule(rule)
            || is_answer_rule(rule)
            || is_land_rule(rule))
            && self
                .wall
                .as_ref()
                .is_some_and(|t| named_class(t.class) && !t.seeded);
        // The wall's continuation typed on the owner's clearing
        // ([`AccountAdvice::Resume`]): one a clearing.
        if (rule == RULE_LIMIT_RESUME || in_limits_place)
            && let Some(AccountAdvice::Resume { cleared_at }) = reading.advised()
        {
            self.account_resumed = Some(
                self.account_resumed
                    .map_or(*cleared_at, |typed| typed.max(*cleared_at)),
            );
        }
        self.acts.push_back(now);
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
        self.try_over = false;
        self.unread_try = None;
        self.seed_answered = false;
        self.awaiting = Some(Awaited {
            rule,
            command: matches!(action, TurnEndAction::TypeCommand { .. }),
            at: now,
            typed: now,
            unseen_since: None,
            taken: false,
            met: false,
            left: left_key(reading.since_limit.as_ref(), reading.auto_continue),
            host: None,
            off_stood: false,
            wall_retry: is_wall_retry(rule) || in_limits_place,
            said: match action {
                TurnEndAction::Type { text, .. } | TurnEndAction::Accept { text, .. } => {
                    Some(crate::supervise::approvals::words_hash(text))
                }
                _ => None,
            },
            over: reading.answered.map(|a| a.prompt),
        });
        let typed_rule = match action {
            TurnEndAction::Type { rule_id, .. } => Some(*rule_id),
            _ => None,
        };
        // A WALL'S RETRY, AS THIS POLICY TYPED IT, DELIVERS AGAIN THE ACT THE
        // WALL CUT ([`Self::cut_check`]): its yield is that act's.
        let again = typed_rule.is_some_and(is_wall_retry);
        let was_cut_check = std::mem::take(&mut self.cut_check);
        let was_cut_land = std::mem::take(&mut self.cut_land);
        // …AND SO DOES A `/COMPACT`'S CONTINUATION, the act typed again
        // ([`Self::compact_owes`]): the `/compact` itself delivers nothing,
        // and owes it to the continuation (or to a `/compact` typed again on
        // the context's ladder).
        let owed = self.compact_owes.take();
        let compact_cmd = matches!(
            action,
            TurnEndAction::TypeCommand {
                rule_id: RULE_COMPACT,
                then: Then::Continue,
                ..
            }
        );
        if compact_cmd {
            self.compact_owes = owed.or_else(|| {
                (was_cut_check || was_cut_land).then_some(CompactOwes {
                    check: was_cut_check,
                    land: was_cut_land,
                })
            });
        }
        let carried = owed.filter(|_| typed_rule == Some(RULE_COMPACT));
        let cut_check = (was_cut_check && again) || carried.is_some_and(|o| o.check);
        let cut_land = (was_cut_land && again) || carried.is_some_and(|o| o.land);
        if !cut_check && self.compact_owes.is_none_or(|o| !o.check) {
            self.seeded_check = None;
        }
        // The act's own text, which a `/compact`'s continuation types again
        // ([`Self::cut_text`]).
        let typed_text = match action {
            TurnEndAction::Type { text, .. } => Some(text.clone()),
            _ => None,
        };
        let check_kin = typed_rule.is_some_and(|r| r == RULE_DONE_CHECK || is_challenge_rule(r));
        let check_or_land = check_kin || typed_rule.is_some_and(is_land_rule);
        self.cut_text = if check_or_land || carried.is_some() {
            typed_text
        } else if cut_check || cut_land || self.compact_owes.is_some() {
            self.cut_text.take()
        } else {
            None
        };
        // The challenges are the done check's kin: a done reply to one ends
        // the task where it leaves nothing open, and a wall's retry delivers
        // it again. Any act answers the point that owed one; the next done
        // report over open work owes it again.
        self.checking = check_kin || cut_check;
        self.open.clear();
        self.repo = None;
        // THE LAND EPISODE: an act decided in another mode has left it; the
        // instruction and each nudge, as this policy typed them, are counted;
        // and a land act typed once the instruction was is the one whose done
        // reply ends the task ([`Self::observe`]).
        if reading.autopilot != Autopilot::Land {
            self.land = LandEpisode::default();
        }
        match typed_rule {
            Some(RULE_LAND) => self.land.typed = true,
            Some(RULE_LAND_NUDGE) => self.land.nudges = self.land.nudges.saturating_add(1),
            _ => {}
        }
        self.landing = self.land.typed && (typed_rule.is_some_and(is_land_rule) || cut_land);
        self.after = match action {
            TurnEndAction::TypeCommand {
                then: Then::Continue,
                ..
            } => Some(rule),
            _ => None,
        };
        if is_wall_act(rule) {
            let window = self.timing.window;
            self.wall_acts
                .retain(|&(t, _)| now.saturating_duration_since(t) < window);
            self.wall_acts.push_back((now, rule));
            while self.wall_acts.len() > 4 * WALL_ACTS_PER_WINDOW {
                self.wall_acts.pop_front();
            }
        }
        if (is_wall_act(rule) || in_limits_place)
            && let Some(t) = &mut self.wall
        {
            t.attempts += 1;
            if rule == RULE_API_BACK {
                t.ups += 1;
            }
            if rule == RULE_LOGIN {
                t.login.typed_at = Some(now);
            }
        }
        if rule == RULE_LOGIN_BACK
            && let Some(t) = self.wall.as_mut().filter(|t| t.class == WallClass::Auth)
        {
            t.login.back_typed = true;
        }
        // The limit's try — the continuation, or an act in its place — and
        // the track it left, which its ledger row carries for a loop that
        // starts after it ([`Self::limit_try`]; any other act is none, from
        // the top).
        if rule == RULE_LIMIT_RESUME || in_limits_place {
            self.limit_try = self
                .wall
                .as_ref()
                .filter(|t| named_class(t.class))
                .map(|t| LimitTry {
                    bucket: t.class == WallClass::Model,
                    in_place: in_limits_place,
                    attempts: t.attempts,
                    entitlement: t.entitlement,
                });
        }
    }

    /// THE AGENT'S PROCESS STARTED at `at` (this policy's clock), as the loop
    /// reads it from its host's snapshot before it folds a point in
    /// (`IdleHost::agent_started`; KI-1015-1 W4 and its relaunch variant).
    /// Claude Code's own countdown (`continuing automatically at …`) belongs
    /// to the process that drew it: a relaunch starts a process that holds
    /// none, though it redraws the notice from its conversation's record. So
    /// a limit first read before `at` goes on by NOBODY — its reset plus the
    /// grace and the jitter, not the vendor's `auto_resume_grace` — whether
    /// its notice shows again or has left the screen. The screen says as
    /// much where it can (the countdown's record with no countdown drawn,
    /// [`TurnEndReading::resumes_by_itself`]; a countdown dropped where the
    /// notice left, [`Self::observe`]): the process's start is the same rule
    /// read where the redrawn screen cannot tell. With no start read nothing
    /// changes here. A `/resume` or `/clear` in the SAME process changes no
    /// start: the screen's read places that one (W4 of ATERM-KI-1015-1).
    /// So too a MODEL's usage credits ([`CreditsEpisode::model`]): drawn once
    /// a process, an episode begun before `at` is the gone process's.
    pub fn agent_started(&mut self, at: Instant) {
        self.agent_start = Some(at);
        self.drop_gone_countdown();
        if self
            .credits
            .as_ref()
            .is_some_and(|c| c.model.is_some() && c.since.is_some_and(|since| since < at))
        {
            self.credits = None;
        }
    }

    /// THE SAME START IN UNIX SECONDS, told by the loop beside
    /// [`Self::agent_started`]: a limit whose notice row the transcript says
    /// was written before it ([`NamedReset::written`]) is a gone process's
    /// countdown too, though this loop first read it after the start — a cold
    /// restore's first loop has no earlier read (ATERM-KI-1016-1: it waited
    /// the vendor's ten minutes past the reset for a countdown no process
    /// held).
    pub fn agent_started_unix(&mut self, unix: i64) {
        self.agent_start_unix = Some(unix);
        // The first process started since the fallback's run began is the
        // one its relaunch made ([`Self::fallback_made`]).
        if self.fallback_started.is_none() && self.fallback_unix.is_some_and(|made| unix >= made) {
            self.fallback_started = Some(unix);
        }
        self.drop_gone_countdown();
    }

    /// The track's countdown, where its process is gone
    /// ([`Self::named_countdown_gone`]), goes on by nobody.
    fn drop_gone_countdown(&mut self) {
        let gone = self
            .wall
            .as_ref()
            .and_then(|t| t.named)
            .is_some_and(|n| self.named_countdown_gone(&n));
        if gone && let Some(n) = self.wall.as_mut().and_then(|t| t.named.as_mut()) {
            n.by_itself = false;
        }
    }

    /// Whether a limit first read at `first` was read before the agent's
    /// process started ([`Self::agent_started`]): its countdown is gone.
    fn countdown_gone(&self, first: Instant) -> bool {
        self.agent_start.is_some_and(|start| start > first)
    }

    /// Whether a notice row written at `written` (unix seconds) was written
    /// before the agent's process started ([`Self::agent_started_unix`]).
    /// Unknown either way: no.
    fn written_before_start(&self, written: Option<i64>) -> bool {
        written
            .zip(self.agent_start_unix)
            .is_some_and(|(written, start)| written < start)
    }

    /// Whether the countdown of limit `n` is a gone process's: first read, or
    /// its row written, before the agent's process started.
    fn named_countdown_gone(&self, n: &NamedReset) -> bool {
        self.countdown_gone(n.first) || self.written_before_start(n.written)
    }

    /// Records a turn the host typed (upgrade notice, carry-on;
    /// [`RULE_HOST_TURN`]), awaited like this policy's acts. A short answer
    /// leaves the streak and back-off as the worker's turns left them; real
    /// work ends the streak (the carry-on's answer is the worker's work
    /// resumed). `kind` says which: a [`HostTurn::CarryOn`] whose answer says
    /// done, short or long, is a done report; a [`HostTurn::Notice`]'s answer
    /// never is; a [`HostTurn::Check`]'s is the done check's yield, ending the
    /// task on the worker's own word. No budget is spent.
    pub fn host_typed(&mut self, now: Instant, kind: HostTurn) {
        self.fallback_unasked = false;
        self.checking = false;
        self.seeded_check = None;
        self.open.clear();
        self.repo = None;
        // A land act's answer the host's line took the place of is no done
        // reply to it: a spent episode ends quiet here, as at its point.
        if (self.landing || self.cut_land) && self.land_spent() {
            self.land.quiet = true;
        }
        self.landing = false;
        self.cut_check = false;
        self.cut_land = false;
        self.cut_text = None;
        self.compact_owes = None;
        // A turn the host typed after an account move (an upgrade's notice:
        // the move's own relaunch types nothing) is a turn asked under the
        // new login ([`Self::account_answer`]), and pays the wall's resume.
        // The process that took it has drawn its start-up: the work after it
        // is that turn's ([`Self::account_startup`]).
        self.account_asked |= self.account.is_some();
        self.account_startup = false;
        self.account_resume = false;
        self.reset_resume = None;
        self.awaiting = Some(Awaited {
            rule: RULE_HOST_TURN,
            command: false,
            at: now,
            typed: now,
            unseen_since: None,
            taken: false,
            met: false,
            left: None,
            host: Some(kind),
            off_stood: false,
            wall_retry: false,
            said: None,
            over: None,
        });
    }
}

impl TurnEndState {
    /// Record a restart the loop asked its host for
    /// ([`TurnEndAction::Restart`]): MADE (`made`, the new process the
    /// loop's) — a bucket's fallback is the switch to undo at the reset
    /// ([`Self::model_switch`], back at the reset the notice named, else
    /// after the longest limit back-off), the reset's relaunch undoes it,
    /// and the bucket's track counts the act; or NOT made (the host refused
    /// it, or none relaunches here) — a bucket's reset is waited out
    /// instead, and a switch that cannot be undone is let go (it was
    /// session-only: nothing else moved). The memory banner's leaves nothing
    /// to remember.
    ///
    /// AN ACCOUNT MOVE made ([`Restart::Account`]) is remembered until it is
    /// answered ([`Self::account_answer`]) and counted
    /// ([`ACCOUNT_MOVES_PER_WINDOW`]); the wall it left is the OLD account's,
    /// so its track ends with it (a limit the new account answers with is a
    /// new wall, its ladder from the start). It also CLEARS AN OPEN MODEL
    /// SWITCH: that switch's return was owed at the old account's bucket's
    /// reset, which says nothing about the account the session runs on now —
    /// kept, it would relaunch the session again at an instant that no
    /// longer means anything. The relaunch itself comes back on the model
    /// the fallback replaced (`crate::harness::relaunch::model_restart`), so
    /// no return is owed at all. MADE AT A WALL it owes that wall's resume
    /// ([`Self::account_resume_owed`]): the continuation the wall would have
    /// had at its reset, at the relaunched session's first point — unless it
    /// was made over a move of the session's own with no turn asked under
    /// that one, which owes what that move owed. MADE AHEAD of one it owes
    /// nothing: the relaunch types nothing, and its first point is decided as
    /// with no move (a lost login there excepted: the new process's own, as
    /// [`Self::login_left`] says). NOT made — refused, or failed — it is not
    /// asked again while the wall stands ([`Self::account_refused`]); one
    /// asked ahead of a wall is not asked again for that account within the
    /// window ([`Self::account_ahead_refused`]).
    ///
    /// ANY OTHER RELAUNCH MADE WHILE A MOVE STANDS (the memory banner's, a
    /// model's fallback or its return) brings up a process of its own under
    /// the login the move chose, and the first point after it is that
    /// process's start-up ([`Self::account_startup`]): its work answers
    /// nothing of the move, told or not.
    pub fn restarted(&mut self, why: &Restart, made: bool, reading: &TurnEndReading, now: Instant) {
        if made && !matches!(why, Restart::Account { .. }) {
            self.account_startup = self.account.is_some();
        }
        match why {
            Restart::Memory => {}
            Restart::Model { to } if made => {
                // Back at the reset the notice named, and only a reset still
                // AHEAD. A bucket that
                // names none — `requires usage credits` is an entitlement,
                // not a window — is not lifted by any clock: the session
                // stays on the fallback (session-only: the owner's default is
                // untouched), where a guessed half hour sent it back to the
                // bucket, into the same wall, and out again — two relaunches
                // an hour, 240 in the five days the account's Fable wall
                // stood. A reset already past that the wall still stands
                // under is no reset either: back at it would be back at
                // once, on every turn end. The bucket is the one its track
                // keeps where the point's screen names none — a relaunch
                // decided where its notice had left ([`still_shown`]): the
                // loop's reading of that screen names no model and no reset.
                let track = self.wall.as_ref().filter(|t| t.class == WallClass::Model);
                // A relaunch off a MODEL's usage credits ([`credits_hold`])
                // goes back at the reset their notice named, to that model;
                // the relaunched process is decided by its own screen.
                let credits = self
                    .credits
                    .take_if(|c| c.model.is_some())
                    .filter(|_| track.is_none());
                let back_at = reading
                    .reset_at
                    .or_else(|| track.and_then(|t| t.named).map(|n| n.at))
                    .or_else(|| credits.as_ref().and_then(|c| c.reset))
                    .filter(|&r| r > now);
                let from = bucket_model(&reading.wall_message)
                    .or_else(|| track.and_then(|t| bucket_model(&t.said)))
                    .or_else(|| credits.and_then(|c| c.model));
                self.model = Some(ModelSwitch {
                    from,
                    to: to.clone(),
                    back_at,
                });
                // The relaunched process redraws the bucket's notice: no
                // wall of the fallback's ([`Self::bucket_left`]).
                self.fallback_unasked = true;
                // The session runs on the fallback now: the bucket's reset
                // binds nothing it runs ([`named_class`]), and a point its
                // notice has left is no wait on it.
                if let Some(t) = &mut self.wall {
                    t.attempts += 1;
                    if t.class == WallClass::Model {
                        t.named = None;
                    }
                }
            }
            Restart::Model { .. } => self.fallback_unmade = true,
            Restart::ModelBack { .. } => {
                self.model = None;
                self.fallback_unasked = false;
            }
            Restart::Account { to, at } if made => {
                // A move made over this session's own move still unanswered,
                // with no turn asked under it (the host moved the session on
                // from a limit repainted on the relaunch's first screen),
                // owes what that move owed and no more: no turn was cut short
                // — a chain begun ahead of a wall types what the first move
                // would have.
                let owed = match self.account {
                    Some(_) if !self.account_asked => self.account_resume,
                    _ => true,
                };
                self.account = Some(AccountMove {
                    to: to.clone(),
                    at: now,
                    wall: *at == AccountAt::Wall,
                });
                self.account_asked = false;
                self.account_told = false;
                self.account_startup = true;
                self.account_resume = *at == AccountAt::Wall && owed;
                self.account_unmade = false;
                self.account_ahead_unmade = None;
                let window = self.timing.window;
                self.account_moves
                    .retain(|&t| now.saturating_duration_since(t) < window);
                self.account_moves.push_back(now);
                while self.account_moves.len() > 4 * ACCOUNT_MOVES_PER_WINDOW {
                    self.account_moves.pop_front();
                }
                self.wall = None;
                self.model = None;
                self.fallback_unmade = false;
                self.fallback_unasked = false;
                // Credits carried the session on the account it left: the
                // relaunched one is decided by what its own screen says.
                self.credits = None;
            }
            Restart::Account {
                at: AccountAt::Wall,
                ..
            } => self.account_unmade = true,
            Restart::Account {
                to,
                at: AccountAt::Ahead,
            } => self.account_ahead_unmade = Some((to.clone(), now)),
        }
    }
}

/// The `/model` alias a bucket's notice names (`You've reached your Fable
/// limit` → `fable`, `Opus 4.1 limit reached` → `opus`): the first word that
/// is one of [`MODEL_ALIASES`].
#[must_use]
pub fn bucket_model(message: &str) -> Option<String> {
    message
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .find(|w| MODEL_ALIASES.contains(&w.as_str()))
}

/// The work a finished turn's done row reports (`✻ Cooked for 3m 2s · done
/// 4:24 PM` → 182 s): Claude Code's own measure, which a supervisor that
/// attached mid-turn saw only part of. Read on the row's LINE
/// ([`status_line`]): a narrow pane wraps the time itself (`✻ Crunched for
/// 1h 9m` over `  27s · done 11:07 AM`).
#[must_use]
pub fn done_row_work(rows: &[String]) -> Option<Duration> {
    let row = status_line(rows).filter(|r| is_done_row(r))?;
    let (_, rest) = row.split_once(" for ")?;
    let span = rest.split(" · ").next()?.trim();
    let mut secs = 0u64;
    for word in span.split_whitespace() {
        let unit = word.chars().last()?;
        let n: u64 = word[..word.len() - unit.len_utf8()].parse().ok()?;
        secs += n * match unit {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
    }
    Some(Duration::from_secs(secs))
}

/// How far a done row may stand off the act whose turn it closes
/// ([`act_turn_work`]): the minute its clock names, the seconds a span past
/// an hour drops (`2h 58m`), and the act's Enter to the turn's start.
pub const ACT_TURN_SLACK: Duration = Duration::from_secs(2 * 60);

/// THE WORK OF THE TURN AN ACT BEGAN, where no read saw that turn run
/// (ATERM-KI-1016-1, "a long unwatched gap": no loop for minutes after the
/// act — a handoff with no loop in the gap — while the worker works; the
/// act's time the wall clock's, a sleep since it set back,
/// [`TurnEndState::untaken_act_typed`]): the done row
/// Claude Code drew at the end of a turn (`work`, [`done_row_work`];
/// `ended`, the start of its clock's minute, unix seconds) is that turn's
/// where the turn ended after the act (`act`, unix seconds) and began at it,
/// its span reaching back to the act within [`ACT_TURN_SLACK`] — never the
/// turn the act was typed after (it ended before the act), nor a person's
/// begun later. Capped at the time since the act, and LESS THE SLEEP of the
/// machine inside the turn's span (`asleep`, as the loop knows its sleeps:
/// `policy::sleeps::SleepBook::in_turn`): the span is the wall clock's,
/// which ran on through it. Work under `min` reads as none, the point as no
/// read saw it run. Read as a point no read saw run,
/// a limit's try whose own turn worked for an hour while no loop watched
/// left the try's rung standing from the reader's first look — two hours on
/// a spend limit where the loop that watched it started the ladder over at
/// that turn's end, 30 minutes.
#[must_use]
pub fn act_turn_work(
    act: i64,
    ended: i64,
    work: Duration,
    asleep: Duration,
    min: Duration,
) -> Option<Duration> {
    if ended <= act || work < min {
        return None;
    }
    let slack = i128::from(ACT_TURN_SLACK.as_secs());
    let span = i128::from(i64::try_from(work.as_secs()).ok()?);
    // Widen before subtracting signed dates or adding the minute/slack:
    // every public i64 date is admitted without overflowing intermediates.
    // The turn ended between the start of its clock's minute and that
    // minute's end, so this long after the act:
    let least = i128::from(ended) - i128::from(act);
    let most = least + 59;
    if span + slack < least || span > most + slack {
        return None;
    }
    let most = u64::try_from(most).unwrap_or(u64::MAX);
    let awake = work.min(Duration::from_secs(most)).checked_sub(asleep)?;
    (awake >= min).then_some(awake)
}

/// The clock time of a turn's DONE ROW (`✻ Sautéed for 1d 2h
/// 58m · done 12:19 PM · 1 shell still running` → `12:19 PM`): Claude Code's
/// own stamp of when the turn ended — for a turn that ended on a wall, when
/// the wall was drawn. `None` without a done row on the screen. Read on the
/// row's LINE ([`status_line`]): at 26 columns the clock is the row under it
/// (`✻ Worked for 0s · done` over `  10:36 AM`, measured on 2.1.299).
#[must_use]
pub fn done_row_clock(rows: &[String]) -> Option<String> {
    let row = status_line(rows).filter(|r| is_done_row(r))?;
    let (_, rest) = row.split_once(" done ")?;
    let clock = rest.split(" · ").next()?.trim();
    (clock.starts_with(|c: char| c.is_ascii_digit())).then(|| clock.to_string())
}

/// Whether a suggestion is a continuation: made only of
/// [`SUGGESTION_ALLOW`] pieces, case and closing punctuation aside.
#[must_use]
pub fn suggestion_allowed(text: &str) -> bool {
    let lower = text
        .to_lowercase()
        .replace(" and ", ",")
        .replace(" then ", ",");
    let mut any = false;
    for piece in lower.split([',', ';', '.', '!']) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        if !SUGGESTION_ALLOW.contains(&piece) {
            return false;
        }
        any = true;
    }
    any
}

/// What the worker's last words ask of a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// A report, or nothing: the point may be continued.
    Plain,
    /// An offer to go on (`want me to take that on?`): continued.
    Offer,
    /// A question for a person: answered.
    Question,
    /// A decision asked for: answered, with why.
    Stop(String),
    /// A decision that names an irreversible act ([`destructive_word`]) —
    /// an offer to do it, a question or a choice that names it anywhere:
    /// answered only with [`REVERSIBLE_ANSWER`], with why.
    Irreversible(String),
}

/// What the words `tail` ask of a person and whether they name an
/// irreversible act ([`classify_said`]): the worker's last words
/// ([`TurnEndReading::asked`]), or its words since a limit ([`limit_end`]).
/// `None` for a report or an offer.
fn asked_by(tail: Option<&str>) -> Option<(String, bool)> {
    match classify_said(tail) {
        Said::Irreversible(why) => Some((why, true)),
        Said::Stop(why) => Some((why, false)),
        Said::Question => Some(("the worker asked a question".to_string(), false)),
        Said::Offer | Said::Plain => None,
    }
}

/// Judges the worker's last words ([`Said`]). `tail` is one line per screen
/// row ([`aterm_phase::said_tail`]), so phrases are matched on the rows joined
/// with spaces, and the question is the last sentence of the last unit (from
/// the last enumerated option row on), never the last row. In order:
///
/// 1. A stop phrase anywhere is a stop; it names the act when a destructive
///    word is anywhere in the tail.
/// 2. An offer with a destructive word after the first offer phrase, or any
///    question naming a destructive act, is a stop.
/// 3. A last sentence that asks (`?`, or a trailing `(y/n)`, `[y/N]`,
///    `(yes/no)`, ignoring a trailing aside): a choice between two or more
///    listed options under an ask that chooses ([`asks_to_choose`]), an
///    either/or the worker would act on ([`either_or`]), or a
///    [`CHOICE_PHRASES`] ask ([`phrase_asks`]) is a stop; any destructive word
///    ([`destructive_word`], inflections included) in a choice's tail names
///    the act. An offer stays an offer, anything else is a question.
/// 4. A tail that does not ask is an offer when it opens next steps, else
///    plain.
///
/// Every stop and question is answered ([`RULE_ANSWER`]); one naming an
/// irreversible act ([`Said::Irreversible`]) only with [`REVERSIBLE_ANSWER`].
/// The reason is the escalation's text under `answer_questions = false`.
///
/// Remembered for the last tail read on this thread (`LastTail`).
#[must_use]
pub fn classify_said(tail: Option<&str>) -> Said {
    let Some(tail) = tail else {
        return Said::Plain;
    };
    thread_local! {
        static LAST: LastTail<Said> = const { LastTail::new() };
    }
    LAST.with(|last| last.get_or(tail, || classify_said_read(tail)))
}

/// THE LAST TAIL A PURE READING OF THE WORKER'S WORDS WAS MADE OF, and what
/// it read, kept per thread: [`classify_said`] and [`says_done`] are pure
/// functions of the tail, and one point of the loop asks them of the same
/// tail several times (`decide_turn_end` up to three times, the reading's
/// `asked` around it, `observe`'s done check). Each asking is answered
/// exactly as a fresh read would answer it: the read is reused only for the
/// same text, compared whole.
struct LastTail<T>(std::cell::RefCell<Option<(String, T)>>);

impl<T: Clone> LastTail<T> {
    const fn new() -> Self {
        LastTail(std::cell::RefCell::new(None))
    }

    /// What `read` gives for `tail`: the remembered answer when `tail` is the
    /// last tail read, else `read()`, remembered.
    fn get_or(&self, tail: &str, read: impl FnOnce() -> T) -> T {
        if let Some((seen, answer)) = &*self.0.borrow()
            && seen == tail
        {
            return answer.clone();
        }
        let answer = read();
        *self.0.borrow_mut() = Some((tail.to_owned(), answer.clone()));
        answer
    }
}

/// [`classify_said`] on a tail, read afresh.
fn classify_said_read(tail: &str) -> Said {
    let lower = yes_no_asks(&tail.to_lowercase().replace('’', "'"));
    let flat = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(p) = STOP_PHRASES.iter().find(|p| flat.contains(*p)) {
        // A stop naming a destructive act anywhere asks consent to it; its
        // answer must consent to nothing irreversible.
        if let Some(w) = destructive_word(&flat) {
            return Said::Irreversible(format!("the worker said \"{p}\" of an act that would {w}"));
        }
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    let recommends = RECOMMEND_PHRASES.iter().any(|p| flat.contains(p));
    let offer_at = OFFER_PHRASES.iter().filter_map(|p| flat.find(p)).min();
    if let Some(at) = offer_at
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Irreversible(format!("the worker offers to {w}"));
    }
    if let Some(w) = questions(&flat).into_iter().find_map(destructive_word) {
        return Said::Irreversible(format!("the worker asks whether to {w}"));
    }
    let offer = offer_at.is_some();
    let lines: Vec<&str> = lower
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let unit_start = lines.iter().rposition(|l| is_option_line(l)).unwrap_or(0);
    let unit = lines[unit_start..].join(" ");
    let unit = without_trailing_aside(&unit).trim_end_matches([')', '"', '*', '`', ' ']);
    let asks = unit.ends_with('?');
    let listed = lines.iter().filter(|l| is_option_line(l)).count();
    let ask = last_sentence(unit);
    // A choice between options: two or more listed and a choosing ask, or an
    // either/or the worker would act on.
    let listed_choice = listed >= 2 && asks_to_choose(ask);
    let chooses = asks && (listed_choice || either_or(ask));
    let choice_phrase = CHOICE_PHRASES
        .iter()
        .filter_map(|p| flat.find(p).map(|at| (*p, at)))
        .min_by_key(|&(_, at)| at);
    let phrase_choice = choice_phrase.filter(|&(p, at)| phrase_asks(&flat, &lines, p, at));
    // A choice naming a destructive act anywhere in the tail (option,
    // description row, ask, preceding report): its answer consents to it.
    if (chooses || phrase_choice.is_some())
        && let Some(w) = destructive_word(&flat)
    {
        return Said::Irreversible(format!("the worker offers a choice that would {w}"));
    }
    if chooses && !recommends {
        return Said::Stop(if listed_choice {
            "the worker listed options with no recommendation".to_string()
        } else {
            "the worker asks to choose between options".to_string()
        });
    }
    if let Some((p, _)) = phrase_choice {
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    // A choice phrase that does not ask (a report that says `which option`)
    // is no choice — but an act named after it is still a person's.
    if let Some((_, at)) = choice_phrase
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Irreversible(format!("the worker asks to choose whether to {w}"));
    }
    if !asks {
        return if offer { Said::Offer } else { Said::Plain };
    }
    if offer { Said::Offer } else { Said::Question }
}

/// The yes/no markers a worker closes a question with, lowercased.
const YES_NO_MARKERS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "(y/n)?", "[y/n]?"];

/// `text` (lowercased) with each yes/no marker ([`YES_NO_MARKERS`]) read as
/// the `?` it means: `delete them now? (y/n)` and `proceed (y/n)` both ask.
fn yes_no_asks(text: &str) -> String {
    let mut out = text.to_string();
    for m in YES_NO_MARKERS {
        out = out.replace(m, "?");
    }
    out
}

/// Every sentence of `flat` that asks — ends in a `?` followed by a space or
/// the end — whole, from the last sentence end before it.
fn questions(flat: &str) -> Vec<&str> {
    let bytes = flat.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate() {
        let ends = matches!(bytes.get(i + 1), None | Some(b' '));
        if !ends {
            continue;
        }
        match b {
            b'?' => {
                out.push(flat[start..=i].trim());
                start = i + 1;
            }
            b'.' | b'!' | b';' | b':' => start = i + 1,
            _ => {}
        }
    }
    out
}

/// `unit` without a parenthetical aside after its last sentence
/// (`Should I rename it? (I have not touched it yet.)` asks).
fn without_trailing_aside(unit: &str) -> &str {
    let mut t = unit.trim_end();
    while t.ends_with(')') {
        let Some(open) = t.rfind('(') else {
            break;
        };
        let before = t[..open].trim_end();
        if !before.ends_with(['?', '.', '!']) {
            break;
        }
        t = before;
    }
    t
}

/// The last sentence of `text`: what follows the last `.`, `!`, `?`, `:` or
/// `;` that is followed by a space (so `v1.2` and `a.rs` do not end one).
fn last_sentence(text: &str) -> &str {
    let body = text.trim_end_matches('?');
    let mut start = 0;
    let bytes = body.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?' | b':' | b';') && bytes.get(i + 1) == Some(&b' ') {
            start = i + 1;
        }
    }
    text[start..].trim()
}

/// Leads that, before an either/or's ` or `, make it a choice the worker
/// would act on (`should I … or …?`, `which would you prefer: … or …?`).
/// Without one (`did the suite pass on your machine, or should I rerun it?`)
/// it asks something only the person knows.
pub const CHOICE_LEADS: &[&str] = &[
    "should i",
    "shall i",
    "should we",
    "shall we",
    "want me to",
    "do you want",
    "would you like",
    "prefer",
    "which",
];

/// Whether the asking sentence `ask` (lowercased, its `?` trimmed) is an
/// either/or the worker would act on ([`CHOICE_LEADS`] before its ` or `).
fn either_or(ask: &str) -> bool {
    ask.find(" or ")
        .is_some_and(|at| CHOICE_LEADS.iter().any(|l| ask[..at].contains(l)))
}

/// Whether the asking sentence under a list of options asks to choose one
/// (`which one?`, `shall I start?`, `1 or 2?`) rather than something else
/// (`does that look right?`).
fn asks_to_choose(ask: &str) -> bool {
    let words: Vec<&str> = ask
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    either_or(ask)
        || CHOICE_LEADS.iter().any(|l| ask.contains(l))
        || words.iter().any(|w| {
            matches!(
                *w,
                "which" | "option" | "options" | "choose" | "pick" | "either" | "or"
            )
        })
}

/// Whether a [`CHOICE_PHRASES`] ask `p` at `at` in `flat` asks: its sentence
/// ends `?`, it is in the tail's last sentence, or its row introduces the
/// enumerated options. A phrase in an earlier sentence of a report does not.
fn phrase_asks(flat: &str, lines: &[&str], p: &str, at: usize) -> bool {
    let rest = &flat[at..];
    let bytes = rest.as_bytes();
    // The first sentence end after the phrase: `.`, `!` or `?` then a space
    // or the end; a `.` after a digit (`1. drop`) is an option number.
    let end = bytes.iter().enumerate().position(|(i, b)| {
        matches!(b, b'.' | b'!' | b'?')
            && matches!(bytes.get(i + 1), None | Some(b' '))
            && !(*b == b'.' && i > 0 && bytes[i - 1].is_ascii_digit())
    });
    let asks_here = end.is_none_or(|i| bytes[i] == b'?' || i + 1 == bytes.len());
    let introduces = lines
        .iter()
        .position(|l| l.contains(p))
        .is_some_and(|k| lines.get(k + 1).is_some_and(|l| is_option_line(l)));
    asks_here || introduces
}

/// The first of [`DESTRUCTIVE_WORDS`] in `text` as a whole word (or phrase),
/// or an inflection of one ([`inflects`]), returned as its base form.
pub(crate) fn destructive_word(text: &str) -> Option<&'static str> {
    // A short force flag belongs to the push in its clause, not necessarily
    // the next word. Keep its leading dash to distinguish a flag from a ref,
    // and stop at command or sentence separators. This remains a conservative
    // prose classifier, not a shell command parser.
    let short_force_push = text.split([';', '|', '&', '\n', '?', '!']).any(|clause| {
        let mut push = false;
        clause
            .split(|c: char| !(c.is_alphanumeric() || c == '-'))
            .filter(|word| !word.is_empty())
            .any(|word| match word {
                word if inflects(word, "push") => {
                    push = true;
                    false
                }
                _ => {
                    push && word.strip_prefix('-').is_some_and(|flags| {
                        // -o consumes the rest of its cluster as a push-option
                        // value; an f there is not the force flag.
                        !flags.starts_with('-')
                            && flags.bytes().take_while(|&b| b != b'o').any(|b| b == b'f')
                    })
                }
            })
    });
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_start_matches('-'))
        .collect();
    DESTRUCTIVE_WORDS.iter().copied().find(|d| {
        if *d == "push -f" {
            return short_force_push;
        }
        let want: Vec<&str> = d.split(' ').map(|w| w.trim_start_matches('-')).collect();
        words.windows(want.len()).any(|win| {
            win.iter()
                .zip(want.iter())
                .all(|(w, base)| inflects(w, base))
        })
    })
}

/// Whether `word` is `base` or a regular inflection: `-s`/`-es`, `-d`/`-ed`,
/// `-ing` (dropping a final `e`, doubling a final consonant), `-en` after a
/// doubled `t` (`overwritten`), and `overwrote`/`rewrote`. Over-matching a
/// noun only escalates a point that would have been answered; missing a verb
/// answers one a person had to.
fn inflects(word: &str, base: &str) -> bool {
    if word == base {
        return true;
    }
    let Some(rest) = word.strip_prefix(base) else {
        // A final `e` dropped: `delet` + `ing`/`ed`; `overwrit` + `ten`.
        let stem = base.strip_suffix('e').unwrap_or(base);
        return (stem.len() < base.len()
            && word
                .strip_prefix(stem)
                .is_some_and(|rest| matches!(rest, "ing" | "ed" | "ten")))
            || (base.ends_with("write") && word == format!("{}ote", &base[..base.len() - 3]));
    };
    if matches!(rest, "s" | "es" | "d" | "ed" | "ing" | "ten") {
        return true;
    }
    // A final consonant doubled: `drop` + `ped`, `reset` + `ting`.
    let last = base.chars().last().unwrap_or(' ');
    rest.strip_prefix(last)
        .is_some_and(|rest| matches!(rest, "ed" | "ing" | "en"))
}

/// An enumerated option row: `1.`, `2)`, `(a)`, `a)`, `- `, `• `,
/// `option a`/`option 1`.
fn is_option_line(line: &str) -> bool {
    let t = line.trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && t[digits..].starts_with(['.', ')']) {
        return true;
    }
    let mut cs = t.chars();
    if let (Some(a), Some(b)) = (cs.next(), cs.next())
        && a.is_ascii_lowercase()
        && b == ')'
    {
        return true;
    }
    t.starts_with("(a)")
        || t.starts_with("(b)")
        || t.starts_with("- ")
        || t.starts_with("• ")
        || t.starts_with("option ")
}

/// What to type as a continuation: `cfg.continue_text`, with the standing
/// rules when there are any.
#[must_use]
pub fn continuation_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.continue_text, rules)
}

/// What to type where the worker said it is done: [`DONE_CHECK`] — followed
/// by [`DONE_CHECK_REVERSIBLE`] where the report names an `irreversible` act
/// — with the standing rules when there are any.
#[must_use]
pub fn done_check_text(rules: Option<&str>, irreversible: bool) -> String {
    if irreversible {
        with_rules(&format!("{DONE_CHECK} {DONE_CHECK_REVERSIBLE}"), rules)
    } else {
        with_rules(DONE_CHECK, rules)
    }
}

/// Whether `typed` — a prompt's words as a transcript keeps them — is the
/// done check as [`done_check_text`] types it, or the preceding marker-based
/// check: either alone or with the standing rules after it. What a reader of
/// the conversation locates the check by (`harness::result`).
#[must_use]
pub fn is_done_check(typed: &str) -> bool {
    let typed = typed.trim();
    [
        DONE_CHECK,
        DONE_CHECK_ORDINARY_BEFORE,
        DONE_CHECK_MARKER_BEFORE,
    ]
    .iter()
    .any(|check| {
        typed.strip_prefix(*check).is_some_and(|rest| {
            // The builder may put its fixed reversible-only clause before
            // the standing rules. Read that emitted form as the same check.
            let rest = rest
                .strip_prefix(' ')
                .and_then(|tail| tail.strip_prefix(DONE_CHECK_REVERSIBLE))
                .unwrap_or(rest);
            rest.is_empty() || rest.starts_with(RULES_OPEN)
        })
    })
}

/// The challenge [`RULE_OPEN_ITEMS`] types: the words [`open_work`] found,
/// in its own tables' spelling and never the screen's (no act types screen
/// text: the module header's ruling of 2026-10-08), then what a finished
/// report is, as [`DONE_CHECK`] defines it (nothing left, a blocked item
/// included), followed by [`DONE_CHECK_REVERSIBLE`] where the report also
/// names an `irreversible` act (D1: `do each item` alone reads as consent to
/// it), with the standing rules when there are any. At most [`OPEN_NAMED`]
/// of them are named, `…` after the rest. It opens with [`OPEN_ITEMS_HEAD`],
/// by which the conversation's record tells it apart
/// (`crate::harness::upgrade::FinishedScan`, [`is_challenge_text`]).
#[must_use]
pub fn open_items_text(found: &[&str], rules: Option<&str>, irreversible: bool) -> String {
    challenge_text(found, None, false, rules, irreversible).0
}

/// The challenge [`RULE_UNCOMMITTED`] types: what the probe found in the
/// session's repository ([`crate::harness::repo_work::RepoWork::summary`]:
/// its path, its counts, a few of its paths, each printable — the host's
/// facts, never the screen's words), and what a finished job leaves there;
/// always with the words that keep its remedy
/// reversible (a probe's finding is no licence to delete a person's files or
/// a stash), with the standing rules when there are any. It opens with
/// [`UNCOMMITTED_HEAD`] ([`is_challenge_text`]).
#[must_use]
pub fn uncommitted_text(
    repo: &crate::harness::repo_work::RepoWork,
    rules: Option<&str>,
    irreversible: bool,
) -> String {
    challenge_text(&[], Some(repo), false, rules, irreversible).0
}

/// THE CHALLENGE OWED AT A DONE REPORT THAT IS NO FINISH, and its rule:
/// [`RULE_UNCOMMITTED`] where the repository holds work no remote has (the
/// report's open work named after it), else [`RULE_OPEN_ITEMS`]. Under
/// Autopilot Land (`land`) it asks for the step in hand alone and for what
/// is left written down in the repository and committed, never for more
/// work: Land starts nothing new, and its report names nothing left.
fn challenge_text(
    found: &[&str],
    repo: Option<&crate::harness::repo_work::RepoWork>,
    land: bool,
    rules: Option<&str>,
    irreversible: bool,
) -> (String, &'static str) {
    let named = match (found.get(..OPEN_NAMED), found) {
        (_, []) => String::new(),
        (Some(first), _) if found.len() > OPEN_NAMED => format!(" ({}, …)", first.join(", ")),
        _ => format!(" ({})", found.join(", ")),
    };
    let finished = "Say the task is complete only when nothing is uncommitted, untracked, \
                    stashed or unpushed and your report names no open item, follow-up or next \
                    step.";
    let work = if land {
        "Autopilot is set to Land: finish only the step in hand and start nothing new, and \
         write each item left down in the repository; commit and push it all"
    } else {
        "Do each item now and verify it, then commit and push it"
    };
    let (mut text, rule) = match repo {
        Some(repo) => {
            let open = if found.is_empty() {
                String::new()
            } else {
                format!(" Your report also names open work{named}: {work}.")
            };
            (
                format!(
                    "{UNCOMMITTED_HEAD}: {}. A done report ends nothing while any is left. \
                     Commit and push it (merged to main where this repository lands work), a \
                     stash's work included; delete, overwrite and force-push nothing to get \
                     there.{open} {finished}",
                    repo.summary()
                ),
                RULE_UNCOMMITTED,
            )
        }
        None => (
            format!(
                "{OPEN_ITEMS_HEAD}{named}: a done report ends nothing while any is left, a \
                 blocked item included. {work} (merged to main where this repository lands \
                 work). {finished}"
            ),
            RULE_OPEN_ITEMS,
        ),
    };
    if irreversible {
        text = format!("{text} {DONE_CHECK_REVERSIBLE}");
    }
    (with_rules(&text, rules), rule)
}

/// The fixed words the open-items challenge opens with ([`open_items_text`]),
/// whatever it names after them: what the conversation's record knows the
/// challenge by (`crate::harness::upgrade::FinishedScan`), as it knows the
/// done check by [`DONE_CHECK`]'s.
pub const OPEN_ITEMS_HEAD: &str = "Your report says done but still names open work";

/// The open-items challenge's opening words before the owner's rule of
/// 2026-10-19 (once a job, a blocked item exempt), which the records of
/// conversations challenged then still hold ([`is_challenge_text`]): the
/// token's, and from `69a8f68178` ordinary language's.
pub const OPEN_ITEMS_HEADS_BEFORE: [&str; 2] = [
    "Your report ends with ATERM-TASK-DONE but still names open",
    "Your report still names open",
];

/// The fixed words the uncommitted-work challenge opens with
/// ([`uncommitted_text`]), whatever it names after them.
pub const UNCOMMITTED_HEAD: &str = "Your report says done but work is not committed and pushed";

/// Whether `typed` is one of the challenges ([`is_challenge_rule`]), by its
/// opening words — the current heads and the ones before them: what the
/// conversation's record and the ledger's typed rows know it by. Word by
/// word, whatever the row breaks between them, and a word's own `:` or `,`
/// after it (`… committed and pushed: /r`) is no other word.
#[must_use]
pub fn is_challenge_text(typed: &str) -> bool {
    [OPEN_ITEMS_HEAD, UNCOMMITTED_HEAD]
        .iter()
        .chain(OPEN_ITEMS_HEADS_BEFORE.iter())
        .any(|head| {
            let mut words = typed
                .split_whitespace()
                .map(|w| w.trim_end_matches([':', ',']));
            head.split_whitespace().all(|w| words.next() == Some(w))
        })
}

/// The most of [`open_work`]'s words the challenge names
/// ([`open_items_text`]): its text stays a line the composer holds as typed
/// ([`VERBATIM_UNITS`]) however many heads a report uses.
const OPEN_NAMED: usize = 4;

/// What to type as an answer to a question for a person: `cfg.answer_text`,
/// with the standing rules when there are any.
#[must_use]
pub fn answer_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.answer_text, rules)
}

/// The answer to a prose question under `cfg.question_priority`:
/// [`answer_text`] under [`QuestionPriority::Recommended`], else that
/// priority's policy text (the same a question dialog's free-text row gets),
/// with the standing rules when there are any. Never for an irreversible act
/// ([`reversible_answer`]).
#[must_use]
pub fn prose_answer(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    match cfg.question_priority.text() {
        Some(text) => with_rules(text, rules),
        None => answer_text(cfg, rules),
    }
}

/// The answer to a question or choice naming an irreversible act
/// ([`Said::Irreversible`]): never "take the option you would recommend" (the
/// recommendation may be the act), and never `answer_text`, which may say so.
pub const REVERSIBLE_ANSWER: &str = "Nobody is here to approve an irreversible step: take the \
                                     option that deletes, overwrites and force-pushes nothing, \
                                     and keep going.";

/// [`REVERSIBLE_ANSWER`], with the standing rules when there are any.
#[must_use]
pub fn reversible_answer(rules: Option<&str>) -> String {
    with_rules(REVERSIBLE_ANSWER, rules)
}

/// The most UTF-16 code units a text this policy types may hold and still be
/// drawn as typed: Claude Code draws a longer one as `[Pasted text #N]`
/// (measured: `docs/measured/claude-code-dialogs-2026-09-24.md`, "Paste
/// collapse"; Codex collapses only longer ones), which neither the fenced
/// write's read-back nor the guarded submit's row guard can match, so the act
/// would be escalated with its text left typed.
pub const VERBATIM_UNITS: usize = 800;

/// `text`, then the standing rules when there are any. The rules give way to
/// the text: cut at a word, `…` after, so the whole stays within
/// [`VERBATIM_UNITS`] (the loop has already cut them to `RULES_CAP`
/// characters, which a long text such as [`DONE_CHECK`] would push past it),
/// and left off where not one word of them fits.
fn with_rules(text: &str, rules: Option<&str>) -> String {
    let Some(r) = rules.map(str::trim).filter(|r| !r.is_empty()) else {
        return text.to_string();
    };
    let frame = text.encode_utf16().count() + RULES_OPEN.len() + ")".len();
    let r = fit_units(r, VERBATIM_UNITS.saturating_sub(frame));
    if r.is_empty() {
        return text.to_string();
    }
    format!("{text}{RULES_OPEN}{r})")
}

/// `s` whole when it fits in `room` UTF-16 code units; else cut after its
/// last whole word that fits with a `…` after it; empty when none does.
fn fit_units(s: &str, room: usize) -> Cow<'_, str> {
    if s.encode_utf16().count() <= room {
        return Cow::Borrowed(s);
    }
    let budget = room.saturating_sub('…'.len_utf16());
    let mut units = 0;
    let mut end = 0;
    for (at, c) in s.char_indices() {
        units += c.len_utf16();
        if units > budget {
            break;
        }
        end = at + c.len_utf8();
    }
    let cut = if s[end..].starts_with(char::is_whitespace) {
        end
    } else {
        s[..end].rfind(char::is_whitespace).unwrap_or(0)
    };
    match s[..cut].trim_end() {
        "" => Cow::Borrowed(""),
        kept => Cow::Owned(format!("{kept}…")),
    }
}

/// How the standing rules open after a typed text ([`with_rules`]).
const RULES_OPEN: &str = " (standing rules: ";

/// `at + d`, saturating at `at` when the sum overflows the clock.
fn later(at: Instant, d: Duration) -> Instant {
    at.checked_add(d).unwrap_or(at)
}

/// The decision at one point (module header). Pure: the same state, reading,
/// policy and clock give the same action.
#[must_use]
pub fn decide_turn_end(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    decide_turn_end_traced(state, reading, cfg, now, &mut NoTrace)
}

/// `action`, decided at `check`, written to `trace` (named by its rule where
/// it has one) and handed back ([`decide_turn_end_traced`]).
fn ended_at(trace: &mut dyn Trace, check: &'static str, action: TurnEndAction) -> TurnEndAction {
    note(trace, check, action.rule_id(), || {
        Outcome::Decided(format!("{action:?}"))
    });
    action
}

/// [`decide_turn_end`], each check it consults written to `trace` as it is
/// consulted ([`super::trace`]): the gates that keep the harness's hands off
/// a point (the autopilot off, a person's grace, an act still to be taken,
/// no task), the reading it decides again on (a notice the session left),
/// the switch that owns a point, and the act at a free point
/// ([`act_at_point`]) by its own branches — each named by the rule that
/// decided where one did. The same function: `decide_turn_end` is this one
/// with no trace. A step's outcome is the action's `Debug`, its `Instant`s
/// this process's clock. Diagnostic callers supply the reconstructed state
/// and clock; no CLI trace reader is wired here.
#[must_use]
pub fn decide_turn_end_traced(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    trace: &mut dyn Trace,
) -> TurnEndAction {
    // AUTOPILOT OFF: the harness keeps its hands off the session — plain
    // Claude Code. Nothing typed, waited for, restarted or escalated,
    // whatever the screen shows: a wall, a memory banner, an owed act or
    // an open switch included.
    if reading.autopilot == Autopilot::Off {
        return ended_at(trace, "autopilot-off", TurnEndAction::Nothing);
    }
    // A LOST LOGIN THE SESSION LEFT ([`TurnEndState::login_left`]) with no
    // resume owed is no wall of this process's: the point is decided as the
    // plain turn end it would be with no such notice — never `/login`, and
    // nothing the host is asked. Where the resume is owed, the wall's arm
    // types it ([`wall_action`]).
    if state.login_left(reading) && !(state.account_resume && cfg.resume_limits) {
        let plain = TurnEndReading {
            wall: None,
            wall_message: String::new(),
            ..reading.clone()
        };
        note(trace, "login-left", None, || Outcome::Passed);
        return decide_turn_end_traced(state, &plain, cfg, now, trace);
    }
    // A BUCKET THE SESSION LEFT ([`TurnEndState::bucket_left`]): the
    // relaunched process's redraw of the notice of the model it no longer
    // runs. Decided as the plain turn end it would be with no such notice.
    if state.bucket_left(reading) {
        let plain = TurnEndReading {
            wall: None,
            wall_message: String::new(),
            ..reading.clone()
        };
        note(trace, "bucket-left", None, || Outcome::Passed);
        return decide_turn_end_traced(state, &plain, cfg, now, trace);
    }
    // A point the live upgrade owns is the upgrade's, except at a wall: the
    // upgrade types nothing there and its host takes no step
    // (`host_steps_here`), so every wall is decided by its own rule, with no
    // relaunch over the upgrade's own (a model bucket is waited out). The
    // memory banner keeps its restart: the upgrade needs a READY an agent
    // past reading never gives. A continuation a wall's act owes
    // (`/compact`'s `then: Continue`) is paid here too, owned or not, since
    // an owed act also keeps the host out.
    if reading.upgrading {
        match reading.wall {
            None if state.after.is_none() => {
                return ended_at(trace, "upgrading", TurnEndAction::Nothing);
            }
            None => {}
            Some(WallKind::Memory) => {}
            Some(_) if reading.restartable => {
                let unrestartable = TurnEndReading {
                    restartable: false,
                    ..reading.clone()
                };
                note(trace, "upgrading", None, || Outcome::Passed);
                return decide_turn_end_traced(state, &unrestartable, cfg, now, trace);
            }
            Some(_) => {}
        }
    }
    // Claude Code's critical-memory banner: nothing is typed at it, so what
    // holds a typed act back (draft, unanswered message, survey, awaited
    // act, spinner) does not hold its restart; the host's restart checks
    // idle and nobody's hand itself. A turn a person stopped still gets
    // their grace.
    if reading.wall == Some(WallKind::Memory) && !reading.interrupted {
        return ended_at(trace, "memory", memory_restart(cfg, reading));
    }
    // An act typed and its point not judged yet ([`TurnEndState::observe`],
    // which judges it at its deadline — taken or not): decided again then.
    if let Some(a) = &state.awaiting {
        if !reading.authoritative
            || reading.survey
            || matches!(reading.phase, Phase::Busy | Phase::Prompt)
        {
            return ended_at(trace, "awaiting", TurnEndAction::Nothing);
        }
        let due = a.due(&state.timing);
        if now < due {
            return ended_at(
                trace,
                "awaiting",
                TurnEndAction::WaitUntil {
                    until: due,
                    why: format!("the worker to take the {} act", a.rule),
                    wait: WaitKind::Other,
                },
            );
        }
        note(trace, "awaiting", None, || Outcome::Passed);
    }
    // CLAUDE CODE'S `/login` DIALOG STANDING ([`super::login::dialog_act`]):
    // its success state gets its one Enter, past a person's grace (they may
    // be pressing it themselves); every other state nothing, the owner told
    // by the login's exception ([`super::exception::exception_for`]). Ahead
    // of the task's gate: a dialog stands whoever opened it, done task or
    // not.
    if super::login::carried(reading.program)
        && let Some(screen) = &reading.login_screen
    {
        let act = super::login::dialog_act(state, screen);
        if act.rule_id().is_some()
            && let Some((until, why)) = carry_held(state, reading, cfg, now)
        {
            return ended_at(
                trace,
                "login-dialog",
                TurnEndAction::WaitUntil {
                    until,
                    why: why.to_string(),
                    wait: WaitKind::Other,
                },
            );
        }
        return ended_at(trace, "login-dialog", act);
    }
    // A LOST LOGIN DRAWN INSTEAD OF THE COMPOSER (agy's sign-in menu, whose
    // options open a browser): no point is ever reached and nothing can be
    // typed, so the owner is told how the program signs in.
    if reading.wall == Some(WallKind::Auth)
        && reading.composer == Composer::Absent
        && reading.phase == Phase::Idle
        && reading.authoritative
    {
        return ended_at(
            trace,
            "sign-in-menu",
            TurnEndAction::Escalate {
                reason: format!(
                    "the login is gone: {}",
                    sign_in_how(reading.program, &reading.wall_message)
                ),
            },
        );
    }
    // No task (none yet, or done after the done check): nothing is typed.
    if !reading.at_point(state.stale_input) || reading.taskless || state.task_done() {
        return ended_at(trace, "no-task", TurnEndAction::Nothing);
    }
    note(trace, "no-task", None, || Outcome::Passed);
    // A person at the keyboard wins ([`person_hold`]) — but the key that
    // sent the prompt a lost login answered holds `/login` only a moment
    // ([`login_quiet`]).
    let held = match (
        person_hold(state, reading, cfg, now),
        login_quiet(state, reading, now),
    ) {
        (Some((until, _)), Some(quiet)) => (now < quiet).then_some((
            quiet.min(until),
            "a person sent the prompt the lost login answered",
        )),
        (held, _) => held,
    };
    if let Some((until, why)) = held {
        return ended_at(
            trace,
            "person",
            TurnEndAction::WaitUntil {
                until,
                why: why.to_string(),
                wait: WaitKind::Other,
            },
        );
    }
    note(trace, "person", None, || Outcome::Passed);
    let act = act_at_point(state, reading, cfg, now, trace);
    // A standing draft (unchanged past the grace) is submitted in the act's
    // place under its rule, never typed over. Never into a lost login: the
    // wall would swallow it, so where `/login` was the act the point is
    // escalated.
    let drafted = match act.rule_id() {
        Some(RULE_LOGIN) if reading.composer.drafted() => TurnEndAction::Escalate {
            reason: format!(
                "the login is gone: {} (a draft stands in the composer; {})",
                state.wall_words(reading),
                sign_in_how(reading.program, state.wall_words(reading))
            ),
        },
        Some(rule_id) if reading.composer == Composer::Typed => TurnEndAction::Submit { rule_id },
        // Aterm's own words: never sent, never typed over, never taken back
        // (a person's key may be in flight); escalated once, for any program.
        Some(_) if reading.composer == Composer::Own => TurnEndAction::Escalate {
            reason: OWN_WORDS_STAND.to_string(),
        },
        // A draft over Claude Code's completion menu: its Enter would run the
        // menu's highlighted entry, not the draft. Never pressed; escalated
        // once, as aterm's own words are.
        Some(_) if reading.composer == Composer::Diverted => TurnEndAction::Escalate {
            reason: MENU_DIVERTS.to_string(),
        },
        _ => return act,
    };
    ended_at(trace, "draft", drafted)
}

/// A PERSON AT THE KEYBOARD WINS: hands off for the grace after their last
/// keystroke, and after a turn they stopped with Esc (itself a keystroke,
/// whether or not the server says so) from the first read of that Esc on
/// ([`TurnEndState::esc_at`]). `Some` — until when, and why — while it holds.
fn person_hold(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<(Instant, &'static str)> {
    let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
    let held = match (reading.person, reading.interrupted) {
        (Some(ago), _) if ago < grace => Some((later(now, grace - ago), "a person is typing")),
        (_, true) => Some((
            later(state.esc_at.unwrap_or(now), grace),
            "a person stopped the turn with Esc",
        )),
        _ => None,
    };
    held.filter(|(until, _)| now < *until)
}

/// HOW LONG THE KEY THAT SENT THE PROMPT A LOST LOGIN ANSWERED HOLDS
/// `/login` ([`login_quiet`]): long enough for a person's next prompt, sent
/// because nothing happened (the incident of 2026-10-19: five seconds after
/// the first), to land before `/login` does — a key typed after `/login`
/// lands in its method picker, whose digits choose.
pub const LOGIN_QUIET: Duration = Duration::from_secs(10);

/// THE KEY THAT SENT THE PROMPT A LOST LOGIN ANSWERED HOLDS `/login` ONLY
/// [`LOGIN_QUIET`] (`Some`: until when): Claude Code's lost login, its
/// composer empty, nothing typed at it yet, no Esc, no `/login` left, and
/// the person's last keystroke no later than the read at which this loop saw
/// the wall answer a prompt ([`LoginBooks::prompt_at`]): the person sent a
/// prompt, the wall answered it, and they have not touched the tab since.
/// Held for the whole grace, `/login` waited two minutes while they sat there
/// — and in the incident of 2026-10-19 they typed it themselves before it
/// went. `None` — the person's whole grace — for a key after that read (a
/// person dealing with the wall), and at a wall this loop did not see arrive
/// (its first point: the wall may have stood for hours, and nothing orders
/// the key before it).
fn login_quiet(state: &TurnEndState, reading: &TurnEndReading, now: Instant) -> Option<Instant> {
    let track = state.wall.as_ref().filter(|t| t.class == WallClass::Auth)?;
    let seen = track.login.prompt_at?;
    let key = now.checked_sub(reading.person?)?;
    (super::login::carried(reading.program)
        && reading.wall == Some(WallKind::Auth)
        && matches!(reading.composer, Composer::Empty | Composer::Placeholder(_))
        && !reading.interrupted
        && track.attempts == 0
        && !track.login.cancelled
        && key <= seen)
        .then(|| later(key, LOGIN_QUIET))
}

/// THE PERSON'S HOLD ON A STEP OF THE `/login` THIS LOOP TYPED (the success
/// Enter here, the method picker in the approval loop): none where their last
/// key came no later than that `/login` — it sent the prompt the wall
/// answered, and `/login` already waited out its quiet ([`login_quiet`]).
/// Held for the grace from that key, the picker and the success waited two
/// minutes while the owner's notice said the sign-in was theirs. A key after
/// it is a person in the dialog, and holds as any key does.
fn carry_held(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<(Instant, &'static str)> {
    let held = person_hold(state, reading, cfg, now)?;
    let typed = state.login_typed_at();
    let key = reading.person.and_then(|ago| now.checked_sub(ago));
    match (typed, key) {
        (Some(typed), Some(key)) if key <= typed && !reading.interrupted => None,
        _ => Some(held),
    }
}

/// THE WAIT FOR THE DIALOG OF THE `/login` THIS LOOP TYPED
/// ([`TurnEndState::login_opening`]): a lost login read with no dialog over
/// it inside the window is decided again at its end — then, with still no
/// dialog, the sign-in was left, and the owner is told so
/// ([`super::exception::exception_for`]); with nothing decided, a read that
/// never changes would leave them told nothing.
fn login_opening_wait(state: &TurnEndState, now: Instant) -> Option<TurnEndAction> {
    state
        .login_opening(now)
        .map(|until| TurnEndAction::WaitUntil {
            until,
            why: "the /login dialog to open".to_string(),
            wait: WaitKind::Other,
        })
}

/// THE HOUR'S TYPING BUDGET IS SPENT (`continue_per_hour`, `0` no cap): a
/// typed act decided now is escalated instead ([`act_at_point`]) — and a
/// lost login's notice says `/login` was not run for it
/// ([`super::exception::exception_for`]).
fn budget_spent(state: &TurnEndState, cfg: &SupervisorConfig, now: Instant) -> bool {
    cfg.continue_per_hour != 0 && state.acts_in_window(now) >= cfg.continue_per_hour as usize
}

/// The act at a free point ([`decide_turn_end`], past a person's grace):
/// a wall's, an owed continuation, a switch back, or the continue policy's —
/// the branch that decided it written to `trace`.
fn act_at_point(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    trace: &mut dyn Trace,
) -> TurnEndAction {
    let budgeted = |act: TurnEndAction| {
        if budget_spent(state, cfg, now) {
            TurnEndAction::Escalate {
                reason: format!(
                    "continue_per_hour = {} reached: {} typed in the last {} min",
                    cfg.continue_per_hour,
                    state.acts_in_window(now),
                    state.timing.window.as_secs() / 60
                ),
            }
        } else {
            act
        }
    };
    // A continuation at a LIMIT'S END ([`RULE_LIMIT_RESUME`], wherever a
    // wall's rule, an owed resume or the owner's clearing types one) answers
    // the worker's words since the limit first ([`limit_end`]).
    let continue_as = |rule: &'static str| {
        if rule == RULE_LIMIT_RESUME
            && let Some(act) = limit_end(state, reading, cfg, &budgeted)
        {
            return act;
        }
        budgeted(TurnEndAction::Type {
            text: continuation_text(cfg, reading.rules.as_deref()),
            rule_id: rule,
        })
    };
    let type_as = |rule: &'static str, text: &str| {
        budgeted(TurnEndAction::Type {
            text: with_rules(text, reading.rules.as_deref()),
            rule_id: rule,
        })
    };
    // USAGE CREDITS CARRY THE SESSION with `[harness] usage_credits` off:
    // whatever the point's act would type is billed to them, so it is held
    // ([`credits_hold`]); an act that types nothing stands as it is.
    let held =
        |trace: &mut dyn Trace, act: TurnEndAction| match credits_hold(state, reading, cfg, now) {
            Some(hold) => ended_at(
                trace,
                "credits-hold",
                held_by_credits(state, reading, cfg, now, act, hold),
            ),
            None => act,
        };
    if let Some(kind) = reading.wall {
        let act = wall_action(
            state,
            reading,
            cfg,
            now,
            kind,
            &budgeted,
            &continue_as,
            &type_as,
        );
        let act = ended_at(trace, "wall", wall_breaker(state, now, act));
        // A retry's, a full context's, a limit's and a bucket's own acts that
        // type (the retry, `/compact`, a limit's continuation, a bucket's
        // fallback relaunch and its carry-on) are model turns, billed to the
        // credits that carry the session; a lost login's `/login` is no model
        // turn, and a memory wall's relaunch is the process's life — neither
        // is held.
        return match class_of(kind) {
            WallClass::Retry | WallClass::Context | WallClass::Usage | WallClass::Model => {
                held(trace, act)
            }
            WallClass::Auth | WallClass::Memory => act,
        };
    }
    // A lost login the policy saw is waited out off the screen too, while its
    // track stands and the worker has not worked: the wall's row gone (the
    // `/login` dialog dismissed) is no login back, and anything typed would
    // meet the wall again. `Login successful`
    // ([`TurnEndReading::login_back`]) or the worker working
    // ([`TurnEndState::observe`]) lifts it. Held as it stands where a point
    // decided it with the wall's own rule (`/login` typed, or the escalation
    // under `retry_api_errors = false`): the owner was told then. One NO
    // POINT DECIDED — read only inside an act's take window, its row gone
    // before the window's end — is decided as the wall it left
    // ([`still_shown`]): `/login` once, the owner told by the lost login's
    // notice ([`super::exception::exception_for`]) (round 16's review: held
    // as told, nothing was typed and nobody was told). So is one a point only
    // WAITED at, the session's own move standing and no `/login` typed
    // ([`left_waited_login`], [`login_wall_waits`]): nobody was told, and
    // held, a person's `/status`, `/usage` or Esc out of `/login` left the
    // tab undecided for good — it waits again, or moves once the host,
    // asked about it as that wall ([`TurnEndState::account_point`]),
    // advises a move.
    if let Some(t) = &state.wall
        && t.class == WallClass::Auth
        && !reading.login_back
    {
        let shown = still_shown(t, reading).or_else(|| {
            waited_login(state, reading, cfg, now).map(|waited| (WallKind::Auth, waited))
        });
        let Some((kind, shown)) = shown else {
            let held = login_opening_wait(state, now).unwrap_or(TurnEndAction::Nothing);
            return ended_at(trace, "login-held", held);
        };
        let act = wall_action(
            state,
            &shown,
            cfg,
            now,
            kind,
            &budgeted,
            &continue_as,
            &type_as,
        );
        return ended_at(trace, "login-held", wall_breaker(state, now, act));
    }
    let act = act_past_walls(
        state,
        reading,
        cfg,
        now,
        trace,
        &budgeted,
        &continue_as,
        &type_as,
    );
    held(trace, act)
}

/// [`act_at_point`] at a point no wall decides — no wall drawn, no lost
/// login held: an owed continuation, a limit's left notice, a retry's, the
/// account move and the model's return, and the continue policy's act.
#[allow(clippy::too_many_arguments)]
fn act_past_walls(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    trace: &mut dyn Trace,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
    type_as: &dyn Fn(&'static str, &str) -> TurnEndAction,
) -> TurnEndAction {
    // A command's continuation, owed once it has run: where a `/compact`
    // owes the act a full context cut ([`TurnEndState::compact_owes`]), that
    // act again — the done check, the challenge or the land act as typed.
    if let Some(rule) = state.after {
        if let Some(text) = state.compact_owed_text(rule, reading.rules.as_deref()) {
            return ended_at(
                trace,
                "owed",
                budgeted(TurnEndAction::Type {
                    text,
                    rule_id: rule,
                }),
            );
        }
        return ended_at(trace, "owed", continue_as(rule));
    }
    // A LIMIT RESET'S CONTINUATION ([`TurnEndState::reset_told`]): the
    // vendor's answer line is said after the wall, so the screen reads idle
    // with no wall, and the step under the dialog opened no wall track — the
    // point would be the continue policy's, or nobody's with it off.
    if let Some(act) = reset_continuation(state, cfg, now, continue_as) {
        return ended_at(trace, "reset-continuation", act);
    }
    // The screen left a limit it was waiting out, with no work since: a
    // usage window, or a model bucket the session has not left, whose notice
    // named its reset is decided as if the notice still showed
    // ([`still_shown`]) — by the wall's policies whole, the move to another
    // account in front of its own rule ([`wall_action`]), and under
    // `resume_limits = false` by nothing, as at the wall: the limit episode
    // told the person. One that named none, or a bucket the session left by
    // a person's `/model` (its way out: [`NamedReset::moved_off_under`]),
    // with no act of ours, goes at once.
    if let Some(t) = &state.wall
        && named_class(t.class)
    {
        if let Some((kind, shown)) = still_shown(t, reading) {
            if !cfg.resume_limits {
                return ended_at(trace, "limit-left", TurnEndAction::Nothing);
            }
            let act = wall_action(
                state,
                &shown,
                cfg,
                now,
                kind,
                budgeted,
                continue_as,
                type_as,
            );
            return ended_at(trace, "limit-left", wall_breaker(state, now, act));
        }
        if t.attempts == 0 && cfg.resume_limits {
            return ended_at(trace, "limit-left", resume_limit(continue_as));
        }
    }
    // A RETRY'S WALL whose notice left the screen with no work since (a
    // person's `/status`, `/usage` or a scroll drawn under it; the cross-lane
    // review's F3) stands: decided as the wall it left ([`left_retry`]), its
    // ladder's rung held and its own retry typed — never `keep going` before
    // the back-off allows. Under `retry_api_errors = false`, nothing: the
    // wall's point told the owner.
    if let Some(t) = &state.wall
        && let Some((kind, shown)) = left_retry(t, reading)
    {
        if !cfg.retry_api_errors {
            return ended_at(trace, "retry-left", TurnEndAction::Nothing);
        }
        let act = wall_action(
            state,
            &shown,
            cfg,
            now,
            kind,
            budgeted,
            continue_as,
            type_as,
        );
        return ended_at(trace, "retry-left", wall_breaker(state, now, act));
    }
    // THE WALL'S RESUME AFTER A MOVE MADE AT IT (the accounts design, §6
    // "The move"): the limit the session left would have been continued at
    // its reset under `resume_limits`, and the move took that reset away —
    // so the same continuation, by the same rule, goes at the relaunched
    // session's first point. Not the continue policy's: under
    // `continue_policy = false` a moved session was typed nothing, where
    // with no roster it was resumed.
    if state.account_resume && cfg.resume_limits {
        return ended_at(trace, "account-resume", resume_limit(continue_as));
    }
    let cont = continue_policy(state, reading, cfg, now, budgeted);
    // A LANDING THAT IS OVER (Autopilot Land, its instruction typed and its
    // nudges spent: no continuation is typed) is not wrapped in a relaunch
    // below, whatever its point gets: the relaunched agent is carried on by
    // its host, which would start the work moving again. That holds for a
    // point where nothing is typed, for the short streak's wait, and for a
    // question, whose land answer is typed as it is with no relaunch around
    // it. Only that: a Land point with nothing to type for another reason
    // (`continue = false`, its landing never begun) is wrapped as Cruise's.
    if reading.autopilot == Autopilot::Land && state.land_spent() {
        return ended_at(trace, "land-spent", cont);
    }
    // THE ACCOUNT MOVE AHEAD OF A WALL (the accounts design, §6): the host
    // advises another account at a plain turn end — the owner's `accounts
    // use`, the account the machine moved to and has seen served. Decided
    // where the model's return is, and as that is: it wraps the turn's own
    // act (`otherwise`: a move the host cannot make yet never holds the
    // point), and is never asked over an escalation. Ahead of the model's
    // return: a move made relaunches on the bucket's model itself and lets
    // the switch go ([`TurnEndState::restarted`]) — one relaunch, not two.
    // Not while this session's own last move is still unanswered: that one
    // is proven, or struck, first. Nor to an account a move ahead of the wall
    // could not be made to within the last window: that is asked again at
    // the wall, or an hour on, never at every turn end ([`move_ahead`]).
    if let Some(to) = move_ahead(state, reading, cfg, now)
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return ended_at(trace, "account-ahead", ahead_to(to, cont));
    }
    // The bucket's reset has come: relaunched on the bucket's model first,
    // which carries it on — or, where no relaunch is made, the point's own
    // act, the model left as it is (session-only: nothing else moved).
    if let Some(m) = &state.model
        && let Some(at) = m.back_at
        && now >= at
        && reading.restartable
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return ended_at(
            trace,
            "model-back",
            TurnEndAction::Restart {
                why: Restart::ModelBack { to: m.from.clone() },
                rule_id: RULE_MODEL_RESTORE,
                otherwise: Box::new(cont),
            },
        );
    }
    ended_at(trace, "continue-policy", cont)
}

/// THE ACCOUNT MOVE AHEAD OF A WALL MAY BE ASKED HERE ([`act_past_walls`]):
/// the account the host advises, where no move of this session's stands
/// unanswered, none to it was refused within the window, the session may be
/// relaunched under it, and the window's moves are not spent. `None`: none.
fn move_ahead<'a>(
    state: &TurnEndState,
    reading: &'a TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<&'a String> {
    match reading.advised() {
        Some(AccountAdvice::Move { to })
            if state.account.is_none()
                && !state.account_ahead_refused(to, now)
                && (account_rotates(state, reading, cfg)
                    || login_move(state, reading, cfg, to))
                && state.account_moves_of(now).0 < ACCOUNT_MOVES_PER_WINDOW =>
        {
            Some(to)
        }
        _ => None,
    }
}

/// The move ahead of a wall to `to`, `otherwise` the point's own act.
fn ahead_to(to: &str, otherwise: TurnEndAction) -> TurnEndAction {
    TurnEndAction::Restart {
        why: Restart::Account {
            to: to.to_string(),
            at: AccountAt::Ahead,
        },
        rule_id: RULE_ACCOUNT,
        otherwise: Box::new(otherwise),
    }
}

/// THE CREDITS HOLD (the owner, 2026-10-19: *"for continuing on credits,
/// that's an optional additional feature that's off by default. use
/// subscriptions."*): where usage credits carry the session with no dialog
/// (credits on at the account: Claude Code says `Now using usage credits`;
/// or a model billed to them per request: `Fable 5.1 is drawing from usage
/// credits`; [`TurnEndReading::credits`]) every turn the harness types — the
/// continuation, the done check, a land act, an answer, an owed act, a
/// retry, `/compact`, a limit's or a bucket's own continuation, the host's
/// own — is billed to them. Under `[harness] usage_credits = false`, the
/// default, the policy types nothing of its own while they carry it
/// ([`TurnEndState::fold_credits`], [`TurnEndState::credits_held`]), by the
/// table the session's surfaces read too
/// ([`super::outlook::credits_outlook`]):
///
/// * the window they carry past named: WAIT until its reset and the grace a
///   wall's wait-out keeps ([`TurnEndTiming::grace_for`]) — Claude Code goes
///   back onto the subscription by itself there, and the point is decided as
///   any other (a notice drawn before the reset and left at rest holds
///   nothing past it; a notice no longer drawn, a reset still ahead, holds:
///   a Team account draws no lasting one);
/// * none named: the owner told ONCE ([`CREDITS_HOLD_TOLD`],
///   [`TurnEndState::credits_told`]), then a wait to the hold's bound
///   ([`TurnEndState::credits_bound`]), where one act goes as the probe — on
///   the subscription its answer draws no notice, and on credits still
///   (a longer window binds) a new episode begins;
/// * a MODEL's, none named: the owner told once
///   ([`CREDITS_HOLD_MODEL_TOLD`]), and nothing more until the session runs
///   another model.
///
/// A MODEL'S CREDITS with `[harness] model_fallback` set — another family,
/// the session restartable, no fallback made or refused — RELAUNCH it onto
/// the fallback ([`RULE_MODEL_FALLBACK`], the hold its otherwise), as the
/// same bucket drawn as a wall would: the subscription serves the fallback,
/// and the return is owed at the reset their notice named
/// ([`TurnEndState::restarted`]). Where the accounts advise a move to
/// another account (a session on credits is one to leave:
/// `crate::harness::accounts::Account::leave_early`) the move goes first,
/// the hold its otherwise ([`held_by_credits`]). A person's own turns are
/// never held: the hold is the policy's acts alone. `None` under the key
/// (credits are work going on), with no notice and no episode standing
/// without one, and past the hold's end.
fn credits_hold(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<TurnEndAction> {
    if cfg.usage_credits {
        return None;
    }
    let held = state.credits_held(reading.credits.as_ref(), reading.program, now)?;
    let hold = match super::outlook::credits_outlook(held.reset.is_some(), cfg, true) {
        super::outlook::CreditsOutlook::WaitsForReset => TurnEndAction::WaitUntil {
            until: held.lifts.unwrap_or(now),
            why: match &held.model {
                Some(model) => format!(
                    "usage credits carry {model}'s requests ({RULE_CREDITS_HOLD}): its limit resets"
                ),
                None => format!(
                    "usage credits carry the session ({RULE_CREDITS_HOLD}): the subscription's \
                     window resets"
                ),
            },
            wait: WaitKind::Other,
        },
        super::outlook::CreditsOutlook::Person if !held.told => TurnEndAction::Escalate {
            reason: if held.model.is_some() {
                CREDITS_HOLD_MODEL_TOLD
            } else {
                CREDITS_HOLD_TOLD
            }
            .to_string(),
        },
        super::outlook::CreditsOutlook::Person => match held.lifts {
            Some(until) => TurnEndAction::WaitUntil {
                until,
                why: format!(
                    "usage credits carry the session ({RULE_CREDITS_HOLD}), no reset named: \
                     the five-hour window has reset by then"
                ),
                wait: WaitKind::Other,
            },
            None => TurnEndAction::Nothing,
        },
        super::outlook::CreditsOutlook::Carried | super::outlook::CreditsOutlook::Unattended => {
            TurnEndAction::Nothing
        }
    };
    if let Some(model) = &held.model
        && let Some(to) = credits_fallback(state, reading, cfg, model)
    {
        return Some(TurnEndAction::Restart {
            why: Restart::Model { to },
            rule_id: RULE_MODEL_FALLBACK,
            otherwise: Box::new(hold),
        });
    }
    Some(hold)
}

/// The fallback a session whose MODEL usage credits carry is relaunched
/// onto ([`credits_hold`]): `[harness] model_fallback`, where it is another
/// family than `model` ([`CreditsCarry::model`]), the session may be
/// relaunched, and no fallback stands, was refused or could not be made.
fn credits_fallback(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    model: &str,
) -> Option<String> {
    let to = cfg.model_fallback.as_ref()?;
    let same = to
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case(model));
    (!same
        && reading.restartable
        && state.model.is_none()
        && !state.fallback_unmade
        && !state.fallback_refused())
    .then(|| to.clone())
}

/// The point's act `act` under the credits hold `hold` ([`credits_hold`]):
/// an act that types nothing (nothing, a wait, an escalation) stands; a move
/// ahead of a wall stands with the hold for its otherwise; anything else
/// that would type — or relaunch the agent, whose carry-on types — gives
/// way to the move to the account the host advises, where one may be asked
/// ([`move_ahead`]), else to the hold.
fn held_by_credits(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    act: TurnEndAction,
    hold: TurnEndAction,
) -> TurnEndAction {
    let types = |act: &TurnEndAction| {
        !matches!(
            act,
            TurnEndAction::Nothing
                | TurnEndAction::WaitUntil { .. }
                | TurnEndAction::Escalate { .. }
        )
    };
    match act {
        act if !types(&act) => act,
        TurnEndAction::Restart {
            why:
                why @ Restart::Account {
                    at: AccountAt::Ahead,
                    ..
                },
            rule_id,
            otherwise,
        } => TurnEndAction::Restart {
            why,
            rule_id,
            otherwise: Box::new(if types(&otherwise) { hold } else { *otherwise }),
        },
        _ => match move_ahead(state, reading, cfg, now) {
            Some(to) => ahead_to(to, hold),
            None => hold,
        },
    }
}

/// The continue policy at a point with no wall: the question answered or the
/// turn continued (the worker's suggestion first) once the short streak's
/// back-off passes; a question escalated only when answers are switched off.
fn continue_policy(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> TurnEndAction {
    let (asked, irreversible) = match reading.asked() {
        Some((why, irreversible)) => (Some(why), irreversible),
        None => (None, false),
    };
    // A done report gets the done check whatever else it said: answering a
    // done worker's "what's next?" with `answer_text` invites invented work.
    let done = state.done == 1 && !reading.interrupted;
    if let Some(act) = words_gate(cfg, asked.as_deref(), done) {
        return act;
    }
    // A MANAGER IS WAITING (design §2.4): a worker's done check and its
    // answers go at once. The back-off saves an idle person's tokens; a
    // worker's manager waits on its `await task` for the done the check
    // reaches, or for the work the answer lets go on. Its continuation keeps
    // the back-off: a worker that stops short with nothing to say is the
    // case it was made for. So does a worker past its first
    // [`MANAGED_SHORT_TURNS`] short turns in a row: one that asks after
    // every short turn is answered at the back-off's pace, not the model's.
    let waited_on =
        state.managed && (done || asked.is_some()) && state.short <= MANAGED_SHORT_TURNS;
    if !waited_on && let (Some(wait), Some(at)) = (state.short_wait(), state.point_at) {
        let due = later(at, wait);
        if now < due {
            let secs = wait.as_secs();
            return TurnEndAction::WaitUntil {
                until: due,
                why: if secs < 120 {
                    format!("{} short turn(s) in a row: {secs} s back-off", state.short)
                } else {
                    format!(
                        "{} short turn(s) in a row: {} min back-off",
                        state.short,
                        secs / 60
                    )
                },
                wait: WaitKind::BackOff {
                    short: u8::try_from(state.short).unwrap_or(u8::MAX),
                },
            };
        }
    }
    // A DONE REPORT THAT IS NO FINISH — its words name open work, or the
    // session's repository holds work no remote has — gets its challenge
    // ([`RULE_UNCOMMITTED`], [`RULE_OPEN_ITEMS`]) in place of the check, under
    // Cruise and Land alike, at every such report: past the short streak's
    // back-off as the check is (a worker's at once within its bound), so a
    // worker that answers each one at once, the same work still left, is
    // challenged at the back-off's pace and never in a loop at the model's —
    // and never let go: no count of challenges ends a job with work left.
    // A report that also names an irreversible act gets the reversible
    // clause, as the done check would (D1).
    if let Some(act) = owed_challenge(state, reading, irreversible, done, budgeted) {
        return act;
    }
    if let Some(act) = words_act(
        state,
        reading,
        cfg,
        asked.is_some(),
        irreversible,
        done,
        budgeted,
    ) {
        return act;
    }
    match &reading.composer {
        Composer::Placeholder(s) if suggestion_allowed(s) => budgeted(TurnEndAction::Accept {
            text: s.clone(),
            rule_id: RULE_SUGGESTION,
        }),
        _ => budgeted(TurnEndAction::Type {
            text: continuation_text(cfg, reading.rules.as_deref()),
            rule_id: RULE_CONTINUE,
        }),
    }
}

/// THE CHALLENGE A DONE REPORT THAT IS NO FINISH OWES ([`challenge_text`]):
/// where the point's done report (`done`) names open work or the session's
/// repository held work no remote has ([`TurnEndState::observe`]), under
/// Cruise and Land alike — at a plain turn end ([`continue_policy`]) and at
/// a limit's end ([`limit_end`]). `None` where none is owed.
fn owed_challenge(
    state: &TurnEndState,
    reading: &TurnEndReading,
    irreversible: bool,
    done: bool,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> Option<TurnEndAction> {
    if !done || (state.repo.is_none() && state.open.is_empty()) {
        return None;
    }
    let (text, rule_id) = challenge_text(
        &state.open,
        state.repo.as_ref(),
        reading.autopilot == Autopilot::Land,
        reading.rules.as_deref(),
        irreversible,
    );
    Some(budgeted(TurnEndAction::Type { text, rule_id }))
}

/// THE CONTINUE POLICY'S GATES ON THE WORKER'S WORDS — one rule for a plain
/// turn end ([`continue_policy`]) and a limit's end ([`limit_end`]):
/// whatever they ask (`asked`, its reason) is escalated under
/// `answer_questions = false` ([`ANSWERS_OFF`]); under `continue_policy =
/// false` a done report (`done`), or words that ask nothing, get nothing.
/// `None`: the policy goes on to its act ([`words_act`]).
fn words_gate(cfg: &SupervisorConfig, asked: Option<&str>, done: bool) -> Option<TurnEndAction> {
    match asked {
        Some(why) if !cfg.answer_questions => Some(TurnEndAction::Escalate {
            reason: format!("{why}; {ANSWERS_OFF}"),
        }),
        _ if done && !cfg.continue_policy => Some(TurnEndAction::Nothing),
        None if !cfg.continue_policy => Some(TurnEndAction::Nothing),
        _ => None,
    }
}

/// THE CONTINUE POLICY'S ANSWER TO THE WORKER'S WORDS, past its gates
/// ([`words_gate`]) — one rule for a plain turn end ([`continue_policy`],
/// past its back-off too) and a limit's end ([`limit_end`]). Under
/// [`Autopilot::Land`], Land's own act ([`land_policy`]): a done report, or
/// words that ask nothing, get [`LAND`], then a [`LAND_NUDGE`], then nothing
/// once the landing is spent; a question, one that names an irreversible act
/// included, [`LAND_ANSWER`]. Else a done report (`done`) gets the done check,
/// its reversible clause where the words name an irreversible act
/// (`irreversible`): its bare `otherwise continue` would read as consent
/// (D1); a question (`asked`) that names one, [`REVERSIBLE_ANSWER`] under
/// [`RULE_ANSWER`] whatever the priority; any other question, the priority's
/// answer. `None` under Cruise where the words are neither: the continuation.
fn words_act(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    asked: bool,
    irreversible: bool,
    done: bool,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> Option<TurnEndAction> {
    if reading.autopilot == Autopilot::Land {
        return Some(land_policy(state, reading, asked, done, budgeted));
    }
    let rules = reading.rules.as_deref();
    if done {
        return Some(budgeted(TurnEndAction::Type {
            text: done_check_text(rules, irreversible),
            rule_id: RULE_DONE_CHECK,
        }));
    }
    if !asked {
        return None;
    }
    Some(budgeted(if irreversible {
        TurnEndAction::Type {
            text: reversible_answer(rules),
            rule_id: RULE_ANSWER,
        }
    } else {
        TurnEndAction::Type {
            text: prose_answer(cfg, rules),
            rule_id: cfg.question_priority.prose_rule_id(),
        }
    }))
}

/// THE CONTINUE POLICY UNDER AUTOPILOT LAND ([`continue_policy`], past the
/// gates it shares with Cruise: `answer_questions = false` escalates a
/// question, `continue = false` types nothing where nothing is asked, and the
/// short streak's back-off waits). In order:
///
/// 1. A done report (`done`, the done check's first) or a point where nothing
///    is asked: [`LAND`] if this episode has not had it — it stands in for the
///    continuation and the done check alike, so a worker that already says
///    done is landed too, and asked for the token; else a [`LAND_NUDGE`]
///    while fewer than [`LAND_NUDGES`] went; else nothing: the landing is
///    over ([`TurnEndState::land_spent`]). A done reply to any of them that
///    leaves nothing never reaches here: it ended the task
///    ([`TurnEndState::observe`]); one that leaves work — open work in its
///    report, or work in the repository no remote has — gets the challenge
///    in Land's words before this (`owed_challenge`), at every such reply.
/// 2. A question: [`LAND_ANSWER`], one that names an irreversible act
///    included. The land answer already says to change nothing
///    irreversible; Cruise's [`REVERSIBLE_ANSWER`] ends "and keep going",
///    which is no landing.
///
/// The done report goes first as under Cruise: a done worker's `what's
/// next?` answered as a question invites invented work.
fn land_policy(
    state: &TurnEndState,
    reading: &TurnEndReading,
    asked: bool,
    done: bool,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> TurnEndAction {
    let rules = reading.rules.as_deref();
    let (text, rule_id) = if done || !asked {
        if !state.land.typed {
            (land_text(rules), RULE_LAND)
        } else if !state.land_spent() {
            (land_nudge_text(rules), RULE_LAND_NUDGE)
        } else {
            return TurnEndAction::Nothing;
        }
    } else {
        (land_answer_text(rules), RULE_LAND_ANSWER)
    };
    budgeted(TurnEndAction::Type { text, rule_id })
}

/// Whether the vendor GOES ON BY ITSELF at the wall `reading` shows, as
/// [`wall_own_act`] decides it: its countdown armed
/// ([`TurnEndReading::resumes_by_itself`]) and not the gone process's (the
/// limit `track` names first read before the agent's process started,
/// [`TurnEndState::countdown_gone`]). Such a wall is waited on the short
/// ladder's last rung from its appearance, whatever its kind, so its try is
/// on no entitlement ladder ([`TurnEndState::track_wall`]). Marked as one, a
/// bucket's armed countdown whose reset the loop could not place left a seed
/// that stood two and six hours where the first loop tries every thirty
/// minutes (the verifier of ed6efadb0, V22-B).
fn goes_on_by_itself(
    state: &TurnEndState,
    track: Option<&WallTrack>,
    reading: &TurnEndReading,
) -> bool {
    reading.resumes_by_itself
        && !track
            .and_then(|t| t.named)
            .is_some_and(|n| state.named_countdown_gone(&n))
}

/// Whether a wall of `kind`, its reset placed (`placed`) or not, is retried
/// on the ENTITLEMENT ladder ([`TurnEndTiming::entitlement_backoff`]): a
/// spend limit, or a model's bucket, that names no reset. A usage window
/// with no readable reset still lifts by itself, and keeps the short ladder.
/// The ledger's wall rows are read by the same rule
/// ([`crate::supervise::approvals::UsageAttempt::limit`]).
pub(crate) fn entitlement_ladder(placed: bool, kind: WallKind) -> bool {
    !placed && matches!(kind, WallKind::Spend | WallKind::ModelBucket { .. })
}

/// A limit waited out: continue [`TurnEndTiming::reset_grace`] past the
/// notice's reset, or [`TurnEndTiming::limit_backoff`] after a continuation
/// that hit the wall again, whichever is later; with no reset, the longest
/// back-off from the appearance.
fn resume_at_reset(
    state: &TurnEndState,
    reading: &TurnEndReading,
    now: Instant,
    since: Instant,
    attempts: usize,
    kind: WallKind,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> TurnEndAction {
    // A wall no reset ends and none names is an entitlement: its own, slower
    // ladder. A usage window with no readable
    // reset still lifts by itself, so it keeps the short one.
    if entitlement_ladder(reading.reset_at.is_some(), kind) {
        let due = crate::supervise::ladder::Ladder(&state.timing.entitlement_backoff)
            .at(attempts)
            .map(|w| later(since, w));
        return match due {
            Some(due) if now < due => TurnEndAction::WaitUntil {
                until: due,
                why: format!("{} (no reset named)", kind.name()),
                wait: WaitKind::Other,
            },
            Some(_) => continue_as(RULE_LIMIT_RESUME),
            None => TurnEndAction::Nothing,
        };
    }
    let back = &state.timing.limit_backoff;
    let backoff = |i: usize| crate::supervise::ladder::Ladder(back).at(i);
    let at_reset = reading
        .reset_at
        .map(|r| later(r, state.timing.grace_for(reading.program) + reading.jitter));
    let after_retry = if attempts == 0 {
        None
    } else {
        backoff(attempts - 1).map(|b| later(since, b))
    };
    let due = match (at_reset, after_retry) {
        (Some(r), Some(b)) => r.max(b),
        (Some(r), None) => r,
        (None, Some(b)) => b,
        // No reset the loop could read: the longest backoff.
        (None, None) => match back.last() {
            Some(&b) => later(since, b),
            None => return TurnEndAction::Nothing,
        },
    };
    if now < due {
        return TurnEndAction::WaitUntil {
            until: due,
            why: format!("{} resets", kind.name()),
            wait: WaitKind::Other,
        };
    }
    resume_limit(continue_as)
}

/// THE WALL A LIMIT'S NOTICE LEFT ON THE SCREEN with no work since
/// ([`act_at_point`]): the point's reading with that wall back on it, as its
/// notice last read — its words ([`WallTrack::said`]) and the reset it named,
/// which [`TurnEndState::observe`] keeps — for the wall's own rule to decide.
/// A usage window, or a model bucket the session has not left
/// ([`named_class`]), stands until the reset its notice named, whatever the
/// screen shows since. Measured on Claude Code 2.1.295 (the sweep of
/// 2026-10-09): Esc on `Continuing automatically at … · esc to cancel` leaves
/// `⏺ Automatic continue cancelled · /rate-limit-options to re-arm`, no wall,
/// and the continuation typed at once met the standing limit minutes before
/// its reset, which put the real resume a back-off past it. Decided as the
/// wall, it waits for the reset and the grace: the vendor said it will not
/// go on by itself. So too where the notice left and Claude Code no longer
/// draws its countdown (a `/resume`, a `/clear`, W4 of ATERM-KI-1015-1): a
/// countdown dropped without a word is a cancelled one
/// ([`NamedReset::by_itself`]). Only a countdown still drawn
/// ([`aterm_phase::AutoContinue::Armed`]) is taken at its word, and
/// [`TurnEndTiming::auto_resume_grace`] past the reset is the net under a
/// vendor that did not go on. Where the vendor went on under THIS limit's own
/// notice (`Usage limit reset · continuing automatically`,
/// [`NamedReset::continued_under`]), the limit is over and no track is left
/// ([`TurnEndState::observe`]); its word under another limit's notice (a
/// `/resume`d conversation's history) ends nothing. A LOST LOGIN NO POINT
/// DECIDED is the wall it left too ([`left_undecided_login`]): its row gone
/// with the login not back is no login, and its own rule has not had its
/// say. `None`: the notice named no reset, the track is a bucket the session
/// left (its relaunch on the fallback made, a person's `/model`), a login a
/// point decided, or the screen says the login is back (`/login`, perhaps to
/// another account).
fn still_shown(t: &WallTrack, reading: &TurnEndReading) -> Option<(WallKind, TurnEndReading)> {
    if t.class == WallClass::Auth {
        return left_undecided_login(t, reading).then(|| {
            (
                WallKind::Auth,
                TurnEndReading {
                    wall: Some(WallKind::Auth),
                    wall_message: t.said.clone(),
                    ..reading.clone()
                },
            )
        });
    }
    let n = left_named(t, reading)?;
    Some((
        n.kind,
        TurnEndReading {
            wall: Some(n.kind),
            wall_message: t.said.clone(),
            reset_at: Some(n.at),
            jitter: n.jitter,
            resumes_by_itself: n.by_itself,
            ..reading.clone()
        },
    ))
}

/// A RETRY'S WALL ITS NOTICE LEFT ([`act_at_point`]): `reading` with the
/// wall back on it, in its own words — a server's, an overload's, a network
/// never reached, a cut-off or an overlong reply: nothing in the tab lifts
/// those, so the rung is held, bounded by their short ladders (at most an
/// hour, or a measured outage's 15 min). Never a refused request
/// ([`ApiCause::Request`]): what lifts it is a person's change in the tab
/// (`/model`, `/effort`, `/rewind`: `policy/exception.rs`), which does no
/// work either and reads as a look does — held, the fix would wait out the
/// refused ladder's rung, up to six hours; continued, an extra refused try
/// leaves its ladder where it was.
fn left_retry(t: &WallTrack, reading: &TurnEndReading) -> Option<(WallKind, TurnEndReading)> {
    let kind = left_retry_kind(t)?;
    Some((
        kind,
        TurnEndReading {
            wall: Some(kind),
            wall_message: t.said.clone(),
            ..reading.clone()
        },
    ))
}

/// The kind of a retry's wall [`left_retry`] decides a point its notice
/// left as: a Retry-class track's, its kind read (no seeded track), never a
/// refused request's.
fn left_retry_kind(t: &WallTrack) -> Option<WallKind> {
    t.kind.filter(|k| {
        t.class == WallClass::Retry
            && !matches!(
                k,
                WallKind::ApiError {
                    cause: ApiCause::Request,
                    ..
                }
            )
    })
}

/// The reset a track's notice named, where a point its notice has left is
/// decided as if it still showed ([`still_shown`]): a usage window's, or a
/// bucket's the session has not left ([`named_class`]: its reset is dropped
/// where it did), while the screen does not say the login is back.
fn left_named(t: &WallTrack, reading: &TurnEndReading) -> Option<NamedReset> {
    t.named
        .filter(|_| named_class(t.class) && !reading.login_back)
}

/// A LOST LOGIN NO POINT DECIDED, its row gone from the screen
/// ([`WallTrack::pointed`]): read only where no point was yet — the answer to
/// an act inside its take window, under anyone's unanswered row — and the
/// login not back. Decided as the wall it left ([`still_shown`]); one a point
/// saw was decided there, and is held ([`act_at_point`]).
fn left_undecided_login(t: &WallTrack, reading: &TurnEndReading) -> bool {
    t.class == WallClass::Auth && !t.pointed && !reading.login_back
}

/// A LOST LOGIN NO `/login` HAS BEEN TYPED AT, its row gone from the screen,
/// while this session's own account move stands
/// ([`TurnEndState::account_move`]) and the login is not back — no
/// `Login successful` read since the wall ([`TurnEndState::observe`] ends the
/// track there): the policy only waited at it ([`login_wall_waits`]), or
/// asked a move the host could not make yet. No `/login` was typed, so it is
/// not held as a sign-in the owner was told to finish: decided again as the
/// wall it left ([`waited_login`]), and asked about as that wall
/// ([`TurnEndState::account_point`]).
fn left_waited_login(state: &TurnEndState, t: &WallTrack, reading: &TurnEndReading) -> bool {
    t.class == WallClass::Auth
        && t.attempts == 0
        && (state.account.is_some() || reading.login_label.is_some())
        && !reading.login_back
}

/// THAT LOST LOGIN AS THE WALL IT LEFT ([`left_waited_login`]): `reading`
/// with the wall back on it, in its own words, where its wait still stands
/// ([`login_wall_waits`]). [`decide_turn_end`] decides the point on it — it
/// waits again, or moves once advised — and
/// [`super::exception::exception_for`] reads it too: a person's `/status` or
/// `/usage` with the row off the screen is no login back. `None` where the screen shows a wall, or no such login is
/// tracked.
pub(super) fn waited_login(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<TurnEndReading> {
    let t = state.wall.as_ref()?;
    if reading.wall.is_some() || !left_waited_login(state, t, reading) {
        return None;
    }
    let waited = TurnEndReading {
        wall: Some(WallKind::Auth),
        wall_message: t.said.clone(),
        ..reading.clone()
    };
    login_wall_waits(state, &waited, cfg, now).map(|_| waited)
}

/// THE LAST LINE AGAINST A HOT LOOP ([`WALL_ACTS_PER_WINDOW`]): a wall's act
/// past the cap waits for the oldest of its rule to leave the window, and
/// says so; anything else — an act under the cap, a wait, a restart (no
/// wall act) — is `act` as it is. Applied to what a wall's point decides
/// ([`act_at_point`]) and, ahead of that, to what an account move falls back
/// to ([`account_first`], [`wall_action`]): a restart is looked at by its own
/// rule, so its `otherwise` would pass the cap unseen.
fn wall_breaker(state: &TurnEndState, now: Instant, act: TurnEndAction) -> TurnEndAction {
    if let Some(rule) = act.rule_id().filter(|r| is_wall_act(r))
        && let (n, Some(free)) = state.wall_acts_of(rule, now)
        && n >= WALL_ACTS_PER_WINDOW
    {
        return TurnEndAction::WaitUntil {
            until: free,
            why: format!(
                "a breaker: {n} acts of {rule} in the last {} min",
                state.timing.window.as_secs() / 60
            ),
            wait: WaitKind::Other,
        };
    }
    act
}

/// THE CONTINUATION A LIMIT RESET LEFT OWED ([`TurnEndState::reset_told`]),
/// under `resume_limits`: typed by the limit-resume rule once it is due
/// ([`resume_limit`]: budgeted, and counted as a wall's act), and waited for
/// until then — the vendor queued its own, and a turn seen running before the
/// wait is out pays it ([`TurnEndState::took`]). `None` with nothing owed, or
/// with `resume_limits` off: the point is then decided as it was before
/// resets existed.
fn reset_continuation(
    state: &TurnEndState,
    cfg: &SupervisorConfig,
    now: Instant,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> Option<TurnEndAction> {
    let owed = state.reset_resume.filter(|_| cfg.resume_limits)?;
    Some(if now < owed.due {
        TurnEndAction::WaitUntil {
            until: owed.due,
            why: "the limit reset's own continuation".to_string(),
            wait: WaitKind::Other,
        }
    } else {
        resume_limit(continue_as)
    })
}

/// A LIMIT'S END TYPES NO KEEP GOING INTO A DONE REPORT (the owner,
/// 2026-10-15: the harness "doesn't respect … if the agent is reporting that
/// it's done"). Every continuation [`act_at_point`] types under
/// [`RULE_LIMIT_RESUME`] — a reset and its grace waited out, the vendor's
/// countdown past its net, a limit reset's or an account move's owed resume,
/// the owner's clearing — first answers the worker's words SINCE THE LIMIT
/// ([`TurnEndReading::said_since_limit`]) that are a done report
/// ([`says_done`]) or ask anything ([`asked_by`]: a question, a stop, one
/// that names an irreversible act) BY THE CONTINUE POLICY'S OWN RULE
/// ([`words_gate`], [`words_act`]: one rule, shared with
/// [`continue_policy`]): under Cruise a done report gets the done
/// check ([`done_check_text`], the reversible clause after it where the
/// report names an irreversible act), or nothing under `continue_policy =
/// false`; words that name an irreversible act get [`REVERSIBLE_ANSWER`]
/// under [`RULE_ANSWER`] — `keep going` there reads as consent (the
/// adversarial verifier of this fix, D4: `Should I force-push over main?`
/// got `keep going`); any other question or stop the priority's answer
/// (ATERM-KI-1016-1: `keep going` answered it, under `answer_questions =
/// false` too); and any of them, where it asks anything under
/// `answer_questions = false`, is escalated ([`ANSWERS_OFF`]). Under
/// [`Autopilot::Land`] the same words get Land's own act, as at a plain turn
/// end: a done report [`LAND`], then a [`LAND_NUDGE`], then nothing once the
/// landing is spent; irreversible words [`LAND_ANSWER`] — never Cruise's done
/// check. Under [`Autopilot::Off`] nothing reaches here ([`decide_turn_end`]).
/// The worker's [`FINISHED`] and a person's exchange left to them end the
/// task where the point is folded in ([`TurnEndState::observe`]), and
/// nothing is typed at all ([`decide_turn_end`]). Measured (the verifier of
/// the merged harness fixes, Claude Code 2.1.296's shapes): the vendor goes
/// on by itself at a five-hour limit's reset, the reply is a done report
/// long enough to scroll the limit's rows off the screen, and a loop with no
/// transcript to tie that continuation to the limit — or one that took the
/// wait over from its predecessor's ledger row — typed `keep going` into it
/// at the reset and its grace. ANOTHER CONVERSATION's words — the screen's
/// newest limit notice another limit's, or the tab's conversation holding no
/// row of this one's ([`TurnEndState::limit_elsewhere`]: a `/resume`d
/// history) — get the continue policy's act whatever they are: the limit cut
/// nothing of that conversation off, so where they ask nothing and report no
/// end it is nothing under `continue_policy = false` (ATERM-KI-1015-1 LP-1:
/// `keep going` was typed into it at the reset). `None` — the continuation,
/// as ever — where the screen still shows the standing limit (the worker
/// said nothing since: `usage-resume@v1` is right) or, in the limit's own
/// conversation, the words ask nothing and report no end (an offer, a report
/// of progress: the continuation, as continue@v1's).
fn limit_end(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> Option<TurnEndAction> {
    let said = reading.said_since_limit()?;
    let done = says_done(Some(said));
    let (asked, irreversible) = match asked_by(Some(said)) {
        Some((why, irreversible)) => (Some(why), irreversible),
        None => (None, false),
    };
    // Another conversation's words ([`TurnEndState::limit_elsewhere`]): the
    // limit cut nothing of it off, so whatever its words are they get the
    // continue policy's act — nothing under `continue_policy = false` where
    // they ask nothing (ATERM-KI-1015-1 LP-1).
    if !done && asked.is_none() && !state.limit_elsewhere(reading) {
        return None;
    }
    if let Some(act) = words_gate(cfg, asked.as_deref(), done) {
        return Some(act);
    }
    if let Some(act) = owed_challenge(state, reading, irreversible, done, budgeted) {
        return Some(act);
    }
    words_act(
        state,
        reading,
        cfg,
        asked.is_some(),
        irreversible,
        done,
        budgeted,
    )
}

/// The act at a limit's reset: the session is continued.
fn resume_limit(continue_as: &dyn Fn(&'static str) -> TurnEndAction) -> TurnEndAction {
    continue_as(RULE_LIMIT_RESUME)
}

/// The retry ladder's wait before the attempt after `attempts`: its entry,
/// the last for every attempt past it.
fn retry_wait(state: &TurnEndState, attempts: usize) -> Option<Duration> {
    crate::supervise::ladder::Ladder(&state.timing.retry_backoff).at(attempts)
}

/// Whether a session of `program` can move to another of the owner's
/// accounts ([`account_rotates`], [`login_wall_waits`]): the roster holds
/// Claude Code's logins only.
fn moves_accounts(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => false,
    }
}

/// WHETHER A RELAUNCH UNDER ANOTHER ACCOUNT MAY BE ASKED HERE AT ALL (the
/// accounts design, §6 "Which walls rotate"): never under `[harness]
/// accounts = "off"`, `resume_limits = false` or `relaunch = false`, never
/// where no host relaunches the agent ([`TurnEndReading::restartable`] —
/// which [`decide_turn_end`] also forces off at a wall the live upgrade's
/// turn end covers), never for an agent that is not Claude Code, and not
/// again after a move that could not be made while its wall stands
/// ([`TurnEndState::account_refused`]). Nor over a DRAFT: the relaunch needs
/// an empty composer, and a restart decided there would send the person's
/// words into the wall at once under this rule's name, where today they
/// wait for the reset ([`decide_turn_end`]'s `Submit`).
fn account_rotates(state: &TurnEndState, reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.accounts != crate::harness::accounts::Mode::Off && relaunch_askable(state, reading, cfg)
}

/// WHETHER THE LOGIN LIFECYCLE MAY RELAUNCH THE SESSION HERE AT ALL
/// (`docs/DESIGN-claude-accounts-v2-2026-10-15.md` §5.3): `[harness] logins`
/// on — apart from `accounts`, so the rotation's off keeps renewal working —
/// a session on a renewable login ([`TurnEndReading::login_label`]), and
/// every other condition [`account_rotates`] has: `resume_limits` and
/// `relaunch` on, a host that relaunches the agent, Claude Code, no refused
/// relaunch standing, no draft in the composer.
fn login_follows(state: &TurnEndState, reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.logins && reading.login_label.is_some() && relaunch_askable(state, reading, cfg)
}

/// What a relaunch under a login asks of a point whatever chose the login
/// ([`account_rotates`], [`login_follows`]).
fn relaunch_askable(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
) -> bool {
    cfg.resume_limits
        && cfg.relaunch
        && reading.restartable
        && moves_accounts(reading.program)
        && !state.account_unmade
        && !reading.composer.drafted()
}

/// Whether a relaunch could ever answer a lost login here, a draft aside:
/// `resume_limits` and `relaunch` on, Claude Code, and a host that relaunches
/// the agent or an upgrade holding the point for now — where
/// [`login_wall_waits`] waits.
fn relaunch_reaches(reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.resume_limits
        && cfg.relaunch
        && moves_accounts(reading.program)
        && (reading.restartable || reading.upgrading)
}

/// WHAT A RENEWED LOGIN'S END NOT WAITED AT ASKS OF THE OWNER ([`renew_step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenewStep {
    /// Renewed already ([`TurnEndReading::login_superseded`]), and only the
    /// draft in the composer holds the relaunch onto the renewed login
    /// ([`login_follows`]): once the composer is empty the session moves by
    /// itself.
    Draft,
    /// Renewed already, and nothing relaunches the session (`relaunch` or
    /// `resume_limits` off, no host that relaunches the agent; the words,
    /// one of [`BY_HAND_WHYS`], name which): no renewal reaches the process,
    /// so the owner signs it in where it stands, with `/login` in the tab —
    /// the one step always in reach (the line `accounts renew` printed may
    /// have been printed on another machine, or be gone).
    ByHand(&'static str),
}

/// A RENEWABLE LOGIN A RENEWAL REPLACED ([`TurnEndReading::login_label`],
/// [`TurnEndReading::login_superseded`], under `[harness] logins`), and what
/// holds its relaunch onto the renewed login where it is not waited at
/// ([`login_wall_waits`]): `/login` is not typed into a process whose row a
/// renewal moved on — the relaunch is what brings it the renewed login.
/// `None` too where the relaunch reaches and nothing holds it: the host
/// moves it when it advises the follow, and advising nothing the renewed
/// login ended as well. `None` for every other lost login, a renewable one
/// included: the wall's
/// own rule signs it in where it stands (`/login` once, the method picker
/// answered with the method it already is, the owner told now — the owner's
/// ruling of 2026-10-19: the harness carries the login itself).
pub(super) fn renew_step(reading: &TurnEndReading, cfg: &SupervisorConfig) -> Option<RenewStep> {
    reading.login_label.as_ref().filter(|_| cfg.logins)?;
    if !reading.login_superseded {
        return None;
    }
    Some(if relaunch_reaches(reading, cfg) {
        // With the composer empty the relaunch follows by itself where the
        // host advises it; advising nothing, the renewed login ended too,
        // and it is signed in where it stands as any other.
        if !reading.composer.drafted() {
            return None;
        }
        RenewStep::Draft
    } else if !cfg.relaunch {
        RenewStep::ByHand(BY_HAND_WHYS[0])
    } else if !cfg.resume_limits {
        RenewStep::ByHand(BY_HAND_WHYS[1])
    } else {
        RenewStep::ByHand(BY_HAND_WHYS[2])
    })
}

/// What holds a renewed login's relaunch ([`RenewStep::ByHand`]): `relaunch`
/// off, `resume_limits` off, no host that relaunches the agent. The owner's
/// notice says it ([`super::notice::RENEWED_BY_HAND`]).
pub(super) const BY_HAND_WHYS: &[&str] = &[
    "relaunch is off",
    "resume_limits is off",
    "no host relaunches it",
];

/// Whether a move to `to` is the login lifecycle's follow: `to` is the
/// session's own renewable login's label and that lifecycle may relaunch here
/// ([`login_follows`]) — the one move asked under `accounts = "off"`.
fn login_move(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    to: &str,
) -> bool {
    reading.login_label.as_deref() == Some(to) && login_follows(state, reading, cfg)
}

/// THE ACCOUNT MOVE AT A WALL, in front of what the wall's own rule answers
/// (`today`, which is the restart's `otherwise` — the point is exactly as it
/// was with no roster wherever the move is not made). `attempts`: the acts
/// the wall has had.
///
/// The move is asked where the wall's own answer is a WAIT — the reset, the
/// vendor's own auto-continue (a relaunch cancels it: the move is the better
/// answer than waiting for either) — or another restart (a bucket's
/// fallback: the account first, it keeps the model), or a retry at a wall
/// that has already answered one. Where the wall has had no act and its own
/// answer is to TYPE — its reset has come, a bucket's consent (under
/// `[harness] usage_credits` alone) — that goes
/// first, as it does before a bucket's fallback: the wall may have lifted,
/// and the continuation is the probe that costs no relaunch.
///
/// [`AccountAdvice::Wait`]: the wall's own wait, cut short at the instant
/// another account becomes usable (the point is decided again then, and the
/// host advises anew) — never lengthened, and never cut to an instant already
/// past. Past [`ACCOUNT_MOVES_PER_WINDOW`] the wall is waited out, and said.
///
/// [`AccountAdvice::Resume`]: THE OWNER LIFTED THIS WALL. The wall's own
/// continuation is typed at once in the wait's place (`continue_as`, the
/// limit-resume rule: counted as the wall's attempt and by its breaker as
/// any is), ONCE A CLEARING — a second advice for a clearing already
/// answered ([`TurnEndState::acted`]) leaves the wall's own ladder as it is.
/// If the account answers with a limit again, its new row is an ordinary
/// wall to the roster, and the advice asked next is the move. (A clearing
/// whose instant moves with the clock — one stamped ahead of it, read as
/// made now at every read — is no key that can be held: the wall's breaker,
/// [`WALL_ACTS_PER_WINDOW`], is what bounds it.)
fn account_first(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    attempts: usize,
    today: TurnEndAction,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> TurnEndAction {
    let Some(advice) = reading.advised() else {
        return today;
    };
    // What the point gets with no roster is the wall's answer PAST ITS
    // BREAKER ([`wall_breaker`], which [`act_at_point`] applies to whatever
    // this returns — but to a restart by the restart's own rule): wrapped as
    // it stood, a relaunch not made typed the wall's act past the cap.
    let today = wall_breaker(state, now, today);
    let waits = matches!(
        today,
        TurnEndAction::WaitUntil { .. } | TurnEndAction::Restart { .. }
    ) || (attempts > 0 && today.rule_id().is_some());
    if !waits || !account_rotates(state, reading, cfg) {
        return today;
    }
    match (advice, today) {
        (AccountAdvice::Move { to }, today) => {
            let (n, free) = state.account_moves_of(now);
            if n < ACCOUNT_MOVES_PER_WINDOW {
                return TurnEndAction::Restart {
                    why: Restart::Account {
                        to: to.clone(),
                        at: AccountAt::Wall,
                    },
                    rule_id: RULE_ACCOUNT,
                    otherwise: Box::new(today),
                };
            }
            match (today, free) {
                // The breaker is named only where it sets the wait: a reset
                // that comes first keeps the wall's own words.
                (TurnEndAction::WaitUntil { until, .. }, Some(free)) if free < until => {
                    TurnEndAction::WaitUntil {
                        until: free,
                        why: format!(
                            "a breaker: {n} account moves in the last {} min",
                            state.timing.window.as_secs() / 60
                        ),
                        wait: WaitKind::Other,
                    }
                }
                (today, _) => today,
            }
        }
        (
            AccountAdvice::Wait { until },
            TurnEndAction::WaitUntil {
                until: due,
                why,
                wait,
            },
        ) if *until > now => {
            let at = (*until).max(later(now, ACCOUNT_WAIT_FLOOR));
            if at < due {
                TurnEndAction::WaitUntil {
                    until: at,
                    why: "another account can be used then".to_string(),
                    wait: WaitKind::Other,
                }
            } else {
                TurnEndAction::WaitUntil {
                    until: due,
                    why,
                    wait,
                }
            }
        }
        (AccountAdvice::Wait { .. }, today) => today,
        (AccountAdvice::Resume { cleared_at }, today) => {
            if state
                .account_resumed
                .is_some_and(|typed| typed >= *cleared_at)
            {
                today
            } else {
                resume_limit(continue_as)
            }
        }
        // Never reached: `advised` reads through a probe. A wall is no
        // point a probe is made at; the wall's own answer stands.
        (AccountAdvice::Probe { .. }, today) => today,
    }
}

/// A LOST LOGIN RIGHT AFTER THIS SESSION'S OWN ACCOUNT MOVE is not the
/// person's (the accounts design, §6): the account the move chose is signed
/// out, the host has struck it ([`AccountBad::SignedOut`]) and advises the
/// next one, the way back, or — once the owner has signed that login in
/// again (`aterm harness accounts login <label>` records `ok` over the
/// strike) — the session's own login come back, a move to its own label at
/// the lost login's point ([`crate::harness::accounts::Point::LostLogin`])
/// whose relaunch runs under the restored login and owes the wall's resume:
/// the label this answers. Then no `/login` is typed and nothing is
/// escalated: the relaunch is the answer, and its `otherwise` the wait of
/// [`login_wall_waits`]. `None` with no such advice, once the move is
/// behind the session ([`TurnEndState::account_move`]), at a lost login the
/// session left ([`TurnEndState::login_left`]: the old process's notice, no
/// turn asked under the new login yet), once `/login` has been typed at this
/// wall, wherever no account move may be asked ([`account_rotates`]: a
/// draft in the composer included, which the login rule hands to its
/// person), and past [`ACCOUNT_MOVES_PER_WINDOW`].
///
/// A LOST LOGIN ON A RENEWABLE LOGIN with no move behind it
/// ([`TurnEndReading::login_label`]) is answered the same way once the
/// host advises a move to that very label — the login renewed since
/// (`crate::harness::accounts::Point::LoginEnded`) — under the logins'
/// switch ([`login_move`]) whatever `accounts` says.
pub(super) fn login_wall_moves<'a>(
    state: &TurnEndState,
    reading: &'a TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<&'a str> {
    let Some(AccountAdvice::Move { to }) = reading.advised() else {
        return None;
    };
    let moved = state.account.is_some() && account_rotates(state, reading, cfg);
    (reading.wall == Some(WallKind::Auth)
        && (moved || login_move(state, reading, cfg, to))
        && !state.login_left(reading)
        && state.wall_attempts(WallKind::Auth).unwrap_or(0) == 0
        && state.account_moves_of(now).0 < ACCOUNT_MOVES_PER_WINDOW)
        .then_some(to.as_str())
}

/// THAT LOST LOGIN WITH A MOVE STILL TO COME (another account usable at an
/// instant the host names, the bound's room, the relaunch refused) IS WAITED
/// AT: as the wall's act, `/login` would shut [`login_wall_moves`] for good,
/// and the move goes by itself when it can. Nothing is typed and no attempt
/// counted; the point is decided again at the host's [`AccountAdvice::Wait`],
/// when the oldest move leaves the window, or after
/// [`TurnEndTiming::restart_backoff`]'s last pause (never nearer than
/// [`ACCOUNT_WAIT_FLOOR`]), or at the accounts' news. Waited at only where a
/// move could ever be made — `resume_limits` and `relaunch` on, Claude Code,
/// a host that relaunches the agent or an upgrade holding the point for now
/// — as the accounts are never handled elsewhere (module header).
///
/// NOT WAITED AT WHERE NO MOVE WILL COME (the owner's ruling of 2026-10-19,
/// after v0.106.0 left a lost login to them: the harness carries the login
/// itself): the host was asked about the wall ([`TurnEndState::account_point`])
/// and advises nothing at all — no account to move to, no instant one opens.
/// The wall's own rule decides it then: `/login` once, the picker answered
/// with the method the session's login already is, the owner told now to
/// finish the sign-in in the browser. So too a RENEWABLE LOGIN
/// ([`TurnEndReading::login_label`]) with nothing renewed: it is signed in
/// where it stands; only one a renewal replaced
/// ([`TurnEndReading::login_superseded`]) is waited at, for its follow onto
/// the renewed login. `None` (the wall's own rule) also with no move of the
/// session's own standing ([`TurnEndState::account_move`]), at a lost login
/// the session left ([`TurnEndState::login_left`]), under `accounts = "off"`,
/// `resume_limits = false` or `relaunch = false`, for an agent that is not
/// Claude Code, where nothing relaunches it, once `/login` has been typed at
/// this wall, and over a draft, which the login rule hands to its person.
pub(super) fn login_wall_waits(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<TurnEndAction> {
    // Waited at after the session's own move under the rotation, and on a
    // renewable login a renewal replaced, under the logins' switch.
    let moved = state.account.is_some() && cfg.accounts != crate::harness::accounts::Mode::Off;
    let renewed = reading.login_label.is_some() && cfg.logins && reading.login_superseded;
    // NOR AFTER A MOVE AT THIS WALL THAT COULD NOT BE MADE
    // ([`TurnEndState::account_refused`]: no host relaunches it, a launch that
    // cannot be carried, a signal refused): nothing ends that wait by itself
    // — the latch lifts only at the accounts' news, work or the wall gone —
    // so waited at, the tab sat on the lost login with nothing typed and
    // nobody told. The wall's own rule signs it in where it stands.
    if reading.wall != Some(WallKind::Auth)
        || !(moved || renewed)
        || state.login_left(reading)
        || !relaunch_reaches(reading, cfg)
        || state.account_unmade
        || state.wall_attempts(WallKind::Auth).unwrap_or(0) > 0
        || reading.composer.drafted()
    {
        return None;
    }
    let (n, free) = state.account_moves_of(now);
    let (until, why) = match (reading.advised(), free) {
        // On a login a renewal replaced, a wait is the follow onto the renewed
        // login held a moment (a limit reset in flight on the account holds
        // every move off it), and advised again then.
        (Some(AccountAdvice::Wait { until }), _) if reading.login_superseded => {
            (*until, "its renewed login can be used then".to_string())
        }
        (Some(AccountAdvice::Wait { until }), _) => {
            (*until, "another account can be used then".to_string())
        }
        (Some(AccountAdvice::Move { .. }), Some(free)) if n >= ACCOUNT_MOVES_PER_WINDOW => (
            free,
            format!(
                "a breaker: {n} account moves in the last {} min",
                state.timing.window.as_secs() / 60
            ),
        ),
        // Asked, and nothing advised: no move will come — signed in here.
        (None, _) if state.account_point(reading, cfg).is_some() => return None,
        _ => (
            later(
                now,
                state
                    .timing
                    .restart_backoff
                    .last()
                    .copied()
                    .unwrap_or(ACCOUNT_WAIT_FLOOR),
            ),
            "no move can be made yet".to_string(),
        ),
    };
    Some(TurnEndAction::WaitUntil {
        until: until.max(later(now, ACCOUNT_WAIT_FLOOR)),
        why: format!("the login is gone: {why}"),
        wait: WaitKind::Other,
    })
}

/// The wall policies (module header): each wall's own rule
/// ([`wall_own_act`]), and in front of it, where the session's host advises
/// one, the move to another account ([`account_first`], [`login_wall_moves`])
/// — at a session or weekly limit and a model bucket, and at a lost login
/// right after a move, waited at where no move can be made yet
/// ([`login_wall_waits`]); never at a spend or credit wall, which is no
/// account's window. With no advice ([`TurnEndReading::account`] `None`) and
/// no move of the session's own behind it, this IS the wall's own rule.
#[allow(clippy::too_many_arguments)]
fn wall_action(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    kind: WallKind,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
    type_as: &dyn Fn(&'static str, &str) -> TurnEndAction,
) -> TurnEndAction {
    let today = wall_own_act(
        state,
        reading,
        cfg,
        now,
        kind,
        budgeted,
        continue_as,
        type_as,
    );
    match kind {
        // The notice of the account the session LEFT, repainted by the
        // conversation it resumed: the wall's resume is owed
        // ([`TurnEndState::account_resume_owed`]) and goes at once — that
        // notice's reset is the old account's, and waiting for it would
        // resume the move later than no roster would have.
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. }
            if state.account_resume && cfg.resume_limits =>
        {
            resume_limit(continue_as)
        }
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. } => {
            // The notice a limit reset left on the screen: the reset's own
            // continuation is this wall's ([`TurnEndState::reset_told`]) —
            // after a reset that went through the notice is stale, and after
            // one kept or failed the continuation is the wall's one try
            // before it is moved from or waited out.
            if let Some(act) = reset_continuation(state, cfg, now, continue_as) {
                return act;
            }
            let attempts = state.wall_attempts(kind).unwrap_or(0);
            account_first(state, reading, cfg, now, attempts, today, continue_as)
        }
        // A lost login the session left ([`TurnEndState::login_left`]) where
        // the move owes the wall's resume ([`decide_turn_end`] decides it as
        // the plain turn end where nothing is owed): the resume is typed, the
        // request that tests the new login — a lost login after it is the
        // new process's own.
        WallKind::Auth if state.login_left(reading) => resume_limit(continue_as),
        WallKind::Auth => {
            let today = login_wall_waits(state, reading, cfg, now).unwrap_or(today);
            match login_wall_moves(state, reading, cfg, now) {
                Some(to) => TurnEndAction::Restart {
                    why: Restart::Account {
                        to: to.to_string(),
                        at: AccountAt::Wall,
                    },
                    rule_id: RULE_ACCOUNT,
                    otherwise: Box::new(wall_breaker(state, now, today)),
                },
                None => today,
            }
        }
        WallKind::Spend
        | WallKind::Overloaded
        | WallKind::ApiError { .. }
        | WallKind::Context
        | WallKind::Memory => today,
    }
}

/// Why an API error `program` drew is not typed at again: Gemini CLI tries
/// every 429 and 5xx again by itself, with no end, under its spinner
/// (measured on 0.46.0: about 4, 9, 15 and 37 s apart), so an error its turn
/// ENDED on and that it does not retry (`retryable` false: a 400, a 403, a
/// 404, or `An unknown error occurred.`, the row its usage-limit dialog's
/// `Stop` leaves) meets the same refusal when sent again — after that
/// `Stop`, the quota a person chose not to keep trying. agy's turn ended on
/// the Gemini API's refusal of the request itself — a 4xx agy does not retry
/// (measured on 1.4.0 against the lab's stub: a 403 `PERMISSION_DENIED`, an
/// API the key's project has not enabled, a 404 model, a 400 location),
/// which time does not answer: sent again it meets the same; a 408 and a 429
/// are time's, and agy's 500, which it does not retry either, is the
/// server's, so each stays on the ladder. `None` for every other program and
/// every other error: the retry ladder.
fn not_retried(program: Program, kind: WallKind) -> Option<&'static str> {
    match program {
        Program::Gemini => matches!(
            kind,
            WallKind::ApiError {
                retryable: false,
                ..
            }
        )
        .then_some(
            "Gemini CLI retries what time answers; after its usage limit's Stop, see /stats model",
        ),
        Program::Agy => matches!(
            kind,
            WallKind::ApiError {
                code: Some(400..=407 | 409..=428 | 430..=499),
                retryable: false,
                ..
            }
        )
        .then_some("the Gemini API refused agy's request; sent again it meets the same"),
        Program::Claude | Program::Codex | Program::Muse | Program::Generic => None,
    }
}

/// Whether a wall's notice names USAGE CREDITS (`requires usage credits`,
/// `now uses usage credits`, `out of usage credits`, or `extra usage`, their
/// older name) as what would lift it: a bucket only credits lift
/// ([`wall_own_act`]). A command's name (`/usage-credits`) is no such word.
fn names_usage_credits(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("usage credits") || lower.contains("extra usage")
}

/// THE FALLBACK AT A SPEND WALL THAT NAMES A MODEL, under `[harness]
/// usage_credits = false` (`You're out of usage credits. Run /usage-credits
/// to keep using Fable 5.1 or /model to switch models.`): relaunched onto
/// `[harness] model_fallback` as that model's bucket would be
/// ([`credits_fallback`]), `otherwise` its own act. `None`: no model named,
/// or no fallback to make.
fn credits_wall(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    message: &str,
    otherwise: &dyn Fn() -> TurnEndAction,
) -> Option<TurnEndAction> {
    let from = bucket_model(message)?;
    let to = credits_fallback(state, reading, cfg, &from)?;
    Some(TurnEndAction::Restart {
        why: Restart::Model { to },
        rule_id: RULE_MODEL_FALLBACK,
        otherwise: Box::new(otherwise()),
    })
}

/// Each wall's own rule (module header): what the point gets with no account
/// to move to.
#[allow(clippy::too_many_arguments)]
fn wall_own_act(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    kind: WallKind,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
    type_as: &dyn Fn(&'static str, &str) -> TurnEndAction,
) -> TurnEndAction {
    let track = state.wall.as_ref().filter(|t| t.class == class_of(kind));
    let since = track.map_or(now, |t| t.since);
    let attempts = track.map_or(0, |t| t.attempts);
    let esc = |reason: String| TurnEndAction::Escalate { reason };
    let msg = &reading.wall_message;
    // A LIMIT WAITED OUT. Where the vendor resumes by itself (its countdown
    // armed: [`TurnEndReading::resumes_by_itself`]) — whichever limit it
    // counts down to, a usage window or a model bucket under 2.1.295's
    // generic banner — it is waited out with nobody told, and continued only
    // if it has not resumed [`TurnEndTiming::auto_resume_grace`] past the
    // named time (with none, the longest limit back-off); with
    // `resume_limits` off, nothing. Otherwise the reset and the grace
    // ([`resume_at_reset`]), or the owner told where `resume_limits` is off.
    // A countdown read before the agent's process started is the gone
    // process's, its notice redrawn from the conversation: nobody goes on by
    // itself ([`TurnEndState::agent_started`]).
    let by_itself = goes_on_by_itself(state, track, reading);
    let wait_out = || {
        if by_itself {
            if !cfg.resume_limits {
                return TurnEndAction::Nothing;
            }
            let t = &state.timing;
            let due = match reading.reset_at {
                Some(at) => later(at, t.auto_resume_grace),
                None => later(since, t.limit_backoff.last().copied().unwrap_or_default()),
            };
            if now < due {
                return TurnEndAction::WaitUntil {
                    until: due,
                    why: format!("{} goes on by itself", kind.name()),
                    wait: WaitKind::Other,
                };
            }
            return continue_as(RULE_LIMIT_RESUME);
        }
        if cfg.resume_limits {
            resume_at_reset(state, reading, now, since, attempts, kind, continue_as)
        } else {
            esc(format!("{}: {msg} (resume_limits is off)", kind.name()))
        }
    };
    // A WALL ONLY USAGE CREDITS LIFT, under `[harness] usage_credits = false`
    // (the owner, 2026-10-19): a spend wall, or a bucket whose notice names
    // usage credits (`requires usage credits`, `now uses usage credits`,
    // `out of usage credits`), with no reset named. Its entitlement ladder's
    // `keep going` (`resume_at_reset`) is served only on credits — a top-up,
    // an auto-reload, the month's credit reset — so none is typed: the
    // fallback where the notice names a model ([`credits_wall`]), else the
    // owner told and nothing typed. A notice that names a reset, or a
    // countdown of the vendor's own, is waited out to it as ever: the
    // subscription's window.
    let credits_only = !cfg.usage_credits
        && reading.reset_at.is_none()
        && !by_itself
        && match kind {
            WallKind::Spend => true,
            WallKind::ModelBucket { .. } => names_usage_credits(msg),
            _ => false,
        };
    let wait_out = || {
        if credits_only {
            esc(format!(
                "{}: {msg} (only usage credits lift it, and `[harness] usage_credits` is off: \
                 nothing is typed)",
                kind.name()
            ))
        } else {
            wait_out()
        }
    };
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => {
            if !cfg.retry_api_errors {
                return esc(format!("{}: {msg} (retry_api_errors is off)", kind.name()));
            }
            // Why goes ahead of the wall's words, which the escalation's
            // reason is cut after (120 cells).
            if let Some(why) = not_retried(reading.program, kind) {
                return esc(format!("{} not retried ({why}): {msg}", kind.name()));
            }
            let cause = match kind {
                WallKind::ApiError { cause, .. } => cause,
                _ => ApiCause::Server,
            };
            let ups = track.map_or(0, |t| t.ups);
            let t = &state.timing;
            let act =
                |rule: &'static str| type_as(rule, &wall_retry_text(rule, kind, reading.program));
            let wait_then = |due: Instant, why: String, act: TurnEndAction| {
                if now < due {
                    TurnEndAction::WaitUntil {
                        until: due,
                        why,
                        wait: WaitKind::Retry,
                    }
                } else {
                    act
                }
            };
            // A ladder's rung for the act after `n`, from this appearance.
            let rung = |ladder: &[Duration], n: usize| {
                crate::supervise::ladder::Ladder(ladder)
                    .at(n)
                    .map_or(now, |w| later(since, w))
            };
            match (cause, reading.reach) {
                // Measured reachable again: continued at once, then spaced on
                // the short ladder by this measure's acts (meeting the wall
                // again means the measure was wrong or the API still fails).
                // Not for a certificate or proxy refusal: the host verifies
                // against the platform trust store and the agent against its
                // own (`NODE_EXTRA_CA_CERTS`), so the host can read Up while
                // the agent is refused. That stays on the ladder.
                (ApiCause::Unreachable, Reach::Up { .. }) => wait_then(
                    ups.checked_sub(1)
                        .map_or(since, |n| rung(&t.net_backoff, n)),
                    format!("api-error back but met again: retry {}", ups + 1),
                    act(RULE_API_BACK),
                ),
                // Measured definitely down: nothing typed into it for up to
                // the hold, then one try all the same.
                (
                    ApiCause::Unreachable | ApiCause::Config | ApiCause::CutOff,
                    Reach::Down { .. },
                ) => wait_then(
                    later(since, t.down_hold),
                    "api-error: the API is unreachable; continuing when it is reachable, or \
                         at this try"
                        .to_string(),
                    act(RULE_API_RETRY),
                ),
                (ApiCause::Unreachable, Reach::Unknown)
                | (ApiCause::Config, Reach::Unknown | Reach::Up { .. }) => wait_then(
                    rung(&t.net_backoff, attempts),
                    format!("api-error retry {}", attempts + 1),
                    act(RULE_API_RETRY),
                ),
                // A reply cut off: continued at once, then on the short
                // ladder.
                (ApiCause::CutOff, _) => wait_then(
                    attempts
                        .checked_sub(1)
                        .map_or(since, |n| rung(&t.net_backoff, n)),
                    format!("api-error cut off: retry {}", attempts + 1),
                    act(RULE_API_CUTOFF),
                ),
                // The server's own failure: its ladder, whatever the reach.
                (ApiCause::Server, _) => wait_then(
                    rung(&t.retry_backoff, attempts),
                    format!("{} retry {}", kind.name(), attempts + 1),
                    act(RULE_API_RETRY),
                ),
                // The request itself refused: one cheap retry, then the slow
                // pace of a wall only a person ends; never given up (a person
                // is told once, `policy/exception.rs`).
                (ApiCause::Request, _) => wait_then(
                    rung(&t.refused_backoff, attempts),
                    format!("api-error refused: retry {}", attempts + 1),
                    act(RULE_API_RETRY),
                ),
                // A reply past its output token maximum: continued at once,
                // then on the server's ladder — climbed across the work each
                // try leads to ([`TurnEndState::observe`]), since that work is
                // what the wall cuts off.
                (ApiCause::Overlong, _) => wait_then(
                    attempts
                        .checked_sub(1)
                        .map_or(since, |n| rung(&t.retry_backoff, n)),
                    format!("api-error overlong: retry {}", attempts + 1),
                    act(RULE_API_CUTOFF),
                ),
            }
        }
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => {
            if !by_itself && !cfg.resume_limits && kind != WallKind::Spend {
                // The limit episode tells the person (the loop's
                // `limit_after`).
                return TurnEndAction::Nothing;
            }
            // A spend wall that names a model (`You're out of usage credits.
            // Run /usage-credits to keep using Fable 5.1 or /model to switch
            // models.`) with the key off: the fallback, as its bucket's.
            if credits_only && let Some(act) = credits_wall(state, reading, cfg, msg, &wait_out) {
                return act;
            }
            wait_out()
        }
        // A bucket asking consent to use usage credits: accepted by
        // continuing (the vendor's confirm box is the approval policy's), and
        // only under `[harness] usage_credits`. Off (the default), hitting it
        // again, or with continuation limited: switched or waited out like
        // any bucket — the subscription kept; one only credits lift, with the
        // key off, is switched or told, never waited out on the entitlement
        // ladder (`wait_out` above).
        WallKind::ModelBucket { consent } => {
            if consent && attempts == 0 && cfg.continue_policy && cfg.usage_credits {
                return continue_as(RULE_CONSENT);
            }
            let Some(fallback) = &cfg.model_fallback else {
                return wait_out();
            };
            // The bucket's reset has come and nothing was tried at it yet:
            // the wall may have lifted, and the continuation is the probe. A relaunch the host keeps
            // deferring (the agent's own background work runs) is no reason
            // to sit past a reset; if the wall answers, the fallback is
            // asked for again.
            if attempts == 0
                && reading.reset_at.is_some_and(|r| {
                    now >= later(r, state.timing.grace_for(reading.program) + reading.jitter)
                })
            {
                return wait_out();
            }
            if state.model.is_some()
                || state.fallback_unmade
                || !reading.restartable
                || bucket_model(msg).as_deref() == Some(fallback.as_str())
            {
                // Switched already, a relaunch that could not be made or no
                // host to make one, or the fallback is the model at its
                // limit: the reset is waited out.
                return wait_out();
            }
            TurnEndAction::Restart {
                why: Restart::Model {
                    to: fallback.clone(),
                },
                rule_id: RULE_MODEL_FALLBACK,
                otherwise: Box::new(wait_out()),
            }
        }
        WallKind::Context => {
            if !cfg.compact_on_context_wall {
                return esc(format!(
                    "context full: {msg} (compact_on_context_wall is off)"
                ));
            }
            if let Some(wait) = attempts.checked_sub(1).and_then(|n| retry_wait(state, n)) {
                let due = later(since, wait);
                if now < due {
                    return TurnEndAction::WaitUntil {
                        until: due,
                        why: format!("context still full after /compact ({attempts})"),
                        wait: WaitKind::Other,
                    };
                }
            }
            let Some(compact) = compact_command(reading.program) else {
                return esc(format!(
                    "context full: {msg} ({} has no compaction aterm types)",
                    reading.program.product()
                ));
            };
            budgeted(TurnEndAction::TypeCommand {
                command: compact.to_string(),
                rule_id: RULE_COMPACT,
                then: Then::Continue,
            })
        }
        // Decided ahead of every gate in `decide_turn_end`; the arm keeps
        // the match exhaustive with the same act.
        WallKind::Memory => memory_restart(cfg, reading),
        WallKind::Auth => {
            // `/login` once per loss, and never into a loss whose `/login`
            // was left — whoever typed it: a person's Esc out of their own
            // sign-in is their answer, which the harness does not override
            // ([`LoginBooks::cancelled`]).
            if attempts > 0 || track.is_some_and(|t| t.login.cancelled) {
                return login_opening_wait(state, now).unwrap_or(TurnEndAction::Nothing);
            }
            // A LOGIN A RENEWAL REPLACED is not signed in where it stands
            // ([`renew_step`]): where it is not waited at for its follow
            // ([`login_wall_waits`]: no relaunch to follow it with, a draft
            // in the composer) the owner is told what holds the relaunch onto
            // the renewed login, in words the tab's badge keeps whole. Every
            // other lost login, a renewable one included, is signed in here.
            match renew_step(reading, cfg) {
                Some(RenewStep::Draft) => {
                    return esc(
                        "the login was renewed; a draft stands in the composer: send or \
                         clear it, and the tab moves onto the new login"
                            .to_string(),
                    );
                }
                Some(RenewStep::ByHand(why)) => {
                    return esc(format!(
                        "the login was renewed, but {why}: type /login in the tab and sign in"
                    ));
                }
                None => {}
            }
            // ONE SIGN-IN PER LOGIN (the review's F7: every tab on a shared
            // login typed its own `/login`, and the owner met as many browser
            // sign-ins). Another tab carrying this login's sign-in is waited
            // on, nothing typed; once the login came back there, this tab's
            // turn is delivered again — once: met by the wall again, its own
            // `/login` follows.
            match &reading.login_shared {
                Some(LoginShared::Back { .. }) if !track.is_some_and(|t| t.login.back_typed) => {
                    return type_as(RULE_LOGIN_BACK, &login_back_text(reading.program));
                }
                Some(LoginShared::Carried { by }) => {
                    return TurnEndAction::WaitUntil {
                        until: later(now, LOGIN_SHARED_LOOK),
                        why: format!("the login's sign-in is {by}'s"),
                        wait: WaitKind::Other,
                    };
                }
                Some(LoginShared::Back { .. }) | None => {}
            }
            // An API failure the worker cannot retry past: typed wherever API
            // errors are retried (`retry_api_errors`, on by default), and only
            // into a program that signs in from its own TUI.
            // How it signs in goes ahead of the wall's words where they would
            // cut it off ([`how_leads`]).
            let how = sign_in_how(reading.program, msg);
            let leads = how_leads(reading.program);
            if !cfg.retry_api_errors {
                return esc(if leads {
                    format!("the login is gone, {how} (retry_api_errors is off): {msg}")
                } else {
                    format!("the login is gone: {msg} (retry_api_errors is off; {how})")
                });
            }
            let Some(login) = login_command(reading.program) else {
                return esc(if leads {
                    format!("the login is gone, {how}: {msg}")
                } else {
                    format!("the login is gone: {msg} ({how})")
                });
            };
            // Told by the owner's notice, not the loop's escalation: one
            // surface ([`super::exception::exception_for`]).
            budgeted(TurnEndAction::TypeCommand {
                command: login.to_string(),
                rule_id: RULE_LOGIN,
                then: Then::Nothing,
            })
        }
    }
}

/// Claude Code's critical-memory banner: nothing typed is read and no reset
/// ends it, so the remedy is a restart ([`TurnEndAction::Restart`]): the host
/// ends the agent at its idle point, relaunches it on its own conversation and
/// tells it to carry on. The decider returns this at any point, ahead of the
/// gates typed acts wait on; the loop asks only at a point (under a running
/// spinner the server's `wall:memory` menu row is the notice).
/// `[harness] relaunch = false`, or a loop no host restarts for (`drive
/// watch`), escalates with the remedy instead.
///
/// The remedy names the agent's own conversation ([`TurnEndReading::resume`],
/// `claude --resume <id>`) or no command, and quotes only the banner's fact
/// ([`aterm_phase::memory_banner_head`]); never `claude --continue`, which
/// resumes the directory's newest conversation.
///
/// The loop also sets this wall at an idle point when the host measures the
/// agent past `[harness] memory_ceiling` while the machine is under memory
/// pressure and this agent holds the most (`supervise/memory.rs`,
/// `MemoryOver::acts`). Past the ceiling alone is only a warning.
fn memory_restart(cfg: &SupervisorConfig, reading: &TurnEndReading) -> TurnEndAction {
    let banner = aterm_phase::memory_banner_head(&reading.wall_message);
    let reason = match reading.resume.as_deref() {
        // The clause the escalation keeps whole or drops, never cuts
        // ([`crate::harness::resume::never_cut`]).
        Some(resume) => format!(
            "memory critical: restart it{}{resume}: {banner}",
            crate::harness::resume::THEN_RESUME_WITH
        ),
        None => format!("memory critical: restart it: {banner}"),
    };
    if !cfg.relaunch {
        return TurnEndAction::Escalate {
            reason: format!("{reason} (relaunch is off)"),
        };
    }
    if !reading.restartable {
        return TurnEndAction::Escalate { reason };
    }
    TurnEndAction::Restart {
        why: Restart::Memory,
        rule_id: RULE_MEMORY_RESTART,
        otherwise: Box::new(TurnEndAction::Escalate { reason }),
    }
}

#[cfg(test)]
#[path = "turn_end_tests.rs"]
mod tests;
