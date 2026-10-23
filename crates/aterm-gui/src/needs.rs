// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure session need classifier: maps [`NeedFacts`] into a [`Verdict`] ([`Need`]/[`NeedWord`],
//! [`Waiting`]/[`WaitWord`], and [`HoldWord`]) published at
//! [`crate::session_timeline::SessionTimeline::need`].

use aterm_messages::presence::WallAnswer;
use aterm_types::needs::{AttentionKind, HoldWord, NeedWord, WaitWord};

/// The published input stall, in the server attention's own words (`input_stall::menu_row`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StallFact {
    /// The stall's words, its remedy at their end.
    pub(crate) text: String,
    /// The episode's identity.
    pub(crate) key: u64,
    /// The job is stopped with input queued, not frozen.
    pub(crate) stopped: bool,
    /// The explicit restart is offered now.
    pub(crate) act: bool,
    /// The restart would be offered but for a hold that Hand Back lifts.
    pub(crate) hand_back_frees: bool,
}

/// The task record's part ([`crate::task_record::TaskRecord`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskFact {
    /// The record is `stalled`.
    pub(crate) stalled: bool,
    /// Its `why=`.
    pub(crate) why: Option<&'static str>,
    /// The rev it entered its state.
    pub(crate) entered: u64,
}

/// One keyed attention entry (`meta set attention owner=<k> [kind=]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttentionFact {
    /// Its owner (`-` the bare one).
    pub(crate) owner: String,
    /// Its text.
    pub(crate) text: String,
    /// The kind its writer named.
    pub(crate) kind: Option<AttentionKind>,
}

/// The published agent verdict ([`crate::session_timeline::AgentPublication`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentFact {
    /// `agent=`: `busy|prompt|question|wall:<kind>|idle|survey|unknown`.
    pub(crate) word: &'static str,
    /// `agent_detail=` (raw): a prompt's `kind[:verdict]`, a limit's reset.
    pub(crate) detail: Option<String>,
    /// `agent_rev=`: one transition's identity.
    pub(crate) rev: u64,
    /// The approval box's command or path, or an API error's cause in words.
    pub(crate) subject: Option<String>,
}

/// Everything [`of`] reads about one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NeedFacts {
    /// The published input stall.
    pub(crate) stall: Option<StallFact>,
    /// The task record.
    pub(crate) task: Option<TaskFact>,
    /// The attention entries, most recent first.
    pub(crate) attention: Vec<AttentionFact>,
    /// The agent verdict, `None` for no identified agent.
    pub(crate) agent: Option<AgentFact>,
    /// A harness presses this session's boxes.
    pub(crate) supervised: bool,
    /// Another holder's `meta set supervisor` claim is live.
    pub(crate) other_supervisor: bool,
    /// A supervisor's claim is live on the session.
    pub(crate) claim_live: bool,
    /// Who answers the agent's wall ([`WallAnswer`]).
    pub(crate) wall_answer: WallAnswer,
    /// The host restarts this agent itself at its next idle point.
    pub(crate) restarts: bool,
    /// The line that resumes this tab's own conversation.
    pub(crate) resume: Option<String>,
    /// Whose hold stands.
    pub(crate) hold: Option<HoldWord>,
    /// The agent's wall's reset placed on this machine's clock (unix seconds).
    pub(crate) reset_unix: Option<i64>,
    /// Now, unix seconds.
    pub(crate) now_unix: i64,
    /// The worker's needs you sets as its loop told the window's host, whole.
    pub(crate) told: ToldNeeds,
}

impl NeedFacts {
    /// The told needs, while their entry stands.
    #[must_use]
    pub(crate) fn told_needs(&self) -> Option<&[String]> {
        let entry = self
            .attention
            .iter()
            .find(|entry| entry.owner == crate::harness_host::NEEDS_ATTENTION_OWNER)
            .map(|entry| entry.text.as_str());
        self.told.words_for(entry)
    }
}

/// The worker's needs you as the window holds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToldNeeds {
    /// The set told last.
    current: Option<std::sync::Arc<HarnessNeeds>>,
    /// The set that described the needs entry standing when [`Self::current`] was told.
    outgoing: Option<std::sync::Arc<HarnessNeeds>>,
}

impl ToldNeeds {
    /// The tell ([`aterm_agent::supervise::IdleHost::needs_standing`]).
    pub(crate) fn tell(
        &mut self,
        told: Option<std::sync::Arc<HarnessNeeds>>,
        standing: Option<&str>,
    ) -> bool {
        if self.current == told {
            return false;
        }
        let before = self.reading(standing).cloned();
        let describes = |held: &std::sync::Arc<HarnessNeeds>| held.words_for(standing).is_some();
        let outgoing = if told.as_ref().is_some_and(describes) {
            None
        } else {
            [self.current.take(), self.outgoing.take()]
                .into_iter()
                .flatten()
                .find(describes)
        };
        self.current = told;
        self.outgoing = outgoing;
        before.as_deref().map(HarnessNeeds::needs)
            != self.reading(standing).map(|held| held.needs())
    }

    /// The held set a reader reads beside `standing`: the one told last
    fn reading(&self, standing: Option<&str>) -> Option<&std::sync::Arc<HarnessNeeds>> {
        [&self.current, &self.outgoing]
            .into_iter()
            .flatten()
            .find(|held| held.words_for(standing).is_some())
    }

    /// The needs a reader reads beside `standing`, the needs owner's entry as stored.
    #[must_use]
    pub(crate) fn words_for(&self, standing: Option<&str>) -> Option<&[String]> {
        self.reading(standing).map(|held| held.needs())
    }

    /// The set told last, whatever entry stands (`None`: none told, or the take-down).
    #[must_use]
    #[cfg_attr(
        not(any(test, debug_assertions)),
        expect(dead_code, reason = "read by the debug `status needs_told=` and tests")
    )]
    pub(crate) fn current(&self) -> Option<&HarnessNeeds> {
        self.current.as_deref()
    }
}

/// The worker's `NEEDS YOU` set, whole, as its loop told them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessNeeds {
    /// Every need standing, in the worker's order.
    needs: Vec<String>,
    /// The entry's text they were told with, through the server's presentation sanitizer.
    entry: String,
}

impl HarnessNeeds {
    /// What a tell leaves standing.
    #[must_use]
    pub(crate) fn told(needs: &[String], entry: &str) -> Option<Self> {
        let entry = crate::session_timeline::sanitize_presentation_line(entry, usize::MAX);
        (!needs.is_empty() && !entry.is_empty()).then(|| Self {
            needs: needs.to_vec(),
            entry,
        })
    }

    /// Every need told, whole, in the worker's order.
    #[must_use]
    pub(crate) fn needs(&self) -> &[String] {
        &self.needs
    }

    /// The entry's text the set was told with, sanitized.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "read by tests; slice 2d compares through `words_for`"
        )
    )]
    pub(crate) fn entry(&self) -> &str {
        &self.entry
    }

    /// The needs, where `standing`.
    #[must_use]
    pub(crate) fn words_for(&self, standing: Option<&str>) -> Option<&[String]> {
        let standing = standing?.trim();
        let same = standing == self.entry
            || (crate::session_timeline::metadata_has_forbidden_formatting(standing)
                && crate::session_timeline::sanitize_presentation_line(standing, usize::MAX)
                    == self.entry);
        same.then_some(self.needs.as_slice())
    }
}

/// What a session needs of its owner now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Need {
    /// The word.
    pub(crate) word: NeedWord,
    /// The transition's identity within the word.
    pub(crate) key: u64,
    /// The row's words, one clipped line, without the tab's label.
    pub(crate) what: String,
    /// The harness's own needs you entry, read without aterm's frame.
    pub(crate) harness: Option<HarnessAsk>,
}

/// A harness's needs you, in the worker's own words ([`Need::harness`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessAsk {
    /// The first need without the frame, folded to one line.
    pub(crate) first: String,
    /// How many further needs stand (the told set's others, else the entry's ` (+N more)`).
    pub(crate) more: u16,
    /// The further needs, whole, in the worker's order.
    pub(crate) others: Vec<String>,
    /// The program whose entry it is.
    pub(crate) program: aterm_phase::Program,
}

impl HarnessAsk {
    /// The entry's words held.
    pub(crate) fn of(words: NeedWords<'_>) -> Self {
        Self {
            first: fold_clip(words.first, NEED_WHAT_MAX),
            more: words.more,
            others: Vec::new(),
            program: words.program,
        }
    }

    /// The whole set, as the loop told it.
    #[must_use]
    pub(crate) fn told(needs: &[String], program: aterm_phase::Program) -> Option<Self> {
        let mut held = needs
            .iter()
            .map(|need| fold_clip(need, NEED_TOLD_MAX))
            .filter(|need| !need.is_empty());
        let first = held.next()?;
        let others: Vec<String> = held.collect();
        Some(Self {
            first,
            more: u16::try_from(others.len()).unwrap_or(u16::MAX),
            others,
            program,
        })
    }

    /// ` (+N more)`, empty where no further need stands.
    #[must_use]
    pub(crate) fn more_words(&self) -> String {
        self.more_past(0)
    }

    /// ` (+N more)` for the further needs a surface does not say on a line of its own.
    #[must_use]
    pub(crate) fn more_past(&self, shown: usize) -> String {
        let left = usize::from(self.more).saturating_sub(shown);
        if left == 0 {
            String::new()
        } else {
            format!(" (+{left} more)")
        }
    }

    /// The need on one line within `max` bytes.
    #[must_use]
    pub(crate) fn line(&self, max: usize) -> String {
        let tail = self.more_words();
        format!(
            "{}{tail}",
            fold_clip(&self.first, max.saturating_sub(tail.len()))
        )
    }

    /// How many needs stand: the first and the further ones (`status needs_n=`).
    #[must_use]
    pub(crate) fn count(&self) -> u32 {
        1 + u32::from(self.more)
    }

    /// The need where nothing around it says it is one.
    #[must_use]
    pub(crate) fn cued_line(&self, max: usize) -> String {
        format!(
            "{NEEDS_YOU_LEAD}{}",
            self.line(max.saturating_sub(NEEDS_YOU_LEAD.len()))
        )
    }
}

/// The words a harness's need leads with where no section header says it is a need.
pub(crate) const NEEDS_YOU_LEAD: &str = "needs you: ";

/// A harness's entry read without aterm's frame ([`need_words`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NeedWords<'a> {
    /// The first need, as the entry carries it.
    pub(crate) first: &'a str,
    /// The further needs the entry counts (` (+N more)`), 0 for none.
    pub(crate) more: u16,
    /// The program the entry names.
    pub(crate) program: aterm_phase::Program,
}

/// Between the program's word and the need in a harness's entry.
const NEEDS_YOU: &str = " needs you: ";
/// The tail a harness's entry ends with
const CARRYING_ON: &str = " \u{2014} carrying on";

/// The need without aterm's frame.
#[must_use]
pub(crate) fn need_words(text: &str) -> Option<NeedWords<'_>> {
    let text = text.trim();
    let (head, rest) = text.split_once(NEEDS_YOU)?;
    let program = aterm_phase::Program::ALL
        .into_iter()
        .find(|program| program.name().eq_ignore_ascii_case(head))?;
    let (rest, tail_seen) = without_carrying_on(rest);
    let rest = rest.trim_end();
    let (first, more) = match more_count(rest) {
        Some(read) => read,
        None if tail_seen => (rest, 0),
        None => return None,
    };
    let first = first.trim();
    (!first.is_empty()).then_some(NeedWords {
        first,
        more,
        program,
    })
}

/// `rest` without its ` — carrying on`, whole or cut from its dash on, and
fn without_carrying_on(rest: &str) -> (&str, bool) {
    let dash = " \u{2014}".len();
    let cuts = CARRYING_ON
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(CARRYING_ON.len()))
        .filter(|&at| at >= dash);
    let mut best = None;
    for at in cuts {
        if rest.ends_with(&CARRYING_ON[..at]) {
            best = Some(at);
        }
    }
    match best {
        Some(at) => (&rest[..rest.len() - at], true),
        None => (rest, false),
    }
}

/// `<first> (+N more)` read into the first need and `N`; `None` where the
fn more_count(rest: &str) -> Option<(&str, u16)> {
    let body = rest.strip_suffix(" more)")?;
    let at = body.rfind(" (+")?;
    let digits = &body[at + " (+".len()..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let more = digits
        .parse::<u64>()
        .map_or(u16::MAX, |n| u16::try_from(n).unwrap_or(u16::MAX));
    Some((&body[..at], more))
}

/// What aterm is waiting out for a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    /// The cause.
    pub(crate) word: WaitWord,
    /// The wall's kind (`usage-session`), where a wall is the cause.
    pub(crate) wall: Option<&'static str>,
    /// The reset the wall names, as it names it.
    pub(crate) reset: Option<String>,
    /// The `kind=wait` entry that is the cause ([`WaitWord::Entry`]).
    pub(crate) entry: Option<WaitEntry>,
}

/// A writer's `kind=wait` attention entry, as a Waiting row shows it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WaitEntry {
    /// Its owner (`-` the bare one).
    pub(crate) owner: String,
    /// Its words, one clipped line.
    pub(crate) text: String,
}

impl WaitEntry {
    /// `waiting for CI on PR 412 (ci)`.
    #[must_use]
    pub(crate) fn words(&self) -> String {
        if self.owner == crate::session_timeline::BARE_ATTENTION_OWNER {
            self.text.clone()
        } else {
            format!("{} ({})", self.text, self.owner)
        }
    }
}

/// What aterm waits out for one tab, in a person's words, never the wire word.
#[must_use]
pub(crate) fn wait_words(wait: &Waiting, unsupervised: bool) -> String {
    match (wait.word, wait.wall) {
        (WaitWord::Api, _) => "can't reach the API".to_string(),
        (WaitWord::Overloaded, _) => "retrying an overloaded service".to_string(),
        (WaitWord::MemoryRestart, _) => "memory restart queued for the next idle point".to_string(),
        (WaitWord::Login, _) => "signing Claude back in".to_string(),
        (WaitWord::Limit, Some(wall)) if unsupervised => {
            format!("at the {}, unsupervised", wall_words(wall))
        }
        (WaitWord::Limit, Some(wall)) => format!("waiting out the {}", wall_words(wall)),
        (WaitWord::Limit, None) => "waiting out a limit".to_string(),
        (WaitWord::Entry, _) => wait
            .entry
            .as_ref()
            .map_or_else(|| "waiting".to_string(), WaitEntry::words),
    }
}

/// [`of`]'s answer for one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Verdict {
    /// What it needs of the owner now.
    pub(crate) need: Option<Need>,
    /// What aterm is waiting out for it.
    pub(crate) waiting: Option<Waiting>,
    /// Whose hold stands.
    pub(crate) held: Option<HoldWord>,
    /// When the verdict changes with no other fact moving (unix seconds).
    pub(crate) recheck_unix: Option<i64>,
}

impl Verdict {
    /// `status needs=` / `survey needs=`.
    #[must_use]
    pub(crate) fn needs_word(&self) -> &'static str {
        self.need.as_ref().map_or("-", |need| need.word.word())
    }

    /// `hold_by=`: the hold's word, `-` for none.
    #[must_use]
    pub(crate) fn hold_word(&self) -> &'static str {
        self.held.map_or("-", HoldWord::word)
    }

    /// The need in the worker's words where the published need is a harness's own ask.
    #[must_use]
    pub(crate) fn harness_ask(&self) -> Option<&HarnessAsk> {
        self.need.as_ref().and_then(|need| need.harness.as_ref())
    }
}

/// Byte budget for a row's words.
pub(crate) const ROW_WHAT_MAX: usize = 72;
/// Byte budget for a harness's need in its own words ([`HarnessAsk::first`]).
pub(crate) const NEED_WHAT_MAX: usize = 240;
/// Byte budget for each need a harness's loop told the window whole ([`HarnessAsk::told`]).
pub(crate) const NEED_TOLD_MAX: usize = aterm_agent::supervise::policy::needs::NEED_CHARS * 4;
/// Byte budget for the words that carry a remedy.
pub(crate) const REMEDY_TEXT_MAX: usize = crate::session_timeline::META_ATTENTION_KEYED_MAX;

/// The attention owners [`of`] reads nothing from: the server's own stall
const NOT_ASKS: [&str; 2] = [
    crate::input_stall::SERVER_ATTENTION_OWNER,
    aterm_agent::harness::upgrade_drive::ATTENTION_OWNER,
];

/// The wall kinds a usage limit reads as (`aterm_phase::WallKind::reads_limited`
const LIMIT_WALLS: [&str; 4] = ["usage-session", "usage-weekly", "model-bucket", "spend"];

/// The classifier: `facts` → [`Verdict`] (the module doc's rule).
#[must_use]
pub(crate) fn of(facts: &NeedFacts) -> Verdict {
    let wall = facts.agent.as_ref().and_then(|agent| {
        let kind = agent
            .word
            .strip_prefix("wall:")
            .filter(|kind| !kind.is_empty())?;
        Some((agent, kind))
    });
    let (ask, attention_wait, supervisor_waits) = attention_outcome(facts);
    let (wall_need, wall_wait, recheck_unix) = wall.map_or((None, None, None), |(agent, kind)| {
        wall_outcome(facts, agent, kind, supervisor_waits)
    });
    let candidates = [
        stall_need(facts),
        task_need(facts),
        wall_need.clone(),
        ask,
        box_need(facts),
    ];
    let waiting = if wall_need.is_some() {
        None
    } else {
        wall_wait.or(attention_wait)
    };
    let need = candidates
        .into_iter()
        .flatten()
        .min_by_key(|need| need.word);
    Verdict {
        need,
        waiting,
        held: facts.hold,
        recheck_unix,
    }
}

/// The published stall: frozen, or stopped with input queued. Its words are
fn stall_need(facts: &NeedFacts) -> Option<Need> {
    let stall = facts.stall.as_ref()?;
    let words = stall_words(&stall.text);
    let what = if stall.stopped || stall.act {
        fold_clip(words, REMEDY_TEXT_MAX)
    } else {
        stall_remedy(words, stall.hand_back_frees, facts.resume.as_deref())
    };
    Some(Need {
        word: if stall.stopped {
            NeedWord::Stopped
        } else {
            NeedWord::Frozen
        },
        key: stall.key,
        what,
        harness: None,
    })
}

/// A frozen program the pill offers no restart for keeps a person's remedy.
pub(crate) fn stall_remedy(words: &str, hand_back: bool, resume: Option<&str>) -> String {
    let words = fold_clip(words, REMEDY_TEXT_MAX);
    if hand_back {
        return fold_clip(
            &format!("{words} \u{2014} Hand Back to let aterm restart it"),
            REMEDY_TEXT_MAX,
        );
    }
    let head = format!("{words} \u{2014} \u{2303}C or close the tab");
    resume
        .into_iter()
        .flat_map(|line| {
            std::iter::once(line.to_string()).chain(aterm_agent::harness::resume::shorter(line))
        })
        .map(|cmd| format!("{head}, then {cmd}"))
        .find(|what| what.len() <= REMEDY_TEXT_MAX)
        .unwrap_or_else(|| fold_clip(&head, REMEDY_TEXT_MAX))
}

/// A stall's text without its remedy: everything before its first ` — `
fn stall_words(text: &str) -> &str {
    text.split_once(" \u{2014} ")
        .map_or(text, |(words, _)| words)
}

/// A stalled task: the window host's deadline's words where its attention
fn task_need(facts: &NeedFacts) -> Option<Need> {
    let task = facts.task.as_ref().filter(|task| task.stalled)?;
    let deadline = facts
        .attention
        .iter()
        .find(|entry| aterm_agent::supervise::deadline_attention(&entry.text));
    let what = match (deadline, task.why) {
        (Some(entry), _) => entry.text.clone(),
        (None, Some("held")) => "held at a screen aterm does not know".to_string(),
        (None, Some("startup" | "shell")) => "never started".to_string(),
        (None, Some(why)) => format!("stalled ({why})"),
        (None, None) => "stalled".to_string(),
    };
    Some(Need {
        word: NeedWord::Stalled,
        key: task.entered,
        what: fold_clip(&what, REMEDY_TEXT_MAX),
        harness: None,
    })
}

/// What `agent`'s wall of `kind` makes of the session: a need, a wait, and
fn wall_outcome(
    facts: &NeedFacts,
    agent: &AgentFact,
    kind: &'static str,
    supervisor_waits: bool,
) -> (Option<Need>, Option<Waiting>, Option<i64>) {
    let reset = agent.detail.clone().filter(|d| !d.is_empty());
    let need = |word: NeedWord, what: String| {
        Some(Need {
            word,
            key: agent.rev,
            what,
            harness: None,
        })
    };
    let wait = |word: WaitWord| {
        Some(Waiting {
            word,
            wall: Some(kind),
            reset: reset.clone(),
            entry: None,
        })
    };
    let unheld = facts.hold.is_none();
    let answered = facts.wall_answer != WallAnswer::Nobody;
    let answered_now = answered && unheld;
    let retried_elsewhere = facts.other_supervisor && unheld;
    let waited = supervisor_waits && facts.claim_live && facts.supervised;
    match kind {
        "unresponsive" => (
            stall_need(facts).or_else(|| {
                if agent.detail.as_deref() == Some("stopped") {
                    need(NeedWord::Stopped, "stopped with input queued".to_string())
                } else {
                    need(NeedWord::Frozen, "frozen: not reading input".to_string())
                }
            }),
            None,
            None,
        ),
        "auth" if answered_now => (None, wait(WaitWord::Login), None),
        "auth" => (
            need(NeedWord::Login, "logged out: log in there".to_string()),
            None,
            None,
        ),
        "memory" if facts.restarts => (None, wait(WaitWord::MemoryRestart), None),
        "memory" => (
            need(NeedWord::Memory, memory_remedy(facts.resume.as_deref())),
            None,
            None,
        ),
        "api-error" if agent.subject.as_deref() == Some(crate::presence::API_UNREACHABLE) => {
            (None, wait(WaitWord::Api), None)
        }
        "overloaded" if answered_now || retried_elsewhere => {
            (None, wait(WaitWord::Overloaded), None)
        }
        kind if LIMIT_WALLS.contains(&kind) => {
            // Answered by this host's harness, or waited out by a live
            if answered || waited {
                return (None, wait(WaitWord::Limit), None);
            }
            match facts.reset_unix {
                Some(at) if at > facts.now_unix => (None, wait(WaitWord::Limit), Some(at)),
                Some(_) => (
                    need(NeedWord::Wall, wall_reset_what(kind, reset.as_deref())),
                    None,
                    None,
                ),
                None => (
                    need(NeedWord::Wall, wall_what(kind, reset.as_deref())),
                    None,
                    None,
                ),
            }
        }
        _ if answered_now => (None, None, None),
        "api-error" | "context" if retried_elsewhere => (None, None, None),
        kind => (
            need(NeedWord::Wall, wall_what_of(agent, kind, reset.as_deref())),
            None,
            None,
        ),
    }
}

/// The wall's words: its kind in a person's words, its reset riding it.
pub(crate) fn wall_what(kind: &str, reset: Option<&str>) -> String {
    let words = wall_words(kind);
    fold_clip(
        &match reset {
            Some(reset) => format!("{words} until {reset}"),
            None => words.to_string(),
        },
        ROW_WHAT_MAX,
    )
}

/// An unsupervised limit past its reset.
pub(crate) fn wall_reset_what(kind: &str, reset: Option<&str>) -> String {
    let words = wall_words(kind);
    fold_clip(
        &match reset {
            Some(reset) => format!("{words} reset at {reset} \u{2014} continue it there"),
            None => format!("{words} \u{2014} continue it there"),
        },
        ROW_WHAT_MAX,
    )
}

/// [`wall_what`], where an API error caused by the network says its cause
fn wall_what_of(agent: &AgentFact, kind: &str, reset: Option<&str>) -> String {
    match agent.subject.as_deref().filter(|s| !s.is_empty()) {
        Some(cause) if kind == "api-error" => fold_clip(cause, ROW_WHAT_MAX),
        _ => wall_what(kind, reset),
    }
}

/// What the attention entries make of the session: the most recent ask,
fn attention_outcome(facts: &NeedFacts) -> (Option<Need>, Option<Waiting>, bool) {
    let mut ask = None;
    let mut wait: Option<WaitEntry> = None;
    let mut supervisor_waits = false;
    for entry in facts
        .attention
        .iter()
        .filter(|entry| !NOT_ASKS.contains(&entry.owner.as_str()))
        .filter(|entry| !entry.text.trim().is_empty())
    {
        match resolved_kind(entry, facts.wall_answer) {
            AttentionKind::Wait => {
                if wait.is_none() {
                    wait = Some(WaitEntry {
                        owner: entry.owner.clone(),
                        text: fold_clip(entry_words(&entry.text), ROW_WHAT_MAX),
                    });
                }
                supervisor_waits |= entry.owner == aterm_agent::supervise::ATTENTION_OWNER;
            }
            AttentionKind::Ask if ask.is_none() => {
                let text = entry.text.trim();
                let harness = (entry.owner == crate::harness_host::NEEDS_ATTENTION_OWNER)
                    .then(|| harness_ask(text, facts.told_needs()))
                    .flatten();
                let what = harness.as_ref().map_or_else(
                    || fold_clip(text.trim_start_matches('⚠').trim_start(), ROW_WHAT_MAX),
                    |ask| ask.line(ROW_WHAT_MAX),
                );
                ask = Some(Need {
                    word: NeedWord::Ask,
                    key: ask_key(text),
                    what,
                    harness,
                });
            }
            AttentionKind::Ask => {}
        }
    }
    let wait = wait.map(|entry| Waiting {
        word: WaitWord::Entry,
        wall: None,
        reset: None,
        entry: Some(entry),
    });
    (ask, wait, supervisor_waits)
}

/// THE NEEDS OWNER'S ASK IN THE WORKER'S WORDS: the set `told` beside this
fn harness_ask(text: &str, told: Option<&[String]>) -> Option<HarnessAsk> {
    let parsed = need_words(text);
    let program = parsed.map(|words| words.program).or_else(|| {
        let (head, _) = text.split_once(NEEDS_YOU)?;
        aterm_phase::Program::ALL
            .into_iter()
            .find(|program| program.name().eq_ignore_ascii_case(head.trim()))
    });
    told.zip(program)
        .and_then(|(needs, program)| HarnessAsk::told(needs, program))
        .or_else(|| parsed.map(HarnessAsk::of))
}

/// An entry's words without a leading `kind=` an older server stored as
fn entry_words(text: &str) -> &str {
    let text = text.trim();
    match text.split_once(' ') {
        Some((head, rest)) if head.starts_with("kind=") => rest.trim_start(),
        _ => text,
    }
}

/// An ask's [`Need::key`].
pub(crate) fn ask_key(text: &str) -> u64 {
    aterm_hash::fnv1a_64(text.trim().as_bytes())
}

/// An entry's kind: the one its writer named, else (§2.4's legacy rule) a
fn resolved_kind(entry: &AttentionFact, wall_answer: WallAnswer) -> AttentionKind {
    if let Some(kind) = entry.kind {
        return kind;
    }
    let text = entry.text.trim_start();
    let text = match text.split_once(' ') {
        Some((head, rest)) if head.starts_with("kind=") => rest.trim_start(),
        _ => text,
    };
    if text.starts_with(aterm_agent::supervise::ATTENTION_PREFIX)
        && wall_answer == WallAnswer::Harness
    {
        AttentionKind::Wait
    } else {
        AttentionKind::Ask
    }
}

/// A box or a question no harness answers: no supervisor, or a hold that
fn box_need(facts: &NeedFacts) -> Option<Need> {
    let agent = facts.agent.as_ref()?;
    if facts.supervised && facts.hold.is_none() {
        return None;
    }
    match agent.word {
        "prompt" => {
            let kind = crate::presence::prompt_kind_words(agent.detail.as_deref());
            let what = match agent.subject.as_deref().filter(|s| !s.is_empty()) {
                Some(subject) => format!("{} {subject}", kind.unwrap_or("approval")),
                None => crate::presence::prompt_band_word(agent.detail.as_deref()),
            };
            Some(Need {
                word: if kind == Some("question") {
                    NeedWord::Question
                } else {
                    NeedWord::Approval
                },
                key: agent.rev,
                what: fold_clip(&what, ROW_WHAT_MAX),
                harness: None,
            })
        }
        "question" => Some(Need {
            word: NeedWord::Question,
            key: agent.rev,
            what: "question".to_string(),
            harness: None,
        }),
        _ => None,
    }
}

/// The memory wall's remedy (`wall:memory`, the host not restarting it).
pub(crate) fn memory_remedy(resume: Option<&str>) -> String {
    const HEAD: &str = "memory critical \u{2014} restart it";
    resume
        .into_iter()
        .flat_map(|line| {
            std::iter::once(line.to_string()).chain(aterm_agent::harness::resume::shorter(line))
        })
        .map(|cmd| format!("{HEAD}, then {cmd}"))
        .find(|what| what.len() <= REMEDY_TEXT_MAX)
        .unwrap_or_else(|| HEAD.to_string())
}

/// A wall kind (`aterm_phase::WallKind::name`) in a person's words.
pub(crate) fn wall_words(kind: &str) -> &str {
    match kind {
        "usage-session" => "session usage limit",
        "usage-weekly" => "weekly usage limit",
        "model-bucket" => "model usage limit",
        "spend" => "spend limit",
        "context" => "context full",
        "auth" => "logged out",
        "api-error" => "API error",
        "overloaded" => "service overloaded",
        "memory" => "memory critical",
        other => other,
    }
}

/// `s` folded to one line (control characters and runs of whitespace become one space) and clipped.
pub(crate) fn fold_clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_control() || c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if crate::session_timeline::is_forbidden_metadata_char(c) {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    if out.len() <= max {
        return out;
    }
    let mut cut = max.saturating_sub('…'.len_utf8());
    while cut > 0 && !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('…');
    out
}

#[cfg(test)]
#[path = "needs_tests.rs"]
mod tests;
