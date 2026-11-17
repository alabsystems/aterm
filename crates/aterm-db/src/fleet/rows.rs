// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet relations: machines, accounts and agents, each a [`Schema`] of the shared relation
//! types (`crate::relation`) and the row type the fold fills. `aterm db fleet` prints by them,
//! and a sheet reads their columns, types and words.
//!
//! Integers only: a percentage is tenths (`_x10`), a core count hundredths (`_x100`), a time
//! Unix milliseconds.
//!
//! A measure is a pair of columns, how much is used and how much there is, each in the
//! measure's own unit; the share is derived where it is drawn. A cell that was not read is
//! [`Cell::Unread`], never a zero; a cell that is known to hold nothing (a machine that answered
//! has no reason for silence) is [`Cell::Empty`].

use super::read::{At, Harness, How, Pair, State, Tally, Task};
use crate::relation::{Cell, Row, Schema, col, opt};
use crate::typed::{Member, Ty, keys};
use aterm_json::{Value, json};

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

/// When a fact was read and the Mac that read it: what every observed fact of a row carries
/// (`<fact>_at`, `<fact>_by`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// When, Unix milliseconds, by the clock of the Mac that read it and never after the read of
    /// the journal that carried it.
    pub at_ms: u64,
    /// The Mac: its machine's label, else its machine's key, else (no live machine row binds
    /// its Commons uuid) the first 8 digits of that uuid.
    pub by: String,
}

/// A fact some Mac observed: what it read, and when and which Mac read it. The registry's own
/// cells are not facts of this kind: a person or an import states them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact<T> {
    pub v: T,
    pub read: Stamp,
}

/// Which of its two sources an account row's `plan` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanSource {
    /// The registry's `plan` cell: what a person (or the sheet a person kept) says it is.
    Set,
    /// The newest plan a Mac's CLI reported, from the journal.
    Observed,
}

impl PlanSource {
    pub const WORDS: &'static [&'static str] = &["set", "observed"];

    pub const fn word(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Observed => "observed",
        }
    }
}

/// What the fleet has observed of an account: derived from evidence each fold and stored
/// nowhere. It stands beside the registry's `status`, which a person states (`paused` is a
/// person's hold, `pending` an import's default) and which no observation rewrites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedState {
    /// The newest sign-in word of some Mac a live machine row binds is `in`.
    SignedIn,
    /// No Mac is signed in to it; Macs state that their login of it ended (a sign-in word
    /// `out`, `ended`, `wrong` or `failed`, or a registry row that says `signed-out` with no
    /// sighting since), a retired Mac among them, and no Mac that states nothing sighted it.
    SignedOut,
    /// No Mac is signed in to it, and a Mac that states no word sighted it: a login file there
    /// names it (a tab runs on it, or the machine sweep found the login place). A sighting is
    /// no sign-in word: a file names its account after the login ended, so a Mac that says its
    /// login ended is not counted here for the file it still holds. The `in` of a Mac no live
    /// machine row binds is no word, and its sighting counts.
    Seen,
    /// No Mac's word of its login stands and none sighted it; a place row, a claim or a
    /// sighted agent session puts it on a Mac.
    PlaceOnly,
    /// The journal was read and nothing a Mac says of it stands: none of the above.
    NeverSeen,
}

impl ObservedState {
    pub const WORDS: &'static [&'static str] = &[
        "signed-in",
        "seen",
        "signed-out",
        "place-only",
        "never-seen",
    ];

    pub const fn word(self) -> &'static str {
        match self {
            Self::SignedIn => "signed-in",
            Self::Seen => "seen",
            Self::SignedOut => "signed-out",
            Self::PlaceOnly => "place-only",
            Self::NeverSeen => "never-seen",
        }
    }
}

/// One Mac that sighted an account: a login file there names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenOn {
    /// The Mac, as a fact's `by` names one ([`Stamp::by`]).
    pub on: String,
    /// The tabs that run the account there now; 0 where none does (an idle login place, or a
    /// tab not seen for two hours).
    pub tabs: u64,
    /// That Mac's newest sighting of it, however old.
    pub at_ms: u64,
}

/// The most Macs an account row's `seen_on` lists: the ones that sighted it last.
pub const SEEN_ON_MAX: usize = 64;

/// One AI account: every registry row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRow {
    pub account: String,
    pub provider: Option<String>,
    /// Its slug (`alab::account_names`): the one word every command names it by.
    pub slug: Option<String>,
    /// Its login address, held as the registry holds it and printed as it is.
    pub login: Option<String>,
    pub status: Option<&'static str>,
    /// Its plan's key: the registry's where the registry states one, else the newest plan a Mac
    /// observed ([`Self::plan_observed`]); [`Self::plan_source`] says which.
    pub plan: Option<String>,
    /// Which of the two `plan` is; `None` where neither says one.
    pub plan_source: Option<PlanSource>,
    /// The newest plan a Mac's CLI reported, as the key of that provider's plan, whatever the
    /// registry states. Beside a stated plan it differs from it is a mismatch: both are shown,
    /// and an observation is never written over what a person stated.
    pub plan_observed: Option<Fact<String>>,
    /// Whether the stated plan and the observed one differ, as the fold judged them
    /// (`fold::plans_differ`: a stated plan that adds a tier to the observed one agrees with
    /// it); `None` where either is missing, since a mismatch is between two plans.
    pub plan_mismatch: Option<bool>,
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
    /// What the fleet observed of it, among the Macs a live machine row binds; `None` where the
    /// journal was not read.
    pub observed: Option<ObservedState>,
    /// The newest evidence [`Self::observed`] stands on: since when it holds, and the Mac it is
    /// of. `None` for `never-seen` and where the journal was not read.
    pub observed_read: Option<Stamp>,
    /// When each window was read and the Mac that read it: the read its figure is of, or, for
    /// a window whose reset passed, the read that named that reset. `None` for a window nobody
    /// read.
    pub w5h_read: Option<Stamp>,
    pub w7d_read: Option<Stamp>,
    /// When the standing wall was hit and the Mac that hit it; `None` where none stands or the
    /// journal says nothing of the account.
    pub wall_read: Option<Stamp>,
    /// The newest sighting of it on any Mac (a `seen` event or an `account-obs` row): when,
    /// and the Mac that sighted it. `None` where no Mac sighted it or no source of sightings
    /// was read.
    pub last_seen: Option<Stamp>,
    /// The Macs that sighted it, at most [`SEEN_ON_MAX`] (the ones that did last), in the
    /// machines' order; `None` where no source of sightings was read.
    pub seen_on: Option<Vec<SeenOn>>,
}

/// The type of a cell that names a machine as a fact's `by` does ([`Stamp::by`]): its label,
/// its row's key or 8 digits of its Commons uuid, each a word of the key grammar. It is an
/// identifier, never free text: free text's guards refuse a run of seven digits as a phone
/// number, which the first 8 digits of a uuid are for one Mac in 43.
const MAC: Ty = Ty::Key;

/// The accounts relation.
pub static ACCOUNTS: Schema = Schema {
    name: "accounts",
    doc: "every AI account of the registry, its last known usage and where it runs",
    key: &["account"],
    order: "by provider, then slug",
    columns: &[
        col("account", Ty::Key, "registry key"),
        opt("provider", Ty::Key, "its provider"),
        opt(
            "slug",
            Ty::Handle(keys::MAX_SLUG),
            "its slug, the one word every command names it by",
        ),
        opt("login", Ty::Email, "its login address"),
        opt("status", Ty::Enum(ACCOUNT_STATUS), "registry status"),
        opt(
            "plan",
            Ty::Key,
            "its plan: the registry's where set, else the newest observed (plan_source)",
        ),
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
        opt("w5h_at", Ty::Time, "when the 5-hour window was read"),
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
        opt("w7d_at", Ty::Time, "when the 7-day window was read"),
        opt("walled", Ty::Bool, "a limit stands"),
        opt("wall_until", Ty::Time, "until when it stands"),
        opt("read_by", Ty::Text(32), "the machine that read the figure"),
        opt("next_reset", Ty::Time, "the next reset or wall end ahead"),
        col(
            "expected",
            Ty::List(&MAC, 64),
            "machines the registry's preferences expect it on, by rank",
        ),
        opt(
            "active",
            Ty::List(
                &Ty::Struct(&[
                    Member {
                        name: "machine",
                        ty: MAC,
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
        opt("last_use_on", MAC, "the machine of that use"),
        opt(
            "plan_source",
            Ty::Enum(PlanSource::WORDS),
            "where plan comes from: set (the registry's cell, a person's) or observed",
        ),
        opt(
            "plan_observed",
            Ty::Key,
            "the newest plan a machine's CLI reported, whatever the registry states",
        ),
        opt("plan_at", Ty::Time, "when the observed plan was read"),
        opt("plan_by", MAC, "the machine that read the observed plan"),
        opt(
            "plan_mismatch",
            Ty::Bool,
            "the set plan is neither the observed one nor a tier of it; unread where either is \
             missing",
        ),
        opt(
            "observed",
            Ty::Enum(ObservedState::WORDS),
            "what machines observed: signed-in, seen, signed-out, place-only or never-seen",
        ),
        opt(
            "observed_at",
            Ty::Time,
            "since when the newest evidence of observed holds",
        ),
        opt("observed_by", MAC, "the machine that evidence is of"),
        opt(
            "last_seen",
            Ty::Time,
            "the newest sighting of its login on any machine",
        ),
        opt(
            "seen_on",
            Ty::List(
                &Ty::Struct(&[
                    Member {
                        name: "machine",
                        ty: MAC,
                        null: false,
                    },
                    Member {
                        name: "tabs",
                        ty: Ty::Uint,
                        null: false,
                    },
                    Member {
                        name: "at",
                        ty: Ty::Time,
                        null: false,
                    },
                ]),
                SEEN_ON_MAX,
            ),
            "machines whose login files name it: tabs running it now, and when last sighted",
        ),
        opt("w5h_by", MAC, "the machine that read the 5-hour window"),
        opt("w7d_by", MAC, "the machine that read the 7-day window"),
        opt("wall_at", Ty::Time, "when the standing limit was hit"),
        opt("wall_by", MAC, "the machine that hit it"),
        opt("last_seen_by", MAC, "the machine of the newest sighting"),
    ],
};

/// The two cells of a fact's provenance, when it was read and the machine that read it: each
/// unread where nobody observed the fact.
fn stamp(cells: &mut Vec<Cell>, read: Option<&Stamp>) {
    cells.push(Cell::read(read.map(|read| read.at_ms)));
    cells.push(Cell::read(read.map(|read| read.by.clone())));
}

/// The three cells of an observed fact: its value, then its provenance ([`stamp`]).
fn fact<T: Clone + Into<Value>>(cells: &mut Vec<Cell>, fact: Option<&Fact<T>>) {
    cells.push(Cell::read(fact.map(|fact| fact.v.clone())));
    stamp(cells, fact.map(|fact| &fact.read));
}

/// A window's four cells: its figure, its reset, its state, and when it was read (`read`: the
/// figure's read, or for a window whose reset passed the read that named the reset).
fn window(cells: &mut Vec<Cell>, window: &WindowRow, read: Option<&Stamp>) {
    cells.push(Cell::read(window.used_x10.map(|at| at.v)));
    cells.push(Cell::read(window.reset_ms));
    cells.push(Cell::of(window.state.word()));
    cells.push(Cell::read(read.map(|read| read.at_ms)));
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
        window(&mut cells, &self.w5h, self.w5h_read.as_ref());
        window(&mut cells, &self.w7d, self.w7d_read.as_ref());
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
        cells.push(Cell::read(self.plan_source.map(PlanSource::word)));
        fact(&mut cells, self.plan_observed.as_ref());
        cells.push(Cell::read(self.plan_mismatch));
        cells.push(Cell::read(self.observed.map(ObservedState::word)));
        match (self.observed, &self.observed_read) {
            // Nothing was seen, and that is known: no evidence, so none to date or to credit.
            (Some(ObservedState::NeverSeen), None) => {
                cells.push(Cell::Empty);
                cells.push(Cell::Empty);
            }
            (_, read) => stamp(&mut cells, read.as_ref()),
        }
        cells.push(match (&self.last_seen, &self.seen_on) {
            // The sightings were read and none names it: known, and nothing to date.
            (None, Some(_)) => Cell::Empty,
            (seen, _) => Cell::read(seen.as_ref().map(|seen| seen.at_ms)),
        });
        cells.push(Cell::read(self.seen_on.as_ref().map(|seen_on| {
            Value::Array(
                seen_on
                    .iter()
                    .map(|seen| json!({"machine": seen.on, "tabs": seen.tabs, "at": seen.at_ms}))
                    .collect(),
            )
        })));
        for read in [&self.w5h_read, &self.w7d_read] {
            cells.push(Cell::read(read.as_ref().map(|read| read.by.clone())));
        }
        match (self.walled, &self.wall_read) {
            // No limit stands, and that is known: none to date or to credit.
            (Some(false), None) => {
                cells.push(Cell::Empty);
                cells.push(Cell::Empty);
            }
            (_, read) => stamp(&mut cells, read.as_ref()),
        }
        cells.push(match (&self.last_seen, &self.seen_on) {
            (None, Some(_)) => Cell::Empty,
            (seen, _) => Cell::read(seen.as_ref().map(|seen| seen.by.clone())),
        });
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
    /// Whether it is pinned; unread where its row did not say.
    pub pinned: Option<bool>,
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
        opt(
            "pinned",
            Ty::Bool,
            "whether a person or a lead agent pinned it; unread from an aterm before the pin",
        ),
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
            Cell::read(self.pinned),
            Cell::of(self.read_at_ms),
            Cell::of(self.how.word()),
            Cell::of(self.proven),
        ]
    }
}

/// Every fleet relation.
pub static SCHEMAS: [&Schema; 3] = [&MACHINES, &ACCOUNTS, &AGENTS];
