// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TURN END'S WORDS: the texts a harness types at a turn end (the continuation's rule, the
//! done check and its angles, Land's instruction, nudge and answer, and the words each retired
//! one used), the marks it keeps of what a turn said, and the readers of a reply's words that
//! name no program: a done claim ([`says_done`]), what a reply asks of a person
//! ([`classify_said`]), and the go-ahead a turn parks on ([`turn_go_ahead`]).

use std::borrow::Cow;
use std::time::Duration;

use crate::land_check::RULE_LAND_CHECK;
use crate::{go_ahead, land_check, needs};

/// A continuation typed at an ordinary turn end (aterm's `decide_turn_end`).
pub const RULE_CONTINUE: &str = "continue@v1";

/// Recovery from a repeated report or a refused routine decision.
pub const FORWARD_RECOVERY: &str = concat!(
    mark!(),
    " Continue autonomously. This approach is not making progress. \
    Decide on a different safe approach and record the decision for another AI to revisit. \
    If this work cannot proceed, defer it with the reason and advance another useful task. \
    Do not ask the person to unblock routine work. Keep moving."
);

/// Why Cruise waits before [`FORWARD_RECOVERY`] at a repeated report: the words ending its
/// `WAITING` line. The wait is the short back-off ladder's step for the streak its point closed
/// (aterm's `TurnEndState::short_wait`), which these words do not name, so a reader of the ladder counts
/// it as the step after the one before.
pub const REPEAT_RECOVERY_WAIT: &str = "repeated report; changing approach on the next turn";

/// The [`DONE_CHECK`] typed under Cruise where the worker claims it is done.
pub const RULE_DONE_CHECK: &str = "continue-done-check@v1";

/// RETIRED: the open-items challenge older builds typed where a done report named open work (aterm's `OPEN_ITEMS_HEAD`, aterm's `OPEN_ITEMS_HEADS_BEFORE`).
pub const RULE_OPEN_ITEMS: &str = "continue-open-items@v1";

/// Retired rule tag for the legacy uncommitted-work continuation challenge.
pub const RULE_UNCOMMITTED: &str = "continue-uncommitted@v1";

/// Whether `rule` is one of the retired challenges ([`RULE_OPEN_ITEMS`], [`RULE_UNCOMMITTED`]).
#[must_use]
pub fn is_challenge_rule(rule: &str) -> bool {
    rule == RULE_OPEN_ITEMS || rule == RULE_UNCOMMITTED
}

/// The journal's name for a person's own exchange left to them (aterm's `TurnEndState::left_to_person`).
pub const RULE_LEFT: &str = "left-to-person@v1";

/// Why the loop keeps its hands off a session a person typed into within their grace: the reason
/// of the policy's hold for them, and the words that end the `WAITING` line the loop journals for
/// it.
pub const PERSON_TYPING: &str = "a person is typing";

/// CRUISE'S DONE CHECK, ITS SHARED BASE: every act typed under [`RULE_DONE_CHECK`] opens with
/// it, then one of [`DONE_ANGLES`] in turn and [`DONE_TAIL`] ([`done_check_words`]), so every
/// reader keyed on the check's text (aterm's `is_done_check`, the ledger's and the upgrade's) reads each
/// angle, and v1's whole text ([`DONE_CHECK_FIXED_BEFORE`]) too, by this prefix.
pub const DONE_CHECK: &str = concat!(mark!(), " Autopilot is Cruise: done is never the end.");

/// CRUISE'S SIX KINDS OF WORK, one asked at each done claim in turn (aterm's `TurnEndState::checks`):
/// the widest check, a hostile review, the deferred work, a measurement, the next issue, and the
/// session's own leftovers. Each names only the agent's own work: none says delete, remove, reset,
/// retire, drop or prune, and the sixth leaves every worktree, branch and stash the agent did not
/// make exactly as it is.
pub const DONE_ANGLES: [&str; NO_WORK_ACTS] = [
    "This time, run the widest check this repository has on the merged tree and fix what it finds.",
    "This time, review the diff you landed as a hostile reviewer would: edge cases, error paths, \
     missing tests. Fix what you find.",
    "This time, do what your reports called out of scope, later or a follow-up.",
    "This time, measure what you changed against the main it started from and fix any regression.",
    "This time, take the next open issue or TODO beside your work.",
    "This time, land every worktree, branch and stash you made in this session that no remote \
     main has. Leave every one you did not make exactly as it is.",
];

/// Where aterm's `TurnEndState::checks` counts round: a whole number of turns of [`DONE_ANGLES`] (42 of
/// them). The count after it is [`CHECKS_ROUND`], never 0, so the angles go on in order across it
/// and a count above 0 always names the angle typed last ([`next_checks`]).
pub const CHECKS_WRAP: u8 = 252;

const _: () = assert!((CHECKS_WRAP as usize).is_multiple_of(DONE_ANGLES.len()));

/// One whole turn of [`DONE_ANGLES`]: the count a wrap goes back to, and the first a seeded count
/// at or past [`CHECKS_WRAP`] (one no loop writes) is folded onto (aterm's `TurnEndState::seed_checks`).
pub const CHECKS_ROUND: u8 = 6;

const _: () = assert!(CHECKS_ROUND as usize == DONE_ANGLES.len());

/// The count once one more check is typed after `checks`: the next, or [`CHECKS_ROUND`] at the
/// wrap, so the angle typed last is still `count - 1`'s.
pub const fn next_checks(checks: u8) -> u8 {
    match checks.checked_add(1) {
        Some(n) if n < CHECKS_WRAP => n,
        _ => CHECKS_ROUND,
    }
}

/// What follows each of [`DONE_ANGLES`]: record decisions and continue; the agent's reversible work is
/// never one ([`go_ahead::NEVER_A_NEED`], owner 2026-10-29: "how dare you block on me for go
/// ahead!"), in the continuation's words, which every done check typed within the person's hour
/// leaves out as the continuation does ([`go_ahead::unless_held`]); and the defense the owner
/// asked for (2026-10-19) with what it ran, then the hunt goes on.
pub const DONE_TAIL: &str = concat!(
    "Decide, log why for later AI review, and keep moving; defer unavailable work. ",
    go_ahead::never_a_need!(),
    " If this turns up nothing, say what you ran and what it showed in a few lines starting \
     Nothing left:, then look for another kind of gap and close it."
);

/// RETIRED: v1's one fixed done check (until 5a, v6 §14.5), which every done claim got word for
/// word. It opens with [`DONE_CHECK`], so the base's own rule reads it with its suffixes; it is not
/// one of [`RETIRED_DONE_CHECKS`], whose stricter tail would drop them.
pub const DONE_CHECK_FIXED_BEFORE: &str = concat!(
    mark!(),
    " Autopilot is Cruise: done is never the end. Land every open item, then look harder: \
     review your own work adversarially, widen the checks, fix what they find, close every \
     gap. Put what only a person can give on a line starting NEEDS YOU: and carry on. If you \
     are certain nothing is left, defend it in a few lines starting Nothing left: and keep \
     looking."
);

/// The done check's words at its `n`th claim (`n` counts from 0; the seventh is the first again):
/// [`DONE_CHECK`], the angle, [`DONE_TAIL`], before any suffix or the standing rules. Whole, as
/// typed outside the person's hour: a caller that types them within it cuts
/// [`go_ahead::NEVER_A_NEED`] ([`go_ahead::unless_held`]).
#[must_use]
pub fn done_check_words(n: usize) -> String {
    format!(
        "{DONE_CHECK} {} {DONE_TAIL}",
        DONE_ANGLES[n % DONE_ANGLES.len()]
    )
}

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

/// The letters and digits of `tail` in order, every other character left out: what two texts are
/// compared by where their spacing, marks and wrapping may differ ([`without_act_rows`]).
#[must_use]
pub fn said_letters(tail: Option<&str>) -> String {
    let mut letters = String::new();
    letters.extend(
        tail.unwrap_or_default()
            .chars()
            .filter(|c| c.is_alphanumeric()),
    );
    letters
}

/// HOW A MANAGED WORKER'S WORK ENDED UNFINISHED while its agent stands (aterm's `TurnEndState::gave_up`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GaveUp {
    /// Its reply said `Could not finish: <why>` ([`needs::could_not_finish`]).
    CouldNotFinish,
    /// The same reply came back with no work and Land's stop for that took the point
    /// (aterm's `TurnEndState::conclude_repeated`).
    Repeated,
    /// A usage-limit failure recorded by an earlier harness. New walls wait for quota.
    UsageLimit,
}

impl GaveUp {
    /// Every ending, in the order [`Self::of_word`] and [`Self::of_journal_line`] try them: the
    /// `why=` words a task record's `failed` mark must take ([`Self::word`]).
    pub const ALL: [Self; 3] = [Self::CouldNotFinish, Self::Repeated, Self::UsageLimit];

    /// The task record's `why=` for it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::CouldNotFinish => "could-not-finish",
            Self::Repeated => "repeated",
            Self::UsageLimit => "usage-limit",
        }
    }

    /// The ending a record's `why=` names; `None` for any other word.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|ending| ending.word() == word)
    }

    /// What the loop's journal says of it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::CouldNotFinish => "the worker said it could not finish",
            Self::Repeated => "the worker repeated the same reply with no work",
            Self::UsageLimit => {
                "the worker's account is at a usage limit that resets too far off to wait for, \
                 and no move off it is made"
            }
        }
    }

    /// The ending a journal's `FAILED` line names, by its [`Self::words`].
    #[must_use]
    pub fn of_journal_line(line: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|ending| line.contains(ending.words()))
    }
}

/// The decider's check that holds an act at the same reply (aterm's `TurnEndState::repeats`).
pub const SAME_REPLY: &str = "same-reply";

/// The check that changes approach when the standing go-ahead's routine answer budget is spent.
pub const GO_AHEAD_REFUSED: &str = "go-ahead-refused";

/// THE PERSON'S HOUR HOLDS A PARK (aterm's `TurnEndState::go_ahead_hour_ends`): the check that waits
/// for the hour's end where the decision would type nothing, or back off past it
/// (aterm's `continue_policy`), so the point is decided again there.
pub const GO_AHEAD_HOUR: &str = "go-ahead-hour";

/// Why a point waits for the end of the person's hour ([`GO_AHEAD_HOUR`]).
pub const GO_AHEAD_HOUR_WHY: &str =
    "a person's key holds the go-ahead for the hour after it; the park is decided again at its end";

/// The books' row of a person's hour that runs from the end of the turn their key came in
/// (`from=<unix ms>`, aterm's `TurnEndState::take_go_ahead_hour`): the window's clock keeps the key
/// alone, so a loop after a restart reads the hour's start from it
/// (aterm's `TurnEndState::seed_go_ahead_hour`).
pub const RULE_GO_AHEAD_HOUR: &str = "go-ahead-hour@v1";

/// Why a person's hour is booked ([`RULE_GO_AHEAD_HOUR`]).
pub const GO_AHEAD_HOUR_BOOKED: &str =
    "a person's key came in a turn that ended later; their hour runs from that end";

/// Why a park on a person's go-ahead escalates where answers are off (aterm's `continue_policy`).
pub const GO_AHEAD_WHY: &str = "the worker parked its own reversible work on a person's go-ahead";

/// Routine go-ahead attempts before switching to forward recovery (aterm's `TurnEndState::go_ahead_spent`).
pub const GO_AHEAD_ACTS: u32 = 3;

/// How long after its first go-ahead act a park may stand before aterm changes approach: the
/// first since the agent last moved past its park, by real work or a turn a wall cut
/// (aterm's `TurnEndState::go_ahead_worked`, 2026-10-30 review 9).
pub const GO_AHEAD_SPAN: Duration = Duration::from_secs(60 * 60);

/// THE LONGEST BACK-OFF BEFORE A GO-AHEAD, measured from the last act typed: an agent that wakes
/// itself sooner than the short-turn ladder's wait (aterm's `TurnEndState::short_wait`) restarts no
/// wait the answer owes (the incident's first twenty hours, 2026-10-27). At a break of the
/// agent's own background work the go-ahead rides the carry's ladder, its wait capped the same
/// from the last carry ([`super::cruise_carry::BackgroundCarries::due_within`]).
pub const GO_AHEAD_WAIT: Duration = Duration::from_secs(15 * 60);

/// How long the person's last key holds the go-ahead (aterm's `TurnEndState::go_ahead_held`), from the
/// key or the end of the turn it came in: the replies after it may restate a hold the person
/// just gave. No word of theirs is read for a hold; one longer than this is the agent's to keep,
/// and the go-ahead says a hold a person gave still stands.
pub const GO_AHEAD_HELD: Duration = Duration::from_secs(60 * 60);

/// HOW MANY REPLIES THE SHORT STREAK KEEPS (aterm's `Replies`).
pub const REPLIES_KEPT: usize = 3;

/// HOW MANY NO-WORK ACTS IN A ROW HOLD WHATEVER THEIR WORDING (aterm's `Replies::run`): two through each reply the streak keeps, so the same-reply hold always comes first.
pub const NO_WORK_ACTS: usize = 2 * REPLIES_KEPT;

/// THE GO-AHEAD A TURN PARKS ON ([`go_ahead::parked`]): `turn_said` (its earlier messages) and
/// `tail` (its last), past the rows of `act`, the act the turn opens with where it does. A turn
/// end (aterm's `TurnEndReading::go_ahead_past`) and a break of the agent's background work
/// (aterm's `TurnEndState::go_ahead_at_break`) read it alike. A need of the earlier messages that the
/// turn's last word retracted parks nothing ([`needs::retracted_cut`]): the agent did that work,
/// or no longer needs it, so a go-ahead for it would be a prompt it cannot act on.
pub fn turn_go_ahead(
    turn_said: Option<needs::Before<'_>>,
    tail: &str,
    act: Option<&str>,
) -> Option<go_ahead::Park> {
    match turn_said {
        Some(before) => go_ahead::parked(
            &needs::retracted_cut(before.past(without_act_rows(before.text(), act)), tail),
            tail,
        ),
        None => go_ahead::parked("", without_act_rows(tail, act)),
    }
}

/// `text` past the rows that only echo `act`, the line typed last: the longest run of whole lines
/// at its head whose letters ([`said_letters`]) end `act`'s, cut with the line breaks after it;
/// `text` whole where none does or `act` holds no letter.
#[must_use]
pub fn without_act_rows<'a>(text: &'a str, act: Option<&str>) -> &'a str {
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

/// `lines` (aterm's `TurnEndReading::said_lines`, aterm's `TurnEndReading::turn_lines`) where they hold the
/// same words as `rows`, its text a line a row; else `rows`.
fn lines_of<'a>(rows: Option<&'a str>, lines: Option<&'a str>) -> Option<&'a str> {
    lines
        .filter(|l| said_letters(Some(l)) == said_letters(rows))
        .or(rows)
}

/// THE WORDS OF A TURN THE NEEDS FOLD AND THE GO-AHEAD'S PARK BOTH READ, `(before, tail)`: its
/// earlier messages and its last as drawn (`said`, `(tail, before)`), each read as its measured
/// lines (`lines`, the same pair with each line the program wrapped read as one) where they hold
/// the same words ([`lines_of`]), the earlier messages told apart by `breaks` in the text read. A
/// point reads its turn so (aterm's `TurnEndReading::needs_said`) and a break of the agent's background
/// work alike, so every line the go-ahead answers is a line the fold stands, never a row of one:
/// the shown set matches lines exactly (aterm's `Needs::less`), and a line the go-ahead answers never
/// goes up (DESIGN-harness-autopilot §15.6).
#[must_use]
pub fn needs_words<'a>(
    (tail, before): (Option<&'a str>, Option<&'a str>),
    (tail_lines, before_lines): (Option<&'a str>, Option<&'a str>),
    breaks: &'a needs::Breaks,
) -> (Option<needs::Before<'a>>, Option<&'a str>) {
    let before = match before_lines.filter(|l| said_letters(Some(l)) == said_letters(before)) {
        Some(lines) => Some(needs::Before::new(lines, &breaks.lines)),
        None => before.map(|t| needs::Before::new(t, &breaks.said)),
    };
    (before, lines_of(tail, tail_lines))
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

/// AUTOPILOT LAND's instruction, typed at the first point of a Land episode where the continuation or the done check would go.
pub const RULE_LAND: &str = "land@v1";

/// WHAT A NOTE RESTATING A LANDING AFTER THE TASK'S OWN WAKE SAYS (aterm's `TurnEndState::take_wake_restated`).
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
pub(crate) use no_repository;

/// FILES NO REPOSITORY HOLDS LAND WHERE THEY ARE: [`LAND`]'s and [`LAND_NUDGE`]'s sentence for them. It names the files, not the folder, so a worker whose tab is no repository still pushes the work it did in one; the probe still reads the session's repository at a defense (aterm's `repo_work_suffix`). It is short so the standing rules keep room on Land's first act with both its clauses ([`VERBATIM_UNITS`]).
pub const NO_REPOSITORY: &str = no_repository!();

/// LAND'S CLOSE, one source for every Land act that asks for the landing ([`LAND`],
/// [`LAND_NUDGE`], aterm's `land_contradicted`): its [`NO_REPOSITORY`] sentence, what a person can give
/// on a `NEEDS YOU:` line, and the defense owed once it is all on remote main.
macro_rules! land_close {
    () => {
        concat!(
            no_repository!(),
            " Record unavailable work and the decision to defer it, then land what can proceed. \
             Once it is all on remote main, defend that in a few lines starting Nothing left:"
        )
    };
}

/// [`land_close!`] as a constant.
pub const LAND_CLOSE: &str = land_close!();

/// What Land types at an episode's first point ([`RULE_LAND`]). Its [`NO_REPOSITORY`] sentence keeps a worker whose files no repository holds from asking a person for a remote to push to: on 2026-10-24 a real model's work in a plain folder answered the words without it with a NEEDS YOU for one in 3 runs of 3 (one a live `spawn` worker), and with it with a clean `Nothing left:` in 3 of 3; Land nudges with no bound, so each nudge would have asked again.
pub const LAND: &str = concat!(
    mark!(),
    " Autopilot is Land: land ALL of this work into the remote main branch, no exceptions. \
     Finish every open item, verify it, commit, merge and push it, and remove your worktrees \
     and merged branches. Leave nothing unlanded, parked on a branch or for later; chew \
     through every wall with safe, reversible fixes. Start no new work. ",
    land_close!()
);

/// What a Land nudge types ([`RULE_LAND_NUDGE`]) at every free point after the instruction until the work is landed, but where it contradicts a defense (aterm's `land_contradicted`).
pub const LAND_NUDGE: &str = concat!(
    mark!(),
    " Still landing: get everything onto the remote main branch, no exceptions, with nothing \
     unlanded, parked on a branch or for later. ",
    land_close!()
);

/// LAND'S CONTRADICTION'S FIXED HEAD (v6 §14.4's slice 4c): what Land's nudge opens with where the
/// reply it answers defends the work with `Nothing left:` while git still finds work no remote
/// main has, or while the agent's own list of what is left still stands (aterm's `land_contradicted`).
/// Every reader of a typed Land act knows it by these words (limit 4).
pub const LAND_CONTRADICTED_HEAD: &str = concat!(
    mark!(),
    " Still landing: your last reply says Nothing left, but"
);

/// The contradiction's measured fact, before git's finding in parentheses (aterm's `repo_finding`).
pub const CONTRADICTED_GIT: &str = "git shows work no remote main has";

/// What the contradiction asks of the work git found: to land it, and never to delete it. It
/// offers no other way out (no "ignore it", "move it out" or "say it is not open"), since each
/// would clear the probe without landing anything.
pub const CONTRADICTED_GIT_ASK: &str =
    "Commit, merge and push it to the remote main; never delete work.";

/// The contradiction's quote of the agent's own list (aterm's `TurnEndState::land_listed`), the words
/// Land's nudge quotes it with ([`land_check::LISTED_OPEN`]) after the head's `but`.
pub const CONTRADICTED_LISTED: &str = concat!("you ", land_check::said_left!());

/// What the contradiction asks of the agent's own list: to finish it and land it, and never to
/// delete work.
pub const CONTRADICTED_LISTED_ASK: &str =
    "Finish it and land it on the remote main; never delete work.";

/// The most UTF-16 units of the agent's list the contradiction quotes, its first item first.
pub const CONTRADICTED_QUOTE_UNITS: usize = 120;

/// What a question gets under Land ([`RULE_LAND_ANSWER`]).
pub const LAND_ANSWER: &str = "Nobody is here to choose: take the option that lands all of this \
                               work on the remote main soonest and start no new work. Pushing \
                               and merging are fine; never force-push or delete work.";

/// What opens Land's instruction where the person's switch over a held tab types it (v6 §14.17
/// item 3), before the held quote ([`switch_why`]).
pub const SWITCH_WHY_HEAD: &str = "You stopped at: \u{201c}";

/// What follows the held quote in the switch's why ([`switch_why`]).
pub const SWITCH_WHY_TAIL: &str = "\u{201d}. The person switched this tab to Land, so land what \
                                   you can, record unavailable work and the decision to defer it, \
                                   and keep moving.";

/// The most UTF-16 units the held quote takes of the switch's ask (aterm's `switch_act`), less where
/// the act's fixed words leave less of [`VERBATIM_UNITS`].
pub const SWITCH_QUOTE_UNITS: usize = 120;

/// THE SWITCH'S WHY, from one source (v6 §14.17 item 3): `You stopped at: “<quote>”. The person
/// switched this tab to Land, so land what you can and name in a NEEDS YOU: line only what a
/// person must give.`
#[must_use]
pub fn switch_why(quote: &str) -> String {
    format!("{SWITCH_WHY_HEAD}{quote}{SWITCH_WHY_TAIL}")
}

/// THE HELD QUOTE: what the agent stopped at, read from its held reply `tail` — the paragraph of
/// its `Nothing left:` defense where it gave one, else its last paragraph — on one line, its
/// curly quotes made straight so the why's own close its quote, no control character, and its
/// one closing full stop left to the why's (an ellipsis kept whole).
/// aterm's `switch_act` cuts it to its room.
#[must_use]
pub fn held_quote(tail: &str) -> String {
    let lines: Vec<&str> = tail.lines().collect();
    let blank = |l: &&str| l.trim().is_empty();
    let start = needs::defense_line(tail)
        .and_then(|d| lines.iter().position(|l| l.trim() == d))
        .or_else(|| {
            let end = lines.iter().rposition(|l| !blank(l))?;
            Some(lines[..end].iter().rposition(blank).map_or(0, |b| b + 1))
        })
        .unwrap_or(lines.len());
    let said: String = lines[start..]
        .iter()
        .take_while(|l| !blank(l))
        .flat_map(|l| l.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| match c {
            '\u{201c}' | '\u{201d}' => '"',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
    // The why closes its quote with its own full stop (`“…All done”. The person …`).
    match said.strip_suffix('.') {
        Some(cut) if !cut.ends_with('.') => cut.to_string(),
        _ => said,
    }
}

/// `text` WITHOUT THE SWITCH'S WHY where it opens with it (aterm's `switch_act`), the harness's mark
/// kept: Land's instruction as every reader keyed on its words reads it. Any other text is
/// itself.
#[must_use]
pub fn past_switch_why(text: &str) -> Cow<'_, str> {
    let mark = mark!();
    let Some(rest) = text
        .strip_prefix(mark)
        .and_then(|r| r.strip_prefix(' '))
        .and_then(|r| r.strip_prefix(SWITCH_WHY_HEAD))
    else {
        return Cow::Borrowed(text);
    };
    match rest.find(SWITCH_WHY_TAIL) {
        Some(at) => Cow::Owned(format!("{mark}{}", &rest[at + SWITCH_WHY_TAIL.len()..])),
        None => Cow::Borrowed(text),
    }
}

/// Whether `text` opens with the switch's why (aterm's `switch_act`): Land's one ask at the person's
/// switch over a held tab.
#[must_use]
pub fn is_switch_ask(text: &str) -> bool {
    matches!(past_switch_why(text), Cow::Owned(_))
}

/// RETIRED: [`LAND`]'s words of `4bff4848b4`, as [`DONE_CHECK_COMMITTED_BEFORE`].
pub const LAND_COMMITTED_BEFORE: &str = concat!(
    mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed and pushed \
     (merged to main where this repository lands work), with nothing uncommitted, untracked or \
     stashed. Write down what is left in the repository and commit that too, so your summary \
     names nothing left. Then write a short summary of whether the work is complete and what \
     is done."
);

/// RETIRED: [`LAND`]'s words from `69a8f68178` until `4bff4848b4`.
pub const LAND_ORDINARY_BEFORE: &str = concat!(
    mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of whether the work is \
     complete, what is done and what is left."
);

/// RETIRED: [`LAND`]'s words of `0b1abdabf3` until `69a8f68178`, as [`DONE_CHECK_WORDS_BEFORE`].
pub const LAND_WORDS_BEFORE: &str = concat!(
    mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of what is done and what \
     is left, and close by saying the work is complete."
);

/// RETIRED: [`LAND`]'s words while it asked for the [`FINISHED`] token.
pub const LAND_MARKER_BEFORE: &str = concat!(
    mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of what is done and what \
     is left, and reply with ATERM-TASK-DONE on a line by itself."
);

/// RETIRED: [`LAND`]'s words of `a355109905` (FOREVER LAND), before its [`NO_REPOSITORY`] sentence.
pub const LAND_REMOTE_MAIN_BEFORE: &str = concat!(
    mark!(),
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
    mark!(),
    " Still landing: finish only what leaves the work clean, write down what is left in the \
     repository and commit it, then summarize whether the work is complete and what is done."
);

/// RETIRED: [`LAND_NUDGE`]'s words from `69a8f68178` until `4bff4848b4`, as [`LAND_ORDINARY_BEFORE`].
pub const LAND_NUDGE_ORDINARY_BEFORE: &str = concat!(
    mark!(),
    " Still landing: finish only what leaves the work clean, then summarize whether the work \
     is complete, what is done and what is left."
);

/// RETIRED: [`LAND_NUDGE`]'s words of `0b1abdabf3` until `69a8f68178`, as [`LAND_WORDS_BEFORE`].
pub const LAND_NUDGE_WORDS_BEFORE: &str = concat!(
    mark!(),
    " Still landing: finish only what leaves the work clean, then close by saying the work is \
     complete."
);

/// RETIRED: [`LAND_NUDGE`]'s words while it asked for the [`FINISHED`] token.
pub const LAND_NUDGE_MARKER_BEFORE: &str = concat!(
    mark!(),
    " Still landing: finish only what leaves the work clean, then reply with ATERM-TASK-DONE on \
     a line by itself."
);

/// RETIRED: [`LAND_NUDGE`]'s words of `a355109905`, as [`LAND_REMOTE_MAIN_BEFORE`].
pub const LAND_NUDGE_REMOTE_MAIN_BEFORE: &str = concat!(
    mark!(),
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

/// Whether `rule` is one of Land's four acts (its instruction, nudge, answer and land check).
#[must_use]
pub fn is_land_rule(rule: &str) -> bool {
    matches!(
        rule,
        RULE_LAND | RULE_LAND_NUDGE | RULE_LAND_ANSWER | RULE_LAND_CHECK
    )
}

/// Whether the tail has a `Nothing left:` defense line ([`needs::defense_line`]) or a
/// legacy done token ([`says_done_outright`]), outside its `NEEDS YOU` lines.
/// Ordinary completion prose receives normal turn-end handling. A claim ends no task:
/// Cruise requests another kind of work; Land finishes only at its land check's `LANDED` answer.
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
            needs::defense_line(t).is_some() || says_done_outright(&needs::without_needs(t))
        })
    })
}

/// Whether `t` holds a legacy done line: [`FINISHED`] alone on a line ([`is_finished_line`]), or
/// `DONE` as the whole reply (the answer [`DONE_CHECK_BEFORE`] asked for).
#[must_use]
pub fn says_done_outright(t: &str) -> bool {
    t.lines().any(is_finished_line) || t.trim_matches(|c: char| !c.is_alphanumeric()) == "DONE"
}

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

/// What the worker's last words ask of a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// Nothing asked of a person.
    Plain,
    /// An offer to go on ([`OFFER_PHRASES`]) naming no destructive act: nothing asked.
    Offer,
    /// A question or a choice a person is asked to answer.
    Question,
    /// A stop phrase ([`STOP_PHRASES`]), with why.
    Stop(String),
    /// A stop, an offer or a question that names a destructive act ([`destructive_word`]), with
    /// why: its answer must consent to nothing irreversible.
    Irreversible(String),
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
    // EVERY STOP PHRASE STOPS WHEREVER IT IS SAID (a fence, a `without`, a negation and an echo
    // included), as before the go-ahead reader: that reader only picks the answer to a stop that
    // is not irreversible ([`TurnEndState::go_ahead_owed`]), and no text the harness types holds
    // a stop phrase.
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

/// Whether `text` names a force-push git's short forms spell: a push whose clause has a short
/// flag cluster with an `f` (`push -f`, `push -uf`, `push origin main -f`), or a refspec forced
/// with `+` (`push origin +main`).
pub fn force_push(text: &str) -> bool {
    text.split([';', '|', '&', '\n', '?', '!']).any(|clause| {
        let mut push = false;
        let flagged = clause
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
            });
        let mut push = false;
        let forced = clause.split_whitespace().any(|token| {
            let token = token.trim_matches(['`', '"', '\'', '(', ')', ',', '.', ':']);
            if token
                .split(|c: char| !(c.is_alphanumeric() || c == '-'))
                .any(|w| inflects(w, "push"))
            {
                push = true;
                return false;
            }
            push && token
                .strip_prefix('+')
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()))
        });
        flagged || forced
    })
}

/// The first of [`DESTRUCTIVE_WORDS`] `text` says, each of its words in any inflection (`deleted`,
/// `overwriting`, `rewrote`), its dashes read past; `push -f` only as [`force_push`] reads it.
#[must_use]
pub fn destructive_word(text: &str) -> Option<&'static str> {
    let short_force_push = force_push(text);
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

/// The UTF-16 code units of `s`: what [`VERBATIM_UNITS`] counts.
#[must_use]
pub fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The most UTF-16 code units a text this policy types may hold and still be drawn as typed.
pub const VERBATIM_UNITS: usize = 800;

/// `text` with the standing `rules` after it in [`RULES_OPEN`]`…)`, the rules cut at a word
/// ([`fit_units`]) so the whole stays within [`VERBATIM_UNITS`]; `text` alone where no rule is
/// set or none fits.
#[must_use]
pub fn with_rules(text: &str, rules: Option<&str>) -> String {
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

/// `s` cut to at most `room` UTF-16 code units at a word's edge, with `…`; `s` whole where it
/// fits, empty where no word does.
#[must_use]
pub fn fit_units(s: &str, room: usize) -> Cow<'_, str> {
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

/// What opens the standing rules after a text this policy types ([`with_rules`]).
pub const RULES_OPEN: &str = " (standing rules: ";

#[cfg(test)]
#[path = "words_tests.rs"]
mod tests;
