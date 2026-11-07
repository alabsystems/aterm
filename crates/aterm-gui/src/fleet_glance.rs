// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLEET GLANCE (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3, S10).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_types::needs::NeedWord;

/// How often this Mac reads the fleet automatically while a read reaches a peer.
pub(crate) const POLL_EVERY: Duration = Duration::from_secs(60);
/// The longest wait between reads that reach no peer ([`Quiet`]).
pub(crate) const QUIET_MAX: Duration = Duration::from_secs(30 * 60);
/// On menu open, a read younger than this is fresh enough.
pub(crate) const MENU_FRESH: Duration = Duration::from_secs(15);
/// A fleet need counts on the badge only from a read this young.
pub(crate) const COUNT_FRESH: Duration = Duration::from_secs(300);
/// One read's whole bound: the child's own deadline is 3 s a peer, so this is the kill, never a wait it is expected to reach.
pub(crate) const READ_CAP: Duration = Duration::from_secs(10);
/// The most fleet peers one snapshot keeps (`aterm_net`'s book is bounded
/// too); the rest are counted as not read.
const MACS_MAX: usize = 256;

/// One need on a fleet peer, from its facts row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FleetNeed {
    /// The session's sid there.
    pub(crate) sid: String,
    /// Its published need.
    pub(crate) word: NeedWord,
    /// Who it is: the title a person gave it where the peer allows content, else its role, else its program.
    pub(crate) who: String,
    /// "worker of s-1e91": the task's `from=` where it names a sid.
    pub(crate) worker_of: Option<String>,
    /// How long ago the fact it stands on moved, as of the read.
    pub(crate) age_ms: Option<u64>,
    /// The need's own words where the peer serves content ([`content_what`]): an ask's text, a box's kind, a wall's kind and reset; at any level, the words of a need the row says is the program's own report (`claim`).
    pub(crate) what: Option<String>,
    /// The need's words where the row says it is the program's own report (`needs_by=program`, [`crate::needs::claim_words_of_row`]): aterm's fixed words, no content of the peer's.
    pub(crate) claim: Option<&'static str>,
    /// The row says the need is a harness's `NEEDS YOU:` report its agent carries on past (`needs_by=harness`): Needs you lists it after the needs that stopped their agents ([`crate::status_item::NeedRow::carries_on`]).
    pub(crate) carries_on: bool,
}

/// One session on a fleet peer that this Mac holds over the fleet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FleetHeld {
    /// The session's sid there.
    pub(crate) sid: String,
    /// Who it is, named as a need is ([`FleetNeed::who`]).
    pub(crate) who: String,
}

/// One manager on a fleet peer: a sid its live tasks' `from=` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FleetManager {
    /// Its label there (role, else program, else short sid).
    pub(crate) label: String,
    /// Its sid.
    pub(crate) sid: String,
    /// How many workers name it.
    pub(crate) workers: usize,
}

/// One fleet peer that answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FleetMac {
    /// How its rows name it: its book name's first label ([`short_name`]).
    pub(crate) name: String,
    /// What `aterm ctl survey --host` is given for it: the name the book knows it by ([`dial_of`]).
    pub(crate) dial: String,
    /// Its node id (its header's `node=`), when it sent one and proved it ([`Self::unproven`]'s is its claim, never kept).
    pub(crate) node: Option<String>,
    /// Where Screen Sharing and ssh go: the peer's own `host=` (a proven peer's only), else the host its book name names.
    pub(crate) host: Option<String>,
    /// Its rows carry titles and words (`[fleet] allow = "content"`).
    pub(crate) content: bool,
    /// It lets this Mac drive it (its header's `drive=1`, the step-in).
    pub(crate) drive: bool,
    /// Its rows publish no `needs=`: an older aterm, listed and never counted.
    pub(crate) older: bool,
    /// Answered without proving a node key this Mac binds (`"unproven":true`, [`BookFacts::unbound_issuer`]).
    pub(crate) unproven: bool,
    /// The sessions its reply holds rows for; `None` on this Mac's copy, where no session count
    /// leaves a Mac (its `now/activity` row counts agents, not sessions).
    pub(crate) sessions: Option<usize>,
    /// What its agents are doing, by their own words ([`AgentWord::of`]): counted from its survey
    /// rows, or on this Mac's copy from its `now/activity` row ([`AgentTally::of_row`]). `None`
    /// where it was not read: an older aterm's rows, and on the copy a Mac that publishes no
    /// activity row, or whose row trails its presence row past
    /// `aterm_metrics::row::ACTIVITY_FRESH_MS` ([`Self::activity_absent`] says which). Absent,
    /// never zero.
    pub(crate) activity: Option<AgentTally>,
    /// Why this Mac's copy holds no activity for it; `None` where it does, and on a survey,
    /// whose rows say themselves why ([`Self::older`], [`Self::unproven`]).
    #[cfg(unix)]
    pub(crate) activity_absent: Option<ActivityAbsent>,
    /// Its agent sessions, the first [`SESSIONS_MAX`] of them in the reply's order; on the copy,
    /// each by its sid and word alone (no program, task or context leaves a Mac there).
    pub(crate) agent_sessions: Vec<FleetSession>,
    /// The agent sessions past [`SESSIONS_MAX`], counted and not kept.
    pub(crate) sessions_more: usize,
    /// Rows its reply cap left out (`sessions=` past every session row sent, exited ones included: the header counts them too).
    pub(crate) left_out: usize,
    /// Live instances the peer reports whose sessions this reply read (`instances_read=`, 1 where
    /// a window answered alone).
    pub(crate) instances_read: usize,
    /// Other live instances the peer reports, whose sessions this reply did not read.
    pub(crate) instances_unread: usize,
    /// Its needs.
    pub(crate) needs: Vec<FleetNeed>,
    /// Its sessions this Mac holds over the fleet, whatever their need: a hold is never a need.
    pub(crate) held_here: Vec<FleetHeld>,
    /// Its managers.
    pub(crate) managers: Vec<FleetManager>,
    /// When ITS rows were read (unix ms): the snapshot's read for a Mac that answered it, an earlier one for a [`Self::carried`] Mac.
    pub(crate) read_at_ms: u64,
    /// It did not answer this read: its rows are its last answer's, kept while they still count ([`carry`]).
    pub(crate) carried: bool,
    /// Its build: the survey header's `version=`, or, on this Mac's copy ([`CopyRunner`]), its
    /// presence row's, else its signed book entry's ([`aterm_agent::fleet_view::SeatRow::build`]).
    pub(crate) build: Option<String>,
    /// Its membership log label (`m<N>`), where this Mac's copy read it and the log seats it.
    pub(crate) label: Option<String>,
    /// It needs an update, as `aterm fleet` judges it
    /// ([`aterm_agent::fleet_view::SeatRow::needs_update`]); read on this Mac's copy only, never
    /// inferred from a build.
    pub(crate) needs_update: bool,
    /// How far its tables are from this Mac's, where this Mac's copy read it.
    #[cfg(unix)]
    pub(crate) tables: Option<aterm_mesh::presence::Behind>,
}

/// Why this Mac's copy holds no activity for a Mac ([`FleetMac::activity_absent`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityAbsent {
    /// The node read no activity rows for its answer (`View::activity` is `None`).
    Unread,
    /// The Mac publishes no activity row: an older aterm, or one whose windows were never counted.
    Unpublished,
    /// Its newest row trails its presence row past `aterm_metrics::row::ACTIVITY_FRESH_MS`: by
    /// `behind_ms`, both of its own clock, or `None` where the seat has no presence row.
    Stale {
        /// How far the row trails the seat's presence row.
        behind_ms: Option<u64>,
    },
}

/// What one Mac's agents are doing, counted from their rows' `agent=` words.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AgentTally {
    /// Its agent sessions.
    pub(crate) agents: u16,
    /// Of them, at work (`busy`, or a `prompt` or `question` its harness answers).
    pub(crate) busy: u16,
    /// At rest (`idle`).
    pub(crate) idle: u16,
    /// Waiting on a person ([`AgentWord::Waiting`]).
    pub(crate) waiting: u16,
    /// At a limit (`wall:*`).
    pub(crate) walled: u16,
    /// Any other word (`unknown`, `survey`, `-`, one this build does not know).
    pub(crate) unknown: u16,
}

impl AgentTally {
    /// The tally a Mac's `now/activity` row counts, each count at most `u16::MAX`.
    #[cfg(unix)]
    pub(crate) fn of_row(row: &aterm_metrics::row::ActivityRow) -> Self {
        let n = |count: u32| u16::try_from(count).unwrap_or(u16::MAX);
        Self {
            agents: n(row.agents()),
            busy: n(row.busy),
            idle: n(row.idle),
            waiting: n(row.waiting),
            walled: n(row.walled),
            unknown: n(row.unknown),
        }
    }

    /// Count one agent whose word is `word`.
    fn add(&mut self, word: &AgentWord) {
        self.agents = self.agents.saturating_add(1);
        let slot = match word {
            AgentWord::Busy => &mut self.busy,
            AgentWord::Idle => &mut self.idle,
            AgentWord::Waiting => &mut self.waiting,
            AgentWord::Walled(_) => &mut self.walled,
            AgentWord::Unknown => &mut self.unknown,
        };
        *slot = slot.saturating_add(1);
    }
}

/// An agent's own word for what it is doing (its row's `agent=`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AgentWord {
    /// `busy`.
    Busy,
    /// `idle`.
    Idle,
    /// It waits on a person: its row names a need a person decides (`ask`, `approval`,
    /// `question`), whatever its own word, or it is at a `prompt` or `question` its row does not
    /// say is answered (`needs=-`).
    Waiting,
    /// `wall:<kind>`, its kind kept (`usage-weekly`, `auth`).
    Walled(String),
    /// Any other word, or none.
    Unknown,
}

impl AgentWord {
    /// The word a row's `agent=` says, beside its raw `needs=`, classed as `aterm ctl survey`
    /// classes it (`class_of`) for every need this build reads: a need a person decides
    /// (`Severity::Decide`) waits on a person whatever the agent says (a need word this build
    /// cannot read, which `class_of` calls asking, keeps the agent's word here, as the glance
    /// draws no need for it either), so an idle agent that ended its turn on a question is waiting,
    /// not quiet. Otherwise a `prompt` or `question` whose row read its needs and found none for
    /// a person (`needs=-`: its harness answers it) is at work, and one whose row names another
    /// need, or sends no `needs` (an older peer), waits on a person.
    pub(crate) fn of(word: Option<&str>, needs: Option<&str>) -> Self {
        if needs
            .and_then(NeedWord::parse)
            .is_some_and(|need| need.severity() == aterm_types::needs::Severity::Decide)
        {
            return Self::Waiting;
        }
        match word {
            Some("busy") => Self::Busy,
            Some("idle") => Self::Idle,
            Some("prompt" | "question") if needs == Some("-") => Self::Busy,
            Some("prompt" | "question") => Self::Waiting,
            Some(word) => word
                .strip_prefix("wall:")
                .map_or(Self::Unknown, |kind| Self::Walled(kind.to_owned())),
            None => Self::Unknown,
        }
    }

    /// The word a session row counts under in its Mac's [`AgentTally`], from its
    /// `program_kind=`, `agent=` and raw `needs=`: `None` for a row that is no agent's. The
    /// survey's read ([`mac_of`]) and the node's own count of its windows
    /// ([`activity_of_survey`]) both count by it.
    pub(crate) fn of_row(
        kind: Option<&str>,
        word: Option<&str>,
        needs: Option<&str>,
    ) -> Option<Self> {
        (kind == Some("agent")).then(|| Self::of(word, needs))
    }

    /// Its word on a `now/activity` row (`aterm_metrics::row::AgentEntry::word`).
    #[cfg(unix)]
    pub(crate) const fn wire_word(&self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Idle => "idle",
            Self::Waiting => "waiting",
            Self::Walled(_) => "walled",
            Self::Unknown => "unknown",
        }
    }

    /// Its detail on a `now/activity` row: a wall's kind.
    #[cfg(unix)]
    pub(crate) fn wire_detail(&self) -> Option<&str> {
        match self {
            Self::Walled(kind) => Some(kind),
            _ => None,
        }
    }

    /// The word a `now/activity` row's entry says ([`Self::wire_word`], [`Self::wire_detail`]);
    /// a word this build does not know is [`Self::Unknown`].
    #[cfg(unix)]
    pub(crate) fn of_wire(word: &str, detail: Option<&str>) -> Self {
        match word {
            "busy" => Self::Busy,
            "idle" => Self::Idle,
            "waiting" => Self::Waiting,
            "walled" => Self::Walled(detail.unwrap_or_default().to_owned()),
            _ => Self::Unknown,
        }
    }
}

/// This Mac's agents as its windows' merged survey answer `wire` (`OK <n>`, the `instance` line,
/// the `session` rows) says at `at` (Unix seconds), counted as a peer's survey rows are
/// ([`mac_of`]): an exited session is not counted, a session row counts by
/// [`AgentWord::of_row`], and the first [`aterm_metrics::row::AGENTS_LISTED_MAX`] agents whose
/// sid is a minted one are listed by sid and word. `None` for an answer that is no survey, as
/// for an older peer where a live row sends no `needs`, and for a partial answer, which a row
/// has no way to mark as a lower bound: one whose `instance` line says some window went unread
/// (`instances_read=` below `instances=`, each 1 where it is absent, as [`mac_of`] reads them),
/// or whose `sessions=` counts rows the reply cap left out. Nothing is published for it, so the
/// last whole count stands until it ages out at its readers.
#[cfg(unix)]
pub(crate) fn activity_of_survey(wire: &str, at: u64) -> Option<aterm_metrics::row::ActivityRow> {
    use aterm_metrics::row::{AGENTS_LISTED_MAX, ActivityRow, AgentEntry, Sid, Word};
    let mut lines = wire.lines();
    lines.next().filter(|head| head.starts_with("OK "))?;
    let lines: Vec<&str> = lines.map(|line| line.trim_end_matches('\r')).collect();
    let instance = lines.iter().find_map(|line| line.strip_prefix("instance "));
    let number = |key: &str| {
        instance?
            .split_ascii_whitespace()
            .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<usize>().ok())
    };
    let sent = lines
        .iter()
        .filter(|line| line.starts_with("session "))
        .count();
    if number("instances_read").unwrap_or(1) < number("instances").unwrap_or(1)
        || number("sessions").is_some_and(|claimed| claimed > sent)
    {
        return None;
    }
    let mut row = ActivityRow {
        at,
        ..ActivityRow::default()
    };
    for line in lines
        .iter()
        .filter_map(|line| line.strip_prefix("session "))
    {
        let fields: Vec<(&str, String)> = line
            .split(' ')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k, aterm_codec::pct::decode(v)))
            .collect();
        let raw = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.as_str())
        };
        let field = |key: &str| raw(key).filter(|v| !v.is_empty() && *v != "-");
        if field("state") == Some("exited") {
            continue;
        }
        let needs = raw("needs")?;
        let Some(word) = AgentWord::of_row(field("program_kind"), field("agent"), Some(needs))
        else {
            continue;
        };
        let count = match word {
            AgentWord::Busy => &mut row.busy,
            AgentWord::Idle => &mut row.idle,
            AgentWord::Waiting => &mut row.waiting,
            AgentWord::Walled(_) => &mut row.walled,
            AgentWord::Unknown => &mut row.unknown,
        };
        *count = count.saturating_add(1);
        if row.sessions.len() < AGENTS_LISTED_MAX
            && let Some(sid) = field("sid").and_then(Sid::parse)
            && let Some(word_on_row) = Word::new(word.wire_word())
        {
            row.sessions.push(AgentEntry {
                sid,
                word: word_on_row,
                detail: word.wire_detail().and_then(Word::new),
            });
        }
    }
    Some(row)
}

/// A session's task, as its row's `task=` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TaskWord {
    /// `working`.
    Working,
    /// `done`.
    Done,
    /// `failed`, or `exited` with `task_conclusion=failed`.
    Failed,
    /// `exited`, unfinished but not failed.
    Exited,
    /// `stalled`.
    Stalled,
}

impl TaskWord {
    /// The task a row's `task=` and `task_conclusion=` say; `None` for no task or a word this
    /// build does not know.
    pub(crate) fn of(task: Option<&str>, conclusion: Option<&str>) -> Option<Self> {
        Some(match task? {
            "working" => Self::Working,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "exited" if conclusion == Some("failed") => Self::Failed,
            "exited" => Self::Exited,
            "stalled" => Self::Stalled,
            _ => return None,
        })
    }
}

/// The most agent sessions one Mac's read keeps ([`FleetMac::agent_sessions`]).
pub(crate) const SESSIONS_MAX: usize = 32;

/// One agent session on a fleet peer, as its survey row says it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FleetSession {
    /// Its sid there.
    pub(crate) sid: String,
    /// Its program (`claude`, `codex-alab`), where the row names one.
    pub(crate) program: Option<String>,
    /// What it is doing.
    pub(crate) agent: AgentWord,
    /// Its task, where it has one.
    pub(crate) task: Option<TaskWord>,
    /// Its context left, percent.
    pub(crate) ctx_left_pct: Option<u8>,
    /// Its account's 5-hour usage, percent.
    pub(crate) usage_5h_pct: Option<u8>,
    /// Its account's weekly usage, percent.
    pub(crate) usage_7d_pct: Option<u8>,
}

/// Why a fleet peer did not answer: the reason token the survey's producer typed
/// (`aterm_net::fleet_member::Unreached::reason`), or what the glance itself knows. The glance
/// reads the token and never the prose, which names an address or an age.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Reach {
    /// `no-name`: its name did not resolve.
    NoName,
    /// `nothing-listens`: the connection was refused.
    NothingListens,
    /// `no-answer`: it did not answer in time.
    NoAnswer,
    /// `no-route`: no route to it from this network.
    NoRoute,
    /// `refused-here`: this Mac refused the connection.
    RefusedHere,
    /// `reset`: the connection was reset.
    Reset,
    /// `connect`: another connect failure.
    Connect,
    /// `no-handshake`: it hung up or ran out of time in the handshake.
    NoHandshake,
    /// `handshake`: another handshake failure.
    Handshake,
    /// `no-reply`: it shook hands and sent no reply in time.
    NoReply,
    /// `identity`: it is not the node it should be.
    Identity,
    /// `not-dialled`: the survey did not dial it.
    NotDialled,
    /// `gone`: it is gone off the link.
    Gone,
    /// This Mac's copy reads it silent, last up here at `since_ms` (unix ms).
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "only the copy read, which is unix only, reads a seat silent"
        )
    )]
    Silent {
        /// When it was last read up here.
        since_ms: u64,
    },
    /// This Mac's copy has never read it up.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the copy read, which is unix only, reads a seat never up"
        )
    )]
    NeverUp,
    /// Past the peers one read keeps ([`MACS_MAX`]).
    PastCap,
    /// Its answer had no header.
    NoHeader,
    /// No token this build knows (an older peer sends none): its words stand alone.
    #[default]
    Unread,
}

impl Reach {
    /// The reason a survey row's `reason` token names; [`Self::Unread`] for one this build does
    /// not know.
    pub(crate) fn parse(token: &str) -> Self {
        match token {
            "no-name" => Self::NoName,
            "nothing-listens" => Self::NothingListens,
            "no-answer" => Self::NoAnswer,
            "no-route" => Self::NoRoute,
            "refused-here" => Self::RefusedHere,
            "reset" => Self::Reset,
            "connect" => Self::Connect,
            "no-handshake" => Self::NoHandshake,
            "handshake" => Self::Handshake,
            "no-reply" => Self::NoReply,
            "identity" => Self::Identity,
            "not-dialled" => Self::NotDialled,
            "gone" => Self::Gone,
            _ => Self::Unread,
        }
    }

    /// The producer's token for it; `None` for a reason the glance itself gives: the round trip
    /// [`Self::parse`] keeps, which only a test of the unix producer's tokens reads.
    #[cfg(all(test, unix))]
    pub(crate) const fn token(self) -> Option<&'static str> {
        Some(match self {
            Self::NoName => "no-name",
            Self::NothingListens => "nothing-listens",
            Self::NoAnswer => "no-answer",
            Self::NoRoute => "no-route",
            Self::RefusedHere => "refused-here",
            Self::Reset => "reset",
            Self::Connect => "connect",
            Self::NoHandshake => "no-handshake",
            Self::Handshake => "handshake",
            Self::NoReply => "no-reply",
            Self::Identity => "identity",
            Self::NotDialled => "not-dialled",
            Self::Gone => "gone",
            Self::Silent { .. } | Self::NeverUp | Self::PastCap | Self::NoHeader | Self::Unread => {
                return None;
            }
        })
    }
}

impl FleetMac {
    /// Whether its needs are this Mac's to count, and its sessions its to hold and hand back.
    pub(crate) const fn counts(&self) -> bool {
        !self.older && !self.unproven
    }

    /// Whether its needs count on the badge at `now_ms`: its rows were read no longer than
    /// [`COUNT_FRESH`] ago, and the read is not ahead of the current clock.
    pub(crate) fn fresh(&self, now_ms: u64) -> bool {
        now_ms
            .checked_sub(self.read_at_ms)
            .is_some_and(|age| age <= millis(COUNT_FRESH))
    }
}

/// One fleet peer that did not answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Unanswered {
    /// How rows name it ([`short_name`]).
    pub(crate) name: String,
    /// The name the book knows it by ([`dial_of`]).
    pub(crate) dial: String,
    /// Why, in the fleet client's words.
    pub(crate) why: String,
    /// Why, as a reason the glance reads ([`Reach`]).
    pub(crate) reason: Reach,
    /// Its build, where this Mac's copy read one.
    pub(crate) build: Option<String>,
    /// When it was last read up (unix ms), where this Mac's copy knows.
    pub(crate) silent_since_ms: Option<u64>,
    /// Its membership log label (`m<N>`), where this Mac's copy read it.
    pub(crate) label: Option<String>,
    /// `aterm fleet` says it needs an update
    /// ([`aterm_agent::fleet_view::SeatRow::needs_update`]); read on this Mac's copy only.
    pub(crate) needs_update: bool,
}

/// One fleet read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FleetSnapshot {
    /// When it was read (unix milliseconds).
    pub(crate) read_at_ms: u64,
    /// The peers that answered, in the read's order.
    pub(crate) macs: Vec<FleetMac>,
    /// The peers that did not, and why.
    pub(crate) unanswered: Vec<Unanswered>,
    /// The lab's members and how many are up, this Mac's seat among them, where the read was
    /// this Mac's copy ([`snapshot_of`]): `status`'s `members=`. It names no update and no log
    /// fill (`aterm fleet` reads those).
    pub(crate) seats: Option<aterm_netplace::words::Seats>,
}

impl FleetSnapshot {
    /// Whether the read itself is no older than [`COUNT_FRESH`] at `now_ms`, without a future
    /// timestamp (each Mac's own rows: [`FleetMac::fresh`]).
    pub(crate) fn fresh(&self, now_ms: u64) -> bool {
        now_ms
            .checked_sub(self.read_at_ms)
            .is_some_and(|age| age <= millis(COUNT_FRESH))
    }

    /// The Macs that answered this read (a [`FleetMac::carried`] one did not).
    pub(crate) fn answered(&self) -> usize {
        self.macs.iter().filter(|m| !m.carried).count()
    }

    /// The agents it read on the Macs that answered it and count ([`FleetMac::counts`]); `None`
    /// where no such Mac's activity was read (an older fleet; on this Mac's copy, no Mac's fresh
    /// `now/activity` row).
    pub(crate) fn agents(&self) -> Option<usize> {
        self.macs
            .iter()
            .filter(|m| !m.carried && m.counts())
            .filter_map(|m| m.activity)
            .map(|tally| usize::from(tally.agents))
            .reduce(|a, b| a + b)
    }

    /// The first instant after `now_ms` (unix ms) at which the read, or one Mac's rows in it,
    /// starts or stops counting. A read ahead of a corrected clock starts at its timestamp.
    pub(crate) fn next_freshness_change(&self, now_ms: u64) -> Option<u64> {
        std::iter::once(self.read_at_ms)
            .chain(self.macs.iter().map(|m| m.read_at_ms))
            .flat_map(|at| [at, at.saturating_add(millis(COUNT_FRESH)).saturating_add(1)])
            .filter(|change| *change > now_ms)
            .min()
    }
}

/// A READ CARRIES A SILENT MAC'S LAST ROWS while they still count.
pub(crate) fn carry(
    previous: Option<&FleetSnapshot>,
    mut next: FleetSnapshot,
    now_ms: u64,
) -> FleetSnapshot {
    let Some(previous) = previous else {
        return next;
    };
    let room = MACS_MAX.saturating_sub(next.macs.len());
    let carried: Vec<FleetMac> = next
        .unanswered
        .iter()
        .filter(|silent| !next.macs.iter().any(|m| m.dial == silent.dial))
        .filter_map(|silent| {
            previous
                .macs
                .iter()
                .find(|m| m.dial == silent.dial && m.fresh(now_ms))
        })
        .map(|last| FleetMac {
            carried: true,
            ..last.clone()
        })
        .take(room)
        .collect();
    next.macs.extend(carried);
    next
}

/// A span in milliseconds.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `name` when it can stand in a `vnc://` URL or an `ssh` line: letters, digits, `.`, `-` and `_`, starting with a letter or digit.
pub(crate) fn host_word(name: &str) -> Option<String> {
    let ok = !name.is_empty()
        && name.len() <= 253
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    ok.then(|| name.to_string())
}

/// THE NAME THE BOOK KNOWS A PEER BY, from its survey label.
pub(crate) fn dial_of(label: &str) -> &str {
    label.split_once(" at ").map_or(label, |(name, _)| name)
}

/// `dial`'s host: without a trailing `:port`; an address whole.
fn host_part(dial: &str) -> &str {
    if dial.parse::<std::net::IpAddr>().is_ok() {
        return dial;
    }
    let host = match dial.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => dial,
    };
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .filter(|h| h.parse::<std::net::Ipv6Addr>().is_ok())
        .unwrap_or(host)
}

/// How a row names the peer the book knows as `dial`: its host's first label (`mac-17` for `mac-17.local`), an address whole.
pub(crate) fn short_name(dial: &str) -> String {
    let host = host_part(dial);
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host.to_string();
    }
    host.split('.')
        .next()
        .filter(|first| !first.is_empty())
        .unwrap_or(host)
        .to_string()
}

/// The `aterm` argv of a fleet row's read: `ctl survey --host <dial>`, and `--sid <sid>` for one session.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) fn fleet_read_args(dial: &str, sid: Option<&str>) -> Option<Vec<String>> {
    if dial.is_empty() || dial.starts_with('-') || dial.chars().any(char::is_whitespace) {
        return None;
    }
    let mut args: Vec<String> = ["ctl", "survey", "--host", dial]
        .map(str::to_string)
        .to_vec();
    if let Some(sid) = sid {
        if !sid.starts_with("s-") || sid.chars().any(char::is_whitespace) {
            return None;
        }
        args.extend(["--sid".to_string(), sid.to_string()]);
    }
    Some(args)
}

/// The argv after the program of a fleet need's Take Over (`on`) or Hand Back on the Mac the book knows as `dial`.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) fn fleet_hold_args(dial: &str, sid: &str, on: bool) -> Option<Vec<String>> {
    let plain = |word: &str| {
        !word.is_empty() && !word.starts_with('-') && !word.chars().any(char::is_whitespace)
    };
    if !plain(dial) || !sid.starts_with("s-") || !plain(sid) {
        return None;
    }
    Some(
        [
            "ctl",
            "--host",
            dial,
            "hold",
            sid,
            if on { "on" } else { "off" },
        ]
        .map(str::to_string)
        .to_vec(),
    )
}

/// A member of a JSON object as a string, `None` for `-`, empty or absent.
fn field<'a>(object: &'a aterm_json::Map, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(aterm_json::Value::as_str)
        .filter(|v| !v.is_empty() && *v != "-")
}

/// One fleet peer's answer, labelled `label`: its header and its session
/// rows, read by the Mac whose node is `own_node`; `unproven` where it proved
/// no node key this Mac binds, whose node, host and drive are then its claim
/// and not taken ([`FleetMac::unproven`]).
fn mac_of(
    label: &str,
    instance: &aterm_json::Map,
    sessions: &[aterm_json::Value],
    own_node: Option<&str>,
    unproven: bool,
) -> FleetMac {
    let sent: Vec<&aterm_json::Map> = sessions
        .iter()
        .filter_map(aterm_json::Value::as_object)
        .collect();
    let rows: Vec<&aterm_json::Map> = sent
        .iter()
        .copied()
        .filter(|row| field(row, "state") != Some("exited"))
        .collect();
    let older = rows.iter().any(|row| !row.contains_key("needs"));
    let content = field(instance, "level") == Some("content");
    let label_of = |row: &aterm_json::Map| {
        field(row, "role")
            .or_else(|| field(row, "program"))
            .map_or_else(
                || crate::presence::short_sid(field(row, "sid").unwrap_or("")),
                str::to_string,
            )
    };
    let dial = dial_of(label);
    let proven = |key: &str| (!unproven).then(|| field(instance, key)).flatten();
    let count = |key: &str| {
        field(instance, key)
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(1)
    };
    let mut mac = FleetMac {
        name: short_name(dial),
        dial: dial.to_string(),
        node: proven("node").map(str::to_ascii_lowercase),
        host: proven("host")
            .and_then(host_word)
            .or_else(|| host_word(host_part(dial))),
        content,
        drive: proven("drive") == Some("1"),
        older,
        unproven,
        sessions: Some(rows.len()),
        build: field(instance, "version").map(str::to_owned),
        // A node service answers for every window it reaches (`instances_read=`); a window that
        // answered alone read itself.
        instances_read: count("instances_read"),
        instances_unread: count("instances").saturating_sub(count("instances_read")),
        ..FleetMac::default()
    };
    let claimed = field(instance, "sessions")
        .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse::<usize>().ok());
    mac.left_out = claimed.map_or(0, |n| n.saturating_sub(sent.len()));
    let mut tally = AgentTally::default();
    for row in &rows {
        let agent = field(row, "program_kind") == Some("agent");
        let needs = row.get("needs").and_then(aterm_json::Value::as_str);
        if let Some(word) =
            AgentWord::of_row(field(row, "program_kind"), field(row, "agent"), needs)
        {
            tally.add(&word);
            if mac.agent_sessions.len() < SESSIONS_MAX {
                let pct = |key: &str| field(row, key).and_then(|n| n.parse::<u8>().ok());
                mac.agent_sessions.push(FleetSession {
                    sid: field(row, "sid").unwrap_or("").to_owned(),
                    program: field(row, "program").map(str::to_owned),
                    agent: word,
                    task: TaskWord::of(field(row, "task"), field(row, "task_conclusion")),
                    ctx_left_pct: pct("ctx_left_pct"),
                    usage_5h_pct: pct("usage_5h_pct"),
                    usage_7d_pct: pct("usage_7d_pct"),
                });
            } else {
                mac.sessions_more += 1;
            }
        }
        if let Some(from) = field(row, "task_from").filter(|f| f.starts_with("s-")) {
            match mac.managers.iter_mut().find(|m| m.sid == from) {
                Some(manager) => manager.workers += 1,
                None => {
                    let label = rows
                        .iter()
                        .find(|r| field(r, "sid") == Some(from))
                        .map_or_else(|| crate::presence::short_sid(from), |r| label_of(r));
                    mac.managers.push(FleetManager {
                        label,
                        sid: from.to_string(),
                        workers: 1,
                    });
                }
            }
        }
        if older {
            continue;
        }
        let Some(sid) = field(row, "sid") else {
            continue;
        };
        let content_field = |key: &str| content.then(|| field(row, key)).flatten();
        let who = crate::status_item::tab_name(
            field(row, "role"),
            content_field("user_title"),
            content_field("title").unwrap_or(""),
        )
        .unwrap_or_else(|| crate::needs::fold_clip(&label_of(row), 40));
        let held_from = field(row, "held_from").map(str::to_ascii_lowercase);
        if own_node.is_some() && held_from.as_deref() == own_node {
            mac.held_here.push(FleetHeld {
                sid: sid.to_string(),
                who: who.clone(),
            });
        }
        let Some(need) = field(row, "needs").and_then(NeedWord::parse) else {
            continue;
        };
        let age_key = match need {
            NeedWord::Stalled => Some("task_ms"),
            NeedWord::Ask => None,
            NeedWord::Frozen | NeedWord::Stopped if !agent => None,
            _ => Some("agent_ms"),
        };
        let claim = (field(row, "needs_by") == Some("program"))
            .then(|| crate::needs::claim_words_of_row(need, field(row, "agent")))
            .flatten();
        mac.needs.push(FleetNeed {
            sid: sid.to_string(),
            word: need,
            who,
            worker_of: field(row, "task_from")
                .filter(|f| f.starts_with("s-"))
                .map(crate::presence::short_sid),
            age_ms: age_key
                .and_then(|key| field(row, key))
                .filter(|ms| ms.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|ms| ms.parse().ok()),
            what: claim
                .map(str::to_string)
                .or_else(|| content.then(|| content_what(need, row)).flatten()),
            claim,
            carries_on: field(row, "needs_by") == Some("harness"),
        });
    }
    mac.activity = (!older).then_some(tally);
    mac
}

/// A content peer's words for its row's need `word`, as a local row has
/// them (§3: "titles and words appear only where that Mac's `[fleet]
/// allow` is content"): an ask's own text — the row's attention excerpt is
/// the entry the session is ranked by, the ask itself — a box's kind
/// (`agent_detail`), a wall's kind and the reset it names.
fn content_what(word: NeedWord, row: &aterm_json::Map) -> Option<String> {
    use crate::needs::{HarnessAsk, ROW_WHAT_MAX, fold_clip, need_words};
    match word {
        NeedWord::Ask => field(row, "attention")
            .map(|text| text.trim().trim_start_matches('\u{26a0}').trim_start())
            .map(|text| {
                need_words(text).map_or_else(
                    || fold_clip(text, ROW_WHAT_MAX),
                    |words| HarnessAsk::of(words).line(ROW_WHAT_MAX),
                )
            })
            .filter(|what| !what.is_empty()),
        NeedWord::Approval | NeedWord::Question => {
            let detail = field(row, "agent_detail");
            Some(crate::presence::prompt_band_word(detail))
        }
        NeedWord::Login | NeedWord::Memory | NeedWord::Wall => {
            let kind = field(row, "agent")?.strip_prefix("wall:")?;
            Some(crate::needs::wall_what(kind, field(row, "agent_detail")))
        }
        NeedWord::Frozen | NeedWord::Stopped | NeedWord::Stalled => None,
    }
}

/// THE FLEET READ, PARSED.
pub(crate) fn parse(
    json: &str,
    own_node: Option<&str>,
    now_ms: u64,
) -> Result<FleetSnapshot, String> {
    let value: aterm_json::Value =
        aterm_json::from_str(json).map_err(|e| format!("the fleet read is no survey: {e}"))?;
    let hosts = value
        .as_array()
        .ok_or_else(|| "the fleet read is no list".to_string())?;
    let mut snapshot = FleetSnapshot {
        read_at_ms: now_ms,
        ..FleetSnapshot::default()
    };
    for host in hosts {
        let Some(host) = host.as_object() else {
            continue;
        };
        if host.get("fleet").and_then(aterm_json::Value::as_bool) != Some(true) {
            continue;
        }
        let label = field(host, "label").unwrap_or("a peer").to_string();
        let unanswered = |why: &str, reason: Reach| Unanswered {
            name: short_name(dial_of(&label)),
            dial: dial_of(&label).to_string(),
            why: why.to_string(),
            reason,
            ..Unanswered::default()
        };
        if let Some(why) = field(host, "error") {
            // The row that stands for a refused read of the whole fleet names no Mac: the read
            // failed, and no seat is silent.
            if field(host, "reason") == Some(aterm_ctl::FLEET_UNREAD) {
                return Err(crate::needs::fold_clip(
                    &format!("fleet not read: {why}"),
                    120,
                ));
            }
            let reason = field(host, "reason").map_or(Reach::Unread, Reach::parse);
            snapshot.unanswered.push(unanswered(why, reason));
            continue;
        }
        let Some(instance) = host.get("instance").and_then(aterm_json::Value::as_object) else {
            snapshot
                .unanswered
                .push(unanswered("its answer had no header", Reach::NoHeader));
            continue;
        };
        let unproven = host.get("unproven").and_then(aterm_json::Value::as_bool) == Some(true);
        if own_node.is_some() && !unproven && field(instance, "node") == own_node {
            continue;
        }
        if snapshot.macs.len() >= MACS_MAX {
            snapshot
                .unanswered
                .push(unanswered("past the peers one read keeps", Reach::PastCap));
            continue;
        }
        let sessions = host
            .get("sessions")
            .and_then(aterm_json::Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        snapshot.macs.push(FleetMac {
            read_at_ms: now_ms,
            ..mac_of(&label, instance, sessions, own_node, unproven)
        });
    }
    Ok(snapshot)
}

/// What this Mac's fleet files say of it, read off the event loop ([`read_book`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BookFacts {
    /// It holds a fleet key: it is in a fleet.
    pub(crate) in_fleet: bool,
    /// It minted the fleet's code (`aterm fleet init`). Joined peer rows no longer
    /// establish this local fact after F17; it never overrides the watch preference.
    pub(crate) minted: bool,
    /// The issuer a member's book names, where it is all the book holds and no copy of that issuer's book was read.
    pub(crate) issuer_label: Option<String>,
    /// The issuer a member's book names with no key bound to it (it joined before node keys came, or its issuer proved none then).
    pub(crate) unbound_issuer: Option<String>,
}

/// WHETHER THIS MAC IS IN A FLEET, the one answer every surface reads (the fleet glance's book here, the Agents rows' and Summarize's `summarize::in_fleet`).
pub(crate) fn holds_fleet_key(state: &Path) -> bool {
    std::fs::symlink_metadata(state.join(aterm_net::fleet::KEY_FILE)).is_ok()
}

/// [`BookFacts`] of the state directory `state` (`fleet.key` and the book of peers).
pub(crate) fn read_book(state: &Path) -> BookFacts {
    let in_fleet = holds_fleet_key(state);
    if !in_fleet {
        return BookFacts::default();
    }
    read_book_peers(state, in_fleet)
}

/// [`read_book`] off unix, where there is no book to read
/// (`aterm_net::fleet_member` is unix-only): a fleet key says only that it is
/// in a fleet — and no reader starts off macOS anyway
/// (`App::fleet_glance_may_start`).
#[cfg(not(unix))]
fn read_book_peers(_state: &Path, in_fleet: bool) -> BookFacts {
    BookFacts {
        in_fleet,
        ..BookFacts::default()
    }
}

/// [`read_book`]'s book: whether this Mac minted the fleet's code, or its
/// book names only its issuer — and then, since the issuer's book (S11) makes a
/// member's read whole, whether the last read got that book
/// ([`aterm_net::fleet_member::holds_issuer_book`]): a member holding it
/// knows the whole fleet, one without it (its issuer an older aterm that
/// shares none, or no read yet) only its issuer — and whether its issuer's
/// entry binds a key ([`BookFacts::unbound_issuer`]).
#[cfg(unix)]
fn read_book_peers(state: &Path, in_fleet: bool) -> BookFacts {
    let book = aterm_net::fleet_member::read_peers(state).unwrap_or_default();
    let minted = std::fs::symlink_metadata(state.join(aterm_net::fleet_member::CODE_FILE))
        .is_ok_and(|meta| meta.is_file());
    let issuer_label = book
        .iter()
        .all(|p| p.via == aterm_net::fleet_member::Via::Issuer)
        .then(|| book.first())
        .flatten()
        .filter(|first| {
            !first
                .node
                .as_deref()
                .is_some_and(|node| aterm_net::fleet_member::holds_issuer_book(state, node))
        })
        .map(|first| first.host.clone());
    let unbound_issuer = book
        .iter()
        .find(|p| p.via == aterm_net::fleet_member::Via::Issuer)
        .filter(|p| p.key.is_none())
        .map(|p| p.host.clone());
    BookFacts {
        in_fleet,
        minted,
        issuer_label,
        unbound_issuer,
    }
}

/// WHEN TO READ THE FLEET: never in no fleet; on its own when `watch`, every `every` ([`Quiet::every`]).
pub(crate) fn should_poll(
    book: &BookFacts,
    watch: bool,
    last_ms: Option<u64>,
    now_ms: u64,
    menu_opening: bool,
    every: Duration,
) -> bool {
    if !book.in_fleet {
        return false;
    }
    let older_than = |span: Duration| {
        last_ms
            .and_then(|at| now_ms.checked_sub(at))
            .is_none_or(|age| age >= millis(span))
    };
    (menu_opening && older_than(MENU_FRESH)) || (watch && older_than(every))
}

/// THE READ'S BACKOFF WHILE AWAY (docs/DESIGN-fluid-swarm-2026-10-28.md F4): a read that reached
/// no peer of those it dialled (a Mac on a hotspot, every peer dark) doubles the wait before the
/// next, from [`POLL_EVERY`] up to [`QUIET_MAX`]; a read that reached one, and a move of this
/// Mac's network ([`Kick::Moved`]), start it again from [`POLL_EVERY`]. Away from the ALab LAN a
/// read that reached no peer waits [`QUIET_MAX`] at once: the doubling would dial members behind
/// the ALab LAN five more times for nothing, and the move back, which ends the wait, reads at
/// once. A read that failed says nothing of the peers and leaves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Quiet {
    every: Duration,
}

impl Default for Quiet {
    fn default() -> Self {
        Self { every: POLL_EVERY }
    }
}

impl Quiet {
    /// The wait before the next read of its own.
    pub(crate) const fn every(self) -> Duration {
        self.every
    }

    /// A read answered `read`, `away` from the ALab LAN or not: whether the wait changed.
    pub(crate) fn after(&mut self, read: &Result<FleetSnapshot, String>, away: bool) -> bool {
        let before = self.every;
        match read {
            Ok(read) if read.answered() == 0 && !read.unanswered.is_empty() && away => {
                self.every = QUIET_MAX;
            }
            Ok(read) if read.answered() == 0 && !read.unanswered.is_empty() => {
                self.every = self.every.saturating_mul(2).min(QUIET_MAX);
            }
            Ok(_) => self.every = POLL_EVERY,
            Err(_) => {}
        }
        self.every != before
    }

    /// This Mac's network moved: the wait starts again.
    pub(crate) fn moved(&mut self) {
        self.every = POLL_EVERY;
    }
}

/// The shared slot the reader writes and the status item reads.
#[derive(Clone, Debug, Default)]
pub(crate) struct FleetSlot {
    /// Moves with every snapshot written: a Fleet submenu click names it.
    pub(crate) generation: u16,
    /// The last good read.
    pub(crate) snapshot: Option<Arc<FleetSnapshot>>,
    /// The read before it, and its generation: a click on a submenu painted from it still resolves.
    pub(crate) previous: Option<(u16, Arc<FleetSnapshot>)>,
    /// Why the last read failed, when it did (cleared by a good one).
    pub(crate) error: Option<String>,
    /// A read is in flight.
    pub(crate) reading: bool,
    /// What the book said at the last look.
    pub(crate) book: BookFacts,
    /// When the last read was tried (unix ms), good or not.
    pub(crate) tried_ms: Option<u64>,
}

impl FleetSlot {
    /// The snapshot a click with `generation` was painted from.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn snapshot_at(&self, generation: u16) -> Option<&Arc<FleetSnapshot>> {
        if self.generation == generation {
            return self.snapshot.as_ref();
        }
        self.previous
            .as_ref()
            .filter(|(g, _)| *g == generation)
            .map(|(_, s)| s)
    }

    /// `chrome`'s `fleet=` word.
    pub(crate) fn word(&self) -> String {
        if !self.book.in_fleet {
            "off".to_string()
        } else if self.reading {
            "reading".to_string()
        } else if self.error.is_some() {
            "failed".to_string()
        } else {
            self.snapshot.as_ref().map_or_else(
                || "unread".to_string(),
                |s| format!("as_of:{}", s.read_at_ms),
            )
        }
    }
}

/// The read itself, injected so a test drives the thread with no child.
pub(crate) trait FleetRunner: Send {
    /// Read the fleet once: every Mac but this one (`own_node`, the survey's), its rows as of
    /// `now_ms()` once the answer is in.
    fn read(
        &mut self,
        own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String>;

    /// Whether a read dials the fleet's Macs (the survey), so the LAN door
    /// ([`ReaderSeams::lan`]), the backoff ([`Quiet`]) and `[fleet] watch` govern it. A read of
    /// this Mac's own copy ([`CopyRunner`]) dials nothing, so it reads on the minute whatever
    /// they say.
    fn dials(&self) -> bool {
        true
    }
}

/// THE REAL READ: `<program> <args>` as a child, SIGKILLed at `cap`.
pub(crate) struct ChildRunner {
    /// The `aterm` binary (`ledger_key::aterm_cli`).
    pub(crate) program: PathBuf,
    /// `ctl survey --fleet --json --timeout 3`.
    pub(crate) args: Vec<String>,
    /// The kill.
    pub(crate) cap: Duration,
}

impl ChildRunner {
    /// The fleet read the status item makes.
    pub(crate) fn fleet() -> Self {
        Self {
            program: crate::ledger_key::aterm_cli(),
            args: ["ctl", "survey", "--fleet", "--json", "--timeout", "3"]
                .map(str::to_string)
                .to_vec(),
            cap: READ_CAP,
        }
    }
}

impl FleetRunner for ChildRunner {
    fn read(
        &mut self,
        own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String> {
        let ran = crate::bounded_child::run_bounded(&self.program, &self.args, Some(self.cap))?;
        if ran.stdout.trim().is_empty() {
            let why = ran.stderr.lines().next().unwrap_or("no answer").to_string();
            return Err(crate::needs::fold_clip(&why, 120));
        }
        parse(&ran.stdout, own_node, now_ms())
    }
}

/// THE MESH READ (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.12, §4.14, §11 B9): this Mac's
/// own copy as its node service holds it, each current seat's standing, presence row and
/// now/needs row from the node's view socket ([`aterm_mesh::view::ask`]), each named by this
/// Mac's book, and composed with this Mac's membership log as `aterm fleet` composes it
/// ([`aterm_agent::fleet_view::compose_here`]). It dials no Mac and starts no child.
#[cfg(unix)]
pub(crate) struct CopyRunner {
    /// The state directory: `<state>/mesh/` holds the node's socket, `<state>` the book.
    pub(crate) state: PathBuf,
    /// This Mac's vault, whose membership log labels the seats and whose records say who needs
    /// an update; `None` reads no log (every seat unlabelled, none needing an update by it).
    pub(crate) paths: Option<aterm_secrets::client::Paths>,
}

#[cfg(unix)]
impl FleetRunner for CopyRunner {
    fn read(
        &mut self,
        _own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String> {
        let view = aterm_mesh::view::ask(
            &self.state.join(aterm_mesh::logs::MESH_DIR),
            aterm_mesh::view::WITHIN,
        )
        .map_err(|e| e.to_string())?;
        // Before its node read the standings a view holds this Mac's seat alone; without its
        // needs rows every up seat would read as having none, and Needs you would lose what
        // they still need. Either is a read that failed, which keeps the last good one.
        if !view.standings {
            return Err("the node has not read the lab's membership standings yet".to_owned());
        }
        if view.needs.is_none() {
            return Err("the node could not read the needs rows".to_owned());
        }
        let fleet =
            aterm_agent::fleet_view::compose_here(&self.state, self.paths.as_ref(), Ok(&view));
        let book = aterm_net::fleet_member::read_peers(&self.state).unwrap_or_default();
        Ok(snapshot_of(&view, &fleet, &book, now_ms()))
    }

    fn dials(&self) -> bool {
        false
    }
}

/// The glance's read of a node's `view` at `now_ms`, each seat named by the host its node key's
/// entry in `book` gives (else its node id) and joined to its row of `fleet` (`aterm fleet`'s
/// composition of the same view) by its node id: its log label, build, update and tables. An up
/// seat is a Mac with its now/needs row's needs, holds this Mac placed and managers (no titles
/// leave a Mac, so a session is its short sid), and its agents' tally and sessions by sid and word
/// where its now/activity row is fresh; a silent one, and a seat of the log the view does
/// not hold, a Mac that did not answer, with how long it has been silent. This Mac's own seat is
/// left out, as the survey leaves out its own answer.
#[cfg(unix)]
pub(crate) fn snapshot_of(
    view: &aterm_mesh::view::View,
    fleet: &aterm_agent::fleet_view::Fleet,
    book: &[aterm_net::fleet_member::Peer],
    now_ms: u64,
) -> FleetSnapshot {
    use aterm_mesh::presence::Standing;
    let booked = |key: &[u8; 32]| {
        book.iter()
            .find(|peer| peer.key.is_some_and(|k| k.bytes() == key))
            .map(|peer| peer.host.as_str())
    };
    let row_of = |principal: &str| {
        fleet
            .rows
            .iter()
            .find(|row| row.node.as_deref() == Some(principal))
    };
    let label_of =
        |row: &aterm_agent::fleet_view::SeatRow| (row.label != "-").then(|| row.label.clone());
    let mut snapshot = FleetSnapshot {
        read_at_ms: now_ms,
        seats: Some(fleet.seats.clone()),
        ..FleetSnapshot::default()
    };
    for seat in view.seats.iter().filter(|seat| !seat.me) {
        let host = booked(&seat.key);
        let dial = host.map_or_else(|| seat.principal.clone(), str::to_owned);
        let name = host.map_or_else(|| seat.principal.clone(), short_name);
        let seat_row = row_of(&seat.principal);
        let label = seat_row.and_then(label_of);
        let presence = seat.row.as_ref().map(|(_, row)| row);
        let build = seat_row
            .and_then(|row| row.build.clone())
            .or_else(|| presence.map(|row| row.build.as_str().to_owned()));
        let Standing::Up = seat.standing else {
            let (why, reason, since) = match seat.standing {
                Standing::Silent {
                    last_seen_ms: Some(seen),
                } => (
                    format!(
                        "silent for {}",
                        aterm_netplace::words::age_words(now_ms.saturating_sub(seen))
                    ),
                    Reach::Silent { since_ms: seen },
                    Some(seen),
                ),
                _ => (
                    "silent, never seen up here".to_owned(),
                    Reach::NeverUp,
                    None,
                ),
            };
            snapshot.unanswered.push(Unanswered {
                name,
                dial,
                why,
                reason,
                build,
                silent_since_ms: since,
                label,
                needs_update: seat_row.is_some_and(|row| row.needs_update),
            });
            continue;
        };
        if snapshot.macs.len() >= MACS_MAX {
            snapshot.unanswered.push(Unanswered {
                name,
                dial,
                why: "past the peers one read keeps".to_owned(),
                reason: Reach::PastCap,
                build,
                silent_since_ms: None,
                label,
                needs_update: seat_row.is_some_and(|row| row.needs_update),
            });
            continue;
        }
        let needs = view
            .needs
            .as_ref()
            .and_then(|needs| needs.get(&seat.principal));
        let mut mac = FleetMac {
            name,
            dial,
            node: Some(seat.principal.clone()),
            host: host.and_then(host_word).or_else(|| {
                presence
                    .and_then(|row| row.addresses.iter().find(|at| at.is_ipv4()))
                    .map(|at| at.ip().to_string())
            }),
            instances_read: 1,
            read_at_ms: now_ms,
            build,
            label,
            needs_update: seat_row.is_some_and(|row| row.needs_update),
            tables: seat_row.and_then(|row| row.behind),
            ..FleetMac::default()
        };
        if let Some(needs) = needs {
            let age_ms =
                |since: Option<u64>| since.map(|s| now_ms.saturating_sub(s.saturating_mul(1000)));
            mac.needs = needs
                .needs
                .iter()
                .filter_map(|need| {
                    let sid = need.sid.to_string();
                    Some(FleetNeed {
                        word: NeedWord::parse(need.word.as_str())?,
                        who: crate::presence::short_sid(&sid),
                        worker_of: need
                            .worker_of
                            .as_ref()
                            .map(|from| crate::presence::short_sid(&from.to_string())),
                        age_ms: age_ms(need.since),
                        what: None,
                        claim: None,
                        carries_on: need.harness,
                        sid,
                    })
                })
                .collect();
            mac.held_here = needs
                .held
                .iter()
                .filter(|held| held.by.is_some_and(|by| by.to_string() == view.me))
                .map(|held| {
                    let sid = held.sid.to_string();
                    FleetHeld {
                        who: crate::presence::short_sid(&sid),
                        sid,
                    }
                })
                .collect();
            mac.managers = needs
                .managers
                .iter()
                .map(|manager| {
                    let sid = manager.sid.to_string();
                    FleetManager {
                        label: crate::presence::short_sid(&sid),
                        workers: usize::try_from(manager.workers).unwrap_or(usize::MAX),
                        sid,
                    }
                })
                .collect();
        }
        // Its activity row counts where it is fresh against the seat's own presence row, both
        // of its clock: a Mac whose windows went unread, or that runs a build that publishes
        // none any more, is not read rather than shown as its last count.
        let behind_ms = |row: &aterm_metrics::row::ActivityRow| {
            presence.map(|at| at.at_ms.saturating_sub(row.at.saturating_mul(1000)))
        };
        let activity = match view.activity.as_ref() {
            None => Err(ActivityAbsent::Unread),
            Some(activity) => match activity.get(&seat.principal) {
                None => Err(ActivityAbsent::Unpublished),
                Some(row) => match behind_ms(row) {
                    Some(behind) if behind <= aterm_metrics::row::ACTIVITY_FRESH_MS => Ok(row),
                    behind_ms => Err(ActivityAbsent::Stale { behind_ms }),
                },
            },
        };
        mac.activity_absent = activity.err();
        if let Ok(row) = activity {
            mac.activity = Some(AgentTally::of_row(row));
            mac.agent_sessions = row
                .sessions
                .iter()
                .take(SESSIONS_MAX)
                .map(|entry| FleetSession {
                    sid: entry.sid.to_string(),
                    program: None,
                    agent: AgentWord::of_wire(
                        entry.word.as_str(),
                        entry.detail.as_ref().map(aterm_metrics::row::Word::as_str),
                    ),
                    task: None,
                    ctx_left_pct: None,
                    usage_5h_pct: None,
                    usage_7d_pct: None,
                })
                .collect();
            mac.sessions_more = usize::try_from(row.agents())
                .unwrap_or(usize::MAX)
                .saturating_sub(mac.agent_sessions.len());
        }
        snapshot.macs.push(mac);
    }
    // A seat of this Mac's log that the view does not hold (it bound no node key, or the node
    // has not read it): never read up here, and counted, so every seat is a Mac.
    for row in fleet.rows.iter().filter(|row| !row.me) {
        let held = row
            .node
            .as_deref()
            .is_some_and(|node| view.seats.iter().any(|seat| seat.principal == node));
        if held {
            continue;
        }
        let dial = if row.name == "-" {
            row.node.clone().unwrap_or_else(|| row.label.clone())
        } else {
            row.name.clone()
        };
        snapshot.unanswered.push(Unanswered {
            name: short_name(&dial),
            dial,
            why: "silent, never seen up here".to_owned(),
            reason: Reach::NeverUp,
            build: row.build.clone(),
            silent_since_ms: None,
            label: label_of(row),
            needs_update: row.needs_update,
        });
    }
    snapshot
}

/// The vault a copy read composes with; the vault client, and with it the copy path, is unix
/// only, so elsewhere there is no vault to hand over.
#[cfg(unix)]
pub(crate) type VaultPaths = aterm_secrets::client::Paths;
/// Off unix the glance only surveys, and no vault is read.
#[cfg(not(unix))]
pub(crate) type VaultPaths = std::convert::Infallible;

/// The glance's read where `mesh` (`aterm_types::mesh_switch::MESH`) says whether this Mac's
/// node holds the lab's copy: this Mac's own copy under the state directory `state`
/// ([`CopyRunner`], composed with the vault at `paths`), else the survey that dials every Mac in
/// the book ([`ChildRunner::fleet`]), until wave D removes it.
pub(crate) fn runner(
    mesh: bool,
    state: Option<PathBuf>,
    paths: Option<VaultPaths>,
) -> Box<dyn FleetRunner> {
    #[cfg(unix)]
    if let (true, Some(state)) = (mesh, state) {
        return Box::new(CopyRunner { state, paths });
    }
    #[cfg(not(unix))]
    let _ = (mesh, state, paths);
    Box::new(ChildRunner::fleet())
}

/// What the reader thread is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kick {
    /// The status menu is opening: read if the last read is stale.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    MenuOpening,
    /// Read the Fleet Now: read whatever the last read's age.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    Now,
    /// This Mac's network moved: the read's backoff ends ([`Quiet::moved`]), and a Mac that reads
    /// on its own reads now.
    Moved,
}

/// The kicks the reader holds at one look ([`reader_loop`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Asked {
    /// A [`Kick::Now`].
    now: bool,
    /// A [`Kick::MenuOpening`].
    menu: bool,
    /// A [`Kick::Moved`].
    moved: bool,
}

impl Asked {
    /// `first`, if any, and every kick queued behind it in `rx`.
    fn with_queued(first: Option<Kick>, rx: &Receiver<Kick>) -> Self {
        let mut asked = Self::default();
        for kick in first
            .into_iter()
            .chain(std::iter::from_fn(|| rx.try_recv().ok()))
        {
            match kick {
                Kick::Now => asked.now = true,
                Kick::MenuOpening => asked.menu = true,
                Kick::Moved => asked.moved = true,
            }
        }
        asked
    }
}

/// The reader thread's inputs besides its runner.
pub(crate) struct ReaderSeams {
    /// The book's facts at each look.
    pub(crate) book: Box<dyn Fn() -> BookFacts + Send>,
    /// `[fleet] watch`, current.
    pub(crate) watch: Arc<std::sync::atomic::AtomicBool>,
    /// This machine's node id, to drop its own answer and to know the holds its drives placed ([`FleetMac::held_here`]).
    pub(crate) own_node: Option<String>,
    /// Now, unix ms.
    pub(crate) now_ms: Box<dyn Fn() -> u64 + Send>,
    /// Told after every write to the slot (`Wake::FleetRead`); `false` when nobody listens any more, which ends the thread.
    pub(crate) post: Box<dyn Fn() -> bool + Send>,
    /// How long the thread waits for a kick before it looks again.
    pub(crate) tick: Duration,
    /// This process's "network moved" signal, each move a [`Kick::Moved`]; none in a test that
    /// kicks by hand.
    pub(crate) moved: Option<crate::network_moved::Watch>,
    /// Whether aterm's own LAN traffic can go out now ([`crate::network_moved::lan_answers`]: on
    /// macOS the root LAN broker answers): a read dials the fleet's Macs, so none goes by itself
    /// (the tick, a menu, a move) while it cannot. A person's Read the Fleet Now still reads, and
    /// its unanswered rows carry the broker's words. It is asked only for a read the tick would
    /// make ([`should_poll`]), right before it.
    pub(crate) lan: Box<dyn Fn() -> bool + Send>,
    /// Whether the automatic LAN lanes pace themselves now (`aterm_netplace::routes::paced`: away
    /// from the ALab LAN, on a hotspot, offline): a read then that reached nobody waits
    /// [`QUIET_MAX`] at once ([`Quiet::after`]), and the move back reads at once.
    pub(crate) away: Box<dyn Fn() -> bool + Send>,
}

#[cfg(test)]
thread_local! {
    /// How many times this thread read [`FleetGlance::generation`]: the Fleet Field's idle tax.
    pub(crate) static GENERATION_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The fleet glance's handle: the slot and the kick channel.
pub(crate) struct FleetGlance {
    /// The shared slot.
    pub(crate) slot: Arc<Mutex<FleetSlot>>,
    /// The reader's kicks.
    kick: Sender<Kick>,
    /// `[fleet] watch`, which the App keeps current.
    watch: Arc<std::sync::atomic::AtomicBool>,
}

impl FleetGlance {
    /// Ask the reader for `kick` (a no-op once the thread is gone).
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn kick(&self, kick: Kick) {
        let _ = self.kick.send(kick);
    }

    /// Follow `[fleet] watch`.
    pub(crate) fn set_watch(&self, on: bool) {
        self.watch.store(on, std::sync::atomic::Ordering::Release);
    }

    /// The seats the last read of this Mac's copy holds ([`FleetSnapshot::seats`]) while it
    /// stands at `now_ms`: this Mac is in the fleet, the last read did not fail, and the read is
    /// fresh ([`FleetSnapshot::fresh`]). `None` otherwise, and for a survey's read.
    pub(crate) fn seats(&self, now_ms: u64) -> Option<aterm_netplace::words::Seats> {
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if !slot.book.in_fleet || slot.error.is_some() {
            return None;
        }
        slot.snapshot
            .as_ref()
            .filter(|snapshot| snapshot.fresh(now_ms))
            .and_then(|snapshot| snapshot.seats.clone())
    }

    /// What moves when the slot does, read under its lock without copying it: the generation,
    /// whether the last read failed, and whether this Mac is in a fleet.
    pub(crate) fn generation(&self) -> (u16, bool, bool) {
        #[cfg(test)]
        GENERATION_READS.with(|n| n.set(n.get() + 1));
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        (slot.generation, slot.error.is_some(), slot.book.in_fleet)
    }

    /// A glance over `slot` with no reader thread, for a test.
    #[cfg(test)]
    pub(crate) fn over(slot: FleetSlot) -> Self {
        let (kick, _) = std::sync::mpsc::channel();
        Self {
            slot: Arc::new(Mutex::new(slot)),
            kick,
            watch: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// The slot as it stands, copied out under its lock (poison recovered: the slot is plain data, its snapshots shared, so a copy is cheap).
    pub(crate) fn copy(&self) -> FleetSlot {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// START THE READER on thread `aterm-fleet-glance` with `runner` and `seams`, its watch switch starting at `watch`, and its moves ([`ReaderSeams::moved`]) on `aterm-fleet-moved`.
pub(crate) fn spawn(runner: Box<dyn FleetRunner>, mut seams: ReaderSeams) -> Option<FleetGlance> {
    let slot = Arc::new(Mutex::new(FleetSlot::default()));
    let (tx, rx) = std::sync::mpsc::channel();
    let watch = Arc::clone(&seams.watch);
    if let Some(mut moved) = seams.moved.take() {
        let kick = tx.clone();
        let _ = std::thread::Builder::new()
            .name("aterm-fleet-moved".to_string())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                loop {
                    let woke = moved.wait(crate::network_moved::LONG);
                    if woke != crate::network_moved::Woke::Timer && kick.send(Kick::Moved).is_err()
                    {
                        return;
                    }
                }
            });
    }
    let thread_slot = Arc::clone(&slot);
    std::thread::Builder::new()
        .name("aterm-fleet-glance".to_string())
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            reader_loop(runner, &seams, &thread_slot, &rx);
        })
        .ok()?;
    Some(FleetGlance {
        slot,
        kick: tx,
        watch,
    })
}

/// The reader: look, read when [`should_poll`] (or a [`Kick::Now`]) says,
/// at the wait [`Quiet`] keeps (a [`Kick::Moved`] starts it again and reads
/// as if no read was made, and one that comes while a read runs does so once
/// that read is over), write the slot ([`carry`]ing a silent Mac's last
/// rows), tell the App, then wait for a kick, the tick, or the next instant
/// the read or one Mac's rows in it starts or stops counting
/// ([`FleetSnapshot::next_freshness_change`]), whichever is first.
fn reader_loop(
    mut runner: Box<dyn FleetRunner>,
    seams: &ReaderSeams,
    slot: &Arc<Mutex<FleetSlot>>,
    rx: &Receiver<Kick>,
) {
    let lock = || slot.lock().unwrap_or_else(|p| p.into_inner());
    let mut asked = Asked::default();
    let mut first = true;
    let mut freshness_at: Option<u64> = None;
    let mut told_ms: u64 = 0;
    let mut quiet = Quiet::default();
    loop {
        let book = (seams.book)();
        let now = (seams.now_ms)();
        if asked.moved {
            quiet.moved();
        }
        let tried = lock().tried_ms.filter(|_| !asked.moved);
        let dials = runner.dials();
        let watch = !dials || seams.watch.load(std::sync::atomic::Ordering::Acquire);
        // The broker is asked last, only for a read this tick makes: a tick that reads nothing
        // dials nothing, and a read of this Mac's own copy dials no Mac.
        let read = book.in_fleet
            && (asked.now
                || (should_poll(&book, watch, tried, now, asked.menu, quiet.every())
                    && (!dials || (seams.lan)())));
        // A move of the network that came while a read ran: that read went out on the old one.
        let mut moved_since = false;
        let book_moved = {
            let mut slot = lock();
            let moved = slot.book != book;
            slot.book = book;
            slot.reading = read;
            moved
        };
        let freshness_changed = freshness_at.is_some_and(|at| now >= at);
        let clock_rewound = now < told_ms;
        let first_look = std::mem::take(&mut first);
        if read || book_moved || freshness_changed || clock_rewound || first_look {
            if !(seams.post)() {
                return;
            }
            told_ms = now;
        }
        if read {
            let parsed = runner.read(seams.own_node.as_deref(), &*seams.now_ms);
            // The kicks that came while it ran are answered by it, but a move of the network,
            // which asks for a read on the new one; one sent once its result is in the slot asks
            // for another.
            moved_since = Asked::with_queued(None, rx).moved;
            let now = (seams.now_ms)();
            // Whom a read on the old network reached says nothing of the new one; a read that
            // dials nobody backs off from nobody.
            if dials && !moved_since {
                let away = (seams.away)();
                if quiet.after(&parsed, away) {
                    aterm_log::info!("{}", quiet_line(quiet, away));
                }
            }
            {
                let mut slot = lock();
                slot.reading = false;
                slot.tried_ms = Some(now);
                match parsed {
                    Ok(snapshot) => {
                        // A silent seat on this Mac's copy is the node's judgment over a row
                        // that stopped advancing: no last rows of its are carried.
                        let snapshot = if dials {
                            carry(slot.snapshot.as_deref(), snapshot, now)
                        } else {
                            snapshot
                        };
                        if let Some(old) = slot.snapshot.take() {
                            slot.previous = Some((slot.generation, old));
                        }
                        slot.generation = slot.generation.wrapping_add(1);
                        slot.snapshot = Some(Arc::new(snapshot));
                        slot.error = None;
                    }
                    Err(why) => slot.error = Some(why),
                }
            }
            lock().book = (seams.book)();
            if !(seams.post)() {
                return;
            }
            told_ms = now;
        }
        let now = (seams.now_ms)();
        freshness_at = lock()
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.next_freshness_change(told_ms));
        let wait = freshness_at.map_or(seams.tick, |at| {
            Duration::from_millis(at.saturating_sub(now)).min(seams.tick)
        });
        asked = if moved_since {
            Asked {
                moved: true,
                ..Asked::default()
            }
        } else {
            match rx.recv_timeout(wait) {
                Ok(kick) => Asked::with_queued(Some(kick), rx),
                Err(RecvTimeoutError::Timeout) => Asked::default(),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
    }
}

/// The aterm.log line for the read's wait as [`Quiet`] set it, `away` from the ALab LAN or not.
pub(crate) fn quiet_line(quiet: Quiet, away: bool) -> String {
    if quiet.every() == POLL_EVERY {
        "fleet glance: a fleet Mac answered; the fleet is read every minute again".to_owned()
    } else if away {
        format!(
            "fleet glance: this Mac is off the ALab LAN and no fleet Mac answered; the next read \
             in {} min, or at once when this Mac's network moves",
            quiet.every().as_secs() / 60
        )
    } else {
        format!(
            "fleet glance: no fleet Mac answered; the next read in {} min, or at once when \
             this Mac's network moves",
            quiet.every().as_secs() / 60
        )
    }
}

impl crate::App {
    /// START THE FLEET GLANCE with the status item (the first OS window's attach).
    pub(crate) fn start_fleet_glance(&mut self) {
        if !self.fleet_glance_may_start() {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let state = aterm_types::dirs::state_dir();
        #[cfg(unix)]
        let own_node = state
            .as_deref()
            .and_then(|dir| aterm_net::fleet_member::read_node_id(dir).ok().flatten());
        #[cfg(not(unix))]
        let own_node = None;
        let watch = Arc::new(std::sync::atomic::AtomicBool::new(
            self.config.fleet_watch_enabled(),
        ));
        #[cfg(unix)]
        let place_state = state.clone();
        let mesh_state = state.clone();
        let seams = ReaderSeams {
            book: Box::new(move || state.as_deref().map(read_book).unwrap_or_default()),
            watch,
            own_node,
            now_ms: Box::new(|| aterm_time::unix_millis().unwrap_or(0)),
            post: Box::new(move || proxy.send_event(crate::Wake::FleetRead).is_ok()),
            tick: POLL_EVERY,
            moved: Some(crate::network_moved::watch()),
            lan: Box::new(crate::network_moved::lan_answers),
            #[cfg(unix)]
            away: Box::new(move || {
                place_state
                    .as_deref()
                    .is_some_and(aterm_netplace::routes::paced_now)
            }),
            #[cfg(not(unix))]
            away: Box::new(|| false),
        };
        // The vault is read only for this Mac's copy, which composes with its membership log.
        #[cfg(unix)]
        let paths = aterm_types::mesh_switch::MESH
            .then(|| aterm_secrets::client::Paths::live().ok())
            .flatten();
        #[cfg(not(unix))]
        let paths = None;
        self.start_fleet_glance_with(
            runner(aterm_types::mesh_switch::MESH, mesh_state, paths),
            seams,
        );
    }

    /// Whether a fleet glance may start here: never headless (no child, no
    /// thread), never off macOS, never twice.
    const fn fleet_glance_may_start(&self) -> bool {
        !self.headless && cfg!(target_os = "macos") && self.fleet_glance.is_none()
    }

    /// Start the reader with `runner` and `seams` where [`Self::fleet_glance_may_start`] allows, and hand its slot to the status item for the Fleet submenu.
    pub(crate) fn start_fleet_glance_with(
        &mut self,
        runner: Box<dyn FleetRunner>,
        seams: ReaderSeams,
    ) {
        if !self.fleet_glance_may_start() {
            return;
        }
        self.fleet_glance = spawn(runner, seams);
        #[cfg(target_os = "macos")]
        if let (Some(handle), Some(fleet)) = (&self._status_item, &self.fleet_glance) {
            crate::status_item::attach_fleet(handle, Arc::clone(&fleet.slot));
        }
    }

    /// `[fleet] watch` moved (a config commit): the reader follows it.
    pub(crate) fn follow_fleet_watch(&self) {
        if let Some(fleet) = &self.fleet_glance {
            fleet.set_watch(self.config.fleet_watch_enabled());
        }
    }

    /// The `Wake::FleetRead` arm: the fleet glance wrote its slot.
    pub(crate) fn on_fleet_read(&mut self) {
        // The Network State band draws the fleet: its service turn runs at once, not at its
        // next poll, so the new read shows on the next turn.
        if self.network_band_drawn() {
            self.network_next_poll = None;
        }
        self.mark_status_dirty();
    }

    /// A fleet row's act on the Mac the book knows as `dial`, shown as `name` (and its session `sid`).
    #[cfg(target_os = "macos")]
    pub(crate) fn fleet_act(
        &mut self,
        action: crate::status_item::StatusAction,
        dial: &str,
        name: &str,
        host: Option<&str>,
        sid: Option<&str>,
    ) {
        use crate::platform::AppRt as _;
        use crate::status_item::StatusAction;
        match action {
            StatusAction::PeerRead(_) | StatusAction::MacRead(_) => {
                let Some(args) = fleet_read_args(dial, sid) else {
                    aterm_log::info!("status item: no read for a fleet row with no name to dial");
                    return;
                };
                let title =
                    sid.map_or_else(|| name.to_string(), |sid| format!("{name} \u{b7} {sid}"));
                self.run_status_read(crate::app_take_over::CliPlan::peer(args, title));
            }
            StatusAction::PeerScreen(_) | StatusAction::MacScreen(_) => {
                if let Some(host) = host.and_then(host_word) {
                    let _ = self.apprt.open_url(&format!("vnc://{host}"));
                }
            }
            StatusAction::PeerSsh(_) | StatusAction::MacSsh(_) => {
                if let Some(host) = host.and_then(host_word) {
                    let _ = self.apprt.pasteboard_set(&format!("ssh {host}"));
                }
            }
            StatusAction::PeerTakeOver(_) | StatusAction::PeerHandBack(_) => {
                let on = matches!(action, StatusAction::PeerTakeOver(_));
                let Some(args) = sid.and_then(|sid| fleet_hold_args(dial, sid, on)) else {
                    aterm_log::info!(
                        "status item: no step-in for a fleet row with no name to dial"
                    );
                    return;
                };
                let title = format!(
                    "{} on {name} \u{b7} {}",
                    if on { "Take Over" } else { "Hand Back" },
                    sid.unwrap_or("")
                );
                self.run_status_read(crate::app_take_over::CliPlan::driven(args, title));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "fleet_glance_tests.rs"]
mod tests;
