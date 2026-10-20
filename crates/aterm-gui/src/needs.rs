// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NEED CLASSIFIER (`docs/DESIGN-owner-surfaces-2026-10-16.md` §2): one
//! session's facts in ([`NeedFacts`]), its [`Verdict`] out — what it needs of
//! the owner now ([`Need`], a [`NeedWord`]), what aterm is waiting out for it
//! ([`Waiting`], a [`WaitWord`]) and whose hold stands ([`HoldWord`]). Pure:
//! no lock, no clock, no AppKit, so the table is tested off macOS.
//!
//! The verdict is computed once per change on the main thread
//! (`App::publish_need`, from `App::refresh_presence_session`) and published
//! on the session's timeline ([`crate::session_timeline::SessionTimeline::need`]).
//! Every reader reads that one value: `status needs=`, `survey needs=`, the
//! menu-bar status item's rows and its notifications. So the badge, the wire
//! and a fleet peer's survey cannot disagree, and no reader re-guesses
//! another's supervision.
//!
//! THE RULE (the AGENTS.md alert rule): a need is the owner needed now, for a
//! reason aterm cannot settle, where one act changes the outcome. What the
//! harness or the network settles is [`Waiting`]; a hold is the owner's own
//! act. Precedence is [`NeedWord`]'s order: the first that applies wins.

use aterm_messages::presence::WallAnswer;
use aterm_types::needs::{AttentionKind, HoldWord, NeedWord, WaitWord};

/// The published input stall, in the server attention's own words
/// (`input_stall::menu_row`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StallFact {
    /// The stall's words, its remedy at their end.
    pub(crate) text: String,
    /// The episode's identity: one notification per stall.
    pub(crate) key: u64,
    /// The job is stopped with input queued, not frozen.
    pub(crate) stopped: bool,
    /// The explicit restart is offered now (`restart_act::offered`, the one
    /// predicate the pill's Restart is offered by): the pill carries the act,
    /// so the words need not.
    pub(crate) act: bool,
    /// The restart would be offered but for a hold that Hand Back lifts (the
    /// person's, or an Owner client's with no other standing,
    /// `fabric::Holds::hand_back_origin`): the person's way out is Hand Back.
    pub(crate) hand_back_frees: bool,
}

/// The task record's part ([`crate::task_record::TaskRecord`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskFact {
    /// The record is `stalled`.
    pub(crate) stalled: bool,
    /// Its `why=`.
    pub(crate) why: Option<&'static str>,
    /// The rev it entered its state at: one stall, one key.
    pub(crate) entered: u64,
}

/// One keyed attention entry (`meta set attention owner=<k> [kind=]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttentionFact {
    /// Its owner (`-` the bare one).
    pub(crate) owner: String,
    /// Its text.
    pub(crate) text: String,
    /// The kind its writer named; `None` for none.
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
    /// The approval box's command or path, or an API error's cause in
    /// words: host-side only, never on the wire.
    pub(crate) subject: Option<String>,
}

/// Everything [`of`] reads about one session, copied under the leaf locks
/// the status glance already takes (`App::need_facts`).
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
    /// A harness presses this session's boxes: the window's host answers for
    /// it (its agent admitted, Autopilot not Off, past its claim grace) or
    /// another supervisor's claim is live. A hold voids it ([`Self::hold`]).
    pub(crate) supervised: bool,
    /// Another holder's `meta set supervisor` claim is live (a manager
    /// session, codex-alab's self-claim; never this host's own, which
    /// [`Self::wall_answer`] speaks for): that holder retries an overload
    /// and an API error and compacts a full context, and escalates through
    /// its own keyed attention what it cannot. Implies [`Self::supervised`] and [`Self::claim_live`].
    pub(crate) other_supervisor: bool,
    /// A supervisor's claim is live on the session (`meta supervisor`, this
    /// host's own or another holder's). The supervisor loop's `kind=wait`
    /// entry waits a wall out only under one: the entry is not tied to the
    /// claim, so a loop that died leaves it standing.
    pub(crate) claim_live: bool,
    /// Who answers the agent's wall ([`WallAnswer`]).
    pub(crate) wall_answer: WallAnswer,
    /// The host restarts this agent itself at its next idle point
    /// (`status_item`'s memory row's word).
    pub(crate) restarts: bool,
    /// The line that resumes this tab's own conversation.
    pub(crate) resume: Option<String>,
    /// Whose hold stands.
    pub(crate) hold: Option<HoldWord>,
    /// The agent's wall's reset placed on this machine's clock (unix
    /// seconds), where it names one this machine can place.
    pub(crate) reset_unix: Option<i64>,
    /// Now, unix seconds.
    pub(crate) now_unix: i64,
}

/// What a session needs of its owner now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Need {
    /// The word.
    pub(crate) word: NeedWord,
    /// The transition's identity within the word: the stall's episode, the
    /// task's entry rev, the agent's rev, the attention text's hash.
    pub(crate) key: u64,
    /// The row's words, one clipped line, without the tab's label.
    pub(crate) what: String,
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
    /// The `kind=wait` entry that is the cause ([`WaitWord::Entry`]): its
    /// writer and its words, which every Waiting row prints.
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
    /// `waiting for CI on PR 412 (ci)`: the words, then whose they are (the
    /// bare owner's alone).
    #[must_use]
    pub(crate) fn words(&self) -> String {
        if self.owner == crate::session_timeline::BARE_ATTENTION_OWNER {
            self.text.clone()
        } else {
            format!("{} ({})", self.text, self.owner)
        }
    }
}

/// WHAT ATERM WAITS OUT FOR ONE TAB, in a person's words, never the wire
/// word: the Overview's line, and the words ❯ ▸ Waiting counts its tabs
/// before (`status_item::wait_line`). `unsupervised`: a limit nobody but the
/// person continues, at its reset (the verdict's recheck).
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
    /// When the verdict changes with no other fact moving (unix seconds): an
    /// unsupervised limit's reset, past which it is a `wall`. The publisher
    /// arms a wake at it.
    pub(crate) recheck_unix: Option<i64>,
}

impl Verdict {
    /// `status needs=` / `survey needs=`: the need's word, `-` for none.
    #[must_use]
    pub(crate) fn needs_word(&self) -> &'static str {
        self.need.as_ref().map_or("-", |need| need.word.word())
    }

    /// `hold_by=`: the hold's word, `-` for none.
    #[must_use]
    pub(crate) fn hold_word(&self) -> &'static str {
        self.held.map_or("-", HoldWord::word)
    }
}

/// Byte budget for a row's words.
pub(crate) const ROW_WHAT_MAX: usize = 72;
/// Byte budget for the words that carry a remedy: the server's keyed
/// attention cap, so a row carries every word its entry does.
pub(crate) const REMEDY_TEXT_MAX: usize = crate::session_timeline::META_ATTENTION_KEYED_MAX;

/// The attention owners [`of`] reads nothing from: the server's own stall
/// entry ([`NeedWord::Frozen`] says it) and the live upgrade's, which the
/// message band tells.
const NOT_ASKS: [&str; 2] = [
    crate::input_stall::SERVER_ATTENTION_OWNER,
    aterm_agent::harness::upgrade_drive::ATTENTION_OWNER,
];

/// The wall kinds a usage limit reads as (`aterm_phase::WallKind::reads_limited`
/// less the API's 429, which is an API error).
const LIMIT_WALLS: [&str; 4] = ["usage-session", "usage-weekly", "model-bucket", "spend"];

/// THE CLASSIFIER: `facts` → [`Verdict`] (the module doc's rule).
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
    // A wall that needs the owner is waited out by nobody, whatever an
    // entry says of it.
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
/// the person's: the stall's own text up to its remedy (`perl is frozen: not
/// reading input since 14:02 (1 B queued)`), since the remedy that text ends
/// with is an agent's command line (`— restart it: aterm ctl @s-… signal
/// term`), and a person's remedy in its place where the pill carries no act
/// for it ([`stall_remedy`]). The attention entry and the wire keep the
/// whole text.
fn stall_need(facts: &NeedFacts) -> Option<Need> {
    let stall = facts.stall.as_ref()?;
    let words = stall_words(&stall.text);
    // The pill's Resume (a stopped job) or Restart (offered now) is the
    // remedy: the words alone.
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
    })
}

/// A FROZEN PROGRAM THE PILL OFFERS NO RESTART FOR keeps a person's remedy:
/// Hand Back where the restart waits on a hold Hand Back lifts
/// (`hand_back`), else the window's own ways out — `⌃C`, or closing the tab
/// — then the tab's own resume line, whole, as the memory wall's remedy
/// carries it ([`memory_remedy`]: its shorter forms, else none). An
/// unsupervised Claude Code, any program that is not an agent, and the tab
/// the person took over (where the hold refuses the restart) all say what to
/// do.
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
/// (`input_stall::attention_text` puts the remedy after one, and the facts
/// before it carry none).
fn stall_words(text: &str) -> &str {
    text.split_once(" \u{2014} ")
        .map_or(text, |(words, _)| words)
}

/// A stalled task: the window host's deadline's words where its attention
/// stands, else the why in words.
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
    })
}

/// What `agent`'s wall of `kind` makes of the session: a need, a wait, and
/// when an unsupervised limit's reset changes it.
///
/// `supervisor_waits`: the supervisor loop's own entry
/// ([`aterm_agent::supervise::ATTENTION_OWNER`], what the window harness
/// writes) says it waits the wall out (`kind=wait`). It is
/// honoured only while a supervisor's claim is live and a harness supervises
/// the session: the entry is not tied to the claim, so a loop that died
/// leaves it standing, and the limit is then the person's at its reset like
/// any other unsupervised one. Any other writer's wait is a record that
/// waits no wall out: a script's `kind=wait` never hides a limit the person
/// must continue.
///
/// A HOLD parks every harness (the server answers its writes `ERR halted`),
/// so a wall this host's harness would retry or compact is nobody's while
/// one stands, as a box is ([`box_need`]) and as another supervisor's walls
/// are. A limit's answer outlives a hold: before its reset no act shortens
/// it, and past it the harness (or Claude Code by itself) continues it once
/// the hold lifts; whoever holds the tab there holds it knowingly, and a
/// hold is the holder's own act, never a need (§2.2).
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
    // What this host's harness retries or compacts, while no hold parks it.
    let answered_now = answered && unheld;
    // The walls another supervisor's loop retries are its, as its boxes are
    // ([`box_need`]), unless a hold keeps its hands off.
    let retried_elsewhere = facts.other_supervisor && unheld;
    let waited = supervisor_waits && facts.claim_live && facts.supervised;
    match kind {
        // The stall the server publishes on an identified agent: the stall
        // fact's own need — its word (a stopped job is not frozen), its
        // words with their remedy, its episode's key, so one stall is one
        // notice. The verdict alone names the word its detail gives.
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
        // Claude Code's lost login under this host's harness, while no hold
        // parks it: the harness carries it (an account move, or its own
        // `/login`), and the one step that is the owner's — the browser
        // sign-in — is its notice's ask on the glass, in the notice's words.
        "auth" if answered_now => (None, wait(WaitWord::Login), None),
        // Unsupervised, or left to the owner: only a person can log in.
        "auth" => (
            need(NeedWord::Login, "logged out: log in there".to_string()),
            None,
            None,
        ),
        // The banner is drawn under a running spinner, where no supervisor
        // decides: the host's restart, or the person's.
        "memory" if facts.restarts => (None, wait(WaitWord::MemoryRestart), None),
        "memory" => (
            need(NeedWord::Memory, memory_remedy(facts.resume.as_deref())),
            None,
            None,
        ),
        // The network coming back answers it, supervised or not.
        "api-error" if agent.subject.as_deref() == Some(crate::presence::API_UNREACHABLE) => {
            (None, wait(WaitWord::Api), None)
        }
        "overloaded" if answered_now || retried_elsewhere => {
            (None, wait(WaitWord::Overloaded), None)
        }
        kind if LIMIT_WALLS.contains(&kind) => {
            // Answered by this host's harness, or waited out by a live
            // supervisor loop that says so (its `kind=wait`).
            if answered || waited {
                return (None, wait(WaitWord::Limit), None);
            }
            // Unsupervised: nothing to do before its reset; past it (or with
            // no reset this machine can place) the person continues it.
            match facts.reset_unix {
                Some(at) if at > facts.now_unix => (None, wait(WaitWord::Limit), Some(at)),
                // Its reset has passed: the words say so, never `until`.
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

/// An unsupervised limit PAST ITS RESET: it needs the person to continue it
/// now, so its words say the reset passed (`session usage limit reset at
/// 5:11pm — continue it there`), never `until`, which reads as a limit still
/// being waited out. `until` stays the Waiting row's, before the reset.
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
/// (the verdict's host-side subject).
fn wall_what_of(agent: &AgentFact, kind: &str, reset: Option<&str>) -> String {
    match agent.subject.as_deref().filter(|s| !s.is_empty()) {
        Some(cause) if kind == "api-error" => fold_clip(cause, ROW_WHAT_MAX),
        _ => wall_what(kind, reset),
    }
}

/// What the attention entries make of the session: the most recent ask,
/// the most recent wait (a record, in its writer's words: [`WaitWord::Entry`]),
/// and whether the supervisor loop's own entry is one (what may wait a wall
/// out, [`wall_outcome`]).
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
                ask = Some(Need {
                    word: NeedWord::Ask,
                    key: ask_key(text),
                    what: fold_clip(text.trim_start_matches('⚠').trim_start(), ROW_WHAT_MAX),
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

/// An entry's words without a leading `kind=` an older server stored as
/// text ([`resolved_kind`] reads past it the same way).
fn entry_words(text: &str) -> &str {
    let text = text.trim();
    match text.split_once(' ') {
        Some((head, rest)) if head.starts_with("kind=") => rest.trim_start(),
        _ => text,
    }
}

/// An ask's [`Need::key`]: its entry's text's hash (trimmed). A reader that
/// shows the entry's whole text (the survey's excerpt) finds the entry the
/// published ask names by it.
pub(crate) fn ask_key(text: &str) -> u64 {
    aterm_hash::fnv1a_64(text.trim().as_bytes())
}

/// An entry's kind: the one its writer named, else (§2.4's legacy rule) a
/// wait only where its text opens with the harness's limit prefix while the
/// harness answers the agent's wall, and an ask otherwise. A leading `kind=`
/// an older server stored as text is read past.
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
/// keeps every harness's hands off.
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
            })
        }
        "question" => Some(Need {
            word: NeedWord::Question,
            key: agent.rev,
            what: "question".to_string(),
        }),
        _ => None,
    }
}

/// The memory wall's remedy (`wall:memory`, the host not restarting it): the
/// restart, then the tab's own resume line — whole, never cut: the line with
/// its carried flags where it fits [`REMEDY_TEXT_MAX`], else its shorter
/// forms ([`aterm_agent::harness::resume::shorter`]), else the restart alone.
/// Never the banner's `claude --continue`, the directory's newest
/// conversation (2026-09-26).
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

/// A wall kind (`aterm_phase::WallKind::name`) in a person's words; a kind
/// this build does not know stays as it came.
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

/// `s` folded to one line (control characters and runs of whitespace become
/// one space) and clipped to `max` bytes on a char boundary, with `…` when
/// cut. Row text comes from titles, commands and meta values — none of it may
/// carry a newline into a menu item or a notification. The characters the
/// crate forbids in native chrome
/// ([`crate::session_timeline::is_forbidden_metadata_char`]: bidi overrides
/// and isolates, zero-width and other invisible format characters) are
/// dropped too: a box's command is program-written text the human approves
/// from, and `rm -rf \u{202e}…` must not render reordered in a menu row.
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
