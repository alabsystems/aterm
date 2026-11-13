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

/// What an agent is doing, by its row's own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// At work.
    Busy,
    /// At rest.
    Idle,
    /// Waiting on a person.
    Waiting,
    /// At a limit.
    Walled,
    /// Its task is stalled.
    Stuck,
    /// Any other word, or none.
    Unknown,
}

impl State {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] =
        &["busy", "idle", "waiting", "walled", "stuck", "unknown"];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Idle => "idle",
            Self::Waiting => "waiting",
            Self::Walled => "walled",
            Self::Stuck => "stuck",
            Self::Unknown => "unknown",
        }
    }
}

/// A session's task, as its row's `task=` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Task {
    Working,
    Done,
    Failed,
    Exited,
    Stalled,
}

impl Task {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &["working", "done", "failed", "exited", "stalled"];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Exited => "exited",
            Self::Stalled => "stalled",
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
}

/// The most agent sessions one machine's answer keeps; the rest are counted
/// ([`Answered::agents_more`]). A reply is byte-capped by its sender well below this.
pub const AGENTS_MAX: usize = 128;

/// What a machine that answered said of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    /// The aterm build its header names.
    pub build: Option<String>,
    /// Free space on its home volume, MiB.
    pub home_free_mb: Option<u64>,
    /// Its live sessions, agents or not, where its answer counted them all: `None` for a
    /// partial answer (a window of it went unread, or its reply left rows out), whose rows are a
    /// lower bound and no count.
    pub sessions: Option<u32>,
    /// Its agents by state; `None` for an older aterm whose rows cannot be classed.
    pub tally: Option<Tally>,
    /// Its agent sessions, by sid, at most [`AGENTS_MAX`].
    pub agents: Vec<AgentSeen>,
    /// Agent sessions counted in [`Self::tally`] and not listed.
    pub agents_more: u32,
}

/// One machine's part of a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// It answered.
    Answered(Answered),
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
                Answer::Answered(answered) => {
                    entry.answered = Some(At::new(answered, at));
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
