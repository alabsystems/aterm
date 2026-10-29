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

/// Where this machine's one Grok Build sign-in stands (`aterm harness signin --cli grok`,
/// [`aterm_agent::harness::grok_signin`]), read for a Grok session on a sign-in wall alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrokSignin {
    /// A sign-in runs: its one need (code and page) stands on the session that leads it.
    Running,
    /// A login this session's grok may not have read (written after it started): only a restart
    /// there reads it.
    SignedIn,
    /// Signed out, or the login was refused by this session's grok (written before it started),
    /// and no sign-in runs.
    Idle,
}

/// The one need every Grok session on a sign-in wall shares while this machine is signed out
/// and no sign-in runs ([`machine_wide`]): its remedy is the sign-in that session's grok already
/// runs, whose code is on its screen and whose page it already opened, never a second device
/// flow with another code (the window starts no sign-in of its own).
pub(crate) const GROK_SIGNIN_WHAT: &str =
    "signed out of Grok Build: approve the sign-in on its screen";

/// A Grok session's sign-in wall while this machine's sign-in runs.
pub(crate) const GROK_SIGNIN_WAIT: &str = "signing Grok Build in for this machine";

/// A Grok session's sign-in wall once this machine is signed in.
pub(crate) const GROK_SIGNED_IN_WHAT: &str =
    "Grok Build is signed in on this machine: restart grok here";

/// Whether `need` is this machine's rather than its session's: every session it stands in
/// shares it, so a surface listing the machine's needs lists it once.
#[must_use]
pub(crate) fn machine_wide(need: &Need) -> bool {
    need.word == NeedWord::Login && need.what == GROK_SIGNIN_WHAT
}

/// The program's own report that it is blocked (OSC 7501 `blocked`, codex's claim), kept apart
/// from [`AgentFact`]: a claim never moves the agent verdict ([`program_need`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProgramClaimFact {
    /// What the report says it is blocked on, where it said.
    pub(crate) kind: Option<aterm_core::terminal::program_state::ProgramBlockedKind>,
    /// The shown report's episode ([`crate::session_timeline::ShownProgram::seq`]): one box,
    /// one key.
    pub(crate) key: u64,
    /// The engine's sequence where that report began
    /// ([`crate::session_timeline::ShownProgram::report`]).
    pub(crate) report: u64,
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
    /// A codex whose reader names no state (`agent=unknown`) reports it is blocked: its own claim.
    pub(crate) program_blocked: Option<ProgramClaimFact>,
    /// The live claim on the session is a proven self-claim: the program carries itself
    /// ([`crate::session_timeline::SessionMeta::supervisor_proven`]).
    pub(crate) self_claimed: bool,
    /// A harness presses this session's boxes.
    pub(crate) supervised: bool,
    /// Another holder's `meta set supervisor` claim is live and speaks for this session's boxes
    /// and walls ([`crate::session_timeline::SupervisorClaim::speaks_for_boxes_and_walls`]): every
    /// claim but a contract v3 scoped one whose scope does not name approvals, questions and walls.
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
    /// The worker's needs you sets as whoever drives it told them, whole ([`told_of`]).
    pub(crate) told: ToldNeeds,
    /// This machine's Grok Build sign-in, for a Grok session on a sign-in wall alone.
    pub(crate) grok_signin: Option<GrokSignin>,
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

/// THE NEEDS YOU SETS EVERY READER READS FOR `ctx`'s SESSION (the facts' [`NeedFacts::told`],
/// so the menu bar's rows one level in, the hover's lines and [`HarnessAsk::others`], and `status
/// needs_told=`), told by whoever drives it: under a proven self-claim, the sets that claim told
/// of its own entry while it stands (`meta set harness_needs`,
/// [`crate::session_timeline::SessionMeta::self_needs`]), else the sets aterm's own loop told the
/// window's host ([`crate::session_timeline::SessionTimeline::harness_needs`]). Each leaf lock is
/// taken alone, the meta's first.
#[must_use]
pub(crate) fn told_of(ctx: &crate::SessionCtx, now_us: u64) -> ToldNeeds {
    let claimed = ctx
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .self_needs(now_us);
    claimed.unwrap_or_else(|| {
        ctx.timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .harness_needs()
    })
}

/// The worker's needs you as the window holds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToldNeeds {
    /// The set told last.
    current: Option<std::sync::Arc<HarnessNeeds>>,
    /// The set that described the needs entry standing when [`Self::current`] was told, while
    /// that entry stands ([`Self::settle`]).
    outgoing: Option<std::sync::Arc<HarnessNeeds>>,
}

impl ToldNeeds {
    /// The tell ([`aterm_agent::supervise::IdleHost::needs_standing`]).
    pub(crate) fn tell(
        &mut self,
        told: Option<std::sync::Arc<HarnessNeeds>>,
        standing: Option<&str>,
    ) -> bool {
        self.settle(standing);
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

    /// DROP AN OUTGOING SET ONCE THE ENTRY IT DESCRIBED NO LONGER STANDS (`standing`, the needs
    /// owner's entry as stored now): it bridged only until the told set's entry replaced that one,
    /// so an entry that goes, or gives way to another, takes it, and the same text set again later
    /// never reads a set its claimant took down. The set told last still waits for its entry.
    pub(crate) fn settle(&mut self, standing: Option<&str>) {
        if self
            .outgoing
            .as_ref()
            .is_some_and(|held| held.words_for(standing).is_none())
        {
            self.outgoing = None;
        }
    }

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

    /// The needs, where `standing` — the needs owner's entry as the server
    /// stored it, `None` where none stands — is the text they were told
    /// with, both through the presentation sanitizer; else `None`.
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
    /// The engine's sequence of the program's own `blocked` report this need stands on (a
    /// program-claim need, [`program_need`]); `None` for every need aterm reads itself.
    pub(crate) report: Option<u64>,
}

/// A harness's needs you, in the worker's own words ([`Need::harness`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessAsk {
    /// The first need without the frame, folded to one line.
    pub(crate) first: String,
    /// How many further needs stand (the told set's others and the needs its entry's ` (+N more)`
    /// counts past them, else the entry's ` (+N more)`).
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

    /// [`Self::told`] beside an entry that counts `counted` further needs (its ` (+N more)`): THE
    /// ENTRY COUNTS WHAT THE SET DOES NOT CARRY, since a self-claim tells only its first
    /// `HARNESS_NEEDS_MAX` needs (`meta set harness_needs`) and its entry counts every need past
    /// the first, so the needs it counts past the told set's own are counted on in [`Self::more`].
    #[must_use]
    pub(crate) fn told_counted(
        needs: &[String],
        program: aterm_phase::Program,
        counted: u16,
    ) -> Option<Self> {
        let mut ask = Self::told(needs, program)?;
        let untold = usize::from(counted).saturating_sub(needs.len().saturating_sub(1));
        ask.more = u16::try_from(ask.others.len().saturating_add(untold)).unwrap_or(u16::MAX);
        Some(ask)
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

const NEEDS_YOU: &str = " needs you: ";
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
        // A Grok sign-in wall's wait carries its words ([`GROK_SIGNIN_WAIT`]).
        (WaitWord::Login, _) => wait
            .entry
            .as_ref()
            .map_or_else(|| "signing Claude back in".to_string(), |e| e.text.clone()),
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

const NOT_ASKS: [&str; 2] = [
    crate::input_stall::SERVER_ATTENTION_OWNER,
    aterm_agent::harness::upgrade_drive::ATTENTION_OWNER,
];

const LIMIT_WALLS: [&str; 4] = ["usage-session", "usage-weekly", "model-bucket", "spend"];

/// Classify `facts` into a [`Verdict`].
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
    let boxed = box_need(facts).or_else(|| program_need(facts));
    let ask = ask.filter(|need| need.harness.is_none() || boxed.is_none());
    let candidates = [
        stall_need(facts),
        task_need(facts),
        wall_need.clone(),
        ask,
        boxed,
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
        report: None,
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

fn stall_words(text: &str) -> &str {
    text.split_once(" \u{2014} ")
        .map_or(text, |(words, _)| words)
}

fn task_need(facts: &NeedFacts) -> Option<Need> {
    let task = facts.task.as_ref().filter(|task| task.stalled)?;
    let deadline = facts
        .attention
        .iter()
        .find(|entry| aterm_agent::supervise::deadline_attention(&entry.text));
    let what = match deadline {
        Some(entry) => entry.text.clone(),
        None => crate::task_record::stall_words(task.why),
    };
    Some(Need {
        word: NeedWord::Stalled,
        key: task.entered,
        what: fold_clip(&what, REMEDY_TEXT_MAX),
        harness: None,
        report: None,
    })
}

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
            report: None,
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
        // Grok: the machine's one sign-in, never a page per session (`harness::grok_signin`).
        "auth" if facts.grok_signin == Some(GrokSignin::Running) => (
            None,
            Some(Waiting {
                word: WaitWord::Login,
                wall: Some(kind),
                reset: None,
                entry: Some(WaitEntry {
                    owner: aterm_agent::harness::grok_signin::NEED_OWNER.to_string(),
                    text: GROK_SIGNIN_WAIT.to_string(),
                }),
            }),
            None,
        ),
        "auth" if facts.grok_signin == Some(GrokSignin::Idle) => (
            need(NeedWord::Login, GROK_SIGNIN_WHAT.to_string()),
            None,
            None,
        ),
        "auth" if facts.grok_signin == Some(GrokSignin::SignedIn) => (
            need(NeedWord::Login, GROK_SIGNED_IN_WHAT.to_string()),
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
            // supervisor loop that says so (its `kind=wait`).
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

fn wall_what_of(agent: &AgentFact, kind: &str, reset: Option<&str>) -> String {
    match agent.subject.as_deref().filter(|s| !s.is_empty()) {
        Some(cause) if kind == "api-error" => fold_clip(cause, ROW_WHAT_MAX),
        _ => wall_what(kind, reset),
    }
}

fn attention_outcome(facts: &NeedFacts) -> (Option<Need>, Option<Waiting>, bool) {
    let mut ask = None;
    let mut reported_ask = None;
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
                let need = Need {
                    word: NeedWord::Ask,
                    key: ask_key(text),
                    what,
                    harness,
                    report: None,
                };
                if need.harness.is_some() {
                    reported_ask.get_or_insert(need);
                } else {
                    ask = Some(need);
                }
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
    (ask.or(reported_ask), wait, supervisor_waits)
}

fn harness_ask(text: &str, told: Option<&[String]>) -> Option<HarnessAsk> {
    let text = entry_words(text);
    let (head, rest) = text.split_once(NEEDS_YOU)?;
    let program = aterm_phase::Program::ALL
        .into_iter()
        .find(|program| program.name().eq_ignore_ascii_case(head.trim()))?;
    let parsed = need_words(text);
    let counted = parsed.map_or(0, |words| words.more);
    told.and_then(|needs| HarnessAsk::told_counted(needs, program, counted))
        .or_else(|| parsed.map(HarnessAsk::of))
        .or_else(|| {
            let (first, _) = without_carrying_on(rest);
            let first = first.trim();
            (!first.is_empty()).then(|| {
                HarnessAsk::of(NeedWords {
                    first,
                    more: 0,
                    program,
                })
            })
        })
}

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
                report: None,
            })
        }
        "question" => Some(Need {
            word: NeedWord::Question,
            key: agent.rev,
            what: "question".to_string(),
            harness: None,
            report: None,
        }),
        _ => None,
    }
}

/// The words of a program-claim need: the box codex says it waits on, said as its own report.
pub(crate) const CODEX_APPROVAL_WORDS: &str = "approval (codex's own report)";
/// [`CODEX_APPROVAL_WORDS`] for a question.
pub(crate) const CODEX_QUESTION_WORDS: &str = "question (codex's own report)";
/// [`CODEX_APPROVAL_WORDS`] for a sign-in.
pub(crate) const CODEX_SIGN_IN_WORDS: &str = "sign-in (codex's own report)";

/// The words for a need another aterm's survey row publishes as the program's own report
/// (`needs_by=program`): the program-claim need's words for its need word and the row's
/// `agent=`, the survey's word for the report (`wall:auth` a sign-in); `None` for a need of
/// another word.
pub(crate) fn claim_words_of_row(
    word: aterm_types::needs::NeedWord,
    agent: Option<&str>,
) -> Option<&'static str> {
    use aterm_types::needs::NeedWord;
    match word {
        NeedWord::Question => Some(CODEX_QUESTION_WORDS),
        NeedWord::Approval if agent == Some("wall:auth") => Some(CODEX_SIGN_IN_WORDS),
        NeedWord::Approval => Some(CODEX_APPROVAL_WORDS),
        _ => None,
    }
}

/// THE PROGRAM-CLAIM NEED: codex says it is blocked (OSC 7501) where its reader names no state
/// (`agent=unknown`) and no claim speaks for the tab. It is codex's claim, so it never moves
/// `agent=` and its words say whose report it is; it stands where a Claude Code box would. None
/// while the tab carries itself (a proven self-claim, which presses and raises its own points),
/// while another holder's unproven claim stands unheld (`box_need`'s rule for a harness pressing
/// boxes), and while codex-alab's own attention entry stands (its startup screens raise one with
/// no claim). A sign-in stays at the approval's severity: a claim raises no stop tone, and
/// codex's sign-in box is a user check or an app link, never a logged-out wall.
fn program_need(facts: &NeedFacts) -> Option<Need> {
    use aterm_core::terminal::program_state::ProgramBlockedKind as K;
    facts
        .agent
        .as_ref()
        .filter(|agent| agent.word == "unknown")?;
    let claim = facts.program_blocked?;
    if facts.self_claimed || (facts.other_supervisor && facts.hold.is_none()) {
        return None;
    }
    if facts
        .attention
        .iter()
        .any(|entry| entry.owner == crate::task_record::SELF_CLAIM_ATTENTION_OWNER)
    {
        return None;
    }
    let (word, what) = match claim.kind {
        Some(K::Question) => (NeedWord::Question, CODEX_QUESTION_WORDS),
        Some(K::Auth) => (NeedWord::Approval, CODEX_SIGN_IN_WORDS),
        Some(K::Permission) | None => (NeedWord::Approval, CODEX_APPROVAL_WORDS),
    };
    Some(Need {
        word,
        key: claim.key,
        what: what.to_string(),
        harness: None,
        report: Some(claim.report),
    })
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
