// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet relations: machines, accounts and agents, each a typed column list
//! ([`Schema`]) and the row type the fold fills. One schema drives every face: `aterm db fleet`
//! prints by it, and a sheet reads its columns, types and words from it.
//!
//! A column is a `typed::Column` (the ATT/1 value types, so a value is checked and masked as a
//! registry cell is) and the words that say what it holds. Integers only: a percentage is tenths
//! (`_x10`), a core count hundredths (`_x100`), a time Unix milliseconds.
//!
//! A measure is a pair of columns, how much is used and how much there is, each in the
//! measure's own unit; the share is derived where it is drawn. A cell that was not read is
//! [`Cell::Unread`], which prints `?` and is JSON `null`, never a zero; a cell that is known to
//! hold nothing (a machine that answered has no reason for silence) is [`Cell::Empty`].

use super::read::{At, Harness, How, Pair, State, Tally, Task};
use crate::typed::{Column, Member, Ty};
use aterm_json::{Value, json};

/// One column: its typed definition and what it holds.
#[derive(Clone, Copy, Debug)]
pub struct Col {
    pub column: Column,
    pub doc: &'static str,
}

/// A column every row holds a value in.
const fn col(name: &'static str, ty: Ty, doc: &'static str) -> Col {
    Col {
        column: Column::new(name, ty),
        doc,
    }
}

/// A column a row may hold no value in.
const fn opt(name: &'static str, ty: Ty, doc: &'static str) -> Col {
    Col {
        column: Column::new(name, ty).null(),
        doc,
    }
}

/// One relation: its name, its key, its order and its columns.
#[derive(Clone, Copy, Debug)]
pub struct Schema {
    pub name: &'static str,
    pub doc: &'static str,
    /// The columns that name a row.
    pub key: &'static [&'static str],
    /// The order rows come in, in words.
    pub order: &'static str,
    pub columns: &'static [Col],
}

impl Schema {
    /// The position of the column `name`.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.column.name == name)
    }
}

/// One cell of a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    /// A value of the column's type.
    Value(Value),
    /// Nobody read it: `?`, JSON `null`. Never a zero.
    Unread,
    /// It is known to hold nothing: an empty cell, JSON `null`.
    Empty,
}

impl Cell {
    /// A read value, else unread.
    fn read<T: Into<Value>>(value: Option<T>) -> Self {
        value.map_or(Self::Unread, |v| Self::Value(v.into()))
    }

    /// A value, else known empty.
    fn known<T: Into<Value>>(value: Option<T>) -> Self {
        value.map_or(Self::Empty, |v| Self::Value(v.into()))
    }

    fn of<T: Into<Value>>(value: T) -> Self {
        Self::Value(value.into())
    }

    /// The JSON value: `null` for a cell with none.
    pub fn json(&self) -> Value {
        match self {
            Self::Value(value) => value.clone(),
            Self::Unread | Self::Empty => Value::Null,
        }
    }
}

/// A row of a fleet relation.
pub trait Row {
    /// Its relation.
    const SCHEMA: &'static Schema;
    /// Its cells, one per column of [`Self::SCHEMA`], in order.
    fn cells(&self) -> Vec<Cell>;
}

/// The status words of a machine row: the registry's, `retired` for a row its owner retired,
/// and `unregistered` for a machine that answered and that no registry row names.
pub const MACHINE_STATUS: &[&str] = &[
    "planned",
    "active",
    "parked",
    "lost",
    "retired",
    "unregistered",
];
/// A machine no registry row names.
pub const UNREGISTERED: &str = "unregistered";
/// A row its owner retired.
pub const RETIRED: &str = "retired";

/// Whether a machine answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// It answered the newest read, made no longer ago than `fold::REACH_KEEP_MS`, or the
    /// observer holds a current contact.
    Up,
    /// The newest read, as young, asked it and it did not answer.
    Silent,
    /// Nothing current says either way.
    Unread,
}

impl Reach {
    pub const WORDS: &'static [&'static str] = &["up", "silent", "unread"];

    pub const fn word(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Silent => "silent",
            Self::Unread => "unread",
        }
    }
}

/// Where a machine row's newest word of the machine came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// This Mac's own windows.
    Own,
    /// This Mac's fleet read.
    Survey,
    /// The Commons network observer.
    Observer,
    /// The journal's weekly presence row.
    Journal,
    /// Nothing has seen it.
    None,
}

impl Via {
    pub const WORDS: &'static [&'static str] = &["self", "survey", "observer", "journal", "none"];

    pub const fn word(self) -> &'static str {
        match self {
            Self::Own => "self",
            Self::Survey => "survey",
            Self::Observer => "observer",
            Self::Journal => "journal",
            Self::None => "none",
        }
    }

    pub(super) const fn of(how: How) -> Self {
        match how {
            How::Own => Self::Own,
            How::Survey => Self::Survey,
        }
    }
}

/// One machine: every registry row, and every machine that answered that no registry row
/// names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineRow {
    /// The registry key (`mc-…`); `un-…` for an unregistered answer, `self` for this Mac while
    /// no registry names it.
    pub machine: String,
    pub label: Option<String>,
    pub number: Option<u32>,
    pub status: Option<&'static str>,
    pub os: Option<String>,
    pub chip: Option<String>,
    pub ram_gb: Option<u64>,
    /// The Commons machine uuid a facts row binds to it.
    pub uuid: Option<String>,
    pub host: Option<String>,
    pub reach: Reach,
    pub reach_why: Option<String>,
    pub reach_at_ms: Option<u64>,
    pub how: Via,
    /// Whether its last answer proved a node key this Mac binds; `None` where it gave none.
    /// What an unproven answer says of the machine is a claim.
    pub proven: Option<bool>,
    pub first_seen_ms: Option<u64>,
    pub last_seen_ms: Option<u64>,
    /// The build it runs, by the newer of its last answer and its journal row.
    pub build: Option<At<String>>,
    pub sessions: Option<At<u32>>,
    pub tally: Option<At<Tally>>,
    /// Its rows in the agents relation.
    pub agents_listed: Option<u32>,
    /// Free space on its home volume, MiB.
    pub disk_free_mb: Option<At<u64>>,
    /// Cores busy ×100, of cores.
    pub cpu: Pair<u64>,
    /// MiB in use, of MiB installed (the registry's figure).
    pub mem: Pair<u64>,
    /// GPU cores busy ×100, of GPU cores.
    pub gpu: Pair<u64>,
    /// MiB used on its home volume, of the volume's MiB.
    pub disk: Pair<u64>,
    /// Kilobits a second moving, of its link's rate.
    pub net: Pair<u64>,
    /// The accounts the registry's preferences expect on it.
    pub accounts_expected: Option<u32>,
    /// The accounts some evidence says run on it now.
    pub accounts_active: Option<u32>,
}

impl MachineRow {
    /// A row with its key and nothing read.
    pub(super) fn unread(machine: String) -> Self {
        Self {
            machine,
            label: None,
            number: None,
            status: None,
            os: None,
            chip: None,
            ram_gb: None,
            uuid: None,
            host: None,
            reach: Reach::Unread,
            reach_why: None,
            reach_at_ms: None,
            how: Via::None,
            proven: None,
            first_seen_ms: None,
            last_seen_ms: None,
            build: None,
            sessions: None,
            tally: None,
            agents_listed: None,
            disk_free_mb: None,
            cpu: Pair::UNREAD,
            mem: Pair::UNREAD,
            gpu: Pair::UNREAD,
            disk: Pair::UNREAD,
            net: Pair::UNREAD,
            accounts_expected: None,
            accounts_active: None,
        }
    }
}

/// The three cells of a measure: used, capacity, and when the newer of the two was read.
fn pair(cells: &mut Vec<Cell>, pair: &Pair<u64>) {
    cells.push(Cell::read(pair.used.map(|at| at.v)));
    cells.push(Cell::read(pair.capacity.map(|at| at.v)));
    cells.push(Cell::read(
        pair.used
            .map(|at| at.at_ms)
            .max(pair.capacity.map(|at| at.at_ms)),
    ));
}

/// The machines relation.
pub static MACHINES: Schema = Schema {
    name: "machines",
    doc: "every machine of the registry, and every machine that answered a fleet read",
    key: &["machine"],
    order: "by machine number (m3 before m22), unnumbered rows after, unregistered answers last",
    columns: &[
        col(
            "machine",
            Ty::Key,
            "registry key; un-… for an unregistered answer",
        ),
        opt("label", Ty::Label, "its m<N> label"),
        opt("number", Ty::Uint, "the N of its label"),
        opt(
            "status",
            Ty::Enum(MACHINE_STATUS),
            "registry status; unregistered where no registry row names it",
        ),
        opt("os", Ty::Text(32), "registry: operating system"),
        opt("chip", Ty::Text(32), "registry: chip"),
        opt("ram_gb", Ty::Uint, "registry: memory installed, GB"),
        opt(
            "uuid",
            Ty::Hex(32),
            "the Commons machine uuid bound to it; empty where none is",
        ),
        opt(
            "host",
            Ty::Text(63),
            "the host name the fleet book dials, first label",
        ),
        col(
            "reach",
            Ty::Enum(Reach::WORDS),
            "up, silent (asked, no answer) or unread",
        ),
        opt(
            "reach_why",
            Ty::Text(32),
            "the read's reason token for a silent machine",
        ),
        opt("reach_at", Ty::Time, "when reach was learned"),
        col(
            "how",
            Ty::Enum(Via::WORDS),
            "what learned reach: self, survey, observer, journal or none",
        ),
        opt(
            "proven",
            Ty::Bool,
            "whether its last answer proved a node key this Mac binds; false marks a claim",
        ),
        opt(
            "first_seen",
            Ty::Time,
            "the observer's first contact: when it came in",
        ),
        opt(
            "last_seen",
            Ty::Time,
            "the newest answer, contact or journal row",
        ),
        opt("build", Ty::Text(48), "the aterm build it runs"),
        opt("sessions", Ty::Uint, "its live sessions, agents or not"),
        opt("agents", Ty::Uint, "its agent sessions"),
        opt("busy", Ty::Uint, "agents at work"),
        opt("idle", Ty::Uint, "agents at rest"),
        opt("waiting", Ty::Uint, "agents waiting on a person"),
        opt("walled", Ty::Uint, "agents at a limit"),
        opt("stuck", Ty::Uint, "agents whose task is stalled"),
        opt("unknown", Ty::Uint, "agents saying anything else"),
        opt("agents_listed", Ty::Uint, "its rows in the agents relation"),
        opt(
            "agents_at",
            Ty::Time,
            "when its sessions and agents were counted",
        ),
        opt(
            "facts_at",
            Ty::Time,
            "when build and disk_free_mb were read",
        ),
        opt(
            "disk_free_mb",
            Ty::Uint,
            "free space on its home volume, MiB",
        ),
        opt("cpu_used_x100", Ty::Uint, "cores busy, hundredths"),
        opt("cpu_cores", Ty::Uint, "cores"),
        opt("cpu_at", Ty::Time, "when the cpu pair was read"),
        opt("mem_used_mb", Ty::Uint, "memory in use, MiB"),
        opt(
            "mem_total_mb",
            Ty::Uint,
            "memory installed, MiB, from the registry",
        ),
        opt("mem_at", Ty::Time, "when the memory pair was read"),
        opt("gpu_used_x100", Ty::Uint, "GPU cores busy, hundredths"),
        opt("gpu_cores", Ty::Uint, "GPU cores"),
        opt("gpu_at", Ty::Time, "when the GPU pair was read"),
        opt("disk_used_mb", Ty::Uint, "home volume used, MiB"),
        opt("disk_total_mb", Ty::Uint, "home volume size, MiB"),
        opt("disk_at", Ty::Time, "when the disk pair was read"),
        opt("net_used_kbps", Ty::Uint, "network traffic, kbit/s"),
        opt("net_link_kbps", Ty::Uint, "link rate, kbit/s"),
        opt("net_at", Ty::Time, "when the network pair was read"),
        opt(
            "accounts_expected",
            Ty::Uint,
            "accounts the registry's preferences expect here",
        ),
        opt(
            "accounts_active",
            Ty::Uint,
            "accounts some evidence says run here now",
        ),
    ],
};

impl Row for MachineRow {
    const SCHEMA: &'static Schema = &MACHINES;

    fn cells(&self) -> Vec<Cell> {
        let tally = self.tally.as_ref().map(|at| at.v);
        let count = |pick: fn(&Tally) -> u32| Cell::read(tally.as_ref().map(pick));
        let facts_at = self
            .build
            .as_ref()
            .map(|at| at.at_ms)
            .max(self.disk_free_mb.map(|at| at.at_ms));
        let agents_at = self
            .tally
            .map(|at| at.at_ms)
            .max(self.sessions.map(|at| at.at_ms));
        let mut cells = vec![
            Cell::of(self.machine.clone()),
            Cell::read(self.label.clone()),
            Cell::read(self.number),
            Cell::read(self.status),
            Cell::read(self.os.clone()),
            Cell::read(self.chip.clone()),
            Cell::read(self.ram_gb),
            Cell::known(self.uuid.clone()),
            Cell::known(self.host.clone()),
            Cell::of(self.reach.word()),
            Cell::known(self.reach_why.clone()),
            Cell::read(self.reach_at_ms),
            Cell::of(self.how.word()),
            Cell::read(self.proven),
            Cell::read(self.first_seen_ms),
            Cell::read(self.last_seen_ms),
            Cell::read(self.build.as_ref().map(|at| at.v.clone())),
            Cell::read(self.sessions.map(|at| at.v)),
            count(|t| t.agents),
            count(|t| t.busy),
            count(|t| t.idle),
            count(|t| t.waiting),
            count(|t| t.walled),
            count(|t| t.stuck),
            count(|t| t.unknown),
            Cell::read(self.agents_listed),
            Cell::read(agents_at),
            Cell::read(facts_at),
            Cell::read(self.disk_free_mb.map(|at| at.v)),
        ];
        for measure in [&self.cpu, &self.mem, &self.gpu, &self.disk, &self.net] {
            pair(&mut cells, measure);
        }
        cells.push(Cell::read(self.accounts_expected));
        cells.push(Cell::read(self.accounts_active));
        cells
    }
}

/// The status words of an account row: the registry's, and `retired`.
pub const ACCOUNT_STATUS: &[&str] = &["pending", "active", "paused", "broken", "banned", "retired"];

/// What a limit window's figure stands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowState {
    /// A reading of the window that is open now.
    Current,
    /// The window it was read in has reset and nothing was read since: no figure, never 0.
    ResetPassed,
    /// Nobody read it.
    Unread,
}

impl WindowState {
    pub const WORDS: &'static [&'static str] = &["current", "reset-passed", "unread"];

    pub const fn word(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::ResetPassed => "reset-passed",
            Self::Unread => "unread",
        }
    }
}

/// One limit window of an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowRow {
    /// Tenths of a percent used, with when it was read.
    pub used_x10: Option<At<u16>>,
    /// The window's reset.
    pub reset_ms: Option<u64>,
    pub state: WindowState,
}

impl WindowRow {
    pub const UNREAD: Self = Self {
        used_x10: None,
        reset_ms: None,
        state: WindowState::Unread,
    };
}

/// A machine as another relation's cell names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineRef {
    /// Its row's key.
    pub machine: String,
    pub label: Option<String>,
}

impl MachineRef {
    /// Its label, else its key.
    pub fn name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.machine)
    }
}

/// What says an account runs on a machine now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceHow {
    /// A sighted agent session names the account.
    Agent,
    /// A live claim of the claims ring.
    Claim,
    /// A live journal `place` row.
    Place,
    /// The machine's journal sign-in word is `in`.
    SignedIn,
}

impl EvidenceHow {
    pub const WORDS: &'static [&'static str] = &["agent", "claim", "place", "signed-in"];

    pub const fn word(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Claim => "claim",
            Self::Place => "place",
            Self::SignedIn => "signed-in",
        }
    }
}

/// One piece of evidence that an account runs on a machine: never stored, read again each fold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub on: MachineRef,
    pub how: EvidenceHow,
    /// Since when, where the evidence says.
    pub since_ms: Option<u64>,
    /// When the evidence was read.
    pub at_ms: u64,
}

/// One AI account: every registry row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRow {
    pub account: String,
    pub provider: Option<String>,
    /// Its label: the short name a person calls it by.
    pub slug: Option<String>,
    /// Its login address, held as the registry holds it and printed masked.
    pub login: Option<String>,
    pub status: Option<&'static str>,
    pub plan: Option<String>,
    pub w5h: WindowRow,
    pub w7d: WindowRow,
    /// Whether a wall stands; `None` where the journal says nothing of the account.
    pub walled: Option<bool>,
    pub wall_until_ms: Option<u64>,
    /// The machine whose reading the newest figure is.
    pub read_by: Option<String>,
    /// The next reset or wall end ahead of the fold.
    pub next_reset_ms: Option<u64>,
    /// The machines the registry's preferences expect it on, by rank.
    pub expected: Vec<MachineRef>,
    /// The machines evidence says it runs on now; `None` where no evidence source was read.
    pub active: Option<Vec<Evidence>>,
    /// Sighted agent sessions on it, of proven answers; `None` where no machine's answer still
    /// speaks for its agents (`fold::AGENTS_KEEP_MS`).
    pub agents: Option<u32>,
    pub last_use_ms: Option<u64>,
    pub last_use_on: Option<String>,
}

/// The accounts relation.
pub static ACCOUNTS: Schema = Schema {
    name: "accounts",
    doc: "every AI account of the registry, its last known usage and where it runs",
    key: &["account"],
    order: "by provider, then slug",
    columns: &[
        col("account", Ty::Key, "registry key"),
        opt("provider", Ty::Key, "its provider"),
        opt("slug", Ty::AccountLabel(32), "its label"),
        opt("login", Ty::Email, "its login address, masked"),
        opt("status", Ty::Enum(ACCOUNT_STATUS), "registry status"),
        opt("plan", Ty::Key, "its plan"),
        opt(
            "w5h_used_x10",
            Ty::Uint,
            "5-hour window used, tenths of a percent",
        ),
        opt("w5h_reset", Ty::Time, "when the 5-hour window resets"),
        col(
            "w5h_state",
            Ty::Enum(WindowState::WORDS),
            "current, reset-passed or unread",
        ),
        opt("w5h_at", Ty::Time, "when the 5-hour figure was read"),
        opt(
            "w7d_used_x10",
            Ty::Uint,
            "7-day window used, tenths of a percent",
        ),
        opt("w7d_reset", Ty::Time, "when the 7-day window resets"),
        col(
            "w7d_state",
            Ty::Enum(WindowState::WORDS),
            "current, reset-passed or unread",
        ),
        opt("w7d_at", Ty::Time, "when the 7-day figure was read"),
        opt("walled", Ty::Bool, "a limit stands"),
        opt("wall_until", Ty::Time, "until when it stands"),
        opt("read_by", Ty::Text(32), "the machine that read the figure"),
        opt("next_reset", Ty::Time, "the next reset or wall end ahead"),
        col(
            "expected",
            Ty::List(&Ty::Text(32), 64),
            "machines the registry's preferences expect it on, by rank",
        ),
        opt(
            "active",
            Ty::List(
                &Ty::Struct(&[
                    Member {
                        name: "machine",
                        ty: Ty::Text(32),
                        null: false,
                    },
                    Member {
                        name: "how",
                        ty: Ty::Enum(EvidenceHow::WORDS),
                        null: false,
                    },
                    Member {
                        name: "since",
                        ty: Ty::Time,
                        null: true,
                    },
                    Member {
                        name: "at",
                        ty: Ty::Time,
                        null: false,
                    },
                ]),
                256,
            ),
            "machines evidence says it runs on now, each with its source word",
        ),
        opt("agents", Ty::Uint, "sighted agent sessions on it"),
        opt("last_use", Ty::Time, "its newest use in the journal"),
        opt("last_use_on", Ty::Text(32), "the machine of that use"),
    ],
};

fn window(cells: &mut Vec<Cell>, window: &WindowRow) {
    cells.push(Cell::read(window.used_x10.map(|at| at.v)));
    cells.push(Cell::read(window.reset_ms));
    cells.push(Cell::of(window.state.word()));
    cells.push(Cell::read(window.used_x10.map(|at| at.at_ms)));
}

impl Row for AccountRow {
    const SCHEMA: &'static Schema = &ACCOUNTS;

    fn cells(&self) -> Vec<Cell> {
        let mut cells = vec![
            Cell::of(self.account.clone()),
            Cell::read(self.provider.clone()),
            Cell::read(self.slug.clone()),
            Cell::read(self.login.clone()),
            Cell::read(self.status),
            Cell::read(self.plan.clone()),
        ];
        window(&mut cells, &self.w5h);
        window(&mut cells, &self.w7d);
        cells.push(Cell::read(self.walled));
        cells.push(match (self.walled, self.wall_until_ms) {
            (_, Some(until)) => Cell::of(until),
            (Some(false), None) => Cell::Empty,
            (_, None) => Cell::Unread,
        });
        cells.push(Cell::read(self.read_by.clone()));
        cells.push(Cell::read(self.next_reset_ms));
        cells.push(Cell::of(Value::Array(
            self.expected
                .iter()
                .map(|on| Value::String(on.name().to_owned()))
                .collect(),
        )));
        cells.push(Cell::read(self.active.as_ref().map(|active| {
            Value::Array(
                active
                    .iter()
                    .map(|e| {
                        json!({
                            "machine": e.on.name(),
                            "how": e.how.word(),
                            "since": e.since_ms,
                            "at": e.at_ms,
                        })
                    })
                    .collect(),
            )
        })));
        cells.push(Cell::read(self.agents));
        cells.push(Cell::read(self.last_use_ms));
        cells.push(Cell::read(self.last_use_on.clone()));
        cells
    }
}

/// The account of an agent row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountRef {
    /// Its row named no account.
    Unnamed,
    /// Its row named one no registry row answers to. The word is not kept.
    Unresolved,
    /// The registry account its word names.
    Resolved(String),
}

/// One sighted agent session: a row per (machine, sid) a machine that answered listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    /// Its machine's row key.
    pub machine: String,
    pub label: Option<String>,
    pub sid: String,
    pub harness: Harness,
    pub state: State,
    pub state_why: Option<String>,
    /// Since when its state word has stood: the read's time less the age its row gave.
    pub state_since_ms: Option<u64>,
    pub task: Option<Task>,
    pub ctx_left_pct: Option<u8>,
    pub usage_5h_pct: Option<u8>,
    pub usage_7d_pct: Option<u8>,
    pub account: AccountRef,
    pub model: Option<String>,
    pub read_at_ms: u64,
    pub how: How,
    /// Whether the answer that listed it proved a node key this Mac binds. An unproven row is a
    /// claim: it is no evidence of where its account runs.
    pub proven: bool,
}

/// The agents relation: sightings, not conversations.
pub static AGENTS: Schema = Schema {
    name: "agents",
    doc: "every agent session a machine that answered listed: a sighting, kept two hours",
    key: &["machine", "sid"],
    order: "by machine, as the machines relation orders them, then sid",
    columns: &[
        col("machine", Ty::Key, "its machine's key"),
        opt("label", Ty::Label, "its machine's m<N> label"),
        col("sid", Ty::Text(40), "its session id there"),
        col("harness", Ty::Enum(Harness::WORDS), "the agent program"),
        col(
            "state",
            Ty::Enum(State::WORDS),
            "busy, idle, waiting on a person, walled at a limit, stuck or unknown",
        ),
        opt("state_why", Ty::Text(32), "the limit a walled agent is at"),
        opt("state_since", Ty::Time, "since when its state has stood"),
        opt(
            "task",
            Ty::Enum(Task::WORDS),
            "its task's state, where it has one",
        ),
        opt("ctx_left_pct", Ty::Uint, "context left, percent"),
        opt(
            "usage_5h_pct",
            Ty::Uint,
            "its account's 5-hour window used, percent, as it read it",
        ),
        opt(
            "usage_7d_pct",
            Ty::Uint,
            "its account's 7-day window used, percent, as it read it",
        ),
        opt(
            "account",
            Ty::Key,
            "the registry account it runs on; unread where its word names none",
        ),
        opt("model", Ty::Text(48), "its model"),
        col("read_at", Ty::Time, "when its row was read"),
        col("how", Ty::Enum(&["self", "survey"]), "self or survey"),
        col(
            "proven",
            Ty::Bool,
            "whether its machine's answer proved a node key this Mac binds; false marks a claim",
        ),
    ],
};

impl Row for AgentRow {
    const SCHEMA: &'static Schema = &AGENTS;

    fn cells(&self) -> Vec<Cell> {
        vec![
            Cell::of(self.machine.clone()),
            Cell::read(self.label.clone()),
            Cell::of(self.sid.clone()),
            Cell::of(self.harness.word()),
            Cell::of(self.state.word()),
            Cell::known(self.state_why.clone()),
            Cell::read(self.state_since_ms),
            Cell::known(self.task.map(Task::word)),
            Cell::read(self.ctx_left_pct),
            Cell::read(self.usage_5h_pct),
            Cell::read(self.usage_7d_pct),
            match &self.account {
                AccountRef::Unnamed => Cell::Empty,
                AccountRef::Unresolved => Cell::Unread,
                AccountRef::Resolved(pk) => Cell::of(pk.clone()),
            },
            Cell::read(self.model.clone()),
            Cell::of(self.read_at_ms),
            Cell::of(self.how.word()),
            Cell::of(self.proven),
        ]
    }
}

/// Every fleet relation.
pub static SCHEMAS: [&Schema; 3] = [&MACHINES, &ACCOUNTS, &AGENTS];
