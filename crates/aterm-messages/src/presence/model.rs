// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence FACTS a host gathers, the per-session [`Slot`] that folds
//! them (with its story: what happened since the human last looked), and the
//! severity ([`Level`]) the rim ([`Rim`]) and the tab chip ([`ChipLevel`])
//! are derived from.

use std::borrow::Cow;
use std::collections::VecDeque;

use super::{Host, SETTLED_GLOW, STORY_CAP, TOLD_FLASH};
use crate::Instant;

/// The colour mood of the row — information, a good end, or something the
/// user should look at. The phase slot is painted in it ([`super::paint_row`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tone {
    /// Information.
    #[default]
    Info,
    /// A good end: the two seconds after a settled turn.
    Success,
    /// A human should look.
    Warn,
}

/// What the classifier read off one screen: the worker's phase and the
/// context indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentReading<V: Host> {
    /// The worker's phase.
    pub phase: AgentPhase<V>,
    /// The context left, in percent, when the screen shows it.
    pub context_pct: Option<u8>,
}

/// The worker's phase as the row spells it. `Wall` keeps only the wall's
/// KIND and the RESET time the notice named — never the notice text (the
/// never-shown law).
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
        /// host's clock: the row counts DOWN to it. `None` prints the reset
        /// time alone — never a figure that is not the countdown.
        until: Option<Instant>,
    },
    /// Waiting at its composer.
    Idle,
    /// Idle with the session survey parked above the composer.
    Survey,
    /// An identified agent whose reader has no EVIDENCE for a phase: its
    /// default `idle` is not published as idle, since whatever acts on an
    /// idle worker would act on a guess.
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

    /// The word the row prints: [`Self::word`], except that a wall is the
    /// host's own word for it ([`Host::wall_band`]: `limited` for the walls
    /// the row has always called that, an API error's cause, else the state
    /// the wall leaves the session in), and a parked survey is `idle`: the
    /// worker waits at its composer either way.
    #[must_use]
    pub fn band_word(&self) -> &'static str {
        match self {
            Self::Wall { kind, .. } => V::wall_band(*kind),
            Self::Survey => "idle",
            other => other.word(),
        }
    }

    /// What the row's age counts from: its phase word, a box by the kind the
    /// row names ([`super::prompt_band_word`]), so a box that follows another
    /// starts its own age.
    pub(super) fn age_key(&self) -> Cow<'static, str> {
        match self {
            Self::Prompt { detail } => super::prompt_band_word(detail.as_deref()).into(),
            other => other.band_word().into(),
        }
    }
}

/// The lease a refresh read, reduced to what the row's GEOMETRY hangs on:
/// the host reads it into [`Facts::lease`] from the SAME read that forms the
/// hand, and again, LIVE, for [`super::drive::Desk::lease_mark`]. Words
/// formed under one mark are never committed to a window's geometry under
/// another (2026-09-28, the drive module's `commit_rows`): every change of
/// the lease posts a wake, and that wake re-derives them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LeaseMark {
    /// No live lease on the session, and it drives no peer.
    #[default]
    Free,
    /// A driver may type into it: a `turn` before its end of input, or a
    /// live drive lease.
    Typing,
    /// A `turn` past its end of input, settling.
    Settling,
    /// It drives a peer's `turn` (its band reads `▸ @<sid>`).
    Driving,
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
    },
    /// THIS session drives another — its own open `turn` on a peer whose
    /// lease names it: `sid` is the peer's short form.
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
            Self::DrivenLease { holder } => format!("lease:{}", V::pct_encode(holder)),
            Self::Driving { sid } => format!("driving:{}", V::pct_encode(sid)),
        }
    }
}

/// Whether `hand` is THIS process's own harness (ruling 313): a drive lease
/// under the holder name its supervisor loops claim sessions by
/// ([`Host::is_aterms_holder`]). Another instance's harness, a manager
/// session or an `aterm drive` is somebody else's hand, and stays a fact the
/// window shows.
#[must_use]
pub(crate) fn is_aterms_hand<V: Host>(hand: &Hand) -> bool {
    matches!(hand, Hand::DrivenLease { holder } if V::is_aterms_holder(holder))
}

/// The standing halt, as the row prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldFact {
    /// The wire's (percent-encoded) reason.
    pub reason: String,
    /// `origin=fleet`: cannot be lifted from this window.
    pub fleet: bool,
}

/// The newest inbox row, trust FIRST (the receiver's verdict is the first
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

/// The mail counts the row prints.
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
    /// the stall has a date. `None` for a bridge still dialing since it
    /// attached: there is no stall to date, and the slot prints `~` with no
    /// figure rather than `~ 0s`.
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
    /// The record came from an EARLIER process, carried across a
    /// self-update handoff: a turn that settled before this process existed.
    /// It is the ledger's baseline, never news — the slot adopts it without a
    /// story point or the Success glow (design §4).
    pub carried: bool,
}

/// Everything one refresh reads. Built by the host from the session's own
/// state; pure data so the model is testable without a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts<V: Host> {
    /// `meta role`.
    pub role: Option<String>,
    /// `meta attention` (a typed escalation).
    pub attention: Option<String>,
    /// `attention` is told ELSEWHERE, in full (ruling 270, "one place tells
    /// it"): the level, `status why=`, the tab chip and the rim still read
    /// `attention`; only the row's phase slot does not repeat its words.
    pub attention_told_elsewhere: bool,
    /// The shell status word (`running`, `idle`, `quiet`, …) and when it was
    /// published — the phase the row shows when no agent reading exists.
    pub shell: Option<(&'static str, Instant)>,
    /// The sequence number of the host's agent reading: `agent` is folded in
    /// only when this is NEWER than the one the slot last absorbed; `0` =
    /// never read.
    pub agent_seq: u64,
    /// The agent reading at `agent_seq` — `None` for a session that is not an
    /// identified agent.
    pub agent: Option<AgentReading<V>>,
    /// Whose hand is on the keyboard.
    pub hand: Hand,
    /// When a cooperative drive lease LAPSES: nothing posts a wake for a
    /// lapse, so the host arms this as a deadline and re-reads the hand when
    /// it passes. `None` without such a lease.
    pub lease_until: Option<Instant>,
    /// A driver may still type into the session (the host's lease: a `turn`
    /// before its settle phase, or a live drive lease). Every window showing
    /// the session holds its row count meanwhile ([`super::drive::Desk::
    /// driver_may_type`] reads the LIVE lease for that); here it is a fact so
    /// the wake that lifts it counts as a change and the held row catches up —
    /// and so the words formed from these facts hold their own birth: a lease
    /// let go between this read and the row's commit must not raise a row for
    /// a hand already gone (the drive module's `commit_rows`).
    pub typing: bool,
    /// The lease as this read saw it — the same read as [`Self::hand`] and
    /// [`Self::typing`], never a second lock of it. The row's commit holds
    /// while the host's live mark differs.
    pub lease: LeaseMark,
    /// The standing hold.
    pub hold: Option<HoldFact>,
    /// The mail counts.
    pub mail: MailFacts,
    /// The bridge link.
    pub link: Link,
    /// The last completed turn.
    pub turn: Option<TurnFact>,
    /// The host's published input stall: the program has stopped reading its
    /// input. Ranks [`Level::Limited`] and takes the phase slot as `frozen`
    /// (or `stopped`) — ahead of typed attention.
    pub input_stall: Option<V::Stall>,
}

impl<V: Host> Default for Facts<V> {
    fn default() -> Self {
        Self {
            role: None,
            attention: None,
            attention_told_elsewhere: false,
            shell: None,
            agent_seq: 0,
            agent: None,
            hand: Hand::None,
            lease_until: None,
            typing: false,
            lease: LeaseMark::Free,
            hold: None,
            mail: MailFacts::default(),
            link: Link::Absent,
            turn: None,
            input_stall: None,
        }
    }
}

/// What a story point says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoryVerb {
    /// A turn settled.
    Turn,
    /// A turn whose settle deadline passed (`status=timeout` on the ledger):
    /// counted with the turns, and said — a busy worker with a timed-out turn
    /// never reads `◇ quiet`.
    TurnTimedOut,
    /// Mail arrived.
    Mail,
    /// A hold began.
    Hold,
    /// The agent hit a wall.
    Limited,
    /// The agent asked.
    Question,
    /// The watcher approved a read (`ctl story approved`).
    Approval,
    /// A stop (hold / limited) ended.
    Resumed,
    // The rest of the CLOSED SET `aterm ctl story <verb>` accepts (design §5):
    // what the watcher decided or saw, which reaches the window no other way.
    /// `ctl story dismissed` — the session survey was dismissed for the human.
    Dismissed,
    /// `ctl story reconnected` — the watcher rode out an outage and is back.
    Reconnected,
    /// `ctl story timeout` — the watcher's budget ran out.
    Timeout,
    /// `ctl story exit` — the watcher's loop ended.
    Exit,
    /// `ctl story compacted` — the worker compacted its context.
    Compacted,
    /// `ctl story warned` — the watcher warned (context running low).
    Warned,
    /// `ctl story chose <policy>` — the SUPERVISOR answered the agent's
    /// question dialog by policy, the text naming the policy word. The one
    /// told verb whose teller is the harness, not a watcher, and the one the
    /// window answers with a chime and a pulse, because the point of it is
    /// that the human notices a question was answered without them.
    Chose,
}

impl StoryVerb {
    /// The closed set of words `aterm ctl story <verb>` accepts, in the order
    /// the usage line prints them. A word outside it is a usage error, never a
    /// free-text story point.
    pub const TOLD_WORDS: [&'static str; 8] = [
        "approved",
        "dismissed",
        "reconnected",
        "timeout",
        "exit",
        "compacted",
        "warned",
        "chose",
    ];

    /// The verb a `ctl story <word>` names, `None` outside the closed set.
    #[must_use]
    pub fn parse_told(word: &str) -> Option<Self> {
        Some(match word {
            "approved" => Self::Approval,
            "dismissed" => Self::Dismissed,
            "reconnected" => Self::Reconnected,
            "timeout" => Self::Timeout,
            "exit" => Self::Exit,
            "compacted" => Self::Compacted,
            "warned" => Self::Warned,
            "chose" => Self::Chose,
            _ => return None,
        })
    }

    /// The word the row prints for a TOLD verb (the wire word), with the
    /// glyph the mock pairs it with: `✓ approved`.
    #[must_use]
    pub(crate) const fn told_words(self) -> Option<(char, &'static str)> {
        Some(match self {
            Self::Approval => ('\u{2713}', "approved"),
            Self::Dismissed => ('\u{2713}', "dismissed"),
            Self::Reconnected => ('\u{27df}', "reconnected"),
            Self::Timeout => ('\u{2715}', "timeout"),
            Self::Exit => ('\u{2715}', "exit"),
            Self::Compacted => ('\u{25c7}', "compacted"),
            Self::Warned => ('\u{26a0}', "warned"),
            // A filled diamond: distinct from an approval's check, and the
            // solid twin of the quiet summary's `◇`.
            Self::Chose => ('\u{25c6}', "chose"),
            Self::Turn
            | Self::TurnTimedOut
            | Self::Mail
            | Self::Hold
            | Self::Limited
            | Self::Question
            | Self::Resumed => return None,
        })
    }

    /// WHO acted on a told point, for the spoken sentence (`approved by
    /// watcher`, `chose by harness`, `compacted by worker`): the harness for
    /// [`Self::Chose`], the worker for [`Self::Compacted`] (the watcher only
    /// saw it happen), a watcher for every other told word.
    #[must_use]
    pub const fn teller(self) -> &'static str {
        match self {
            Self::Chose => "harness",
            Self::Compacted => "worker",
            Self::Approval
            | Self::Dismissed
            | Self::Reconnected
            | Self::Timeout
            | Self::Exit
            | Self::Warned
            | Self::Turn
            | Self::TurnTimedOut
            | Self::Mail
            | Self::Hold
            | Self::Limited
            | Self::Question
            | Self::Resumed => "watcher",
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
    /// The point happened under aterm's OWN harness's hand (ruling 313):
    /// kept in the story and counted by `story=`, never news — the row and
    /// the log already tell what aterm did there.
    pub aterm: bool,
}

/// A session's presence, as the last refresh left it.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent fact the fold keeps (told elsewhere, baselined, aterm's hand, noting it); no two of them are one state"
)]
pub struct Slot<V: Host> {
    /// The agent-reading sequence last absorbed ([`Facts::agent_seq`]); `None`
    /// before the first. This only keeps a refresh from re-folding a reading
    /// it already has.
    pub agent_seq_seen: Option<u64>,
    /// [`Facts::role`].
    pub role: Option<String>,
    /// [`Facts::attention`].
    pub attention: Option<String>,
    /// [`Facts::attention_told_elsewhere`].
    pub attention_told_elsewhere: bool,
    /// [`Facts::shell`].
    pub shell: Option<(&'static str, Instant)>,
    /// [`Facts::agent`].
    pub agent: Option<AgentReading<V>>,
    /// When the AGENT phase word last changed — the row's `since` for it.
    pub agent_since: Instant,
    /// [`Facts::hand`].
    pub hand: Hand,
    /// When the cooperative lease behind `hand` lapses (see
    /// [`Facts::lease_until`]); the host's timer re-reads the hand then.
    pub lease_until: Option<Instant>,
    /// [`Facts::typing`].
    pub typing: bool,
    /// [`Facts::lease`].
    pub lease: LeaseMark,
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
    /// When the last turn SETTLED (the Success tone's 2 s window).
    pub settled_at: Option<Instant>,
    /// When a stop (hold or limit) began, for the story's stop duration.
    /// `None` under a hold the slot first saw at its own mint: nothing in a
    /// hold says when it began, so the row prints no figure for it rather
    /// than dating it from the mint.
    stop_began: Option<(StoryVerb, Instant)>,
    /// Whether a refresh has been absorbed yet. The FIRST absorb is the
    /// baseline — what already stood when the slot was minted — and a fact
    /// already standing then (a hold) is adopted as state, not narrated as
    /// something that happened since the human last looked.
    baselined: bool,
    story: VecDeque<StoryPoint>,
    /// The seq of the newest story point; 0 = nothing ever happened.
    pub story_seq: u64,
    /// The seq of the newest point that is NEWS — one that did not happen
    /// under aterm's own hand ([`StoryPoint::aterm`]); what the level reads.
    news_seq: u64,
    /// aterm's OWN harness has its hand on the session (ruling 313): a drive
    /// lease this process's harness holds (`is_aterms_hand`), or a `turn`
    /// typed inside one (an Owner-token turn whose lease hands back to it).
    /// Not a presence fact: the window's own chrome does not show it, and the
    /// story points it makes are not news.
    pub(super) aterm_hand: bool,
    /// Set for the length of one [`Self::absorb`] in which aterm's hand was on
    /// the session at either end: every point it notes is aterm's.
    noting_aterm: bool,
    /// The story up to this seq is CLOSED: the agent it was about left the
    /// session, so it is no longer news to hold the row for (2026-09-24,
    /// D10). Every window's watermark is read as at least this.
    story_closed: u64,
    /// The last TOLD point (`ctl story`), with its text: the phase slot reads
    /// it for [`TOLD_FLASH`] after its instant, then returns to the phase.
    told: Option<(StoryVerb, String, Instant)>,
}

impl<V: Host> Slot<V> {
    /// A slot minted at `now`, before its first refresh.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            agent_seq_seen: None,
            role: None,
            attention: None,
            attention_told_elsewhere: false,
            shell: None,
            agent: None,
            agent_since: now,
            hand: Hand::None,
            lease_until: None,
            typing: false,
            lease: LeaseMark::Free,
            hold: None,
            mail: MailFacts::default(),
            link: Link::Absent,
            turn: None,
            input_stall: None,
            settled_at: None,
            stop_began: None,
            baselined: false,
            story: VecDeque::new(),
            story_seq: 0,
            news_seq: 0,
            aterm_hand: false,
            noting_aterm: false,
            story_closed: 0,
            told: None,
        }
    }

    /// `aterm ctl story <verb> [<text>]` landed: one story point, and the
    /// phase slot reads the verb for [`TOLD_FLASH`]. `text` is the wire text
    /// (already bounded); it is sanitized for cells here like every other
    /// free-text value. Returns the point's seq.
    pub fn tell(&mut self, verb: StoryVerb, text: &str, now: Instant) -> u64 {
        self.note(verb, now);
        self.told = Some((verb, super::sanitize_token(text, 48), now));
        self.story_seq
    }

    /// The told point still standing in the phase slot at `now`.
    pub(super) fn told_now(&self, now: Instant) -> Option<(StoryVerb, &str)> {
        self.told
            .as_ref()
            .filter(|(_, _, at)| now.saturating_duration_since(*at) < TOLD_FLASH)
            .map(|(v, t, _)| (*v, t.as_str()))
    }

    /// When the phase slot returns from a told point to the phase — the one
    /// deadline a story post adds (there is no other clock).
    #[must_use]
    pub fn told_deadline(&self, now: Instant) -> Option<Instant> {
        self.told
            .as_ref()
            .map(|(_, _, at)| *at + TOLD_FLASH)
            .filter(|end| *end > now)
    }

    fn note(&mut self, verb: StoryVerb, now: Instant) {
        self.story_seq += 1;
        let aterm = self.noting_aterm || self.aterm_hand;
        if !aterm {
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
    }

    /// Fold one refresh's facts in. Returns whether anything the view reads
    /// moved.
    #[allow(
        clippy::too_many_lines,
        reason = "one fold per fact, in the order the facts are declared; the story points depend on that order"
    )]
    pub fn absorb(&mut self, facts: Facts<V>, now: Instant) -> bool {
        let mut changed = false;
        // aterm's own hand at either end of this refresh makes every point it
        // notes aterm's (ruling 313): a relaunch's turn settles and its lease
        // is handed back between two refreshes, in either order.
        let was_aterm = self.aterm_hand;
        let now_aterm = is_aterms_hand::<V>(&facts.hand)
            || (was_aterm && matches!(facts.hand, Hand::DrivenTurn { holder: None, .. }));
        self.noting_aterm = was_aterm || now_aterm;
        if self.aterm_hand != now_aterm {
            self.aterm_hand = now_aterm;
            changed = true;
        }
        if self.role != facts.role {
            self.role = facts.role;
            changed = true;
        }
        if self.attention != facts.attention {
            self.attention = facts.attention;
            changed = true;
        }
        if self.attention_told_elsewhere != facts.attention_told_elsewhere {
            self.attention_told_elsewhere = facts.attention_told_elsewhere;
            changed = true;
        }
        if self.shell.map(|s| s.0) != facts.shell.map(|s| s.0) {
            changed = true;
        }
        self.shell = facts.shell;
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
                    Some(AgentPhase::Wall { .. }) => {
                        self.note(StoryVerb::Limited, now);
                        self.stop_began = Some((StoryVerb::Limited, now));
                    }
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
            // The agent LEFT (a verdict, then none): what it did while nobody
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
        if self.typing != facts.typing {
            self.typing = facts.typing;
            changed = true;
        }
        if self.lease != facts.lease {
            self.lease = facts.lease;
            changed = true;
        }
        // A renewed lease moves its deadline without moving a word.
        self.lease_until = facts.lease_until;
        if self.hold != facts.hold {
            match (&self.hold, &facts.hold) {
                (None, Some(_)) if self.baselined => {
                    self.note(StoryVerb::Hold, now);
                    self.stop_began = Some((StoryVerb::Hold, now));
                }
                // Standing at the mint: the hold is state, its age unknown.
                (None, Some(_)) => self.stop_began = None,
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
        if self.turn != facts.turn {
            // A CARRIED record settled in a previous process: the baseline,
            // not news (see [`TurnFact::carried`]).
            if let Some(t) = facts.turn
                && !t.carried
                && self.turn.is_none_or(|old| old.id < t.id)
            {
                if t.settled {
                    self.note(StoryVerb::Turn, now);
                    self.settled_at = Some(now);
                } else {
                    self.note(StoryVerb::TurnTimedOut, now);
                }
            }
            self.turn = facts.turn;
            changed = true;
        }
        self.baselined = true;
        self.noting_aterm = false;
        changed
    }

    /// The tab chip's mark: a hollow diamond to wait, a filled one at a stop
    /// (with why it stopped, which the hover names), a dot for a story.
    #[must_use]
    pub fn chip(&self, watermark: u64) -> ChipLevel {
        match self.level(watermark) {
            Level::Quiet | Level::Note | Level::Driving | Level::Driven => ChipLevel::Off,
            Level::Story => ChipLevel::Story,
            Level::Attention => ChipLevel::Wait,
            Level::Hold => ChipLevel::Stop(StopCause::Hold),
            // `level` ranks a stall ahead of a wall, and so does this.
            Level::Limited => ChipLevel::Stop(match &self.input_stall {
                Some(stall) if V::stall_stopped(stall) => StopCause::Suspended,
                Some(_) => StopCause::Frozen,
                None => StopCause::Wall,
            }),
        }
    }

    /// The severity this slot stands at (`status level=`, `why=` and the
    /// tab chip follow it).
    #[must_use]
    pub fn level(&self, watermark: u64) -> Level {
        self.level_counting(watermark, true, true)
    }

    /// The level the window's OWN chrome shows — the rim and the row's
    /// existence: [`Self::level`], except that an attention another place
    /// already tells ([`Facts::attention_told_elsewhere`]) is not counted,
    /// and neither is aterm's own hand (ruling 313). A colour with no words
    /// is what [`Level::rim`]'s own rule forbids; the fact stays where it is
    /// read on purpose: `status level=attention why=escalation` and the tab's
    /// wait mark.
    #[must_use]
    pub fn shown_level(&self, watermark: u64) -> Level {
        self.level_counting(watermark, !self.attention_told_elsewhere, !self.aterm_hand)
    }

    /// `attention`: an escalation counts; `hand`: a hand counts even when it
    /// is aterm's own (ruling 313 — `status level=` keeps the fact, the
    /// window's chrome does not show it).
    fn level_counting(&self, watermark: u64, attention: bool, hand: bool) -> Level {
        if self.hold.is_some() {
            return Level::Hold;
        }
        // A program that reads nothing is stopped as surely as one at a
        // wall, whatever its screen still shows (2026-09-24).
        if self.input_stall.is_some() {
            return Level::Limited;
        }
        if matches!(
            self.agent.as_ref().map(|a| &a.phase),
            Some(AgentPhase::Wall { .. })
        ) {
            return Level::Limited;
        }
        if (attention && self.attention.is_some())
            || matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Prompt { .. } | AgentPhase::Question)
            )
        {
            return Level::Attention;
        }
        match &self.hand {
            Hand::DrivenTurn { .. } | Hand::DrivenLease { .. } if hand => return Level::Driven,
            Hand::Driving { .. } => return Level::Driving,
            _ => {}
        }
        // An unread task or ask is a wait state only while no hand is on the
        // session: a driven worker with mail waiting stays teal (the mock's
        // first row), and the mail slot says the rest.
        if self.unread_wants_a_human() {
            return Level::Attention;
        }
        // A story outranks waiting mail: "something happened since you
        // looked" is the glance fact, and the mail slot prints the mail.
        if self.news_seq > watermark.max(self.story_closed) {
            return Level::Story;
        }
        if self.mail.unread > 0
            || self.mail.queued > 0
            || self.mail.dropped > 0
            || matches!(self.link, Link::Stalled { .. } | Link::Disconnected)
        {
            return Level::Note;
        }
        Level::Quiet
    }

    /// WHY the slot stands at [`Level::Attention`] (`status why=`): the causes
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

    /// CALM: nothing is happening that the human's next keystroke should not
    /// fold away — the fold law's second conjunct.
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

    /// The tone the row paints in: Warn while a human should look, Success for
    /// `SETTLED_GLOW` (2 s) after a settled turn, else Info. It follows the level
    /// the row is SHOWN at ([`Self::shown_level`], ruling 280).
    #[must_use]
    pub fn tone(&self, now: Instant, watermark: u64) -> Tone {
        if self.shown_level(watermark) >= Level::Attention {
            return Tone::Warn;
        }
        if self
            .settled_at
            .is_some_and(|t| now.saturating_duration_since(t) < SETTLED_GLOW)
        {
            return Tone::Success;
        }
        Tone::Info
    }

    /// The story points after `watermark` (and after a closed story), oldest
    /// first.
    pub fn story_since(&self, watermark: u64) -> impl Iterator<Item = &StoryPoint> {
        let seen = watermark.max(self.story_closed);
        self.story.iter().filter(move |p| p.seq > seen && !p.aterm)
    }

    /// The current stop (hold / limit) in progress, for the story's summary.
    pub(super) fn stop_in_progress(&self) -> Option<(StoryVerb, Instant)> {
        self.stop_began.filter(|(verb, _)| match verb {
            StoryVerb::Hold => self.hold.is_some(),
            _ => matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Wall { .. })
            ),
        })
    }
}

/// Severity, ascending. `hold > limited > attention > driven > story > quiet`
/// (design §1), with the rim-less states between: mail waiting (`Note`)
/// sits under a story, and driving another session under being driven.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Nothing to show.
    Quiet,
    /// Mail waiting, a queued post, a stalled bridge: information, no rim.
    Note,
    /// Something happened since the human last looked; the session is calm.
    Story,
    /// This session drives another.
    Driving,
    /// A peer's hand is on this keyboard.
    Driven,
    /// A human should look: prompt, question, escalation, an unread task.
    Attention,
    /// At a wall, or not reading its input.
    Limited,
    /// Held.
    Hold,
}

impl Level {
    /// The rim this level paints — colour only, and only for the levels whose
    /// row sentence says why.
    #[must_use]
    pub const fn rim(self) -> Rim {
        match self {
            Self::Quiet | Self::Story | Self::Note | Self::Driving => Rim::None,
            Self::Driven => Rim::Drive,
            Self::Attention => Rim::Wait,
            Self::Limited => Rim::Stop { hold: false },
            Self::Hold => Rim::Stop { hold: true },
        }
    }

    /// Whether the row EXISTS for this level (the fold law's first half:
    /// state ≠ quiet). `Story` counts — it is the row the summary lives in.
    #[must_use]
    pub const fn shows_row(self) -> bool {
        !matches!(self, Self::Quiet)
    }

    /// The wire word `status level=` and `chrome` print: the variant's name in
    /// lower case, a closed set a reader can match on.
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

/// The rim's colour state. `Stop { hold }` doubles the thickness and adds the
/// wash under a hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rim {
    /// No rim.
    #[default]
    None,
    /// A peer's hand (teal).
    Drive,
    /// A human should look (amber).
    Wait,
    /// Stopped (red); doubled with a wash under a hold.
    Stop {
        /// Under a hold.
        hold: bool,
    },
}

impl Rim {
    /// The wire word `chrome` prints for the rim.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Drive => "drive",
            Self::Wait => "wait",
            Self::Stop { hold: false } => "stop",
            Self::Stop { hold: true } => "stop-hold",
        }
    }
}

/// The tab chip's attention mark as a LEVEL (the chip's old `attention: bool`
/// is `Wait`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum ChipLevel {
    /// No mark.
    #[default]
    Off,
    /// A dot: something happened while the human was away.
    Story,
    /// A hollow diamond: a human should look.
    Wait,
    /// A filled diamond: stopped, and why.
    Stop(StopCause),
}

/// Why a chip stands at a stop: what decides the remedy (lift a hold, restart
/// or resume the program, see the agent's wall). Ascending as
/// [`Slot::level`] ranks them, so a tab's `max` over its panes keeps a hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StopCause {
    /// An agent at a wall ([`AgentPhase::Wall`]).
    Wall,
    /// The program has stopped reading its input.
    Frozen,
    /// The foreground job is stopped with input queued.
    Suspended,
    /// A hold.
    Hold,
}

impl ChipLevel {
    /// The chrome-state tokens the introspection line prints for this mark.
    #[must_use]
    pub const fn chrome_states(self) -> &'static [&'static str] {
        match self {
            Self::Off => &[],
            Self::Story => &["story"],
            Self::Wait => &["attention"],
            Self::Stop(_) => &["attention", "stop"],
        }
    }

    /// The hover-help clause.
    #[must_use]
    pub const fn help(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Story => Some("Something happened while you were away"),
            Self::Wait => Some("Needs attention"),
            Self::Stop(StopCause::Hold) => Some("Held"),
            Self::Stop(StopCause::Frozen) => Some("Frozen, not reading input"),
            Self::Stop(StopCause::Suspended) => Some("Stopped with input queued"),
            Self::Stop(StopCause::Wall) => Some("Agent can't continue"),
        }
    }
}
