// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OPEN-WORK READERS: what a report still names open ([`open_work`], [`open_work_strict`],
//! [`open_work_found`]), and Land's strict reading of a `Nothing left:` defense
//! ([`land_open_found`], [`clean_defense`]), over the words alone.

use std::ops::Range;

use crate::needs;
use crate::words::{is_finished_line, says_done};

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
    pub name: &'static str,
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
    once(open_work_found(tail, land, defense))
}

/// Each found word once, in the order found.
fn once(found: Vec<Found>) -> Vec<&'static str> {
    let mut once: Vec<&'static str> = Vec::with_capacity(found.len());
    for word in found.into_iter().map(|f| f.word) {
        if !once.contains(&word) {
            once.push(word);
        }
    }
    once
}

/// A piece of open work a report names: the reader's word for it, and the sentence it stood in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The reader's word, as [`open_work`] and [`open_work_strict`] name it.
    pub word: &'static str,
    /// The sentence the word stood in, in the agent's own words, with markdown's emphasis, code
    /// ticks and the marks the reader dropped left out and its spaces closed up; for a head
    /// over items `<head>: <first item>`, for an unticked line `to do: <item>`, and for a word
    /// a code block reads, the block's line that held it. At most [`SAID_CAP`] bytes: a longer
    /// one is cut around its match, which it keeps whole, with `…` at each cut end.
    pub said: String,
    /// The bytes of `said` that matched: the phrase, the head, `to do`, a count beside none
    /// with the word that places it, or a code block's whole line.
    pub matched: Range<usize>,
}

/// The most bytes a [`Found::said`] holds.
pub const SAID_CAP: usize = 160;

/// What a [`Found::said`] leads an unticked line with.
const TO_DO: &str = "to do";

impl Found {
    /// `word`, matched at `matched` in the sentence `held` of `line`'s words.
    fn of(
        line: &ReportLine,
        held: Range<usize>,
        word: &'static str,
        matched: Range<usize>,
    ) -> Self {
        let (said, matched) = line.said(held, matched);
        Self::cut(word, &said, matched)
    }

    /// An unticked line: `to do: <item>`.
    fn to_do(line: &ReportLine) -> Self {
        let (item, _) = line.said(0..line.words.len(), 0..0);
        let said = if item.is_empty() {
            TO_DO.to_string()
        } else {
            format!("{TO_DO}: {item}")
        };
        Self::cut(UNCHECKED, &said, 0..TO_DO.len())
    }

    /// A head over items: `<head>: <first item>`, the first item that is not a nil one.
    fn over(
        line: &ReportLine,
        held: Range<usize>,
        word: &'static str,
        head: Range<usize>,
        item: &ReportLine,
    ) -> Self {
        let (lead, matched) = line.said(held, head);
        let lead = lead.trim_end_matches(|c: char| c == ':' || c.is_whitespace());
        let matched = matched.start.min(lead.len())..matched.end.min(lead.len());
        let (item, _) = item.said(0..item.words.len(), 0..0);
        let said = if item.is_empty() {
            lead.to_string()
        } else {
            format!("{lead}: {item}")
        };
        Self::cut(word, &said, matched)
    }

    fn cut(word: &'static str, said: &str, matched: Range<usize>) -> Self {
        let (said, matched) = cut_around(said, matched, SAID_CAP);
        Self {
            word,
            said,
            matched,
        }
    }
}

impl ReportLine {
    /// The agent's own words for `words[held]`, with where `words[matched]` stands in them.
    fn said(&self, held: Range<usize>, matched: Range<usize>) -> (String, Range<usize>) {
        if self.block {
            // A block is quoted by its lines: a match by the line that held it, which is the
            // match, and an item by the block's first line.
            let line = if matched.is_empty() {
                0
            } else {
                self.from.get(matched.start).copied().unwrap_or(0)
            };
            let line = self.shown.get(line..).unwrap_or("");
            let line = line.split('\n').next().unwrap_or("");
            let (said, _) = plain(line, 0..line.len(), 0..0, None);
            let at = if matched.is_empty() {
                0..0
            } else {
                0..said.len()
            };
            return (said, at);
        }
        let (said, at) = plain(
            &self.shown,
            self.shown_at(held.clone()),
            self.shown_match(matched),
            Some(&self.from[..]),
        );
        if said.is_empty() {
            // Nothing the agent wrote stands for it (a lone tick): the words read do, but for a
            // code span's placeholder, which the agent never wrote.
            let words: Vec<&str> = self.words[held]
                .split_whitespace()
                .filter(|w| *w != CODE)
                .collect();
            return (words.join(" "), 0..0);
        }
        (said, at)
    }
}

/// `shown[range]` as said: markdown's emphasis and code ticks dropped and its spaces closed up,
/// with where `matched`, bytes of `shown`, stands in it. Given `from` (for each byte the reader
/// read, the byte of `shown` it came from), the underscores and box marks outside code that
/// the reader dropped go too.
fn plain(
    shown: &str,
    range: Range<usize>,
    matched: Range<usize>,
    from: Option<&[usize]>,
) -> (String, Range<usize>) {
    let dropped = |at: usize, len: usize| {
        from.is_some_and(|from| {
            let first = from.partition_point(|&f| f < at);
            from.get(first).is_none_or(|&f| f >= at + len)
        })
    };
    let mut code = shown[..range.start].matches('`').count() % 2 == 1;
    let mut out = String::with_capacity(range.len());
    let (mut start, mut end) = (None, None);
    let mut space = false;
    let mut past = range.start;
    for (at, c) in shown[range.clone()].char_indices() {
        let at = range.start + at;
        if at < past {
            continue;
        }
        if c == '`' {
            code = !code;
            continue;
        }
        if c == '*' && !code {
            continue;
        }
        if !code && from.is_some() {
            let mark = if c == '_' {
                Some(1)
            } else if c == '[' || !c.is_ascii() {
                UNCHECKED_BOXES
                    .iter()
                    .chain(CHECKED_BOXES)
                    .filter(|b| shown[at..].starts_with(**b))
                    .map(|b| b.len())
                    .max()
            } else {
                None
            };
            if let Some(len) = mark.filter(|&len| dropped(at, len)) {
                past = at + len;
                continue;
            }
        }
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if std::mem::take(&mut space) {
            out.push(' ');
        }
        if matched.contains(&at) {
            start.get_or_insert(out.len());
            end = Some(out.len() + c.len_utf8());
        }
        out.push(c);
    }
    let at = start.zip(end).map_or(0..0, |(s, e)| s..e);
    (out, at)
}

/// `said` cut to at most `cap` bytes around `matched`, which it keeps whole, at a word's edge
/// where one stands outside the match, with `…` at each cut end. A match longer than the room
/// keeps its start and as many of its whole words as fit.
pub fn cut_around(said: &str, matched: Range<usize>, cap: usize) -> (String, Range<usize>) {
    const CUT: &str = "…";
    let matched = matched.start.min(said.len())..matched.end.min(said.len());
    if said.len() <= cap {
        return (said.to_string(), matched);
    }
    let room = cap.saturating_sub(CUT.len());
    let (from, to) = if matched.end <= room {
        (0, room)
    } else if said.len() - matched.start <= room {
        (said.len() - room, said.len())
    } else {
        let room = cap.saturating_sub(2 * CUT.len());
        let spare = room.saturating_sub(matched.len());
        let from = matched.start - (spare / 2).min(matched.start);
        (from, (from + room).min(said.len()))
    };
    let mut from = from;
    while !said.is_char_boundary(from) {
        from += 1;
    }
    let mut to = to;
    while !said.is_char_boundary(to) {
        to -= 1;
    }
    if from > 0
        && let Some(space) = said[from..matched.start.max(from)].find(' ')
    {
        from += space + 1;
    }
    // The cut's end goes back to a word's edge: past a match that fits, and inside one that
    // does not, which then keeps its start and whole words of it.
    let (floor, inside) = if matched.end <= to {
        (matched.end, false)
    } else {
        (matched.start.max(from), true)
    };
    if to < said.len()
        && let Some(space) = said
            .get(floor..to)
            .and_then(|s| s.rfind(' '))
            .filter(|&s| !inside || s > 0)
    {
        to = floor + space;
    }
    let mut out = String::with_capacity(cap);
    if from > 0 {
        out.push_str(CUT);
    }
    let shift = out.len();
    out.push_str(&said[from..to]);
    if to < said.len() {
        out.push_str(CUT);
    }
    let start = (matched.start.max(from) - from + shift).min(out.len());
    let end = (matched.end.min(to).max(from) - from + shift).clamp(start, out.len());
    (out, start..end)
}

/// What a report's own words leave open, each with the sentence it stood in, in the order found.
fn open_work_found(tail: &str, land: Option<&[LandFamily]>, defense: bool) -> Vec<Found> {
    let lines = report_lines(tail, land.is_some());
    let said_main = land.is_some()
        && (!defense
            || lines
                .iter()
                .any(|l| main_named(&l.words) || main_claimed(&l.words)));
    let mut found: Vec<Found> = Vec::new();
    let mut i = 0;
    while let Some(line) = lines.get(i) {
        if line.unchecked {
            found.push(Found::to_do(line));
        }
        let words = line.words.as_str();
        let starts = sentence_starts(words);
        let end = |k: usize| starts.get(k + 1).copied().unwrap_or(words.len());
        let mut next = i + 1;
        for k in 0..starts.len() {
            let held = starts[k]..end(k);
            let sentence = &words[held.clone()];
            let head = open_head(sentence)
                .or_else(|| land.and_then(|_| head_in(sentence, LAND_HEADS)))
                .or_else(|| {
                    (k == 0 && line.heading)
                        .then(|| heading_head(sentence))
                        .flatten()
                });
            let Some((head, rest)) = head else {
                let (line_on, len) = (&words[starts[k]..], end(k) - starts[k]);
                let hit = match land {
                    None => open_status(line_on, len, &STATUS_PHRASES),
                    Some(families) => {
                        land_status(line_on, len, &words[..starts[k]], families, said_main)
                    }
                };
                if let Some(hit) = hit {
                    let at = starts[k] + hit.at..starts[k] + hit.at + hit.len;
                    found.push(Found::of(line, held, hit.word, at));
                }
                continue;
            };
            let head_at = starts[k]..starts[k] + head.len();
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
                let mut items: Vec<&ReportLine> = Vec::new();
                while let Some(under) = lines.get(j) {
                    if under.block {
                        // Land's reading: a code block right under a bare
                        // head is its item (`Still to do:` over a command).
                        if items.is_empty() {
                            items.push(under);
                        }
                        break;
                    }
                    if under.separated
                        || open_head(&under.words).is_some()
                        || !(under.item || items.is_empty())
                    {
                        break;
                    }
                    items.push(under);
                    j += 1;
                }
                next = j;
                if !cleared && let Some(first) = items.iter().find(|item| !nil(&item.words)) {
                    found.push(Found::over(line, held, head, head_at, first));
                }
                if let Some(families) = land {
                    found.extend(items.iter().filter_map(|item| {
                        let hit = aside_status(&item.words, families, said_main)?;
                        Some(Found::of(item, 0..item.words.len(), hit.word, hit.range()))
                    }));
                }
                break;
            }
            if !cleared && !nil(inline) {
                found.push(Found::of(line, held.clone(), head, head_at));
            }
            if let Some(families) = land
                && let Some(hit) = aside_status(inline, families, said_main)
            {
                let base = starts[k] + offset_in(sentence, inline);
                let at = base + hit.at..base + hit.at + hit.len;
                found.push(Found::of(line, held, hit.word, at));
            }
        }
        i = next;
    }
    found
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
    /// The line as the agent wrote it, past its marks (the quote, the bullet, the box, the
    /// heading's hashes): the text `words` was read from. A code block's is its first line and
    /// then each line it read a word in, a `\n` before each.
    shown: String,
    /// For each byte of `words`, the byte of `shown` it was read from; never decreasing.
    from: Vec<usize>,
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
        // Each step keeps, beside its text, the byte of `t` each of its bytes came from.
        let mut flat = String::with_capacity(t.len());
        let mut flat_from: Vec<usize> = Vec::with_capacity(t.len());
        let mut code = false;
        let mut span = String::new();
        let mut span_at = 0;
        for (i, c) in t.char_indices() {
            match c {
                '`' => {
                    if code && land {
                        flat.push(' ');
                        flat_from.resize(flat.len(), span_at);
                        let named = t[i + 1..]
                            .split_whitespace()
                            .next()
                            .map(bare)
                            .is_some_and(|w| SPAN_NOUNS.contains(&w.to_lowercase().as_str()));
                        let words = code_words(&span);
                        if named && SPAN_STATUS.iter().any(|(_, w)| *w == words) {
                            flat.push_str(CODE);
                            flat_from.resize(flat.len(), span_at);
                        } else {
                            flat.push_str(&words);
                            flat_from.extend(span_words_from(&span, span_at, &words));
                        }
                        span.clear();
                    }
                    if !code {
                        span_at = i;
                    }
                    code = !code;
                    flat.push(' ');
                }
                _ if code => span.push(c),
                '*' => {}
                '’' => flat.push('\''),
                c => flat.extend(c.to_lowercase()),
            }
            flat_from.resize(flat.len(), i);
        }
        if land {
            let mut padded = format!(" {flat} ");
            let mut padded_from = Vec::with_capacity(padded.len());
            padded_from.push(0);
            padded_from.append(&mut flat_from);
            padded_from.push(t.len());
            for b in UNCHECKED_BOXES.iter().chain(CHECKED_BOXES) {
                let mark = format!(" {}", b.to_lowercase());
                let mut from = 0;
                while let Some(found) = padded[from..].find(&mark).map(|i| from + i) {
                    let past = found + mark.len();
                    if padded[past..].starts_with([' ', '.', ',', ';', ':', ')', '!']) {
                        unchecked |= UNCHECKED_BOXES.contains(b);
                        padded.replace_range(found..past, " ");
                        padded_from.drain(found + 1..past);
                        from = found;
                    } else {
                        from = past;
                    }
                }
            }
            (flat, flat_from) = replaced(&padded, &padded_from, "#[ignore]", " check ignored ");
        }
        let (flat, flat_from) = without_underscore_emphasis(&flat, &flat_from);
        let mut words = String::with_capacity(flat.len());
        let mut from: Vec<usize> = Vec::with_capacity(flat.len());
        let mut after_not = false;
        let mut gap = 0;
        for (at, token) in tokens_at(&flat) {
            if after_not && matches!(token, "yet" | "been") {
                continue;
            }
            if !words.is_empty() {
                words.push(' ');
                from.push(gap);
            }
            let source = &flat_from[at..at + token.len()];
            let spelled = token.split_once("n't").filter(|(verb, rest)| {
                CONTRACTED.contains(verb) && !rest.contains(char::is_alphanumeric)
            });
            if let Some((verb, rest)) = spelled {
                words.push_str(verb);
                from.extend_from_slice(&source[..verb.len()]);
                words.push_str(" not");
                from.resize(words.len(), source[verb.len()]);
                words.push_str(rest);
                from.extend_from_slice(&source[verb.len() + "n't".len()..]);
                after_not = rest.is_empty();
            } else if let Some((n, m)) = land.then(|| ratio(token)).flatten() {
                words.push_str(&format!("{n} of {m}"));
                from.resize(words.len(), source[0]);
                let digits = token.trim_end_matches(|c: char| !c.is_ascii_digit()).len();
                words.push_str(&token[digits..]);
                from.extend_from_slice(&source[digits..]);
                after_not = false;
            } else {
                words.push_str(token);
                from.extend_from_slice(source);
                after_not = token == "not";
            }
            gap = flat_from.get(at + token.len()).copied().unwrap_or(t.len());
        }
        let mut shown = t.to_string();
        if from.len() != words.len() {
            // A byte the steps above lost would misplace every quote after it. The tests read
            // every corpus row with this held; a build quotes such a line in the words it read.
            if cfg!(test) {
                panic!("{t:?} reads {words:?} from {from:?}");
            }
            shown.clone_from(&words);
            from = (0..words.len()).collect();
        }
        Self {
            separated: false,
            block: false,
            unchecked,
            item,
            heading,
            capital,
            words,
            shown,
            from,
        }
    }

    /// The bytes of `shown` that `words[range]` was read from.
    fn shown_at(&self, range: Range<usize>) -> Range<usize> {
        let at = |b: usize| {
            let mut at = self
                .from
                .get(b)
                .copied()
                .unwrap_or(self.shown.len())
                .min(self.shown.len());
            while !self.shown.is_char_boundary(at) {
                at -= 1;
            }
            at
        };
        let start = at(range.start);
        start..at(range.end).max(start)
    }

    /// The bytes of `shown` that the match `words[range]` was read from: from its first byte's
    /// source to one character past its last byte's, so a mark the reader dropped after it
    /// (`_still open_`) stays out. Where its last word stands for a token of the agent's that
    /// reads otherwise (`3/5` read as `3 of 5`, `n't` as `not`, a code span as its status),
    /// the match runs to that token's end.
    fn shown_match(&self, range: Range<usize>) -> Range<usize> {
        let at = self.shown_at(range.clone());
        let last = self
            .words
            .get(range.clone())
            .and_then(|m| m.char_indices().next_back());
        let Some((last, read)) = last.map(|(b, c)| (range.start + b, c)) else {
            return at;
        };
        let source = self.shown_at(last..last).start;
        let Some(said) = self.shown[source..].chars().next() else {
            return at;
        };
        let rewritten = !(said.to_lowercase().any(|c| c == read) || (said == '’' && read == '\''))
            || (last > range.start && self.from.get(last - 1) == self.from.get(last));
        let end = if !rewritten {
            source + said.len_utf8()
        } else if said == '`' {
            // A code span read as a word it does not hold: the whole span.
            self.shown[source + 1..]
                .find('`')
                .map_or(self.shown.len(), |close| source + close + 2)
        } else {
            let token = &self.shown[source..];
            let token = &token[..token.find(char::is_whitespace).unwrap_or(token.len())];
            source
                + token
                    .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '_', '*'])
                    .len()
        };
        at.start..end.max(at.start)
    }

    /// Joins `line`, which carries this one on, to this one.
    fn join(&mut self, line: Self) {
        self.unchecked |= line.unchecked;
        self.words.push(' ');
        self.words.push_str(&line.words);
        self.from.push(self.shown.len());
        self.shown.push(' ');
        let base = self.shown.len();
        self.shown.push_str(&line.shown);
        self.from.extend(line.from.iter().map(|f| f + base));
    }
}

/// Where each byte of the words a code span read ([`code_words`]) stands in its line: a word
/// the span holds where it holds it, and any other (`codespan`, `skipped`) at the opening `tick`.
fn span_words_from(span: &str, tick: usize, words: &str) -> Vec<usize> {
    let lower = span.to_lowercase();
    let mut from = Vec::with_capacity(words.len());
    let (mut past, mut last) = (0, tick);
    for (at, token) in tokens_at(words) {
        from.resize(at, last);
        let held = (lower.len() == span.len())
            .then(|| lower[past..].find(token))
            .flatten();
        if let Some(p) = held {
            let start = tick + 1 + past + p;
            from.extend(start..start + token.len());
            past += p + token.len();
            last = start + token.len();
        } else {
            from.resize(at + token.len(), last);
        }
    }
    from.resize(words.len(), last);
    from
}

/// `text`'s whitespace-separated tokens, as `split_whitespace` gives them, each with its byte.
fn tokens_at(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut past = 0;
    std::iter::from_fn(move || {
        let rest = &text[past..];
        let start = past + (rest.len() - rest.trim_start().len());
        if start >= text.len() {
            return None;
        }
        let len = text[start..]
            .find(char::is_whitespace)
            .unwrap_or(text.len() - start);
        past = start + len;
        Some((start, &text[start..past]))
    })
}

/// `str::replace` that keeps, for each byte, where it came from.
fn replaced(text: &str, from: &[usize], pattern: &str, with: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut out_from = Vec::with_capacity(from.len());
    let mut kept = 0;
    for (at, _) in text.match_indices(pattern) {
        out.push_str(&text[kept..at]);
        out_from.extend_from_slice(&from[kept..at]);
        out.push_str(with);
        out_from.resize(out.len(), from[at]);
        kept = at + pattern.len();
    }
    out.push_str(&text[kept..]);
    out_from.extend_from_slice(&from[kept..]);
    (out, out_from)
}

fn without_underscore_emphasis(text: &str, from: &[usize]) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut out_from = Vec::with_capacity(from.len());
    let mut keep = |range: Range<usize>| {
        out.push_str(&text[range.clone()]);
        out_from.extend_from_slice(&from[range]);
    };
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
            keep(kept..start);
            keep(start + marks..at);
            kept = cursor;
            open = None;
        } else if open.is_none()
            && before.is_none_or(|c| !word_char(c))
            && after.is_some_and(|c| !c.is_whitespace())
        {
            open = Some((at, width));
        }
    }
    keep(kept..text.len());
    (out, out_from)
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
    raw.trim_start().starts_with('|') && next.is_some_and(table_rule)
}

/// A table's rule row (`|---|:--|`).
fn table_rule(raw: &str) -> bool {
    let t = raw.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
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
                // A block shows its first line, and then, a line apiece, each line it reads a
                // word in: the word maps to its line, which is what a find of it quotes.
                let text = raw.trim();
                let first = block.shown.is_empty();
                if first {
                    text.clone_into(&mut block.shown);
                }
                let mut held = first.then_some(0);
                for word in code_work(raw).into_iter().chain(output_status(raw)) {
                    if !block.words.split(' ').any(|w| w == word) {
                        let at = *held.get_or_insert_with(|| {
                            block.shown.push('\n');
                            let at = block.shown.len();
                            block.shown.push_str(text);
                            at
                        });
                        block.words.push(' ');
                        block.words.push_str(word);
                        block.from.resize(block.words.len(), at);
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
                block.shown.clear();
                block.from.fill(0);
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
        if land && table_rule(raw) {
            // A table's rule reads as words but shows none, so a quote across it keeps to the
            // rows.
            line.shown.clear();
            line.from.fill(0);
        }
        line.separated = std::mem::take(&mut separated);
        match out.last_mut() {
            Some(above) if !line.separated && !line.item && !line.capital => above.join(line),
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

/// Where a reader found open work in a sentence: its word, and the bytes that matched.
#[derive(Clone, Copy, Debug)]
struct Hit {
    word: &'static str,
    at: usize,
    len: usize,
}

impl Hit {
    const fn range(self) -> Range<usize> {
        self.at..self.at + self.len
    }
}

fn open_status(line: &str, len: usize, phrases: &Phrases) -> Option<Hit> {
    let sentence = &line[..len];
    let stands: Vec<(usize, &'static str)> = phrases_in(sentence, phrases).collect();
    phrases.table.iter().copied().find_map(|word| {
        stands
            .iter()
            .filter(|&&(_, w)| w == word)
            .find(|&&(at, _)| {
                let clause = sentence[..at]
                    .rsplit([',', ';', ':', '(', ')', '—', '–'])
                    .next()
                    .unwrap_or("");
                let before: Vec<&str> = clause.split_whitespace().collect();
                let prev = before.last().copied();
                !before.iter().any(|w| OPEN_NEGATIONS.contains(w))
                    && !(word.starts_with("not ") && prev.is_some_and(|p| OPEN_MODALS.contains(&p)))
                    && !(word == "skipped"
                        && prev.is_some_and(|p| ["is", "are", "be"].contains(&p)))
                    && !quoted(sentence, at)
                    && !nothing_to_do(&line[at + word.len()..])
            })
            .map(|&(at, _)| Hit {
                word,
                at,
                len: word.len(),
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
) -> Option<Hit> {
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
        .map(|st| Hit {
            word: st.family.and_then(|f| f.says).unwrap_or(st.word),
            at: st.at,
            len: st.len,
        });
    if !asks(s) {
        return open;
    }
    let leftover = match stated(s) {
        Some(t) if open.is_some() && reply(&line[len..]) == Some(Answer::Yes) => {
            land_status(&t, t.len(), prior, families, said_main).is_some()
        }
        _ => open.is_some(),
    };
    let mark = s.trim_end().len() - 1;
    question_open(s, &line[len..], leftover).then(|| {
        open.unwrap_or(Hit {
            word: QUESTION,
            at: mark,
            len: 1,
        })
    })
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
    if quoted(s, at) || in_path(s, at, end) || nil_item(s, at, end) || needs::token_you(s, at) {
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
    let failed_proof = matches!(st.word, "fail" | "fails" | "failed" | "failing")
        && next == Some("to")
        && !after
            .get(1)
            .is_some_and(|w| matches!(*w, "pass" | "succeed"))
        && (before
            .windows(2)
            .any(|w| matches!(w, ["negative", "control" | "controls"]))
            || before
                .iter()
                .any(|w| matches!(*w, "mutation" | "mutations" | "mutant" | "mutants")));
    if proof_row && !failed_proof && proves_by_failing(s, (from, to), (at, end)) {
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
    if failed_proof {
        return true;
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
    if nil_aside(item).is_some_and(|(_, inner)| placed_count(inner).is_some()) {
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

/// Where a `none (…)` aside opens on a count, the bytes of `inner` that hold it and the word
/// that places it (`3 open`).
fn placed_count(inner: &str) -> Option<Range<usize>> {
    let words: Vec<(usize, &str)> = tokens_at(inner)
        .map(|(at, raw)| {
            let word = bare(raw);
            (at + offset_in(raw, word), word)
        })
        .filter(|(_, w)| !w.is_empty())
        .collect();
    match words.as_slice() {
        [(at, n), rest @ ..]
            if (count(n).is_some_and(|c| c > 0) || matches!(*n, "some" | "few"))
                && rest
                    .first()
                    .is_none_or(|(_, w)| ELSEWHERE.contains(w) || CHANGE_NOUNS.contains(w)) =>
        {
            let end = rest
                .first()
                .map_or(at + n.len(), |(next, w)| next + w.len());
            Some(*at..end)
        }
        _ => None,
    }
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

/// An item's `none (…)` aside that leaves work open; the hit stands in `item`.
fn aside_status(item: &str, families: &[LandFamily], said_main: bool) -> Option<Hit> {
    let (nil, inner) = nil_aside(item)?;
    if !nil_list(nil) {
        return None;
    }
    let base = offset_in(item, inner);
    if let Some(count) = placed_count(inner) {
        return Some(Hit {
            word: COUNT_BESIDE_NONE,
            at: base + count.start,
            len: count.len(),
        });
    }
    land_status(inner, inner.len(), "", families, said_main).map(|hit| Hit {
        at: base + hit.at,
        ..hit
    })
}

/// Where `inner`, a slice of `outer`, starts in it.
fn offset_in(outer: &str, inner: &str) -> usize {
    let at = inner.as_ptr().addr().wrapping_sub(outer.as_ptr().addr());
    debug_assert!(
        at + inner.len() <= outer.len(),
        "{inner:?} is not in {outer:?}"
    );
    at.min(outer.len())
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
    crate::text::parse_uint(w).or_else(|| {
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
        // example-org/harness main`).
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

/// WHETHER THE REPLY `tail` IS THE TAB'S LOOP'S TO READ, not a relaunch's to carry on from: a clean
/// defense ([`clean_defense`]) or a `Could not finish:` ([`needs::could_not_finish`]).
#[must_use]
pub fn reply_for_the_loop(tail: Option<&str>) -> bool {
    clean_defense(tail) || tail.and_then(needs::could_not_finish).is_some()
}

/// WHETHER THE REPLY `tail` LANDS THE WORK, read at a land act's yield (aterm's `TurnEndState::observe`).
#[must_use]
pub fn clean_defense(tail: Option<&str>) -> bool {
    tail.is_some_and(|t| {
        needs::defense_line(t).is_some()
            && needs::needs_you(t).is_empty()
            && open_work_strict(Some(&needs::defense_body(t))).is_empty()
    })
}

/// The words Land's reading of the reply `tail` finds open at a land act's yield
/// ([`land_open_found`]), each once, in the order found.
#[must_use]
pub fn land_open_words(tail: Option<&str>) -> Vec<&'static str> {
    once(land_open_found(tail))
}

/// What Land's reading of the reply `tail` finds open at a land act's yield, each with the
/// sentence it stood in ([`Found`]), in the order found: the words a hold names.
#[must_use]
pub fn land_open_found(tail: Option<&str>) -> Vec<Found> {
    match tail {
        Some(t) if needs::defense_line(t).is_some() => {
            open_work_found(&needs::defense_body(t), Some(&LAND_FAMILIES), true)
        }
        Some(t) if says_done(Some(t)) => {
            open_work_found(&needs::without_needs(t), Some(&LAND_FAMILIES), false)
        }
        _ => Vec::new(),
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

#[cfg(test)]
#[path = "open_work_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "open_work_found_tests.rs"]
mod found_tests;

#[cfg(test)]
#[path = "open_work_measure_tests.rs"]
mod measure_tests;
