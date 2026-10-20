// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence facts a host gathers, the per-session [`Slot`] that folds
//! them (with its story: what happened since the human last looked, which
//! `status level=` and `story=` count), the wire severity ([`Level`]), and
//! the two marks the window draws from the host's PUBLISHED need verdict:
//! the border ([`Rim`]) and the tab chip ([`ChipLevel`]). Nothing here
//! classifies a need: [`Facts::need`] is the host's one verdict, read.

use std::borrow::Cow;
use std::collections::VecDeque;

use super::{Host, STORY_CAP};
use crate::Instant;

/// What the classifier read off one screen: the worker's phase and the
/// context indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentReading<V: Host> {
    /// The worker's phase.
    pub phase: AgentPhase<V>,
    /// The context left, in percent, when the screen shows it.
    pub context_pct: Option<u8>,
}

/// The worker's phase as the wire spells it. `Wall` keeps only the wall's
/// kind and the reset time the notice named, never the notice text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentPhase<V: Host> {
    /// Working.
    Busy,
    /// An approval box is showing; `detail` is the box's kind and the
    /// classifier's verdict on the command (`bash safe`), never the command.
    Prompt {
        /// `kind[:verdict]`.
        detail: Option<String>,
    },
    /// The agent asked a question.
    Question,
    /// The last turn ended on a wall: a usage window, a model bucket, spend,
    /// a full context, a lost login, an API error, an overload. The worker
    /// sits at an idle composer and will not move on its own (an overload or
    /// a retryable API error only after a retry).
    Wall {
        /// The wall's kind ([`Host::Wall`]).
        kind: V::Wall,
        /// The reset time the notice named, sanitized.
        reset: Option<String>,
        /// When the reset falls, if the notice's time could be placed on the
        /// host's clock.
        until: Option<Instant>,
    },
    /// Waiting at its composer.
    Idle,
    /// Idle with the session survey parked above the composer.
    Survey,
    /// An identified agent whose reader has no evidence for a phase: its
    /// default `idle` is not published, since whatever acts on an idle worker
    /// would act on a guess.
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

    /// The word a person reads ([`super::state_line`]): [`Self::word`],
    /// except that a wall is the host's word for it ([`Host::wall_band`]) and
    /// a parked survey is `idle`, since the worker waits at its composer
    /// either way.
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
    /// A peer's `turn` is open. `holder` names the driver the lease itself
    /// records — its `meta role` if it is a local session with one, else its
    /// short sid — and is `None` for a turn driven over the Owner token.
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
        /// The kernel's pid of the process that took the lease (the peer of
        /// the connection the acquire came on), `None` where the host could
        /// not attest one. The name is the taker's own spelling; this is not.
        pid: Option<u32>,
    },
    /// This session drives another: its own open `turn` on a peer whose
    /// lease names it; `sid` is the peer's short form.
    Driving {
        /// The driven peer's short sid.
        sid: String,
    },
}

impl Hand {
    /// The `status hand=` token: `-` | `turn:<id>[:<holder>]` | `lease:<holder>`
    /// | `driving:<sid>`, the free-text part percent-encoded
    /// ([`Host::pct_encode`]) so the record stays one line of `key=value`
    /// words whatever a holder is called.
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

    /// The `status hand_pid=` token: the kernel-attested pid of a drive
    /// lease's taker ([`Self::DrivenLease`]), `-` for anything else (no hand,
    /// a `turn`, a lease whose taker no kernel named).
    #[must_use]
    pub fn pid_wire(&self) -> String {
        match self {
            Self::DrivenLease { pid: Some(pid), .. } => pid.to_string(),
            _ => "-".to_string(),
        }
    }
}

/// Whether `hand` is this process's own harness: a drive lease under the
/// holder name its supervisor loops claim sessions by, TAKEN BY this process
/// as the kernel attests it ([`Host::is_aterms_holder`]). Another instance's
/// harness, a manager session, an `aterm drive`, or a client that spelled
/// aterm's holder name itself is somebody else's hand.
#[must_use]
pub(crate) fn is_aterms_hand<V: Host>(hand: &Hand) -> bool {
    matches!(hand, Hand::DrivenLease { holder, pid } if V::is_aterms_holder(holder, *pid))
}

/// The standing halt, as the wire reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldFact {
    /// The wire's (percent-encoded) reason.
    pub reason: String,
    /// `origin=fleet` (the bridge's): the host's menus say it cannot be
    /// lifted from this window. Presence reads only that a hold stands.
    pub fleet: bool,
}

/// The newest inbox row, trust first (the receiver's verdict is the first
/// thing a reader sees).
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
    /// The highest row id ever delivered — how the story counts arrivals.
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
    /// A bridge is attached but its broker link is down — for `age_ms`, when
    /// the stall has a date.
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
    /// The record came from an earlier process across a self-update handoff:
    /// the ledger's baseline, never news, so the slot adopts it without a
    /// story point.
    pub carried: bool,
    /// THIS PROCESS'S harness typed it (the ledger's `by=harness`): aterm's
    /// own turn, whose story point is aterm's (ruling 313) whether or not a
    /// refresh saw the hand it was typed under — the turn-end loop's
    /// continuation holds the hand only from its text to its Enter.
    pub aterm: bool,
    /// When the turn opened (the ledger's own `started_ms`, on the host's
    /// clock, never later than it), `None` where the host cannot say (a
    /// carried record's clock was another process's). What a program's turn
    /// must have begun no earlier than to be this turn
    /// ([`Facts::program_turn_began`]); unknown, it is read as the settle less
    /// its `dur_ms`.
    pub opened: Option<Instant>,
}

/// Who answers the wall the agent is at: a host fact (aterm's own harness
/// holds the session and has a plan for the wall) the screen cannot show. A
/// wall the harness resumes by itself is a record (`level=note`), not a stop.
/// The need classifier reads it too (`aterm-gui`'s `needs::NeedFacts`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WallAnswer {
    /// Nobody: no supervisor, the owner's switch off, a lost login, a
    /// critical-memory banner. The wall stands for a person to see.
    #[default]
    Nobody,
    /// The harness resumes it by itself (or Claude Code does, with the harness
    /// the net under it).
    Harness,
    /// The harness resumes it slowly and a person can lift it faster: a
    /// reset-less model bucket, a spend limit no reset ends. The payload is
    /// the one thing to do, a static phrase (`/model to switch`, `add
    /// credits`).
    HarnessAsks(&'static str),
}

/// How a need presses: the two severities of the host's need words
/// (`aterm_types::needs::Severity`, which this wasm-clean crate does not
/// link; the host maps its word onto these).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NeedSeverity {
    /// Nothing moves until the person acts: the stop tone.
    Stuck,
    /// The person decides: the wait tone.
    Decide,
}

/// THE PUBLISHED NEED, as the presence engine reads it: what the host's one
/// classifier (`aterm-gui`'s `needs::of`, published on the session's
/// timeline) says the session needs of its owner now. The engine never
/// re-classifies: the border, the pill and the tab mark draw this value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedFact {
    /// The need word's severity: it picks the border and the mark.
    pub severity: NeedSeverity,
    /// What a stuck need's remedy turns on (the host maps `frozen` to
    /// [`StopCause::Frozen`], `stopped` to [`StopCause::Suspended`], every
    /// other word to [`StopCause::Wall`]); [`StopCause::Wall`] for a decide
    /// need.
    pub cause: StopCause,
    /// The word and the transition's identity within it, as one key: a need
    /// whose key stands is the same need, its age unmoved
    /// ([`Slot::need_since`]).
    pub key: (u8, u64),
    /// Its words, one clipped line, without the tab's label.
    pub what: String,
}

/// Everything one refresh reads. Built by the host from the session's own
/// state; pure data so the model is testable without a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts<V: Host> {
    /// `meta attention` (a typed escalation): `status level=` and `why=`.
    pub attention: Option<String>,
    /// The shell status word (`running`, `idle`, `quiet`, …) and when it was
    /// published — the state the hover line names when no agent reading
    /// exists.
    pub shell: Option<(&'static str, Instant)>,
    /// The program's own state report (OSC 7501) the host honours now, as
    /// the hover's state line names it (`codex working`), and since when:
    /// named when no agent reading names a phase, ahead of the shell's word.
    /// A claim for display only; nothing decides on it.
    pub program: Option<(&'static str, Instant)>,
    /// The sequence number of the host's agent reading: `agent` is folded in
    /// only when this is newer than the one the slot last absorbed; `0` =
    /// never read.
    pub agent_seq: u64,
    /// The agent reading at `agent_seq` — `None` for a session that is not an
    /// identified agent.
    pub agent: Option<AgentReading<V>>,
    /// Whose hand is on the keyboard.
    pub hand: Hand,
    /// When a cooperative drive lease lapses: nothing posts a wake for a
    /// lapse, so the host arms this as a deadline and re-reads the hand when
    /// it passes. `None` without such a lease.
    pub lease_until: Option<Instant>,
    /// The standing hold (`status level=hold`).
    pub hold: Option<HoldFact>,
    /// The mail counts.
    pub mail: MailFacts,
    /// The bridge link.
    pub link: Link,
    /// The last completed turn.
    pub turn: Option<TurnFact>,
    /// The host's published input stall: the program has stopped reading its
    /// input (`status level=limited`).
    pub input_stall: Option<V::Stall>,
    /// Who answers the agent's wall, if it is at one ([`WallAnswer`]).
    pub wall_answer: WallAnswer,
    /// THE PUBLISHED NEED ([`NeedFact`]): what the border, the pill and the
    /// tab mark draw. `None`: the session needs nothing of its owner.
    pub need: Option<NeedFact>,
    /// The person's own hold stands on the session (the published
    /// `hold_by=person`): the "yours" outline and mark. A hold another origin
    /// set (the Owner token's, the bridge's) is no mark of yours.
    pub held_by_person: bool,
    /// How many times the program in front has reported its own turn ending
    /// (OSC 7501 `working` → `done`|`idle`, the host's gate counting what it
    /// honoured). Each new one is a story point ([`StoryVerb::ProgramDone`]):
    /// `status story=` and `level=story` say a background codex finished,
    /// and one that is news counts in [`Slot::turn_end_news`], which the
    /// host's `•` reads. Display only; nothing decides on it.
    pub program_turn_ends: u64,
    /// The session is in front of a window the person has focused, so a turn
    /// end it reports ([`Self::program_turn_ends`]) happens in plain sight: it
    /// joins the story without being news.
    pub program_turn_watched: bool,
    /// How many turns the program in front has begun by its own report (OSC
    /// 7501 `working` after no turn in progress, as the host's gate showed
    /// it). What tells one driven turn's two ends apart from two turns: a
    /// [`Self::program_turn_ends`] and a ledger settle ([`Self::turn`]) read
    /// at the same count are the same turn, whichever refresh reads which.
    pub program_turn_starts: u64,
    /// When the host's gate counted the program's latest turn start
    /// ([`Self::program_turn_starts`]), `None` before any. A program turn end
    /// and a ledger settle are one turn only when that turn began no earlier
    /// than the ledger turn opened ([`TurnFact::opened`]): the person's own
    /// turn, begun before a driven one was typed, is not the driven turn,
    /// whichever end is read first and whether or not one refresh reads both.
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
    /// Which end it was.
    end: TurnEnd,
    /// The program's turn-start count it was read at
    /// ([`Facts::program_turn_starts`]).
    starts: u64,
    /// When its turn began: a settle's, when the ledger turn opened
    /// ([`TurnFact::opened`]); a program end's, when the program's turn began
    /// ([`Facts::program_turn_began`], else the end itself, the latest it can
    /// have begun). The two pair only when the program's began no earlier
    /// than the ledger's ([`began_in`]).
    began: Option<Instant>,
    /// The story point it was told as, and whether that point was news.
    seq: u64,
    news: bool,
}

/// Whether a program turn that began at `program` is the ledger turn that
/// opened at `ledger`: it began no earlier ([`ToldEnd::began`]). Either
/// unknown pairs by the turn-start count alone.
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
    /// The program in front reported its own turn ended (OSC 7501 `working`
    /// → `done`|`idle`): counted with the turns, and never told beside the
    /// ledger settle of the same turn, whichever refresh reads which first
    /// ([`Facts::program_turn_starts`]).
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
    /// `ctl story chose <policy>`: the supervisor answered the agent's
    /// question dialog by policy. The one told verb a process sends (the
    /// harness's approval loop): a record (`status story=`), never a sound
    /// or a flash.
    Chose,
}

impl StoryVerb {
    /// The closed set of words `aterm ctl story <verb>` accepts. A word
    /// outside it is a usage error, never a free-text story point. The seven
    /// words no process sent went with the presence row that showed them
    /// (owner surfaces, 2026-10-16 §7).
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
    /// The point happened under aterm's own harness's hand: kept in the story
    /// and counted by `story=`, never news.
    pub aterm: bool,
}

/// A session's presence, as the last refresh left it.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent facts of one slot: its baseline, aterm's hand now and at either end of an absorb, and the person's hold; no two are states of one thing"
)]
pub struct Slot<V: Host> {
    /// The agent-reading sequence last absorbed ([`Facts::agent_seq`]); `None`
    /// before the first. This only keeps a refresh from re-folding a reading
    /// it already has.
    pub agent_seq_seen: Option<u64>,
    /// [`Facts::attention`].
    pub attention: Option<String>,
    /// [`Facts::shell`].
    pub shell: Option<(&'static str, Instant)>,
    /// [`Facts::program`].
    pub program: Option<(&'static str, Instant)>,
    /// [`Facts::agent`].
    pub agent: Option<AgentReading<V>>,
    /// When the agent phase word last changed: the hover line's age for it.
    pub agent_since: Instant,
    /// [`Facts::hand`].
    pub hand: Hand,
    /// When the cooperative lease behind `hand` lapses (see
    /// [`Facts::lease_until`]); the host's timer re-reads the hand then.
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
    /// When the standing need began: the absorb that first saw its word and
    /// key. A need re-read with the same word and key keeps it; another
    /// episode starts its own. What the pill's age counts from.
    pub need_since: Instant,
    /// [`Facts::held_by_person`].
    pub held_by_person: bool,
    /// [`Facts::program_turn_ends`], as the last refresh read it.
    pub program_turn_ends: u64,
    /// How many of the program's own turn ends were NEWS: reported while
    /// nobody watched the session ([`Facts::program_turn_watched`]) and not
    /// under aterm's own hand, a driven turn's end told once with its ledger
    /// settle. Only ever grows, from 0 at the mint (the count standing at the
    /// baseline is no news). The host's tab `•` reads it against the count
    /// its looking rule last saw, as it reads a finished task's done rev: a
    /// background codex that finished its turn is what finished.
    pub turn_end_news: u64,
    /// The one turn end told and not yet paired: a ledger settle told as
    /// [`StoryVerb::Turn`] (`Settle`) or a program turn end told as
    /// [`StoryVerb::ProgramDone`] (`Done`), with the program's turn-start
    /// count it was read at ([`Facts::program_turn_starts`]). The other end of
    /// the same turn, read at the same count, is that turn already told.
    told_end: Option<ToldEnd>,
    /// Whether a refresh has been absorbed yet. The first absorb is the
    /// baseline: a fact already standing when the slot was minted (a hold) is
    /// adopted as state, not narrated as news.
    baselined: bool,
    story: VecDeque<StoryPoint>,
    /// The seq of the newest story point; 0 = nothing ever happened.
    pub story_seq: u64,
    /// The seq of the newest point that is news (one that did not happen
    /// under aterm's own hand, [`StoryPoint::aterm`]); what the level reads.
    news_seq: u64,
    /// aterm's own harness has its hand on the session: a drive lease this
    /// process's harness holds (`is_aterms_hand`), or a `turn` typed inside
    /// one (an Owner-token turn whose lease hands back to it). Its story
    /// points are not news.
    pub(super) aterm_hand: bool,
    /// Set for the length of one [`Self::absorb`] in which aterm's hand was on
    /// the session at either end: every point it notes is aterm's.
    noting_aterm: bool,
    /// The story up to this seq is closed: the agent it was about left the
    /// session, so it is no news. Every window's watermark is read as at
    /// least this.
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

    /// `aterm ctl story <verb> [<text>]` landed: one story point (`status
    /// story=`). The text is the wire's and reaches no surface. Returns the
    /// point's seq.
    pub fn tell(&mut self, verb: StoryVerb, now: Instant) -> u64 {
        self.note(verb, now);
        self.story_seq
    }

    fn note(&mut self, verb: StoryVerb, now: Instant) {
        let _ = self.note_as(verb, now, true);
    }

    /// Note a point; `news` false keeps it out of the news a level reads (the
    /// person saw it happen), as aterm's own points are. Returns whether the
    /// point is news.
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
    /// either end would have been, whichever was read first. A point already
    /// news stays it; a retained non-owned point told quietly becomes news at its
    /// own place. An owned point stays omitted, and a retired point cannot carry news.
    /// Returns whether the turn is news now.
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

    /// Fold one refresh's facts in. Returns whether anything a reader reads
    /// moved.
    #[allow(
        clippy::too_many_lines,
        reason = "one fold per fact, in the order the facts are declared; the story points depend on that order"
    )]
    pub fn absorb(&mut self, facts: Facts<V>, now: Instant) -> bool {
        let mut changed = false;
        // aterm's own hand at either end of this refresh makes every point it
        // notes aterm's: a relaunch's turn settles and its lease is handed
        // back between two refreshes, in either order.
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
            // The agent left (a verdict, then none): what it did while nobody
            // looked is closed, not left standing on the shell it returned to.
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
        // A renewed lease moves its deadline without moving a word.
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
        // ONE DRIVEN TURN, TOLD ONCE. A driven codex turn ends twice: the
        // ledger's settle (`turn`) and the program's own report (OSC 7501
        // `working` → `done`, `program_turn_ends`), read by different sweeps in
        // either order. The two pair when no turn of the program's began
        // between them (the same `program_turn_starts`) and the program's turn
        // began no earlier than the ledger turn opened (`began_in`, in both
        // orders): an end of the person's own turn, begun before the driven
        // one was typed, is not the driven turn's, whether it is read before
        // the settle, after it, or in the same refresh. The second is that
        // turn already told, and makes it news if it would have been
        // (`pair_end`), so the order of the reads changes neither the count
        // nor the level. Only a SETTLE pairs: a timed-out or hung-up turn is
        // its own story. Each end pairs at most once.
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
                            // The program's own end already told this turn
                            // (and counted it where it was news).
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
        // The program's own turn end (OSC 7501): a story point (`status
        // story=`), adopted silently at the baseline, and not told beside the
        // ledger settle of the same turn.
        if facts.program_turn_ends != self.program_turn_ends {
            if facts.program_turn_ends > self.program_turn_ends && self.baselined {
                let news = !facts.program_turn_watched;
                // Unknown, the turn began no later than its end, read now.
                let began = Some(facts.program_turn_began.unwrap_or(now));
                let news = match self.told_end {
                    Some(told)
                        if told.end == TurnEnd::Settle
                            && told.starts == starts
                            && began_in(began, told.began) =>
                    {
                        // The ledger's settle already told this turn: the
                        // program's end is news where nobody watched it and
                        // the turn is news (not aterm's own).
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
        // The need: a new word or key is a new episode, its age from now; the
        // same need re-read keeps its age whatever its words now say.
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

    /// The tab chip's mark from the published need and the person's hold: a
    /// filled diamond for a stuck need, a hollow one for a decide need, the
    /// hand for the person's own hold, else none. A supervised box is no
    /// need, so it makes no mark. The finished dot — a task's done rev, or a
    /// program's own turn end ([`Self::turn_end_news`]), not yet looked at —
    /// is the host's ([`ChipLevel::Done`]), folded under these.
    #[must_use]
    pub fn chip(&self) -> ChipLevel {
        match &self.need {
            Some(need) => match need.severity {
                NeedSeverity::Stuck => ChipLevel::Stop(need.cause),
                NeedSeverity::Decide => ChipLevel::Wait,
            },
            None if self.held_by_person => ChipLevel::Yours,
            None => ChipLevel::Off,
        }
    }

    /// The severity this slot stands at (`status level=` and `why=`): a
    /// record, unchanged by the owner surfaces. The border reads the need.
    #[must_use]
    pub fn level(&self, watermark: u64) -> Level {
        if self.hold.is_some() {
            return Level::Hold;
        }
        // A program that reads nothing is stopped as surely as one at a
        // wall, whatever its screen still shows.
        if self.input_stall.is_some() {
            return Level::Limited;
        }
        // A wall the harness answers is a record, not a stop (`WallAnswer`).
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
        // An unread task or ask is a wait state only while no hand is on the
        // session.
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

    /// Why the slot stands at [`Level::Attention`] (`status why=`): the causes
    /// the level merges, comma-joined in a fixed order — `prompt` (an approval
    /// box), `question` (the agent asked), `escalation` (a typed `meta
    /// attention`), `mail` (an unread task/ask with no hand on the session).
    /// `-` at any other level.
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

    /// Calm: nothing is happening that the human's next keystroke should not
    /// read away (the story read's guard, [`super::drive::human_acted_on`]).
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

    /// The story points after `watermark` (and after a closed story), oldest
    /// first.
    pub fn story_since(&self, watermark: u64) -> impl Iterator<Item = &StoryPoint> {
        let seen = watermark.max(self.story_closed);
        self.story.iter().filter(move |p| p.seq > seen && !p.aterm)
    }
}

/// Severity, ascending: `hold > limited > attention > driven > story >
/// quiet`, with the quieter states between: mail waiting (`Note`) sits under
/// a story, and driving another session under being driven. The wire's word
/// (`status level=`), a record: the border reads the published need.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Nothing to say.
    Quiet,
    /// Mail waiting, a queued post, a stalled bridge, a wall the harness
    /// answers.
    Note,
    /// Something happened since the human last looked; the session is calm.
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
    /// The wire word `status level=` prints: the variant's name in lower
    /// case, a closed set a reader can match on.
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

/// THE WINDOW'S BORDER (§5): what the window's most pressing session needs
/// of its owner, or the person's own hold on its front session. Steady:
/// nothing pulses, nothing washes the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rim {
    /// No border.
    #[default]
    None,
    /// A stuck need: the stop tone.
    Stuck,
    /// A decide need and no stuck one: the wait tone.
    Decide,
    /// The front session has the person's hold and nothing in the window
    /// needs them: a 1 px outline in the theme's dim ink.
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

/// The tab chip's mark, ascending, so a tab's `max` over its panes keeps the
/// most pressing: none, the tab's own news (a failed command, a notice), a
/// finished task not yet looked at (`•`), the person's own hold (`✋`), a
/// decide need (`◇`), a stuck need (`◆`). Only the last two are need marks,
/// and only the published verdict makes them ([`Slot::chip`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum ChipLevel {
    /// No mark.
    #[default]
    Off,
    /// A short dash in the label's ink: the tab's own indicator (its last
    /// command failed and that was not yet seen, or a native leaf's notice).
    /// A record, never a need: no presence tone, no border, no pill.
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

/// Why a chip stands at a stop: what decides its remedy (restart or resume
/// the program, see what holds it). The host reads it off the need's word
/// ([`NeedFact::cause`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StopCause {
    /// Any other stuck need: a wall, a login, a memory banner, a stalled
    /// task.
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
