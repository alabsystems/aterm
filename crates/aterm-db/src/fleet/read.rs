// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The records the fleet tables are offered: what one fleet read saw of each machine and each
//! agent on it ([`Read`], made by the fleet glance's reader from the survey it already makes),
//! and what the folder keeps of the reads so far ([`Latest`]).
//!
//! A record holds words and numbers. An account travels as the word its session row named
//! ([`AccountWord`]), which the fold resolves to a registry key and then drops: no row and no
//! debug print carries it. A machine that answered under an address is known by a digest of
//! what the book dials ([`MachineSeen::peer`]), never by the address.
//!
//! Absent is not zero: an observed value is an [`At`], a value with the time it was read, and
//! every observed field is an `Option<At<T>>`. [`At`] has no `Default`, so no code can make one
//! without a reading in hand.

use aterm_digest::Sha256;
use aterm_types::machine_name::Number;
use std::collections::BTreeMap;

/// The glance and task words a record holds, which [`crate::words`] defines on every platform.
pub use crate::words::{State, Task};

/// A value and when it was read, Unix milliseconds. It has no `Default`: an unread value is the
/// `None` of an `Option<At<T>>`, never a zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At<T> {
    /// What was read.
    pub v: T,
    /// When, Unix milliseconds.
    pub at_ms: u64,
}

impl<T> At<T> {
    /// `v`, read at `at_ms`.
    pub const fn new(v: T, at_ms: u64) -> Self {
        Self { v, at_ms }
    }

    /// The same reading of another value.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> At<U> {
        At {
            v: f(self.v),
            at_ms: self.at_ms,
        }
    }
}

/// A measure as the owner reads it: how much is used and how much there is, each in the
/// measure's own unit and each read or not on its own. The share is derived where it is drawn,
/// and only where both are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair<T> {
    /// How much is in use.
    pub used: Option<At<T>>,
    /// How much there is.
    pub capacity: Option<At<T>>,
}

impl<T> Pair<T> {
    /// Neither side read.
    pub const UNREAD: Self = Self {
        used: None,
        capacity: None,
    };
}

/// How a machine or an agent was sighted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum How {
    /// This Mac's own windows answered for themselves.
    Own,
    /// A fleet peer answered this Mac's survey.
    Survey,
}

impl How {
    /// `self` or `survey`.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Own => "self",
            Self::Survey => "survey",
        }
    }
}

/// The agent program of a session, by the word its row names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Harness {
    Claude,
    Codex,
    CodexAlab,
    Agy,
    Muse,
    Grok,
    Kimi,
    /// A program this build has no word for.
    Other,
}

impl Harness {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &[
        "claude",
        "codex",
        "codex-alab",
        "agy",
        "muse",
        "grok",
        "kimi",
        "other",
    ];

    /// The harness a session row's `program=` names.
    pub fn of_program(program: Option<&str>) -> Self {
        match program {
            Some("claude") => Self::Claude,
            Some("codex") => Self::Codex,
            Some("codex-alab") => Self::CodexAlab,
            Some("agy" | "antigravity") => Self::Agy,
            Some("muse" | "muse-code") => Self::Muse,
            Some("grok") => Self::Grok,
            Some("kimi") => Self::Kimi,
            _ => Self::Other,
        }
    }

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::CodexAlab => "codex-alab",
            Self::Agy => "agy",
            Self::Muse => "muse",
            Self::Grok => "grok",
            Self::Kimi => "kimi",
            Self::Other => "other",
        }
    }

    /// The provider realms an account of this harness can belong to, as
    /// `typed::keys::canonical_realm` spells them: what an address is keyed under.
    pub const fn realms(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude"],
            Self::Codex | Self::CodexAlab => &["codex"],
            Self::Agy => &["google"],
            Self::Muse => &["meta"],
            Self::Grok => &["grok"],
            Self::Kimi => &["kimi:global", "kimi:mainland-cn"],
            Self::Other => &[],
        }
    }
}

/// One machine's agents, counted by [`State`]. A tally is a count somebody made: a machine
/// nobody counted has none (`Option<At<Tally>>`), and its zero is a counted zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub agents: u32,
    pub busy: u32,
    pub idle: u32,
    pub waiting: u32,
    pub walled: u32,
    pub stuck: u32,
    pub unknown: u32,
}

impl Tally {
    /// Count one agent in `state`.
    pub const fn add(&mut self, state: State) {
        self.agents = self.agents.saturating_add(1);
        let slot = match state {
            State::Busy => &mut self.busy,
            State::Idle => &mut self.idle,
            State::Waiting => &mut self.waiting,
            State::Walled => &mut self.walled,
            State::Stuck => &mut self.stuck,
            State::Unknown => &mut self.unknown,
        };
        *slot = slot.saturating_add(1);
    }
}

/// The account a session row named: an address, or a label where its Mac knows no address.
/// Held only until the fold resolves it to a registry key; it has no `Display`, and its `Debug`
/// prints no part of it.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountWord(String);

impl AccountWord {
    /// The word `raw` names, where it names one (`-` and an empty word name none).
    pub fn new(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        (!raw.is_empty() && raw != "-" && raw.len() <= 254).then(|| Self(raw.to_owned()))
    }

    /// The word, for the fold's resolution alone.
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccountWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountWord(..)")
    }
}

/// One agent session as a read saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSeen {
    /// Its session id on its machine.
    pub sid: String,
    pub harness: Harness,
    pub state: State,
    /// The kind of limit a walled agent is at (`usage-weekly`, `auth`).
    pub state_why: Option<String>,
    /// How long its state word had stood when the read was made, where its row says.
    pub state_age_ms: Option<u64>,
    pub task: Option<Task>,
    /// Context left, percent.
    pub ctx_left_pct: Option<u8>,
    /// Its account's 5-hour window used, percent, as its own footer read it.
    pub usage_5h_pct: Option<u8>,
    /// Its account's 7-day window used, percent, as its own footer read it.
    pub usage_7d_pct: Option<u8>,
    /// The account its row named.
    pub account: Option<AccountWord>,
    pub model: Option<String>,
    /// Whether a person or a lead agent pinned it, as its row's `pinned=` says; `None` where its
    /// row said neither (an aterm before the pin).
    pub pinned: Option<bool>,
    /// What its row says of its turn, its open step and its last output.
    pub turn: TurnFacts,
    /// Input tokens of its conversation so far, where its row sent the whole conversation's.
    pub tok_in: Option<u64>,
    /// Output tokens, as [`Self::tok_in`] counts input.
    pub tok_out: Option<u64>,
    /// How long those two have counted this conversation unbroken, ms.
    pub tok_for_ms: Option<u64>,
    /// Turns it has opened this session.
    pub turns: Option<u32>,
    /// Tool steps its agent run in front has taken, every class: that run's count, never the
    /// session's (a new run counts from 0, so a fall between two reads is a new run, not a
    /// count that went back).
    pub tools: Option<u32>,
    /// Its work class, by [`Work::of`] its state and [`Self::turn`].
    pub work: Work,
    /// When the window that listed it answered, Unix ms on its machine's own clock (its reply's
    /// header's `at_ms=`, or for a peer the row's own `at_ms=`, which its node's merge adds where
    /// that window answered at another time than the merged header says): what its token rate is
    /// timed by. Each window answers at its own time, so one machine's agents can carry
    /// different times.
    pub answer_at_ms: Option<u64>,
    /// Its token rate between the machine's last answer and this one ([`Latest::with`]); no
    /// wire key fills it, and one read alone has none.
    pub rate: Option<Rate>,
}

/// The most agent sessions one machine's answer keeps; the rest are counted
/// ([`Answered::agents_more`]). A reply is byte-capped by its sender well below this.
pub const AGENTS_MAX: usize = 128;

/// Where a session's last output was read, by the word its row's `out_src=` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutSrc {
    /// Its transcript: a row the API served, a text row the agent wrote, or a tool step.
    Transcript,
    /// The last feed that moved a cell of its screen, which a spinner also moves: no proof of
    /// output, and only its staleness says anything.
    Screen,
}

impl OutSrc {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &["transcript", "screen"];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::Screen => "screen",
        }
    }

    /// The source `raw` names: exactly one of [`Self::WORDS`]; any other word names none.
    pub fn of(raw: &str) -> Option<Self> {
        match raw {
            "transcript" => Some(Self::Transcript),
            "screen" => Some(Self::Screen),
            _ => None,
        }
    }
}

/// What a session row says of its turn and its output: the inputs of [`Work::of`]. Every age
/// is the answering machine's own (its clock less a time on it), and `None` where the row sent
/// `-` or no key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnFacts {
    /// Whether its session log has a turn open (`turn=`).
    pub turn_open: Option<bool>,
    /// How long that turn has been open, ms.
    pub turn_age_ms: Option<u64>,
    /// How long ago its newest closed turn closed, ms.
    pub turn_end_age_ms: Option<u64>,
    /// How long its open step (a tool call, an approval, a question) has been open, ms.
    pub step_age_ms: Option<u64>,
    /// How long ago its last output was written, ms, read from [`Self::out_src`].
    pub out_age_ms: Option<u64>,
    pub out_src: Option<OutSrc>,
    /// Its background shells (`bg=`).
    pub bg: Option<u32>,
    /// Its row had no `turn=` key: an aterm before the turn, whose turn is read from its
    /// agent's own word ([`Self::word_turn`]).
    pub older: bool,
    /// Its agent's own word (`agent=`) is one the session log's turn tracker opens a turn on:
    /// `busy`, or `prompt` (a box: a permission, a question's options). `question` (the agent
    /// ended its turn on a question) and `idle` close one. Read only where [`Self::turn_open`]
    /// is unread (D12's fallback for an aterm before the turn).
    pub word_turn: bool,
}

impl TurnFacts {
    /// The facts of a row that sends none of them: an aterm before the turn.
    pub const OLDER: Self = Self {
        turn_open: None,
        turn_age_ms: None,
        turn_end_age_ms: None,
        step_age_ms: None,
        out_age_ms: None,
        out_src: None,
        bg: None,
        older: true,
        word_turn: false,
    };

    /// Whether the agent is in a turn (D12): its session log has one open; or it runs
    /// background work (`bg` at least 1) and its transcript wrote output after its last turn
    /// closed (any time, where none has closed) and within [`OUTPUT_KEEP_MS`], since output
    /// older than the close is the closed turn's own. A row whose `turn=` is unread (an aterm
    /// before the turn) is in a turn where its agent's own word opens one ([`Self::word_turn`]),
    /// as that aterm's session log would have it: a box (`prompt`) is inside a turn whatever
    /// its state says, and a question the agent ended its turn on is not.
    pub fn in_turn(&self) -> bool {
        match self.turn_open {
            Some(true) => true,
            Some(false) => {
                let since_close = self
                    .out_age_ms
                    .filter(|_| self.out_src == Some(OutSrc::Transcript))
                    .is_some_and(|out| {
                        out <= OUTPUT_KEEP_MS && self.turn_end_age_ms.is_none_or(|end| out < end)
                    });
                self.bg.is_some_and(|bg| bg >= 1) && since_close
            }
            None => self.word_turn,
        }
    }
}

/// How recent output must be for an agent in a turn to count as with output: written in the
/// last 10 minutes.
pub const OUTPUT_KEEP_MS: u64 = 600_000;

/// What an agent is doing with its turn: one class per agent, decided here so a machine's
/// tally and its agents' rows use one rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Work {
    /// At a limit (its state is `walled`).
    Held,
    /// In no turn.
    NoTurn,
    /// In a turn, waiting on a person (a box, a question, an approval).
    Asking,
    /// In a turn, with a step open: a tool call, however long, writes no transcript row.
    Tool,
    /// In a turn, its transcript written within [`OUTPUT_KEEP_MS`].
    WithOutput,
    /// In a turn, its last output (from either source) older than [`OUTPUT_KEEP_MS`].
    NoOutput,
    /// In a turn, and no output age says which: none read, or only a fresh screen.
    OutputUnread,
    /// Its state is unknown.
    Unknown,
}

impl Work {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &[
        "held",
        "no-turn",
        "asking",
        "tool",
        "with-output",
        "no-output",
        "output-unread",
        "unknown",
    ];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::NoTurn => "no-turn",
            Self::Asking => "asking",
            Self::Tool => "tool",
            Self::WithOutput => "with-output",
            Self::NoOutput => "no-output",
            Self::OutputUnread => "output-unread",
            Self::Unknown => "unknown",
        }
    }

    /// The class of an agent in `state` whose row says `row`, in this order: an unknown state
    /// is [`Self::Unknown`], a wall [`Self::Held`]; out of a turn ([`TurnFacts::in_turn`])
    /// [`Self::NoTurn`]; waiting on a person [`Self::Asking`]; a step open [`Self::Tool`];
    /// transcript output within [`OUTPUT_KEEP_MS`] [`Self::WithOutput`]; output older than it,
    /// from either source, [`Self::NoOutput`]; anything else [`Self::OutputUnread`]. A fresh
    /// screen proves nothing (a spinner moves it), so it is never with output.
    pub fn of(state: State, row: &TurnFacts) -> Self {
        if state == State::Unknown {
            return Self::Unknown;
        }
        if state == State::Walled {
            return Self::Held;
        }
        if !row.in_turn() {
            return Self::NoTurn;
        }
        if state == State::Waiting {
            return Self::Asking;
        }
        if row.step_age_ms.is_some() {
            return Self::Tool;
        }
        match row.out_age_ms {
            Some(age) if age <= OUTPUT_KEEP_MS && row.out_src == Some(OutSrc::Transcript) => {
                Self::WithOutput
            }
            Some(age) if age > OUTPUT_KEEP_MS => Self::NoOutput,
            _ => Self::OutputUnread,
        }
    }
}

/// One machine's agents, counted by [`Work`] class: every agent its answer counted, listed or
/// not. A counted zero is a zero; a machine whose agents went uncounted has none
/// (`Option<WorkTally>`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkTally {
    pub held: u32,
    pub no_turn: u32,
    pub asking: u32,
    pub tool: u32,
    pub with_output: u32,
    pub no_output: u32,
    pub output_unread: u32,
    pub unknown: u32,
}

impl WorkTally {
    /// Count one agent of class `work`.
    pub const fn add(&mut self, work: Work) {
        let slot = match work {
            Work::Held => &mut self.held,
            Work::NoTurn => &mut self.no_turn,
            Work::Asking => &mut self.asking,
            Work::Tool => &mut self.tool,
            Work::WithOutput => &mut self.with_output,
            Work::NoOutput => &mut self.no_output,
            Work::OutputUnread => &mut self.output_unread,
            Work::Unknown => &mut self.unknown,
        };
        *slot = slot.saturating_add(1);
    }
}

/// A machine's sessions waiting for memory: those whose row says `wait=memory` (a build's
/// front its memory ledger has not admitted), and the memory their claims ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitTally {
    /// Sessions waiting.
    pub waiting: u32,
    /// What their claims ask together, MiB; `None` where a waiter's claim was unread on its
    /// machine. With none waiting it is a counted 0.
    pub wait_mb: Option<u64>,
}

impl WaitTally {
    /// None waiting.
    pub const NONE: Self = Self {
        waiting: 0,
        wait_mb: Some(0),
    };

    /// Count one session waiting for `mb` MiB, `None` where its claim is unread.
    pub fn add(&mut self, mb: Option<u64>) {
        self.waiting = self.waiting.saturating_add(1);
        self.wait_mb = self.wait_mb.zip(mb).map(|(sum, mb)| sum.saturating_add(mb));
    }
}

/// An agent's token rate: what its counters gained between two reads of one session, per
/// minute, and the interval it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    /// Input tokens a minute.
    pub in_per_min: u64,
    /// Output tokens a minute.
    pub out_per_min: u64,
    /// The interval between the two reads, ms.
    pub over_ms: u64,
}

/// The shortest interval a token rate is taken over, ms.
pub const RATE_MIN_MS: u64 = 30_000;
/// The longest interval a token rate is taken over, ms: past it a burst is averaged away.
pub const RATE_MAX_MS: u64 = 300_000;

impl Rate {
    /// The rate of the session `then` was and `now` is, `over_ms` apart: `None` unless both
    /// reads hold both counters, neither counter went back, `over_ms` is [`RATE_MIN_MS`] to
    /// [`RATE_MAX_MS`], and `now`'s counters have counted this conversation for at least
    /// `over_ms` ([`AgentSeen::tok_for_ms`]): younger counters are another conversation's, or
    /// a catch-up's, and their gain is no rate.
    pub fn between(then: &AgentSeen, now: &AgentSeen, over_ms: u64) -> Option<Self> {
        if !(RATE_MIN_MS..=RATE_MAX_MS).contains(&over_ms) || now.tok_for_ms? < over_ms {
            return None;
        }
        let per_min = |then: Option<u64>, now: Option<u64>| {
            let gained = now?.checked_sub(then?)?;
            u64::try_from(u128::from(gained) * 60_000 / u128::from(over_ms)).ok()
        };
        Some(Self {
            in_per_min: per_min(then.tok_in, now.tok_in)?,
            out_per_min: per_min(then.tok_out, now.tok_out)?,
            over_ms,
        })
    }
}

/// A machine's memory pressure, by the word its header's `mem_pressure=` names
/// (`aterm_sysprobe::MemoryLevel::as_str`, which this crate does not link).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    Normal,
    Warn,
    Critical,
}

impl Pressure {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &["normal", "warn", "critical"];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }

    /// The level `raw` names: exactly one of [`Self::WORDS`]; any other word names none.
    pub fn of(raw: &str) -> Option<Self> {
        match raw {
            "normal" => Some(Self::Normal),
            "warn" => Some(Self::Warn),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// What a machine lets the other machines of its fleet do to it, by the word its header's
/// `access=` names ([`aterm_types::fleet_access::FleetAccess::word`]). It has no `Default`: a
/// header with no `access=`, or a word this build does not know, is an unread level, never the
/// lowest one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// Read Only.
    Read,
    /// Read and Write, no new tabs.
    Write,
    /// Full Access.
    Full,
}

impl Access {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &["read", "write", "full"];

    /// Its word, the one the fleet access level of that rank says on the wire.
    pub const fn word(self) -> &'static str {
        self.level().word()
    }

    /// The level `raw` names: exactly a word [`FleetAccess::from_word`] takes; any other word
    /// names none.
    ///
    /// [`FleetAccess::from_word`]: aterm_types::fleet_access::FleetAccess::from_word
    pub fn of(raw: &str) -> Option<Self> {
        use aterm_types::fleet_access::FleetAccess;
        FleetAccess::from_word(raw).map(|level| match level {
            FleetAccess::ReadOnly => Self::Read,
            FleetAccess::ReadWrite => Self::Write,
            FleetAccess::Full => Self::Full,
        })
    }

    const fn level(self) -> aterm_types::fleet_access::FleetAccess {
        use aterm_types::fleet_access::FleetAccess;
        match self {
            Self::Read => FleetAccess::ReadOnly,
            Self::Write => FleetAccess::ReadWrite,
            Self::Full => FleetAccess::Full,
        }
    }
}

/// A machine's own verdicts on whether each resource is at its limit, as its header's
/// `cpu_lim=`, `mem_lim=`, `disk_lim=` and `gpu_lim=` say: each is decided on the machine, by
/// its own build's rule, and `None` where it sent none or could not read an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub cpu: Option<bool>,
    pub mem: Option<bool>,
    pub disk: Option<bool>,
    pub gpu: Option<bool>,
}

impl Limits {
    /// No verdict read.
    pub const UNREAD: Self = Self {
        cpu: None,
        mem: None,
        disk: None,
        gpu: None,
    };
}

/// A machine's system-load score, as its header's `load=<level>:<over>` says: its own resource
/// deck's dial (the busier of its CPU and GPU, lifted by memory pressure, and how far past
/// capacity), each in hundredths and decided on the machine by its own build's rule. A header
/// that sends `load=-` (a dial with no level) or none has no score, never a score of 0, and one
/// that sends `<level>:-` has a level and no over, never an over of 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Load {
    /// The dial's level, `0..=100`.
    pub level_x100: u64,
    /// How far past capacity, `0..=100`: 0 within capacity; `None` where the machine's dial
    /// could not judge it (an input of it unread).
    pub over_x100: Option<u64>,
}

impl Load {
    /// The score a `load=` value names: two runs of digits, each `0..=100`, with one `:` between
    /// them, the second `-` where the over is unread. `-`, one number alone, a third part, a
    /// sign, a decimal or anything past 100 names none.
    pub fn of(raw: &str) -> Option<Self> {
        let (level, over) = raw.split_once(':')?;
        let hundredths = |raw: &str| {
            Some(raw)
                .filter(|raw| !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))?
                .parse::<u64>()
                .ok()
                .filter(|n| *n <= 100)
        };
        Some(Self {
            level_x100: hundredths(level)?,
            over_x100: match over {
                "-" => None,
                over => Some(hundredths(over)?),
            },
        })
    }
}

/// What a machine's header says of its resources: its look, taken on the machine at most a few
/// seconds before it answered. Every figure is the machine's own, in its own unit, and `None`
/// where its header sent none (an aterm before the look), sent `-` or sent what is not a whole
/// number in range. It has no `Default`; [`Look::UNREAD`] is the look of a header with none.
///
/// The two ages are differences of two times on the machine's own clock (`at_ms=` less
/// `m_at_ms=`, `at_ms=` less `up_since_ms=`), so no reading ever compares this Mac's clock with
/// another's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    /// How old the look was when the machine answered, ms.
    pub age_ms: Option<u64>,
    /// Logical cores.
    pub cores: Option<u64>,
    /// The 1-minute load average × 100.
    pub load1_x100: Option<u64>,
    /// Cores busy × 100, the mean over [`Self::rate_win_ms`].
    pub cpu_busy_x100: Option<u64>,
    /// The interval the CPU and network rates cover, ms.
    pub rate_win_ms: Option<u64>,
    /// Memory installed, MiB.
    pub mem_total_mb: Option<u64>,
    /// Memory in use, MiB.
    pub mem_used_mb: Option<u64>,
    /// Memory available, MiB.
    pub mem_avail_mb: Option<u64>,
    /// Swap in use, MiB.
    pub swap_used_mb: Option<u64>,
    /// Swap there is, MiB.
    pub swap_total_mb: Option<u64>,
    /// The size of its home volume, MiB; [`Answered::home_free_mb`] is the free space on it.
    pub disk_total_mb: Option<u64>,
    /// GPU cores, where its host reports a count.
    pub gpu_cores: Option<u64>,
    /// GPU device busy, percent.
    pub gpu_busy_pct: Option<u64>,
    /// The driver rate of its fastest counted network link, kbit/s.
    pub net_link_kbps: Option<u64>,
    /// Received on that link (summed over its counted links where none reports a rate), kbit/s,
    /// the mean over [`Self::rate_win_ms`].
    pub net_rx_kbps: Option<u64>,
    /// Sent, as [`Self::net_rx_kbps`] is received.
    pub net_tx_kbps: Option<u64>,
    /// Received plus sent on its other counted links, kbit/s.
    pub net_other_kbps: Option<u64>,
    pub mem_pressure: Option<Pressure>,
    pub limits: Limits,
    /// Its system-load score, beside its verdicts.
    pub load: Option<Load>,
    /// When it came up (the later of boot and its last wake), Unix ms on its own clock: what
    /// names one stay, compared only with its own earlier values.
    pub up_since_own_ms: Option<u64>,
    /// How long it had been up when it answered, ms.
    pub up_for_ms: Option<u64>,
    /// Its OS: `macos`, `linux` or `windows` ([`aterm_types::look_words::os_of`]).
    pub os: Option<&'static str>,
    /// Its package target triple ([`aterm_types::look_words::target_of`]).
    pub target: Option<String>,
}

impl Look {
    /// A header with no look: every figure unread.
    pub const UNREAD: Self = Self {
        age_ms: None,
        cores: None,
        load1_x100: None,
        cpu_busy_x100: None,
        rate_win_ms: None,
        mem_total_mb: None,
        mem_used_mb: None,
        mem_avail_mb: None,
        swap_used_mb: None,
        swap_total_mb: None,
        disk_total_mb: None,
        gpu_cores: None,
        gpu_busy_pct: None,
        net_link_kbps: None,
        net_rx_kbps: None,
        net_tx_kbps: None,
        net_other_kbps: None,
        mem_pressure: None,
        limits: Limits::UNREAD,
        load: None,
        up_since_own_ms: None,
        up_for_ms: None,
        os: None,
        target: None,
    };
}

/// What a machine that answered said of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    /// The aterm build its header names.
    pub build: Option<String>,
    /// When it answered, Unix ms on its own clock (its first reply's header's `at_ms=`), compared
    /// only with its own earlier answers. A token rate is timed by each agent's own reply
    /// ([`AgentSeen::answer_at_ms`]).
    pub answer_at_ms: Option<u64>,
    /// Free space on its home volume, MiB.
    pub home_free_mb: Option<u64>,
    /// Its resources, as its header's look says; [`Look::UNREAD`] for a header with none.
    pub look: Look,
    /// Its fleet access, as its header's `access=` says; `None` for a header with none (an
    /// aterm from before the lock) or a word this build does not know.
    pub access: Option<Access>,
    /// Whether a drive grant there admits this Mac: `Some(true)` for a peer's header that says
    /// `drive=1`, `Some(false)` for one that says `access=` without it, `None` for one with no
    /// `access=` (an aterm that does not say) and for this Mac's own machine, whose windows
    /// answer a local read that asks for no grant (it drives its own sessions without one).
    pub drive: Option<bool>,
    /// Its live sessions, agents or not, where its answer counted them all: `None` for a
    /// partial answer (a window of it went unread, or its reply left rows out), whose rows are a
    /// lower bound and no count.
    pub sessions: Option<u32>,
    /// Its agents by state; `None` for an older aterm whose rows cannot be classed.
    pub tally: Option<Tally>,
    /// Its agents by [`Work`] class, every agent it counted, listed or not; `None` where
    /// [`Self::tally`] is.
    pub work: Option<WorkTally>,
    /// Its sessions waiting for memory; `None` for a partial answer (as [`Self::sessions`]) and
    /// for one a row of which said no `wait=` (an aterm before it).
    pub wait: Option<WaitTally>,
    /// Its agent sessions, by sid, at most [`AGENTS_MAX`].
    pub agents: Vec<AgentSeen>,
    /// Agent sessions counted in [`Self::tally`] and not listed.
    pub agents_more: u32,
}

impl Answered {
    /// Set each agent's [`AgentSeen::rate`] against `then`, the machine's previous answer, heard
    /// by a read at `then.at_ms`, this answer being heard at `now_ms`: [`Rate::between`] the
    /// agent of the same sid there and here, over the interval between the two replies that
    /// listed it on the machine's own clock ([`AgentSeen::answer_at_ms`]), or between the two
    /// reads where either reply has no time. An agent with no earlier sighting, or no previous
    /// answer, has none.
    fn rate_since(&mut self, then: Option<&At<Self>>, now_ms: u64) {
        let Some(then) = then else {
            for agent in &mut self.agents {
                agent.rate = None;
            }
            return;
        };
        let before: BTreeMap<&str, &AgentSeen> = then
            .v
            .agents
            .iter()
            .map(|agent| (agent.sid.as_str(), agent))
            .collect();
        for agent in &mut self.agents {
            agent.rate = before.get(agent.sid.as_str()).and_then(|was| {
                let over_ms = match (was.answer_at_ms, agent.answer_at_ms) {
                    (Some(was), Some(is)) => is.checked_sub(was),
                    _ => now_ms.checked_sub(then.at_ms),
                }?;
                Rate::between(was, agent, over_ms)
            });
        }
    }
}

/// One machine's part of a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// It answered. Boxed: an answer is some hundreds of bytes and a silence a word.
    Answered(Box<Answered>),
    /// It was asked and did not answer; `why` is the producer's reason token where it gave one
    /// (`no-name`, `nothing-listens`, …), never its prose.
    Silent { why: Option<String> },
}

/// One machine as a read named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineSeen {
    pub how: How,
    /// What names it from one read to the next: `self` for this Mac, else a digest of the name
    /// the book dials (`un-<16 hex>`), which is also its row's key while no registry row names
    /// it.
    pub id: String,
    /// The first label of the host name the book dials; `None` for this Mac and for a peer
    /// dialled by address.
    pub host: Option<String>,
    /// Its own `m<N>` label, where its header names one.
    pub label: Option<String>,
    /// Its answer proved no node key this Mac binds ([`Self::unproven()`]): what it says of
    /// itself is a claim.
    pub unproven: bool,
    pub answer: Answer,
}

/// The id of this Mac's own sighting.
pub const OWN_ID: &str = "self";

impl MachineSeen {
    /// This Mac, as its own windows answered; `label` is its header's `label=`.
    pub fn own(label: Option<&str>, answer: Answer) -> Self {
        Self {
            how: How::Own,
            id: OWN_ID.to_owned(),
            host: None,
            label: label.and_then(label_word),
            unproven: false,
            answer,
        }
    }

    /// A fleet peer the book dials as `dial`, shown as `short` (its host name's first label);
    /// `label` is its header's `label=`. An address is never kept: `dial` is digested, and a
    /// `short` that is not a host label (an IP address) is dropped.
    pub fn peer(dial: &str, short: &str, label: Option<&str>, answer: Answer) -> Self {
        let mut sha = Sha256::new();
        sha.update(b"aterm/fleet/sighted/1\0");
        sha.update(dial.trim().to_ascii_lowercase().as_bytes());
        let mut id = String::from("un-");
        aterm_codec::hex::push_lower(&mut id, &sha.finalize()[..8]);
        let host = (!short.is_empty()
            && short.len() <= 63
            && short
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && short.bytes().any(|b| b.is_ascii_alphabetic()))
        .then(|| short.to_ascii_lowercase());
        Self {
            how: How::Survey,
            id,
            host,
            label: label.and_then(label_word),
            unproven: false,
            answer,
        }
    }

    /// This peer, whose answer proved no node key this Mac binds. The label it claims is not
    /// taken: it stays the machine the book dials. What it says of itself is kept and marked
    /// (`proven` is false in its machine row and in each of its agent rows), and names no
    /// account as running anywhere.
    #[must_use]
    pub fn unproven(mut self) -> Self {
        self.unproven = true;
        self.label = None;
        self
    }

    /// The machine number its label names, else the one its host name starts with.
    pub fn number(&self) -> Option<Number> {
        self.label
            .as_deref()
            .or(self.host.as_deref())
            .and_then(Number::leading)
    }
}

/// The canonical `m<N>` a header's `label=` names.
pub fn label_word(raw: &str) -> Option<String> {
    Number::leading(raw).map(|n| format!("m{}", n.get()))
}

/// `raw` as a bounded word a row may carry: 1 to `max` bytes of letters, digits, spaces and
/// `. _ + : / ( ) [ ] -`, trimmed. Anything else (an `@`, a control character, a quote) names
/// nothing: a field that could carry an address or a sentence is dropped whole.
pub fn word(raw: &str, max: usize) -> Option<String> {
    let raw = raw.trim();
    (!raw.is_empty()
        && raw != "-"
        && raw.len() <= max
        && raw.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b' ' | b'.' | b'_' | b'+' | b':' | b'/' | b'(' | b')' | b'[' | b']' | b'-'
                )
        }))
    .then(|| raw.to_owned())
}

/// One fleet read: every machine it named, answered or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    /// When the read was made, Unix milliseconds.
    pub read_at_ms: u64,
    pub machines: Vec<MachineSeen>,
    /// Whether the read asked the fleet. `false` for a read whose question of the whole fleet
    /// was refused (this Mac is away, or its roster is unread) while its own windows answered:
    /// `machines` then holds this Mac alone, and the read says nothing of any peer.
    pub fleet: bool,
}

/// What the reads so far say of one machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sighted {
    pub how: How,
    pub host: Option<String>,
    pub label: Option<String>,
    /// Its last answer, with the read that heard it.
    pub answered: Option<At<Answered>>,
    /// That answer proved no node key this Mac binds ([`MachineSeen::unproven()`]).
    pub unproven: bool,
    /// The newest read that named it found it silent: its reason token, and that read's time.
    /// `None` once it answers again.
    pub silent: Option<At<Option<String>>>,
    /// The newest read that named it at all.
    pub named_ms: u64,
}

/// How long a machine no read names stays held: past it, it has left the book this Mac reads.
pub const SIGHTED_KEEP_MS: u64 = 2 * 3_600_000;

/// What the reads so far say: each machine's last answer and whether the newest read heard it.
/// It is the folder's memory between reads, bounded by the book (256 peers and this Mac) and by
/// [`SIGHTED_KEEP_MS`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Latest {
    /// The newest read's time; 0 before any. This Mac's own answer is judged against it.
    pub read_at_ms: u64,
    /// The time of the newest read that asked the fleet ([`Read::fleet`]); 0 before any. A
    /// peer is judged against it: a read that asked no peer says nothing of one.
    pub fleet_at_ms: u64,
    /// Each machine by its [`MachineSeen::id`].
    pub machines: BTreeMap<String, Sighted>,
}

impl Latest {
    /// These reads and `read`: a machine that answered takes its new answer, one that was silent
    /// keeps its last answer beside the silence, one the read does not name keeps what it had,
    /// and one no read has named for [`SIGHTED_KEEP_MS`] is dropped. A read that asked no peer
    /// ([`Read::fleet`]) drops none: no peer left the book by going unasked.
    #[must_use]
    pub fn with(&self, read: Read) -> Self {
        let at = read.read_at_ms;
        let mut machines = self.machines.clone();
        for seen in read.machines {
            let entry = machines.entry(seen.id).or_insert_with(|| Sighted {
                how: seen.how,
                host: None,
                label: None,
                answered: None,
                unproven: false,
                silent: None,
                named_ms: at,
            });
            entry.how = seen.how;
            entry.named_ms = at;
            if seen.host.is_some() {
                entry.host = seen.host;
            }
            if seen.label.is_some() {
                entry.label = seen.label;
            }
            match seen.answer {
                Answer::Answered(mut answered) => {
                    // A rate is taken only between two answers of one proof state: a claim
                    // (an answer that proved no node key) is never the baseline of a proven
                    // row's rate, nor a proven answer the baseline of a claim's.
                    let then = entry
                        .answered
                        .as_ref()
                        .filter(|_| entry.unproven == seen.unproven);
                    answered.rate_since(then, at);
                    entry.answered = Some(At::new(*answered, at));
                    entry.unproven = seen.unproven;
                    entry.silent = None;
                }
                Answer::Silent { why } => entry.silent = Some(At::new(why, at)),
            }
        }
        if read.fleet {
            machines.retain(|_, sighted| at.saturating_sub(sighted.named_ms) <= SIGHTED_KEEP_MS);
        }
        Self {
            read_at_ms: at,
            fleet_at_ms: if read.fleet { at } else { self.fleet_at_ms },
            machines,
        }
    }
}
