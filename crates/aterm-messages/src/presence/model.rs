// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence facts ([`Facts::need`]), per-session [`Slot`] state, wire severity ([`Level`]), and
//! window border/tab indicators ([`Rim`], [`ChipLevel`]).

use std::borrow::Cow;
use std::collections::VecDeque;

use super::{Host, STORY_CAP};
use crate::Instant;

/// What the classifier read off one screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentReading<V: Host> {
    /// The worker's phase.
    pub phase: AgentPhase<V>,
    /// The context left, in percent, when the screen shows it.
    pub context_pct: Option<u8>,
}

/// The worker's phase as the wire spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentPhase<V: Host> {
    /// Working.
    Busy,
    /// An approval box is showing.
    Prompt {
        /// `kind[:verdict]`.
        detail: Option<String>,
    },
    /// The agent asked a question.
    Question,
    /// The last turn ended on a wall.
    Wall {
        /// The wall's kind ([`Host::Wall`]).
        kind: V::Wall,
        /// The reset time the notice named, sanitized.
        reset: Option<String>,
        /// When the reset falls, if the notice's time could be placed on the host's clock.
        until: Option<Instant>,
    },
    /// Waiting at its composer.
    Idle,
    /// Idle with the session survey parked above the composer.
    Survey,
    /// An identified agent whose reader has no evidence for a phase.
    Unknown,
}

impl<V: Host> AgentPhase<V> {
    /// The `agent=` wire word (`wall:<kind>` for a wall — [`Host::wall_wire`]).
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Prompt { .. } => "prompt",
            Self::Question => "question",
            Self::Wall { kind, .. } => V::wall_wire(*kind),
            Self::Idle => "idle",
            Self::Survey => "survey",
            Self::Unknown => "unknown",
        }
    }

    /// The word a person reads ([`super::state_line`]).
    #[must_use]
    pub fn band_word(&self) -> &'static str {
        match self {
            Self::Wall { kind, .. } => V::wall_band(*kind),
            Self::Survey => "idle",
            other => other.word(),
        }
    }

    /// What the phase's age counts from: its phase word, a box by the kind it
    /// names ([`super::prompt_band_word`]), so a box that follows another
    /// starts its own age.
    pub(super) fn age_key(&self) -> Cow<'static, str> {
        match self {
            Self::Prompt { detail } => super::prompt_band_word(detail.as_deref()).into(),
            other => other.band_word().into(),
        }
    }
}

/// Whose hand is on this session's keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hand {
    /// Nobody's.
    None,
    /// A peer's `turn` is open.
    DrivenTurn {
        /// The turn's id.
        id: u64,
        /// The driver, when the lease names one.
        holder: Option<String>,
    },
    /// A cooperative drive lease is held.
    DrivenLease {
        /// The lease's holder name.
        holder: String,
        /// The kernel's pid of the process that took the lease.
        pid: Option<u32>,
    },
    /// This session drives another.
    Driving {
        /// The driven peer's short sid.
        sid: String,
    },
}

impl Hand {
    /// The `status hand=` token.
    #[must_use]
    pub fn wire<V: Host>(&self) -> String {
        match self {
            Self::None => "-".to_string(),
            Self::DrivenTurn { id, holder: None } => format!("turn:{id}"),
            Self::DrivenTurn {
                id,
                holder: Some(h),
            } => format!("turn:{id}:{}", V::pct_encode(h)),
            Self::DrivenLease { holder, .. } => format!("lease:{}", V::pct_encode(holder)),
            Self::Driving { sid } => format!("driving:{}", V::pct_encode(sid)),
        }
    }

    /// The `status hand_pid=` token.
    #[must_use]
    pub fn pid_wire(&self) -> String {
        match self {
            Self::DrivenLease { pid: Some(pid), .. } => pid.to_string(),
            _ => "-".to_string(),
        }
    }
}

/// Whether `hand` is this process's own harness.
#[must_use]
pub(crate) fn is_aterms_hand<V: Host>(hand: &Hand) -> bool {
    matches!(hand, Hand::DrivenLease { holder, pid } if V::is_aterms_holder(holder, *pid))
}

/// The standing halt, as the wire reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldFact {
    /// The wire's (percent-encoded) reason.
    pub reason: String,
    /// `origin=fleet` (the bridge's).
    pub fleet: bool,
}

/// The newest inbox row, trust first (the receiver's verdict is the first thing a reader sees).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailLast {
    /// Its kind (`task`, `ask`, `note`, …).
    pub kind: String,
    /// Its sender token.
    pub from: String,
    /// One of the wire trusts (`agent`, `human`, `unknown`, `forged-self`, …).
    pub trust: String,
}

/// The mail counts `status level=` weighs.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MailFacts {
    /// Unread rows.
    pub unread: u64,
    /// Rows delivered, not yet seen through.
    pub pending: u64,
    /// Rows dropped.
    pub dropped: u64,
    /// Posts queued for the bridge.
    pub queued: u64,
    /// The newest row.
    pub last: Option<MailLast>,
    /// The highest row id ever delivered.
    pub head: u64,
}

/// The instance's bridge link, as `status`'s `fabric=` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// No bridge.
    Absent,
    /// Up.
    Connected {
        /// The last round trip, when measured.
        rtt_ms: Option<u64>,
    },
    /// A bridge is attached but its broker link is down.
    Stalled {
        /// How long the link has been down.
        age_ms: Option<u64>,
    },
    /// Lost.
    Disconnected,
}

/// The last completed turn on this session's ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnFact {
    /// The turn's id.
    pub id: u64,
    /// It settled (else it timed out).
    pub settled: bool,
    /// How long it took.
    pub dur_ms: u64,
    /// The record came from an earlier process across a self-update handoff.
    pub carried: bool,
    /// This process's harness typed it (the ledger's `by=harness`).
    pub aterm: bool,
    /// When the turn opened.
    pub opened: Option<Instant>,
}

/// Who answers the wall the agent is at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WallAnswer {
    /// Nobody: no supervisor, the owner's switch off, a lost login, a critical-memory banner.
    #[default]
    Nobody,
    /// The harness resumes it by itself (or Claude Code does, with the harness the net under it).
    Harness,
    /// The harness resumes it slowly and a person can lift it faster.
    HarnessAsks(&'static str),
}

/// How a need presses: the two severities of the host's need words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NeedSeverity {
    /// Nothing moves until the person acts.
    Stuck,
    /// The person decides: the wait tone.
    Decide,
}

/// The published need, as the presence engine reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedFact {
    /// The need word's severity.
    pub severity: NeedSeverity,
    /// What a stuck need's remedy turns on.
    pub cause: StopCause,
    /// The word and the transition's identity within it, as one key.
    pub key: (u8, u64),
    /// Its words, one clipped line, without the tab's label.
    pub what: String,
    /// Whether to display a tab-bar pill for this need.
    pub show_pill: bool,
}

/// Everything one refresh reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts<V: Host> {
    /// `meta attention` (a typed escalation).
    pub attention: Option<String>,
    /// The shell status word (`running`, `idle`, `quiet`, …) and when it was published.
    pub shell: Option<(&'static str, Instant)>,
    /// The program's own state report (OSC 7501) the host honours now.
    pub program: Option<(&'static str, Instant)>,
    /// The sequence number of the host's agent reading.
    pub agent_seq: u64,
    /// The agent reading at `agent_seq`.
    pub agent: Option<AgentReading<V>>,
    /// Whose hand is on the keyboard.
    pub hand: Hand,
    /// When a cooperative drive lease lapses.
    pub lease_until: Option<Instant>,
    /// The standing hold (`status level=hold`).
    pub hold: Option<HoldFact>,
    /// The mail counts.
    pub mail: MailFacts,
    /// The bridge link.
    pub link: Link,
    /// The last completed turn.
    pub turn: Option<TurnFact>,
    /// The host's published input stall.
    pub input_stall: Option<V::Stall>,
    /// Who answers the agent's wall, if it is at one ([`WallAnswer`]).
    pub wall_answer: WallAnswer,
    /// The published need ([`NeedFact`]).
    pub need: Option<NeedFact>,
    /// The person's own hold stands on the session (the published `hold_by=person`).
    pub held_by_person: bool,
    /// How many times the program in front has reported its own turn ending.
    pub program_turn_ends: u64,
    /// The session is in front of a window the person has focused.
    pub program_turn_watched: bool,
    /// How many turns the program in front has begun by its own report.
    pub program_turn_starts: u64,
    /// When the host's gate counted the program's latest turn start.
    pub program_turn_began: Option<Instant>,
}

impl<V: Host> Default for Facts<V> {
    fn default() -> Self {
        Self {
            attention: None,
            shell: None,
            program: None,
            agent_seq: 0,
            agent: None,
            hand: Hand::None,
            lease_until: None,
            hold: None,
            mail: MailFacts::default(),
            link: Link::Absent,
            turn: None,
            input_stall: None,
            wall_answer: WallAnswer::Nobody,
            need: None,
            held_by_person: false,
            program_turn_ends: 0,
            program_turn_watched: false,
            program_turn_starts: 0,
            program_turn_began: None,
        }
    }
}

/// Which of a driven turn's two ends was told ([`Slot::absorb`]): the
/// ledger's settle, or the program's own report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnEnd {
    Settle,
    Done,
}

/// The one turn end told and not yet paired ([`Slot::absorb`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToldEnd {
    /// Which end it is.
    end: TurnEnd,
    /// The program's turn-start count it was read at ([`Facts::program_turn_starts`]).
    starts: u64,
    /// When its turn began: a settle's, when the ledger turn opened ([`TurnFact::opened`]).
    began: Option<Instant>,
    /// The story point it was told as, and whether that point was news.
    seq: u64,
    news: bool,
}

/// Whether a program turn that began at `program` is the ledger turn that
/// opened at `ledger`: it began no earlier ([`ToldEnd::began`]).
fn began_in(program: Option<Instant>, ledger: Option<Instant>) -> bool {
    match (program, ledger) {
        (Some(program), Some(ledger)) => program >= ledger,
        _ => true,
    }
}

/// What a story point says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoryVerb {
    /// A turn settled.
    Turn,
    /// The program in front reported its own turn ended (OSC 7501 `working` → `done`|`idle`).
    ProgramDone,
    /// A turn whose settle deadline passed (`status=timeout` on the ledger).
    TurnTimedOut,
    /// Mail arrived.
    Mail,
    /// A hold began.
    Hold,
    /// The agent hit a wall.
    Limited,
    /// The agent asked.
    Question,
    /// A stop (hold / limited) ended.
    Resumed,
    /// `ctl story chose <policy>`.
    Chose,
}

impl StoryVerb {
    /// The closed set of words `aterm ctl story <verb>` accepts.
    pub const TOLD_WORDS: [&'static str; 1] = ["chose"];

    /// The verb a `ctl story <word>` names, `None` outside the closed set.
    #[must_use]
    pub fn parse_told(word: &str) -> Option<Self> {
        match word {
            "chose" => Some(Self::Chose),
            _ => None,
        }
    }
}

/// One point of a session's story.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoryPoint {
    /// Its sequence number (1 = the first).
    pub seq: u64,
    /// When it happened.
    pub at: Instant,
    /// What happened.
    pub verb: StoryVerb,
    /// The point happened under aterm's own harness's hand.
    pub aterm: bool,
}

/// A session's presence, as the last refresh left it.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent facts of one slot: its baseline, aterm's hand now and at either end of an absorb, and the person's hold; no two are states of one thing"
)]
pub struct Slot<V: Host> {
    /// The agent-reading sequence last absorbed ([`Facts::agent_seq`]).
    pub agent_seq_seen: Option<u64>,
    /// [`Facts::attention`].
    pub attention: Option<String>,
    /// [`Facts::shell`].
    pub shell: Option<(&'static str, Instant)>,
    /// [`Facts::program`].
    pub program: Option<(&'static str, Instant)>,
    /// [`Facts::agent`].
    pub agent: Option<AgentReading<V>>,
    /// When the agent phase word last changed.
    pub agent_since: Instant,
    /// [`Facts::hand`].
    pub hand: Hand,
    /// When the cooperative lease behind `hand` lapses (see [`Facts::lease_until`]).
    pub lease_until: Option<Instant>,
    /// [`Facts::hold`].
    pub hold: Option<HoldFact>,
    /// [`Facts::mail`].
    pub mail: MailFacts,
    /// [`Facts::link`].
    pub link: Link,
    /// [`Facts::turn`].
    pub turn: Option<TurnFact>,
    /// [`Facts::input_stall`].
    pub input_stall: Option<V::Stall>,
    /// [`Facts::wall_answer`].
    pub wall_answer: WallAnswer,
    /// [`Facts::need`].
    pub need: Option<NeedFact>,
    /// When the standing need began.
    pub need_since: Instant,
    /// [`Facts::held_by_person`].
    pub held_by_person: bool,
    /// [`Facts::program_turn_ends`], as the last refresh read it.
    pub program_turn_ends: u64,
    /// How many of the program's own turn ends were news.
    pub turn_end_news: u64,
    /// The one turn end told and not yet paired.
    told_end: Option<ToldEnd>,
    /// Whether a refresh has been absorbed.
    baselined: bool,
    story: VecDeque<StoryPoint>,
    /// The seq of the newest story point.
    pub story_seq: u64,
    /// The seq of the newest point that is news.
    news_seq: u64,
    /// Aterm's own harness has its hand on the session.
    pub(super) aterm_hand: bool,
    /// Set for the length of one [`Self::absorb`] in which aterm's hand was on the session.
    noting_aterm: bool,
    /// The story up to this seq is closed.
    story_closed: u64,
}

impl<V: Host> Slot<V> {
    /// A slot minted at `now`, before its first refresh.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            agent_seq_seen: None,
            attention: None,
            shell: None,
            program: None,
            agent: None,
            agent_since: now,
            hand: Hand::None,
            lease_until: None,
            hold: None,
            mail: MailFacts::default(),
            link: Link::Absent,
            turn: None,
            input_stall: None,
            wall_answer: WallAnswer::Nobody,
            need: None,
            need_since: now,
            held_by_person: false,
            program_turn_ends: 0,
            turn_end_news: 0,
            told_end: None,
            baselined: false,
            story: VecDeque::new(),
            story_seq: 0,
            news_seq: 0,
            aterm_hand: false,
            noting_aterm: false,
            story_closed: 0,
        }
    }

    /// `aterm ctl story <verb> [<text>]` landed.
    pub fn tell(&mut self, verb: StoryVerb, now: Instant) -> u64 {
        self.note(verb, now);
        self.story_seq
    }

    fn note(&mut self, verb: StoryVerb, now: Instant) {
        let _ = self.note_as(verb, now, true);
    }

    /// Note a point; `news` false keeps it out of the news a level reads (the
    /// person saw it happen), as aterm's own points are.
    fn note_as(&mut self, verb: StoryVerb, now: Instant, news: bool) -> bool {
        self.story_seq += 1;
        let aterm = self.noting_aterm || self.aterm_hand;
        let news = !aterm && news;
        if news {
            self.news_seq = self.story_seq;
        }
        if self.story.len() >= STORY_CAP {
            self.story.pop_front();
        }
        self.story.push_back(StoryPoint {
            seq: self.story_seq,
            at: now,
            verb,
            aterm,
        });
        news
    }

    /// The second end of a told turn, `news` the news it would have carried
    /// told alone: the turn is one story point either way, and news when
    /// either end would have been, whichever was read first.
    fn pair_end(&mut self, told: ToldEnd, news: bool) -> bool {
        self.told_end = None;
        let aterm = self.noting_aterm || self.aterm_hand;
        if news
            && !aterm
            && !told.news
            && self
                .story
                .iter()
                .any(|point| point.seq == told.seq && !point.aterm)
        {
            self.news_seq = self.news_seq.max(told.seq);
            return true;
        }
        told.news
    }

    /// Fold one refresh's facts.
    #[allow(
        clippy::too_many_lines,
        reason = "one fold per fact, in the order the facts are declared; the story points depend on that order"
    )]
    pub fn absorb(&mut self, facts: Facts<V>, now: Instant) -> bool {
        let mut changed = false;
        let was_aterm = self.aterm_hand;
        let now_aterm = is_aterms_hand::<V>(&facts.hand)
            || (was_aterm && matches!(facts.hand, Hand::DrivenTurn { holder: None, .. }));
        self.noting_aterm = was_aterm || now_aterm;
        if self.aterm_hand != now_aterm {
            self.aterm_hand = now_aterm;
            changed = true;
        }
        if self.attention != facts.attention {
            self.attention = facts.attention;
            changed = true;
        }
        if self.wall_answer != facts.wall_answer {
            self.wall_answer = facts.wall_answer;
            changed = true;
        }
        if self.shell.map(|s| s.0) != facts.shell.map(|s| s.0) {
            changed = true;
        }
        self.shell = facts.shell;
        if self.program.map(|p| p.0) != facts.program.map(|p| p.0) {
            changed = true;
        }
        self.program = facts.program;
        if facts.agent_seq > self.agent_seq_seen.unwrap_or(0) {
            self.agent_seq_seen = Some(facts.agent_seq);
            let agent = facts.agent;
            let word_moved = self.agent.as_ref().map(|a| a.phase.word())
                != agent.as_ref().map(|a| a.phase.word());
            if self.agent.as_ref().map(|a| a.phase.age_key())
                != agent.as_ref().map(|a| a.phase.age_key())
            {
                self.agent_since = now;
            }
            if word_moved {
                match agent.as_ref().map(|a| &a.phase) {
                    Some(AgentPhase::Question) => self.note(StoryVerb::Question, now),
                    Some(AgentPhase::Wall { .. }) => self.note(StoryVerb::Limited, now),
                    _ => {}
                }
                if matches!(
                    self.agent.as_ref().map(|a| &a.phase),
                    Some(AgentPhase::Wall { .. })
                ) && !matches!(
                    agent.as_ref().map(|a| &a.phase),
                    Some(AgentPhase::Wall { .. })
                ) {
                    self.note(StoryVerb::Resumed, now);
                }
            }
            if self.agent != agent {
                changed = true;
            }
            if self.agent.is_some() && agent.is_none() && self.story_closed < self.story_seq {
                self.story_closed = self.story_seq;
                changed = true;
            }
            self.agent = agent;
        }
        if self.hand != facts.hand {
            self.hand = facts.hand;
            changed = true;
        }
        self.lease_until = facts.lease_until;
        if self.hold != facts.hold {
            match (&self.hold, &facts.hold) {
                (None, Some(_)) if self.baselined => self.note(StoryVerb::Hold, now),
                (Some(_), None) => self.note(StoryVerb::Resumed, now),
                _ => {}
            }
            self.hold = facts.hold;
            changed = true;
        }
        if self.mail != facts.mail {
            if facts.mail.head > self.mail.head {
                self.note(StoryVerb::Mail, now);
            }
            self.mail = facts.mail;
            changed = true;
        }
        if self.link != facts.link {
            self.link = facts.link;
            changed = true;
        }
        if self.input_stall != facts.input_stall {
            self.input_stall = facts.input_stall;
            changed = true;
        }
        let starts = facts.program_turn_starts;
        if self.turn != facts.turn {
            // A carried record settled in a previous process: the baseline,
            // not news (see [`TurnFact::carried`]).
            if let Some(t) = facts.turn
                && !t.carried
                && self.turn.is_none_or(|old| old.id < t.id)
            {
                let noting = self.noting_aterm;
                self.noting_aterm |= t.aterm;
                if t.settled {
                    let opened = t
                        .opened
                        .or_else(|| now.checked_sub(std::time::Duration::from_millis(t.dur_ms)));
                    match self.told_end {
                        Some(told)
                            if told.end == TurnEnd::Done
                                && told.starts == starts
                                && began_in(told.began, opened) =>
                        {
                            let _ = self.pair_end(told, true);
                        }
                        _ => {
                            let news = self.note_as(StoryVerb::Turn, now, true);
                            self.told_end = Some(ToldEnd {
                                end: TurnEnd::Settle,
                                starts,
                                began: opened,
                                seq: self.story_seq,
                                news,
                            });
                        }
                    }
                } else {
                    self.note(StoryVerb::TurnTimedOut, now);
                }
                self.noting_aterm = noting;
            }
            self.turn = facts.turn;
            changed = true;
        }
        if facts.program_turn_ends != self.program_turn_ends {
            if facts.program_turn_ends > self.program_turn_ends && self.baselined {
                let news = !facts.program_turn_watched;
                let began = Some(facts.program_turn_began.unwrap_or(now));
                let news = match self.told_end {
                    Some(told)
                        if told.end == TurnEnd::Settle
                            && told.starts == starts
                            && began_in(began, told.began) =>
                    {
                        let turn_news = self.pair_end(told, news);
                        news && turn_news
                    }
                    _ => {
                        let news = self.note_as(StoryVerb::ProgramDone, now, news);
                        self.told_end = Some(ToldEnd {
                            end: TurnEnd::Done,
                            starts,
                            began,
                            seq: self.story_seq,
                            news,
                        });
                        news
                    }
                };
                self.turn_end_news += u64::from(news);
            }
            self.program_turn_ends = facts.program_turn_ends;
            changed = true;
        }
        let episode = |n: &Option<NeedFact>| n.as_ref().map(|n| n.key);
        if episode(&self.need) != episode(&facts.need) {
            self.need_since = now;
        }
        if self.need != facts.need {
            self.need = facts.need;
            changed = true;
        }
        if self.held_by_person != facts.held_by_person {
            self.held_by_person = facts.held_by_person;
            changed = true;
        }
        self.baselined = true;
        self.noting_aterm = false;
        changed
    }

    /// The need's severity, `None` with no need.
    #[must_use]
    pub fn severity(&self) -> Option<NeedSeverity> {
        self.need.as_ref().map(|n| n.severity)
    }

    /// The tab chip's mark from the published need and the person's hold.
    #[must_use]
    pub const fn chip(&self) -> ChipLevel {
        match &self.need {
            Some(need) => match need.severity {
                NeedSeverity::Stuck => ChipLevel::Stop(need.cause),
                NeedSeverity::Decide => ChipLevel::Wait,
            },
            None if self.held_by_person => ChipLevel::Yours,
            None => ChipLevel::Off,
        }
    }

    /// The severity this slot stands at (`status level=` and `why=`).
    #[must_use]
    pub fn level(&self, watermark: u64) -> Level {
        if self.hold.is_some() {
            return Level::Hold;
        }
        if self.input_stall.is_some() {
            return Level::Limited;
        }
        let at_wall = matches!(
            self.agent.as_ref().map(|a| &a.phase),
            Some(AgentPhase::Wall { .. })
        );
        let handled_wall = at_wall && self.wall_answer != WallAnswer::Nobody;
        if at_wall && !handled_wall {
            return Level::Limited;
        }
        if self.attention.is_some()
            || matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Prompt { .. } | AgentPhase::Question)
            )
        {
            return Level::Attention;
        }
        match &self.hand {
            Hand::DrivenTurn { .. } | Hand::DrivenLease { .. } => return Level::Driven,
            Hand::Driving { .. } => return Level::Driving,
            Hand::None => {}
        }
        if self.unread_wants_a_human() {
            return Level::Attention;
        }
        if self.news_seq > watermark.max(self.story_closed) {
            return Level::Story;
        }
        if self.mail.unread > 0
            || self.mail.queued > 0
            || self.mail.dropped > 0
            || matches!(self.link, Link::Stalled { .. } | Link::Disconnected)
            || handled_wall
        {
            return Level::Note;
        }
        Level::Quiet
    }

    /// Why the slot stands at [`Level::Attention`] (`status why=`).
    #[must_use]
    pub fn why(&self, watermark: u64) -> String {
        if self.level(watermark) != Level::Attention {
            return "-".to_string();
        }
        let phase = self.agent.as_ref().map(|a| &a.phase);
        let mut causes = Vec::new();
        if matches!(phase, Some(AgentPhase::Prompt { .. })) {
            causes.push("prompt");
        }
        if matches!(phase, Some(AgentPhase::Question)) {
            causes.push("question");
        }
        if self.attention.is_some() {
            causes.push("escalation");
        }
        if matches!(self.hand, Hand::None) && self.unread_wants_a_human() {
            causes.push("mail");
        }
        if causes.is_empty() {
            "-".to_string()
        } else {
            causes.join(",")
        }
    }

    /// An unread `task` or `ask` is addressed to someone: a wait state.
    fn unread_wants_a_human(&self) -> bool {
        self.mail.unread > 0
            && self
                .mail
                .last
                .as_ref()
                .is_some_and(|l| l.kind == "task" || l.kind == "ask")
    }

    /// Calm: nothing is happening that the human's next keystroke should not read away.
    #[must_use]
    pub fn calm(&self) -> bool {
        self.hold.is_none()
            && self.input_stall.is_none()
            && matches!(self.hand, Hand::None)
            && self.attention.is_none()
            && !matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Prompt { .. } | AgentPhase::Question | AgentPhase::Wall { .. })
            )
            && !self.unread_wants_a_human()
    }

    /// The story points after `watermark` (and after a closed story), oldest first.
    pub fn story_since(&self, watermark: u64) -> impl Iterator<Item = &StoryPoint> {
        let seen = watermark.max(self.story_closed);
        self.story.iter().filter(move |p| p.seq > seen && !p.aterm)
    }
}

/// Severity, ascending: `hold > limited > attention > driven > story > quiet`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Nothing to say.
    Quiet,
    /// Mail waiting, a queued post, a stalled bridge, a wall the harness answers.
    Note,
    /// Something happened since the human last looked.
    Story,
    /// This session drives another.
    Driving,
    /// A peer's hand is on this keyboard.
    Driven,
    /// A prompt, a question, an escalation, an unread task.
    Attention,
    /// At a wall, or not reading its input.
    Limited,
    /// Held.
    Hold,
}

impl Level {
    /// The wire word `status level=` prints.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Note => "note",
            Self::Story => "story",
            Self::Driving => "driving",
            Self::Driven => "driven",
            Self::Attention => "attention",
            Self::Limited => "limited",
            Self::Hold => "hold",
        }
    }
}

/// The window's border.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rim {
    /// No border.
    #[default]
    None,
    /// A stuck need: the stop tone.
    Stuck,
    /// A decide need and no stuck one.
    Decide,
    /// The front session has the person's hold and nothing in the window needs them.
    Yours,
}

impl Rim {
    /// The wire word `chrome presence rim=` prints.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Stuck => "stuck",
            Self::Decide => "decide",
            Self::Yours => "yours",
        }
    }

    /// The border a need of `severity` lights.
    #[must_use]
    pub const fn of(severity: NeedSeverity) -> Self {
        match severity {
            NeedSeverity::Stuck => Self::Stuck,
            NeedSeverity::Decide => Self::Decide,
        }
    }
}

/// The tab chip's mark, ascending, so a tab's `max` over its panes keeps the most pressing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum ChipLevel {
    /// No mark.
    #[default]
    Off,
    /// A short dash in the label's ink.
    Note,
    /// A dot: a task finished and its result was not yet looked at.
    Done,
    /// The hand: the person has this tab.
    Yours,
    /// A hollow diamond: a decide need.
    Wait,
    /// A filled diamond: a stuck need, and which kind (the hover's remedy).
    Stop(StopCause),
}

/// Why a chip stands at a stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StopCause {
    /// Any other stuck need: a wall, a login, a memory banner, a stalled task.
    Wall,
    /// The program has stopped reading its input.
    Frozen,
    /// The foreground job is stopped with input queued.
    Suspended,
}

impl ChipLevel {
    /// The chrome-state tokens the introspection line prints for this mark.
    #[must_use]
    pub const fn chrome_states(self) -> &'static [&'static str] {
        match self {
            Self::Off => &[],
            Self::Note => &["note"],
            Self::Done => &["done"],
            Self::Yours => &["yours"],
            Self::Wait => &["attention"],
            Self::Stop(_) => &["attention", "stop"],
        }
    }

    /// The hover-help clause.
    #[must_use]
    pub const fn help(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Note => Some("Something to look at: a failed command or a notice"),
            Self::Done => Some("Finished; not yet looked at"),
            Self::Yours => Some("You have this tab"),
            Self::Wait => Some("Needs attention"),
            Self::Stop(StopCause::Frozen) => Some("Frozen, not reading input"),
            Self::Stop(StopCause::Suspended) => Some("Stopped with input queued"),
            Self::Stop(StopCause::Wall) => Some("Stuck: needs you"),
        }
    }
}
