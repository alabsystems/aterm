// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure supervisor turn-end policy decider (`TurnEndState`, `TurnEndReading`, `TurnEndDecision`).
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
use super::needs::{self, NEEDS_ACK, NEEDS_HINT};
use super::reset::{RESET_CONTINUES_WAIT, ResetOutcome};
use super::sleeps::InTurn;
use super::trace::{NoTrace, Outcome, Trace, note};
use crate::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};
use crate::harness::harness_mark;

/// A continuation typed at an ordinary turn end ([`decide_turn_end`]).
pub const RULE_CONTINUE: &str = "continue@v1";
/// The [`DONE_CHECK`] typed under Cruise where the worker claims it is done.
pub const RULE_DONE_CHECK: &str = "continue-done-check@v1";
/// RETIRED: the open-items challenge older builds typed where a done report named open work ([`OPEN_ITEMS_HEAD`], [`OPEN_ITEMS_HEADS_BEFORE`]).
pub const RULE_OPEN_ITEMS: &str = "continue-open-items@v1";
/// Retired rule tag for the legacy uncommitted-work continuation challenge.
pub const RULE_UNCOMMITTED: &str = "continue-uncommitted@v1";

/// Whether `rule` is one of the retired challenges ([`RULE_OPEN_ITEMS`], [`RULE_UNCOMMITTED`]).
#[must_use]
pub fn is_challenge_rule(rule: &str) -> bool {
    rule == RULE_OPEN_ITEMS || rule == RULE_UNCOMMITTED
}
/// The journal's name for a person's own exchange left to them ([`TurnEndState::left_to_person`]).
pub const RULE_LEFT: &str = "left-to-person@v1";
/// CRUISE'S DEFEND-AND-FIND-MORE PROMPT.
pub const DONE_CHECK: &str = concat!(
    harness_mark!(),
    " Autopilot is Cruise: done is never the end. Land every open item, then look harder: \
     review your own work adversarially, widen the checks, fix what they find, close every \
     gap. Put what only a person can give on a line starting NEEDS YOU: and carry on. If you \
     are certain nothing is left, defend it in a few lines starting Nothing left: and keep \
     looking."
);

/// RETIRED: the done check of `4bff4848b4`.
pub const DONE_CHECK_COMMITTED_BEFORE: &str = "Before you stop: do every open item, skipped \
     check, follow-up and next step, and verify it (build, test, measure where numbers were \
     asked for), then commit and push it (merged to main where this repository lands work). \
     Report whether the task is complete, the outcome and its verification in ordinary \
     language. It is complete only when nothing of the job is left: nothing uncommitted, \
     untracked, stashed or unpushed, and no open item, follow-up or next step in your report, \
     a blocked one included. Otherwise continue.";

/// Retired ordinary-language done-check prompt from `69a8f68178`.
pub const DONE_CHECK_ORDINARY_BEFORE: &str = "Before you stop: do every open item, \
     skipped check, follow-up and next step you can still do yourself now, and verify it \
     (build, test, measure where numbers were asked for, commit and land where this repository \
     lands work). Report whether the task is complete, the outcome, verification, and any \
     remaining blockers in ordinary language. Stop only when everything left needs a person (a \
     credential, a decision, physical access) or cannot be done on this host, each item named \
     with its blocker; a slow check is not a blocker. Otherwise continue.";

/// RETIRED: the done check of `0b1abdabf3`.
pub const DONE_CHECK_WORDS_BEFORE: &str = "Before you stop: do every open item, skipped check, \
     follow-up and next step you can still do yourself now, and verify it (build, test, measure \
     where numbers were asked for, commit and land where this repository lands work). Close \
     your reply by saying the work is complete, only when everything left needs a person (a \
     credential, a decision, physical access) or cannot be done on this host, each item named \
     with its blocker; a slow check is not a blocker. Otherwise continue.";

/// RETIRED: the last completion check that asked for the token, retained only for reading existing conversations and their task results.
pub const DONE_CHECK_MARKER_BEFORE: &str = "Before you stop: do every open item, skipped \
     check, follow-up and next step you can still do yourself now, and verify it (build, test, \
     measure where numbers were asked for, commit and land where this repository lands \
     work). End your reply with ATERM-TASK-DONE on a line by itself only when everything \
     left needs a person (a credential, a decision, physical access) or cannot be done on \
     this host, each item named with its blocker; a slow check is not a blocker. Otherwise \
     continue.";

/// RETIRED: the primer's definition of finished as main's check asked it before the merge of `f09ef3b15` asked for the token LAST.
pub const DONE_CHECK_OPEN_BEFORE: &str = "Before you stop: do every open item, skipped check, \
     follow-up and next step you can still do yourself now, and verify it (build, test, \
     measure where numbers were asked for, commit and land where this repository lands \
     work). Reply with ATERM-TASK-DONE on a line by itself only when everything left needs \
     a person (a credential, a decision, physical access) or cannot be done on this host, \
     each item named with its blocker; a slow check is not a blocker. Otherwise continue.";

/// RETIRED: the done check's words that asked for the token LAST until the check took the primer's definition of finished.
pub const DONE_CHECK_LAST_BEFORE: &str = "If the task is finished and verified, end your reply \
     with ATERM-TASK-DONE on a line by itself and stop; otherwise continue.";

/// RETIRED: the done check's words from `3e34e2f6d` until the check asked for the token as the reply's LAST line.
pub const DONE_CHECK_TOKEN_BEFORE: &str = "If the task is finished and verified, reply with \
     ATERM-TASK-DONE on a line by itself and stop; otherwise continue.";

/// RETIRED: the done check main's tip typed from `d92d817dc` until the check took the primer's definition of finished.
pub const DONE_CHECK_NOTHING_OPEN_BEFORE: &str = "If the task is finished, verified and has \
     nothing left open, reply with ATERM-TASK-DONE on a line by itself and stop; otherwise \
     continue with what is open.";

/// RETIRED: the done check's words before `3e34e2f6d`.
pub const DONE_CHECK_BEFORE: &str =
    "If the task is finished and verified, reply DONE and stop; otherwise continue.";

/// EVERY DONE CHECK AN OLDER BUILD TYPED, newest first.
pub const RETIRED_DONE_CHECKS: [&str; 9] = [
    DONE_CHECK_COMMITTED_BEFORE,
    DONE_CHECK_ORDINARY_BEFORE,
    DONE_CHECK_WORDS_BEFORE,
    DONE_CHECK_MARKER_BEFORE,
    DONE_CHECK_OPEN_BEFORE,
    DONE_CHECK_NOTHING_OPEN_BEFORE,
    DONE_CHECK_LAST_BEFORE,
    DONE_CHECK_TOKEN_BEFORE,
    DONE_CHECK_BEFORE,
];

/// What follows [`DONE_CHECK`] where the done report also names an irreversible act ([`Said::Irreversible`], D1).
pub const DONE_CHECK_REVERSIBLE: &str =
    "Approve nothing irreversible: delete, overwrite and force-push nothing.";

/// THE RETIRED DONE TOKEN.
pub const FINISHED: &str = "ATERM-TASK-DONE";

/// Managed short-turn allowance before short-turn backoff applies after a done check.
pub const MANAGED_SHORT_TURNS: u32 = 3;

/// THE MARK OF THE WORKER'S LAST WORDS (`tail`, [`TurnEndReading::said_tail`] as its program's reader reads them).
#[must_use]
pub fn said_mark(tail: Option<&str>) -> Option<u64> {
    let letters = said_letters(tail);
    (!letters.is_empty()).then(|| aterm_hash::fnv1a_64(letters.as_bytes()))
}

fn said_letters(tail: Option<&str>) -> String {
    let mut letters = String::new();
    letters.extend(
        tail.unwrap_or_default()
            .chars()
            .filter(|c| c.is_alphanumeric()),
    );
    letters
}

fn words_end_with(tail: Option<&str>, mark: Option<u64>, len: usize) -> bool {
    let Some(mark) = mark else {
        return false;
    };
    let letters = said_letters(tail);
    letters
        .len()
        .checked_sub(len)
        .and_then(|from| letters.get(from..))
        .is_some_and(|end| aterm_hash::fnv1a_64(end.as_bytes()) == mark)
}

fn without_act_rows<'a>(text: &'a str, act: Option<&str>) -> &'a str {
    let act = said_letters(act);
    if act.is_empty() {
        return text;
    }
    let mut letters = String::new();
    let mut at = 0;
    let mut cut = None;
    for line in text.split_inclusive('\n') {
        letters.extend(line.chars().filter(|c| c.is_alphanumeric()));
        at += line.len();
        if letters.len() > act.len() {
            break;
        }
        if !letters.is_empty() && act.ends_with(&letters) {
            cut = Some(at);
        }
    }
    cut.map_or(text, |at| text[at..].trim_start_matches('\n'))
}

/// THE MARK OF A SCREEN'S LETTERS ([`TurnEndReading::screen_mark`]).
#[must_use]
pub fn screen_mark(rows: &[String]) -> u64 {
    let mut letters = String::new();
    for row in rows {
        letters.extend(row.chars().filter(|c| c.is_alphabetic()));
    }
    aterm_hash::fnv1a_64(letters.as_bytes())
}

/// THE MARK OF A READING'S CONVERSATION ([`TurnEndReading::screen_mark`]).
#[must_use]
pub fn conversation_mark(reading: &aterm_phase::Reading, rows: &[String]) -> u64 {
    screen_mark(
        reading
            .composer
            .as_ref()
            .and_then(|(caret, _)| rows.get(..*caret))
            .unwrap_or(rows),
    )
}

fn reply_stands(reading: &TurnEndReading) -> bool {
    turn_opener(reading).is_some() || (!reading.message_drawn && reading.said_tail.is_some())
}

/// THE MESSAGE THAT OPENED THE TURN `reading`'s last words end, hashed.
#[must_use]
pub fn turn_opener(reading: &TurnEndReading) -> Option<u64> {
    reading.answered.map(|a| a.prompt).or(reading.opened_by)
}

/// Whether one line of a report is [`FINISHED`] alone (Markdown punctuation allowed).
#[must_use]
pub fn is_finished_line(line: &str) -> bool {
    !line.trim_start().starts_with(['>', '▎', '│', '┃'])
        && line.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) == FINISHED
}

/// `text` with its [`is_finished_line`] lines dropped, trimmed.
#[must_use]
pub fn without_finished_lines(text: &str) -> String {
    let kept: Vec<&str> = text.lines().filter(|l| !is_finished_line(l)).collect();
    kept.join("\n").trim().to_string()
}

/// The heads a report lists open work under, lowercased ([`open_work`]).
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

const HEAD_TAIL: usize = 60;

const OPEN_NOUNS: &[&str] = &[
    "items", "item", "issues", "work", "tasks", "steps", "points", "to do", "yet",
];

/// The words that leave work open wherever they stand in a sentence ([`open_work`]).
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

/// WHAT LAND READS AS OPEN BESIDE [`OPEN_STATUS`] ([`open_work_strict`]).
pub const LAND_STATUS: &[&str] = &[
    "not landed",
    "unlanded",
    "not pushed",
    "unpushed",
    "not merged",
    "unmerged",
    "not committed",
    "uncommitted",
    "parked",
    "wip",
    "work in progress",
    "left on a branch",
    "on its own branch",
    "follow-up",
    "follow-ups",
    "follow up",
    "followup",
    "followups",
    "follow-on",
    "follow-ons",
    "follow on",
    "follow ons",
    "untracked",
    "unstaged",
    "modified files",
    "modified file",
    "without landing",
    "without merging",
    "without pushing",
    "without committing",
    "without being merged",
    "without being applied",
    "without applying",
    "moved again before",
    "moved before i pushed",
    "moved before i could",
    "before i could push",
    "dry run",
    "dry-run",
    "--dry-run",
    "staged for commit",
    "staged for",
    "is staged",
    "are staged",
    "staged changes",
    "is modified",
    "are modified",
    "still modified",
    "left modified",
    "shows m",
    "still on disk",
    "left the branch",
    "left the branches",
    "left the worktree",
    "left a branch",
];

/// A LATER DAY ([`LAND_FAMILIES`]).
pub const LAND_LATER: &[&str] = &[
    "for later",
    "later",
    "tomorrow",
    "tonight",
    "next week",
    "next month",
    "next sprint",
    "next quarter",
    "next round",
    "next pass",
    "next iteration",
    "in the next session",
    "for the next session",
    "a later session",
    "a future session",
    "another session",
    "another day",
    "another time",
    "in the future",
    "future work",
    "eventually",
    "someday",
    "some day",
    "at some point",
    "down the line",
    "for now",
    "for the time being",
    "until later",
    "whenever",
    "scheduled for",
    "planned for",
    "is planned",
    "is scheduled",
    "slated for",
    "are planned",
    "in due course",
    "when time permits",
    "time permitting",
    "pencilled in",
    "penciled in",
    "next release",
    "next restart",
    "next nightly",
    "next deploy",
    "next rebuild",
    "by monday",
    "by tuesday",
    "by wednesday",
    "by thursday",
    "by friday",
    "by the weekend",
    "by end of day",
    "by eod",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "this morning",
    "this afternoon",
    "this evening",
    "in the morning",
    "in the afternoon",
    "in the evening",
    "in a bit",
    "in a while",
    "in a minute",
    "in a few minutes",
    "in a few hours",
    "in a few days",
    "in an hour",
    "shortly",
    "asap",
    "over the weekend",
    "this weekend",
    "next time",
    "next session picks",
    "next session will",
    "next session can",
    "phase 2",
    "phase two",
    "phase ii",
    "post-freeze",
    "after the freeze",
    "next milestone",
    "next drop",
    "set for",
    "penciled for",
    "pencilled for",
    "is in the pipeline",
    "are in the pipeline",
    "still in the pipeline",
    "goes out with",
    "go out with",
    "comes with",
    "come with",
    "lands with",
    "land with",
    "ships with",
    "ship with",
    "when i get a chance",
    "when i'm back",
    "when i am back",
    "bumped to",
    "pushed back",
    "pushed out to",
    "afterwards",
    "afterward",
];

const DATED: &[&str] = &[
    "afterwards",
    "afterward",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "this morning",
    "this afternoon",
    "this evening",
    "in the morning",
    "in the afternoon",
    "in the evening",
    "shortly",
];

const LATER_WITH: &[&str] = &[
    "next",
    "release",
    "drop",
    "train",
    "milestone",
    "sprint",
    "version",
    "update",
    "batch",
    "cut",
    "rc",
    "monday's",
    "tuesday's",
    "wednesday's",
    "thursday's",
    "friday's",
];

#[derive(Clone, Copy, Debug)]
enum Reads {
    Anywhere,
    Alone,
    Beside(&'static [&'static str]),
    Exception,
    Later,
    Intent,
    Until,
    Elided,
    Gerund,
    Still,
    Ref,
    Fraction,
    NotDone,
    Lost,
    Better,
    Concession,
    State,
    End,
    Count,
    OneMore,
    Only,
    Elliptic,
    Absent,
    Nobody,
    Unlanded,
    Undone,
    Onto,
    Most,
    Dropped,
    OnBranch,
    Change,
    Percent,
    Party,
    OnlyTail,
    Qualified,
    Tally,
    Without,
    Approval,
    Rerun,
    Status,
}

impl Reads {
    const fn common(self) -> bool {
        matches!(
            self,
            Self::End
                | Self::Party
                | Self::OnlyTail
                | Self::Qualified
                | Self::Count
                | Self::OneMore
                | Self::Only
                | Self::Elliptic
                | Self::Absent
                | Self::Nobody
                | Self::Unlanded
                | Self::Undone
                | Self::Onto
                | Self::Most
                | Self::OnBranch
                | Self::Change
                | Self::Percent
                | Self::Tally
                | Self::Without
                | Self::Approval
                | Self::Status
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LandFamily {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "named by the defense table's mutation check")
    )]
    pub(crate) name: &'static str,
    rows: &'static [&'static str],
    reads: Reads,
    settled_by: &'static [&'static str],
    says: Option<&'static str>,
    phrases: Phrases,
}

impl LandFamily {
    const fn new(
        name: &'static str,
        rows: &'static [&'static str],
        reads: Reads,
        settled_by: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            rows,
            reads,
            settled_by,
            says: None,
            phrases: Phrases::of(rows),
        }
    }

    const fn saying(self, says: &'static str) -> Self {
        Self {
            says: Some(says),
            ..self
        }
    }
}

const SETTLED: &[&str] = &[
    "merged",
    "landed",
    "done",
    "fixed",
    "resolved",
    "closed",
    "approved",
    "reviewed",
    "re-enabled",
    "reenabled",
    "enabled",
    "passes",
    "pass",
    "passed",
    "passing",
    "green",
    "removed",
    "deleted",
    "dropped",
    "gone",
    "addressed",
    "finished",
    "completed",
    "complete",
    "stable",
    "deflaked",
    "unblocked",
    "cleared",
    "applied",
    "popped",
    "answered",
    "empty",
    "investigated",
    "explained",
    "succeeded",
    "succeeds",
];

const LANDS: &[&str] = &[
    "merged",
    "landed",
    "squash-merged",
    "rebase-merged",
    "auto-merged",
    "upstreamed",
];

const SHIPS: &[&str] = &[
    "merged",
    "landed",
    "squash-merged",
    "rebase-merged",
    "auto-merged",
    "upstreamed",
    "published",
    "released",
    "shipped",
    "deployed",
];

const MENDS: &[&str] = &[
    "fixed",
    "resolved",
    "addressed",
    "closed",
    "removed",
    "deleted",
    "corrected",
    "done",
    "gone",
    "cleared",
    "finished",
    "completed",
    "complete",
    "investigated",
    "explained",
    "answered",
];

const CHECKS: &[&str] = &[
    "passes",
    "pass",
    "passed",
    "passing",
    "green",
    "fixed",
    "resolved",
    "succeeded",
    "succeeds",
    "stable",
    "deflaked",
    "re-enabled",
    "reenabled",
    "enabled",
    "removed",
    "deleted",
    "done",
    "finished",
    "completed",
    "complete",
    "investigated",
    "explained",
];

const DEFLAKED: &[&str] = &["fixed", "resolved", "deflaked", "removed", "deleted"];

const FLAKY: &[&str] = &[
    "flaky",
    "flakes",
    "flaking",
    "intermittently",
    "intermittent",
    "sporadically",
    "unstable",
];

const STEPS: &[&str] = &[
    "passes",
    "pass",
    "passed",
    "passing",
    "green",
    "fixed",
    "resolved",
    "succeeded",
    "succeeds",
    "merged",
    "landed",
];

const REVIEWED: &[&str] = &[
    "approved",
    "reviewed",
    "done",
    "given",
    "answered",
    "received",
    "addressed",
    "resolved",
    "finished",
    "completed",
    "decided",
];

const REVIEW_NOUNS: &[&str] = &[
    "review",
    "reviews",
    "approval",
    "approvals",
    "sign-off",
    "signoff",
    "feedback",
    "question",
    "questions",
    "decision",
    "decisions",
    "answer",
    "comment",
    "comments",
    "thread",
    "threads",
];

const CHANGE_NOUNS: &[&str] = &[
    "pr", "prs", "mr", "mrs", "request", "requests", "branch", "branches", "commit", "commits",
    "patch", "patches", "stash", "draft",
];

const STATUS_BY: &[&str] = &[
    "passes",
    "pass",
    "passed",
    "passing",
    "green",
    "succeeded",
    "succeeds",
    "stable",
    "deflaked",
    "re-enabled",
    "reenabled",
    "enabled",
    "merged",
    "landed",
    "fixed",
    "resolved",
    "addressed",
    "closed",
    "removed",
    "deleted",
    "corrected",
    "done",
    "gone",
    "finished",
    "completed",
    "complete",
    "investigated",
    "explained",
    "answered",
];

const RETIRED: &[&str] = &[
    "removed",
    "cleaned",
    "deleted",
    "dropped",
    "reverted",
    "replaced",
    "gone",
    "retired",
    "completed",
    "complete",
    "finished",
];

const REFRESHED: &[&str] = &[
    "updated",
    "refreshed",
    "replaced",
    "rewritten",
    "corrected",
    "fixed",
    "removed",
    "deleted",
    "regenerated",
];

const WAIT_NOUNS: &[&str] = &[
    "review",
    "reviews",
    "reviewer",
    "reviewers",
    "reviewing",
    "approval",
    "approvals",
    "approve",
    "sign-off",
    "signoff",
    "decision",
    "decide",
    "decides",
    "merge",
    "merging",
    "landing",
    "owner",
    "person",
    "someone",
    "somebody",
    "ci",
    "rebase",
    "rebasing",
    "re-run",
    "rerun",
    "cleanup",
    "follow-up",
    "answer",
    "feedback",
    "go-ahead",
    "confirmation",
    "permission",
    "credential",
    "credentials",
    "qa",
    "sign",
    "signs",
    "major",
    "version",
    "upstream",
    "release",
    "vendor",
    "vendors",
    "maintainer",
    "maintainers",
    "team",
    "#secret",
];

const CHECK_NOUNS: &[&str] = &[
    "ci",
    "build",
    "builds",
    "pipeline",
    "pipelines",
    "check",
    "checks",
    "suite",
    "suites",
    "lane",
    "lanes",
    "workflow",
    "workflows",
    "job",
    "jobs",
    "test",
    "tests",
    "run",
    "runs",
    "benchmark",
    "benchmarks",
    "bench",
    "soak",
    "e2e",
    "snapshot",
    "cell",
    "cells",
    "matrix",
    "lint",
    "lints",
    "warning",
    "warnings",
    "assertion",
    "assertions",
    "threshold",
    "tolerance",
    "timeout",
    "clippy",
    "tippy",
    "linter",
    "rustfmt",
    "fmt",
    "p50",
    "p90",
    "p95",
    "p99",
    "p999",
    "latency",
    "latencies",
    "throughput",
    "perf",
    "performance",
];

const BRANCH_THINGS: &[&str] = &[
    "branch", "branches", "fork", "worktree", "pr", "prs", "mr", "#branch",
];

const LOCAL_THINGS: &[&str] = &[
    "commit", "commits", "branch", "branches", "tag", "tags", "change", "changes", "work", "fix",
    "fixes", "patch", "patches", "release", "stash", "worktree", "checkout", "copy",
];

const MADE_LOCALLY: &[&str] = &[
    "tagged",
    "tag",
    "tags",
    "committed",
    "merged",
    "cut",
    "released",
    "amended",
    "staged",
];

const FAULT_NOUNS: &[&str] = &[
    "bug",
    "bugs",
    "outage",
    "crash",
    "crashes",
    "hang",
    "hangs",
    "leak",
    "leaks",
    "race",
    "regression",
    "regressions",
    "flake",
    "flakes",
    "failure",
    "failures",
    "error",
    "errors",
    "problem",
    "problems",
    "issue",
    "issues",
    "panic",
    "panics",
    "deadlock",
    "stall",
    "stalls",
    "glitch",
    "slowdown",
];

const STEP_NOUNS: &[&str] = &[
    "push",
    "merge",
    "rebase",
    "commit",
    "upload",
    "deploy",
    "release",
    "publish",
    "install",
    "build",
    "builds",
    "test",
    "tests",
    "check",
    "checks",
    "ci",
    "job",
    "jobs",
    "lane",
    "lanes",
    "run",
    "suite",
    "pipeline",
    "hook",
    "signing",
    "notarization",
];

const OPEN_NOUNS_LAND: &[&str] = &[
    "pr",
    "prs",
    "request",
    "requests",
    "mr",
    "mrs",
    "issue",
    "issues",
    "ticket",
    "tickets",
    "question",
    "questions",
    "point",
    "points",
    "decision",
    "decisions",
    "thread",
    "threads",
    "comment",
    "comments",
    "discussion",
    "discussions",
    "case",
    "cases",
    "bug",
    "bugs",
    "gap",
    "gaps",
    "problem",
    "problems",
    "item",
    "items",
    "task",
    "tasks",
    "#n",
];

pub(crate) static LAND_FAMILIES: [LandFamily; 68] = [
    LandFamily::new("unlanded", LAND_STATUS, Reads::Anywhere, LANDS),
    LandFamily::new(
        "on a branch",
        &[
            "on a branch",
            "on its branch",
            "on my branch",
            "on a feature branch",
            "on the feature branch",
            "on a side branch",
            "on a separate branch",
            "on a local branch",
            "my local branch",
            "to my branch",
            "to a branch",
            "to a feature branch",
            "to its branch",
            "in a worktree",
            "in my worktree",
            "commits behind",
            "commit behind",
            "commits ahead",
            "commit ahead",
            "ahead of main",
            "ahead of origin",
            "ahead by",
            "behind my branch",
            "on my fork",
            "to my fork",
            "in my fork",
            "on a fork",
            "merged locally",
            "main locally",
            "local main",
            "keeping the worktree",
            "kept the worktree",
            "keep the worktree",
            "keeping the branch",
            "kept the branch",
            "keep the branch",
            "not on main",
            "not on origin",
            "not on the remote",
            "ahead of",
            "into my branch",
            "into my fork",
            "into a branch",
            "into a feature branch",
            "into the feature branch",
            "in my branch",
            "in a branch",
            "in a feature branch",
            "is ahead",
            "are ahead",
            "ahead again",
            "my main",
            "main in the worktree",
            "main in my worktree",
            "main on my machine",
            "main on this machine",
            "main on this mac",
            "main is unchanged",
            "main unchanged",
            "main is untouched",
            "main untouched",
            "left main alone",
            "revision behind",
            "revisions behind",
            "version behind",
            "versions behind",
            "is behind",
            "are behind",
            "still behind",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "current with main",
        &[
            "up to date with",
            "up-to-date with",
            "in sync with",
            "current with",
            "rebased onto",
            "rebased on",
        ],
        Reads::Beside(BRANCH_THINGS),
        LANDS,
    )
    .saying("on a branch"),
    LandFamily::new(
        "local",
        &[
            "still local",
            "only local",
            "local only",
            "local-only",
            "committed locally",
            "only locally",
            "is local",
            "are local",
            "away from done",
            "merge away",
            "push away",
            "commit away",
            "created locally",
            "tagged locally",
            "locally tagged",
            "kept locally",
            "sits locally",
            "updated locally",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "on my machine",
        &[
            "my machine",
            "my laptop",
            "my computer",
            "my box",
            "my workstation",
            "this machine",
            "this host",
        ],
        Reads::Beside(LOCAL_THINGS),
        LANDS,
    )
    .saying("local"),
    LandFamily::new(
        "made locally",
        &["locally"],
        Reads::Beside(MADE_LOCALLY),
        LANDS,
    )
    .saying("local"),
    LandFamily::new(
        "untouched",
        &["untouched", "not touched", "never touched", "unstarted"],
        Reads::Beside(WORK_ITEMS),
        SHIPS,
    )
    .saying("untouched work"),
    LandFamily::new("without", &["without"], Reads::Without, &[]).saying("work owed"),
    LandFamily::new(
        "approved only",
        &[
            "approved",
            "reviewed",
            "accepted",
            "signed off",
            "lgtm",
            "lgtm'd",
        ],
        Reads::Approval,
        LANDS,
    )
    .saying("approved, not landed"),
    LandFamily::new(
        "stash",
        &["stash", "stashed", "stashes"],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new("draft", &["draft", "drafts"], Reads::Anywhere, LANDS),
    LandFamily::new(
        "left over",
        &[
            "remaining",
            "left over",
            "leftover",
            "leftovers",
            "left to do",
            "left to land",
            "left to be",
            "left to the",
            "left to finish",
            "still to do",
            "still to land",
            "still to be",
            "yet to",
            "still have to",
            "still has to",
            "still need to",
            "still needs to",
            "has to be",
            "have to be",
            "tbd",
            "to be done",
            "to be decided",
            "what remains",
            "thing left",
            "things left",
            "item left",
            "items left",
            "work left",
            "next step",
            "next steps",
            "next task",
            "next tasks",
            "next thing",
            "next item",
            "next items",
            "next job",
            "next change",
            "next pr",
            "next piece",
            "next up",
            "up next",
            "to come",
            "outstanding",
            "should be",
            "must be",
            "needs to be",
            "ought to be",
            "lingering",
            "residual",
            "dangling",
            "stray",
            "last step",
            "final step",
            "last thing",
            "one last",
            "caveat",
            "caveats",
            "as an exercise",
            "for completeness",
            "to go",
            "on my list",
            "on the list",
            "to-do",
            "owed",
            "i owe",
            "still owe",
            "tba",
            "tbc",
            "by hand",
            "manual step",
            "only fixed",
            "only patched",
            "only papered",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "remains",
        &[
            "remains",
            "remain",
            "is left",
            "are left",
            "still left",
            "stays",
            "stay",
            "is next",
            "are next",
            "comes next",
            "come next",
            "can wait",
            "will wait",
            "has to wait",
            "yet",
            "mostly",
            "blocked",
            "missing",
        ],
        Reads::Alone,
        &[],
    ),
    LandFamily::new(
        "exception",
        &[
            "except",
            "apart from",
            "aside from",
            "other than",
            "save for",
            "but for",
            "besides",
            "excluding",
            "with the exception of",
            "barring",
            "minus",
            "modulo",
            "one exception",
            "an exception",
            "a single exception",
            "only exception",
            "excepting",
            "excepted",
            "sans",
            "but",
            "bar",
            "save",
            "less",
            "beyond",
            "without",
            "exception",
            "the exception",
            "one gap",
            "the one gap",
            "only gap",
            "the only gap",
            "single gap",
            "not included",
            "not including",
            "not counting",
            "don't count",
            "do not count",
            "excluded",
            "exclusive of",
            "short of",
            "notwithstanding",
            "leaving out",
            "outside of",
            "not the",
            "omission",
            "omissions",
            "one omission",
            "holdout",
            "holdouts",
            "the holdout",
            "one holdout",
            "well, nearly",
            "not withstanding",
            "short one",
            "short by one",
            "one short",
            "give or take",
        ],
        Reads::Exception,
        &[],
    ),
    LandFamily::new(
        "qualified none",
        &[
            "important",
            "big",
            "urgent",
            "blocking",
            "critical",
            "serious",
            "major",
            "real",
            "hard",
            "significant",
            "pressing",
            "must-have",
            "must-haves",
            "showstopper",
            "showstoppers",
            "show-stopper",
            "show-stoppers",
            "that blocks",
            "that matters",
            "that can't wait",
            "that cannot wait",
            "that can not wait",
            "in scope",
            "critical path",
            "today",
            "beyond",
            "worth",
            "of note",
            "noteworthy",
            "substantial",
            "essential",
            "vital",
            "high-priority",
            "high priority",
            "release-blocking",
            "material",
            "meaningful",
            "large",
            "huge",
            "on my side",
            "on my end",
            "from me",
            "for now",
            "yet",
            "here",
            "in this repo",
            "in this pr",
            "for this pr",
        ],
        Reads::Qualified,
        &[],
    )
    .saying("a qualified none"),
    LandFamily::new(
        "a party's",
        &[
            "owner",
            "the owner",
            "owner's",
            "whoever",
            "you",
            "yours",
            "maintainer",
            "maintainers",
            "the maintainer",
            "reviewer",
            "the reviewer",
            "reviewers",
            "human",
            "a human",
        ],
        Reads::Party,
        &[],
    )
    .saying("someone else"),
    LandFamily::new(
        "only that",
        &[
            "nothing more",
            "nothing else",
            "that's all",
            "that is all",
            "that's it",
            "that is it",
        ],
        Reads::OnlyTail,
        &[],
    )
    .saying("only"),
    LandFamily::new(
        "pending",
        &[
            "pending",
            "awaiting",
            "awaits",
            "on hold",
            "up for review",
            "is for review",
            "are for review",
            "sent for review",
            "submitted for review",
            "posted for review",
            "opened for review",
            "pushed for review",
            "under review",
            "in review",
            "out for review",
            "in the merge queue",
            "queued for",
            "unreviewed",
            "unapproved",
            "ready for review",
            "ready to review",
            "ready to merge",
            "ready for merge",
            "ready to land",
            "ready to be merged",
            "ready to be landed",
            "auto-merge",
            "automerge",
            "auto merge",
            "merge train",
            "merge queue",
            "is queued",
            "are queued",
            "on approval",
            "upon approval",
            "merges on",
            "merges once",
            "merges when",
            "merges after",
            "lands once",
            "lands when",
            "lands after",
            "lands on",
            "merging after",
            "merging once",
            "merging when",
            "landing after",
            "landing once",
            "ready to tag",
            "ready to publish",
            "ready to release",
            "ready to ship",
            "ready to push",
            "ready to deploy",
            "for review",
            "for a review",
            "is ready",
            "are ready",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "waits on",
        &[
            "needs", "need", "requires", "require", "waiting", "waits", "wait",
        ],
        Reads::Beside(WAIT_NOUNS),
        LANDS,
    ),
    LandFamily::new(
        "blocked",
        &[
            "blocked on",
            "blocked by",
            "is blocked",
            "are blocked",
            "still blocked",
            "remains blocked",
            "stays blocked",
        ],
        Reads::Anywhere,
        SHIPS,
    ),
    LandFamily::new(
        "open change",
        &["open", "opened", "reopened", "filed"],
        Reads::Beside(OPEN_NOUNS_LAND),
        MENDS,
    ),
    LandFamily::new(
        "check",
        &[
            "running",
            "red",
            "failing",
            "queued",
            "cut short",
            "aborted",
            "interrupted",
            "stopped early",
            "killed early",
            "ended early",
            "in flight",
            "flaky",
            "flakes",
            "flaking",
            "disabled",
            "ignored",
            "quarantined",
            "muted",
            "skipped",
            "broken",
            "timing out",
            "timed out",
            "hanging",
            "unstable",
            "yellow",
            "suppressed",
            "silenced",
            "commented out",
            "xfail",
            "fails",
            "fail",
            "intermittently",
            "intermittent",
            "sporadically",
            "amber",
            "turned off",
            "switched off",
            "allow",
            "allowed",
            "relaxed",
            "loosened",
            "weakened",
            "not passing",
            "not green",
            "not succeeding",
            "not succeeded",
            "not stable",
            "not clean",
            "error out",
            "errors out",
            "erroring out",
            "errored out",
            "complains",
            "complain",
            "complaining",
            "regressed",
            "silence",
            "xfailed",
            "deselected",
        ],
        Reads::Beside(CHECK_NOUNS),
        CHECKS,
    ),
    LandFamily::new(
        "rerun",
        &[
            "on rerun",
            "on re-run",
            "on a rerun",
            "on a re-run",
            "on the rerun",
            "on retry",
            "on a retry",
            "on the retry",
            "after a rerun",
            "after one rerun",
            "after a retry",
            "after one retry",
            "after two retries",
            "after retrying",
            "with a retry",
            "with one retry",
            "one retry",
            "two retries",
            "retried",
            "second run",
            "2nd run",
            "third run",
            "3rd run",
            "second attempt",
            "2nd attempt",
            "third attempt",
            "3rd attempt",
            "second try",
            "2nd try",
            "third try",
            "3rd try",
            "ran it again",
            "ran them again",
            "run it again",
            "running it again",
            "flaked",
            "rerun clean",
            "rerun green",
            "rerun passed",
            "re-run clean",
            "re-run green",
            "reran clean",
            "reran green",
            "re-ran clean",
            "re-ran green",
            "passed this time",
            "green this time",
            "passes this time",
        ],
        Reads::Rerun,
        &[],
    )
    .saying("a rerun"),
    LandFamily::new(
        "status item",
        &[
            "skip",
            "skips",
            "named skip",
            "named skips",
            "none run",
            "not read",
            "unread",
            "next session",
            "to do",
            "n/a",
        ],
        Reads::Status,
        &[],
    ),
    LandFamily::new("later", LAND_LATER, Reads::Later, LANDS),
    LandFamily::new("until", &["until", "till"], Reads::Until, &[]),
    LandFamily::new(
        "intent",
        &[
            "i will",
            "i'll",
            "we will",
            "we'll",
            "i am going to",
            "i'm going to",
            "plan to",
            "planning to",
            "intend to",
            "will land",
            "will be landed",
            "will merge",
            "will be merged",
            "will push",
            "will be pushed",
            "will be done",
            "will be fixed",
            "will do",
            "will add",
            "will write",
            "will follow",
            "should still",
            "still should",
            "we should",
            "i should",
            "ought to",
            "would be good to",
            "would be nice to",
            "would be nice",
            "would be good",
            "would help",
            "would be better",
            "nice to have",
            "nice-to-have",
            "worth adding",
            "can be done",
            "could be done",
            "can be added",
            "could be added",
            "we can",
            "we could",
            "i'd like",
            "i would like",
            "would like",
            "second pair of eyes",
            "a second look",
            "another look",
            "a closer look",
            "a final look",
            "whether to",
            "whether we should",
            "if we should",
            "asked the owner",
            "asked the user",
            "i'm leaving",
            "i am leaving",
            "i'm keeping",
            "i am keeping",
            "will be",
            "will happen",
            "will update",
            "will rebuild",
            "will refresh",
            "will regenerate",
            "will redeploy",
            "will deploy",
            "will publish",
            "will roll",
            "will pick",
            "will go",
            "will get",
            "will run",
            "will need",
            "will come",
            "will ship",
            "should i",
            "should we",
            "shall i",
            "shall we",
            "want me to",
            "do you want",
            "let me know",
            "happy to",
            "i can also",
            "if you want",
            "if you'd like",
            "if you like",
            "if needed",
            "if desired",
            "when you are",
            "when you're",
            "let's",
            "let us",
            "worth considering",
            "consider",
        ],
        Reads::Intent,
        &[],
    ),
    LandFamily::new(
        "todo",
        &["todo", "todos", "fixme", "fixmes"],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "check off in code",
        &["check ignored", "lint allowed"],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "could not",
        &[
            "could not",
            "couldn't",
            "not able to",
            "unable to",
            "cannot run",
            "cannot test",
            "cannot verify",
            "cannot check",
            "cannot measure",
            "cannot confirm",
            "can't run",
            "can't test",
            "can't verify",
            "can't check",
            "can't measure",
            "can't confirm",
            "did not get to",
            "ran out of",
            "ran out",
            "out of time",
            "ended before",
            "stopped before",
            "unverified",
            "only type-checked",
        ],
        Reads::Anywhere,
        CHECKS,
    ),
    LandFamily::new(
        "not done",
        &[
            "not written",
            "not reviewed",
            "not approved",
            "not released",
            "not deployed",
            "not published",
            "not tagged",
            "not documented",
            "not implemented",
            "not started",
            "not finished",
            "not complete",
            "not completed",
            "not resolved",
            "not addressed",
            "not handled",
            "not updated",
            "not corrected",
            "not driven end to end",
            "not wired",
            "not hooked up",
            "not hooked in",
            "not plugged in",
            "not integrated",
            "not ported",
            "not migrated",
            "not rebased",
            "not signed",
            "not land",
            "not merge",
            "not push",
            "not commit",
            "not all",
            "not removed",
            "not deleted",
            "not cleaned up",
            "not reverted",
            "not dropped",
            "not investigated",
            "not looked at",
            "not looked into",
            "not profiled",
            "not diagnosed",
            "not understood",
            "not root-caused",
            "uninvestigated",
            "did not update",
            "did not add",
            "did not write",
            "did not push",
            "did not merge",
            "did not land",
            "did not finish",
            "did not do",
            "did not address",
            "did not handle",
            "did not document",
            "did not commit",
            "did not remove",
            "did not delete",
            "did not rebase",
            "not covered",
            "uncovered",
            "not tried",
            "untried",
            "not attempted",
            "did not apply",
            "stopped there",
            "gave up",
            "did not have time",
            "had no time",
            "no time for",
            "forgot to",
            "overlooked",
            "did not get into",
            "did not dig into",
            "not get around",
            "unfinished",
            "unaddressed",
            "unresolved",
            "unhandled",
            "unimplemented",
            "unwritten",
            "undocumented",
            "unreleased",
            "unpublished",
            "untagged",
            "unanswered",
            "unsolved",
            "unstarted",
            "never written",
            "never tested",
            "never run",
            "never ran",
            "never reviewed",
            "never documented",
            "never finished",
            "never implemented",
            "never released",
            "never published",
            "never tagged",
            "never verified",
            "never checked",
            "never measured",
            "not so much",
            "left out",
            "missed the merge",
            "missed the release",
            "missed the cut",
            "missed the train",
            "missed the window",
            "missed the deadline",
            "to be written",
            "to be added",
            "to be updated",
            "to be fixed",
            "to be ported",
            "still to write",
        ],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "partly",
        &[
            "only partly",
            "partly",
            "partially",
            "partial",
            "half done",
            "half-done",
            "half written",
            "half-written",
            "half finished",
            "half-finished",
            "half implemented",
            "half-implemented",
            "half merged",
            "halfway",
            "half-way",
            "half way",
            "mostly done",
            "mostly merged",
            "mostly landed",
            "almost done",
            "nearly done",
            "incomplete",
            "incompletely",
            "not fully",
            "not completely",
            "not entirely",
            "spot-checked",
            "spot-check",
            "spot checked",
            "only checked",
            "only tested",
            "only ran",
            "only verified",
            "only tried",
            "smoke-tested",
            "eyeballed",
            "some of the",
            "a few of the",
            "several of the",
            "half of",
            "half fixed",
            "mostly fixed",
            "fixed only for",
            "a quarter of",
            "a third of",
            "two thirds of",
            "three quarters of",
            "a fraction of",
            "not the real",
            "against a mock",
            "staging only",
            "on staging only",
            "only on staging",
            "only in staging",
            "not zero",
            "only some",
            "only part",
            "done-ish",
            "merged-ish",
            "finished-ish",
            "landed-ish",
            "complete-ish",
            "mostly-done",
        ],
        Reads::Anywhere,
        RETIRED,
    ),
    LandFamily::new(
        "stopgap",
        &[
            "workaround",
            "workarounds",
            "temporary",
            "temporarily",
            "stopgap",
            "stop-gap",
            "hack",
            "hacks",
            "hacky",
            "interim",
            "band-aid",
            "quick fix",
            "old code path",
            "old path",
            "behind a flag",
            "behind a cfg",
            "behind cfg",
            "behind a feature gate",
            "behind a gate",
            "left in place",
            "in place behind",
            "behind a feature flag",
            "behind the flag",
            "off by default",
            "works around",
            "work around",
            "worked around",
            "working around",
            "rather than fixing",
            "instead of fixing",
            "rather than solving",
            "instead of solving",
            "as a fallback",
            "as a backup",
            "rather than deleted",
            "rather than deleting",
            "rather than removed",
            "rather than removing",
            "instead of deleting",
            "instead of removing",
            "placeholder",
            "placeholders",
            "stubbed out",
            "is a stub",
            "just a stub",
            "only a stub",
            "kept around",
            "keep around",
            "keeping around",
        ],
        Reads::Anywhere,
        RETIRED,
    ),
    LandFamily::new(
        "needs doing",
        &[
            "needs",
            "need",
            "requires",
            "require",
            "still needs",
            "wants",
        ],
        Reads::Gerund,
        &[],
    ),
    LandFamily::new("still", &["still"], Reads::Still, &[]),
    LandFamily::new(
        "remote branch",
        &["origin", "upstream", "--no-merged"],
        Reads::Ref,
        LANDS,
    ),
    LandFamily::new("part of a count", &["of", "only"], Reads::Fraction, &[]),
    LandFamily::new(
        "hedge",
        &[
            "not sure",
            "unsure",
            "uncertain",
            "unclear whether",
            "unclear if",
            "not certain",
            "not convinced",
            "not confident",
            "i think",
            "i believe",
            "we think",
            "we believe",
            "i doubt",
            "doubt that",
            "doubtful",
            "i suspect",
            "i guess",
            "i hope",
            "i imagine",
            "probably",
            "should pass",
            "should work",
            "should be fine",
            "likely",
            "hopefully",
            "a guess",
            "guessed",
            "guesswork",
            "i assume",
            "presumably",
            "seems to",
            "appears to",
            "from my laptop",
            "from my machine",
            "tested on my",
            "measured on my",
            "benchmarked on my",
            "only locally",
            "locally only",
            "just locally",
            "only on my",
            "assuming",
            "fairly sure",
            "pretty sure",
            "reasonably sure",
            "quite sure",
            "fairly confident",
            "pretty confident",
            "if i recall",
            "if i remember",
            "iirc",
            "afaict",
            "afaik",
            "as far as i can tell",
            "as far as i know",
            "to my knowledge",
            "looks merged",
            "looks landed",
            "looks done",
            "looks good",
            "looks fine",
            "looks like it",
            "looks like everything",
            "looks like all",
            "it looks like",
            "provided",
            "providing",
            "fingers crossed",
            "should go",
            "should land",
            "should merge",
            "should be merged",
            "should be green",
            "unknown",
            "no idea",
            "no data",
            "not known",
        ],
        Reads::Anywhere,
        &[],
    ),
    LandFamily::new(
        "failed step",
        &[
            "failed",
            "errored",
            "rejected",
            "refused",
            "denied",
            "bounced",
            "conflict",
            "conflicts",
            "conflicted",
            "conflicting",
        ],
        Reads::Beside(STEP_NOUNS),
        STEPS,
    ),
    LandFamily::new(
        "elided",
        &[
            "is not", "are not", "has not", "have not", "was not", "were not", "did not",
            "does not", "do not",
        ],
        Reads::Elided,
        &[],
    ),
    LandFamily::new(
        "did not",
        &["did not", "have not", "has not", "had not", "never got to"],
        Reads::NotDone,
        &[],
    ),
    LandFamily::new(
        "no longer works",
        &[
            "doesn't",
            "does not",
            "don't",
            "do not",
            "no longer",
            "won't",
            "will not",
            "cannot",
            "can't",
        ],
        Reads::Lost,
        &[],
    ),
    LandFamily::new(
        "could be better",
        &["could be", "might be"],
        Reads::Better,
        &[],
    ),
    LandFamily::new(
        "concession",
        &["though", "although", "however", "but", "even so"],
        Reads::Concession,
        &[],
    ),
    LandFamily::new(
        "wrong",
        &[
            "is wrong",
            "are wrong",
            "still wrong",
            "is incorrect",
            "are incorrect",
            "is inaccurate",
            "is misleading",
            "is buggy",
            "is broken",
            "are broken",
            "will break",
            "will fail",
            "would break",
            "only handles",
            "only covers",
            "only supports",
            "only works",
            "is missing",
            "are missing",
            "still missing",
            "lacks",
            "still lacks",
            "lacking",
            "same bug",
            "same problem",
            "same issue",
            "nothing covers",
            "no test covers",
            "no tests cover",
            "says nothing about",
            "does not mention",
            "doesn't mention",
            "no mention of",
            "got worse",
            "gets worse",
            "is worse",
            "are worse",
            "got slower",
            "is slower",
            "are slower",
            "is dirty",
            "are dirty",
            "still dirty",
            "regression in",
            "regressions in",
            "is a regression",
            "there is a regression",
            "introduces a regression",
            "introduced a regression",
            "regressed",
            "main is red",
            "main is broken",
            "main is failing",
            "red on main",
        ],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "back again",
        &[
            "is back",
            "are back",
            "came back",
            "comes back",
            "is back again",
            "resurfaced",
            "reappeared",
            "recurs",
            "recurred",
            "returned",
            "still reproduces",
        ],
        Reads::Beside(FAULT_NOUNS),
        MENDS,
    ),
    LandFamily::new(
        "stale",
        &[
            "is stale",
            "are stale",
            "still stale",
            "now stale",
            "outdated",
            "out of date",
            "out-of-date",
            "still describes",
            "still mentions",
            "still refers to",
            "still points to",
            "still documents",
        ],
        Reads::Anywhere,
        REFRESHED,
    ),
    LandFamily::new(
        "known issue",
        &[
            "known issue",
            "known issues",
            "known bug",
            "known bugs",
            "known limitation",
            "known limitations",
            "known gap",
            "known gaps",
            "known problem",
            "known problems",
            "known regression",
            "known regressions",
            "known crash",
            "known crashes",
            "known hang",
            "known flake",
            "known failure",
            "known failures",
            "nit",
            "nits",
        ],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "someone else",
        &[
            "someone else",
            "somebody else",
            "someone will",
            "somebody will",
            "someone should",
            "somebody should",
            "someone can",
            "someone could",
            "someone must",
            "someone needs to",
            "until someone",
            "until somebody",
            "up to the owner",
            "left to the owner",
            "for the owner to",
            "the owner will",
            "the owner must",
            "the owner has to",
            "the owner needs to",
            "the owner's call",
            "up to you",
            "for you to",
            "your call",
            "another agent",
            "another worker",
            "another person",
            "the next agent",
            "the next worker",
            "a later worker",
            "owner's to do",
            "yours to do",
            "the owner's job",
            "the owner's step",
            "the owner's turn",
            "in the owner's hands",
            "in your hands",
            "on my side",
            "on my end",
            "leave to",
            "leave it to",
            "i leave",
            "leaving it to",
            "hand off",
            "handed off",
            "handoff",
            "hand-off",
            "hand-offs",
            "handing off",
            "handing it off",
            "handing over",
            "handed over",
            "you can",
            "you may",
            "you could",
            "you might",
            "feel free",
            "over to you",
            "your court",
            "your move",
            "your turn",
            "on you to",
            "is yours",
            "are yours",
            "yours to",
            "with the owner",
            "with you",
            "sits with",
            "rests with",
            "lies with",
            "has to sign",
            "must sign",
            "needs to sign",
            "to sign off",
            "for sign-off",
            "for signoff",
            "for your review",
            "for your approval",
            "on me",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "asks you",
        &[
            "please",
            "kindly",
            "i need you",
            "we need you",
            "need you to",
            "needs you to",
            "need from you",
            "needed from you",
            "need your",
            "needs your",
            "requires you",
            "require you",
            "requires your",
            "require your",
            "you need to",
            "you'll need to",
            "you will need to",
            "you'd need to",
            "you would need to",
            "you'll have to",
            "you will have to",
            "you have to",
            "you must",
            "you should",
            "could you",
            "can you",
            "would you",
            "will you",
            "ask you",
            "asking you",
            "action for you",
            "actions for you",
            "thing for you",
            "things for you",
            "item for you",
            "items for you",
            "task for you",
            "step for you",
            "steps for you",
            "left for you",
            "is for you",
            "are for you",
            "waiting on you",
            "waiting for you",
            "waits on you",
            "waits for you",
            "depends on you",
            "on you",
            "owner action",
            "action needed",
            "action required",
            "needs the owner",
            "requires the owner",
            "require the owner",
            "only a person can",
        ],
        Reads::Anywhere,
        &[],
    ),
    LandFamily::new(
        "deferred",
        &[
            "deferred",
            "postponed",
            "put off",
            "punted",
            "shelved",
            "backlogged",
            "in the backlog",
            "tracked in issue",
            "tracked in an issue",
            "tracked in a ticket",
            "tracked as issue",
            "tracked as an issue",
            "separately",
            "separate pr",
            "separate prs",
            "separate change",
            "separate job",
            "separate task",
            "separate step",
            "separate issue",
            "separate effort",
            "its own pr",
            "another pr",
            "a second pr",
            "second pr",
            "a later pr",
            "subsequent pr",
            "subsequent prs",
            "subsequent change",
            "subsequent changes",
            "subsequent commit",
            "subsequent session",
            "subsequent release",
            "subsequent version",
            "is coming",
            "are coming",
            "coming in",
            "coming soon",
            "in v2",
            "for v2",
            "next version",
            "a future version",
            "a later version",
            "out of scope",
            "outside my scope",
            "beyond scope",
            "beyond the scope",
            "not in scope",
            "another ticket",
            "another issue",
            "a separate ticket",
            "a follow-up ticket",
            "its own ticket",
            "another task",
            "tracked in",
            "tracked by",
            "tracked as",
            "tracked under",
            "tracked separately",
            "tracking issue",
            "on the roadmap",
            "on our roadmap",
            "in the roadmap",
            "can follow",
            "could follow",
            "may follow",
            "might follow",
            "after this lands",
            "once this lands",
            "after this merges",
            "once this merges",
            "after this is merged",
            "once this is merged",
            "second step",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "in progress",
        &[
            "in progress",
            "underway",
            "under way",
            "paused",
            "on pause",
            "is in flight",
            "are in flight",
            "still in flight",
            "abandoned",
            "omitted",
            "skipping",
            "ongoing",
            "stalled",
            "stuck",
            "suspended",
            "on the back burner",
            "on ice",
            "about to",
            "wrapping up",
            "almost through",
            "nearly through",
            "going through",
            "going in",
            "merging now",
            "pushing now",
            "landing now",
            "rolling out",
            "being rolled out",
            "being merged",
            "being pushed",
            "in the works",
            "slipped",
            "descoped",
            "de-scoped",
            "fell through",
            "fell behind",
            "fell off",
            "lag behind",
            "lags behind",
            "lagging behind",
            "behind the code",
        ],
        Reads::State,
        MENDS,
    ),
    LandFamily::new(
        "end of a clause",
        &[
            "soon",
            "next",
            "aside",
            "one thing",
            "one more thing",
            "to follow",
            "one item",
            "one small thing",
            "one small item",
            "one last item",
            "a single item",
            "coming",
            "forthcoming",
            "upcoming",
            "nearly",
            "almost",
            "absent",
            "to write",
            "to add",
            "to update",
            "to fix",
            "to port",
            "to land",
            "to merge",
            "to push",
            "to tag",
            "to review",
            "to publish",
            "to release",
            "last bit",
            "the last bit",
            "local",
            "behind",
            "on origin",
            "on the remote",
        ],
        Reads::End,
        &[],
    ),
    LandFamily::new("counted left", &["left"], Reads::Count, &[]),
    LandFamily::new("one more", &["one more"], Reads::OneMore, &[]),
    LandFamily::new("only", &["only", "just", "merely"], Reads::Only, &[]),
    LandFamily::new("after an event", &["after", "once"], Reads::Elliptic, &[]).saying("later"),
    LandFamily::new(
        "absent",
        &[
            "no test",
            "no tests",
            "no unit test",
            "no unit tests",
            "no new test",
            "no new tests",
            "no regression test",
            "no docs",
            "no doc",
            "no documentation",
            "no coverage",
            "no test coverage",
            "no benchmark",
            "no benchmarks",
            "no changelog",
            "no changelog entry",
            "no example",
            "no examples",
            "no readme",
            "no release notes",
            "no release note",
            "no migration",
            "no migrations",
            "zero tests",
            "zero test",
            "0 tests",
            "no test case",
            "no test cases",
            "no integration test",
            "no integration tests",
            "no run",
            "no runs",
            "no result",
            "no results",
            "no numbers",
            "no measurement",
            "no measurements",
        ],
        Reads::Absent,
        &[],
    ),
    LandFamily::new(
        "nobody",
        &["no one", "no-one", "noone", "nobody", "nothing"],
        Reads::Nobody,
        &[],
    ),
    LandFamily::new(
        "landing denied",
        &[
            "merged",
            "landed",
            "pushed",
            "reached",
            "committed",
            "on origin/main",
            "on main",
            "on remote main",
            "on the remote main",
            "in origin/main",
            "on upstream/main",
            "made it",
            "push",
            "merge",
        ],
        Reads::Unlanded,
        &[],
    )
    .saying("not landed"),
    LandFamily::new(
        "work denied",
        &[
            "written",
            "run",
            "tested",
            "done",
            "finished",
            "implemented",
            "documented",
            "reviewed",
        ],
        Reads::Undone,
        &[],
    )
    .saying("not done"),
    LandFamily::new(
        "onto a branch",
        &[
            "pushed",
            "merged",
            "landed",
            "committed",
            "cherry-picked",
            "cherry picked",
            "backported",
            "picked",
        ],
        Reads::Onto,
        &[],
    )
    .saying("to a branch"),
    LandFamily::new(
        "most",
        &[
            "most",
            "most of",
            "half the",
            "the bulk of",
            "the majority of",
            "almost all",
            "almost everything",
            "nearly all",
            "nearly everything",
            "virtually all",
            "practically all",
            "some of it",
            "some of them",
            "part of it",
            "parts of it",
            "half of it",
            "half of them",
            "much of",
            "the majority",
            "largely",
            "virtually everything",
            "practically everything",
            "the better part of",
            "a good part of",
            "pretty much",
            "more or less",
            "essentially",
            "basically",
            "practically",
            "lion's share",
            "the lion's share",
            "tenths",
            "nine tenths",
            "the lion's share of",
            "most commits",
        ],
        Reads::Most,
        &[],
    ),
    LandFamily::new(
        "dropped",
        &[
            "was dropped",
            "were dropped",
            "got dropped",
            "been dropped",
            "is dropped",
            "are dropped",
            "reverted",
            "rolled back",
            "backed out",
            "backed off",
            "scrapped",
            "was cut",
            "were cut",
            "got cut",
            "been cut",
            "cut from",
        ],
        Reads::Dropped,
        &[],
    ),
    LandFamily::new(
        "on a named branch",
        &[
            "on",
            "in",
            "has everything",
            "has it all",
            "has all of it",
            "holds everything",
            "carries everything",
        ],
        Reads::OnBranch,
        &[],
    )
    .saying("on a branch"),
    LandFamily::new(
        "change",
        &[
            "pr",
            "prs",
            "pull request",
            "pull requests",
            "mr",
            "mrs",
            "merge request",
            "merge requests",
            "the second one",
            "the other one",
        ],
        Reads::Change,
        SHIPS,
    )
    .saying("an unmerged change"),
    LandFamily::new(
        "percent",
        &["percent", "per cent", "pct"],
        Reads::Percent,
        &[],
    )
    .saying("a part"),
    LandFamily::new(
        "counted failures",
        &[
            "errors", "error", "failures", "failure", "panicked", "panics", "panic", "crashes",
            "crash", "crashed", "timeouts", "timeout", "warnings", "warning", "xfailed", "xfail",
            "flakes", "flake",
        ],
        Reads::Tally,
        CHECKS,
    ),
];

const CONTRACTED: &[&str] = &[
    "did", "was", "were", "has", "have", "had", "is", "are", "does", "do",
];

const OPEN_NEGATIONS: &[&str] = &[
    "none", "nothing", "no", "n/a", "nil", "never", "zero", "0", "without", "neither", "nor",
];

const OPEN_MODALS: &[&str] = &[
    "do", "does", "don't", "doesn't", "must", "should", "shall", "will", "would", "can", "may",
    "might", "need", "needs",
];

/// What a report's own words leave open, in [`OPEN_HEADS`]' and [`OPEN_STATUS`]' spelling, each once, in the order found.
#[must_use]
pub fn open_work(tail: Option<&str>) -> Vec<&'static str> {
    tail.map_or_else(Vec::new, |t| open_work_read(t, None))
}

/// LAND'S STRICT READING of open work.
#[must_use]
pub fn open_work_strict(tail: Option<&str>) -> Vec<&'static str> {
    tail.map_or_else(Vec::new, |t| open_work_read(t, Some(&LAND_FAMILIES)))
}

fn open_work_read(tail: &str, land: Option<&[LandFamily]>) -> Vec<&'static str> {
    open_work_read_as(tail, land, true)
}

fn open_work_read_as(tail: &str, land: Option<&[LandFamily]>, defense: bool) -> Vec<&'static str> {
    let lines = report_lines(tail, land.is_some());
    let said_main = land.is_some()
        && (!defense
            || lines
                .iter()
                .any(|l| main_named(&l.words) || main_claimed(&l.words)));
    let mut found: Vec<&'static str> = Vec::new();
    let mut i = 0;
    while let Some(line) = lines.get(i) {
        if line.unchecked {
            found.push(UNCHECKED);
        }
        let words = line.words.as_str();
        let starts = sentence_starts(words);
        let end = |k: usize| starts.get(k + 1).copied().unwrap_or(words.len());
        let mut next = i + 1;
        for k in 0..starts.len() {
            let sentence = &words[starts[k]..end(k)];
            let head = open_head(sentence)
                .or_else(|| land.and_then(|_| head_in(sentence, LAND_HEADS)))
                .or_else(|| {
                    (k == 0 && line.heading)
                        .then(|| heading_head(sentence))
                        .flatten()
                });
            let Some((head, rest)) = head else {
                let (line, len) = (&words[starts[k]..], end(k) - starts[k]);
                let word = match land {
                    None => open_status(line, len, &STATUS_PHRASES),
                    Some(families) => {
                        land_status(line, len, &words[..starts[k]], families, said_main)
                    }
                };
                if let Some(word) = word {
                    found.push(word);
                }
                continue;
            };
            let nil = |item: &str| {
                if land.is_some() {
                    nil_list(item)
                } else {
                    negated(item)
                }
            };
            let (qualifier, inline) = qualified(rest);
            let cleared = nil(qualifier);
            let one_sentence = sentence_starts(inline).len() == 1;
            if inline.is_empty() || (one_sentence && inline.ends_with(':')) {
                let mut j = i + 1;
                let mut items: Vec<&str> = Vec::new();
                while let Some(under) = lines.get(j) {
                    if under.block {
                        // Land's reading: a code block right under a bare
                        // head is its item (`Still to do:` over a command).
                        if items.is_empty() {
                            items.push(&under.words);
                        }
                        break;
                    }
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
                if !cleared && items.iter().any(|item| !nil(item)) {
                    found.push(head);
                }
                if let Some(families) = land {
                    found.extend(
                        items
                            .iter()
                            .filter_map(|item| aside_status(item, families, said_main)),
                    );
                }
                break;
            }
            if !cleared && !nil(inline) {
                found.push(head);
            }
            if let Some(families) = land {
                found.extend(aside_status(inline, families, said_main));
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

const UNCHECKED_BOXES: &[&str] = &[
    "[ ]", "[  ]", "[]", "[-]", "[~]", "☐", "⬜", "◻️", "◻", "□", "🔲", "⏳", "⌛", "🚧", "❌",
    "✗", "✘", "✕", "✖", "✖️", "⚠️", "⚠",
];

const CHECKED_BOXES: &[&str] = &[
    "[x]", "[X]", "[✓]", "[✔]", "[v]", "☑️", "☑", "✅", "✔️", "✔", "✓",
];

const UNCHECKED: &str = "unchecked box";

const CODE: &str = "codespan";

const CODE_MARKS: &[(&str, &str)] = &[
    ("todo!(", "todo"),
    ("unimplemented!(", "todo"),
    ("#[ignore", "check ignored"),
    ("continue-on-error", "check ignored"),
    ("allow(", "lint allowed"),
    ("stash@{", "stash"),
    ("--dry-run", "dry run"),
];

fn code_words(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let lower = lower.as_str();
    if let Some(word) = code_work(lower).first() {
        return (*word).to_string();
    }
    let whole = lower.trim().trim_matches(|c: char| !c.is_alphanumeric());
    if let Some(word) = SPAN_STATUS
        .iter()
        .find(|(said, _)| *said == whole)
        .map(|(_, word)| *word)
    {
        return word.to_string();
    }
    if let Some(word) = output_status(raw).first() {
        return (*word).to_string();
    }
    let mut tokens = lower.split_whitespace();
    if let (Some(one), None) = (tokens.next(), tokens.next()) {
        // A refspec names where it pushes (`HEAD:main` is main).
        if let Some((_, dst)) = one.split_once(':')
            && matches!(
                dst,
                "main" | "master" | "refs/heads/main" | "refs/heads/master"
            )
        {
            return "main".to_string();
        }
        let named = one.contains(['-', '_'])
            && one
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
        if code_ref(one)
            || matches!(one, "main" | "master")
            || BRANCHES.contains(&one)
            || (named && one.starts_with(|c: char| c.is_ascii_alphabetic()))
        {
            return one.to_string();
        }
    }
    let mut out = CODE.to_string();
    for t in lower.split_whitespace().filter(|t| code_ref(t)) {
        out.push(' ');
        out.push_str(t);
        if t.ends_with("..") {
            out.push_str("head");
        }
    }
    out
}

const SPAN_NOUNS: &[&str] = &[
    "line", "lines", "row", "rows", "label", "labels", "word", "words", "status", "marker",
    "markers", "entry", "entries", "prefix", "column", "columns", "case", "cases", "state",
    "states", "outcome", "outcomes", "result", "results", "output", "colour", "color", "text",
];

const SPAN_STATUS: &[(&str, &str)] = &[
    ("skip", "skipped"),
    ("skips", "skipped"),
    ("skipped", "skipped"),
    ("fail", "check failed"),
    ("failed", "check failed"),
    ("failing", "check failed"),
    ("not run", "not run"),
    ("pending", "pending"),
];

fn output_status(raw: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let t = raw.trim();
    let mut tokens = t.split_whitespace();
    let first = tokens
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| matches!(c, '[' | ']' | ':' | '-'));
    let second = tokens.next();
    let named = second.is_some_and(|w| w.starts_with(|c: char| c.is_alphanumeric()));
    if matches!(first, "SKIP" | "SKIPPED") || (first.eq_ignore_ascii_case("skip") && named) {
        out.push("skipped");
    }
    if matches!(first, "FAIL" | "FAILED") || t.contains("test result: FAILED") {
        out.push("check failed");
    }
    if first == "NOT" && second == Some("RUN") {
        out.push("not run");
    }
    let words: Vec<String> = t
        .split_whitespace()
        .map(|w| bare(w).to_lowercase())
        .collect();
    for pair in words.windows(2) {
        if count(&pair[0]).is_some_and(|n| n > 0) {
            match pair[1].as_str() {
                "skip" | "skips" | "skipped" => out.push("skipped"),
                "fail" | "fails" | "failed" | "failures" | "failing" => out.push("check failed"),
                _ => {}
            }
        }
    }
    if t.starts_with("## ") && t.contains("[ahead ") {
        out.push("commits ahead");
    }
    let bytes = raw.trim_end().as_bytes();
    let state = |b: u8| matches!(b, b' ' | b'M' | b'A' | b'D' | b'R' | b'C' | b'U');
    if raw.starts_with("?? ") && raw.len() > 3 {
        out.push("untracked");
    } else if bytes.len() > 3
        && state(bytes[0])
        && state(bytes[1])
        && bytes[..2] != *b"  "
        && bytes[2] == b' '
        && bytes[3] != b' '
        && raw[3..].contains(['.', '/'])
    {
        out.push("modified files");
    }
    out.dedup();
    out
}

fn code_ref(t: &str) -> bool {
    let plain = t
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'));
    let file = t.rsplit('/').next().is_some_and(|last| {
        last.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        })
    });
    plain
        && t.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !t.ends_with('/')
        && (t.contains("..") || (t.contains('/') && !file))
}

fn table_row(t: &str) -> Option<String> {
    let inner = t.strip_prefix('|')?.trim_end().strip_suffix('|')?;
    let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
    match cells.as_slice() {
        [label, item] if !label.is_empty() && !item.is_empty() => Some(format!("{label}: {item}")),
        _ => None,
    }
}

fn code_work(raw: &str) -> Vec<&'static str> {
    let lower = raw.to_lowercase();
    let mut out: Vec<&'static str> = CODE_MARKS
        .iter()
        .filter(|(mark, _)| lower.contains(mark))
        .map(|(_, word)| *word)
        .collect();
    if lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| matches!(w, "todo" | "fixme"))
    {
        out.push("todo");
    }
    out
}

struct ReportLine {
    separated: bool,
    block: bool,
    unchecked: bool,
    item: bool,
    heading: bool,
    capital: bool,
    words: String,
}

impl ReportLine {
    fn of(raw: &str, land: bool) -> Self {
        let lead = raw.trim_start();
        let marks = lead.trim_start_matches('#');
        let heading = lead.starts_with('#') && !(land && !marks.starts_with(char::is_whitespace));
        let t = raw.trim().trim_start_matches(|c: char| {
            c == '>' || (c == '#' && heading) || (c == '#' && !land) || c.is_whitespace()
        });
        let marked = ["- ", "* ", "+ ", "• ", "◦ ", "‣ "]
            .iter()
            .find_map(|m| t.strip_prefix(m))
            .or_else(|| {
                let digits = t.bytes().take_while(u8::is_ascii_digit).count();
                let rest = t.get(digits..).filter(|_| (1..=3).contains(&digits))?;
                rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
            });
        let (item, t) = marked.map_or((false, t), |r| (true, r.trim_start()));
        let boxed = UNCHECKED_BOXES.iter().chain(CHECKED_BOXES).find(|b| {
            land && t
                .strip_prefix(**b)
                .is_some_and(|r| r.is_empty() || r.starts_with(' '))
        });
        let mut unchecked = boxed.is_some_and(|b| UNCHECKED_BOXES.contains(b));
        let t = boxed.map_or(t, |b| t[b.len()..].trim_start());
        let cells = land.then(|| table_row(t)).flatten();
        let t = cells.as_deref().unwrap_or(t);
        let capital = t
            .chars()
            .find(|&c| c != '*' && c != '_')
            .is_some_and(char::is_uppercase);
        let mut flat = String::with_capacity(t.len());
        let mut code = false;
        let mut span = String::new();
        for (i, c) in t.char_indices() {
            match c {
                '`' => {
                    if code && land {
                        flat.push(' ');
                        let named = t[i + 1..]
                            .split_whitespace()
                            .next()
                            .map(bare)
                            .is_some_and(|w| SPAN_NOUNS.contains(&w.to_lowercase().as_str()));
                        let words = code_words(&span);
                        if named && SPAN_STATUS.iter().any(|(_, w)| *w == words) {
                            flat.push_str(CODE);
                        } else {
                            flat.push_str(&words);
                        }
                        span.clear();
                    }
                    code = !code;
                    flat.push(' ');
                }
                _ if code => span.push(c),
                '*' => {}
                '’' => flat.push('\''),
                c => flat.extend(c.to_lowercase()),
            }
        }
        if land {
            let mut padded = format!(" {flat} ");
            for b in UNCHECKED_BOXES.iter().chain(CHECKED_BOXES) {
                let mark = format!(" {}", b.to_lowercase());
                let mut from = 0;
                while let Some(found) = padded[from..].find(&mark).map(|i| from + i) {
                    let past = found + mark.len();
                    if padded[past..].starts_with([' ', '.', ',', ';', ':', ')', '!']) {
                        unchecked |= UNCHECKED_BOXES.contains(b);
                        padded.replace_range(found..past, " ");
                        from = found;
                    } else {
                        from = past;
                    }
                }
            }
            flat = padded.replace("#[ignore]", " check ignored ");
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
            } else if let Some((n, m)) = land.then(|| ratio(token)).flatten() {
                words.push_str(&format!("{n} of {m}"));
                words.push_str(
                    &token[token.trim_end_matches(|c: char| !c.is_ascii_digit()).len()..],
                );
                after_not = false;
            } else {
                words.push_str(token);
                after_not = token == "not";
            }
        }
        Self {
            separated: false,
            block: false,
            unchecked,
            item,
            heading,
            capital,
            words,
        }
    }
}

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

fn ratio(token: &str) -> Option<(u32, u32)> {
    let t = token.trim_end_matches(['.', ',', ';', ':', ')']);
    let (n, m) = t.split_once('/')?;
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if !(digits(n) && digits(m)) {
        return None;
    }
    let (n, m): (u32, u32) = (n.parse().ok()?, m.parse().ok()?);
    (n < m && m <= 1000).then_some((n, m))
}

fn table_header(raw: &str, next: Option<&str>) -> bool {
    raw.trim_start().starts_with('|')
        && next.is_some_and(|n| {
            let t = n.trim();
            t.starts_with('|')
                && t.contains('-')
                && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
        })
}

fn report_lines(tail: &str, land: bool) -> Vec<ReportLine> {
    let mut out: Vec<ReportLine> = Vec::new();
    let mut fence = None;
    let mut separated = false;
    let lines: Vec<&str> = tail.lines().collect();
    for (i, raw) in lines.iter().copied().enumerate() {
        if let Some((mark, width)) = fence {
            if needs::code_fence(raw)
                .is_some_and(|(m, n, rest)| m == mark && n >= width && rest.trim().is_empty())
            {
                fence = None;
            } else if let Some(block) = out.last_mut().filter(|b| land && b.block) {
                for word in code_work(raw).into_iter().chain(output_status(raw)) {
                    if !block.words.split(' ').any(|w| w == word) {
                        block.words.push(' ');
                        block.words.push_str(word);
                    }
                }
            }
            continue;
        }
        if let Some((mark, width, _)) = needs::code_fence(raw) {
            fence = Some((mark, width));
            separated = true;
            if land {
                let mut block = ReportLine::of(CODE, true);
                block.block = true;
                block.separated = true;
                out.push(block);
            }
            continue;
        }
        if is_finished_line(raw) || (land && table_header(raw, lines.get(i + 1).copied())) {
            continue;
        }
        let mut line = ReportLine::of(raw, land);
        if line.words.is_empty() && !line.unchecked {
            continue;
        }
        line.separated = std::mem::take(&mut separated);
        match out.last_mut() {
            Some(above) if !line.separated && !line.item && !line.capital => {
                above.unchecked |= line.unchecked;
                above.words.push(' ');
                above.words.push_str(&line.words);
            }
            _ => out.push(line),
        }
    }
    out
}

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

fn open_head(text: &str) -> Option<(&'static str, &str)> {
    head_in(text, OPEN_HEADS)
}

const LAND_HEADS: &[&str] = &[
    "blockers",
    "blocker",
    "blocking",
    "known issues",
    "known issue",
    "open questions",
    "unresolved",
    "fixmes",
    "fixme",
    "gaps",
    "possible improvements",
    "potential improvements",
    "further improvements",
    "future improvements",
    "improvement ideas",
    "worth considering",
    "optional",
    "optional follow-up",
    "optional follow-ups",
    "ideas",
    "one idea",
    "an idea",
    "suggestions",
    "suggestion",
    "suggested follow-ups",
    "possible follow-ups",
    "nice to have",
    "nice to haves",
    "nice-to-haves",
    "for later",
    "owner",
    "for the owner",
    "for you",
    "action for you",
    "actions for you",
    "action needed",
    "action required",
    "ask",
    "asks",
    "one ask",
    "an ask",
    "my ask",
    "one request",
    "a request",
    "your decision",
    "your input",
    "your action",
    "your actions",
    "decision needed",
    "decision required",
    "owner action",
    "owner actions",
    "owner step",
    "owner steps",
    "owner action needed",
    "owner action required",
    "blocking on you",
    "next session",
    "next time",
    "later",
    "regression",
    "regressions",
];

fn head_in<'t>(text: &'t str, heads: &[&'static str]) -> Option<(&'static str, &'t str)> {
    let ends = |rest: &str| {
        rest.is_empty()
            || rest.starts_with([':', '('])
            || [" -", " –", " —", " ("].iter().any(|p| rest.starts_with(p))
    };
    heads
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

fn head_tail(rest: &str) -> Option<&str> {
    let tail = [", ", " from ", " for "]
        .iter()
        .find_map(|p| rest.strip_prefix(p))?;
    let colon = tail.find(':').filter(|&at| at <= HEAD_TAIL)?;
    Some(&tail[colon..])
}

fn heading_head(text: &str) -> Option<(&'static str, &str)> {
    OPEN_HEADS
        .iter()
        .filter(|&&head| text.strip_prefix(head).is_some_and(|r| r.starts_with(',')))
        .max_by_key(|head| head.len())
        .map(|&head| (head, ""))
}

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

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

#[derive(Clone, Copy)]
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

const STATUS_PHRASES: Phrases = Phrases::of(OPEN_STATUS);

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

fn negated(item: &str) -> bool {
    item.trim_start_matches(|c: char| !c.is_alphanumeric())
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '!' | ')'))
        .next()
        .is_some_and(|first| OPEN_NEGATIONS.contains(&first))
        || item.starts_with("n/a")
}

fn open_status(line: &str, len: usize, phrases: &Phrases) -> Option<&'static str> {
    let sentence = &line[..len];
    let stands: Vec<(usize, &'static str)> = phrases_in(sentence, phrases).collect();
    phrases.table.iter().copied().find(|&word| {
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

struct Stand<'f> {
    at: usize,
    word: &'static str,
    family: Option<&'f LandFamily>,
    rank: (usize, usize),
    len: usize,
}

impl Stand<'_> {
    const fn end(&self) -> usize {
        self.at + self.len
    }

    fn reads(&self) -> Reads {
        self.family.map_or(Reads::Anywhere, |f| f.reads)
    }

    fn settled_by(&self) -> &'static [&'static str] {
        self.family.map_or(STATUS_BY, |f| f.settled_by)
    }

    fn lands_by_place(&self) -> bool {
        match self.family {
            Some(f) => f.settled_by == LANDS || f.settled_by == SHIPS,
            None => matches!(
                self.word,
                "follow-up"
                    | "follow-ups"
                    | "still open"
                    | "remains open"
                    | "left open"
                    | "left for"
            ),
        }
    }
}

fn land_status(
    line: &str,
    len: usize,
    prior: &str,
    families: &[LandFamily],
    said_main: bool,
) -> Option<&'static str> {
    let s = &line[..len];
    let rank =
        |table: &[&str], word: &str| table.iter().position(|w| *w == word).unwrap_or(usize::MAX);
    let mut stands: Vec<Stand<'_>> = phrases_in(s, &STATUS_PHRASES)
        .map(|(at, word)| Stand {
            at,
            word,
            family: None,
            rank: (0, rank(OPEN_STATUS, word)),
            len: word.len(),
        })
        .collect();
    for (k, family) in families.iter().enumerate() {
        stands.extend(phrases_in(s, &family.phrases).map(|(at, word)| Stand {
            at,
            word,
            family: Some(family),
            rank: (k + 1, rank(family.rows, word)),
            len: word.len(),
        }));
        stands.extend(
            pattern_stands(s, family.reads)
                .into_iter()
                .map(|(at, len, word)| Stand {
                    at,
                    word,
                    family: Some(family),
                    rank: (k + 1, 0),
                    len,
                }),
        );
    }
    stands.sort_by_key(|st| st.rank);
    let open = stands
        .iter()
        .find(|st| land_open(line, s, (prior, said_main), st, &stands))
        .map(|st| st.family.and_then(|f| f.says).unwrap_or(st.word));
    if !asks(s) {
        return open;
    }
    let leftover = match stated(s) {
        Some(t) if open.is_some() && reply(&line[len..]) == Some(Answer::Yes) => {
            land_status(&t, t.len(), prior, families, said_main).is_some()
        }
        _ => open.is_some(),
    };
    question_open(s, &line[len..], leftover).then(|| open.unwrap_or(QUESTION))
}

fn stated(s: &str) -> Option<String> {
    let t = s.trim_end().strip_suffix('?')?;
    let head = t.rfind(':').map_or(0, |i| i + 1);
    let words: Vec<&str> = t[head..].split_whitespace().collect();
    let state = |w: &str| {
        let w = bare(w);
        verbish(w) || matches!(w, "empty" | "clean" | "gone" | "green" | "red")
    };
    let place = |k: usize| {
        let rest: Vec<&str> = words[k..].iter().map(|w| bare(w)).collect();
        onto_main(&rest)
    };
    let first = bare(words.first()?);
    let (aux, subject, p) = if matches!(first, "is" | "are" | "was" | "were") {
        let p = (2..words.len()).find(|&k| state(words[k]) || place(k))?;
        (first, 1, p)
    } else {
        let adjective = |w: &str| {
            let w = bare(w);
            past_form(w) || matches!(w, "empty" | "clean" | "gone" | "green" | "red")
        };
        let p = (1..words.len()).find(|&k| adjective(words[k]) || place(k))?;
        if words[1..p].iter().any(|w| verbish(bare(w))) {
            return None;
        }
        ("is", 0, p)
    };
    Some(format!(
        "{} {} {aux} {}.",
        t[..head].trim_end(),
        words[subject..p].join(" "),
        words[p..].join(" ")
    ))
}

fn pattern_stands(s: &str, reads: Reads) -> Vec<(usize, usize, &'static str)> {
    if matches!(reads, Reads::Absent) {
        let words: Vec<(usize, &str)> = words_at(s, 0, s.len()).collect();
        return words
            .windows(3)
            .filter(|w| {
                bare(w[0].1) == "no"
                    && bare(w[1].1).contains('-')
                    && !w[1].1.ends_with([',', ';', ':', '.'])
                    && matches!(bare(w[2].1), "test" | "tests")
            })
            .map(|w| {
                let at = w[0].0 + w[0].1.find("no").unwrap_or(0);
                let (raw, noun) = (w[2].1, bare(w[2].1));
                let end = w[2].0 + raw.find(noun).unwrap_or(0) + noun.len();
                (at, end - at, "no test")
            })
            .collect();
    }
    if !matches!(reads, Reads::Change | Reads::Percent) {
        return Vec::new();
    }
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    words_at(s, 0, s.len())
        .filter_map(|(p, raw)| {
            let lead = raw.len() - raw.trim_start_matches('(').len();
            let t = raw[lead..].trim_end_matches(['.', ',', ';', ':', ')', '!', '?']);
            let at = p + lead;
            match reads {
                Reads::Change => {
                    let number = t
                        .strip_prefix('#')
                        .or_else(|| t.strip_prefix('!'))
                        .or_else(|| t.strip_prefix("gh-"))
                        .or_else(|| {
                            ["/pull/", "/pulls/", "/merge_requests/"]
                                .iter()
                                .find_map(|p| t.split_once(p))
                                .filter(|(site, _)| site.contains("://"))
                                .map(|(_, n)| n.trim_end_matches('/'))
                        });
                    number
                        .filter(|n| digits(n))
                        .map(|_| (at, t.len(), CHANGE_NUMBER))
                }
                Reads::Percent => t
                    .strip_suffix('%')
                    .filter(|n| below_hundred(n))
                    .map(|_| (at, t.len(), PERCENT)),
                _ => None,
            }
        })
        .collect()
}
const QUESTION: &str = "a question";

const CHANGE_NUMBER: &str = "#n";

const PERCENT: &str = "%";

const COUNT_BESIDE_NONE: &str = "a count beside none";

fn asks(s: &str) -> bool {
    let t = s.trim_end();
    t.ends_with('?') && !quoted(t, t.len() - 1)
}

fn reply(rest: &str) -> Option<Answer> {
    let t = reply_text(rest);
    let end = t.len();
    let words: Vec<&str> = t
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let first = *words.first()?;
    let bare_word = t.strip_prefix(first).is_some_and(|r| {
        r.is_empty()
            || r.starts_with(['.', ',', ';', ':', '!', '—', '–', ')'])
            || [" —", " –", " - "].iter().any(|p| r.starts_with(p))
    });
    let empty = words
        .windows(2)
        .any(|w| matches!(w, ["is" | "are" | "was" | "were", "empty"]));
    Some(
        if bare_word
            && matches!(
                first,
                "yes" | "yep" | "yeah" | "yup" | "correct" | "confirmed" | "affirmative" | "indeed"
            )
        {
            let main_too = words.get(1) == Some(&"and")
                && main_claimed(&words[2..].join(" "))
                && !words
                    .iter()
                    .any(|w| negation(w) || UNSETTLED.contains(w) || DEFICITS.contains(w));
            if yes_tail(&words[1..]) || main_too {
                Answer::Yes
            } else {
                Answer::Said
            }
        } else if (bare_word
            && matches!(first, "no" | "nope" | "none" | "nothing" | "nil" | "zero"))
            || nil_words(&t[..end])
            || empty
        {
            Answer::No
        } else {
            Answer::Said
        },
    )
}

fn reply_text(rest: &str) -> &str {
    let t = rest.trim_start();
    let t = t
        .strip_prefix("a:")
        .or_else(|| t.strip_prefix("answer:"))
        .unwrap_or(t)
        .trim_start();
    &t[..sentence_starts(t).get(1).copied().unwrap_or(t.len())]
}

fn yes_tail(tail: &[&str]) -> bool {
    let mut k = 0;
    while k < tail.len() {
        let w = tail[k];
        let run = matches!(w, "run" | "build" | "job" | "pipeline")
            && tail
                .get(k + 1)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let every = matches!(w, "every" | "each")
            && tail
                .get(k + 1)
                .is_some_and(|n| matches!(*n, "lane" | "check" | "job" | "cell" | "one" | "test"));
        if run || every {
            k += 2;
        } else if YES_TAIL.contains(&w) || sha(w) {
            k += 1;
        } else {
            return false;
        }
    }
    true
}

const YES_TAIL: &[&str] = &[
    "as",
    "at",
    "on",
    "in",
    "to",
    "into",
    "the",
    "remote",
    "main",
    "origin/main",
    "upstream/main",
    "both",
    "all",
    "of",
    "them",
    "it",
    "is",
    "are",
    "merged",
    "pushed",
    "and",
];

fn sha(w: &str) -> bool {
    (6..=40).contains(&w.len())
        && w.bytes().all(|b| b.is_ascii_hexdigit())
        && w.bytes().any(|b| b.is_ascii_digit())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Yes,
    No,
    Said,
}

const DEFICITS: &[&str] = &[
    "need",
    "needs",
    "needed",
    "required",
    "requires",
    "missing",
    "left",
    "pending",
    "open",
    "broken",
    "fail",
    "fails",
    "failed",
    "failing",
    "skipped",
    "flaky",
    "blocked",
    "outstanding",
    "unmerged",
    "unpushed",
    "uncommitted",
    "unlanded",
    "stale",
    "red",
    "deferred",
    "disabled",
    "waiting",
    "owed",
];

const UNLANDED_DEFICITS: &[&str] = &["unmerged", "unpushed", "uncommitted", "unlanded"];

fn question_open(s: &str, rest: &str, leftover: bool) -> bool {
    let q = s.rsplit_once(':').map_or(s, |(_, q)| q);
    let words: Vec<&str> = q
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let first = words.first().copied().unwrap_or("");
    let second = words.get(1).copied().unwrap_or("");
    let addressed = words.contains(&"or")
        || first == "shall"
        || words.windows(2).any(|w| {
            matches!(
                w,
                [
                    "should" | "shall" | "may" | "can" | "could" | "would" | "will" | "do" | "did",
                    "i" | "we" | "you"
                ] | ["want", "me"]
                    | ["like", "me"]
            )
        });
    if addressed {
        return true;
    }
    let Some(answer) = reply(rest) else {
        return true;
    };
    let wh = matches!(
        first,
        "what"
            | "what's"
            | "which"
            | "who"
            | "who's"
            | "whom"
            | "whose"
            | "when"
            | "where"
            | "where's"
            | "why"
            | "how"
    );
    let change = words
        .iter()
        .any(|w| CHANGE_NOUNS.contains(w) || w.starts_with('#'))
        || q.contains("pull request");
    let lands = |w: &[&str]| {
        w.iter().any(|x| SHIPS.contains(x) || LANDED.contains(x))
            || (0..w.len()).any(|i| onto_main(&w[i..]))
    };
    let told: Vec<&str> = reply_text(rest)
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    if wh {
        if second == "about" {
            // `What about the docs? Merged with the fix.`: a thing's state,
            // which only its landing answers (the answer is read on its own).
            return leftover || !lands(&told);
        }
        let remote = |w: &&str| matches!(*w, "origin/main" | "upstream/main" | "origin/master");
        let placed = matches!(first, "where" | "where's")
            && told.iter().any(remote)
            && told
                .iter()
                .all(|w| remote(w) || YES_TAIL.contains(w) || sha(w));
        return !((matches!(first, "why" | "how") || placed) && !leftover) && answer != Answer::No;
    }
    let settled_word = |w: &&str| SETTLED.contains(w) || matches!(*w, "clean" | "gone" | "empty");
    let exists = second == "there"
        || !(words.iter().any(|w| verbish(w) || settled_word(w)) || lands(&words))
        || words.iter().any(|w| {
            matches!(
                *w,
                "any"
                    | "anything"
                    | "anyone"
                    | "anybody"
                    | "something"
                    | "else"
                    | "left"
                    | "remaining"
                    | "outstanding"
            )
        });
    if exists {
        return answer != Answer::No;
    }
    let deficit = words.iter().find(|w| DEFICITS.contains(w));
    match answer {
        Answer::Yes => {
            let gone = |w: &&str| GONE.contains(w) || matches!(*w, "gone" | "empty");
            leftover
                || deficit.is_some()
                || !(lands(&words)
                    || (!change && words.iter().any(settled_word))
                    || words.iter().any(gone))
        }
        Answer::No => {
            !((leftover || deficit.is_some())
                && (!change
                    || deficit.is_some_and(|d| UNLANDED_DEFICITS.contains(d))
                    || lands(&told)))
        }
        Answer::Said => true,
    }
}

fn land_open(
    line: &str,
    s: &str,
    (prior, said_main): (&str, bool),
    st: &Stand<'_>,
    stands: &[Stand<'_>],
) -> bool {
    let (at, end) = (st.at, st.end());
    if quoted(s, at) || in_path(s, at, end) || nil_item(s, at, end) {
        return false;
    }
    if matches!(st.reads(), Reads::Exception) {
        return nothing_but(s, st) || (excepted(s, st) && excepting(s, stands));
    }
    if matches!(st.reads(), Reads::Qualified) {
        return qualified_nil(s, st);
    }
    if matches!(st.reads(), Reads::OnlyTail) {
        return only_item(s, st);
    }
    if matches!(st.reads(), Reads::Party) {
        let rest = &s[st.end()..];
        let clause: Vec<&str> = rest[..clause_end(rest, 0)]
            .split_whitespace()
            .map(bare)
            .collect();
        let owed = |modal: usize, verb: usize| {
            clause
                .get(verb)
                .is_some_and(|w| matches!(*w, "decide" | "fix" | "choose" | "rule"))
                && !clause[..modal].iter().any(|w| negation(w))
                && !clause
                    .get(verb + 1)
                    .is_some_and(|w| matches!(*w, "nothing" | "none"))
        };
        if (0..clause.len()).any(|i| {
            (matches!(clause[i], "has" | "have" | "needs" | "need")
                && clause.get(i + 1) == Some(&"to")
                && owed(i, i + 2))
                || (matches!(clause[i], "should" | "must") && owed(i, i + 1))
        }) {
            return true;
        }
        return s[..st.at]
            .trim_end()
            .strip_suffix(':')
            .is_some_and(|label| !label.trim().is_empty())
            && rest[..clause_end(rest, 0)]
                .split_whitespace()
                .map(bare)
                .all(|w| w.is_empty() || matches!(w, "only" | "alone" | "now"));
    }
    if matches!(
        st.word,
        "stash" | "stashes" | "draft" | "drafts" | "wip" | "worktree"
    ) && s[end..]
        .split_whitespace()
        .next()
        .map(|w| bare(w).trim_end_matches("'s"))
        .is_some_and(|w| CODE_PARTS.contains(&w))
    {
        return false;
    }
    if s[end..]
        .split_whitespace()
        .next()
        .map(bare)
        .is_some_and(|w| matches!(w, "nothing" | "none"))
    {
        return false;
    }
    let from = clause_start(s, at);
    let to = clause_end(s, end);
    let before: Vec<&str> = s[from..at].split_whitespace().map(bare).collect();
    let after: Vec<&str> = s[end..to]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let prev = before.last().copied();
    let next = after.first().copied();
    if st.word.starts_with("not ") && prev.is_some_and(|p| OPEN_MODALS.contains(&p)) {
        return false;
    }
    if st.word == "unstaged" && next.is_some_and(|n| matches!(n, "it" | "them" | "those" | "these"))
    {
        return false;
    }
    if (matches!(st.word, "up to date with" | "up-to-date with") && before.contains(&"your"))
        || (matches!(st.word, "provided" | "providing" | "unknown")
            && prev.is_some_and(|p| DETERMINERS.contains(&p) || p == "as"))
    {
        return false;
    }
    if matches!(st.word, "could not" | "couldn't") && cannot_happen(&s[end..to]) {
        return false;
    }
    let proof_row = PROOF_ROWS.contains(&st.word)
        || (st.word == "still" && next.is_some_and(|n| PROOF_ROWS.contains(&n)));
    if proof_row && proves_by_failing(s, (from, to), (at, end)) {
        return false;
    }
    if st.family.is_none()
        && st.word == "skipped"
        && prev.is_some_and(|p| ["is", "are", "be"].contains(&p))
        && !before.iter().any(|w| TASK_ITEMS.contains(w))
    {
        return false;
    }
    if described(&before) || behind(&before) {
        return false;
    }
    if matches!(st.reads(), Reads::Unlanded | Reads::Undone) {
        return landing_denied(s, from, at, &after, st, stands);
    }
    if aside_settled(s, st) || also_landed(s, (from, to), st, stands) {
        return false;
    }
    let settles = match past_aside(s, to) {
        Some((joined, to)) => settled(&joined, from, to, st),
        None => settled(s, from, to, st),
    };
    if settles || (st.word != "yet" && denied(s, from, at, st, stands)) {
        return false;
    }
    match st.reads() {
        Reads::Anywhere if matches!(st.word, "local main" | "main locally") => {
            !even_with_remote(&after)
        }
        Reads::Anywhere
            if st.word == "partial" && next.is_some_and(|n| PARTIAL_TERMS.contains(&n)) =>
        {
            false
        }
        Reads::Anywhere if matches!(st.word, "for review" | "for a review") => {
            before.iter().any(|w| {
                matches!(
                    *w,
                    "pushed" | "force-pushed" | "sent" | "posted" | "submitted" | "up" | "put"
                ) || CHANGE_NOUNS.contains(w)
                    || branch_name(w)
            })
        }
        Reads::Anywhere
            if st.word == "separately"
                && before.iter().any(|w| TESTED.contains(w))
                && !before.iter().any(|w| negation(w) || UNSETTLED.contains(w)) =>
        {
            false
        }
        Reads::Anywhere
        | Reads::Exception
        | Reads::Party
        | Reads::OnlyTail
        | Reads::Qualified
        | Reads::Unlanded
        | Reads::Undone => true,
        Reads::Alone => {
            alone(&s[end..to])
                && !prev.is_some_and(|p| matches!(p, "main" | "origin/main" | "master"))
        }
        Reads::Beside(nouns) => {
            let (a, b) = stretch(s, from, to, (at, end), st.settled_by());
            let counted = prev.and_then(count).is_some_and(|n| n > 0);
            let tally: Vec<&str> = s[..at].split_whitespace().map(bare).collect();
            let tail = s[end..].trim_start();
            let tallied = st.word == "ignored"
                && (tail.starts_with(')') || tail.split_whitespace().all(|w| bare(w).is_empty()))
                && counted
                && tally
                    .windows(2)
                    .any(|w| count(w[0]).is_some() && w[1] == "passed")
                && tally
                    .windows(2)
                    .any(|w| count(w[0]) == Some(0) && w[1] == "failed");
            if tallied {
                return false;
            }
            let turned_off = prev.is_some_and(|p| matches!(p, "i" | "we"))
                && next.is_some_and(|n| matches!(n, "it" | "them"))
                && matches!(
                    st.word,
                    "disabled"
                        | "ignored"
                        | "quarantined"
                        | "muted"
                        | "skipped"
                        | "suppressed"
                        | "silenced"
                        | "turned off"
                        | "switched off"
                        | "commented out"
                );
            let code_mark = matches!(
                (prev, st.word),
                (Some("check"), "ignored") | (Some("lint"), "allowed")
            );
            !code_mark
                && !next.is_some_and(|n| CONTROLS.contains(&n))
                && (counted
                    || turned_off
                    || nouns_in(s, a, b, nouns)
                    || labelled(s, from, &before, nouns))
        }
        Reads::Later => {
            let with = st.word.ends_with(" with")
                && !after
                    .iter()
                    .find(|w| !matches!(**w, "the" | "a" | "an"))
                    .is_some_and(|w| {
                        LATER_WITH.contains(w)
                            || (w.starts_with('v')
                                && w[1..].starts_with(|c: char| c.is_ascii_digit()))
                            || w.starts_with(|c: char| c.is_ascii_digit())
                    });
            let dated = DATED.contains(&st.word)
                && before.iter().any(|w| {
                    past_form(w) || matches!(*w, "came" | "went" | "ran" | "got" | "was" | "were")
                })
                && !before.iter().enumerate().any(|(i, w)| {
                    let put_off = *w == "out"
                        && i.checked_sub(1).is_some_and(|p| {
                            matches!(
                                before[p],
                                "pushed" | "moved" | "bumped" | "put" | "kicked" | "punted"
                            )
                        });
                    put_off
                        || negation(w)
                        || (UNSETTLED.contains(w) && !(*w == "to" && onto_main(&before[i..])))
                        || matches!(
                            *w,
                            "back"
                                | "until"
                                | "till"
                                | "by"
                                | "for"
                                | "moved"
                                | "bumped"
                                | "rescheduled"
                                | "delayed"
                                | "postponed"
                                | "deferred"
                                | "planned"
                                | "scheduled"
                                | "slated"
                                | "set"
                                | "booked"
                                | "requested"
                                | "asked"
                                | "pencilled"
                                | "penciled"
                        )
                });
            !with
                && !dated
                && !AUTOMATIC.iter().any(|p| s.contains(p))
                && !past_attributive(st, prev, &after)
        }
        Reads::Until => {
            !before
                .iter()
                .any(|w| MECHANISMS.contains(w) && !(WAITS.contains(w) && waits_on_work(&before)))
                && !AUTOMATIC.iter().any(|p| s.contains(p))
        }
        Reads::Elided => next.is_none(),
        Reads::Intent => {
            (st.word != "consider" || s[..at].trim().is_empty())
                && !ends_intent(&s[end..], line[s.len()..].trim().is_empty())
                && !after.iter().any(|w| DENIALS.contains(w))
        }
        Reads::Gerund => next.is_some_and(|n| {
            (n.len() > 4 && n.ends_with("ing") && !n.ends_with("thing") && n != "during")
                || matches!(n, "a" | "an" | "another" | "more" | "some")
                || (n == "to"
                    && (s[..at].trim().is_empty()
                        || prev.is_some_and(|p| matches!(p, "i" | "we" | "also" | "just"))))
        }),
        Reads::Still => {
            let rest: Vec<&str> = s[end..to].split_whitespace().map(bare).collect();
            if rest
                .windows(2)
                .any(|w| matches!(w, ["on", "purpose"] | ["by", "design"]))
                && !before.iter().any(|w| FAULT_NOUNS.contains(w))
            {
                return false;
            }
            let (first, second) = (rest.first().copied(), rest.get(1).copied());
            if matches!(first, Some("catches" | "catch")) {
                return !rest[1..].iter().any(|w| FAULT_NOUNS.contains(w));
            }
            if matches!(first, Some("pass" | "passes"))
                && rest.iter().enumerate().any(|(i, w)| {
                    let later = &rest[i + 1..];
                    (matches!(*w, "if" | "when")
                        && later
                            .iter()
                            .any(|w| matches!(*w, "removed" | "reverted" | "undone")))
                        || (*w == "without"
                            && later
                                .iter()
                                .take(3)
                                .any(|w| matches!(*w, "fix" | "change" | "patch")))
                })
            {
                return true;
            }
            let ok = match rest.get(1..) {
                Some(["with", code]) | Some(["with", "a" | "an", code]) => ok_status(code),
                _ => false,
            };
            let more = s[to..]
                .strip_prefix(',')
                .filter(|_| rest.len() == 1)
                .map(|r| {
                    let words: Vec<&str> = r[..clause_end(r, 0)]
                        .split_whitespace()
                        .map(bare)
                        .filter(|w| !w.is_empty())
                        .collect();
                    (
                        !words.is_empty(),
                        !words.is_empty() && !words.iter().any(|w| verbish(w)),
                    )
                });
            let (any_more, fragment) = more.unwrap_or((false, false));
            let np = before
                .iter()
                .position(|w| PREPOSITIONS.contains(w))
                .unwrap_or(before.len());
            let head = before[..np]
                .iter()
                .rfind(|w| !matches!(**w, "is" | "are" | "was" | "were"));
            let fault = head.is_some_and(|w| FAULT_NOUNS.contains(w));
            fault
                || s[to..].starts_with(':')
                || first.is_none_or(|n| {
                    !((STILL_GOOD.contains(&n) && !fragment)
                        || (STILL_HOLDS.contains(&n)
                            && !any_more
                            && (second.is_none_or(|w| {
                                matches!(w, "for" | "to" | "as" | "on" | "in" | "after" | "from")
                                    || STILL_MANNER.contains(&w)
                            }) || ok))
                        || (matches!(n, "exists" | "exist") && second == Some("as"))
                        || n.starts_with(|c: char| c.is_ascii_digit())
                        || (DETERMINERS.contains(&n) && !second.is_some_and(|w| OLD.contains(&w))))
                })
        }
        Reads::Ref if st.word == "--no-merged" => !lists_none(&s[end..]),
        Reads::Ref => off_main(&s[end..]),
        Reads::Fraction if st.word == "only" => {
            (before.is_empty() && only_part(&after))
                || (after.is_empty()
                    && !before.iter().any(|w| matches!(*w, "was" | "were"))
                    && before.iter().any(|w| {
                        matches!(
                            *w,
                            "covered" | "tested" | "exercised" | "checked" | "verified"
                        )
                    }))
        }
        Reads::Fraction => {
            (short_count(&before, &s[end..to]) || leading_part(&before, &s[end..]))
                && !rest_settled(&line[end..])
        }
        Reads::NotDone => next.is_some_and(|n| {
            !AVOIDED.contains(&n)
                && !negation(n)
                && !(STEADY.contains(&n) && !prev.is_some_and(|p| matches!(p, "i" | "we")))
        }),
        Reads::Lost => next.is_some_and(|n| WORKS.contains(&n)),
        Reads::Better => {
            next.is_some_and(|n| (n.len() > 4 && n.ends_with("ed")) || COMPARATIVES.contains(&n))
        }
        Reads::Concession => conceding(&format!("{prior}{}", &s[..at]), &s[end..]),
        Reads::State => {
            !next.is_some_and(|n| CONTROLS.contains(&n) || n == "while")
                && !prev.is_some_and(|p| STOPS.contains(&p))
                && !after.iter().take(3).any(|w| ATTEMPTS.contains(w))
        }
        Reads::End => {
            let kept_off_main = matches!(st.word, "on origin" | "on the remote" | "local")
                && (prev.is_some_and(|p| MAINS.contains(&p))
                    || before.iter().any(|w| {
                        GONE.contains(w)
                            || matches!(*w, "tag" | "tags" | "deleted" | "cleaned")
                            || version(w)
                    }));
            next.is_none()
                && !kept_off_main
                && !prev.is_some_and(|p| matches!(p, "too" | "as" | "so" | "how"))
                && !(st.word == "aside" && before.is_empty() && s[to..].starts_with(':'))
        }
        Reads::Count => {
            (next.is_none() || next == Some("to"))
                && before.iter().rev().take(3).any(|w| counted(w))
        }
        Reads::OneMore => {
            let settles = |w: &&str| {
                ((SETTLED.contains(w) || LANDED.contains(w))
                    && (past_form(w) || matches!(*w, "done" | "green" | "clean")))
                    || negation(w)
            };
            !next.is_some_and(|n| matches!(n, "time" | "times"))
                && !before
                    .iter()
                    .any(|w| SETTLED.contains(w) || LANDED.contains(w) || negation(w))
                && !after
                    .iter()
                    .skip(1)
                    .take_while(|w| {
                        !CONDITIONS.contains(w) && !matches!(**w, "before" | "so" | "to")
                    })
                    .any(settles)
        }
        Reads::Only => {
            (s[..at].trim().is_empty()
                && s.split_whitespace().count() <= 5
                && !s[end..].split_whitespace().map(bare).any(verbish))
                || (before.is_empty()
                    && s[..from].trim_end().ends_with(',')
                    && s[clause_start(s, from.saturating_sub(1))..from]
                        .split_whitespace()
                        .map(bare)
                        .next()
                        .is_some_and(|w| matches!(w, "nothing" | "none" | "no" | "zero")))
        }
        Reads::Elliptic => {
            (1..=3).contains(&before.len())
                && !before.iter().any(|w| {
                    verbish(w)
                        || negation(w)
                        || CUTS.contains(w)
                        || PREPOSITIONS.contains(w)
                        || LEADS.contains(w)
                })
                && !s[end..].split_whitespace().map(bare).any(|w| {
                    SETTLED.contains(&w)
                        || LANDED.contains(&w)
                        || (st.word == "after" && verbish(w))
                })
        }
        Reads::Absent => {
            let past = prev.is_some_and(|p| matches!(p, "was" | "were" | "had"));
            let kind = st.len != st.word.len();
            (next.is_none_or(|n| GAP_PREPOSITIONS.contains(&n)) && !(kind && past))
                || (next == Some("that")
                    && !past
                    && after.get(1).is_some_and(|w| {
                        matches!(*w, "a" | "an" | "the" | "its" | "each" | "every")
                    }))
        }
        Reads::Nobody => {
            let nothing = st.word == "nothing";
            let filler = |w: &str| {
                matches!(
                    w,
                    "has" | "have" | "had" | "yet" | "ever" | "actually" | "else"
                )
            };
            let Some(k) = after.iter().position(|w| !filler(w)) else {
                return false;
            };
            let w = after[k];
            if nothing {
                matches!(
                    w,
                    "ran" | "run" | "tested" | "checked" | "verified" | "measured"
                ) || (matches!(w, "tests" | "covers" | "exercises")
                    && after
                        .get(k + 1)
                        .is_some_and(|o| matches!(*o, "it" | "them" | "this" | "that")))
            } else {
                CHECKED.contains(&w)
            }
        }
        Reads::Onto => {
            let bracketed: Vec<&str> = s[end..]
                .trim_start()
                .strip_prefix('(')
                .and_then(|r| r.split_once(')'))
                .map(|(inner, _)| inner.split_whitespace().map(bare).collect())
                .unwrap_or_default();
            ((onto_branch(&after) || (after.is_empty() && onto_branch(&bracketed)))
                && !main_claimed(s))
                || (matches!(st.word, "pushed" | "committed")
                    && unplaced(s, (prior, said_main), &before, &after))
        }
        Reads::Dropped => !disposable(st.word, &before, &after),
        Reads::OnBranch if !matches!(st.word, "on" | "in") => prev.is_some_and(|p| {
            (branch_name(p) && !p.split_once('/').is_some_and(|(_, m)| MAINS.contains(&m)))
                || matches!(p, "branch" | "worktree")
        }),
        Reads::OnBranch => {
            on_branch(&after, prev.is_some_and(|p| LANDING_VERBS.contains(&p))) && !main_claimed(s)
        }
        Reads::Change => change_open(s, (from, to), st, &before, &after),
        Reads::Percent => {
            let rolling = before.iter().any(|w| {
                matches!(
                    *w,
                    "rollout"
                        | "rolled"
                        | "rolling"
                        | "canary"
                        | "deploy"
                        | "deployed"
                        | "ramp"
                        | "ramped"
                        | "staged"
                )
            });
            let label = s[..at]
                .trim_end()
                .strip_suffix(':')
                .and_then(|l| l.split_whitespace().last())
                .is_some_and(|l| WORK_LABELS.contains(&bare(l)));
            (st.word == "%" || prev.is_some_and(below_hundred))
                && (rolling || percent_part(&s[end..]) || (label && after.is_empty()))
        }
        Reads::Without => after
            .iter()
            .find(|w| !DETERMINERS.contains(w) && !matches!(**w, "any" | "its" | "their"))
            .is_some_and(|w| OWED_WITHOUT.contains(w)),
        Reads::Approval => {
            let (a, b) = stretch(s, from, to, (at, end), LANDS);
            let landed_after = {
                let rest = line[to..].trim_start_matches([',', ';', '—', '–', ' ']);
                let words: Vec<&str> = rest[..clause_end(rest, 0)]
                    .split_whitespace()
                    .map(bare)
                    .filter(|w| !w.is_empty())
                    .collect();
                to < line.len()
                    && settles_with(&words, SHIPS, true)
                    && words.first().is_some_and(|w| {
                        SHIPS.contains(w)
                            || LANDING_VERBS.contains(w)
                            || matches!(*w, "then" | "and" | "since" | "now")
                    })
            };
            let upstream = st.word == "accepted" && next == Some("upstream");
            nouns_in(s, a, b, APPROVED_THINGS) && !landed_after && !upstream
        }
        Reads::Tally => {
            let one = prev.is_some_and(|p| matches!(p, "a" | "an" | "one" | "single"))
                && before.len().checked_sub(2).is_none_or(|i| {
                    matches!(
                        before[i],
                        "has"
                            | "have"
                            | "had"
                            | "shows"
                            | "showed"
                            | "hit"
                            | "hits"
                            | "got"
                            | "saw"
                            | "sees"
                            | "is"
                            | "are"
                            | "was"
                            | "were"
                            | "still"
                            | "remains"
                            | "with"
                            | "there's"
                            | "left"
                    )
                });
            let labelled = next.is_none()
                && s[to..].strip_prefix(':').is_some_and(|r| {
                    let mut item = r[..clause_end(r, 0)].split_whitespace().map(bare);
                    item.next().and_then(count).is_some_and(|n| n > 0) && item.next().is_none()
                });
            labelled
                || ((prev.and_then(count).is_some_and(|n| n > 0) || one)
                    && next.is_none_or(|n| {
                        PREPOSITIONS.contains(&n) || JOINS.contains(&n) || verbish(n) || n == "left"
                    }))
        }
        Reads::Rerun => {
            let mechanism = st.word == "retried"
                && prev.is_some_and(|p| matches!(p, "is" | "are" | "gets" | "get"))
                && !before.iter().any(|w| CHECK_NOUNS.contains(w))
                && !s.split_whitespace().map(bare).any(|w| {
                    matches!(
                        w,
                        "pass"
                            | "passes"
                            | "passed"
                            | "passing"
                            | "green"
                            | "succeeds"
                            | "succeeded"
                            | "fail"
                            | "fails"
                            | "failed"
                            | "failing"
                            | "red"
                            | "second"
                            | "until"
                    )
                });
            !mechanism
                && !s[..at]
                    .split_whitespace()
                    .map(bare)
                    .any(|w| RETRIED_STEPS.contains(&w))
        }
        Reads::Status => {
            let label_text = s[..at]
                .trim_end_matches(|c: char| {
                    c.is_whitespace() || !(c.is_ascii() || c.is_alphanumeric())
                })
                .strip_suffix(':');
            let label = label_text.is_some_and(|l| l.chars().any(char::is_alphanumeric));
            let counted = prev.and_then(count).is_some_and(|n| n > 0);
            let opens = matches!(st.word, "skip" | "skips")
                && s[..at].trim().is_empty()
                && !after.iter().any(|w| verbish(w));
            if st.word == "n/a" {
                label
                    && after.is_empty()
                    && label_text
                        .and_then(|l| l.split_whitespace().last())
                        .is_some_and(|w| CHECK_LABELS.contains(&bare(w)))
            } else {
                (label && after.is_empty()) || counted || opens
            }
        }
        Reads::Most => {
            let object = prev.is_some_and(|p| {
                SHIPS.contains(&p) || matches!(p, "pushed" | "committed" | "landed" | "done")
            });
            object
                || !prev.is_some_and(|p| {
                    matches!(p, "the" | "at" | "a" | "an" | "which" | "what" | "whose")
                }) && (after
                    .iter()
                    .any(|w| SETTLED.contains(w) || LANDED.contains(w))
                    || (0..after.len()).any(|i| onto_main(&after[i..])))
        }
    }
}

const CHECK_LABELS: &[&str] = &[
    "appkit", "measure", "verify", "check", "checks", "test", "tests", "lab", "lane", "lanes",
    "fmt", "tippy", "census", "spec", "perf", "cells", "cell", "suite", "e2e",
];

const RETRIED_STEPS: &[&str] = &[
    "push", "pushed", "pushes", "pull", "fetch", "fetched", "clone", "download", "upload",
];

fn verbish(w: &str) -> bool {
    VERBS.contains(&w)
        || AUX.contains(&w)
        || SETTLED.contains(&w)
        || STILL_GOOD.contains(&w)
        || MORE_VERBS.contains(&w)
        || past_form(w)
}

fn past_attributive(st: &Stand<'_>, prev: Option<&str>, after: &[&str]) -> bool {
    st.word == "later"
        && prev.is_some_and(|p| matches!(p, "a" | "an" | "the"))
        && after.first().is_some_and(|n| !verbish(n))
        && after
            .get(1)
            .is_some_and(|v| past_form(v) || matches!(*v, "was" | "were" | "had" | "ran"))
}

const STILL_HOLDS: &[&str] = &[
    "stable",
    "responds",
    "respond",
    "load",
    "taken",
    "served",
    "serves",
    "reachable",
    "usable",
    "readable",
    "unchanged",
    "byte-identical",
    "identical",
    "restore",
    "restores",
    "resume",
    "resumes",
    "reconnect",
    "reconnects",
];

const STILL_MANNER: &[&str] = &["unchanged", "fine", "correctly", "normally", "identically"];

fn ok_status(w: &str) -> bool {
    matches!(w, "ok" | "success")
        || (w.len() == 3 && w.starts_with('2') && w.bytes().all(|b| b.is_ascii_digit()))
}

const MORE_VERBS: &[&str] = &[
    "exists", "exist", "matters", "counts", "differs", "changes", "change", "uses", "use", "calls",
    "touches", "stays", "stay", "lives", "live", "took", "brought", "carried",
];

const NOT_PAST: &[&str] = &[
    "need", "embed", "speed", "feed", "seed", "proceed", "succeed", "exceed", "indeed", "hundred",
    "sacred", "naked", "wicked", "kindred",
];

fn past_form(w: &str) -> bool {
    w.len() > 3 && w.ends_with("ed") && !NOT_PAST.contains(&w)
}

const ATTEMPTS: &[&str] = &[
    "approach",
    "attempt",
    "attempts",
    "idea",
    "plan",
    "design",
    "experiment",
    "try",
    "prototype",
];

const STOPS: &[&str] = &[
    "stops", "stop", "stopped", "avoids", "avoid", "avoided", "prevents", "prevent",
];

fn counted(w: &str) -> bool {
    count(w).is_some_and(|n| n > 0)
        || matches!(
            w,
            "a" | "an" | "few" | "some" | "several" | "more" | "many" | "couple" | "only" | "last"
        )
}

const LEADS: &[&str] = &[
    "even", "right", "just", "only", "soon", "long", "shortly", "day", "week", "shortly",
];

const GAP_PREPOSITIONS: &[&str] = &[
    "for", "on", "of", "in", "at", "around", "about", "covering", "against", "under", "yet",
];

const CHECKED: &[&str] = &[
    "run",
    "ran",
    "tested",
    "test",
    "reviewed",
    "review",
    "checked",
    "check",
    "verified",
    "verify",
    "tried",
    "try",
    "looked",
    "measured",
    "confirmed",
    "approved",
    "signed",
    "merged",
    "pushed",
    "landed",
    "released",
    "published",
];

const WAITS: &[&str] = &["waits", "wait", "waiting"];

const WORK_ITEMS: &[&str] = &[
    "docs",
    "doc",
    "documentation",
    "pr",
    "prs",
    "release",
    "changelog",
    "migration",
    "tag",
    "readme",
    "port",
    "upgrade",
    "cleanup",
    "rest",
    "remainder",
    "rollout",
    "deploy",
];

fn waits_on_work(before: &[&str]) -> bool {
    before
        .iter()
        .take_while(|w| !WAITS.contains(w))
        .any(|w| WORK_ITEMS.contains(w))
}

fn ends_intent(rest: &str, last: bool) -> bool {
    let words: Vec<&str> = rest
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    last && match words.as_slice() {
        ["stop" | "end"] => true,
        ["stop" | "end", w] => matches!(*w, "here" | "now" | "there"),
        [
            "stop" | "end",
            "here" | "now" | "there",
            "since" | "as" | "because",
            reason @ ..,
        ] => {
            (0..reason.len()).any(|i| onto_main(&reason[i..]))
                && !reason
                    .iter()
                    .any(|w| negation(w) || UNSETTLED.contains(w) || DEFICITS.contains(w))
        }
        ["leave", "it" | "things", "there"] | ["leave", "it", "at", "that"] => true,
        ["wrap", "up", w] | ["wrap", "it" | "things", "up", w] => matches!(*w, "here" | "there"),
        _ => false,
    }
}

const NIL: &[&str] = &["none", "nothing", "n/a", "nil", "zero", "0", "empty", "no"];

const NIL_TAIL: &[&str] = &[
    "left",
    "remaining",
    "found",
    "open",
    "pending",
    "outstanding",
    "at",
    "all",
    "whatsoever",
];

fn nil_words(item: &str) -> bool {
    let mut words = item.split_whitespace().map(bare).filter(|w| !w.is_empty());
    words.next().is_some_and(|w| NIL.contains(&w)) && words.all(|w| NIL_TAIL.contains(&w))
}

fn nil_item(s: &str, at: usize, end: usize) -> bool {
    if s[clause_start(s, at)..at].split_whitespace().count() > 1 {
        return false;
    }
    let colon = s[end..].find(':').map(|c| (end + c, 1));
    let dash = [" — ", " – "]
        .iter()
        .filter_map(|d| s[end..].find(d).map(|c| (end + c, d.len())))
        .min();
    let Some((colon, mark)) = [colon, dash].into_iter().flatten().min() else {
        return false;
    };
    let label = &s[end..colon];
    if label.contains(CLAUSE_MARKS) || label.split_whitespace().count() > 2 {
        return false;
    }
    nil_list(&s[colon + mark..])
}

fn nil_list(item: &str) -> bool {
    if nil_aside(item).is_some_and(|(_, inner)| placed_count(inner)) {
        return false;
    }
    let item = nil_aside(item).map_or(item, |(nil, _)| nil);
    let item = item.trim().trim_end_matches(['.', ';', '!', ' ']);
    if matches!(item, "-" | "—" | "–") {
        // A dash for none, as a list or a table writes it (`Open PRs: —`).
        return true;
    }
    let parts: Vec<&str> = item.split([',', ';']).collect();
    let fresh = |part: &str| {
        let words: Vec<&str> = part
            .split_whitespace()
            .map(bare)
            .filter(|w| !w.is_empty())
            .collect();
        matches!(words.as_slice(), [n, rest @ ..] if NIL.contains(n)
            && !rest.is_empty()
            && rest.iter().all(|w| matches!(*w, "added" | "new" | "introduced")))
    };
    let labelled = |part: &str| {
        let split = part
            .split_once(':')
            .or_else(|| part.split_once(" — "))
            .or_else(|| part.split_once(" – "));
        match split {
            Some((label, value)) => {
                label.split_whitespace().count() <= 3
                    && !label.contains(CLAUSE_MARKS)
                    && nil_words(value)
            }
            None => false,
        }
    };
    let first = parts[0];
    (nil_words(first) || (fresh(first) && parts[1..].iter().any(|p| nil_words(p))))
        && parts[1..]
            .iter()
            .all(|part| labelled(part) || nil_words(part) || fresh(part))
}

fn nil_aside(item: &str) -> Option<(&str, &str)> {
    let t = item.trim().trim_end_matches(['.', ';', '!', ' ']);
    t.strip_suffix(')')?.split_once('(')
}

fn placed_count(inner: &str) -> bool {
    let words: Vec<&str> = inner
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    matches!(words.as_slice(), [n, rest @ ..]
        if (count(n).is_some_and(|c| c > 0) || matches!(*n, "some" | "few"))
            && rest.first().is_none_or(|w| ELSEWHERE.contains(w) || CHANGE_NOUNS.contains(w)))
}

const ELSEWHERE: &[&str] = &[
    "upstream",
    "elsewhere",
    "on",
    "in",
    "open",
    "pending",
    "left",
    "remaining",
    "outstanding",
    "unmerged",
    "unpushed",
    "of",
    "more",
    "still",
    "here",
    "there",
];

fn aside_status(item: &str, families: &[LandFamily], said_main: bool) -> Option<&'static str> {
    let (nil, inner) = nil_aside(item)?;
    if !nil_list(nil) {
        return None;
    }
    if placed_count(inner) {
        return Some(COUNT_BESIDE_NONE);
    }
    land_status(inner, inner.len(), "", families, said_main)
}

fn behind(before: &[&str]) -> bool {
    before.iter().enumerate().any(|(r, w)| {
        matches!(*w, "where" | "which" | "that" | "when" | "whereby")
            && before[r.saturating_sub(4)..r]
                .iter()
                .any(|v| FIXERS.contains(v))
            && !before[r + 1..].iter().any(|v| PRESENT.contains(v))
    })
}

const PRESENT: &[&str] = &[
    "is", "are", "am", "has", "have", "remains", "remain", "stays", "stay", "will", "can", "does",
    "do",
];

const FIXERS: &[&str] = &[
    "fixed",
    "resolved",
    "removed",
    "addressed",
    "closed",
    "eliminated",
    "dropped",
    "deleted",
    "retired",
    "replaced",
    "reverted",
    "rewrote",
    "rewritten",
    "solved",
];

const AVOIDED: &[&str] = &[
    "change",
    "changed",
    "touch",
    "touched",
    "break",
    "broken",
    "broke",
    "alter",
    "altered",
    "modify",
    "modified",
    "affect",
    "affected",
    "need",
    "needed",
    "regress",
    "regressed",
    "fail",
    "failed",
    "introduce",
    "introduced",
    "start",
    "started",
    "expand",
    "expanded",
    "widen",
    "widened",
    "force-push",
    "force-pushed",
    "delete",
    "deleted",
    "leave",
    "left",
    "find",
    "found",
    "see",
    "seen",
    "hit",
    "notice",
    "noticed",
    "add",
    "added",
];

const STEADY: &[&str] = &[
    "move",
    "moved",
    "budge",
    "budged",
    "shift",
    "shifted",
    "grow",
    "grew",
    "degrade",
    "degraded",
    "crash",
    "crashed",
    "hang",
    "hung",
    "leak",
    "leaked",
    "recur",
    "recurred",
    "reappear",
    "reappeared",
];

const WORKS: &[&str] = &[
    "compile", "compiles", "build", "builds", "work", "works", "pass", "passes", "run", "runs",
    "parse", "parses", "load", "loads", "start", "starts", "handle", "handles", "cover", "covers",
    "support", "supports", "render", "renders", "apply", "applies", "match", "matches", "link",
    "links", "install", "installs", "launch", "launches",
];

const COMPARATIVES: &[&str] = &[
    "better", "cleaner", "faster", "simpler", "nicer", "tidier", "smaller", "safer", "clearer",
];

const OWED_WITHOUT: &[&str] = &[
    "docs",
    "doc",
    "documentation",
    "changelog",
    "readme",
    "tag",
    "tags",
    "tests",
    "test",
    "coverage",
    "release",
    "notes",
    "migration",
    "port",
    "backport",
    "updating",
    "adding",
    "writing",
    "tagging",
    "porting",
    "testing",
    "publishing",
    "pushing",
    "merging",
    "releasing",
    "documenting",
    "running",
    "rerunning",
    "verifying",
    "reviewing",
    "signing",
    "bumping",
];

const APPROVED_THINGS: &[&str] = &[
    "change",
    "changes",
    "fix",
    "fixes",
    "patch",
    "patches",
    "work",
    "feature",
    "features",
    "refactor",
    "rewrite",
    "migration",
    "port",
    "docs",
    "update",
    "updates",
    "diff",
    "commit",
    "commits",
    "implementation",
];

const PARTIAL_TERMS: &[&str] = &[
    "index",
    "indexes",
    "indices",
    "application",
    "function",
    "functions",
    "order",
    "ordering",
    "match",
    "matches",
    "matching",
    "sum",
    "sums",
    "eq",
    "ord",
    "view",
    "views",
    "mock",
    "mocks",
    "template",
    "templates",
    "specialization",
    "key",
    "keys",
    "derivative",
    "evaluation",
    "line",
    "lines",
];

const TESTED: &[&str] = &[
    "tested",
    "ran",
    "checked",
    "verified",
    "benchmarked",
    "measured",
    "built",
    "profiled",
];

const COSTS: &[&str] = &[
    "it",
    "ci",
    "the",
    "took",
    "a",
    "an",
    "few",
    "couple",
    "of",
    "several",
    "runs",
    "run",
    "tries",
    "try",
    "attempts",
    "retries",
    "reruns",
    "rounds",
    "iterations",
    "while",
    "longer",
    "than",
    "expected",
    "hours",
    "hour",
    "minutes",
    "days",
    "to",
    "go",
    "get",
    "turn",
    "green",
    "pass",
    "lane",
    "job",
    "build",
    "builds",
    "pipeline",
];

const LANDED: &[&str] = &[
    "pushed",
    "committed",
    "main",
    "origin/main",
    "upstream/main",
    "shipped",
    "already",
];

fn conceding(before: &str, after: &str) -> bool {
    let claims = |text: &str| {
        text.split_whitespace()
            .map(bare)
            .any(|w| SETTLED.contains(&w) || LANDED.contains(&w))
    };
    let words: Vec<&str> = after
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let cost = words.contains(&"took")
        && words
            .iter()
            .all(|w| COSTS.contains(w) || count(w).is_some_and(|n| n > 0));
    claims(before) && !claims(after) && !words.iter().any(|w| negation(w)) && !cost
}

const CONTROLS: &[&str] = &[
    "without", "if", "when", "unless", "whenever", "against", "before",
];

const OLD: &[&str] = &[
    "old", "previous", "former", "outdated", "stale", "earlier", "last",
];

const STILL_GOOD: &[&str] = &[
    "passes",
    "pass",
    "passed",
    "passing",
    "works",
    "work",
    "working",
    "builds",
    "build",
    "compiles",
    "compile",
    "parses",
    "parse",
    "holds",
    "hold",
    "green",
    "merged",
    "landed",
    "pushed",
    "on",
    "clean",
    "runs",
    "run",
    "fine",
    "correct",
    "valid",
    "intact",
    "supported",
    "matches",
    "match",
    "reads",
    "read",
    "fits",
    "fit",
    "applies",
    "apply",
    "opens",
    "starts",
    "loads",
    "renders",
    "behaves",
    "succeeds",
    "exits",
    "returns",
    "accepts",
    "handles",
    "accepted",
    "compatible",
    "backward",
    "backwards",
    "available",
    "honoured",
    "honored",
    "respected",
    "recognised",
    "recognized",
];

fn off_main(after: &str) -> bool {
    let Some(name) = after.strip_prefix('/') else {
        return false;
    };
    let token = name
        .split(|c: char| !(word_char(c) || c == '/' || c == '.'))
        .next()
        .unwrap_or("");
    if let Some((left, _)) = token.split_once("..") {
        return matches!(left, "main" | "master") && !lists_none(&name[token.len()..]);
    }
    let name = token.trim_end_matches('.');
    !name.is_empty() && !matches!(name, "main" | "master" | "head")
}

fn lists_none(rest: &str) -> bool {
    rest[..clause_end(rest, 0)]
        .split_whitespace()
        .map(bare)
        .any(|w| matches!(w, "nothing" | "empty" | "none" | "no" | "0" | "zero"))
}

const REMAINDER: &[&str] = &[
    "the other",
    "the others",
    "the rest",
    "the remaining",
    "the last",
    "the second",
    "the third",
    "the fourth",
    "the fifth",
];

fn rest_settled(after: &str) -> bool {
    REMAINDER.iter().any(|r| {
        after.find(r).is_some_and(|at| {
            after[at + r.len()..]
                .split_whitespace()
                .map(bare)
                .any(|w| SETTLED.contains(&w) || LANDED.contains(&w))
        })
    })
}

fn count(w: &str) -> Option<u32> {
    const WORDS: [&str; 12] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    w.parse().ok().or_else(|| {
        WORDS
            .iter()
            .position(|n| *n == w)
            .and_then(|i| u32::try_from(i + 1).ok())
    })
}

fn short_count(before: &[&str], after: &str) -> bool {
    let mut words = after
        .split_whitespace()
        .map(bare)
        .skip_while(|w| *w == "the");
    let prev = match before {
        [.., "every" | "each", "one"] => None,
        [.., n, "out"] => Some(*n),
        [.., n] => Some(*n),
        [] => None,
    };
    let (Some(n), Some(m)) = (prev.and_then(count), words.next().and_then(count)) else {
        return false;
    };
    n < m
}

const PART_DONE: &[&str] = &[
    "done",
    "converted",
    "ported",
    "migrated",
    "implemented",
    "finished",
    "complete",
    "completed",
    "wired",
    "covered",
    "handled",
    "supported",
    "fixed",
    "merged",
    "landed",
    "written",
];

fn leading_part(before: &[&str], after: &str) -> bool {
    let Some((whole, part)) = after.split_once(',') else {
        return false;
    };
    let whole = whole
        .split_whitespace()
        .map(bare)
        .find(|w| *w != "the" && *w != "all")
        .and_then(count);
    let part: Vec<&str> = part.split_whitespace().map(bare).collect();
    before.is_empty()
        && whole
            .zip(part.first().and_then(|w| count(w)))
            .is_some_and(|(m, n)| n < m)
        && only_part(&part[1..])
}

fn only_part(after: &[&str]) -> bool {
    after
        .windows(2)
        .take(8)
        .any(|w| AUX.contains(&w[0]) && PART_DONE.contains(&w[1]))
}

const CLAUSE_MARKS: [char; 7] = [',', ';', ':', '(', ')', '—', '–'];

fn clause_mark(s: &str, i: usize, c: char) -> bool {
    let (before, after) = (&s[..i], &s[i + c.len_utf8()..]);
    CLAUSE_MARKS.contains(&c)
        && !(c == ','
            && before.ends_with(|d: char| d.is_ascii_digit())
            && after.starts_with(|d: char| d.is_ascii_digit()))
        && !(c == ':' && (before.ends_with(':') || after.starts_with(':')))
}

fn clause_start(s: &str, at: usize) -> usize {
    s[..at]
        .char_indices()
        .rev()
        .find(|&(i, c)| clause_mark(s, i, c))
        .map_or(0, |(i, c)| i + c.len_utf8())
}

fn clause_end(s: &str, from: usize) -> usize {
    s[from..]
        .char_indices()
        .find(|&(i, c)| clause_mark(s, from + i, c))
        .map_or(s.len(), |(i, _)| from + i)
}

fn bare(w: &str) -> &str {
    w.trim_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '\'' | '-' | '_' | '/')))
}

fn words_at(s: &str, a: usize, b: usize) -> impl Iterator<Item = (usize, &str)> {
    let mut at = a;
    s[a..b]
        .split(' ')
        .map(move |w| {
            let p = at;
            at += w.len() + 1;
            (p, w)
        })
        .filter(|(_, w)| !w.is_empty())
}

fn in_path(s: &str, at: usize, end: usize) -> bool {
    s[..at].ends_with(['/', '.'])
        || s[end..]
            .strip_prefix('.')
            .is_some_and(|r| r.starts_with(char::is_alphanumeric))
}

const PAST_MARKERS: &[&str] = &[
    "previously",
    "formerly",
    "once",
    "earlier",
    "initially",
    "originally",
];

fn described(before: &[&str]) -> bool {
    let back = |k: usize| {
        before
            .len()
            .checked_sub(k)
            .and_then(|i| before.get(i))
            .copied()
    };
    let relative = |k: usize| back(k).is_some_and(|w| matches!(w, "that" | "which" | "who"));
    back(1).is_some_and(|w| PAST_MARKERS.contains(&w))
        || (matches!(back(1), Some("was" | "were" | "had")) && relative(2))
        || (matches!(back(1), Some("was" | "were")) && back(2) == Some("said"))
        || (back(1) == Some("been") && back(2) == Some("had") && relative(3))
}

const DETERMINERS: &[&str] = &[
    "the", "a", "an", "this", "that", "these", "those", "each", "every", "all", "both", "its",
    "my", "our", "their", "his", "her", "your", "any", "some", "one", "two", "three", "four",
    "five", "six", "seven", "eight", "nine", "ten",
];

const UNSETTLED: &[&str] = &[
    "to", "be", "will", "would", "can", "could", "should", "must", "may", "might", "shall",
    "needs", "need", "not",
];

const PAST_AUX: &[&str] = &["was", "were", "got", "had", "been"];

const CONDITIONS: &[&str] = &["once", "when", "if", "unless", "until"];

const AUX: &[&str] = &[
    "is", "was", "are", "were", "has", "have", "had", "been", "got", "now", "then", "since",
    "also", "finally", "already", "just", "all", "both",
];

const CUTS: &[&str] = &[
    "and", "but", "while", "whereas", "though", "although", "so", "yet", "until", "unless",
    "before", "because",
];

fn stretch(s: &str, from: usize, to: usize, span: (usize, usize), by: &[&str]) -> (usize, usize) {
    let (mut a, mut b) = (from, to);
    for (p, w) in words_at(s, from, to) {
        if !CUTS.contains(&bare(w)) {
            continue;
        }
        let past = p + w.len();
        if past <= span.0 {
            a = past;
        } else if p >= span.1 && !continues(&s[past..to], by) {
            b = p;
            break;
        }
    }
    (a, b)
}

fn continues(rest: &str, by: &[&str]) -> bool {
    let words: Vec<&str> = rest.split_whitespace().map(bare).collect();
    settles_at(&words, by, true)
}

const LANDING_VERBS: &[&str] = &[
    "pushed",
    "committed",
    "landed",
    "merged",
    "shipped",
    "squash-merged",
    "rebase-merged",
];

fn settles_at(words: &[&str], by: &[&str], place: bool) -> bool {
    let rest: Vec<&str> = words
        .iter()
        .copied()
        .skip_while(|w| AUX.contains(w))
        .collect();
    rest.first().is_some_and(|w| by.contains(w))
        || (place
            && (onto_main(&rest)
                || (rest.first().is_some_and(|w| {
                    LANDING_VERBS.contains(w) || matches!(*w, "lives" | "sits" | "went")
                }) && onto_main(&rest[1..]))
                || (rest.first().is_some_and(|w| {
                    LANDING_VERBS.contains(w) || matches!(*w, "took" | "brought" | "carried")
                }) && rest.get(1).is_some_and(|o| matches!(*o, "it" | "them"))
                    && onto_main(&rest[2..]))))
}

fn onto_main(words: &[&str]) -> bool {
    let Some((&first, rest)) = words.split_first() else {
        return false;
    };
    let lead = rest
        .iter()
        .take_while(|w| matches!(**w, "the" | "remote"))
        .count();
    let remote = rest[..lead].contains(&"remote");
    let mut names = rest[lead..].iter().copied();
    let remote_main = |n: &str| {
        matches!(
            n,
            "origin/main" | "origin/master" | "upstream/main" | "upstream/master"
        )
    };
    match (first, names.next()) {
        ("on" | "to" | "into" | "onto" | "in", Some(n)) => {
            let repo = n.split_once('/').is_some_and(|(owner, name)| {
                !owner.is_empty() && !name.is_empty() && !name.contains(['/', '.'])
            });
            remote_main(n)
                || matches!(n, "main" | "master" | "trunk")
                || ((matches!(n, "origin" | "upstream") || repo)
                    && names.next().is_some_and(|m| matches!(m, "main" | "master")))
        }
        ("matches" | "match" | "equals" | "at" | "with", Some(n)) => {
            remote_main(n) || (remote && matches!(n, "main" | "master"))
        }
        _ => false,
    }
}

const HOLDS: &[&str] = &["has", "contains", "includes", "carries", "got", "holds"];

fn main_claimed(s: &str) -> bool {
    let words: Vec<&str> = s.split_whitespace().map(bare).collect();
    (0..words.len()).any(|i| {
        let undenied = i
            .checked_sub(1)
            .and_then(|j| words.get(j))
            .is_none_or(|w| *w != "not" && !negation(w));
        let carried = || {
            let prev = i.checked_sub(1).and_then(|j| words.get(j)).copied();
            let back = &words[i.saturating_sub(3)..i];
            if back
                .iter()
                .any(|w| UNSETTLED.contains(w) || CHANGE_NOUNS.contains(w) || *w == "date")
            {
                return false;
            }
            prev.is_some_and(|p| AUX.contains(&p) && !matches!(p, "all" | "both"))
                || back
                    .iter()
                    .rev()
                    .take_while(|w| LANDING_VERBS.contains(w) || !verbish(w))
                    .any(|w| LANDING_VERBS.contains(w) || *w == "went")
        };
        undenied
            && ((onto_main(&words[i..]) && carried())
                || (matches!(
                    words[i],
                    "main" | "origin/main" | "upstream/main" | "master"
                ) && words.get(i + 1).is_some_and(|w| HOLDS.contains(w))))
    })
}

const REMOTES: &[&str] = &[
    "the", "remote", "origin", "upstream", "github", "gitlab", "our",
];

const MAINS: &[&str] = &[
    "main",
    "master",
    "trunk",
    "head",
    "default",
    "origin/main",
    "origin/master",
    "upstream/main",
    "upstream/master",
    "origin/head",
];

const BRANCHES: &[&str] = &[
    "develop",
    "development",
    "dev",
    "staging",
    "next",
    "beta",
    "stable",
    "release",
    "integration",
];

fn onto_branch(after: &[&str]) -> bool {
    let Some(k) = after
        .iter()
        .take(6)
        .position(|w| matches!(*w, "to" | "into" | "onto"))
    else {
        return false;
    };
    let mut names = after[k + 1..]
        .iter()
        .copied()
        .skip_while(|w| REMOTES.contains(w));
    let Some(name) = names.next() else {
        return false;
    };
    let then = names.next();
    let file = name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphabetic()));
    if MAINS.contains(&name)
        || DETERMINERS.contains(&name)
        || count(name).is_some()
        || file
        || !name.starts_with(char::is_alphabetic)
    {
        return false;
    }
    if then.is_some_and(|w| matches!(w, "main" | "master")) {
        // An integration branch's own main (`pushed to the staging main`)
        // is off main; a repository's is main (`pushed to
        // example-org/codex-alab main`).
        return BRANCHES.contains(&name);
    }
    then == Some("branch")
        || name.contains(['/', '-', '_'])
        || main_grown(name)
        || (BRANCHES.contains(&name) && then.is_none_or(|w| JOINS.contains(&w)))
}

fn main_grown(name: &str) -> bool {
    ["main", "master"].iter().any(|m| {
        name.strip_prefix(m).is_some_and(|rest| {
            !rest.is_empty()
                && !matches!(rest, "ly" | "s")
                && !rest.starts_with("tain")
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
    })
}

const DISPOSABLE: &[&str] = &[
    "workaround",
    "workarounds",
    "hack",
    "hacks",
    "stopgap",
    "stopgaps",
    "stop-gap",
    "band-aid",
    "shim",
    "shims",
    "kludge",
    "approach",
    "attempt",
    "attempts",
    "idea",
    "experiment",
    "experiments",
    "prototype",
    "spike",
    "noise",
    "churn",
    "hotfix",
    "hotfixes",
    "copy",
    "copies",
    "duplicate",
    "duplicates",
];

const DISPOSABLE_MODS: &[&str] = &[
    "temporary",
    "interim",
    "hacky",
    "old",
    "outdated",
    "stale",
    "dead",
    "unused",
    "obsolete",
    "legacy",
    "deprecated",
    "redundant",
    "duplicate",
    "duplicated",
    "bad",
    "broken",
    "faulty",
    "buggy",
    "offending",
    "wrong",
    "debug",
    "accidental",
    "unrelated",
    "stray",
    "unintended",
    "bogus",
    "spurious",
    "vendored",
];

const CUT_ROWS: &[&str] = &["was cut", "were cut", "got cut", "been cut", "cut from"];

const CUT_THINGS: &[&str] = &[
    "release",
    "releases",
    "tag",
    "tags",
    "rc",
    "branch",
    "version",
    "build",
    "candidate",
];

fn disposable(word: &str, before: &[&str], after: &[&str]) -> bool {
    if after.iter().take(4).any(|w| {
        matches!(
            *w,
            "relanded" | "re-landed" | "remerged" | "re-merged" | "reapplied" | "re-applied"
        )
    }) {
        return true;
    }
    let passive = word.split(' ').next().is_some_and(|w| AUX.contains(&w))
        || CUT_ROWS.contains(&word)
        || before
            .last()
            .is_some_and(|w| matches!(*w, "was" | "were" | "is" | "are" | "got" | "been" | "be"));
    let np: Vec<&str> = if passive {
        let subject = before
            .iter()
            .rposition(|w| verbish(w) && !AUX.contains(w))
            .map_or(before, |i| &before[i + 1..]);
        subject
            .iter()
            .copied()
            .filter(|w| !AUX.contains(w))
            .collect()
    } else {
        after
            .iter()
            .copied()
            .take_while(|w| !PREPOSITIONS.contains(w) && !CUTS.contains(w) || *w == "and")
            .collect()
    };
    let mut things: Vec<Vec<&str>> = vec![Vec::new()];
    for w in np {
        if matches!(w, "and" | "or" | "plus") {
            things.push(Vec::new());
        } else if let Some(thing) = things.last_mut() {
            thing.push(w);
        }
    }
    let things: Vec<Vec<&str>> = things
        .into_iter()
        .map(|t| {
            t.into_iter()
                .take_while(|w| !PREPOSITIONS.contains(w) && *w != "of")
                .filter(|w| !DETERMINERS.contains(w) && count(w).is_none())
                .collect::<Vec<&str>>()
        })
        .filter(|t| !t.is_empty())
        .take_while(|t| {
            !t.first()
                .is_some_and(|w| verbish(w) && !DISPOSABLE_MODS.contains(w))
        })
        .collect();
    !things.is_empty()
        && things.iter().all(|t| {
            let head = t[t.len() - 1];
            let modifier = t.len().checked_sub(2).map(|i| t[i]);
            DISPOSABLE.contains(&head)
                || modifier.is_some_and(|m| DISPOSABLE_MODS.contains(&m))
                || (CUT_ROWS.contains(&word) && CUT_THINGS.contains(&head))
        })
}

const BRANCH_PREFIXES: &[&str] = &[
    "fix", "feat", "feature", "features", "bugfix", "hotfix", "release", "releases", "chore",
    "refactor", "wip", "topic", "exp", "dev", "docs", "test", "tmp",
];

fn branch_name(name: &str) -> bool {
    let Some((prefix, rest)) = name.split_once('/') else {
        return false;
    };
    let code = prefix.len() <= 3
        && prefix.starts_with(char::is_alphabetic)
        && prefix.bytes().skip(1).all(|b| b.is_ascii_digit());
    let round = (1..=3).contains(&prefix.len())
        && prefix.bytes().all(|b| b.is_ascii_lowercase())
        && rest.starts_with(|c: char| c.is_ascii_lowercase())
        && rest.bytes().any(|b| b.is_ascii_digit())
        && rest.bytes().all(|b| b.is_ascii_alphanumeric());
    !rest.is_empty()
        && !name.contains("://")
        && (BRANCH_PREFIXES.contains(&prefix)
            || code
            || round
            || rest.contains(['-', '_'])
            || matches!(prefix, "origin" | "upstream"))
}

fn on_branch(after: &[&str], landed: bool) -> bool {
    let mut names = after.iter().copied().skip_while(|w| REMOTES.contains(w));
    let Some(name) = names.next() else {
        return false;
    };
    let name = name.strip_suffix("'s").unwrap_or(name);
    let then = names.next();
    let file = name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphabetic()));
    if name == "branch" {
        return then.is_some_and(|n| {
            branch_name(n) || BRANCHES.contains(&n) || (n.contains(['-', '_']) && !n.contains('.'))
        });
    }
    if name.split_once('/').is_some_and(|(remote, m)| {
        MAINS.contains(&m) && !REMOTES.contains(&remote) && !MAINS.contains(&name)
    }) {
        return true;
    }
    if landed
        && !file
        && !MAINS.contains(&name)
        && name.starts_with(char::is_alphabetic)
        && (main_grown(name)
            || (then.is_some_and(|w| matches!(w, "main" | "master")) && BRANCHES.contains(&name))
            || ((BRANCHES.contains(&name) || name.contains(['-', '_']))
                && then.is_none_or(|w| JOINS.contains(&w))))
    {
        return true;
    }
    if MAINS.contains(&name)
        || DETERMINERS.contains(&name)
        || count(name).is_some()
        || file
        || !name.starts_with(char::is_alphabetic)
        || name
            .split_once('/')
            .is_some_and(|(_, m)| MAINS.contains(&m))
    {
        return false;
    }
    then == Some("branch")
        || branch_name(name)
        || (matches!(name, "develop" | "development" | "integration")
            && then.is_none_or(|w| JOINS.contains(&w)))
}

const CODE_PARTS: &[&str] = &[
    "command",
    "commands",
    "subcommand",
    "flag",
    "flags",
    "option",
    "options",
    "api",
    "module",
    "function",
    "method",
    "code",
    "logic",
    "parser",
    "handler",
    "helper",
    "implementation",
    "type",
    "struct",
    "enum",
    "crate",
    "help",
];

const CHANGE_PARTS: &[&str] = &[
    "description",
    "title",
    "body",
    "template",
    "comment",
    "comments",
    "thread",
    "threads",
    "link",
    "url",
    "number",
    "author",
    "label",
    "labels",
    "summary",
    "text",
];

fn unplaced(s: &str, (prior, said_main): (&str, bool), before: &[&str], after: &[&str]) -> bool {
    let object = &after[..after.len().min(4)];
    let a_branch =
        |w: &&str| branch_name(w) && !w.split_once('/').is_some_and(|(_, m)| MAINS.contains(&m));
    if (object.iter().any(|w| matches!(*w, "branch" | "branches"))
        && !object.iter().any(|w| MAINS.contains(w)))
        || before.iter().any(a_branch)
    {
        return !main_claimed(s);
    }
    let placed = before.iter().any(|w| MAINS.contains(w))
        || after
            .iter()
            .find(|w| !matches!(**w, "the" | "it" | "them"))
            .is_some_and(|w| MAINS.contains(w))
        || (0..after.len()).any(|k| {
            onto_main(&after[k..])
                && !after[..k]
                    .iter()
                    .any(|w| CHANGE_NOUNS.contains(w) || UNSETTLED.contains(w))
        });
    let tagged = before
        .iter()
        .chain(after)
        .any(|w| matches!(*w, "tag" | "tags") || version(w))
        || s.split_whitespace().next().map(bare).is_some_and(version);
    !placed
        && !tagged
        && !said_main
        && ![s, prior]
            .iter()
            .any(|t| landing_claimed(t) || main_named(t))
}

fn version(w: &str) -> bool {
    let n = w.strip_prefix('v').unwrap_or(w);
    n.starts_with(|c: char| c.is_ascii_digit())
        && n.contains('.')
        && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn main_named(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().map(bare).collect();
    (0..words.len()).any(|i| {
        let back = &words[i.saturating_sub(3)..i];
        !back.iter().any(|w| {
            negation(w)
                || matches!(
                    *w,
                    "not"
                        | "never"
                        | "without"
                        | "before"
                        | "nearly"
                        | "almost"
                        | "rebased"
                        | "rebasing"
                        | "rebase"
                        | "based"
                        | "pr"
                        | "prs"
                        | "mr"
                        | "request"
                        | "requests"
                        | "be"
                        | "to"
                        | "date"
                        | "sync"
                        | "against"
                        | "targeting"
                        | "targets"
                        | "opened"
                        | "will"
                )
        }) && (onto_main(&words[i..])
            || (matches!(words[i], "fast-forwarded" | "ff-merged")
                && words.get(i + 1).is_some_and(|m| MAINS.contains(m))))
    })
}

fn change_open(
    s: &str,
    (from, to): (usize, usize),
    st: &Stand<'_>,
    before: &[&str],
    after: &[&str],
) -> bool {
    let lead = s[..from].trim_end();
    if matches!(
        st.word,
        "the second one" | "the other one" | "the second" | "the other"
    ) {
        let named = s[..st.at]
            .split_whitespace()
            .map(bare)
            .any(|w| CHANGE_NOUNS[..4].contains(&w) || w.starts_with('#'))
            || s[..st.at].contains("pull request");
        if !named {
            return false;
        }
    }
    let included = after.len() <= 2 && after.last() == Some(&"included") && landing_claimed(lead);
    let bracketed = lead.ends_with('(')
        && s[to..].strip_prefix(')').is_none_or(|r| {
            r.split_whitespace()
                .map(bare)
                .find(|w| !w.is_empty())
                .is_none_or(|w| !verbish(w))
        });
    if included
        || bracketed
        || (lead.ends_with(',')
            && before.is_empty()
            && after.is_empty()
            && !s[to..].starts_with(','))
        || after.first().is_some_and(|w| CHANGE_PARTS.contains(w))
    {
        return false;
    }
    let names = |words: &[&str]| {
        let landing = words
            .iter()
            .rposition(|w| verbish(w) || CUTS.contains(w))
            .is_some_and(|k| {
                (SHIPS.contains(&words[k])
                    || LANDING_VERBS.contains(&words[k])
                    || words[k] == "rebase-merged")
                    && !words[..k]
                        .iter()
                        .rev()
                        .take(2)
                        .any(|w| UNSETTLED.contains(w) || negation(w))
            });
        landing
            || words.windows(2).any(|w| {
                matches!(w[1], "as" | "in")
                    && (SHIPS.contains(&w[0]) || LANDING_VERBS.contains(&w[0]))
            })
            || words.iter().enumerate().any(|(i, w)| {
                !words[i + 1..]
                    .iter()
                    .any(|x| verbish(x) || CUTS.contains(x))
                    && matches!(
                        *w,
                        "no" | "none"
                            | "zero"
                            | "0"
                            | "via"
                            | "see"
                            | "per"
                            | "fixes"
                            | "closes"
                            | "resolves"
                            | "refs"
                            | "addresses"
                            | "supersedes"
                            | "fix"
                            | "close"
                            | "resolve"
                    )
            })
    };
    let closed = st.word == "#n"
        && before
            .iter()
            .rposition(|w| verbish(w))
            .is_some_and(|k| matches!(before[k], "closed" | "fixed" | "resolved" | "addressed"));
    let same = before.last().is_some_and(|w| *w == "same");
    let run = st.word == "#n"
        && before
            .last()
            .is_some_and(|w| matches!(*w, "run" | "build" | "job" | "pipeline" | "workflow"));
    if run {
        return false;
    }
    let record = matches!(before, [.., "in" | "on", "the"])
        && before.iter().any(|w| {
            matches!(
                *w,
                "numbers"
                    | "results"
                    | "details"
                    | "notes"
                    | "logs"
                    | "benchmarks"
                    | "measurements"
                    | "figures"
                    | "writeup"
                    | "write-up"
                    | "rationale"
                    | "discussion"
                    | "context"
            )
        });
    if closed || same || record || names(before) {
        return false;
    }
    let mut f = from;
    while !after.iter().any(|w| verbish(w)) {
        let lead = s[..f].trim_end();
        let Some(comma) = lead.strip_suffix(',').map(str::len) else {
            break;
        };
        let start = clause_start(s, comma);
        let words: Vec<&str> = s[start..comma].split_whitespace().map(bare).collect();
        if names(&words) {
            return false;
        }
        f = start;
    }
    if !after.iter().any(|w| verbish(w)) && list_landed(s, to) {
        return false;
    }
    let (a, b) = stretch(s, from, to, (st.at, st.end()), SHIPS);
    let words: Vec<&str> = s[a..b]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let shared = before.iter().all(list_item) && {
        let v = after.iter().take_while(|w| list_item(w)).count();
        after[..v].contains(&"and") && settles_with(&after[v..], SHIPS, true)
    };
    !shared && !lands_in(&words) && !landed_again(s, st, b)
}

fn landed_again(s: &str, st: &Stand<'_>, to: usize) -> bool {
    let name = &s[st.at..st.end()];
    if st.word != "#n" || !name.starts_with('#') {
        return false;
    }
    let mut from = st.end();
    while let Some(k) = s[from..].find(name).map(|k| from + k) {
        let end = k + name.len();
        from = end;
        if s[end..].starts_with(|c: char| c.is_alphanumeric() || c == '\'' || c == '’') {
            continue;
        }
        let (a, b) = stretch(s, clause_start(s, k), clause_end(s, end), (k, end), SHIPS);
        let words: Vec<&str> = s[a..b]
            .split_whitespace()
            .map(bare)
            .filter(|w| !w.is_empty())
            .collect();
        if k >= to && lands_in(&words) {
            return true;
        }
    }
    false
}

fn list_item(w: &&str) -> bool {
    w.starts_with('#')
        || w.bytes().all(|b| b.is_ascii_digit())
        || matches!(
            *w,
            "and"
                | "or"
                | "the"
                | "its"
                | "pr"
                | "prs"
                | "mr"
                | "mrs"
                | "pull"
                | "request"
                | "requests"
                | "follow-up"
                | "both"
                | "all"
        )
}

fn lands_in(words: &[&str]) -> bool {
    (0..words.len()).any(|k| {
        let w = words[k];
        let landing =
            SHIPS.contains(&w) || w == "rebase-merged" || settles_at(&words[k..], &[], true);
        landing
            && !words[..k]
                .iter()
                .rev()
                .take(3)
                .any(|x| UNSETTLED.contains(x) || negation(x) || *x == "without")
            && !words[k..].iter().any(|x| CONDITIONS.contains(x))
    })
}

fn list_landed(s: &str, mut to: usize) -> bool {
    let item = list_item;
    while s[to..].starts_with([',', '(', ')', ':']) {
        let start = to + 1;
        let end = clause_end(s, start);
        let words: Vec<&str> = s[start..end]
            .split_whitespace()
            .map(bare)
            .filter(|w| !w.is_empty())
            .collect();
        match words.iter().position(|w| starts_predicate(w, SHIPS)) {
            Some(v) => {
                return words[..v].iter().all(item) && settles_with(&words[v..], SHIPS, true);
            }
            None if words.iter().all(item) => to = end,
            None => return false,
        }
    }
    false
}

fn below_hundred(n: &str) -> bool {
    const TENS: [&str; 9] = [
        "ten", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    let decimal = !n.is_empty()
        && n.starts_with(|c: char| c.is_ascii_digit())
        && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && n.parse::<f64>().is_ok_and(|v| v < 100.0);
    let spelled = !n.starts_with(|c: char| c.is_ascii_digit()) && count(n).is_some();
    decimal
        || spelled
        || TENS.contains(&n)
        || n.split_once('-')
            .is_some_and(|(t, u)| TENS.contains(&t) && count(u).is_some())
}

const WORK_LABELS: &[&str] = &[
    "docs",
    "documentation",
    "tests",
    "migration",
    "port",
    "porting",
    "conversion",
    "progress",
    "done",
    "completion",
    "implementation",
    "rewrite",
    "refactor",
    "work",
    "readers",
    "lanes",
    "status",
];

fn percent_part(after: &str) -> bool {
    after
        .split_whitespace()
        .map(bare)
        .find(|w| !w.is_empty())
        .is_some_and(|w| {
            w == "of"
                || SETTLED.contains(&w)
                || LANDED.contains(&w)
                || matches!(
                    w,
                    "coverage" | "covered" | "migrated" | "ported" | "tested" | "there"
                )
        })
}

const JOINS: &[&str] = &[
    "and", "but", "then", "only", "too", "first", "so", "while", "now", "with", "for", "on",
];

fn settled(s: &str, from: usize, to: usize, st: &Stand<'_>) -> bool {
    if ["not ", "never ", "did not ", "could", "can", "unable"]
        .iter()
        .any(|p| st.word.starts_with(p))
    {
        // A lack negates its own clause: `not all of the comments were
        // addressed` settles nothing.
        return false;
    }
    let (at, end) = (st.at, st.end());
    let mut back: Vec<&str> = s[from..at]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    if let Some(k) = back.iter().rposition(|w| *w == "that")
        && k > 0
        && CONFIRMED.contains(&back[k - 1])
        && !back[..k]
            .iter()
            .any(|w| negation(w) || UNSETTLED.contains(w))
    {
        back.drain(..=k);
    }
    let whole: Vec<&str> = s[end..to]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let np_end = whole
        .iter()
        .position(|w| starts_predicate(w, &[]))
        .unwrap_or(whole.len());
    let np: Vec<&str> = back.iter().chain(&whole[..np_end]).copied().collect();
    let by = settlers(st, &np);
    if by.is_empty() {
        return false;
    }
    let place = st.lands_by_place()
        || CHANGE_NOUNS.contains(&st.word)
        || np.iter().any(|w| CHANGE_NOUNS.contains(w));
    if let Some(v) = object_verb(&back, st) {
        let did = worker_did(&back[..v]) && !whole.iter().any(|w| UNDOES.contains(w));
        if did
            && (by.contains(&back[v])
                || (place
                    && GONE.contains(&back[v])
                    && (landing_claimed(s) || merged_first(&whole))))
        {
            return true;
        }
    }
    if matches!(st.reads(), Reads::Change) && np_end == whole.len() && s[to..].starts_with(':') {
        let item: Vec<&str> = s[to + 1..clause_end(s, to + 1)]
            .split_whitespace()
            .map(bare)
            .filter(|w| !w.is_empty())
            .collect();
        return settles_once(&item, &by, place)
            && !item
                .iter()
                .any(|w| UNSETTLED.contains(w) || negation(w) || *w == "without");
    }
    let (_, b) = stretch(s, from, to, (at, end), &by);
    let mut fwd: Vec<&str> = s[end..b]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    if whole.first() == Some(&"and") {
        let shared = (2..=4).find(|&k| {
            whole
                .get(k)
                .is_some_and(|w| AUX.contains(w) && !matches!(*w, "all" | "both"))
                && whole[1..k].iter().all(|w| !verbish(w))
        });
        if let Some(k) = shared {
            fwd = whole[k..].to_vec();
        }
    }
    if b == to
        && fwd
            .iter()
            .any(|w| AUX.contains(w) && !matches!(*w, "all" | "both"))
    {
        let mut t = to;
        while s[t..].starts_with(',') {
            let next = clause_end(s, t + 1);
            let words: Vec<&str> = s[t + 1..next]
                .split_whitespace()
                .map(bare)
                .filter(|w| !w.is_empty())
                .collect();
            if !words.first().is_some_and(|w| past_form(w))
                || words.iter().any(|w| negation(w) || UNSETTLED.contains(w))
            {
                break;
            }
            fwd.push("and");
            fwd.extend(words);
            t = next;
        }
    }
    let lead = st.word.split(' ').next().unwrap_or("");
    let relative = past_form(lead)
        && !st.word.contains(' ')
        && back
            .iter()
            .position(|w| matches!(*w, "i" | "we" | "you" | "they"))
            .is_some_and(|p| {
                p >= 2
                    && DETERMINERS.contains(&back[0])
                    && !back[1..p].iter().any(|w| verbish(w))
                    && back[p + 1..]
                        .iter()
                        .all(|w| matches!(*w, "had" | "have" | "has"))
            });
    if relative {
        return whole
            .first()
            .is_some_and(|w| AUX.contains(w) && !matches!(*w, "all" | "both"))
            && !whole
                .iter()
                .any(|w| UNSETTLED.contains(w) || negation(w) || *w == "without")
            && settles_with(&whole, &by, place);
    }
    let named =
        !(AUX.contains(&lead) || FINITE.contains(&lead) || lead == "still" || lead == "only")
            && (back.last().is_none_or(|w| DETERMINERS.contains(w)) || !past_form(lead));
    if back.iter().any(|w| verbish(w)) || !named {
        let now = back.iter().any(|w| matches!(*w, "was" | "were"))
            && s[to..].strip_prefix(", now ").is_some_and(|r| {
                let words: Vec<&str> = r[..clause_end(r, 0)]
                    .split_whitespace()
                    .map(bare)
                    .filter(|w| !w.is_empty())
                    .collect();
                settles_once(&words, &by, place)
            });
        return now
            || fwd.iter().enumerate().any(|(i, w)| {
                matches!(*w, "and" | "then") && settles_once(&fwd[i + 1..], &by, place)
            });
    }
    let head = usize::from(
        past_form(lead)
            && back.last().is_some_and(|w| DETERMINERS.contains(w))
            && fwd
                .first()
                .is_some_and(|w| !AUX.contains(w) && !by.contains(w) && !LANDING_VERBS.contains(w)),
    );
    let mut starts = fwd
        .iter()
        .enumerate()
        .skip(head)
        .filter(|(_, w)| starts_predicate(w, &by))
        .map(|(k, _)| k);
    if let ["no", "longer", "exists" | "exist", rest @ ..] = &fwd[head.min(fwd.len())..] {
        return by.contains(&"gone")
            && rest
                .iter()
                .all(|w| matches!(*w, "anymore" | "now" | "at" | "all"));
    }
    let branch = np
        .iter()
        .any(|w| matches!(*w, "branch" | "branches" | "worktree" | "worktrees"));
    let gone = fwd.iter().position(|w| AUX.contains(w)).is_some_and(|v| {
        !fwd[..v].iter().any(|w| verbish(w)) && fwd.get(v + 1).is_some_and(|w| GONE.contains(w))
    });
    if branch && gone && landing_claimed(s) {
        return true;
    }
    let Some(mut k) = starts.next() else {
        return false;
    };
    if fwd
        .first()
        .is_some_and(|w| matches!(*w, "i" | "we" | "you" | "they" | "that" | "which" | "who"))
    {
        let Some(next) = starts.next() else {
            return false;
        };
        k = next;
    }
    k <= 5
        && !fwd[..k]
            .iter()
            .any(|w| UNSETTLED.contains(w) || negation(w) || *w == "without")
        && (settles_with(&fwd[k..], &by, place) || empty_now(&fwd[k..]))
}

const CONFIRMED: &[&str] = &[
    "checked",
    "double-checked",
    "confirmed",
    "verified",
    "ensured",
    "saw",
];

fn empty_now(words: &[&str]) -> bool {
    let skip = words.iter().take_while(|w| AUX.contains(w)).count();
    skip > 0
        && match &words[skip..] {
            ["empty"] => true,
            ["empty", w, ..] => matches!(*w, "and" | "now" | "too" | "again"),
            _ => false,
        }
}

const FINITE: &[&str] = &[
    "is", "are", "am", "was", "were", "has", "have", "had", "remains", "remain", "stays", "stay",
    "will", "would", "can", "could", "should", "must", "may", "might", "does", "do", "needs",
    "need", "requires", "require", "lacks", "wants", "waits", "wait", "awaits",
];

fn settlers(st: &Stand<'_>, np: &[&str]) -> Vec<&'static str> {
    let by = st.settled_by();
    if by.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<&'static str> = if FLAKY.contains(&st.word) {
        DEFLAKED.to_vec()
    } else {
        by.to_vec()
    };
    let change = np.iter().any(|w| CHANGE_NOUNS.contains(w)) || CHANGE_NOUNS.contains(&st.word);
    if st.word == "stray" {
        // A stray thing is junk, a stray commit too: gone is done (`the stray
        // debug print is removed`, `the stray debug commit was dropped`).
        out.extend(["removed", "deleted", "gone", "dropped"]);
    } else if change {
        out = SHIPS.to_vec();
    } else if st.word == "#n"
        || st
            .word
            .split(' ')
            .any(|w| matches!(w, "issue" | "issues" | "ticket" | "tickets"))
    {
        // An issue (`the tracking issue`), or a bare number, which names
        // one as often as a change: its fix or its close settles it (`#45
        // is closed`); a number a change noun names (`PR #45 is closed`) is
        // a change, settled only by landing.
        out.extend(["fixed", "resolved", "closed"]);
    } else {
        if np.iter().any(|w| REVIEW_NOUNS.contains(w)) {
            out.extend(REVIEWED);
        }
        if matches!(st.word, "remaining" | "leftover" | "outstanding" | "paused")
            && np.iter().any(|w| CHECK_NOUNS.contains(w))
        {
            out.extend(CHECKS);
        }
    }
    out
}

fn starts_predicate(w: &str, by: &[&str]) -> bool {
    (AUX.contains(&w) && !matches!(w, "all" | "both"))
        || by.contains(&w)
        || LANDING_VERBS.contains(&w)
        || (verbish(w) && !PREPOSITIONS.contains(&w) && w != "on")
}

fn settles_with(words: &[&str], by: &[&str], place: bool) -> bool {
    settles_once(words, by, place)
        || words
            .iter()
            .enumerate()
            .any(|(i, w)| matches!(*w, "and" | "then") && settles_once(&words[i + 1..], by, place))
}

fn settles_once(words: &[&str], by: &[&str], place: bool) -> bool {
    let skip = words.iter().take_while(|w| AUX.contains(w)).count();
    let past = skip
        .checked_sub(1)
        .and_then(|i| words.get(i))
        .is_some_and(|w| PAST_AUX.contains(w));
    settles_at(words, by, place) && (past || !words[skip..].iter().any(|w| CONDITIONS.contains(w)))
}

fn object_verb(back: &[&str], st: &Stand<'_>) -> Option<usize> {
    let own = st
        .word
        .split(' ')
        .next()
        .is_some_and(|w| DETERMINERS.contains(&w) || w == "local");
    let determiner = |w: &str| {
        DETERMINERS.contains(&w) || (!w.is_empty() && w.bytes().all(|c| c.is_ascii_digit()))
    };
    let d = match back {
        _ if own => back.len(),
        [.., d] if determiner(d) => back.len() - 1,
        [.., d, w] if determiner(d) && !CUTS.contains(w) && !negation(w) => back.len() - 2,
        _ => return None,
    };
    d.checked_sub(1)
}

fn worker_did(pre: &[&str]) -> bool {
    let from = pre
        .iter()
        .rposition(|w| matches!(*w, "and" | "then"))
        .map_or(0, |i| i + 1);
    pre[from..].iter().all(|w| {
        matches!(
            *w,
            "i" | "we"
                | "then"
                | "also"
                | "finally"
                | "just"
                | "so"
                | "have"
                | "had"
                | "has"
                | "now"
                | "already"
                | "since"
        )
    })
}

const GONE: &[&str] = &["deleted", "removed", "dropped", "pruned"];

fn merged_first(whole: &[&str]) -> bool {
    matches!(
        whole,
        ["after", "merging" | "landing"] | ["after", "merging" | "landing", "it" | "them" | "both"]
    )
}

const UNDOES: &[&str] = &[
    "without", "before", "instead", "unmerged", "unlanded", "unpushed", "never", "not",
];

fn landing_claimed(s: &str) -> bool {
    let words: Vec<&str> = s.split_whitespace().map(bare).collect();
    main_claimed(s)
        || words.iter().enumerate().any(|(i, w)| {
            matches!(*w, "merged" | "landed")
                && !words[..i]
                    .iter()
                    .rev()
                    .take(3)
                    .any(|x| negation(x) || matches!(*x, "not" | "without" | "before"))
        })
}

fn past_aside(s: &str, to: usize) -> Option<(String, usize)> {
    let rest = &s[to..];
    for lead in [", then ", ", and then ", ", but then "] {
        if let Some(r) = rest.strip_prefix(lead) {
            let joined = format!("{} then {r}", &s[..to]);
            let end = clause_end(&joined, to + 1);
            return Some((joined, end));
        }
    }
    let mark = rest.chars().next()?;
    let (close, most) = match mark {
        ',' => (',', 4),
        '—' | '–' => (mark, 6),
        '(' => (')', 8),
        _ => return None,
    };
    let inner_at = to + mark.len_utf8();
    let len = s[inner_at..].find(close)?;
    let inner = &s[inner_at..inner_at + len];
    let after = &s[inner_at + len + close.len_utf8()..];
    let words: Vec<&str> = inner.split_whitespace().map(bare).collect();
    let first = after.split_whitespace().map(bare).find(|w| !w.is_empty());
    let participle = match words.as_slice() {
        ["as", w] => past_form(w),
        [w, rest @ ..] => {
            past_form(w)
                && !rest
                    .iter()
                    .any(|r| verbish(r) && !PREPOSITIONS.contains(r) && *r != "on")
        }
        [] => false,
    } && !words.iter().any(|w| negation(w) || UNSETTLED.contains(w));
    if words.is_empty()
        || words.len() > most
        || inner.contains(CLAUSE_MARKS)
        || (words.iter().any(|w| verbish(w))
            && !participle
            && !words
                .first()
                .is_some_and(|w| matches!(*w, "which" | "who" | "that")))
        || !first.is_some_and(verbish)
    {
        return None;
    }
    let joined = format!("{}{after}", &s[..to]);
    let end = clause_end(&joined, to);
    Some((joined, end))
}

fn even_with_remote(after: &[&str]) -> bool {
    let remote = |m: &str| matches!(m, "origin/main" | "origin/master" | "upstream/main");
    if let [
        "is",
        "even" | "level" | "identical" | "equal",
        "with" | "to",
        m,
    ]
    | ["is", "in", "sync", "with", m]
    | ["is", "at", m] = after
    {
        return remote(m);
    }
    let rest = match after {
        ["and", m, rest @ ..]
            if matches!(*m, "origin/main" | "origin/master" | "upstream/main") =>
        {
            rest
        }
        [verb, ..] if matches!(*verb, "matches" | "equals") => after,
        _ => return false,
    };
    matches!(
        rest,
        ["point" | "are", "at", "the", "same", "commit" | "sha"]
            | ["point", "to", "the", "same", "commit" | "sha"]
            | ["match" | "agree"]
            | ["matches" | "equals", "origin/main" | "upstream/main"]
            | ["are", "in", "sync" | "step"]
            | ["are", "identical" | "even" | "equal"]
    )
}

fn also_landed(s: &str, (from, to): (usize, usize), st: &Stand<'_>, stands: &[Stand<'_>]) -> bool {
    let words: Vec<&str> = s[from..to]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let lead = s[..from].trim_end();
    let added = matches!(words.last(), Some(&"too" | &"also" | &"included"))
        || words.ends_with(&["as", "well"]);
    added
        && words.len() <= 6
        && matches!(
            st.word,
            "follow-up"
                | "follow-ups"
                | "followup"
                | "followups"
                | "pr"
                | "prs"
                | "pull request"
                | "pull requests"
                | "mr"
                | "mrs"
                | "#n"
        )
        && lead.ends_with(',')
        && landing_claimed(lead)
        && !words.iter().any(|w| verbish(w) && *w != "included")
        && !stands.iter().any(|o| {
            (from..to).contains(&o.at) && o.at != st.at && !matches!(o.reads(), Reads::Change)
        })
}

const BEFORE_NOW: &[&str] = &[
    "earlier",
    "before",
    "previously",
    "initially",
    "originally",
    "at",
    "first",
    "once",
    "yesterday",
];

fn aside_settled(s: &str, st: &Stand<'_>) -> bool {
    let (at, end) = (st.at, st.end());
    let Some(open_at) = s[..at].rfind(CLAUSE_MARKS) else {
        return false;
    };
    let Some(mark) = s[open_at..].chars().next() else {
        return false;
    };
    let close = match mark {
        ',' | '—' | '–' => mark,
        '(' => ')',
        _ => return false,
    };
    let Some(close_at) = s[end..].find(close).map(|c| end + c) else {
        return false;
    };
    let inner: Vec<&str> = s[open_at + mark.len_utf8()..close_at]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let row: Vec<&str> = st.word.split(' ').collect();
    let relative = inner
        .first()
        .is_some_and(|w| matches!(*w, "which" | "who" | "that"));
    let mut i = 0;
    if relative {
        if !inner
            .get(1)
            .is_some_and(|w| matches!(*w, "was" | "were" | "had"))
        {
            return false;
        }
        i = 2;
        if inner.get(i) == Some(&"been") {
            i += 1;
        }
    }
    if inner.get(i..i + row.len()) != Some(row.as_slice()) {
        return false;
    }
    let dated = &inner[i + row.len()..];
    let on: Vec<&str> = dated
        .iter()
        .copied()
        .filter(|w| !BEFORE_NOW.contains(w))
        .collect();
    let waited_on = on.is_empty()
        || (on.len() <= 3
            && matches!(on[0], "on" | "by" | "in" | "for" | "behind")
            && dated.iter().any(|w| BEFORE_NOW.contains(w) && *w != "at")
            && !on
                .iter()
                .any(|w| negation(w) || UNSETTLED.contains(w) || matches!(*w, "still" | "now")));
    if !waited_on || (dated.is_empty() && !relative) {
        return false;
    }
    let from = clause_start(s, open_at);
    let subject: Vec<&str> = s[from..open_at]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    if subject.is_empty() || subject.iter().any(|w| verbish(w)) {
        return false;
    }
    let rest = &s[close_at + close.len_utf8()..];
    let pred: Vec<&str> = rest[..clause_end(rest, 0)]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let by = match settlers(st, &subject) {
        by if by.is_empty() => SHIPS.to_vec(),
        by => by,
    };
    let place = st.lands_by_place() || subject.iter().any(|w| CHANGE_NOUNS.contains(w));
    pred.first().is_some_and(|w| starts_predicate(w, &by))
        && !pred
            .iter()
            .any(|w| UNSETTLED.contains(w) || negation(w) || *w == "without")
        && settles_with(&pred, &by, place)
}

fn nouns_in(s: &str, a: usize, b: usize, nouns: &[&str]) -> bool {
    let mut prev = None;
    s[a..b].split_whitespace().any(|raw| {
        let hit = noun_in(prev, raw, nouns);
        prev = Some(raw);
        hit
    })
}

const SECRETS: &[&str] = &[
    "token",
    "tokens",
    "key",
    "keys",
    "password",
    "passwords",
    "secret",
    "secrets",
    "passphrase",
];

const SECRET_KINDS: &[&str] = &[
    "crates.io",
    "npm",
    "pypi",
    "api",
    "access",
    "auth",
    "github",
    "gitlab",
    "release",
    "publish",
    "publishing",
    "signing",
    "deploy",
    "personal",
    "apple",
    "notary",
    "ssh",
    "gpg",
    "admin",
];

const NOT_POSSESSIVE: &[&str] = &[
    "it's", "that's", "what's", "there's", "here's", "let's", "he's", "she's", "who's", "one's",
];

fn noun_in(prev: Option<&str>, raw: &str, nouns: &[&str]) -> bool {
    let w = bare(raw);
    let numbered = || {
        let t = raw.trim_matches(|c: char| matches!(c, ',' | '.' | ':' | ';' | '(' | ')'));
        t.len() > 1 && t.starts_with(['#', '!']) && t[1..].bytes().all(|b| b.is_ascii_digit())
    };
    let secret = || {
        SECRETS.contains(&w)
            && prev.map(bare).is_some_and(|p| {
                (p.ends_with("'s") && !NOT_POSSESSIVE.contains(&p)) || SECRET_KINDS.contains(&p)
            })
    };
    nouns.contains(&w)
        || w.strip_suffix("'s").is_some_and(|w| nouns.contains(&w))
        || (nouns.contains(&"tests") && (w.contains("tests::") || w.ends_with("::tests")))
        || (nouns.contains(&"#n") && numbered())
        || (nouns.contains(&"#secret") && secret())
        || (nouns.contains(&"#branch")
            && branch_name(w)
            && !w.split_once('/').is_some_and(|(_, m)| MAINS.contains(&m)))
}

fn labelled(s: &str, from: usize, before: &[&str], nouns: &[&str]) -> bool {
    if !before.iter().all(|w| AUX.contains(w)) {
        return false;
    }
    let lead = s[..from].trim_end();
    let label_end = if let Some(l) = lead.strip_suffix(':') {
        l.len()
    } else if lead.ends_with(',') {
        match lead.rfind(':') {
            Some(colon) if !lead[colon..].contains([';', '(', ')']) => colon,
            _ => return false,
        }
    } else if lead.ends_with(')') {
        match lead.rfind('(') {
            Some(open) => open,
            None => return false,
        }
    } else {
        return false;
    };
    nouns_in(s, clause_start(s, label_end), label_end, nouns)
}

fn negation(w: &str) -> bool {
    OPEN_NEGATIONS.contains(&w) || matches!(w, "nobody" | "noone" | "no-one")
}

const CONTRASTS: &[&str] = &[
    "but", "while", "whereas", "though", "although", "yet", "however",
];

const GLUE: &[&str] = &[
    "and", "or", "nor", "a", "an", "any", "also", "either", "neither",
];

const VERBS: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "has", "have", "had", "will", "would", "can",
    "could", "do", "does", "did", "remains", "remain", "stays", "needs", "awaits", "waits",
];

const STRONG: &[&str] = &[
    "the", "this", "that", "these", "those", "its", "my", "our", "their", "his", "her", "your",
];

fn denied(s: &str, from: usize, at: usize, st: &Stand<'_>, stands: &[Stand<'_>]) -> bool {
    let covered = |p: usize| stands.iter().any(|o| (o.at..o.end()).contains(&p));
    let before: Vec<(usize, &str)> = words_at(s, from, at).map(|(p, w)| (p, bare(w))).collect();
    let reach = reach(&before, stands);
    let near = &before[reach..];
    let has = |w: &str| near.iter().any(|&(_, x)| x == w);
    if near.iter().any(|&(_, w)| negation(w))
        || (!st.word.starts_with("not ") && near.last().is_some_and(|&(_, w)| w == "not"))
        || (has("not")
            && ["any", "anything", "anyone", "anybody"]
                .iter()
                .any(|w| has(w)))
    {
        return true;
    }
    let to = clause_end(s, st.end());
    if reach > 0 || s[to..].starts_with(':') {
        return false;
    }
    let tail: Vec<(usize, &str)> = words_at(s, st.end(), to)
        .map(|(p, w)| (p, bare(w)))
        .filter(|(_, w)| !w.is_empty())
        .collect();
    let listed =
        |words: &[(usize, &str)]| words.iter().all(|&(p, w)| covered(p) || GLUE.contains(&w));
    let mut cut = from;
    let mut current = before;
    let mut first = true;
    loop {
        if cut == 0 || !s[..cut].ends_with(',') {
            return false;
        }
        let start = clause_start(s, cut - 1);
        if s[..start].trim_end().ends_with(':') {
            return false;
        }
        let previous: Vec<(usize, &str)> = words_at(s, start, cut - 1)
            .map(|(p, w)| (p, bare(w)))
            .collect();
        let predicates = listed(&current) && (!first || listed(&tail));
        let joined = current
            .iter()
            .chain(if first { tail.iter() } else { [].iter() })
            .any(|&(_, w)| matches!(w, "or" | "nor" | "and"));
        let subjects = previous.len() >= 2
            && matches!(previous[0].1, "no" | "neither" | "zero")
            && !previous.iter().any(|&(_, w)| VERBS.contains(&w))
            && joined
            && !current
                .iter()
                .any(|&(_, w)| VERBS.contains(&w) || STRONG.contains(&w) || CONTRASTS.contains(&w));
        if !(predicates || subjects) {
            return false;
        }
        if previous.iter().any(|&(_, w)| negation(w)) {
            return true;
        }
        current = previous;
        cut = start;
        first = false;
    }
}

const REACH_BREAKS: &[&str] = &[
    "so",
    "because",
    "since",
    "as",
    "then",
    "therefore",
    "hence",
    "which",
    "until",
    "unless",
    "when",
    "if",
];

fn reach(before: &[(usize, &str)], stands: &[Stand<'_>]) -> usize {
    let covered = |p: usize| stands.iter().any(|o| (o.at..o.end()).contains(&p));
    before
        .iter()
        .enumerate()
        .rev()
        .find(|&(i, &(_, w))| {
            CONTRASTS.contains(&w)
                || REACH_BREAKS.contains(&w)
                || (w == "and"
                    && before
                        .get(i + 1)
                        .is_some_and(|&(p, x)| !(covered(p) || GLUE.contains(&x) || negation(x))))
        })
        .map_or(0, |(i, _)| i + 1)
}

const WORK_NOUNS: &[&str] = &[
    "pr", "prs", "mr", "mrs", "change", "changes", "commit", "commits", "branch", "branches",
    "fix", "fixes", "work", "code", "patch", "patches", "push", "merge", "part", "piece",
];

const BESIDE_LANDING: &[&str] = &[
    "else",
    "new",
    "more",
    "other",
    "further",
    "extra",
    "but",
    "except",
    "besides",
    "apart",
    "without",
    "since",
    "after",
    "afterwards",
    "anywhere",
    "elsewhere",
    "outside",
    "than",
];

const MERGE_THINGS: &[&str] = &[
    "conflict",
    "conflicts",
    "commit",
    "commits",
    "request",
    "requests",
    "queue",
    "train",
    "base",
    "tool",
    "button",
    "strategy",
];

fn landing_denied(
    s: &str,
    from: usize,
    at: usize,
    after: &[&str],
    st: &Stand<'_>,
    stands: &[Stand<'_>],
) -> bool {
    let before: Vec<(usize, &str)> = words_at(s, from, at).map(|(p, w)| (p, bare(w))).collect();
    let near: Vec<&str> = before[reach(&before, stands)..]
        .iter()
        .map(|&(_, w)| w)
        .collect();
    if near.last().is_some_and(|w| UNSETTLED.contains(w))
        || near.iter().chain(after).any(|w| BESIDE_LANDING.contains(w))
    {
        return false;
    }
    if matches!(st.word, "push" | "merge")
        && (!near.last().is_some_and(|w| negation(w))
            || after.first().is_some_and(|w| MERGE_THINGS.contains(w)))
    {
        return false;
    }
    let undone = matches!(st.reads(), Reads::Undone);
    near.iter().enumerate().any(|(i, w)| {
        let work = || {
            near[i + 1..]
                .iter()
                .any(|x| WORK_NOUNS.contains(x) || (undone && OWED.contains(x)))
                || WORK_NOUNS.contains(&st.word)
        };
        let any = matches!(
            *w,
            "nothing" | "none" | "never" | "nobody" | "noone" | "no-one" | "neither" | "nor"
        );
        (any && !undone) || ((any || matches!(*w, "no" | "zero" | "0")) && work())
    })
}

const OWED: &[&str] = &[
    "test",
    "tests",
    "docs",
    "doc",
    "documentation",
    "benchmark",
    "benchmarks",
    "changelog",
    "example",
    "examples",
    "readme",
    "migration",
    "port",
    "suite",
    "check",
    "checks",
    "review",
];

const WHOLE: &[&str] = &[
    "done",
    "merged",
    "landed",
    "pushed",
    "finished",
    "complete",
    "everything",
    "all",
    "nothing",
    "main",
    "origin/main",
    "master",
];

fn excepted(s: &str, st: &Stand<'_>) -> bool {
    let before = s[..st.at].trim_end();
    let prev = before.split_whitespace().last().map(bare);
    let near: Vec<&str> = before.split_whitespace().rev().take(4).map(bare).collect();
    if near.windows(2).any(|w| {
        matches!(w[0], "text" | "file" | "copy") && matches!(w[1], "main's" | "origin/main's")
    }) {
        return false;
    }
    if st.word == "not the" {
        let next = s[st.end()..].split_whitespace().next().map(bare);
        return before.ends_with(',')
            && landing_claimed(before)
            && !next.is_some_and(|n| {
                matches!(
                    n,
                    "branch" | "fork" | "worktree" | "feature" | "side" | "local" | "old" | "stale"
                )
            });
    }
    if matches!(
        st.word,
        "omission" | "omissions" | "holdout" | "holdouts" | "excluded" | "excepted"
    ) && prev.is_some_and(|p| matches!(p, "no" | "nothing" | "none" | "without" | "zero"))
    {
        // `with no omissions`, `nothing excluded`: none is made.
        return false;
    }
    if matches!(st.word, "exception" | "the exception") {
        let next = s[st.end()..].split_whitespace().next().map(bare);
        return !prev.is_some_and(|p| matches!(p, "without" | "no" | "any"))
            && next.is_none_or(|n| {
                matches!(
                    n,
                    "" | "of"
                        | "to"
                        | "is"
                        | "was"
                        | "are"
                        | "were"
                        | "being"
                        | "here"
                        | "there"
                        | "remains"
                        | "stays"
                )
            });
    }
    if !matches!(
        st.word,
        "but" | "bar" | "save" | "less" | "beyond" | "without"
    ) {
        return true;
    }
    let next = s[st.end()..].split_whitespace().next().map(bare);
    if st.word == "without" {
        // Only a thing left out after a comma: `all on main, without the
        // docs`, never `ran on main without a crash`.
        return before.ends_with(',') && next.is_some_and(|n| STRONG.contains(&n));
    }
    let whole = prev.is_some_and(|p| WHOLE.contains(&p) && !before.ends_with(','));
    let after_comma = st.word != "but" && before.ends_with(',');
    (whole || after_comma)
        && next.is_some_and(|n| {
            DETERMINERS.contains(&n) || (!n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
        })
}

const NIL_GLUE: &[&str] = &[
    "else",
    "more",
    "really",
    "truly",
    "too",
    "very",
    "particularly",
    "terribly",
    "super",
    "of",
    "on",
    "the",
    "remaining",
    "items",
    "item",
    "issues",
    "issue",
    "left",
    "is",
    "are",
    "was",
    "were",
    "still",
    "open",
    "pending",
    "outstanding",
    "work",
    "tasks",
    "task",
    "things",
    "thing",
    "it",
    "them",
    "that",
    "which",
    "seems",
    "seem",
    "looks",
    "remains",
    "remain",
    "at",
    "all",
];

fn qualified_nil(s: &str, st: &Stand<'_>) -> bool {
    let words: Vec<&str> = s[clause_start(s, st.at)..st.at]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let next = s[st.end()..].split_whitespace().next().map(bare);
    if st.word == "major" && next.is_some_and(|n| matches!(n, "version" | "release" | "bump")) {
        return false;
    }
    words.first().is_some_and(|w| {
        matches!(*w, "nothing" | "none" | "no" | "zero" | "nil")
            || (*w == "not" && words.get(1) == Some(&"much"))
    }) && words.len() <= 7
        && words[1..].iter().all(|w| NIL_GLUE.contains(w))
}

fn only_item(s: &str, st: &Stand<'_>) -> bool {
    let from = clause_start(s, st.at);
    if !s[from..st.at].trim().is_empty() {
        return false;
    }
    let Some(lead) = s[..from].trim_end().strip_suffix(',') else {
        return false;
    };
    let words: Vec<&str> = lead[clause_start(lead, lead.len())..]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    !words.is_empty()
        && words.len() <= 5
        && !words.iter().any(|w| {
            verbish(w)
                || SHIPS.contains(w)
                || LANDED.contains(w)
                || MAINS.contains(w)
                || negation(w)
                || matches!(*w, "all" | "everything" | "both")
        })
        && !(0..words.len()).any(|i| onto_main(&words[i..]))
}

const LEFTOVER: &[&str] = &[
    "remains",
    "remain",
    "left",
    "pending",
    "outstanding",
    "open",
    "do",
    "land",
];

fn nothing_but(s: &str, st: &Stand<'_>) -> bool {
    if st.word != "but" {
        return false;
    }
    let lead = s[clause_start(s, st.at)..st.at].trim_end();
    let words: Vec<&str> = lead.split_whitespace().map(bare).collect();
    let next = s[st.end()..]
        .split_whitespace()
        .map(bare)
        .find(|w| !w.is_empty());
    !lead.ends_with(',')
        && matches!(words.first(), Some(&("nothing" | "none")))
        && words.iter().any(|w| LEFTOVER.contains(w))
        && next.is_some_and(|n| !negation(n))
}

const FAULTS: &[&str] = &[
    "recur", "happen", "occur", "deadlock", "race", "crash", "hang", "leak", "break", "fail",
    "regress", "overflow", "panic",
];

const PROOF_ROWS: &[&str] = &[
    "fail",
    "fails",
    "failed",
    "failing",
    "still fail",
    "still fails",
    "still failing",
    "reverted",
];

const FIX_NOUNS: &[&str] = &[
    "fix", "fixes", "change", "patch", "commit", "guard", "rule", "check",
];

const OLD_CODE: &[&str] = &[
    "code",
    "rule",
    "rules",
    "reader",
    "filter",
    "logic",
    "implementation",
    "parser",
    "guard",
];

fn proves_by_failing(s: &str, (from, to): (usize, usize), (at, end): (usize, usize)) -> bool {
    let sentence: Vec<&str> = s
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let pair = |a: &str, b: &[&str]| sentence.windows(2).any(|w| w[0] == a && b.contains(&w[1]));
    let flake = sentence.iter().any(|w| {
        matches!(
            *w,
            "flaky"
                | "flake"
                | "flakes"
                | "flaked"
                | "flaking"
                | "intermittent"
                | "intermittently"
                | "sporadically"
                | "rerun"
                | "re-run"
                | "retry"
                | "retried"
                | "isolation"
        )
    }) || pair("under", &["load"])
        || pair("run", &["alone"])
        || pair("ran", &["alone"])
        || ["fail", "fails", "failed", "passed", "passes"]
            .iter()
            .any(|f| pair(f, &["once", "alone"]));
    if flake {
        return false;
    }
    let raw: Vec<(usize, &str)> = words_at(s, from, to).collect();
    let w: Vec<&str> = raw.iter().map(|&(_, r)| bare(r)).collect();
    let fail = |x: &str| matches!(x, "fail" | "fails" | "failed" | "failing");
    let (Some(first), Some(last)) = (
        raw.iter().position(|&(p, r)| p + r.len() > at),
        raw.iter().rposition(|&(p, _)| p < end),
    ) else {
        return false;
    };
    let f = if fail(w[last]) || w[last] == "reverted" {
        last
    } else if w.get(last + 1).is_some_and(|n| fail(n)) {
        last + 1
    } else {
        return false;
    };
    let joined = |a: usize, b: usize| {
        w[a.min(b) + 1..a.max(b)]
            .iter()
            .any(|x| CONTRASTS.contains(x) || matches!(*x, "and" | "or" | "so" | "then"))
    };
    let undone = |x: &str| matches!(x, "reverted" | "removed" | "undone");
    let taken_out = (0..w.len()).any(|i| {
        matches!(w[i], "when" | "if" | "once")
            && (i + 1..w.len().min(i + 7)).any(|r| {
                undone(w[r])
                    && w[i + 1..r].iter().any(|x| FIX_NOUNS.contains(x))
                    && (0..i).any(|g| fail(w[g]) && (g == f || r == f) && !joined(g, r))
            })
    }) || matches!(
        w.get(f + 1..),
        Some(["without", "the" | "its" | "that" | "this", n, ..]) if FIX_NOUNS.contains(n)
    ) || matches!(
        w.get(f + 1..),
        Some(["with", "the" | "its" | "that" | "this", n, u, ..])
            if FIX_NOUNS.contains(n) && undone(u)
    );
    if w[f] == "reverted" {
        return taken_out;
    }
    let check = |i: usize| {
        w.get(i + 1).is_some_and(|n| {
            matches!(
                *n,
                "test" | "tests" | "job" | "jobs" | "suite" | "check" | "checks" | "lane"
            )
        }) && w.iter().skip(i + 2).take(2).any(|x| fail(x))
    };
    let agent = (0..first).any(|i| {
        let noun = matches!(w[i], "mutation" | "mutations" | "mutant" | "mutants")
            || (matches!(w[i], "control" | "controls") && i > 0 && w[i - 1] == "negative");
        noun && !check(i) && !joined(i, first)
    });
    let seen = (0..w.len().saturating_sub(2)).any(|i| {
        matches!(w[i], "seen" | "shown" | "watched")
            && w[i + 1] == "to"
            && w[i + 2] == "fail"
            && (i + 2 == f || (f < i && !joined(f, i)))
    });
    let after_fix = sentence.windows(4).any(|x| {
        matches!(x[0], "pass" | "passes")
            && matches!(x[1], "after" | "with")
            && matches!(x[2], "the" | "its" | "this")
            && x[3] == "fix"
    });
    let on_old = match w.get(f + 1..) {
        Some(["on" | "against", "the", old, code, ..]) => {
            matches!(
                *old,
                "old" | "unfixed" | "original" | "previous" | "pre-fix"
            ) && OLD_CODE.contains(code)
        }
        Some(["on" | "against", "main's" | "origin/main's", "code", ..]) => after_fix,
        _ => false,
    };
    agent || seen || on_old || taken_out
}

const TASK_ITEMS: &[&str] = &["item", "items", "finding", "findings"];

fn cannot_happen(after: &str) -> bool {
    let mut words = after.split_whitespace().map(bare);
    words.next().is_some_and(|w| FAULTS.contains(&w))
}

const IDLE: &[&str] = &["else", "more", "other", "at", "all", "of", "it", "them", ""];

fn excepting(s: &str, stands: &[Stand<'_>]) -> bool {
    if stands
        .iter()
        .any(|st| !matches!(st.reads(), Reads::Exception) && !st.reads().common())
    {
        return true;
    }
    !s.split(CLAUSE_MARKS).any(|clause| {
        let words: Vec<&str> = clause.split_whitespace().map(bare).collect();
        words.iter().enumerate().any(|(i, w)| {
            negation(w)
                && *w != "without"
                && !words[..i]
                    .iter()
                    .any(|x| matches!(*x, "that" | "which" | "who"))
                && words[i + 1..].iter().any(|x| !IDLE.contains(x))
        })
    })
}

const PREPOSITIONS: &[&str] = &[
    "in", "on", "for", "to", "with", "at", "from", "until", "till", "after", "before", "behind",
    "over", "under",
];

fn alone(after: &str) -> bool {
    after
        .split_whitespace()
        .map(bare)
        .find(|w| !w.is_empty())
        .is_none_or(|w| PREPOSITIONS.contains(&w))
}

const MECHANISMS: &[&str] = &[
    "waits", "wait", "waiting", "blocks", "block", "blocking", "retries", "retry", "spins", "spin",
    "loops", "loop", "sleeps", "sleep", "polls", "poll", "holds", "hold", "buffers", "queues",
    "reads", "scrolls", "runs", "repeats", "backs",
];

const AUTOMATIC: &[&str] = &[
    "on its own",
    "on their own",
    "by itself",
    "by themselves",
    "automatically",
    "unattended",
];

const DENIALS: &[&str] = &["no", "not", "nothing", "never", "none", "nobody"];

fn quoted(text: &str, at: usize) -> bool {
    let before = &text[..at];
    before.matches('"').count() % 2 == 1 || before.rfind('“') > before.rfind('”')
}

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

fn reason_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | '(' | ')'))
        .filter(|w| !w.is_empty())
}

const ONLY_SLOW: &[&str] = &["time", "slow", "long", "minutes", "hours"];

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

/// AUTOPILOT LAND's instruction, typed at the first point of a Land episode where the continuation or the done check would go.
pub const RULE_LAND: &str = "land@v1";

/// WHAT A NOTE RESTATING A LANDING AFTER THE TASK'S OWN WAKE SAYS ([`TurnEndState::take_wake_restated`]).
pub const WAKE_KEPT: &str = "the landing holds through the task's own wake";
/// A nudge after the land instruction ([`LAND_NUDGE`]).
pub const RULE_LAND_NUDGE: &str = "land-nudge@v1";
/// The answer a question gets under Land ([`LAND_ANSWER`]).
pub const RULE_LAND_ANSWER: &str = "land-answer@v1";

/// What Land's instruction and nudge say of files no git repository holds ([`NO_REPOSITORY`]).
macro_rules! no_repository {
    () => {
        "Files no git repository holds land where they are once verified."
    };
}

/// FILES NO REPOSITORY HOLDS LAND WHERE THEY ARE: [`LAND`]'s and [`LAND_NUDGE`]'s sentence for them. It names the files, not the folder, so a worker whose tab is no repository still pushes the work it did in one; the probe still reads the session's repository at a defense ([`repo_work_suffix`]). It is short so the standing rules keep room on Land's first act with both its clauses ([`VERBATIM_UNITS`]).
pub const NO_REPOSITORY: &str = no_repository!();

/// What Land types at an episode's first point ([`RULE_LAND`]). Its [`NO_REPOSITORY`] sentence keeps a worker whose files no repository holds from asking a person for a remote to push to: on 2026-10-24 a real model's work in a plain folder answered the words without it with a NEEDS YOU for one in 3 runs of 3 (one a live `spawn agent=codex` worker), and with it with a clean `Nothing left:` in 3 of 3; Land nudges with no bound, so each nudge would have asked again.
pub const LAND: &str = concat!(
    harness_mark!(),
    " Autopilot is Land: land ALL of this work into the remote main branch, no exceptions. \
     Finish every open item, verify it, commit, merge and push it, and remove your worktrees \
     and merged branches. Leave nothing unlanded, parked on a branch or for later; chew \
     through every wall with safe, reversible fixes. Start no new work. ",
    no_repository!(),
    " Put what only a person can give on a line starting NEEDS YOU: and land the rest. Once it \
     is all on remote main, defend that in a few lines starting Nothing left:"
);

/// What a Land nudge types ([`RULE_LAND_NUDGE`]) at every free point after the instruction until the work is landed.
pub const LAND_NUDGE: &str = concat!(
    harness_mark!(),
    " Still landing: get everything onto the remote main branch, no exceptions, with nothing \
     unlanded, parked on a branch or for later. ",
    no_repository!(),
    " Put what only a person can give on a line starting NEEDS YOU: and land the rest. Once it \
     is all on remote main, defend that in a few lines starting Nothing left:"
);

/// What a question gets under Land ([`RULE_LAND_ANSWER`]).
pub const LAND_ANSWER: &str = "Nobody is here to choose: take the option that lands all of this \
                               work on the remote main soonest and start no new work. Pushing \
                               and merging are fine; never force-push or delete work.";

/// WHETHER THE REPLY `tail` LANDS THE WORK, read at a land act's yield ([`TurnEndState::observe`]).
#[must_use]
pub fn clean_defense(tail: Option<&str>) -> bool {
    tail.is_some_and(|t| {
        needs::defense_line(t).is_some()
            && needs::needs_you(t).is_empty()
            && open_work_strict(Some(&needs::defense_body(t))).is_empty()
    })
}

fn land_open_words(tail: Option<&str>) -> Vec<&'static str> {
    match tail {
        Some(t) if needs::defense_line(t).is_some() => {
            open_work_strict(Some(&needs::defense_body(t)))
        }
        Some(t) if says_done(Some(t)) => {
            open_work_read_as(&needs::without_needs(t), Some(&LAND_FAMILIES), false)
        }
        _ => Vec::new(),
    }
}

fn hold_at_yield(reading: &TurnEndReading) -> LandHeld {
    let tail = reading.said_tail.as_deref();
    let said_need = [reading.turn_said.as_deref(), tail]
        .into_iter()
        .flatten()
        .any(|t| !needs::needs_you(t).is_empty());
    let words = land_open_words(tail);
    let mut open = [None; 4];
    for (slot, word) in open
        .iter_mut()
        .zip(words.iter().filter_map(|w| OpenWord::of(w)))
    {
        *slot = Some(word);
    }
    let repo = reading.repo.found();
    let legs = [
        (HoldLeg::Needs, said_need),
        (HoldLeg::Repo, repo.is_some()),
        (HoldLeg::Unread, reading.repo.is_unread()),
        (HoldLeg::Open, !words.is_empty()),
        (
            HoldLeg::Defense,
            tail.and_then(needs::defense_line).is_none(),
        ),
        (HoldLeg::Cut, reading.said_cut),
    ]
    .into_iter()
    .filter(|&(_, held)| held)
    .fold(0, |bits, (leg, _)| bits | leg.bit());
    let count = |n: fn(&crate::harness::repo_work::RepoWork) -> usize| {
        repo.map_or(0, |r| u16::try_from(n(r)).unwrap_or(u16::MAX))
    };
    LandHeld {
        since_ms: None,
        nudges: 0,
        read: true,
        legs,
        open,
        changed: count(|r| r.changed),
        untracked: count(|r| r.untracked),
        stashes: count(|r| r.stashes),
        unpushed: count(|r| r.unpushed),
        unlanded: count(|r| r.unlanded),
    }
}

fn open_words() -> impl Iterator<Item = &'static str> {
    OPEN_HEADS
        .iter()
        .chain(LAND_HEADS)
        .chain(OPEN_STATUS)
        .copied()
        .chain(
            LAND_FAMILIES
                .iter()
                .flat_map(|f| f.says.into_iter().chain(f.rows.iter().copied())),
        )
        .chain([
            UNCHECKED,
            QUESTION,
            CHANGE_NUMBER,
            PERCENT,
            COUNT_BESIDE_NONE,
        ])
}

/// The index of `word` among [`open_words`], counted from 1 (its first place).
#[must_use]
pub fn open_word_index(word: &str) -> Option<std::num::NonZeroU16> {
    let at = open_words().position(|w| w == word)?;
    u16::try_from(at + 1)
        .ok()
        .and_then(std::num::NonZeroU16::new)
}

/// The word at `index` ([`open_word_index`]).
#[must_use]
pub fn open_word_at(index: std::num::NonZeroU16) -> Option<&'static str> {
    open_words().nth(usize::from(index.get()) - 1)
}

fn wake_left_open(reading: &TurnEndReading) -> bool {
    reading.said_cut || wake_words_open(reading)
}

fn wake_words_open(reading: &TurnEndReading) -> bool {
    [reading.turn_said.as_deref(), reading.said_tail.as_deref()]
        .into_iter()
        .flatten()
        .any(|t| {
            !needs::needs_you(t).is_empty()
                || !open_work_strict(Some(&needs::without_needs(t))).is_empty()
        })
}

/// RETIRED: [`LAND`]'s words of `4bff4848b4`, as [`DONE_CHECK_COMMITTED_BEFORE`].
pub const LAND_COMMITTED_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed and pushed \
     (merged to main where this repository lands work), with nothing uncommitted, untracked or \
     stashed. Write down what is left in the repository and commit that too, so your summary \
     names nothing left. Then write a short summary of whether the work is complete and what \
     is done."
);

/// RETIRED: [`LAND`]'s words from `69a8f68178` until `4bff4848b4`.
pub const LAND_ORDINARY_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of whether the work is \
     complete, what is done and what is left."
);

/// RETIRED: [`LAND`]'s words of `0b1abdabf3` until `69a8f68178`, as [`DONE_CHECK_WORDS_BEFORE`].
pub const LAND_WORDS_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of what is done and what \
     is left, and close by saying the work is complete."
);

/// RETIRED: [`LAND`]'s words while it asked for the [`FINISHED`] token.
pub const LAND_MARKER_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of what is done and what \
     is left, and reply with ATERM-TASK-DONE on a line by itself."
);

/// RETIRED: [`LAND`]'s words of `a355109905` (FOREVER LAND), before its [`NO_REPOSITORY`] sentence.
pub const LAND_REMOTE_MAIN_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is Land: land ALL of this work into the remote main branch, no exceptions. \
     Finish every open item, verify it, commit, merge and push it, and remove your worktrees \
     and merged branches. Leave nothing unlanded, parked on a branch or for later; chew \
     through every wall with safe, reversible fixes. Start no new work. Put what only a person \
     can give on a line starting NEEDS YOU: and land the rest. Once it is all on remote main, \
     defend that in a few lines starting Nothing left:"
);

/// EVERY LAND INSTRUCTION AN OLDER BUILD TYPED, newest first.
pub const RETIRED_LANDS: [&str; 5] = [
    LAND_REMOTE_MAIN_BEFORE,
    LAND_COMMITTED_BEFORE,
    LAND_ORDINARY_BEFORE,
    LAND_WORDS_BEFORE,
    LAND_MARKER_BEFORE,
];

/// RETIRED: [`LAND_NUDGE`]'s words of `4bff4848b4`, as [`LAND_COMMITTED_BEFORE`].
pub const LAND_NUDGE_COMMITTED_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, write down what is left in the \
     repository and commit it, then summarize whether the work is complete and what is done."
);

/// RETIRED: [`LAND_NUDGE`]'s words from `69a8f68178` until `4bff4848b4`, as [`LAND_ORDINARY_BEFORE`].
pub const LAND_NUDGE_ORDINARY_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, then summarize whether the work \
     is complete, what is done and what is left."
);

/// RETIRED: [`LAND_NUDGE`]'s words of `0b1abdabf3` until `69a8f68178`, as [`LAND_WORDS_BEFORE`].
pub const LAND_NUDGE_WORDS_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, then close by saying the work is \
     complete."
);

/// RETIRED: [`LAND_NUDGE`]'s words while it asked for the [`FINISHED`] token.
pub const LAND_NUDGE_MARKER_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, then reply with ATERM-TASK-DONE on \
     a line by itself."
);

/// RETIRED: [`LAND_NUDGE`]'s words of `a355109905`, as [`LAND_REMOTE_MAIN_BEFORE`].
pub const LAND_NUDGE_REMOTE_MAIN_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: get everything onto the remote main branch, no exceptions, with nothing \
     unlanded, parked on a branch or for later. Put what only a person can give on a line \
     starting NEEDS YOU: and land the rest. Once it is all on remote main, defend that in a \
     few lines starting Nothing left:"
);

/// EVERY LAND NUDGE AN OLDER BUILD TYPED, newest first.
pub const RETIRED_LAND_NUDGES: [&str; 5] = [
    LAND_NUDGE_REMOTE_MAIN_BEFORE,
    LAND_NUDGE_COMMITTED_BEFORE,
    LAND_NUDGE_ORDINARY_BEFORE,
    LAND_NUDGE_WORDS_BEFORE,
    LAND_NUDGE_MARKER_BEFORE,
];

/// RETIRED: the [`LAND_ANSWER`] every build before Autopilot Land typed.
pub const LAND_ANSWER_BEFORE: &str = "Nobody is here to choose: take the option that finishes \
                                      this work cleanly soonest, start nothing new, and change \
                                      nothing irreversible.";

/// EVERY LAND ANSWER AN OLDER BUILD TYPED.
pub const RETIRED_LAND_ANSWERS: [&str; 1] = [LAND_ANSWER_BEFORE];

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

/// Whether `rule` is one of Land's three acts.
#[must_use]
pub fn is_land_rule(rule: &str) -> bool {
    matches!(rule, RULE_LAND | RULE_LAND_NUDGE | RULE_LAND_ANSWER)
}

/// The worker's own suggestion accepted.
pub const RULE_SUGGESTION: &str = "continue-suggestion@v1";
/// A continuation after an overload or an API error's wait.
pub const RULE_API_RETRY: &str = "api-retry@v1";
/// A continuation once the host measures the API reachable again, after an API error that never reached it.
pub const RULE_API_BACK: &str = "api-back@v1";
/// A continuation after a reply the connection cut off.
pub const RULE_API_CUTOFF: &str = "api-cutoff@v1";
/// A LOST LOGIN'S TURN DELIVERED AGAIN once the login came back in another tab of the host ([`super::login::LoginShared::Back`]).
pub const RULE_LOGIN_BACK: &str = "login-back@v1";

/// How often a tab waiting on another tab's sign-in of its login reads that sign-in again ([`super::login::LoginShared::Carried`]).
pub const LOGIN_SHARED_LOOK: Duration = Duration::from_secs(15);

/// [`RULE_LOGIN_BACK`]'s words, the agent `program`'s.
#[must_use]
pub fn login_back_text(program: Program) -> String {
    format!(
        "{} reported that its login had expired, and it is signed in again now. {CARRY_ON}",
        program.product()
    )
}

const CARRY_ON: &str = concat!(
    "Carry on from where you stopped; if the result of your last step is missing, check \
     whether it ran before you repeat it. ",
    super::needs::needs_hint!()
);

pub(super) fn sign_in_how(program: Program, message: &str) -> String {
    match program {
        Program::Claude | Program::Grok | Program::Kimi | Program::Generic => {
            "sign in with /login".to_string()
        }
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

const fn how_leads(program: Program) -> bool {
    match program {
        Program::Gemini => true,
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => false,
    }
}

const fn login_command(program: Program) -> Option<&'static str> {
    match program {
        Program::Claude => Some("/login"),
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Gemini
        | Program::Generic => None,
    }
}

const fn compact_command(program: Program) -> Option<&'static str> {
    match program {
        Program::Claude | Program::Muse => Some("/compact"),
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Gemini
        | Program::Generic => None,
    }
}

/// Text typed at an API wall's act (`rule`, at a wall of `kind` drawn by `program`).
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
        (_, ApiCause::Overlong) => format!(
            "{who} reported {what}, so your last reply was cut off. Write long files and \
             outputs across several smaller replies. {CARRY_ON}"
        ),
        (_, ApiCause::Server | ApiCause::Request) => format!("{who} reported {what}. {CARRY_ON}"),
    }
}

/// An API wall in aterm's words, from its kind alone.
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

/// What the session's host last measured of the agent's route to its API ([`crate::supervise::IdleHost::reach`]), never read off the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    #[default]
    Unknown,
    Down {
        since: Instant,
    },
    Up {
        since: Instant,
    },
}

/// The most acts of one wall rule this policy types in its window ([`TurnEndTiming::window`], an hour).
pub const WALL_ACTS_PER_WINDOW: usize = 12;

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
/// The agent relaunched under another of the owner's Claude Code accounts.
pub const RULE_ACCOUNT: &str = "account-rotate@v1";
/// The accounts probe.
pub const RULE_USAGE_PROBE: &str = "usage-probe@v1";

/// The gate that shut on a due accounts probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeGate {
    NotDue,
    NotIdle,
    NotAuthoritative,
    Survey,
    Wall,
    Composer,
    PendingInput,
    Interrupted,
    Question,
    NotTurnEnd,
    ActOwed,
    Person,
    Watching,
    NoLedger,
    ProbesOff,
    PanelOwed,
    NotLive,
    NotNew,
    Escalated,
    FoldLatch,
    HostWants,
    Stalled,
    WakeSettle,
    NoFence,
    NotClaude,
    NotFullscreen,
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

    /// The word the journal's `SKIPPED … gate=<word>` line carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
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
/// A model bucket's consent to go on on usage credits, accepted.
pub const RULE_CONSENT: &str = "consent-accept@v1";
/// THE CREDITS HOLD ([`credits_hold`]).
pub const RULE_CREDITS_HOLD: &str = "credits-hold@v1";
/// The credits hold's escalation where no reset is named, told once an episode.
pub const CREDITS_HOLD_TOLD: &str = "usage credits carry the session and name no reset: \
     nothing is typed on them (credits-hold@v1) but one turn when the five-hour window must \
     have reset; `[harness] usage_credits = true` continues on credits";
/// The same where the credits carry a MODEL's requests, none named ([`CreditsEpisode::model`]).
pub const CREDITS_HOLD_MODEL_TOLD: &str = "usage credits carry the requests of the model \
     this session runs, with no reset named: nothing is typed on them (credits-hold@v1) until \
     it runs another (`[harness] model_fallback`, or a relaunch); `[harness] usage_credits = \
     true` continues on credits";
/// The agent restarted in its tab for Claude Code's critical-memory banner.
pub const RULE_MEMORY_RESTART: &str = "memory-restart@v1";
/// A turn the session's host typed (upgrade notice, carry-on; [`TurnEndState::host_typed`]).
pub const RULE_HOST_TURN: &str = "harness-turn@v1";

/// Which turn the session's host typed, told by the host from its own step, never read off the screen or the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostTurn {
    Notice,
    CarryOn,
    Check,
    /// The occupancy door's line at a break ([`TurnEndState::fact_typed`]): its answer is never a done
    /// report, never a short turn, and moves no streak.
    Fact,
}

/// Why the session's host restarts the agent in its tab ([`TurnEndAction::Restart`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    Memory,
    Model { to: String },
    ModelBack { to: Option<String> },
    Account { to: String, at: AccountAt },
}

/// Where an account move is asked ([`Restart::Account`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAt {
    Wall,
    Ahead,
}

/// WHAT AN ACCOUNT MOVE THE HOST COULD NOT MAKE AT A WALL SAYS OF ASKING IT AGAIN ([`TurnEndState::account_refusal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmade {
    Again,
    NoShell,
    Launch,
}

impl Unmade {
    /// The class of the host's `step` word for a restart it did not make.
    #[must_use]
    pub fn of(step: Option<&str>) -> Self {
        match step {
            Some("refused:not-a-shell-job") => Self::NoShell,
            Some(step) if step.starts_with("refused:") || step.starts_with("failed:") => {
                Self::Launch
            }
            _ => Self::Again,
        }
    }
}

impl Restart {
    /// The one word the journal names it by (`restart:<word>`).
    #[must_use]
    pub const fn word(&self) -> &'static str {
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

/// Roster classification of the session's login at a weekly usage limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterWord {
    Unlisted,
    Row {
        next: Option<SignInNext>,
        ready: Option<Ready>,
        held: Option<HeldBy>,
    },
    Unread,
}

/// Another roster row ready to run the tab now ([`RosterWord::Row`]'s `ready`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub label: String,
}

/// WHY THE ROSTER DOES NOT MOVE A TAB AT ITS USAGE LIMIT ONTO A ROW READY ([`RosterWord::Row`]'s `held`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldBy {
    Earlier,
    Pin,
    Reset,
}

impl HeldBy {
    /// The clause [`super::notice::LIMIT_USE`]'s `{why}` is filled with.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::Earlier => super::notice::WHY_USE_EARLIER,
            Self::Pin => super::notice::WHY_USE_PIN,
            Self::Reset => super::notice::WHY_USE_RESET,
        }
    }
}

/// The roster row a sign-in by name would make ready ([`RosterWord::Row`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInNext {
    pub label: String,
    pub renew: bool,
}

/// What the session's host advises about its account at one point ([`TurnEndReading::account`], from `crate::supervise::IdleHost::account`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccountAdvice {
    Probe {
        otherwise: Option<Box<AccountAdvice>>,
    },
    Move {
        to: String,
    },
    Wait {
        until: Instant,
    },
    Resume {
        cleared_at: i64,
    },
}

impl AccountAdvice {
    /// The advice the decider acts on.
    #[must_use]
    pub fn past_probe(&self) -> Option<&AccountAdvice> {
        let mut advice = self;
        while let AccountAdvice::Probe { otherwise } = advice {
            advice = otherwise.as_deref()?;
        }
        Some(advice)
    }

    /// The roster's advice (`crate::harness::accounts::Advice`) as the policy takes it, `clock` being the `Instant` of the unix second `now`.
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
        let plain = |advice: &Advice| match advice {
            Advice::Move { to, .. } => Some(AccountAdvice::Move { to: to.clone() }),
            Advice::Wait { until } => Some(AccountAdvice::Wait { until: at(*until) }),
            Advice::Resume { cleared_at } => Some(AccountAdvice::Resume {
                cleared_at: *cleared_at,
            }),
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

    /// [`Self::of_roster_at`] for a caller that has no roster's opening and no advice past a probe to hand over.
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

/// The point the host is asked about ([`crate::supervise::IdleHost::account`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPoint {
    Wall { kind: WallKind },
    LoginEnded,
    TurnEnd,
}

impl AccountPoint {
    /// Whether the session stands at a wall here, not at a plain turn end.
    #[must_use]
    pub const fn at_wall(self) -> bool {
        !matches!(self, AccountPoint::TurnEnd)
    }
}

/// What the first turn after this session's own account move was answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountBad {
    Wall,
    SignedOut,
}

/// [`TurnEndState::account_answer`]: how the move was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAnswer {
    Served,
    Bad(AccountBad),
}

/// An account move this session's host made and no point has answered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMove {
    pub to: String,
    pub at: Instant,
    pub wall: bool,
}

/// The most account moves one session makes in its window ([`TurnEndTiming::window`], an hour).
pub const ACCOUNT_MOVES_PER_WINDOW: usize = 4;

const ACCOUNT_WAIT_FLOOR: Duration = Duration::from_secs(60);

/// The words a worker says it is DONE with.
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
    "what's next?",
    "anything else?",
    "i'm stopping here",
    "i am stopping here",
    "i'll stop here",
    "i will stop here",
    "i've stopped.",
    "i have stopped.",
    "so the next session can",
    "everything i can do is",
    "everything this machine can do",
    "nothing more i can do",
    "nothing else i can do",
];

/// WHETHER THE WORKER'S LAST WORDS CLAIM IT IS DONE.
#[must_use]
pub fn says_done(tail: Option<&str>) -> bool {
    let Some(t) = tail else {
        return false;
    };
    thread_local! {
        static LAST: LastTail<bool> = const { LastTail::new() };
    }
    LAST.with(|last| {
        last.get_or(t, || {
            needs::defense_line(t).is_some() || says_done_read(&needs::without_needs(t))
        })
    })
}

const CHECK_ECHO: [&str; 3] = [
    "nothing of the job is left",
    "everything left needs a person",
    "finished and verified",
];

fn says_done_read(t: &str) -> bool {
    if says_done_outright(t) {
        return true;
    }
    let flat = done_flat(t);
    says_idle(&flat) || CHECK_ECHO.iter().any(|echo| said_as_report(&flat, echo))
}

fn said_as_report(flat: &str, echo: &str) -> bool {
    flat.match_indices(echo).any(|(at, _)| {
        !negated_before(flat, at)
            && !conditional_at(flat, at, echo)
            && !partial_subject_at(flat, at, echo)
    })
}

const CLAUSE_ENDS: &[char] = &[',', ';', ':', '.', '!', '?', '—', '–', '(', ')'];

fn clause_before(flat: &str, at: usize) -> Vec<&str> {
    flat[..at]
        .rsplit(CLAUSE_ENDS)
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect()
}

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

fn without_code(t: &str) -> String {
    let mut block = None;
    let mut out: Vec<String> = Vec::new();
    for line in t.lines() {
        if needs::fenced_line(line, &mut block) {
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

const WAIT_WORDS: &[&str] = &["until", "till", "once"];

const WAIT_PHRASES: &[&str] = &[
    "will notify",
    "notify me",
    "notifies me",
    "waiting on",
    "waiting for",
    "reports back",
    "report back",
];

const ASKS_NEXT: &[&str] = &["what's next?", "anything else?"];

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

fn whole_words_at(flat: &str, at: usize, phrase: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let end = at + phrase.len();
    !word(flat[..at].chars().next_back())
        && !(phrase.ends_with(char::is_alphanumeric) && word(flat[end..].chars().next()))
}

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

fn waits_in(sentence: &str) -> bool {
    if WAIT_PHRASES
        .iter()
        .chain(BUSY)
        .any(|w| sentence.contains(w))
    {
        return true;
    }
    let words: Vec<&str> = sentence
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    (0..words.len()).any(|i| WAIT_WORDS.contains(&words[i]) && waits_at(&words, i))
}

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

fn said_unnegated(flat: &str, phrase: &str) -> bool {
    flat.match_indices(phrase)
        .any(|(at, _)| !negated_before(flat, at))
}

fn negated_before(flat: &str, at: usize) -> bool {
    flat[..at]
        .split(' ')
        .rev()
        .filter(|w| !w.is_empty())
        .take(2)
        .take_while(|w| !w.ends_with(['.', ',', ':', ';', '!', '?']))
        .any(|w| w == "not" || w == "cannot" || w.ends_with("n't"))
}

fn done_flat(t: &str) -> String {
    t.to_lowercase()
        .replace('’', "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn says_idle(flat: &str) -> bool {
    const EMPTY_QUEUE: &[&str] = &["all queued work has landed", "all queued work is landed"];
    if BUSY.iter().chain(STOP_PHRASES).any(|b| flat.contains(b)) {
        return false;
    }
    if EMPTY_QUEUE.iter().any(|p| said_unnegated(flat, p)) {
        return true;
    }
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
    let of_one = words
        .windows(4)
        .any(|w| one_clause(w) && none_of(w) && be(w[2].0) && idle(w[3].0));
    of_one
        || words
            .windows(3)
            .any(|w| one_clause(w) && w[0].0 == "nothing" && be(w[1].0) && idle(w[2].0))
}

pub(crate) fn is_continue_rule(rule: &str) -> bool {
    rule == RULE_CONTINUE
        || rule == RULE_SUGGESTION
        || is_answer_rule(rule)
        || rule == RULE_DONE_CHECK
        || is_challenge_rule(rule)
        || is_land_rule(rule)
}

/// Whether `rule` answers a question the worker asked in prose.
#[must_use]
pub fn is_answer_rule(rule: &str) -> bool {
    QuestionPriority::ALL
        .into_iter()
        .any(|p| p.prose_rule_id() == rule)
}

/// Claude Code's own `/model` aliases: the only names a switch back is typed with.
pub const MODEL_ALIASES: &[&str] = &["sonnet", "opus", "haiku", "fable"];

/// Pieces a suggestion may be made of to be accepted as a continuation, joined by `,`, `;`, `.`, ` and ` or ` then `.
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

/// Phrases a worker asks a person for a decision with.
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

/// Phrases a worker asks a person to choose with.
pub const CHOICE_PHRASES: &[&str] = &[
    "which option",
    "which would you prefer",
    "which do you prefer",
    "let me know which",
];

/// Words that make an offer, stop, choice or question one a person must answer.
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

/// Words that make a question an offer to go on (it counts as a continuation).
pub const OFFER_PHRASES: &[&str] = &[
    "want me to",
    "shall i",
    "should i continue",
    "should i proceed",
    "should i keep going",
    "should i go ahead",
    "should i also",
    "would you like me to",
    "next steps:",
    "next step:",
    "i can also",
    "if you'd like, i can",
    "let me know if you'd like",
    "happy to",
];

/// Words that recommend one option.
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

/// The busy work that makes a turn real work by default ([`TurnEndTiming::min_work`]).
pub const MIN_WORK: Duration = Duration::from_secs(2 * 60);

/// The policy's timings; tests shorten them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndTiming {
    pub min_work: Duration,
    pub short_backoff: Duration,
    pub short_backoff_max: Duration,
    pub retry_backoff: Vec<Duration>,
    pub net_backoff: Vec<Duration>,
    pub down_hold: Duration,
    pub progress: Duration,
    pub reset_grace: Duration,
    pub reset_jitter: Duration,
    pub auto_resume_grace: Duration,
    pub limit_backoff: Vec<Duration>,
    pub entitlement_backoff: Vec<Duration>,
    pub refused_backoff: Vec<Duration>,
    pub window: Duration,
    pub take_within: Duration,
    pub restart_backoff: Vec<Duration>,
    pub credits_bound: Duration,
}

const NO_AUTO_CONTINUE_GRACE: Duration = Duration::from_secs(60);

impl TurnEndTiming {
    /// The grace past a wall's reset for the program the wall is in.
    #[must_use]
    pub fn grace_for(&self, program: Program) -> Duration {
        if auto_continues(program) {
            self.reset_grace
        } else {
            self.reset_grace.min(NO_AUTO_CONTINUE_GRACE)
        }
    }
}

const fn auto_continues(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Gemini
        | Program::Generic => false,
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

/// THE LADDER'S DEVELOPMENT SEAM.
pub const LADDER_SCALE_SEAM: &str = "ATERM_DEV_LADDER_SCALE";

impl TurnEndTiming {
    /// The same timing, its short back-off ladder scaled by `factor`.
    #[must_use]
    pub fn ladder_scaled(mut self, factor: f64) -> Self {
        self.short_backoff = self.short_backoff.mul_f64(factor);
        self.short_backoff_max = self.short_backoff_max.mul_f64(factor);
        self
    }

    /// The factor [`LADDER_SCALE_SEAM`]'s value names.
    #[must_use]
    pub fn ladder_scale(raw: Option<&std::ffi::OsStr>) -> Option<f64> {
        let f: f64 = raw?.to_str()?.trim().parse().ok()?;
        (f.is_finite() && f > 0.0 && f <= 1.0).then_some(f)
    }

    /// Construct timing parameters for a session started by this build.
    #[must_use]
    pub fn for_build() -> Self {
        let shipped = Self::default();
        match Self::ladder_scale(aterm_types::dev_seam!(LADDER_SCALE_SEAM).as_deref()) {
            Some(factor) => shipped.ladder_scaled(factor),
            None => shipped,
        }
    }
}

/// The composer as the reading found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composer {
    Absent,
    Unknown,
    Empty,
    Placeholder(String),
    Typed,
    Own,
    Diverted,
}

impl Composer {
    /// Whether text stands in the composer: a draft, or aterm's own words.
    #[must_use]
    pub const fn drafted(&self) -> bool {
        matches!(self, Composer::Typed | Composer::Own | Composer::Diverted)
    }
}

/// Escalation reason while aterm's own words stand unsent in the prompt ([`Composer::Own`]).
pub const OWN_WORDS_STAND: &str =
    "aterm's own words stand unsent in the prompt; clear them and the session goes on";

/// Escalation reason while a draft stands over Claude Code's open completion menu ([`Composer::Diverted`]).
pub const MENU_DIVERTS: &str = "a draft stands with Claude Code's completion menu open: Enter \
     may run or complete the menu's highlighted entry, not send the draft; send or clear it \
     and the session goes on";

/// Suffix of an escalated question's reason when `answer_questions` holds it (`<why>; answer_questions is off`).
pub const ANSWERS_OFF: &str = "answer_questions is off";

/// What the loop read of the composer beyond its rows, for [`TurnEndReading::of`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerRead {
    pub typed: Option<bool>,
    pub menu_fg: Option<Vec<Option<String>>>,
}

/// USAGE CREDITS CARRY THE SESSION, by Claude Code's own notice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreditsCarry {
    pub reset_at: Option<Instant>,
    pub since: Option<Instant>,
    pub model: Option<String>,
    pub kept: bool,
}

/// The word of a model's family a credits notice names ([`CreditsCarry::model`]).
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

/// The conversation evidence of a real supervising context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conversation {
    Hostless,
    Unknown,
    Named(String),
}

impl Conversation {
    pub(crate) fn fresh_after(&self, before: &Self) -> Self {
        match (self, before) {
            (Self::Hostless, Self::Unknown | Self::Named(_)) => Self::Unknown,
            _ => self.clone(),
        }
    }

    pub(crate) const fn unread(host: bool, program: Program) -> Self {
        if host && !screen_alone(program) {
            Self::Unknown
        } else {
            Self::Hostless
        }
    }
}

#[must_use]
pub(crate) const fn reads_conversation(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Gemini
        | Program::Generic => false,
    }
}

#[must_use]
pub(crate) const fn screen_alone(program: Program) -> bool {
    match program {
        Program::Agy | Program::Muse | Program::Gemini => true,
        Program::Claude | Program::Codex | Program::Grok | Program::Kimi | Program::Generic => {
            false
        }
    }
}

#[must_use]
pub(crate) fn met_under_another(met: Option<Program>, here: Program) -> bool {
    let decided = |p: Program| reads_conversation(p) || screen_alone(p);
    met.is_some_and(|met| met != here && decided(met) && decided(here))
}

/// THE SEAT A SCREEN NAMES: the model and the login a screen-alone agent
/// draws ([`screen_alone`]) — agy's footer model and its launch card's login
/// row (`Gemini API key`, or the signed-in account), Gemini CLI's footer
/// `/model` value and its banner's login (`Authenticated with
/// gemini-api-key`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Seat {
    pub model: Option<String>,
    pub login: Option<String>,
}

impl Seat {
    /// The seat `program`'s screen `rows` name.
    #[must_use]
    pub fn of(program: Program, rows: &[String]) -> Self {
        match program {
            Program::Agy => Self {
                model: aterm_phase::agy::launch_card(rows).map(|c| c.model),
                login: aterm_phase::agy::launch_card_login(rows).map(str::to_owned),
            },
            Program::Gemini => Self {
                model: aterm_phase::gemini::footer_model(rows),
                login: aterm_phase::gemini::banner_login(rows),
            },
            Program::Claude
            | Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Muse
            | Program::Generic => Self::default(),
        }
    }

    /// The fields `later` names, each over this one's.
    fn update(&mut self, later: &Self) {
        if later.model.is_some() {
            self.model.clone_from(&later.model);
        }
        if later.login.is_some() {
            self.login.clone_from(&later.login);
        }
    }

    /// This seat with the fields `later` names over it.
    fn merged(&self, later: &Self) -> Self {
        let mut seat = self.clone();
        seat.update(later);
        seat
    }

    /// WHETHER `now` IS THIS SEAT'S ACCOUNT BY THE SCREEN'S OWN WORD: both
    /// name the login, alike, and it names an account — an address
    /// (`user@example.com (Google AI Pro)`, `Signed in with Google:
    /// user@example.com`) — never a key's label (`Gemini API key`,
    /// `Authenticated with gemini-api-key`), which every key draws alike: a
    /// relaunch under another key draws the same words.
    fn same_account(&self, now: &Self) -> bool {
        matches!((&self.login, &now.login), (Some(a), Some(b)) if a == b && a.contains('@'))
    }

    /// Whether `now` names another login than this seat, or — where the
    /// limit is a model's (`per_model`) — another model: both named, and
    /// different.
    fn moved(&self, now: &Self, per_model: bool) -> bool {
        let differs =
            |a: &Option<String>, b: &Option<String>| matches!((a, b), (Some(a), Some(b)) if a != b);
        differs(&self.login, &now.login) || (per_model && differs(&self.model, &now.model))
    }
}

/// WHETHER A LIMIT ON `program`'s SCREEN IS A MODEL'S: Gemini CLI's (`Usage
/// limit reached for <model>.`, its quota per model) and agy's (`You have
/// exhausted your quota on this model.`, its 429's `QuotaFailure` per
/// model).
const fn limit_per_model(program: Program) -> bool {
    match program {
        Program::Gemini | Program::Agy => true,
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Muse
        | Program::Generic => false,
    }
}

/// What one screen says, as the turn-end policy reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndReading {
    pub phase: Phase,
    pub authoritative: bool,
    pub survey: bool,
    pub wall: Option<WallKind>,
    pub wall_message: String,
    pub reset_at: Option<Instant>,
    pub reset_clock: Option<ResetClock>,
    pub jitter: Duration,
    pub resumes_by_itself: bool,
    pub composer: Composer,
    pub said_tail: Option<String>,
    pub said_cut: bool,
    pub opens_cut: bool,
    pub turn_said: Option<String>,
    pub opened_by: Option<u64>,
    pub message_drawn: bool,
    pub submits: Option<u64>,
    pub inputs_before: Option<u64>,
    pub screen_mark: Option<u64>,
    pub worked: Option<Duration>,
    pub worked_doubt: Duration,
    pub rules: Option<String>,
    /// The `aterm:` fact the act typed here may carry ([`FACT_RIDES`],
    /// [`crate::supervise::facts`]): the session's untold one, filled by the loop only at the
    /// final pass of an act it rides, with `[harness] facts` on; `None` everywhere else.
    pub fact: Option<crate::supervise::facts::Fact>,
    pub pending_input: bool,
    pub background: bool,
    pub interrupted: bool,
    pub restartable: bool,
    pub relaunch_off: bool,
    pub resume: Option<String>,
    pub upgrading: bool,
    pub taskless: bool,
    pub finished_record: Option<bool>,
    pub left_record: bool,
    pub left_work: Option<Duration>,
    pub login_back: bool,
    pub auto_continue: Option<aterm_phase::AutoContinue>,
    pub since_limit: Option<aterm_phase::SinceLimit>,
    pub served_in_transcript: bool,
    pub chosen_in_transcript: Option<String>,
    pub conversation: Conversation,
    pub notice_elsewhere: bool,
    pub wall_remembered: bool,
    pub reach: Reach,
    pub person: Option<Duration>,
    pub program: Program,
    /// The model and the login the screen names, where it is all the
    /// evidence ([`Seat::of`]): what a limit met under one of them binds.
    pub seat: Seat,
    pub account: Option<AccountAdvice>,
    pub autopilot: Autopilot,
    pub login_label: Option<String>,
    pub login_superseded: bool,
    pub login_notice: Option<u32>,
    pub login_screen: Option<aterm_phase::LoginScreen>,
    pub login_interrupted: bool,
    pub login_at_wall: Option<aterm_phase::LoginAtWall>,
    pub login_method: Option<aterm_phase::LoginMethod>,
    pub login_shared: Option<super::login::LoginShared>,
    pub roster: Option<RosterWord>,
    pub answered: Option<ScreenAnswer>,
    pub record_turn: Option<RecordTurn>,
    pub credits: Option<CreditsCarry>,
    pub repo: crate::harness::repo_work::RepoRead,
}

/// THE TURN A SCREEN'S LAST WORDS CLOSE ([`TurnEndReading::answered`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenAnswer {
    pub prompt: u64,
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

/// THE TURN THE CONVERSATION'S RECORD ENDS ON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordTurn {
    pub prompt: u64,
    pub age: Duration,
    pub ended: Option<Duration>,
}

impl RecordTurn {
    /// The record's last turn `t`, read at `now_ms` (unix milliseconds).
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

/// How much earlier than the act's own stamp the record may date the prompt that act typed ([`TurnEndState::observe`]'s fast answer).
pub const RECORD_SLACK: Duration = Duration::from_secs(5);

impl TurnEndReading {
    /// The advice the decider acts on ([`AccountAdvice::past_probe`] of [`Self::account`]), read the same at every deciding site.
    #[must_use]
    pub fn advised(&self) -> Option<&AccountAdvice> {
        self.account.as_ref().and_then(AccountAdvice::past_probe)
    }

    /// A person's hand in the turn that ended here.
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

    /// THE SCREEN SHOWS A STANDING WALL.
    #[must_use]
    pub const fn wall_drawn(&self) -> bool {
        self.wall.is_some() && !(self.wall_remembered && self.since_limit.is_none())
    }

    /// NOTHING OF A CONVERSATION STANDS.
    #[must_use]
    pub fn holds_no_conversation(&self) -> bool {
        self.phase == Phase::Idle
            && !self.message_drawn
            && self.worked.is_none()
            && self.said_tail.is_none()
            && self.turn_said.is_none()
            && self.opened_by.is_none()
            && self.answered.is_none()
            && !self.pending_input
            && !self.composer.drafted()
            && self.composer != Composer::Unknown
            && self.wall.is_none()
            && self.login_screen.is_none()
            && !self.interrupted
    }

    /// THE WORKER'S WORDS SINCE A USAGE LIMIT, where the screen orders them after it ([`limit_end`]).
    #[must_use]
    pub fn said_since_limit(&self) -> Option<&str> {
        let claude = match self.program {
            Program::Claude => true,
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Gemini
            | Program::Generic => false,
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

    /// What the worker asks of a person here and whether it names an irreversible act ([`Said::Irreversible`]).
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

    /// Reads one screen.
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

    /// [`Self::of`] where the machine slept `asleep` inside the turn the screen closes.
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
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Gemini
            | Program::Generic => false,
        };
        let own = reading
            .composer
            .as_ref()
            .is_some_and(|(_, lines)| crate::harness::upgrade::own_draft(&lines.concat()));
        let read = typed.into();
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
        let auto_continue = claude
            .then(|| aterm_phase::auto_continue_end(rows))
            .flatten();
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
            said_cut: reading.said_tail.is_some() && !reading.program.reader().said_whole(rows),
            opens_cut: reading.said_tail.is_some()
                && !reading.program.reader().bounds_messages()
                && !reading.program.reader().turn_whole(rows),
            turn_said: reading.program.reader().turn_said_before(rows),
            opened_by: reading
                .program
                .reader()
                .turn_opened_by(rows)
                .map(|t| crate::supervise::approvals::words_lead(&t)),
            message_drawn: reading.program.reader().message_drawn(rows),
            submits: None,
            inputs_before: None,
            screen_mark: Some(conversation_mark(reading, rows)),
            worked: worked.map(|w| less(w, asleep.most)),
            worked_doubt: worked.map_or(Duration::ZERO, |w| {
                less(w, asleep.least).saturating_sub(less(w, asleep.most))
            }),
            rules,
            fact: None,
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
            relaunch_off: false,
            resume: None,
            upgrading: false,
            taskless: reading.fresh,
            finished_record: None,
            left_record: false,
            left_work: None,
            person: None,
            login_back: claude && aterm_phase::login_restored(rows),
            auto_continue,
            since_limit: claude.then(|| aterm_phase::since_limit(rows)).flatten(),
            served_in_transcript: false,
            chosen_in_transcript: None,
            conversation: Conversation::Hostless,
            notice_elsewhere: false,
            wall_remembered: false,
            reach: Reach::Unknown,
            program: reading.program,
            seat: Seat::of(reading.program, rows),
            account: None,
            autopilot: Autopilot::Cruise,
            login_label: None,
            login_superseded: false,
            login_notice: claude
                .then(|| aterm_phase::login_expires_in(rows))
                .flatten(),
            login_screen: claude.then(|| aterm_phase::login_screen(rows)).flatten(),
            login_interrupted: claude && aterm_phase::login_interrupted(rows),
            login_at_wall: claude.then(|| aterm_phase::login_at_wall(rows)).flatten(),
            login_method: None,
            login_shared: None,
            roster: None,
            answered: claude.then(|| ScreenAnswer::of(rows)).flatten(),
            record_turn: None,
            credits: reading.credits.as_ref().map(|c| CreditsCarry {
                model: c.model.as_deref().and_then(credits_model),
                ..CreditsCarry::default()
            }),
            repo: crate::harness::repo_work::RepoRead::Clean,
        }
    }

    const fn at_point(&self, stale_input: bool) -> bool {
        matches!(
            self.phase,
            Phase::Idle | Phase::Question | Phase::Limited { .. }
        ) && self.authoritative
            && !self.survey
            && !matches!(self.composer, Composer::Absent | Composer::Unknown)
            && (!self.pending_input || stale_input)
    }

    const fn answered_point(&self) -> bool {
        matches!(self.phase, Phase::Idle | Phase::Question)
            && self.authoritative
            && !self.survey
            && self.wall.is_none()
            && !self.pending_input
            && !self.interrupted
            && matches!(self.composer, Composer::Empty | Composer::Placeholder(_))
    }
}

/// The turn running now, as the loop's busy reads track it (the busy-read twin of [`TurnEndReading::person_in_turn`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunningTurn {
    since: Option<Instant>,
}

impl RunningTurn {
    /// A busy read of the worker's own turn at `now`.
    pub fn busy(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    /// The point ending the turn, at `now`.
    pub fn point(&mut self, now: Instant) -> Option<Duration> {
        self.since.take().map(|t| now.saturating_duration_since(t))
    }
}

/// What a typed command leaves owed for the next point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    Nothing,
    Continue,
}

/// WHAT KIND OF WAIT A [`TurnEndAction::WaitUntil`] IS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitKind {
    Retry,
    BackOff { short: u8 },
    Other,
}

impl WaitKind {
    /// Whether the loop tells the host of a wait of this kind.
    #[must_use]
    pub const fn told(self) -> bool {
        !matches!(self, WaitKind::Other)
    }
}

/// What [`decide_turn_end`] says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndAction {
    Nothing,
    WaitUntil {
        until: Instant,
        why: String,
        wait: WaitKind,
    },
    Type {
        text: String,
        rule_id: &'static str,
    },
    Accept {
        text: String,
        rule_id: &'static str,
    },
    TypeCommand {
        command: String,
        rule_id: &'static str,
        then: Then,
    },
    Submit {
        rule_id: &'static str,
    },
    Confirm {
        rule_id: &'static str,
        row: String,
    },
    Escalate {
        reason: String,
    },
    Restart {
        why: Restart,
        rule_id: &'static str,
        otherwise: Box<TurnEndAction>,
    },
}

impl TurnEndAction {
    /// The rule an act is taken under (`None` for no act).
    #[must_use]
    pub const fn rule_id(&self) -> Option<&'static str> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallClass {
    Retry,
    Usage,
    Model,
    Context,
    Auth,
    Memory,
}

const fn class_of(kind: WallKind) -> WallClass {
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => WallClass::Retry,
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => WallClass::Usage,
        WallKind::ModelBucket { .. } => WallClass::Model,
        WallKind::Context => WallClass::Context,
        WallKind::Auth => WallClass::Auth,
        WallKind::Memory => WallClass::Memory,
    }
}

const fn named_class(class: WallClass) -> bool {
    matches!(class, WallClass::Usage | WallClass::Model)
}

/// THE LIMIT'S TRY [`TurnEndState::acted`] last recorded ([`TurnEndState::limit_try`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitTry {
    pub bucket: bool,
    pub in_place: bool,
    pub attempts: usize,
    pub entitlement: bool,
}

/// HOW FAR APART A LIMIT'S TRY AND ITS ANSWER STAND, at most, where the limit answered it at once ([`TurnEndState::date_unread_try`]).
pub const TRY_ANSWER_SLACK: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnreadTry {
    at: Instant,
    limit: LimitTry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WallTrack {
    class: WallClass,
    since: Instant,
    attempts: usize,
    ups: usize,
    exhausted: bool,
    fill: Option<ContextFill>,
    named: Option<NamedReset>,
    pointed: bool,
    said: String,
    kind: Option<WallKind>,
    seeded: bool,
    entitlement: bool,
    owed: bool,
    login: LoginBooks,
    conversation: Conversation,
    program: Option<Program>,
    /// THE AGENT RELAUNCHED UNDER THE LIMIT ([`TurnEndState::relaunch_startup`]).
    relaunch: RelaunchBooks,
    /// The seat the limit was met under ([`Seat`]: the model and login its
    /// screen named where the wall was read).
    seat: Seat,
    /// A later screen named another seat ([`Seat::moved`]), or the agent
    /// was relaunched on a seat nothing vouches for
    /// ([`TurnEndState::relaunch_startup`]): the limit binds the session no
    /// longer — its reset dropped, the point going at once — and it holds no
    /// host's step ([`TurnEndState::limit_holds_host`]).
    seat_moved: bool,
}

/// WHAT A LIMIT'S TRACK KEEPS OF ITS AGENT'S RELAUNCH ([`WallTrack::relaunch`],
/// [`TurnEndState::relaunch_startup`]): a limit met under an agent whose
/// screen is all its evidence ([`screen_alone`]: Muse Code, agy, Gemini CLI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RelaunchBooks {
    /// The last point of the limit's own program read while the track
    /// stood; `None`: none since the track opened ([`WallTrack::since`]).
    read: Option<Instant>,
    /// A point of no agent's was read since (a shell's prompt: the agent
    /// left the tab, crashed or upgraded), as no host's start says.
    away: bool,
    /// THE RELAUNCH'S START-UP STANDS: the agent's process came back since
    /// the limit was last read and no turn has run since — the limit is the
    /// account's still, whatever the new process drew (a fresh banner with no
    /// task included: [`TurnEndReading::taskless`] ends no such track).
    startup: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LoginBooks {
    cancelled: bool,
    hand: bool,
    prompt_at: Option<Instant>,
    prompt: Option<u64>,
    typed_at: Option<Instant>,
    back_typed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arrival {
    hand: bool,
    seen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NamedReset {
    kind: WallKind,
    at: Instant,
    jitter: Duration,
    by_itself: bool,
    said: Option<ResetClock>,
    first: Instant,
    written: Option<i64>,
}

impl NamedReset {
    fn served_under(&self, since: Option<&aterm_phase::SinceLimit>) -> bool {
        since.is_some_and(|s| s.served) && notice_of(since, self.kind, self.said)
    }

    fn continued_under(
        &self,
        since: Option<&aterm_phase::SinceLimit>,
        now: Option<Instant>,
    ) -> bool {
        self.said.is_none()
            || (notice_of(since, self.kind, self.said)
                && now.is_none_or(|now| later(now, FIRED_EARLY) >= self.at))
    }

    fn moved_off_under(&self, reading: &TurnEndReading, limited: &str) -> bool {
        let since = reading.since_limit.as_ref();
        let chosen = if notice_of(since, self.kind, self.said) {
            since
                .filter(|s| s.model_set)
                .and_then(|s| s.chosen_model.as_deref())
        } else if since.is_none() && self.said.is_some() {
            reading.chosen_in_transcript.as_deref()
        } else {
            None
        };
        chosen
            .and_then(bucket_model)
            .zip(bucket_model(limited))
            .is_some_and(|(chosen, limited)| chosen != limited)
    }
}

/// How long before a limit's reset.
pub const FIRED_EARLY: Duration = Duration::from_secs(60);

fn notice_of(
    since: Option<&aterm_phase::SinceLimit>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    since.is_some_and(|s| {
        let notice = if s.generic && named_class(class_of(kind)) {
            kind
        } else {
            s.kind
        };
        names_reset(notice, s.reset.as_deref(), kind, said)
    })
}

fn names_reset(
    notice: WallKind,
    reset: Option<&str>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    class_of(notice) == class_of(kind) && said.is_some() && ResetClock::of(reset) == said
}

/// THE LIMIT THIS POLICY WAITS ON, by the reset its notice named ([`TurnEndState::waited_limit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitedLimit {
    kind: WallKind,
    pub at: Instant,
    said: Option<ResetClock>,
}

impl WaitedLimit {
    /// Whether a limit notice of `kind` whose words name the reset `reset` names THIS limit's reset: of its class, the same clock.
    #[must_use]
    pub fn named_by(&self, kind: WallKind, reset: Option<&str>) -> bool {
        names_reset(kind, reset, self.kind, self.said)
    }
}

/// Return whether the screen's newest limit notice ended under a turn or retry.
#[must_use]
pub fn ended_under(
    since: Option<&aterm_phase::SinceLimit>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    since.is_some_and(|s| s.served || s.fired) && notice_of(since, kind, said)
}

const fn work_is_its_cause(kind: WallKind) -> bool {
    matches!(
        kind,
        WallKind::ApiError {
            cause: ApiCause::Overlong,
            ..
        }
    )
}

fn compaction_exhausted_row(message: &str) -> bool {
    let lower = message
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let anchor = |id: &str| aterm_phase::anchor(id).to_lowercase();
    lower.starts_with(&anchor("wall.compaction_failed"))
        || lower.starts_with(&anchor("wall.not_enough_to_compact"))
        || lower.contains(&anchor("wall.cannot_be_compacted"))
}

/// WHAT FILLS A FULL CONTEXT beyond the conversation's words, by the vendor's own notice (2.1.300).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFill {
    Images,
    Attachments,
}

impl ContextFill {
    /// The fill notice `message` names, by the anchor table's words.
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
    pub from: Option<String>,
    pub to: String,
    pub back_at: Option<Instant>,
}

/// How far past a turn's busy work a person's keystroke still counts as their hand in it.
pub const PERSON_TURN_SLACK: Duration = Duration::from_secs(30);

/// The longest answer that is still a person's own exchange ([`TurnEndState::left_to_person`]).
pub const EXCHANGE_WORK: Duration = Duration::from_secs(10 * 60);

/// A line the occupancy door typed at a break ([`TurnEndState::fact_typed`]): whether a read has
/// seen the agent take it ([`TurnEndState::took`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FactAwaited {
    taken: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Awaited {
    rule: &'static str,
    command: bool,
    at: Instant,
    typed: Instant,
    unseen_since: Option<Instant>,
    taken: bool,
    met: bool,
    left: Option<LeftKey>,
    host: Option<HostTurn>,
    off_stood: bool,
    wall_retry: bool,
    said: Option<u64>,
    over: Option<u64>,
    lead: Option<u64>,
    opened: Option<u64>,
    submitted: bool,
    submits: Option<u64>,
    screen: Option<u64>,
}

impl Awaited {
    fn submitted_since(&self, reading: &TurnEndReading) -> bool {
        matches!((reading.submits, self.submits), (Some(now), Some(then)) if now > then)
            && reading.screen_mark.is_some()
            && reading.screen_mark != self.screen
    }
}

type LeftKey = (WallClass, Option<ResetClock>);

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

/// A limit a cancel left, read as the answer to a turn asked since the policy last saw the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftAnswer {
    pub wall: aterm_phase::Wall,
    pub since: Instant,
    pub screen_tells: bool,
}

impl Awaited {
    fn due(&self, timing: &TurnEndTiming) -> Instant {
        later(self.unseen_since.unwrap_or(self.at), timing.take_within)
    }
}

/// THE FIRST READ AFTER AN ACT A LOOP BEFORE THIS ONE TYPED, as its ledger shows it ([`TurnEndState::seed_awaited`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRead {
    Read(Duration),
    Unread,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetResume {
    told: Instant,
    due: Instant,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LandEpisode {
    typed: bool,
    nudges: u32,
    hold: HoldStart,
    unstamped: bool,
    legs: Option<LandHeld>,
}

/// WHERE THE HOLD BEGAN in a Land episode ([`TurnEndState::land_held`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoldStart {
    pub since_ms: Option<i64>,
    pub base: u32,
    pub relanded: bool,
}

impl HoldStart {
    /// The hold a landing leaves: none, until Land's next act begins one.
    pub const LANDED: HoldStart = HoldStart {
        since_ms: None,
        base: 0,
        relanded: true,
    };
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Needs {
    standing: Vec<String>,
    key: u64,
    acked: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CreditsEpisode {
    reset: Option<Instant>,
    since: Option<Instant>,
    model: Option<String>,
    passed: Option<Instant>,
    told: bool,
}

const SAME_CREDITS_RESET: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreditsHeld {
    reset: Option<Instant>,
    model: Option<String>,
    lifts: Option<Instant>,
    told: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactOwes {
    check: bool,
    land: bool,
}

/// What the policy remembers of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndState {
    pub timing: TurnEndTiming,
    login_confirmed: bool,
    acts: VecDeque<Instant>,
    wall_acts: VecDeque<(Instant, &'static str)>,
    awaiting: Option<Awaited>,
    /// The occupancy door's line at a break ([`Self::fact_typed`]), beside the act awaited.
    fact: Option<FactAwaited>,
    followed: Option<&'static str>,
    point_worked: Duration,
    short: u32,
    point_at: Option<Instant>,
    points: u64,
    done_at: Option<Instant>,
    esc_at: Option<Instant>,
    wall: Option<WallTrack>,
    notice_typed: Option<String>,
    agent_start: Option<Instant>,
    agent_start_unix: Option<i64>,
    after: Option<&'static str>,
    model: Option<ModelSwitch>,
    fallback_unmade: bool,
    fallback_unasked: bool,
    fallback_unix: Option<i64>,
    fallback_started: Option<i64>,
    notice_unix: Option<i64>,
    account: Option<AccountMove>,
    account_asked: bool,
    account_told: bool,
    account_startup: bool,
    account_resume: bool,
    account_resume_seed: bool,
    account_resumed: Option<i64>,
    limit_try: Option<LimitTry>,
    limit_ended: bool,
    /// That end was ANOTHER PROGRAM'S SCREEN read ([`met_under_another`],
    /// [`Self::limit_ended_elsewhere`]).
    limit_ended_elsewhere: bool,
    /// THE SEAT THE TAB'S SCREEN-ALONE AGENT LAST NAMED ([`Seat`], each field
    /// kept from the last read that drew it; [`Self::seat_seen`]): the login
    /// its launch card or banner drew at the session's start, which scroll
    /// off long before a limit is met, is the one a limit met later binds.
    seat_seen: Option<(Program, Seat)>,
    reset_resume: Option<ResetResume>,
    account_unmade: Option<Unmade>,
    account_unmade_seed: Option<Unmade>,
    account_unmade_ms: Option<i64>,
    account_ahead_unmade: Option<(String, Instant)>,
    account_moves: VecDeque<Instant>,
    stale_input: bool,
    done: u8,
    checking: bool,
    seeded_check: Option<&'static str>,
    check_point_seed: Option<&'static str>,
    seeded_land: Option<&'static str>,
    land_point_seed: bool,
    open: Vec<&'static str>,
    repo: crate::harness::repo_work::RepoRead,
    needs: Needs,
    autopilot: Option<Autopilot>,
    land: LandEpisode,
    landing: bool,
    landing_act: Option<(u64, u64, Instant)>,
    landed: bool,
    landed_mark: Option<u64>,
    off_landing: Option<Option<u64>>,
    point_mark: Option<u64>,
    point_mark_len: usize,
    point_opener: Option<u64>,
    point_submits: Option<u64>,
    point_screen: Option<u64>,
    point_tasked: bool,
    point_inputs: Option<u64>,
    point_clean: bool,
    last_prompt: Option<u64>,
    cut_check: bool,
    cut_land: bool,
    cut_text: Option<String>,
    act_words: Option<String>,
    compact_owes: Option<CompactOwes>,
    managed: bool,
    cut_reply: bool,
    exchange: bool,
    exchange_work: Duration,
    carrying: bool,
    carry_at_attach: bool,
    looked: Option<(Instant, Option<LeftKey>)>,
    other_turn: bool,
    quiet_wake: bool,
    wake_repo: Option<crate::harness::repo_work::RepoRead>,
    landed_seed: Option<bool>,
    landed_words: Option<u64>,
    seed_by_wake: bool,
    by_wake: bool,
    wake_restated: bool,
    seed_noted_ms: Option<i64>,
    restating: Option<(TaskFace, i64)>,
    face_told: Option<TaskFace>,
    held_told: Option<Option<LandHeld>>,
    try_over: bool,
    unread_try: Option<UnreadTry>,
    seed_answered: bool,
    point_seed: Option<Instant>,
    ran_turn_seed: Option<(Instant, Instant)>,
    esc_seed: bool,
    point_moved: bool,
    login_returned: bool,
    login_seed: Option<Instant>,
    login_retaken: bool,
    yield_seed: Option<bool>,
    left_judged: bool,
    point_last: bool,
    ran: bool,
    gap: bool,
    credits: Option<CreditsEpisode>,
}

impl Default for TurnEndState {
    fn default() -> Self {
        Self::new(TurnEndTiming::default())
    }
}

impl TaskFace {
    /// THE TASK'S FACE, a pure function of the policy's memory, taken in order.
    #[must_use]
    pub fn of(state: &TurnEndState, other_turn: bool) -> TaskFace {
        if other_turn {
            return TaskFace::Open;
        }
        if state.left_to_person() {
            return TaskFace::Theirs;
        }
        if state.landed() || state.landed_seed.is_some() {
            TaskFace::Landed
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
            fact: None,
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
            limit_ended_elsewhere: false,
            seat_seen: None,
            reset_resume: None,
            account_unmade: None,
            account_unmade_seed: None,
            account_unmade_ms: None,
            account_ahead_unmade: None,
            account_moves: VecDeque::new(),
            stale_input: false,
            done: 0,
            checking: false,
            seeded_check: None,
            check_point_seed: None,
            seeded_land: None,
            land_point_seed: false,
            open: Vec::new(),
            repo: crate::harness::repo_work::RepoRead::Clean,
            needs: Needs::default(),
            autopilot: None,
            land: LandEpisode::default(),
            landing: false,
            landing_act: None,
            landed: false,
            landed_mark: None,
            off_landing: None,
            point_mark: None,
            point_mark_len: 0,
            point_opener: None,
            point_submits: None,
            point_screen: None,
            point_tasked: false,
            point_inputs: None,
            point_clean: false,
            last_prompt: None,
            cut_check: false,
            cut_land: false,
            cut_text: None,
            act_words: None,
            compact_owes: None,
            managed: false,
            cut_reply: false,
            exchange: false,
            exchange_work: Duration::ZERO,
            carrying: false,
            carry_at_attach: false,
            looked: None,
            other_turn: false,
            quiet_wake: false,
            wake_repo: None,
            landed_seed: None,
            landed_words: None,
            seed_by_wake: false,
            by_wake: false,
            wake_restated: false,
            seed_noted_ms: None,
            restating: None,
            face_told: None,
            held_told: None,
            try_over: false,
            unread_try: None,
            seed_answered: false,
            point_seed: None,
            ran_turn_seed: None,
            esc_seed: false,
            point_moved: false,
            login_returned: false,
            login_seed: None,
            login_retaken: false,
            yield_seed: None,
            left_judged: false,
            point_last: true,
            ran: false,
            gap: false,
            credits: None,
        }
    }

    /// A model switch a previous loop on the session made and did not undo (the approval ledger's open `/model` row).
    pub fn seed_model(&mut self, switch: ModelSwitch) {
        if self.model.is_none() {
            self.model = Some(switch);
        }
    }

    /// The account moves a previous loop on the session made, `ages` ago (the approval ledger's `typed` rows of [`RULE_ACCOUNT`]).
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

    /// An account move a previous loop made.
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

    /// An account move a previous loop asked for could not be made.
    pub fn seed_account_unmade(&mut self, unmade: Unmade, written_ms: Option<i64>) {
        self.account_unmade = Some(unmade);
        self.account_unmade_seed = Some(unmade);
        self.account_unmade_ms = written_ms;
        self.drop_gone_launch_latch();
    }

    fn drop_gone_launch_latch(&mut self) {
        let gone = matches!(self.account_unmade, Some(Unmade::NoShell | Unmade::Launch))
            && self
                .account_unmade_ms
                .zip(self.agent_start_unix)
                .is_some_and(|(written, start)| written < start.saturating_mul(1000));
        if gone {
            self.account_unmade = None;
            self.account_unmade_seed = None;
            self.account_unmade_ms = None;
        }
    }

    /// Acts a previous loop on the session typed, `ages` ago (the approval ledger's `typed` rows).
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
        self.carrying |= !self.acts.is_empty();
    }

    /// THE CARRY THE SEEDED ACTS GAVE IS OVER (`crate::supervise::approvals::carry_ended_in`).
    pub const fn seed_carry_over(&mut self) {
        self.carrying = false;
    }

    /// Seed a usage-resume attempt recorded by a previous supervisor loop.
    pub fn seed_usage_attempt(&mut self, ago: Duration, limit: LimitTry, now: Instant) {
        self.seed_try(ago, limit, false, now);
    }

    /// THE TRY A LOOP BEFORE THIS ONE MADE, MET AGAIN.
    pub fn seed_met_attempt(&mut self, ago: Duration, limit: LimitTry, now: Instant) {
        self.seed_try(ago, limit, true, now);
    }

    fn seed_try(&mut self, ago: Duration, limit: LimitTry, met: bool, now: Instant) {
        if self.wall.is_some() {
            return;
        }
        let at = now.checked_sub(ago).unwrap_or(now);
        if !met {
            self.unread_try = Some(UnreadTry { at, limit });
        }
        let next = if limit.entitlement {
            crate::supervise::ladder::Ladder(&self.timing.entitlement_backoff).at(limit.attempts)
        } else {
            self.timing.limit_backoff.last().copied()
        };
        if !met && next.is_none_or(|next| ago >= next) {
            self.try_over = next.is_some();
            return;
        }
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            class: if limit.bucket {
                WallClass::Model
            } else {
                WallClass::Usage
            },
            since: at,
            attempts: limit.attempts.max(1),
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: String::new(),
            kind: None,
            seeded: !met,
            entitlement: limit.entitlement,
            owed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program: None,
        });
    }

    /// THE RETRY LADDER OF THE API ERROR'S WALL THIS LOOP WAITS AT.
    #[must_use]
    pub fn retry_track(&self) -> Option<(WallKind, usize, usize, Instant)> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Retry)
            .and_then(|t| Some((t.kind?, t.attempts, t.ups, t.since)))
    }

    /// THE RETRY LADDER A LOOP BEFORE THIS ONE WAS ON at the API error's wall `kind` ([`Self::retry_track`], its last row's mark).
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
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            class: WallClass::Retry,
            since: now.checked_sub(ago).unwrap_or(now),
            attempts,
            ups,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: String::new(),
            kind: Some(kind),
            seeded: false,
            entitlement: false,
            owed,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program: None,
        });
    }

    /// THE `/login` A LOOP BEFORE THIS ONE TYPED, `ago` ago, as the ledger says it.
    pub fn seed_login(&mut self, ago: Duration, now: Instant) {
        self.login_seed = Some(now.checked_sub(ago).unwrap_or(now));
    }

    /// Record or adopt an in-progress `/login` attempt at a lost-login wall.
    pub fn take_up_login(
        &mut self,
        at_wall: Option<&aterm_phase::LoginAtWall>,
        interrupted: bool,
        now: Instant,
    ) -> bool {
        let Some(at_wall) = at_wall else {
            return false;
        };
        if self
            .wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Auth)
        {
            return false;
        }
        let typed_at = self.login_seed.take();
        let cancelled = interrupted || at_wall.left_before;
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            class: WallClass::Auth,
            since: now,
            attempts: usize::from(typed_at.is_some()),
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: at_wall.message.clone(),
            kind: Some(WallKind::Auth),
            seeded: false,
            entitlement: false,
            owed: false,
            conversation: Conversation::Hostless,
            program: None,
            login: LoginBooks {
                cancelled,
                typed_at,
                ..LoginBooks::default()
            },
        });
        self.login_retaken |= typed_at.is_some() && !cancelled;
        true
    }

    /// Seed a named usage-limit or model-bucket reset wait from a previous loop.
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
        let (since, attempts, named, program) = match &self.wall {
            Some(t) if t.class == class => {
                (t.since, t.attempts, t.named.unwrap_or(named), t.program)
            }
            _ => (since, 0, named, None),
        };
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            class,
            since,
            attempts,
            ups: 0,
            exhausted: false,
            fill: None,
            named: Some(named),
            pointed: true,
            said: message.to_string(),
            kind: None,
            seeded: false,
            entitlement: false,
            owed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program,
        });
    }

    /// A LIMIT WHOSE NOTICE PLACED NO RESET a previous loop on the session waited on and no loop has ended.
    pub fn seed_unnamed_wall(&mut self, kind: WallKind, message: &str, since: Instant) {
        let class = class_of(kind);
        if self.wall.is_some() || self.try_over || !named_class(class) {
            return;
        }
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            class,
            since,
            attempts: 0,
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: message.to_string(),
            kind: Some(kind),
            seeded: false,
            entitlement: entitlement_ladder(false, kind),
            owed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program: None,
        });
    }

    /// AN ACT A LOOP BEFORE THIS ONE TYPED IS STILL INSIDE ITS TAKE WINDOW (typed `ago` ago, under [`TurnEndTiming::take_within`]).
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
            lead: None,
            opened: None,
            submitted: false,
            submits: None,
            screen: None,
        });
        true
    }

    /// THE DONE CHECK A LOOP BEFORE THIS ONE ASKED IS OWED ITS ANSWER ([`crate::supervise::approvals::done_check_owed`]).
    pub const fn seed_done_check(&mut self, rule: &'static str, cut: bool, answered: bool) {
        self.seeded_check = Some(rule);
        if cut {
            self.cut_check = true;
        } else {
            self.checking = true;
            self.seed_answered |= answered;
        }
    }

    /// THE DONE CHECK A FULL CONTEXT CUT.
    pub const fn seed_compact_check(&mut self, rule: &'static str) {
        self.seeded_check = Some(rule);
        self.compact_owes = Some(CompactOwes {
            check: true,
            land: false,
        });
        self.after = Some(RULE_COMPACT);
    }

    /// THE CHECK WHOSE ANSWER THE POINT A LOOP BEFORE THIS ONE JUDGED LAST WAS.
    pub const fn seed_check_point(&mut self, rule: &'static str) {
        self.check_point_seed = Some(rule);
    }

    /// THE LAND ACT A LOOP BEFORE THIS ONE TYPED.
    pub const fn seed_land_owed(&mut self, rule: &'static str) {
        self.seeded_land = Some(rule);
    }

    /// THE LAND ACT A FULL CONTEXT CUT.
    pub const fn seed_compact_land(&mut self) {
        self.compact_owes = Some(CompactOwes {
            check: false,
            land: true,
        });
        self.after = Some(RULE_COMPACT);
    }

    /// THE LAND ACT WHOSE ANSWER THE POINT A LOOP BEFORE THIS ONE JUDGED LAST WAS.
    pub const fn seed_land_point(&mut self) {
        self.land_point_seed = true;
    }

    fn leave_land_episode(&mut self) {
        self.land = LandEpisode::default();
        self.landing = false;
        self.landing_act = None;
        self.cut_land = false;
        self.seeded_land = None;
        self.land_point_seed = false;
        if self.cut_text.as_deref().is_some_and(is_land_words) {
            self.cut_text = None;
        }
        self.compact_owes = self.compact_owes.and_then(|mut owed| {
            owed.land = false;
            owed.check.then_some(owed)
        });
    }

    /// A TURN WAS READ RUNNING after the point a loop before this one recorded last ([`crate::supervise::approvals::StreakSeed::ran`]).
    pub fn seed_ran(&mut self) {
        if std::mem::take(&mut self.seed_answered) {
            self.checking = false;
            self.landing = false;
        }
        self.gap = true;
    }

    /// THE LIMIT'S TRY A LOOP BEFORE THIS ONE TYPED LAST WAS TAKEN.
    pub const fn seed_try_taken(&mut self) {
        self.gap = true;
    }

    pub(crate) fn take_gap(&mut self) -> bool {
        std::mem::take(&mut self.gap) && self.awaiting.is_none()
    }

    pub(crate) const fn launched_since_books(&mut self) {
        self.gap = false;
        self.ran_turn_seed = None;
    }

    /// …AND WHEN.
    pub fn seed_ran_turn(&mut self, ran_ago: Duration, point_ago: Duration, now: Instant) {
        self.ran_turn_seed = Some((
            now.checked_sub(ran_ago).unwrap_or(now),
            now.checked_sub(point_ago).unwrap_or(now),
        ));
    }

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

    /// THE LAND EPISODE A LOOP BEFORE THIS ONE HAD IN HAND ([`crate::supervise::approvals::land_episode`]).
    pub fn seed_land(&mut self, nudges: u32, landing: bool, cut: bool, answered: bool) {
        self.land = LandEpisode {
            typed: true,
            nudges,
            ..LandEpisode::default()
        };
        self.landing = landing;
        self.cut_land = cut;
        self.seed_answered |= landing && answered;
    }

    /// …AND THAT EPISODE'S LAST ACT, as the ledger holds it ([`crate::supervise::approvals::LandAct`]).
    pub fn seed_land_act(&mut self, said: u64, lead: u64, ago: Duration, now: Instant) {
        self.landing_act = Some((said, lead, now.checked_sub(ago).unwrap_or(now)));
    }

    /// A LAND ACT A LOOP BEFORE THIS ONE TYPED IS OWED ITS ANSWER at this state's first point ([`Self::seed_land_act`]).
    #[must_use]
    pub const fn land_act_seeded(&self) -> bool {
        self.landing && self.landing_act.is_some() && self.point_at.is_none()
    }

    /// THE TURN ENDING AT `r` IS OWED ITS WHOLE WORDS, whatever its last ones claim.
    #[must_use]
    pub fn reads_turn_whole(&self, r: &TurnEndReading, worked: bool) -> bool {
        let first_point = self.point_at.is_none() && !r.program.reader().bounds_messages();
        if !(worked || first_point) || matches!(r.phase, Phase::Busy | Phase::Prompt) {
            return false;
        }
        let folds_needs = r.wall.is_none() && !r.pending_input;
        let keeps_landing =
            r.finished_record == Some(true) && (self.landed() || self.landed_seed.is_some());
        folds_needs || keeps_landing
    }

    /// WHETHER THE WORKER'S LAST WORDS `tail` ARE A LANDING'S OWN, READ AGAIN.
    #[must_use]
    pub fn landing_words_read(&self, tail: Option<&str>) -> bool {
        let mark = said_mark(tail);
        mark.is_some()
            && self.awaiting.is_none()
            && ((self.landed_seed == Some(true) && self.landed_words == mark)
                || (self.landed() && self.off_landing.is_none() && self.landed_mark == mark))
    }

    /// Seed the consecutive short-turn streak recorded by a previous loop.
    pub const fn seed_short(&mut self, short: u32) {
        self.short = short;
        self.point_last = false;
    }

    /// THE STREAK SEEDED WAS RECORDED AT A POINT a loop before this one judged `ago` ago ([`crate::supervise::approvals::RULE_POINT`]).
    pub fn seed_point(&mut self, ago: Duration, now: Instant) {
        self.point_seed = Some(now.checked_sub(ago).unwrap_or(now));
        self.point_last = true;
    }

    /// A PERSON'S ESC STOOD AT THE POINT SEEDED ([`Self::seed_point`]), as its record says (`approvals::StreakSeed::esc`).
    pub const fn seed_esc(&mut self) {
        self.esc_seed = true;
    }

    /// Whether a person's Esc stands at the point now ([`Self::esc_at`]).
    #[must_use]
    pub const fn persons_esc_stands(&self) -> bool {
        self.esc_at.is_some()
    }

    /// THE STREAK SEEDED IS THE ONE THE ACT `rule` A LOOP BEFORE THIS ONE TYPED LAST WAS TYPED AFTER.
    pub fn seed_yield(&mut self, rule: &str) {
        self.yield_seed = Some(is_continue_rule(rule));
    }

    pub(crate) fn take_point_moved(&mut self) -> bool {
        let moved = std::mem::take(&mut self.point_moved);
        self.point_last |= moved;
        moved
    }

    pub(crate) fn take_login_returned(&mut self) -> bool {
        std::mem::take(&mut self.login_returned)
    }

    pub(crate) fn take_login_retaken(&mut self) -> bool {
        std::mem::take(&mut self.login_retaken)
    }

    pub(crate) fn take_ran(&mut self) -> bool {
        std::mem::take(&mut self.ran)
    }

    #[must_use]
    pub(crate) const fn left_judged(&self) -> bool {
        self.left_judged
    }

    pub(crate) fn take_try_over(&mut self) -> bool {
        std::mem::take(&mut self.try_over)
    }

    /// WHEN THE LIMIT'S TRY NO LOOP HAS READ WAS TYPED (on the policy's clock, [`UnreadTry::at`]), while this loop has not read it either.
    #[must_use]
    pub fn unread_try(&self) -> Option<Instant> {
        self.unread_try
            .map(|u| u.at)
            .filter(|&at| self.awaits_unread(at))
    }

    fn awaits_unread(&self, at: Instant) -> bool {
        self.awaiting.is_some_and(|a| {
            a.wall_retry && !a.taken && !a.met && a.unseen_since.is_none() && a.typed == at
        })
    }

    /// THE ANSWER OF A TRY NO LOOP READ.
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
        let program = self.wall.as_ref().and_then(|t| t.program);
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            class,
            since: later(read, self.timing.take_within),
            attempts: u.limit.attempts.max(1),
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: String::new(),
            kind: None,
            seeded: false,
            entitlement: u.limit.entitlement,
            owed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program,
        });
        Some(read)
    }

    pub(crate) fn seed_limit_conversation(
        &mut self,
        conversation: Conversation,
        program: Option<Program>,
    ) {
        if let Some(t) = self.wall.as_mut().filter(|t| named_class(t.class)) {
            t.conversation = conversation.fresh_after(&t.conversation);
            t.program = program.or(t.program);
        }
    }

    pub(crate) fn rescope_limit_conversation(&mut self, from: &Conversation, to: &Conversation) {
        if let Some(t) = self.wall.as_mut().filter(|t| named_class(t.class))
            && t.conversation == *from
        {
            t.conversation = to.clone();
        }
    }

    pub(crate) fn limit_conversation(&self) -> Option<&Conversation> {
        self.wall
            .as_ref()
            .filter(|t| named_class(t.class))
            .map(|t| &t.conversation)
    }

    pub(crate) fn limit_program(&self) -> Option<Program> {
        self.wall
            .as_ref()
            .filter(|t| named_class(t.class))
            .and_then(|t| t.program)
    }

    pub(crate) fn has_limit_track(&self) -> bool {
        self.wall.as_ref().is_some_and(|t| named_class(t.class))
    }

    /// THE SEAT `program`'s SCREEN NAMES NOW ([`WallTrack::seat`]): the
    /// fields `here` (this screen's [`Seat::of`]) names, over those the tab's
    /// earlier screens of the same agent named ([`TurnEndState::seat_seen`]:
    /// the launch card's or banner's login, read at the session's start and
    /// scrolled off since).
    #[must_use]
    pub(crate) fn seat_now(&self, program: Program, here: &Seat) -> Seat {
        match &self.seat_seen {
            Some((seen_by, seen)) if *seen_by == program => seen.merged(here),
            _ => here.clone(),
        }
    }

    /// WHETHER THE SCREEN `here` OF `program`'s RELAUNCHED PROCESS NAMES THE
    /// ACCOUNT THE TAB'S EARLIER SCREENS NAMED ([`Seat::same_account`]): the
    /// seat vouched for across a relaunch nobody here made.
    #[must_use]
    pub(crate) fn seat_vouched(&self, program: Program, here: &Seat) -> bool {
        self.seat_seen
            .as_ref()
            .is_some_and(|(seen_by, seen)| *seen_by == program && seen.same_account(here))
    }

    /// The seat `reading`'s screen names, kept ([`TurnEndState::seat_seen`]):
    /// a screen-alone agent's fields over what the tab's earlier screens of
    /// it named; another agent's screen replaces it (its seat is none of
    /// this one's); a screen no reader names (a shell, a frame before the
    /// agent draws) keeps it.
    fn note_seat(&mut self, reading: &TurnEndReading) {
        let program = reading.program;
        if program == Program::Generic {
            return;
        }
        if !screen_alone(program) {
            self.seat_seen = None;
            return;
        }
        match &mut self.seat_seen {
            Some((seen_by, seen)) if *seen_by == program => seen.update(&reading.seat),
            _ => self.seat_seen = Some((program, reading.seat.clone())),
        }
    }

    /// WHETHER A LIMIT THIS POLICY WAITS ON HOLDS THE HOST'S STEPS at `now`
    /// (`Session::host_steps_here`, `host_steps_in_background`: a relaunch's
    /// carry-on, an upgrade's notice): a usage limit, spend limit or model
    /// bucket ([`named_class`]) met under an agent whose screen is all its
    /// evidence ([`screen_alone`]), its track standing — through the
    /// agent's relaunch ([`Self::relaunch_startup`]), where the loop's own
    /// limit episode closed on the relaunched screen — until `grace` past the
    /// reset it named, and for good where it names none (a reset is never
    /// guessed: the policy's own tries, or a person's turn, end it).
    #[must_use]
    pub fn limit_holds_host(&self, now: Instant, grace: Duration) -> bool {
        let Some(t) = self.wall.as_ref().filter(|t| named_class(t.class)) else {
            return false;
        };
        if !t.program.is_some_and(screen_alone) || t.seat_moved {
            return false;
        }
        t.named.is_none_or(|n| now < later(n.at, grace))
    }

    /// Whether a usage limit or a model bucket whose notice named its reset is waited on ([`named_class`]).
    #[must_use]
    pub fn waits_on_named_reset(&self) -> bool {
        self.waited_limit().is_some()
    }

    /// The standing retry wall's kind when its notice left the screen ([`left_retry_kind`]).
    #[must_use]
    pub(crate) fn standing_retry_wall(&self) -> Option<WallKind> {
        let retry_taken = self.awaiting.is_some_and(|a| a.wall_retry && a.taken);
        self.wall
            .as_ref()
            .filter(|t| !retry_taken && !t.owed)
            .and_then(left_retry_kind)
    }

    fn limit_elsewhere(&self, reading: &TurnEndReading) -> bool {
        if self.wall.as_ref().is_some_and(|t| {
            matches!((&t.conversation, &reading.conversation),
                (Conversation::Named(ours), Conversation::Named(theirs)) if ours != theirs)
        }) {
            return true;
        }
        let Some(n) = self.waited_limit() else {
            return false;
        };
        reading.notice_elsewhere
            || (n.said.is_some()
                && reading.since_limit.is_some()
                && !notice_of(reading.since_limit.as_ref(), n.kind, n.said))
    }

    /// THE CLOCK THE LIMIT THIS POLICY WAITS ON NAMED, while its reset is still ahead of `now`.
    #[must_use]
    pub fn named_reset_clock(&self, now: Instant) -> Option<ResetClock> {
        let n = self.wall.as_ref().filter(|t| named_class(t.class))?.named?;
        n.said.filter(|_| n.at > now)
    }

    /// The limit [`Self::waits_on_named_reset`] waits on.
    #[must_use]
    pub fn waited_limit(&self) -> Option<WaitedLimit> {
        let n = self.wall.as_ref().filter(|t| named_class(t.class))?.named?;
        Some(WaitedLimit {
            kind: n.kind,
            at: n.at,
            said: n.said,
        })
    }

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
            (t.class == WallClass::Model
                && limit_conversation_ours(t, &reading)
                && t.named
                    .is_some_and(|n| n.moved_off_under(&reading, &t.said)))
                || (named_class(t.class)
                    && (reading.login_back
                        || reading.login_interrupted
                        || reading.login_screen.is_some()))
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

    /// Seed a post-rotation usage-resume timestamp recorded by a previous loop.
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

    /// When the typing budget `cap` (`continue_per_hour`) next allows an act.
    #[must_use]
    pub fn budget_frees_at(&self, cap: u32, now: Instant) -> Instant {
        let window = self.timing.window;
        let mut within: Vec<Instant> = self
            .acts
            .iter()
            .copied()
            .filter(|&t| now.saturating_duration_since(t) < window)
            .collect();
        within.sort_unstable();
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        if cap == 0 || within.len() < cap {
            return now;
        }
        within
            .get(within.len() - cap)
            .map_or(now, |&t| (t + window).max(now))
    }

    /// How many acts of wall rule `rule` this policy typed within the window ending at `now`, and when the oldest of them leaves it.
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

    /// Short turns in a row.
    #[must_use]
    pub const fn short_streak(&self) -> u32 {
        self.short
    }

    /// Where the task stands.
    #[must_use]
    pub const fn done_reports(&self) -> u8 {
        self.done
    }

    /// Whether the conversation's record is read at `r` ([`TurnEndReading::finished_record`]).
    #[must_use]
    pub fn reads_record(&self, r: &TurnEndReading) -> bool {
        self.off_landing
            .is_some_and(|mark| said_mark(r.said_tail.as_deref()) != mark)
            || r.worked.is_some()
            || r.auto_continue == Some(aterm_phase::AutoContinue::Fired)
            || self.point_at.is_none()
            || (r.wall.is_none() && self.wall.as_ref().is_some_and(|t| named_class(t.class)))
            || self.untaken().is_some()
    }

    /// THE TASK IS DONE.
    #[must_use]
    pub const fn task_done(&self) -> bool {
        self.done >= 2
    }

    /// A LOST LOGIN'S `/login` WAS LEFT BEFORE IT SIGNED IN (`⎿  Login interrupted` read while its
    /// track stands, [`LoginBooks::cancelled`]).
    #[must_use]
    pub fn login_cancelled(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Auth && t.login.cancelled)
    }

    /// A PERSON'S `/login` AT A USAGE OR MODEL LIMIT WAS LEFT BEFORE IT SIGNED IN (`⎿  Login interrupted` read while the limit's track stands).
    #[must_use]
    pub fn login_cancelled_on_limit(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| named_class(t.class) && t.login.cancelled)
    }

    /// WHEN THIS LOOP TYPED `/login` INTO THE LOST LOGIN STANDING ([`LoginBooks::typed_at`]), its sign-in not left.
    #[must_use]
    pub fn login_typed_at(&self) -> Option<Instant> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Auth && t.attempts > 0 && !t.login.cancelled)
            .and_then(|t| t.login.typed_at)
    }

    /// UNTIL WHEN THE `/login` THIS LOOP TYPED MAY STILL BE OPENING ITS DIALOG ([`TurnEndTiming::take_within`] from the keys).
    #[must_use]
    pub fn login_opening(&self, now: Instant) -> Option<Instant> {
        self.login_typed_at()
            .map(|at| later(at, self.timing.take_within))
            .filter(|until| now < *until)
    }

    /// `/login`'s success state was answered with its Enter and has not left since.
    #[must_use]
    pub const fn login_confirmed(&self) -> bool {
        self.login_confirmed
    }

    /// The open work the current point's done claim names, which its act names as a suffix ([`open_work_suffix`]; empty: none).
    #[must_use]
    pub fn open_items(&self) -> &[&'static str] {
        &self.open
    }

    /// What the session's repository held that no remote main has at the current point's done claim ([`Self::repo`]; `None`: nothing found, or the probe did not finish, [`Self::repo_unread`]).
    #[must_use]
    pub const fn repo_owed(&self) -> Option<&crate::harness::repo_work::RepoWork> {
        self.repo.found()
    }

    /// Whether the repository probe at the current point's done claim did not finish ([`crate::harness::repo_work::RepoRead::Unread`]).
    #[must_use]
    pub const fn repo_unread(&self) -> bool {
        self.repo.is_unread()
    }

    /// THE NEEDS YOU STANDING ([`needs::needs_you`], [`Self::needs`]), as the worker's last report named them.
    #[must_use]
    pub fn standing_needs(&self) -> &[String] {
        &self.needs.standing
    }

    /// The key of the needs standing ([`needs::needs_key`]; `0` for none).
    #[must_use]
    pub const fn needs_key(&self) -> u64 {
        self.needs.key
    }

    /// A NEW SET OF NEEDS STANDS UNACKED.
    #[must_use]
    pub fn needs_ack_owed(&self) -> bool {
        self.needs.key != 0 && self.needs.acked != Some(self.needs.key)
    }

    fn needs_destroy(&self) -> bool {
        self.needs
            .standing
            .iter()
            .any(|n| destructive_word(&n.to_lowercase()).is_some())
    }

    /// The task was landed.
    #[must_use]
    pub const fn landed(&self) -> bool {
        self.task_done() && self.landed
    }

    /// The Land episode in hand has had its instruction ([`LAND`]) typed, and how many nudges ([`LAND_NUDGE`]) since.
    #[must_use]
    pub const fn land_episode(&self) -> (bool, u32) {
        (self.land.typed, self.land.nudges)
    }

    /// WHY LAND STILL HOLDS ([`LandHeld`]).
    #[must_use]
    pub fn land_held(&self) -> Option<LandHeld> {
        if !self.land.typed || self.land.hold.relanded || self.face() != TaskFace::Open {
            return None;
        }
        let base = LandHeld {
            since_ms: self.land.hold.since_ms,
            nudges: self.land.nudges.saturating_sub(self.land.hold.base),
            ..LandHeld::default()
        };
        Some(self.land.legs.map_or(base, |legs| LandHeld {
            read: true,
            legs: legs.legs,
            open: legs.open,
            changed: legs.changed,
            untracked: legs.untracked,
            stashes: legs.stashes,
            unpushed: legs.unpushed,
            unlanded: legs.unlanded,
            ..base
        }))
    }

    /// THE HOLD TO TELL THE HOST.
    pub fn held_moved(&mut self, here: bool) -> Option<Option<LandHeld>> {
        let held = self.land_held().filter(|_| here);
        (self.held_told.replace(held) != Some(held)).then_some(held)
    }

    /// THE LOOP ENDS (`Session::tell_hold_ended`).
    pub const fn held_ended(&mut self) -> bool {
        matches!(self.held_told.replace(None), Some(Some(_)))
    }

    /// AN ACT TYPED UNDER `rule` WAS LEDGERED at `at_ms` (unix ms, the row's own stamp).
    pub fn ledgered(&mut self, rule: &str, at_ms: i64) {
        if self.land.unstamped && is_land_rule(rule) {
            self.land.hold.since_ms = Some(at_ms);
            self.land.unstamped = false;
        }
    }

    /// …AND WHERE THAT EPISODE'S HOLD BEGAN.
    pub const fn seed_land_hold(&mut self, hold: HoldStart) {
        if self.land.typed {
            self.land.hold = hold;
        }
    }

    const fn hold_landed(&mut self) {
        self.land.hold = HoldStart::LANDED;
        self.land.unstamped = false;
        self.land.legs = None;
    }

    pub(crate) fn park_off(&mut self) {
        // A landing holds past the park only while its words do
        // ([`Self::off_landing`]).
        if self.landed() {
            self.off_landing = Some(self.landed_mark);
        }
        self.needs = Needs::default();
        self.land = LandEpisode::default();
        self.landing = false;
        self.cut_check = false;
        self.cut_land = false;
        self.compact_owes = None;
        self.check_point_seed = None;
        self.seeded_land = None;
        self.land_point_seed = false;
        self.carrying = false;
        self.carry_at_attach = false;
        if let Some(episode) = &mut self.landed_seed {
            *episode = false;
        }
        if let Some(a) = &mut self.awaiting {
            a.off_stood = true;
        }
    }

    pub(crate) fn start_lifted_off(&mut self) {
        let seeded = self.carrying;
        self.park_off();
        self.carry_at_attach = seeded;
    }

    /// THE LANDING A LOOP BEFORE THIS ONE LANDED.
    pub const fn seed_landed(&mut self, words: Option<u64>) {
        self.landed_seed = Some(true);
        self.landed_words = words;
        self.seed_by_wake = false;
    }

    /// THE SEEDED LANDING ([`Self::seed_landed`]) WAS KEPT THROUGH THE TASK'S OWN WAKE by the loop before this one.
    pub const fn seed_landed_by_wake(&mut self) {
        if self.landed_seed.is_some() {
            self.seed_by_wake = true;
        }
    }

    /// The landing this state holds was kept through the task's own wake ([`Self::seed_landed_by_wake`]).
    #[must_use]
    pub const fn landed_by_wake(&self) -> bool {
        self.landed && self.by_wake
    }

    /// A WAKE KEPT THE LANDING AT THE POINT FOLDED LAST, asked once by the loop after the fold.
    pub fn take_wake_restated(&mut self) -> bool {
        std::mem::take(&mut self.wake_restated)
    }

    /// THE LOOP STARTS UNDER `mode`, the session's word as the loop read it before it tells its start face ([`Self::face_start`]).
    pub fn start_under(&mut self, mode: Autopilot) {
        if mode == Autopilot::Cruise {
            self.landed_seed = None;
            self.landed_words = None;
            self.seed_by_wake = false;
            self.seed_noted_ms = None;
        }
    }

    /// A landing seeded from a loop before this one is armed, not yet taken up or dropped ([`Self::seed_landed`]).
    #[must_use]
    pub const fn landed_seed_armed(&self) -> bool {
        self.landed_seed.is_some()
    }

    /// THE TIME THE SEEDED LANDING WAS NOTED.
    pub const fn seed_noted_at(&mut self, noted_ms: i64) {
        self.seed_noted_ms = Some(noted_ms);
    }

    /// The time to tell with the face at the loop's start ([`Self::face_start`]).
    #[must_use]
    pub fn face_start_noted(&self) -> Option<i64> {
        let restated = match self.face_told? {
            TaskFace::Landed => self.landed_seed.is_some(),
            TaskFace::Open | TaskFace::Theirs => false,
        };
        self.seed_noted_ms.filter(|_| restated)
    }

    /// THE LANDING THIS POLICY HOLDS IS A SEED RESTATED.
    #[must_use]
    pub fn restated_noted_ms(&self) -> Option<i64> {
        self.restating
            .filter(|(face, _)| *face == self.face())
            .map(|(_, at)| at)
    }

    /// THE LAST REPLY SAID DONE, as this policy read it at its last point.
    #[must_use]
    pub fn said_done(&self) -> bool {
        !self.other_turn
            && !self.cut_reply
            && !self.left_to_person()
            && (self.done >= 1 || self.landed_seed.is_some())
    }

    /// A live turn of someone else's has been read since the last point ([`Self::other_turn`]).
    #[must_use]
    pub const fn other_turn(&self) -> bool {
        self.other_turn
    }

    /// THE TASK'S FACE NOW.
    #[must_use]
    pub fn face(&self) -> TaskFace {
        TaskFace::of(self, self.other_turn)
    }

    /// THE FACE TO TELL THE HOST (`IdleHost::task_face`), where it moved since the loop last told one.
    pub fn face_moved(&mut self) -> Option<TaskFace> {
        let face = self.face();
        let moved = (self.face_told.replace(face) != Some(face)).then_some(face);
        if moved.is_some() {
            self.restating = None;
        }
        moved
    }

    /// The face at the loop's start, told whatever it is.
    pub fn face_start(&mut self) -> TaskFace {
        let face = self.face();
        self.face_told = Some(face);
        face
    }

    /// The task is done because the turn was a person's own exchange.
    #[must_use]
    pub const fn left_to_person(&self) -> bool {
        self.task_done() && self.exchange
    }

    /// The act the point last judged followed.
    #[must_use]
    pub const fn followed(&self) -> Option<&'static str> {
        self.followed
    }

    /// Which point this policy judged last ([`Self::observe`]), counted from `1`.
    #[must_use]
    pub fn point_number(&self) -> Option<u64> {
        (self.points > 0).then_some(self.points)
    }

    /// WHEN THE DONE STANDING NOW WAS DECIDED, on the loop's clock.
    #[must_use]
    pub const fn done_shown(&self) -> Option<Instant> {
        self.done_at
    }

    /// A MANAGER WAITS ON THIS SESSION (`managed`).
    pub const fn set_managed(&mut self, managed: bool) {
        self.managed = managed;
    }

    /// Whether a manager waits on this session ([`Self::set_managed`]).
    #[must_use]
    pub const fn managed(&self) -> bool {
        self.managed
    }

    /// THE TASK IS LANDED BY ITS RECORD.
    pub fn seed_task_done(&mut self) -> bool {
        if self.task_done() {
            return true;
        }
        if self.autopilot == Some(Autopilot::Cruise)
            || self.wall.is_some()
            || self.awaiting.is_some()
            || self.after.is_some()
        {
            return false;
        }
        self.done = 2;
        self.landed = true;
        self.hold_landed();
        self.landed_mark = self.point_mark;
        self.done_at = self.point_at;
        self.exchange = false;
        self.carrying = false;
        self.checking = false;
        self.open.clear();
        self.repo = crate::harness::repo_work::RepoRead::Clean;
        true
    }

    /// WHETHER A LANDING THE TASK RECORD NAMES CAN STAND AT THE POINT JUDGED LAST (`supervise::task_mark`'s seed, before [`Self::seed_task_done`]).
    #[must_use]
    pub const fn record_landing_holds(&self) -> bool {
        self.point_clean
    }

    /// The work of the person's exchange in all so far.
    #[must_use]
    pub const fn exchange_work(&self) -> Duration {
        self.exchange_work
    }

    /// A PERSON'S OWN EXCHANGE STANDS AT A WALL.
    #[must_use]
    pub const fn exchange_at_wall(&self) -> bool {
        self.exchange && self.wall.is_some() && !self.task_done()
    }

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

    /// The back-off before the next act after the short streak.
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

    /// Return whether a retry wall still stands after its notice left the screen.
    #[must_use]
    pub fn holds_left_retry(&self) -> bool {
        self.wall.as_ref().and_then(left_retry_kind).is_some()
    }

    /// An act of this policy's is in flight.
    #[must_use]
    pub fn act_in_flight(&self) -> bool {
        self.awaiting.is_some_and(|a| a.rule != RULE_HOST_TURN) || self.after.is_some()
    }

    /// The account move's answer waits on the act awaited.
    #[must_use]
    pub const fn account_answer_awaited(&self, reading: &TurnEndReading) -> bool {
        self.account.is_some()
            && !self.account_told
            && self.awaiting.is_some()
            && reading.worked.is_none()
    }

    /// An act of this policy's the agent has not taken yet (no live turn seen since, [`Self::took`]), or a command's continuation owed.
    #[must_use]
    pub fn act_untaken(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.rule != RULE_HOST_TURN && !a.taken)
            || self.after.is_some()
            || self.fact_untaken()
    }

    /// A TURN THE HARNESS TYPED THAT THE AGENT HAS NOT TAKEN YET.
    #[must_use]
    pub fn typed_untaken(&self) -> bool {
        self.awaiting.is_some_and(|a| !a.taken) || self.fact_untaken()
    }

    /// THE OCCUPANCY DOOR TYPED ITS LINE AT A BREAK ([`HostTurn::Fact`]): kept in a record of its own, so an act awaited before it keeps its own (a taken
    /// continuation's point judges that act's answer), and untaken until a read sees the agent
    /// take it ([`Self::took`]), which holds the next break ([`Self::act_untaken`]) and reads
    /// that turn as the harness's ([`Self::typed_untaken`]).
    pub const fn fact_typed(&mut self) {
        self.keyed_by_harness();
        self.fact = Some(FactAwaited { taken: false });
    }

    /// The occupancy door's line stands untaken ([`Self::fact_typed`]).
    #[must_use]
    pub fn fact_untaken(&self) -> bool {
        self.fact.is_some_and(|f| !f.taken)
    }

    /// A BREAK OF THE AGENT'S OWN BACKGROUND WORK READ AFTER THE DOOR'S LINE WAS TAKEN: its
    /// answer ended there (`OccupancyDoor`'s `Break`), so the point after it is the agent's own.
    pub fn fact_answered(&mut self) {
        if self.fact.is_some_and(|f| f.taken) {
            self.fact = None;
        }
    }

    /// Whether an act under `rule` typed now would be typed IN A LIMIT'S PLACE ([`Self::acted`]
    /// records it so): the done check, an answer or a land act while the track of a usage window
    /// or a model bucket stands that no loop before this one only seeded.
    #[must_use]
    pub fn in_limits_place(&self, rule: &str) -> bool {
        (rule == RULE_DONE_CHECK || is_answer_rule(rule) || is_land_rule(rule))
            && self
                .wall
                .as_ref()
                .is_some_and(|t| named_class(t.class) && !t.seeded)
    }

    /// THE FACT `fact` RODE THE ACT [`Self::acted`] LAST RECORDED ([`FACT_RIDES`]): the act's
    /// own text, kept for a `/compact`'s continuation to type again, is kept without it, so a
    /// fact told once never rides a retyped act.
    pub fn fact_rode(&mut self, fact: &crate::supervise::facts::Fact) {
        let suffix = fact_suffix(fact);
        if let Some(text) = &mut self.cut_text
            && let Some(kept) = text.strip_suffix(&suffix)
        {
            *text = kept.to_string();
        }
    }

    fn compact_owed_text(&self, rule: &str, rules: Option<&str>) -> Option<String> {
        let owes = self.compact_owes.filter(|_| rule == RULE_COMPACT)?;
        Some(self.cut_text.clone().unwrap_or_else(|| {
            if owes.land && !owes.check {
                match self.seeded_land {
                    Some(RULE_LAND) => land_text(rules),
                    Some(RULE_LAND_ANSWER) => land_answer_text(rules),
                    _ => land_nudge_text(rules),
                }
            } else {
                done_check_text(rules, true)
            }
        }))
    }

    /// A `/compact` of this policy's is running.
    #[must_use]
    pub fn compacting(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.command && a.rule == RULE_COMPACT)
    }

    /// THE WALL'S RETRY AWAITED ([`Awaited::wall_retry`]: the limit's try among them) WAS ANSWERED BY A WALL inside its take window.
    #[must_use]
    pub fn retry_met_untaken(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.wall_retry && a.met && !a.taken)
    }

    #[cfg(test)]
    pub(crate) fn untaken_act_seen(&self) -> Option<Instant> {
        self.awaiting
            .as_ref()
            .filter(|a| !a.taken)
            .and_then(|a| a.unseen_since)
    }

    /// When the act this policy typed last went, while no read since saw the agent's live turn ([`Self::took`]).
    #[must_use]
    pub fn untaken_act(&self) -> Option<Instant> {
        self.untaken().map(|a| a.at)
    }

    /// When [`Self::untaken_act`]'s act was typed on the policy's clock ([`Awaited::typed`]).
    #[must_use]
    pub fn untaken_act_typed(&self) -> Option<Instant> {
        self.untaken().map(|a| a.typed)
    }

    fn untaken(&self) -> Option<&Awaited> {
        self.awaiting
            .as_ref()
            .filter(|a| a.rule != RULE_HOST_TURN && !a.command && !a.taken)
    }

    /// The limit a cancel left, answering a turn no read saw answered.
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

    /// WHETHER A LIVE TURN READ NOW MAY BE A LANDED TASK'S OWN WAKE.
    #[must_use]
    pub fn wake_may_keep_landing(&self) -> bool {
        (self.landed() && self.autopilot == Some(Autopilot::Land)
            || self.landed_seed.is_some() && self.autopilot.is_none_or(|a| a == Autopilot::Land))
            && self.awaiting.is_none()
            && self.after.is_none()
            && !self.other_turn
            && !self.quiet_wake
            && self.fact.is_none()
    }

    /// [`Self::took`] for A LANDED TASK'S OWN WAKE.
    pub fn took_quiet(&mut self) {
        self.quiet_wake = true;
        self.took();
    }

    /// WHAT THE SESSION'S REPOSITORY HELD AS A LANDED TASK'S OWN WAKE BEGAN ([`Self::took_quiet`]).
    pub fn wake_began_over(&mut self, found: crate::harness::repo_work::RepoRead) {
        if self.wake_start_owed() {
            self.wake_repo = Some(found);
        }
    }

    /// A LANDED TASK'S OWN WAKE IS READ AT ITS START ([`Self::took_quiet`]) and THIS STATE SAW THE POINT IT BEGAN AFTER.
    #[must_use]
    pub const fn wake_start_owed(&self) -> bool {
        self.quiet_wake && self.point_at.is_some() && self.wake_repo.is_none()
    }

    /// A read saw the agent's live turn (its spinner or busy footer, never a break of its own background work).
    pub fn took(&mut self) {
        // The door's line's answer is the harness's turn, no one else's.
        if self.awaiting.is_none()
            && self.after.is_none()
            && !self.quiet_wake
            && self.fact.is_none()
        {
            self.other_turn = true;
            self.land.legs = None;
        }
        self.ran_turn_seed = None;
        self.login_seed = None;
        if !self.quiet_wake {
            self.landed_seed = None;
            self.landed_words = None;
            self.seed_by_wake = false;
            self.seed_noted_ms = None;
        }
        self.restating = None;
        self.ran |= std::mem::take(&mut self.point_last);
        self.gap = false;
        self.esc_at = None;
        self.esc_seed = false;
        if let Some(a) = &mut self.awaiting {
            a.taken = true;
            a.met = false;
        }
        if let Some(f) = &mut self.fact {
            f.taken = true;
        }
        if std::mem::take(&mut self.seed_answered) {
            self.checking = false;
            self.landing = false;
        }
        self.reset_resume = None;
        if std::mem::take(&mut self.carry_at_attach) {
            self.carrying = true;
        }
    }

    const fn new_point(&mut self, now: Instant) {
        self.point_at = Some(now);
        self.points = self.points.saturating_add(1);
    }

    /// A BOX WAS READ UP (a permission box, a dialog).
    pub fn box_up(&mut self) {
        self.esc_at = None;
        self.esc_seed = false;
        self.ran |= std::mem::take(&mut self.point_last);
        self.gap = false;
    }

    #[cfg(test)]
    pub(crate) fn esc_read(&self) -> Option<Instant> {
        self.esc_at
    }

    /// The Mac slept `by`, as the loop measured it (`Instant` stops while the Mac sleeps; the wall clock does not).
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
        if let Some(c) = &mut self.credits {
            for t in [&mut c.reset, &mut c.passed, &mut c.since]
                .into_iter()
                .flatten()
            {
                back(t);
            }
        }
    }

    /// The loop's limit-reset step ended and its host read `outcome` (`crate::supervise::IdleHost::reset_told`).
    pub fn reset_told(&mut self, outcome: &ResetOutcome, now: Instant) {
        let (asks, cleared_at) = match *outcome {
            ResetOutcome::Pending => return,
            ResetOutcome::Spent {
                asks, cleared_at, ..
            }
            | ResetOutcome::Lifted { asks, cleared_at } => (asks, Some(cleared_at)),
            ResetOutcome::Kept
            | ResetOutcome::Failed { .. }
            | ResetOutcome::Unread
            | ResetOutcome::Walled { .. } => (true, None),
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

    /// From when the continuation a limit reset left is typed, while one is owed.
    #[must_use]
    pub fn reset_resume_owed(&self) -> Option<Instant> {
        self.reset_resume.map(|owed| owed.due)
    }

    /// The model switch in force, if any.
    #[must_use]
    pub const fn model_switch(&self) -> Option<&ModelSwitch> {
        self.model.as_ref()
    }

    /// Whether the act [`Self::acted`] last recorded (a typed act; a restart is [`Self::restarted`]'s) was typed IN A LIMIT'S PLACE.
    #[must_use]
    pub fn acted_in_limits_place(&self) -> bool {
        self.limit_try.is_some_and(|l| l.in_place)
    }

    /// THE LIMIT'S TRY the act [`Self::acted`] last recorded was, if it was one ([`LimitTry`]).
    #[must_use]
    pub const fn limit_try(&self) -> Option<LimitTry> {
        self.limit_try
    }

    /// A LIMIT'S TRACK ENDED at the last fold ([`Self::observe`]), by no act of this policy's.
    #[must_use]
    pub const fn limit_ended(&self) -> bool {
        self.limit_ended
    }

    /// The limit's track ended at the last fold ([`Self::limit_ended`])
    /// because ANOTHER PROGRAM'S SCREEN was read: a limit met under one
    /// agent the loop decides on, ended where another such agent runs in
    /// the tab now ([`met_under_another`]), not by the worker's work or
    /// another wall.
    #[must_use]
    pub const fn limit_ended_elsewhere(&self) -> bool {
        self.limit_ended && self.limit_ended_elsewhere
    }

    /// The acts made at the wall being worked, if it is of `kind`'s class
    /// ([`exception_for`](super::exception::exception_for)'s and the conformance walks'
    /// projection).
    #[must_use]
    pub fn wall_attempts(&self, kind: WallKind) -> Option<usize> {
        self.wall
            .as_ref()
            .filter(|t| t.class == class_of(kind))
            .map(|t| t.attempts)
    }

    /// Return whether this state needs the account roster to choose the owner's notice.
    #[must_use]
    pub fn reads_roster(&self, reading: &TurnEndReading) -> bool {
        reading.wall == Some(WallKind::UsageWeekly)
            || (self.wall_attempts(WallKind::UsageWeekly).is_some()
                && (reading.login_interrupted || self.login_cancelled_on_limit()))
    }

    /// Since when the wall being worked shows, if it is of `kind`'s class.
    #[must_use]
    pub fn wall_since(&self, kind: WallKind) -> Option<Instant> {
        self.wall
            .as_ref()
            .filter(|t| t.class == class_of(kind))
            .map(|t| t.since)
    }

    /// Whether the full context being worked has met the vendor's own word that a `/compact` could not lower it.
    #[must_use]
    pub fn compaction_exhausted(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Context && t.exhausted)
    }

    /// What the full context being worked was said to be filled by, by its own notice this appearance ([`ContextFill::of`]).
    #[must_use]
    pub fn context_fill(&self) -> Option<ContextFill> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Context)
            .and_then(|t| t.fill)
    }

    /// THE TYPED KIND OF THE TRANSCRIPT'S NOTICE at this decision's read, told by the loop (`supervise/wall_confirm.rs`).
    pub fn notice_typed_was(&mut self, typed: Option<String>) {
        self.notice_typed = typed;
    }

    /// The typed kind of the notice the transcript ended on at the last decision's read.
    #[must_use]
    pub fn notice_typed(&self) -> Option<&str> {
        self.notice_typed.as_deref()
    }

    /// `/compact`'S OWN ANSWER.
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

    /// Whether the fallback relaunch for the bucket being worked was refused for good ([`Self::restarted`] with `made` false).
    #[must_use]
    pub const fn fallback_refused(&self) -> bool {
        self.fallback_unmade
    }

    /// The account move made and not answered yet, if any.
    #[must_use]
    pub const fn account_move(&self) -> Option<&AccountMove> {
        self.account.as_ref()
    }

    /// Whether an account move asked for at the wall being worked could not be made ([`Self::restarted`] with `made` false).
    #[must_use]
    pub const fn account_refused(&self) -> bool {
        self.account_unmade.is_some()
    }

    /// What the host's word for that move says of asking it again ([`Unmade`]; [`Self::restart_unmade`]).
    #[must_use]
    pub const fn account_refusal(&self) -> Option<Unmade> {
        self.account_unmade
    }

    /// The accounts' news reached this session: both refusal latches end and the next point asks again.
    pub fn account_news(&mut self) {
        self.account_unmade = None;
        self.account_unmade_seed = None;
        self.account_unmade_ms = None;
        self.account_ahead_unmade = None;
    }

    /// Whether the wall's resume is owed after a move this session's host made at a wall ([`Self::account_resume`]).
    #[must_use]
    pub const fn account_resume_owed(&self) -> bool {
        self.account_resume
    }

    fn account_ahead_refused(&self, to: &str, now: Instant) -> bool {
        self.account_ahead_unmade
            .as_ref()
            .is_some_and(|(label, at)| {
                label == to && now.saturating_duration_since(*at) < self.timing.window
            })
    }

    /// How many account moves this session made within the window ending at `now`, and when the oldest of them leaves it.
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

    /// What this point says of the account move not answered yet.
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
                && !self.account_startup
                && !self.login_drew(reading))
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
                && !self.login_drew(reading)
                && matches!(reading.phase, Phase::Idle | Phase::Question) =>
            {
                Some(AccountAnswer::Served)
            }
            _ => None,
        }
    }

    #[must_use]
    pub(super) fn login_left(&self, reading: &TurnEndReading) -> bool {
        reading.wall == Some(WallKind::Auth)
            && self.account.as_ref().is_some_and(|m| m.wall)
            && !self.account_asked
            && !self.account_told
    }

    const fn login_drew(&self, reading: &TurnEndReading) -> bool {
        self.wall.is_some()
            && reading.worked.is_some()
            && (reading.login_screen.is_some() || reading.login_interrupted || reading.login_back)
    }

    /// A BUCKET THE SESSION LEFT.
    #[must_use]
    pub fn bucket_left(&self, reading: &TurnEndReading) -> bool {
        reading
            .wall
            .is_some_and(|kind| self.left_bucket(kind, &reading.wall_message))
    }

    /// [`Self::bucket_left`] for a wall of `kind` in `message`'s words.
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

    fn relaunched_off(&self, t: &WallTrack) -> bool {
        t.class == WallClass::Model
            && t.named.is_none()
            && self
                .model
                .as_ref()
                .and_then(|m| m.from.as_deref())
                .is_some_and(|from| bucket_model(&t.said).as_deref() == Some(from))
    }

    fn fallback_made(&self) -> Option<i64> {
        self.fallback_started.or(self.fallback_unix)
    }

    /// THE FALLBACK'S RELAUNCH WAS MADE at `unix` (seconds), told by the loop after [`Self::restarted`] made it.
    pub const fn fallback_made_at(&mut self, unix: i64) {
        self.fallback_unix = Some(unix);
        self.fallback_started = None;
    }

    /// WHEN THE TRANSCRIPT'S NOTICE WAS WRITTEN, at this decision's read, told by the loop (`supervise/wall_confirm.rs`).
    pub const fn notice_written_at(&mut self, unix: Option<i64>) {
        self.notice_unix = unix;
    }

    /// When the transcript's notice the last decision read was written.
    #[must_use]
    pub const fn notice_written(&self) -> Option<i64> {
        self.notice_unix
    }

    /// THE FALLBACK'S RELAUNCH WAS MADE AND NO TURN ASKED since ([`Self::bucket_left`]'s latch).
    #[must_use]
    pub const fn fallback_unasked(&self) -> bool {
        self.fallback_unasked
    }

    /// Seed a fallback relaunch from a previous loop where no turn has been asked yet.
    pub const fn seed_fallback_unasked(&mut self, made_at: i64) {
        self.fallback_unasked = self.model.is_some();
        self.fallback_unix = Some(made_at);
        self.fallback_started = None;
    }

    const fn ran_under_move(&self, reading: &TurnEndReading) -> bool {
        self.account.is_some()
            && reading.worked.is_some()
            && !reading.pending_input
            && !self.account_startup
    }

    /// The point the host is asked about ([`AccountPoint`]).
    #[must_use]
    pub fn account_point(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
    ) -> Option<AccountPoint> {
        let rotates = account_rotates(self, reading, cfg);
        let follows = login_follows(self, reading, cfg);
        let lost = follows
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Auth);
        if !reading.at_point(self.stale_input)
            || reading.taskless
            || reading.upgrading
            || (self.task_done() && !lost)
            || self.reset_resume.is_some()
            || !(rotates || follows)
        {
            return None;
        }
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

    /// Whether this point may take the accounts probe ([`RULE_USAGE_PROBE`]).
    #[must_use]
    pub fn probe_point(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> bool {
        self.probe_gate(reading, cfg, now).is_none()
    }

    /// [`Self::probe_point`] by the gate that shut.
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

    /// The loop told its host what answered the move ([`Self::account_answer`]).
    pub fn account_told(&mut self, answer: AccountAnswer) {
        match answer {
            AccountAnswer::Served => self.account_over(),
            AccountAnswer::Bad(_) => self.account_told = true,
        }
    }

    fn account_over(&mut self) {
        self.account = None;
        self.account_asked = false;
        self.account_told = false;
        self.account_startup = false;
        self.account_resume = false;
    }

    fn wall_words<'a>(&'a self, reading: &'a TurnEndReading) -> &'a str {
        match &self.wall {
            Some(t) if reading.wall.is_none() => &t.said,
            _ => &reading.wall_message,
        }
    }

    /// The lost login's track, if one stands.
    #[must_use]
    pub fn login_track(&self) -> Option<usize> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Auth)
            .map(|t| t.attempts)
    }

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

    const fn credits_bound(&self) -> Duration {
        self.timing.credits_bound
    }

    /// THE CREDITS HOLD'S ESCALATION WAS DECIDED ([`CREDITS_HOLD_TOLD`], [`CREDITS_HOLD_MODEL_TOLD`]).
    pub fn credits_told(&mut self) {
        self.credits
            .get_or_insert_with(CreditsEpisode::default)
            .told = true;
    }

    /// WHETHER THE CREDITS HOLD STANDS at `now` under `cfg`, for a read that says credits carry the session (`read`, `None` for one that does not).
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

    fn credits_reset(&self, read: &CreditsCarry) -> Option<Instant> {
        match &self.credits {
            Some(c) => c.reset.or(read
                .reset_at
                .filter(|&at| c.passed.is_none_or(|p| at > later(p, SAME_CREDITS_RESET)))),
            None => read.reset_at,
        }
    }

    /// Folds what a point shows into the state, before [`decide_turn_end`].
    pub fn observe(&mut self, reading: &TurnEndReading, now: Instant) {
        let arrived = self.points > 0 || reading.worked.is_some();
        let ran_turn = self.ran_turn_reading(reading, now);
        let reading = ran_turn.as_ref().unwrap_or(reading);
        // The seat this screen names, kept for a limit met later
        // ([`Self::note_seat`]).
        self.note_seat(reading);
        if reading.phase != Phase::Busy {
            if super::login::carried(reading.program) {
                self.take_up_login(
                    reading.login_at_wall.as_ref(),
                    reading.login_interrupted,
                    now,
                );
            }
            self.login_seed = None;
        }
        if !matches!(
            reading.login_screen,
            Some(aterm_phase::LoginScreen::Success { .. })
        ) {
            self.login_confirmed = false;
        }
        self.limit_ended = false;
        self.limit_ended_elsewhere = false;
        self.point_moved = false;
        self.left_judged = false;
        if !reading.wall_remembered
            && self.wall.as_ref().is_some_and(|t| {
                named_class(t.class) && met_under_another(t.program, reading.program)
            })
        {
            self.wall = None;
            self.fallback_unmade = false;
            self.limit_ended = true;
            self.limit_ended_elsewhere = true;
        }
        self.looked = Some((
            now,
            left_key(reading.since_limit.as_ref(), reading.auto_continue),
        ));
        if reading.autopilot != Autopilot::Land {
            self.leave_land_episode();
        }
        self.autopilot = Some(reading.autopilot);
        let persons_esc =
            reading.interrupted && !matches!(reading.phase, Phase::Busy | Phase::Prompt);
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
        if reading.taskless
            && reading.wall.is_none()
            && self.wall.as_ref().is_some_and(|t| {
                matches!(t.class, WallClass::Usage | WallClass::Model) && !t.relaunch.startup
            })
        {
            self.wall = None;
            self.fallback_unmade = false;
            self.limit_ended = true;
        }
        let left_login = self.login_left(reading) && !self.ran_under_move(reading);
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
        let fired;
        let went_on = self.limit_went_on(reading, Some(now));
        let reading = if went_on {
            fired = TurnEndReading {
                worked: Some(Duration::ZERO),
                ..reading.clone()
            };
            &fired
        } else {
            reading
        };
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
        let login_spin;
        let reading = if self.login_drew(reading) {
            login_spin = TurnEndReading {
                worked: None,
                worked_doubt: Duration::ZERO,
                ..reading.clone()
            };
            &login_spin
        } else {
            reading
        };
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
        let other_unseen;
        let reading = if self.unseen_turn(reading) {
            other_unseen = TurnEndReading {
                worked: Some(Duration::ZERO),
                ..reading.clone()
            };
            &other_unseen
        } else {
            reading
        };
        if !reading.pending_input || reading.worked.is_some() {
            self.stale_input = false;
        }
        let awaited_command = self.awaiting.is_some_and(|a| a.command);
        let worked_to_wall =
            !awaited_command && reading.worked.is_some_and(|w| w >= self.timing.progress);
        if !self.point_judged(reading, now) {
            // No point yet: the wall it shows is tracked all the same (taking up a latch a
            // loop before this one left, as the first wall this state sees does), and an
            // act awaited is answered by it ([`Awaited::met`]).
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
        self.other_turn = false;
        self.quiet_wake = false;
        let wake_repo = self.wake_repo.take();
        let seeded = self.landed_seed.take();
        let landing_words = self.landed_words.take();
        let seeded_by_wake = std::mem::take(&mut self.seed_by_wake);
        let noted = self.seed_noted_ms.take();
        self.restating = noted
            .filter(|_| seeded.is_some())
            .map(|at| (TaskFace::Landed, at));
        let min = self.timing.min_work;
        let short_before = self.short;
        let point_at_before = self.point_at;
        let seeded_point = self.point_seed.take();
        let check_point = self.check_point_seed.take();
        let land_point = std::mem::take(&mut self.land_point_seed);
        self.esc_seed = false;
        let seeded_yield = self.yield_seed.take();
        let same_point =
            seeded_point.is_some() && reading.worked.is_none() && self.awaiting.is_none();
        let mut checked = std::mem::take(&mut self.checking);
        let mut landing = std::mem::take(&mut self.landing);
        let landing_act = self.landing_act.take();
        self.seed_answered = false;
        let met = self.awaiting.is_some_and(|a| a.met);
        // The door's line, where no act was awaited before it: its answer is judged as the line's
        // ([`HostTurn::Fact`]); with one, as that act's.
        let fact = self.fact.take();
        let host = self
            .awaiting
            .and_then(|a| a.host)
            .or(fact.map(|_| HostTurn::Fact));
        let unseen_after_off =
            self.awaiting.is_some_and(|a| a.off_stood) && reading.worked.is_none();
        let wall_retry = self.awaiting.is_some_and(|a| a.wall_retry);
        let act_taken = self.awaiting.is_some_and(|a| a.taken) || reading.worked.is_some();
        let awaited = self.awaiting.take().map(|a| a.rule);
        if awaited.is_some() || reading.worked.is_some() {
            // THE VENDOR'S OWN CONTINUATION of the limit that cut it (Claude
            // Code's `Usage limit reset · continuing automatically`, no `❯`
            // row) delivers it again, as the wall's retry does: the turn
            // that ran here, no act of this policy's awaited, is its answer
            // (read as a turn nobody asked, its done reply
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
        }
        let done = says_done(reading.said_tail.as_deref());
        let clean_words = clean_defense(reading.said_tail.as_deref())
            && !reading.said_cut
            && reading
                .turn_said
                .as_deref()
                .is_none_or(|t| needs::needs_you(t).is_empty());
        let clean = clean_words && reading.repo.is_clean();
        let record_says = reading.finished_record == Some(true)
            && !reading.pending_input
            && reading.autopilot != Autopilot::Cruise;
        let by_record = record_says && reading.repo.is_clean();
        let off_stood = self.off_landing.is_some() || seeded == Some(false);
        let began_over = !off_stood
            && wake_repo.as_ref().is_some_and(|began| {
                crate::harness::repo_work::RepoRead::same(began, &reading.repo)
            });
        let wake_repo_kept = reading.repo.is_clean() || began_over;
        let wake_keeps = record_says && wake_repo_kept && !wake_left_open(reading);
        let repo_theirs = reading.repo.found().is_some() && began_over;
        let landed_before = self.landed();
        let tail_mark = said_mark(reading.said_tail.as_deref());
        let words_hold = clean_defense(reading.said_tail.as_deref())
            && reading
                .turn_said
                .as_deref()
                .is_none_or(|t| needs::needs_you(t).is_empty())
            && landing_words.is_some_and(|w| tail_mark == Some(w))
            && (seeded == Some(true) || reading.repo.is_clean())
            && reading.worked.is_none()
            && awaited.is_none();
        let wake_hold = seeded_by_wake
            && record_says
            && landing_words.is_some_and(|w| tail_mark == Some(w))
            && !wake_words_open(reading)
            && (seeded == Some(true) || reading.repo.is_clean())
            && reading.worked.is_none()
            && awaited.is_none();
        let act_speaks = match reading.program {
            aterm_phase::Program::Claude => true,
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic => false,
        };
        let act_opens = match reading.program {
            aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Gemini => true,
            aterm_phase::Program::Claude
            | aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Generic => false,
        };
        let act_seen = match landing_act {
            Some((_, lead, _)) if act_opens => reading.opened_by.is_none_or(|o| o == lead),
            Some((said, _, typed)) if act_speaks => {
                reading.answered.is_some_and(|a| a.prompt == said)
                    || reading.record_turn.is_some_and(|t| {
                        t.prompt == said
                            && t.ended.is_some()
                            && now
                                .checked_sub(t.age)
                                .is_some_and(|recorded| recorded + RECORD_SLACK >= typed)
                    })
            }
            Some(_) | None => true,
        };
        let streak = |short: u32, d: Duration, done: bool| {
            if d < min || done {
                short.saturating_add(1)
            } else {
                0
            }
        };
        let carried = self.carrying && self.done == 0;
        let redelivers = reading.worked.is_some()
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Auth && t.login.hand);
        let hand = self.hand(reading, now);
        let theirs = !carried && !reading.interrupted && (hand || redelivers);
        let prompted = !went_on
            && (hand
                || reading
                    .answered
                    .is_some_and(|a| Some(a.prompt) != self.last_prompt));
        let seed_was_armed = seeded.is_some();
        let off_landing = self.off_landing.take();
        let ok_to_leave = reading.wall.is_none() && !reading.pending_input;
        let (points_before, done_before) = (self.points, self.done);
        let yield_stopped = seeded_yield.is_some() && reading.interrupted;
        let first_point = self.point_at.is_none();
        let mut seed_mark = None;
        let mut wake_kept = false;
        let mut land_judged = false;
        match (awaited, reading.worked) {
            (Some(RULE_HOST_TURN) | None, worked) if host == Some(HostTurn::Fact) => {
                self.point_worked = worked.unwrap_or_default();
                self.new_point(now);
            }
            (Some(RULE_HOST_TURN), worked) if host == Some(HostTurn::Check) => {
                let d = worked.unwrap_or_default();
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
                    self.done = u8::from(done);
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
                if unseen_after_off && self.exchange {
                    self.exchange = false;
                    self.short = self.short.max(1);
                }
                self.exchange_work = self.exchange_work.saturating_add(d);
                self.exchange &= self.exchange_work < EXCHANGE_WORK
                    && !is_continue_rule(rule)
                    && (!awaited_command || rule == RULE_LOGIN);
                if rule != RULE_LOGIN {
                    self.carrying = !self.exchange && !reading.interrupted;
                }
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
                if is_continue_rule(rule) {
                    self.short = streak(self.short, d, done);
                }
                if ok_to_leave {
                    land_judged = landing && !self.exchange && act_taken;
                    let lands = land_judged && clean;
                    self.done = if self.exchange || lands {
                        2
                    } else {
                        u8::from(done)
                    };
                    self.landed = lands;
                }
            }
            (None, Some(_)) if wake_keeps && landed_before => wake_kept = true,
            (None, Some(_)) if wake_keeps && self.point_at.is_none() && seeded.is_some() => {
                self.new_point(now);
                self.done = 2;
                self.landed = true;
                self.exchange = false;
                if let Some(episode) = seeded {
                    self.land.typed |= episode;
                }
                wake_kept = true;
            }
            (None, Some(d)) => {
                self.exchange_work = d;
                self.exchange = theirs && d < EXCHANGE_WORK;
                self.carrying &= carried;
                self.landed = false;
                let left = self.exchange && ok_to_leave;
                self.followed = if checked { self.seeded_check } else { None };
                self.point_worked = d;
                self.new_point(now);
                land_judged = landing && !left && ok_to_leave;
                self.land.legs = None;
                let lands = land_judged && clean;
                self.done = if left || lands {
                    2
                } else {
                    u8::from(done && reading.wall.is_none())
                };
                self.landed = lands;
                match seeded_yield {
                    Some(continues) => {
                        if yield_stopped {
                            self.carrying = false;
                        }
                        if continues {
                            self.short = streak(self.short, d, done);
                        }
                    }
                    None if reading.wall.is_none() && !left => {
                        self.short = streak(self.short, d, false);
                    }
                    None => {}
                }
            }
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
                    self.short = self.short.saturating_add(1);
                }
                land_judged = landing && act_seen && ok_to_leave;
                if landing && act_seen && reading.wall.is_some() {
                    self.cut_land = true;
                }
                if checked && reading.wall.is_some() {
                    self.cut_check = true;
                }
                if let Some(rule) = check_point.filter(|_| same_point && reading.wall.is_some()) {
                    self.cut_check = true;
                    self.seeded_check = Some(rule);
                }
                if land_point && same_point && reading.wall.is_some() {
                    self.cut_land = true;
                }
                let lands = land_judged && clean;
                self.done = if lands {
                    2
                } else {
                    u8::from(done && reading.wall.is_none())
                };
                self.landed = lands;
                self.exchange = false;
                let at_wall = by_record && reading.wall.is_some();
                if let Some(episode) = seeded
                    && reading.autopilot == Autopilot::Land
                    && ((ok_to_leave
                        && landing_words.is_none_or(|w| tail_mark == Some(w))
                        && (clean || words_hold || wake_hold))
                        || at_wall)
                {
                    self.done = 2;
                    self.landed = true;
                    self.land.typed |= episode;
                    self.by_wake = seeded_by_wake;
                    if at_wall {
                        seed_mark = landing_words;
                    }
                }
            }
            (None, None)
                if off_landing.is_some_and(|mark| !(mark == tail_mark && clean && ok_to_leave))
                    && landed_before
                    && !by_record =>
            {
                self.landed = false;
                self.done = u8::from(done && reading.wall.is_none());
                self.followed = None;
                self.new_point(now);
                self.short = self.short.saturating_add(1);
            }
            (None, None) => {}
        }
        self.point_moved = self.short != short_before
            || ((checked || landing) && !same_point)
            || awaited.is_some()
            || (self.point_at != point_at_before && !same_point)
            || wake_kept;
        if wake_kept {
            self.by_wake = true;
            self.wake_restated = true;
        } else if !self.landed || landing {
            self.by_wake = false;
        }
        if reading.left_record && !reading.pending_input && !reading.wall_drawn() {
            self.exchange = true;
            self.done = 2;
        }
        if reading.left_record && reading.wall.is_some() && first_point {
            self.exchange = true;
            self.exchange_work = self
                .exchange_work
                .max(reading.left_work.unwrap_or_default());
        }
        if reading.autopilot == Autopilot::Cruise && self.landed && !self.exchange {
            self.landed = false;
            self.done = 1;
        }
        if awaited.is_some() || reading.worked.is_some() || first_point {
            let tail = reading.said_tail.as_deref();
            let claim = self.done == 1 && !reading.interrupted;
            self.open = if claim {
                match reading.autopilot {
                    Autopilot::Land => land_open_words(tail),
                    Autopilot::Cruise | Autopilot::Off => {
                        tail.map_or_else(Vec::new, |t| open_work(Some(&needs::without_needs(t))))
                    }
                }
            } else {
                Vec::new()
            };
            let unlanded_by_repo = record_says && landed_before && !self.landed;
            self.repo = if (claim || unlanded_by_repo) && !repo_theirs {
                reading.repo.clone()
            } else {
                crate::harness::repo_work::RepoRead::Clean
            };
            if reading.wall.is_none() && !reading.pending_input {
                // Someone else's turn on someone's prompt (`prompted`): the
                // person answered what stood. A turn nobody asked — Claude
                // Code's own wake, the vendor's continuation of a limit —
                // answered nothing.
                if awaited.is_none() && reading.worked.is_some() && prompted {
                    self.needs = Needs::default();
                }
                let act = self.act_words.clone().filter(|_| reading.opens_cut);
                let opening =
                    |t: &'_ str| -> String { without_act_rows(t, act.as_deref()).to_string() };
                match reading.turn_said.as_deref() {
                    Some(before) => self.fold_needs(Some(&format!(
                        "{}\n\n{}",
                        opening(before),
                        tail.unwrap_or_default()
                    ))),
                    None => self.fold_needs(tail.map(opening).as_deref()),
                }
            }
        }
        if self.done < 2 {
            self.done_at = None;
        } else if done_before < 2 || self.points != points_before {
            self.done_at = Some(now);
        }
        if self.done < 2 {
            self.landed = false;
        }
        if land_judged {
            self.land.legs = (!self.landed()).then(|| hold_at_yield(reading));
        }
        if self.landed() && !landed_before {
            self.landed_mark = seed_mark.or(tail_mark);
            self.hold_landed();
        }
        self.point_mark = tail_mark;
        self.point_mark_len = said_letters(reading.said_tail.as_deref()).len();
        if let Some(opener) = turn_opener(reading) {
            self.point_opener = Some(opener);
        }
        let judged_turn = awaited.is_some() || reading.worked.is_some();
        if reading.answered_point() || judged_turn {
            let undrawn = self.submitted_since(reading) == Some(true)
                && !self.drawn_since(reading)
                && !judged_turn;
            if !undrawn {
                self.point_submits = reading.submits;
                self.point_screen = reading.screen_mark;
                self.point_tasked = !reading.taskless;
                self.point_inputs = reading.inputs_before;
            }
        }
        self.point_clean = (clean || (words_hold && seed_was_armed && self.landed()))
            && ok_to_leave
            && reading.worked.is_none()
            && awaited.is_none()
            && !(seed_was_armed && !self.landed());
        if let Some(a) = reading.answered {
            self.last_prompt = Some(a.prompt);
        }
        if self.done == 2 {
            self.carrying = false;
        }
        self.carry_at_attach = false;
        self.left_judged = (awaited.is_some() || reading.worked.is_some()) && self.left_to_person();
        let retried = wall_retry;
        let compacted_nothing = awaited == Some(RULE_COMPACT)
            && awaited_command
            && reading.worked.is_none()
            && reading.wall.is_none()
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Context);
        let retry_taken = retried && !met && !compacted_nothing;
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
                let seat_now = self.seat_now(reading.program, &reading.seat);
                if !reading.wall_remembered
                    && let Some(t) = self.wall.as_mut().filter(|t| {
                        named_class(t.class)
                            && t.program == Some(reading.program)
                            && screen_alone(reading.program)
                    })
                    && (t.seat == Seat::default() || reading.worked.is_some() || t.seat_moved)
                {
                    t.seat.update(&seat_now);
                    t.seat_moved = false;
                }
            }
            None => {
                let login_back = reading.login_back
                    && self
                        .wall
                        .as_ref()
                        .is_some_and(|t| t.class == WallClass::Auth);
                let owed = self.wall.as_ref().is_some_and(|t| t.owed);
                let interrupted = reading.login_interrupted
                    && self.wall.as_ref().is_some_and(|t| {
                        matches!(
                            t.class,
                            WallClass::Auth | WallClass::Usage | WallClass::Model
                        )
                    });
                let dialog = reading.login_screen.as_ref().is_some_and(|screen| {
                    !matches!(screen, aterm_phase::LoginScreen::Success { .. })
                }) && self
                    .wall
                    .as_ref()
                    .is_some_and(|t| t.class == WallClass::Auth);
                let owns_answer = self
                    .wall
                    .as_ref()
                    .is_none_or(|t| !named_class(t.class) || limit_conversation_ours(t, reading));
                let gone = (reading.worked.is_some() && !interrupted && !dialog)
                    || ((retry_taken || owed) && owns_answer)
                    || login_back;
                if gone || unmade_seeded.is_some() {
                    self.account_unmade = None;
                }
                let dropped = auto_continues(reading.program)
                    && reading.composer != Composer::Absent
                    && reading.auto_continue != Some(aterm_phase::AutoContinue::Armed);
                if gone {
                    self.limit_ended |= self.wall.as_ref().is_some_and(|t| named_class(t.class));
                    self.login_returned |= self.wall.as_ref().is_some_and(|t| {
                        t.class == WallClass::Auth || (named_class(t.class) && t.login.back_typed)
                    });
                    self.wall = None;
                    self.fallback_unmade = false;
                } else if interrupted && let Some(t) = self.wall.as_mut() {
                    t.login.cancelled = true;
                } else if reading.login_back
                    && let Some(t) = self
                        .wall
                        .as_mut()
                        .filter(|t| named_class(t.class) && !t.login.back_typed)
                {
                    t.named = None;
                    t.attempts = 0;
                    t.login.cancelled = false;
                    t.login.back_typed = true;
                    if self.done >= 2 && !self.landed {
                        self.done = 0;
                    }
                } else if let Some(t) = self.wall.as_mut().filter(|t| {
                    t.class == WallClass::Model
                        && limit_conversation_ours(t, reading)
                        && t.named.is_some_and(|n| n.moved_off_under(reading, &t.said))
                }) {
                    t.named = None;
                } else if let Some((t, seat_now)) = {
                    let seat_now = self.seat_now(reading.program, &reading.seat);
                    self.wall
                        .as_mut()
                        .filter(|t| {
                            named_class(t.class)
                                && !t.seat_moved
                                && t.program == Some(reading.program)
                                && screen_alone(reading.program)
                                && t.seat.moved(&seat_now, limit_per_model(reading.program))
                        })
                        .map(|t| (t, seat_now))
                } {
                    t.named = None;
                    t.seat_moved = true;
                    t.seat.update(&seat_now);
                } else if (dropped
                    || reading.auto_continue == Some(aterm_phase::AutoContinue::Unkept))
                    && let Some(n) = self.wall.as_mut().and_then(|t| t.named.as_mut())
                {
                    n.by_itself = false;
                }
                if let (Some(at), Some(n)) = (
                    reading.reset_at,
                    self.wall.as_mut().and_then(|t| t.named.as_mut()),
                ) {
                    n.at = at;
                    n.jitter = reading.jitter;
                }
            }
        }
        let startup = std::mem::take(&mut self.account_startup);
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
        if let Some(owed) = self.reset_resume
            && reading
                .worked
                .and_then(|worked| now.checked_sub(worked))
                .is_some_and(|began| began >= owed.told)
        {
            self.reset_resume = None;
        }
    }

    fn limit_went_on(&self, reading: &TurnEndReading, now: Option<Instant>) -> bool {
        if self
            .wall
            .as_ref()
            .is_some_and(|t| named_class(t.class) && !limit_conversation_ours(t, reading))
        {
            return false;
        }
        let since = reading
            .since_limit
            .as_ref()
            .filter(|_| !self.limit_elsewhere(reading));
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

    fn answered_unseen(&self, reading: &TurnEndReading, now: Instant) -> Option<Duration> {
        // A common continuation/check row in another or unread conversation
        // is not proof that this wall's awaited act ran. Actual observed work
        // is supplied independently by the caller and keeps its normal rule.
        if self
            .wall
            .as_ref()
            .is_some_and(|t| named_class(t.class) && !limit_conversation_ours(t, reading))
        {
            return None;
        }
        let a = self.untaken().filter(|a| !a.met && !a.off_stood)?;
        if reading.worked.is_some() || !reading.answered_point() {
            return None;
        }
        let since = now.saturating_duration_since(a.at);
        let Some(said) = a.said else {
            let by_screen = reading
                .answered
                .filter(|s| a.over.is_some_and(|o| o != s.prompt))
                .map(|s| s.work.unwrap_or(since));
            let by_opener = reading
                .opened_by
                .filter(|o| reading.said_tail.is_some() && a.opened.is_some_and(|at| at != *o))
                .map(|_| since);
            return a
                .submitted
                .then(|| by_screen.or(by_opener))
                .flatten()
                .map(|work| work.min(since));
        };
        let by_record = reading
            .record_turn
            .filter(|t| {
                t.prompt == said
                    && now
                        .checked_sub(t.age)
                        .is_some_and(|recorded| recorded + RECORD_SLACK >= a.typed)
            })
            .and_then(|t| t.ended);
        let submitted = a.submitted_since(reading);
        let by_screen = reading
            .answered
            .filter(|s| s.prompt == said && (submitted || a.over.is_some_and(|o| o != said)))
            .map(|s| s.work.unwrap_or(since));
        let by_opener = a
            .lead
            .filter(|lead| {
                reading.opened_by == Some(*lead)
                    && reading.said_tail.is_some()
                    && (submitted || a.opened.is_some_and(|o| o != *lead))
            })
            .map(|_| since);
        by_record
            .or(by_screen)
            .or(by_opener)
            .map(|work| work.min(since))
    }

    const fn submitted_since(&self, reading: &TurnEndReading) -> Option<bool> {
        match (reading.submits, self.point_submits) {
            (Some(now), Some(then)) if now >= then => Some(now > then),
            _ => None,
        }
    }

    fn drawn_since(&self, reading: &TurnEndReading) -> bool {
        reading.screen_mark.is_some() && reading.screen_mark != self.point_screen
    }

    const fn keyed_by_harness(&mut self) {
        self.point_submits = None;
        self.point_screen = None;
        self.point_tasked = false;
        self.point_inputs = None;
    }

    /// THE COUNT READ AGAIN ONCE THE HARNESS'S OWN KEYS WERE SEEN TO LAND.
    pub const fn counted_after_harness(&mut self, at: Option<(u64, u64)>) {
        if let Some((submits, screen)) = at {
            self.point_submits = Some(submits);
            self.point_screen = Some(screen);
        }
    }

    #[cfg(test)]
    pub(crate) fn counted_from(&self) -> (Option<u64>, Option<u64>) {
        (self.point_submits, self.point_screen)
    }

    /// THE HOST'S STEP AT AN IDLE POINT MOVED THE SESSION: its keys are the harness's own.
    pub const fn host_moved(&mut self) {
        self.keyed_by_harness();
    }

    /// A SCREEN READ AS NO TASK YET THAT IS THE CONVERSATION OF THE POINT JUDGED LAST, REDRAWN.
    #[must_use]
    pub const fn redrawn_fresh(&self, submits: Option<u64>, inputs: Option<u64>) -> bool {
        self.point_tasked
            && matches!((submits, self.point_submits), (Some(now), Some(then)) if now == then)
            && matches!((inputs, self.point_inputs), (Some(now), Some(then)) if now == then)
    }

    fn unseen_turn(&self, reading: &TurnEndReading) -> bool {
        let premises = self.awaiting.is_none()
            && self.wall.is_none()
            && reading.since_limit.is_none()
            && reading.auto_continue.is_none()
            && reading.worked.is_none()
            && self.point_at.is_some()
            && !reading.taskless
            && reading.answered_point();
        if !premises {
            return false;
        }
        if self.submitted_since(reading) == Some(true) {
            return self.drawn_since(reading) && reply_stands(reading);
        }
        turn_opener(reading).is_some_and(|o| self.point_opener.is_some_and(|last| last != o))
            && !words_end_with(
                reading.said_tail.as_deref(),
                self.point_mark,
                self.point_mark_len,
            )
    }

    fn fold_needs(&mut self, tail: Option<&str>) {
        let Some(t) = tail else {
            return;
        };
        let named = needs::needs_you(t);
        if named.is_empty() && needs::defense_line(t).is_none() && !needs::says_no_needs(t) {
            return;
        }
        let key = needs::needs_key(&named);
        if key == 0 {
            self.needs = Needs::default();
        } else if key != self.needs.key {
            self.needs.standing = named;
            self.needs.key = key;
        }
    }

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

    #[allow(clippy::too_many_arguments)]
    fn track_wall(
        &mut self,
        reading: &TurnEndReading,
        kind: WallKind,
        retried: bool,
        worked: bool,
        unmade_seeded: Option<Unmade>,
        at_point: bool,
        arrival: Arrival,
        now: Instant,
    ) {
        let class = class_of(kind);
        let entitlement = entitlement_ladder(reading.reset_at.is_some(), kind)
            && !goes_on_by_itself(
                self,
                self.wall.as_ref().filter(|t| t.class == class),
                reading,
            );
        if !reading.wall_remembered
            && self.wall.as_ref().is_some_and(|t| {
                named_class(t.class)
                    && matches!((&t.conversation, &reading.conversation),
                    (Conversation::Named(ours), Conversation::Named(theirs)) if ours != theirs)
            })
        {
            self.wall = None;
            self.limit_ended = true;
        }
        match &mut self.wall {
            Some(t) if t.class == class => {
                let retried = retried || std::mem::take(&mut t.owed);
                t.pointed |= at_point;
                let anew = worked && !work_is_its_cause(kind) && (retried || t.seeded);
                if retried || (t.seeded && reading.worked.is_some()) {
                    t.since = now;
                }
                if retried && named_class(class) {
                    t.login.back_typed = false;
                    t.login.cancelled = false;
                }
                if anew {
                    t.since = now;
                    t.attempts = 0;
                    t.ups = 0;
                    t.exhausted = false;
                    t.fill = None;
                    self.limit_ended |= named_class(class);
                }
                if worked {
                    self.account_unmade = None;
                }
                let prompt = reading.answered.map(|a| a.prompt);
                if class == WallClass::Auth && prompt.is_some() && prompt != t.login.prompt {
                    t.login.prompt = prompt;
                    t.login.prompt_at = Some(now);
                    t.login.hand |= arrival.hand;
                }
            }
            _ => {
                self.limit_ended |= self.wall.as_ref().is_some_and(|t| named_class(t.class));
                self.wall = Some(WallTrack {
                    relaunch: RelaunchBooks::default(),
                    seat: Seat::default(),
                    seat_moved: false,
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
                    conversation: Conversation::Hostless,
                    program: None,
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
            if named_class(class) && !reading.wall_remembered {
                t.conversation = reading.conversation.fresh_after(&t.conversation);
                t.program = Some(reading.program);
            }
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
            t.seeded = false;
            t.entitlement = entitlement;
            if let Some(at) = reading.reset_at {
                let kept = t
                    .named
                    .filter(|n| n.said.is_some() && n.said == reading.reset_clock);
                let first = kept.map_or(now, |n| n.first);
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

    /// Records an act the loop typed (not a skipped or refused one).
    pub fn acted(&mut self, action: &TurnEndAction, reading: &TurnEndReading, now: Instant) {
        self.limit_try = None;
        if matches!(action, TurnEndAction::Restart { .. }) {
            return;
        }
        if matches!(action, TurnEndAction::Confirm { .. }) {
            self.login_confirmed = true;
            return;
        }
        let Some(rule) = action.rule_id() else {
            return;
        };
        self.point_last = false;
        self.fallback_unasked = false;
        self.account_asked |= self.account.is_some();
        self.account_startup = false;
        self.account_resume = false;
        self.reset_resume = None;
        let in_limits_place = self.in_limits_place(rule);
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
            lead: match action {
                TurnEndAction::Type { text, .. } | TurnEndAction::Accept { text, .. } => {
                    Some(crate::supervise::approvals::words_lead(text))
                }
                _ => None,
            },
            opened: reading.opened_by,
            submitted: matches!(action, TurnEndAction::Submit { .. }),
            submits: reading.submits,
            screen: reading.screen_mark,
        });
        let typed_rule = match action {
            TurnEndAction::Type { rule_id, .. } => Some(*rule_id),
            _ => None,
        };
        let again = typed_rule.is_some_and(is_wall_retry);
        let was_cut_check = std::mem::take(&mut self.cut_check);
        let was_cut_land = std::mem::take(&mut self.cut_land);
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
        if !cut_land && self.compact_owes.is_none_or(|o| !o.land) {
            self.seeded_land = None;
        }
        let typed_text = match action {
            TurnEndAction::Type { text, .. } => Some(text.clone()),
            _ => None,
        };
        self.act_words = match action {
            TurnEndAction::Type { text, .. } | TurnEndAction::Accept { text, .. } => {
                Some(text.clone())
            }
            _ => None,
        };
        let land_text = reading.autopilot == Autopilot::Land
            && typed_text.as_deref().is_some_and(is_land_words);
        let land_given = land_text && typed_text.as_deref().is_some_and(|t| t.starts_with(LAND));
        let check_or_land =
            typed_rule == Some(RULE_DONE_CHECK) || typed_rule.is_some_and(is_land_rule);
        self.cut_text = if check_or_land || carried.is_some() {
            typed_text
        } else if cut_check || cut_land || self.compact_owes.is_some() {
            self.cut_text.take()
        } else {
            None
        };
        self.checking = typed_rule == Some(RULE_DONE_CHECK) || cut_check;
        self.open.clear();
        self.repo = crate::harness::repo_work::RepoRead::Clean;
        if matches!(action, TurnEndAction::Type { text, .. } if text.contains(NEEDS_ACK)) {
            self.needs.acked = Some(self.needs.key);
        }
        if reading.autopilot != Autopilot::Land {
            self.leave_land_episode();
        }
        match typed_rule {
            Some(RULE_LAND) => self.land.typed = true,
            Some(RULE_LAND_NUDGE) => self.land.nudges = self.land.nudges.saturating_add(1),
            _ => {}
        }
        if typed_rule == Some(RULE_LAND)
            || (self.land.hold.relanded && typed_rule.is_some_and(is_land_rule))
        {
            self.land.hold = HoldStart {
                base: self.land.nudges,
                ..HoldStart::default()
            };
            self.land.unstamped = true;
        }
        self.land.typed |= land_given;
        self.landing =
            self.land.typed && (typed_rule.is_some_and(is_land_rule) || cut_land || land_text);
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

    /// THE AGENT'S PROCESS STARTED at `at` (this policy's clock).
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

    /// THE SAME START IN UNIX SECONDS, told by the loop beside [`Self::agent_started`].
    pub fn agent_started_unix(&mut self, unix: i64) {
        self.agent_start_unix = Some(unix);
        if self.fallback_started.is_none() && self.fallback_unix.is_some_and(|made| unix >= made) {
            self.fallback_started = Some(unix);
        }
        self.drop_gone_countdown();
        self.drop_gone_launch_latch();
    }

    /// THE RELAUNCH'S START-UP UNDER A LIMIT: a usage limit, spend limit or
    /// model bucket ([`named_class`]) met under an agent whose screen is all
    /// its evidence ([`screen_alone`]: Muse Code, agy, Gemini CLI), whose
    /// process came back since the limit was last read — the host names a
    /// start later than that read ([`Self::agent_started`]), or, with no
    /// host's word, a point of no agent's (a shell's prompt: it crashed,
    /// exited or upgraded) was read in between.
    pub fn relaunch_startup(
        &mut self,
        reading: &mut TurnEndReading,
        now: Instant,
        ours: impl FnOnce() -> bool,
    ) {
        if matches!(reading.phase, Phase::Busy | Phase::Prompt) {
            return;
        }
        let start = self.agent_start;
        let Some(t) = self.wall.as_mut().filter(|t| named_class(t.class)) else {
            return;
        };
        let Some(met) = t.program.filter(|p| screen_alone(*p)) else {
            return;
        };
        if reading.program != met {
            // No agent's screen: the agent left the tab. Another agent's
            // ends the limit ([`met_under_another`]).
            if !reads_conversation(reading.program) && !screen_alone(reading.program) {
                t.relaunch.away = true;
            }
            return;
        }
        let last = t.relaunch.read.unwrap_or(t.since);
        let relaunched = t.relaunch.away || start.is_some_and(|start| start > last);
        t.relaunch.away = false;
        t.relaunch.read = Some(now);
        if relaunched {
            t.relaunch.startup = true;
            reading.worked = None;
            if !t.seat_moved && !t.seat.same_account(&reading.seat) && !ours() {
                t.named = None;
                t.seat_moved = true;
                t.seat = reading.seat.clone();
            }
        } else if reading.worked.is_some() {
            t.relaunch.startup = false;
        }
        // The new process names its own seat: the old one's login is gone
        // with it ([`TurnEndState::seat_seen`]).
        if relaunched {
            self.seat_seen = Some((met, reading.seat.clone()));
        }
    }

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

    fn countdown_gone(&self, first: Instant) -> bool {
        self.agent_start.is_some_and(|start| start > first)
    }

    fn written_before_start(&self, written: Option<i64>) -> bool {
        written
            .zip(self.agent_start_unix)
            .is_some_and(|(written, start)| written < start)
    }

    fn named_countdown_gone(&self, n: &NamedReset) -> bool {
        self.countdown_gone(n.first) || self.written_before_start(n.written)
    }

    /// Records a turn the host typed (upgrade notice, carry-on; [`RULE_HOST_TURN`]), awaited like this policy's acts.
    pub fn host_typed(&mut self, now: Instant, kind: HostTurn) {
        self.keyed_by_harness();
        self.fallback_unasked = false;
        self.checking = false;
        self.seeded_check = None;
        self.seeded_land = None;
        self.open.clear();
        self.repo = crate::harness::repo_work::RepoRead::Clean;
        self.landing = false;
        self.cut_check = false;
        self.cut_land = false;
        self.cut_text = None;
        self.compact_owes = None;
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
            lead: None,
            opened: None,
            submitted: false,
            submits: None,
            screen: None,
        });
    }
}

impl TurnEndState {
    /// Record a restart the loop asked its host for ([`TurnEndAction::Restart`]).
    pub fn restarted(&mut self, why: &Restart, made: bool, reading: &TurnEndReading, now: Instant) {
        self.keyed_by_harness();
        if made && !matches!(why, Restart::Account { .. }) {
            self.account_startup = self.account.is_some();
        }
        match why {
            Restart::Memory => {}
            Restart::Model { to } if made => {
                let track = self.wall.as_ref().filter(|t| t.class == WallClass::Model);
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
                self.fallback_unasked = true;
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
                self.account_unmade = None;
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
                self.credits = None;
            }
            Restart::Account {
                at: AccountAt::Wall,
                ..
            } => {
                self.account_unmade = Some(Unmade::Again);
                self.account_unmade_ms = None;
            }
            Restart::Account {
                to,
                at: AccountAt::Ahead,
            } => self.account_ahead_unmade = Some((to.clone(), now)),
        }
    }

    /// [`Self::restarted`] for a restart the host did NOT make, with its word for why (`step`; `None`: no host answered).
    pub fn restart_unmade(
        &mut self,
        why: &Restart,
        step: Option<&str>,
        reading: &TurnEndReading,
        now: Instant,
    ) {
        self.restarted(why, false, reading, now);
        if self.account_unmade.is_some()
            && matches!(
                why,
                Restart::Account {
                    at: AccountAt::Wall,
                    ..
                }
            )
        {
            self.account_unmade = Some(Unmade::of(step));
        }
    }
}

/// The `/model` alias a bucket's notice names (`You've reached your Fable limit` → `fable`, `Opus 4.1 limit reached` → `opus`).
#[must_use]
pub fn bucket_model(message: &str) -> Option<String> {
    message
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .find(|w| MODEL_ALIASES.contains(&w.as_str()))
}

/// The work a finished turn's done row reports (`✻ Cooked for 3m 2s · done 4:24 PM` → 182 s).
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

/// How far a done row may stand off the act whose turn it closes ([`act_turn_work`]).
pub const ACT_TURN_SLACK: Duration = Duration::from_secs(2 * 60);

/// THE WORK OF THE TURN AN ACT BEGAN.
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
    let least = i128::from(ended) - i128::from(act);
    let most = least + 59;
    if span + slack < least || span > most + slack {
        return None;
    }
    let most = u64::try_from(most).unwrap_or(u64::MAX);
    let awake = work.min(Duration::from_secs(most)).checked_sub(asleep)?;
    (awake >= min).then_some(awake)
}

/// The clock time of a turn's DONE ROW (`✻ Sautéed for 1d 2h 58m · done 12:19 PM · 1 shell still running` → `12:19 PM`).
#[must_use]
pub fn done_row_clock(rows: &[String]) -> Option<String> {
    let row = status_line(rows).filter(|r| is_done_row(r))?;
    let (_, rest) = row.split_once(" done ")?;
    let clock = rest.split(" · ").next()?.trim();
    (clock.starts_with(|c: char| c.is_ascii_digit())).then(|| clock.to_string())
}

/// Whether a suggestion is a continuation.
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
    Plain,
    Offer,
    Question,
    Stop(String),
    Irreversible(String),
}

fn asked_by(tail: Option<&str>) -> Option<(String, bool)> {
    let rest = tail.map(needs::without_needs);
    match classify_said(rest.as_deref()) {
        Said::Irreversible(why) => Some((why, true)),
        Said::Stop(why) => Some((why, false)),
        Said::Question => Some(("the worker asked a question".to_string(), false)),
        Said::Offer | Said::Plain => None,
    }
}

/// Judges the worker's last words ([`Said`]).
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

struct LastTail<T>(std::cell::RefCell<Option<(String, T)>>);

impl<T: Clone> LastTail<T> {
    const fn new() -> Self {
        LastTail(std::cell::RefCell::new(None))
    }

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
    let listed_choice = listed >= 2 && asks_to_choose(ask);
    let chooses = asks && (listed_choice || either_or(ask));
    let choice_phrase = CHOICE_PHRASES
        .iter()
        .filter_map(|p| flat.find(p).map(|at| (*p, at)))
        .min_by_key(|&(_, at)| at);
    let phrase_choice = choice_phrase.filter(|&(p, at)| phrase_asks(&flat, &lines, p, at));
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

const YES_NO_MARKERS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "(y/n)?", "[y/n]?"];

fn yes_no_asks(text: &str) -> String {
    let mut out = text.to_string();
    for m in YES_NO_MARKERS {
        out = out.replace(m, "?");
    }
    out
}

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

/// Leads that, before an either/or's ` or `, make it a choice the worker would act on (`should I … or …?`, `which would you prefer: … or …?`).
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

fn either_or(ask: &str) -> bool {
    ask.find(" or ")
        .is_some_and(|at| CHOICE_LEADS.iter().any(|l| ask[..at].contains(l)))
}

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

fn phrase_asks(flat: &str, lines: &[&str], p: &str, at: usize) -> bool {
    let rest = &flat[at..];
    let bytes = rest.as_bytes();
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

pub(crate) fn destructive_word(text: &str) -> Option<&'static str> {
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

fn inflects(word: &str, base: &str) -> bool {
    if word == base {
        return true;
    }
    let Some(rest) = word.strip_prefix(base) else {
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
    let last = base.chars().last().unwrap_or(' ');
    rest.strip_prefix(last)
        .is_some_and(|rest| matches!(rest, "ed" | "ing" | "en"))
}

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

/// What to type as a continuation.
#[must_use]
pub fn continuation_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.continue_text, rules)
}

/// What to type where the worker claims it is done under Cruise.
#[must_use]
pub fn done_check_text(rules: Option<&str>, irreversible: bool) -> String {
    act_words(DONE_CHECK, &[reversible_clause(irreversible)], rules)
}

/// Whether `typed` — a prompt's words as a transcript keeps them — is the
/// done check.
#[must_use]
pub fn is_done_check(typed: &str) -> bool {
    let typed = typed.trim();
    if typed
        .strip_prefix(DONE_CHECK)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    {
        return true;
    }
    RETIRED_DONE_CHECKS.iter().any(|check| {
        typed.strip_prefix(*check).is_some_and(|rest| {
            let rest = rest
                .strip_prefix(' ')
                .and_then(|tail| tail.strip_prefix(DONE_CHECK_REVERSIBLE))
                .unwrap_or(rest);
            rest.is_empty() || rest.starts_with(RULES_OPEN)
        })
    })
}

/// THE OPEN-WORK SUFFIX (the [`DONE_CHECK`] and the [`LAND_NUDGE`] where the claim or the defense named open work).
#[must_use]
pub fn open_work_suffix(found: &[&str]) -> Option<String> {
    open_named(found, OPEN_NAMED)
}

fn open_named(found: &[&str], most: usize) -> Option<String> {
    let most = most.max(1);
    let named = match found.get(..most) {
        _ if found.is_empty() => return None,
        Some(first) if found.len() > most => format!("{}, …", first.join(", ")),
        _ => found.join(", "),
    };
    Some(format!(
        "Your report still names open work ({named}): finish and land it first."
    ))
}

/// THE REPOSITORY SUFFIX: its finding in a few words, cut to [`REPO_SUMMARY_UNITS`] so the act stays a line the composer holds as typed.
#[must_use]
pub fn repo_work_suffix(repo: &crate::harness::repo_work::RepoWork) -> String {
    repo_said(&fit_units(&repo.summary(), REPO_SUMMARY_UNITS))
}

fn repo_said(summary: &str) -> String {
    if summary.is_empty() {
        return REPO_BARE.to_string();
    }
    format!(
        "Your repository still holds work no remote main has ({summary}): commit, merge and push \
         it to the remote main first."
    )
}

const REPO_BARE: &str = "Your repository still holds work no remote main has: commit, merge \
                         and push it to the remote main first.";

/// THE REPOSITORY SUFFIX WHERE THE PROBE DID NOT FINISH ([`crate::harness::repo_work::RepoRead::Unread`]).
pub const REPO_UNREAD: &str = "Your repository could not be read: show its git status yourself \
                               and land what it holds first.";

const REPO_SUMMARY_UNITS: usize = 160;

/// RETIRED: the fixed words the last build before Autopilot Cruise opened its open-items challenge with ([`RULE_OPEN_ITEMS`]).
pub const OPEN_ITEMS_HEAD: &str = "Your report says done but still names open work";

/// RETIRED: the open-items challenge's opening words before [`OPEN_ITEMS_HEAD`].
pub const OPEN_ITEMS_HEADS_BEFORE: [&str; 2] = [
    "Your report ends with ATERM-TASK-DONE but still names open",
    "Your report still names open",
];

/// RETIRED: the fixed words the uncommitted-work challenge opened with ([`RULE_UNCOMMITTED`], [`is_challenge_text`]).
pub const UNCOMMITTED_HEAD: &str = "Your report says done but work is not committed and pushed";

/// Whether `typed` is one of the retired challenges ([`is_challenge_rule`]), by its opening words.
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

const OPEN_NAMED: usize = 4;

const fn reversible_clause(irreversible: bool) -> &'static str {
    if irreversible {
        DONE_CHECK_REVERSIBLE
    } else {
        ""
    }
}

const fn ack_clause(ack: bool) -> &'static str {
    if ack { NEEDS_ACK } else { "" }
}

fn act_words(base: &str, after: &[&str], rules: Option<&str>) -> String {
    let parts: Vec<Suffix<'_>> = after.iter().map(|a| Suffix::Fixed(a)).collect();
    fitted_act(base, &parts, rules)
}

#[derive(Clone, Copy)]
enum Suffix<'a> {
    Fixed(&'a str),
    Open(&'a [&'static str]),
    Repo(&'a crate::harness::repo_work::RepoRead),
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

fn fitted_act(base: &str, parts: &[Suffix<'_>], rules: Option<&str>) -> String {
    let fixed = units(base)
        + parts
            .iter()
            .map(|p| match p {
                Suffix::Fixed(f) if !f.is_empty() => 1 + units(f),
                _ => 0,
            })
            .sum::<usize>();
    let room = VERBATIM_UNITS.saturating_sub(fixed);
    let found: &[&'static str] = parts
        .iter()
        .find_map(|p| match p {
            Suffix::Open(f) => Some(*f),
            _ => None,
        })
        .unwrap_or(&[]);
    let repo_read = parts.iter().find_map(|p| match p {
        Suffix::Repo(r) => Some(*r),
        _ => None,
    });
    let repo = repo_read.and_then(|r| r.found());
    let unread = repo_read.is_some_and(|r| r.is_unread());
    let repo_in = |room: usize| -> Option<String> {
        if unread {
            return (units(REPO_UNREAD) < room).then(|| REPO_UNREAD.to_string());
        }
        let repo = repo?;
        let whole = repo_work_suffix(repo);
        if units(&whole) < room {
            return Some(whole);
        }
        let frame = units(&repo_said("x"));
        let cut = fit_units(&repo.summary(), room.saturating_sub(frame)).into_owned();
        let said = repo_said(&cut);
        (units(&said) < room).then_some(said)
    };
    let mut chosen: (Option<String>, Option<String>) = (None, None);
    for most in (0..=OPEN_NAMED.min(found.len())).rev() {
        let open = (most > 0).then(|| open_named(found, most)).flatten();
        let left = room.saturating_sub(open.as_ref().map_or(0, |o| 1 + units(o)));
        if open.as_ref().is_some_and(|o| 1 + units(o) > room) {
            continue;
        }
        let repo_s = repo_in(left);
        if repo_s.is_some() || (repo.is_none() && !unread) || most == 0 {
            chosen = (open, repo_s);
            break;
        }
    }
    let (open, repo_s) = chosen;
    let mut text = base.to_string();
    for p in parts {
        let said = match p {
            Suffix::Fixed(f) => Some(*f),
            Suffix::Open(_) => open.as_deref(),
            Suffix::Repo(_) => repo_s.as_deref(),
        };
        if let Some(a) = said.filter(|a| !a.is_empty()) {
            text.push(' ');
            text.push_str(a);
        }
    }
    with_rules(&text, rules)
}

/// What to type as an answer to a question for a person.
#[must_use]
pub fn answer_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.answer_text, rules)
}

/// The answer to a prose question under `cfg.question_priority`.
#[must_use]
pub fn prose_answer(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    act_words(prose_words(cfg), &[NEEDS_HINT], rules)
}

fn prose_words(cfg: &SupervisorConfig) -> &str {
    cfg.question_priority
        .text()
        .unwrap_or(cfg.answer_text.as_str())
}

/// The answer to a question or choice naming an irreversible act ([`Said::Irreversible`]).
pub const REVERSIBLE_ANSWER: &str = "Nobody is here to approve an irreversible step: take the \
                                     option that deletes, overwrites and force-pushes nothing, \
                                     and keep going.";

/// [`REVERSIBLE_ANSWER`], then [`NEEDS_HINT`], with the standing rules when there are any.
#[must_use]
pub fn reversible_answer(rules: Option<&str>) -> String {
    act_words(REVERSIBLE_ANSWER, &[NEEDS_HINT], rules)
}

/// The most UTF-16 code units a text this policy types may hold and still be drawn as typed.
pub const VERBATIM_UNITS: usize = 800;

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

const RULES_OPEN: &str = " (standing rules: ";

/// THE ACTS AN `aterm:` FACT RIDES ([`crate::supervise::facts`]; system awareness §14.4): prompts
/// this policy types anyway, in its own words — the continuation, the done check, Land's
/// instruction and its nudge. Never an answer (a fact beside it could steer the choice), a
/// wall's retry or an act typed in a limit's place ([`TurnEndState::in_limits_place`]), an
/// accepted suggestion (the vendor's words), the person's draft, a command, or a line of the
/// upgrade's or a relaunch's.
pub const FACT_RIDES: [&str; 4] = [RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND, RULE_LAND_NUDGE];

/// How a fact opens after the text it rides and its standing rules.
const FACT_OPEN: &str = " (aterm: ";

/// The words a fact adds to the act it rides: ` (aterm: {text})`.
#[must_use]
pub fn fact_suffix(fact: &crate::supervise::facts::Fact) -> String {
    format!("{FACT_OPEN}{})", fact.text)
}

/// Whether `action` is an act a fact may ride in `state` ([`FACT_RIDES`]).
#[must_use]
pub fn fact_may_ride(state: &TurnEndState, action: &TurnEndAction) -> bool {
    matches!(action, TurnEndAction::Type { rule_id, .. }
        if FACT_RIDES.contains(rule_id) && !state.in_limits_place(rule_id))
}

/// Whether `action` carries `fact` ([`fact_rides`]): the act whose taking tells it.
#[must_use]
pub fn carries_fact(action: &TurnEndAction, fact: &crate::supervise::facts::Fact) -> bool {
    matches!(action, TurnEndAction::Type { text, rule_id }
        if FACT_RIDES.contains(rule_id) && text.ends_with(&fact_suffix(fact)))
}

/// `action` with `fact` after its text and standing rules, where it is an act the fact rides
/// ([`fact_may_ride`]) and the whole still fits [`VERBATIM_UNITS`]. ALL OR NOTHING: a fact that
/// does not fit is left off whole (the rules are never cut further for it, and the fact is never
/// cut) and stays untold for a later act.
fn fact_rides(
    state: &TurnEndState,
    action: TurnEndAction,
    fact: Option<&crate::supervise::facts::Fact>,
    trace: &mut dyn Trace,
) -> TurnEndAction {
    let Some(fact) = fact.filter(|_| fact_may_ride(state, &action)) else {
        return action;
    };
    let TurnEndAction::Type { text, rule_id } = action else {
        return action;
    };
    let said = format!("{text}{}", fact_suffix(fact));
    if said.encode_utf16().count() > VERBATIM_UNITS {
        note(trace, "fact", Some(rule_id), || {
            Outcome::Unproven(format!(
                "{} {:016x} would pass {VERBATIM_UNITS} units: typed without it",
                fact.key.kind.word(),
                fact.key.id
            ))
        });
        return TurnEndAction::Type { text, rule_id };
    }
    note(trace, "fact", Some(rule_id), || {
        Outcome::Decided(format!(
            "{} {:016x} rides",
            fact.key.kind.word(),
            fact.key.id
        ))
    });
    TurnEndAction::Type {
        text: said,
        rule_id,
    }
}

fn later(at: Instant, d: Duration) -> Instant {
    at.checked_add(d).unwrap_or(at)
}

/// The decision at one point (module header).
#[must_use]
pub fn decide_turn_end(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    decide_turn_end_traced(state, reading, cfg, now, &mut NoTrace)
}

fn ended_at(trace: &mut dyn Trace, check: &'static str, action: TurnEndAction) -> TurnEndAction {
    note(trace, check, action.rule_id(), || {
        Outcome::Decided(format!("{action:?}"))
    });
    action
}

/// [`decide_turn_end`], each check it consults written to `trace` as it is consulted ([`super::trace`]).
/// The reading's fact rides the act decided last ([`fact_rides`]), once.
#[must_use]
pub fn decide_turn_end_traced(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    trace: &mut dyn Trace,
) -> TurnEndAction {
    let action = decide_unridden(state, reading, cfg, now, trace);
    // `[harness] facts = false` limits wherever the policy decides, a shadow's candidate too.
    let fact = reading.fact.as_ref().filter(|_| cfg.facts);
    fact_rides(state, action, fact, trace)
}

/// [`decide_turn_end_traced`] before the reading's fact rides the act: the readings it decides
/// again on come back here, so a fact rides once.
fn decide_unridden(
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
    if state.login_left(reading) && !(state.account_resume && cfg.resume_limits) {
        let plain = TurnEndReading {
            wall: None,
            wall_message: String::new(),
            ..reading.clone()
        };
        note(trace, "login-left", None, || Outcome::Passed);
        return decide_unridden(state, &plain, cfg, now, trace);
    }
    if state.bucket_left(reading) {
        let plain = TurnEndReading {
            wall: None,
            wall_message: String::new(),
            ..reading.clone()
        };
        note(trace, "bucket-left", None, || Outcome::Passed);
        return decide_unridden(state, &plain, cfg, now, trace);
    }
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
                return decide_unridden(state, &unrestartable, cfg, now, trace);
            }
            Some(_) => {}
        }
    }
    if reading.wall == Some(WallKind::Memory) && !reading.interrupted {
        return ended_at(trace, "memory", memory_restart(cfg, reading));
    }
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
    let back_elsewhere = !state.landed()
        && state.login_track().is_some()
        && (login_back_elsewhere(state, reading) || login_left_renewed(state, reading));
    let rests = state.task_done()
        && !(reading.autopilot == Autopilot::Cruise && state.landed)
        && !back_elsewhere;
    if !reading.at_point(state.stale_input) || reading.taskless || rests {
        return ended_at(trace, "no-task", TurnEndAction::Nothing);
    }
    note(trace, "no-task", None, || Outcome::Passed);
    let login_resuming = reading.login_back
        && !reading.interrupted
        && matches!(reading.composer, Composer::Empty | Composer::Placeholder(_))
        && state
            .wall
            .as_ref()
            .is_some_and(|t| named_class(t.class) && t.attempts == 0);
    let held = match (
        (!login_resuming)
            .then(|| person_hold(state, reading, cfg, now))
            .flatten(),
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
    let drafted = match act.rule_id() {
        Some(RULE_LOGIN) if reading.composer.drafted() => TurnEndAction::Escalate {
            reason: format!(
                "the login is gone: {} (a draft stands in the composer; {})",
                state.wall_words(reading),
                sign_in_how(reading.program, state.wall_words(reading))
            ),
        },
        Some(rule_id) if reading.composer == Composer::Typed => TurnEndAction::Submit { rule_id },
        Some(_) if reading.composer == Composer::Own => TurnEndAction::Escalate {
            reason: OWN_WORDS_STAND.to_string(),
        },
        Some(_) if reading.composer == Composer::Diverted => TurnEndAction::Escalate {
            reason: MENU_DIVERTS.to_string(),
        },
        _ => return act,
    };
    ended_at(trace, "draft", drafted)
}

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

/// HOW LONG THE KEY THAT SENT THE PROMPT A LOST LOGIN ANSWERED HOLDS `/login` ([`login_quiet`]).
pub const LOGIN_QUIET: Duration = Duration::from_secs(10);

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

pub(crate) fn login_back_elsewhere(state: &TurnEndState, reading: &TurnEndReading) -> bool {
    matches!(reading.login_shared, Some(LoginShared::Back { .. }))
        && !state
            .wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Auth && t.login.back_typed)
}

fn login_opening_wait(state: &TurnEndState, now: Instant) -> Option<TurnEndAction> {
    state
        .login_opening(now)
        .map(|until| TurnEndAction::WaitUntil {
            until,
            why: "the /login dialog to open".to_string(),
            wait: WaitKind::Other,
        })
}

fn budget_spent(state: &TurnEndState, cfg: &SupervisorConfig, now: Instant) -> bool {
    cfg.continue_per_hour != 0 && state.acts_in_window(now) >= cfg.continue_per_hour as usize
}

const BUDGET_REACHED: &str = "continue_per_hour = ";

/// THE HOUR'S TYPING BUDGET ESCALATES AND BLOCKS NOTHING.
#[must_use]
pub fn budget_frees(
    state: &TurnEndState,
    cfg: &SupervisorConfig,
    action: &TurnEndAction,
    now: Instant,
) -> Option<Instant> {
    match action {
        TurnEndAction::Escalate { reason } if reason.starts_with(BUDGET_REACHED) => {
            Some(state.budget_frees_at(cfg.continue_per_hour, now))
        }
        _ => None,
    }
}

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
                    "{BUDGET_REACHED}{} reached: {} typed in the last {} min",
                    cfg.continue_per_hour,
                    state.acts_in_window(now),
                    state.timing.window.as_secs() / 60
                ),
            }
        } else {
            act
        }
    };
    let continue_as = |rule: &'static str| {
        if rule == RULE_LIMIT_RESUME
            && let Some(act) = limit_end(state, reading, cfg, &budgeted)
        {
            return act;
        }
        let text = match reading.autopilot {
            Autopilot::Land => land_words(state, reading).0,
            Autopilot::Cruise | Autopilot::Off => continuation_text(cfg, reading.rules.as_deref()),
        };
        budgeted(TurnEndAction::Type {
            text,
            rule_id: rule,
        })
    };
    let type_as = |rule: &'static str, text: &str| {
        budgeted(TurnEndAction::Type {
            text: with_rules(text, reading.rules.as_deref()),
            rule_id: rule,
        })
    };
    let held =
        |trace: &mut dyn Trace, act: TurnEndAction| match credits_hold(state, reading, cfg, now) {
            Some(hold) => ended_at(
                trace,
                "credits-hold",
                held_by_credits(state, reading, cfg, now, act, hold),
            ),
            None => act,
        };
    if let Some(t) = unread_track(state, reading) {
        let look = later(now, LIMIT_CONVERSATION_LOOK);
        let until = t
            .named
            .filter(|n| now < n.at)
            .map_or(look, |n| n.at.min(look));
        return ended_at(
            trace,
            "limit-conversation-unread",
            TurnEndAction::WaitUntil {
                until,
                why: "the remembered limit's conversation is unread; no act on its account"
                    .to_string(),
                wait: WaitKind::Other,
            },
        );
    }
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
        return match class_of(kind) {
            WallClass::Retry | WallClass::Context | WallClass::Usage | WallClass::Model => {
                held(trace, act)
            }
            WallClass::Auth | WallClass::Memory => act,
        };
    }
    if let Some(t) = &state.wall
        && t.class == WallClass::Auth
        && !reading.login_back
    {
        let shown = still_shown(t, reading).or_else(|| {
            waited_login(state, reading, cfg, now).map(|waited| (WallKind::Auth, waited))
        });
        let Some((kind, shown)) = shown else {
            let held = login_opening_wait(state, now).unwrap_or(TurnEndAction::Nothing);
            if reading.login_screen.is_none() && reading.composer != Composer::Absent {
                let back = login_back_elsewhere(state, reading)
                    .then(|| type_as(RULE_LOGIN_BACK, &login_back_text(reading.program)));
                let act = match (login_wall_moves(state, reading, cfg, now), back) {
                    (Some(to), back) => Some(TurnEndAction::Restart {
                        why: Restart::Account {
                            to: to.to_string(),
                            at: AccountAt::Wall,
                        },
                        rule_id: RULE_ACCOUNT,
                        otherwise: Box::new(back.unwrap_or_else(|| held.clone())),
                    }),
                    (None, back) => back,
                };
                if let Some(act) = act {
                    return ended_at(trace, "login-held", act);
                }
            }
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
    if let Some(act) = reset_continuation(state, cfg, now, continue_as) {
        return ended_at(trace, "reset-continuation", act);
    }
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
    if state.account_resume && cfg.resume_limits {
        return ended_at(trace, "account-resume", resume_limit(continue_as));
    }
    let cont = continue_policy(state, reading, cfg, now, budgeted);
    if let Some(to) = move_ahead(state, reading, cfg, now)
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return ended_at(trace, "account-ahead", ahead_to(to, cont));
    }
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
    let done = state.done >= 1 && !state.exchange && !reading.interrupted;
    if let Some(act) = words_gate(cfg, asked.as_deref(), done) {
        return act;
    }
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
    let ack = state.needs_ack_owed();
    match &reading.composer {
        Composer::Placeholder(s) if suggestion_allowed(s) && !ack => {
            budgeted(TurnEndAction::Accept {
                text: s.clone(),
                rule_id: RULE_SUGGESTION,
            })
        }
        _ => budgeted(TurnEndAction::Type {
            text: act_words(
                &cfg.continue_text,
                &[
                    reversible_clause(ack && state.needs_destroy()),
                    ack_clause(ack),
                ],
                reading.rules.as_deref(),
            ),
            rule_id: RULE_CONTINUE,
        }),
    }
}

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
    let ack = state.needs_ack_owed();
    let ack_s = ack_clause(ack);
    let need_destroys = ack && state.needs_destroy();
    if done {
        return Some(budgeted(TurnEndAction::Type {
            text: fitted_act(
                DONE_CHECK,
                &[
                    Suffix::Open(&state.open),
                    Suffix::Repo(&state.repo),
                    Suffix::Fixed(reversible_clause(irreversible || need_destroys)),
                    Suffix::Fixed(ack_s),
                ],
                rules,
            ),
            rule_id: RULE_DONE_CHECK,
        }));
    }
    if !asked {
        return None;
    }
    Some(budgeted(if irreversible {
        TurnEndAction::Type {
            text: act_words(REVERSIBLE_ANSWER, &[NEEDS_HINT, ack_s], rules),
            rule_id: RULE_ANSWER,
        }
    } else {
        TurnEndAction::Type {
            text: act_words(
                prose_words(cfg),
                &[NEEDS_HINT, reversible_clause(need_destroys), ack_s],
                rules,
            ),
            rule_id: cfg.question_priority.prose_rule_id(),
        }
    }))
}

fn land_policy(
    state: &TurnEndState,
    reading: &TurnEndReading,
    asked: bool,
    done: bool,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> TurnEndAction {
    let (text, rule_id) = if done || !asked {
        land_words(state, reading)
    } else {
        let ack_s = ack_clause(state.needs_ack_owed());
        (
            act_words(LAND_ANSWER, &[ack_s], reading.rules.as_deref()),
            RULE_LAND_ANSWER,
        )
    };
    budgeted(TurnEndAction::Type { text, rule_id })
}

fn land_words(state: &TurnEndState, reading: &TurnEndReading) -> (String, &'static str) {
    let rules = reading.rules.as_deref();
    let ack = state.needs_ack_owed();
    let ack_s = ack_clause(ack);
    let rev = reversible_clause(ack && state.needs_destroy());
    if state.land.typed {
        (
            fitted_act(
                LAND_NUDGE,
                &[
                    Suffix::Open(&state.open),
                    Suffix::Repo(&state.repo),
                    Suffix::Fixed(rev),
                    Suffix::Fixed(ack_s),
                ],
                rules,
            ),
            RULE_LAND_NUDGE,
        )
    } else {
        (act_words(LAND, &[rev, ack_s], rules), RULE_LAND)
    }
}

fn is_land_words(text: &str) -> bool {
    text.starts_with(LAND) || text.starts_with(LAND_NUDGE) || text.starts_with(LAND_ANSWER)
}

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

pub(crate) const fn entitlement_ladder(placed: bool, kind: WallKind) -> bool {
    !placed && matches!(kind, WallKind::Spend | WallKind::ModelBucket { .. })
}

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
    // ladder. A usage window with no readable reset still lifts by itself, so it
    // keeps the short one.
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

fn left_named(t: &WallTrack, reading: &TurnEndReading) -> Option<NamedReset> {
    t.named
        .filter(|_| named_class(t.class) && !reading.login_back)
}

fn left_undecided_login(t: &WallTrack, reading: &TurnEndReading) -> bool {
    t.class == WallClass::Auth && !t.pointed && !reading.login_back
}

fn left_waited_login(state: &TurnEndState, t: &WallTrack, reading: &TurnEndReading) -> bool {
    t.class == WallClass::Auth
        && t.attempts == 0
        && (state.account.is_some() || reading.login_label.is_some())
        && !reading.login_back
}

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
    if !done && asked.is_none() && !state.limit_elsewhere(reading) {
        return None;
    }
    if let Some(act) = words_gate(cfg, asked.as_deref(), done) {
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

pub(crate) const LIMIT_CONVERSATION_LOOK: Duration = Duration::from_secs(5 * 60);

fn unread_track<'a>(state: &'a TurnEndState, reading: &TurnEndReading) -> Option<&'a WallTrack> {
    state.wall.as_ref().filter(|t| {
        named_class(t.class)
            && (reading.wall.is_none() || reading.wall_remembered)
            && !limit_conversation_known(t, reading)
            && !state.relaunched_off(t)
    })
}

impl TurnEndState {
    #[must_use]
    pub(crate) fn held_unread_from(&self, reading: &TurnEndReading) -> Option<Instant> {
        unread_track(self, reading).map(|t| t.named.map_or(t.since, |n| n.at))
    }
}

fn limit_conversation_ours(t: &WallTrack, reading: &TurnEndReading) -> bool {
    limit_conversation_known(t, reading) && t.conversation == reading.conversation
}

const fn limit_conversation_known(t: &WallTrack, reading: &TurnEndReading) -> bool {
    matches!(
        (&t.conversation, &reading.conversation),
        (Conversation::Hostless, Conversation::Hostless)
            | (Conversation::Named(_), Conversation::Named(_))
    )
}

fn resume_limit(continue_as: &dyn Fn(&'static str) -> TurnEndAction) -> TurnEndAction {
    continue_as(RULE_LIMIT_RESUME)
}

fn retry_wait(state: &TurnEndState, attempts: usize) -> Option<Duration> {
    crate::supervise::ladder::Ladder(&state.timing.retry_backoff).at(attempts)
}

const fn moves_accounts(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Gemini
        | Program::Generic => false,
    }
}

fn account_rotates(state: &TurnEndState, reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.accounts != crate::harness::accounts::Mode::Off && relaunch_askable(state, reading, cfg)
}

fn login_follows(state: &TurnEndState, reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.logins && reading.login_label.is_some() && relaunch_askable(state, reading, cfg)
}

const fn relaunch_askable(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
) -> bool {
    cfg.resume_limits
        && relaunch_on(reading, cfg)
        && reading.restartable
        && moves_accounts(reading.program)
        && state.account_unmade.is_none()
        && !reading.composer.drafted()
}

pub(super) fn relaunch_reaches(reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.resume_limits && relaunch_on(reading, cfg) && relaunch_lane(reading)
}

pub(super) const fn relaunch_lane(reading: &TurnEndReading) -> bool {
    moves_accounts(reading.program)
        && (reading.restartable || reading.relaunch_off || reading.upgrading)
}

pub(super) const fn relaunch_on(reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.relaunch && !reading.relaunch_off
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenewStep {
    Draft,
    ByHand(&'static str),
}

fn renewed_draft_reason(composer: &Composer) -> String {
    let what = match composer {
        Composer::Own => "aterm's own words stand unsent in the prompt: clear them",
        Composer::Diverted => {
            "a draft stands with Claude Code's completion menu open: close the menu, then send \
             or clear the draft"
        }
        _ => "a draft stands in the composer: send or clear it",
    };
    format!("the login was renewed; {what}, and the tab moves onto the new login")
}

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
    } else if !relaunch_on(reading, cfg) {
        RenewStep::ByHand(BY_HAND_WHYS[0])
    } else if !cfg.resume_limits {
        RenewStep::ByHand(BY_HAND_WHYS[1])
    } else {
        RenewStep::ByHand(BY_HAND_WHYS[2])
    })
}

use super::notice::BY_HAND_WHYS;

fn login_move(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    to: &str,
) -> bool {
    reading.login_label.as_deref() == Some(to) && login_follows(state, reading, cfg)
}

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
            let own_reset = reading.reset_at.is_some_and(|reset| at >= reset);
            if at < due && !own_reset {
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
        (AccountAdvice::Probe { .. }, today) => today,
    }
}

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
    let untyped = reading.wall == Some(WallKind::Auth)
        && state.wall_attempts(WallKind::Auth).unwrap_or(0) == 0;
    let back = state.login_track().is_some() && login_back_elsewhere(state, reading);
    ((untyped || back || login_left_renewed(state, reading))
        && (moved || login_move(state, reading, cfg, to))
        && !state.login_left(reading)
        && state.account_moves_of(now).0 < ACCOUNT_MOVES_PER_WINDOW)
        .then_some(to.as_str())
}

fn login_left_renewed(state: &TurnEndState, reading: &TurnEndReading) -> bool {
    state.login_track().is_some()
        && reading.login_interrupted
        && reading.login_screen.is_none()
        && reading.login_superseded
}

pub(super) fn login_wall_waits(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<TurnEndAction> {
    let moved = state.account.is_some() && cfg.accounts != crate::harness::accounts::Mode::Off;
    let renewed = reading.login_label.is_some() && cfg.logins && reading.login_superseded;
    if reading.wall != Some(WallKind::Auth)
        || !(moved || renewed)
        || state.login_left(reading)
        || !relaunch_reaches(reading, cfg)
        || state.account_unmade.is_some()
        || state.wall_attempts(WallKind::Auth).unwrap_or(0) > 0
        || reading.composer.drafted()
    {
        return None;
    }
    let (n, free) = state.account_moves_of(now);
    let (until, why) = match (reading.advised(), free) {
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
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Muse
        | Program::Generic => None,
    }
}

fn names_usage_credits(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("usage credits") || lower.contains("extra usage")
}

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
            let rung = |ladder: &[Duration], n: usize| {
                crate::supervise::ladder::Ladder(ladder)
                    .at(n)
                    .map_or(now, |w| later(since, w))
            };
            match (cause, reading.reach) {
                (ApiCause::Unreachable, Reach::Up { .. }) => wait_then(
                    ups.checked_sub(1)
                        .map_or(since, |n| rung(&t.net_backoff, n)),
                    format!("api-error back but met again: retry {}", ups + 1),
                    act(RULE_API_BACK),
                ),
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
                (ApiCause::CutOff, _) => wait_then(
                    attempts
                        .checked_sub(1)
                        .map_or(since, |n| rung(&t.net_backoff, n)),
                    format!("api-error cut off: retry {}", attempts + 1),
                    act(RULE_API_CUTOFF),
                ),
                (ApiCause::Server, _) => wait_then(
                    rung(&t.retry_backoff, attempts),
                    format!("{} retry {}", kind.name(), attempts + 1),
                    act(RULE_API_RETRY),
                ),
                (ApiCause::Request, _) => wait_then(
                    rung(&t.refused_backoff, attempts),
                    format!("api-error refused: retry {}", attempts + 1),
                    act(RULE_API_RETRY),
                ),
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
            if credits_only && let Some(act) = credits_wall(state, reading, cfg, msg, &wait_out) {
                return act;
            }
            wait_out()
        }
        WallKind::ModelBucket { consent } => {
            if consent && attempts == 0 && cfg.continue_policy && cfg.usage_credits {
                return continue_as(RULE_CONSENT);
            }
            let Some(fallback) = &cfg.model_fallback else {
                return wait_out();
            };
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
        WallKind::Memory => memory_restart(cfg, reading),
        WallKind::Auth => {
            let back = login_back_elsewhere(state, reading);
            if (attempts > 0 || track.is_some_and(|t| t.login.cancelled)) && !back {
                return login_opening_wait(state, now).unwrap_or(TurnEndAction::Nothing);
            }
            match renew_step(reading, cfg) {
                Some(RenewStep::Draft) => return esc(renewed_draft_reason(&reading.composer)),
                Some(RenewStep::ByHand(why)) => {
                    return esc(format!(
                        "the login was renewed, but {why}: type /login in the tab and sign in"
                    ));
                }
                None => {}
            }
            match &reading.login_shared {
                Some(LoginShared::Back { .. }) if back => {
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
            budgeted(TurnEndAction::TypeCommand {
                command: login.to_string(),
                rule_id: RULE_LOGIN,
                then: Then::Nothing,
            })
        }
    }
}

fn memory_restart(cfg: &SupervisorConfig, reading: &TurnEndReading) -> TurnEndAction {
    let banner = aterm_phase::memory_banner_head(&reading.wall_message);
    let reason = match reading.resume.as_deref() {
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
