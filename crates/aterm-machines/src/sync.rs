// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_machines::sync`: the converge pass that keeps a Mac aligned with the lab's expected
//! state after an update, a network move or half an hour (docs/DESIGN-init-on-by-default-
//! 2026-10-25.md §8.5, the one converge entry its sync kinds act through).
//!
//! Its triggers decide when a pass runs: the vault's subscriber as it starts, after each version
//! it applies and once its own recheck or a route move says so (`aterm_secrets::converge::Due`),
//! the window's lane at launch and every 30 minutes, the first-launch Secrets stage as it ends,
//! and a person. [`converge`] runs the one it is asked for, under a try-lock
//! (`<state_dir>/converge/align.lock`, one pass at a time), records it in
//! `<state_dir>/converge/aligned.json` ([`Record`]) beside the change journal, and writes one
//! aterm.log line, a refused pass included. The record names why the pass was [`due`] (a build
//! that ran none or a new [`SPEC`], moved routes, [`RECHECK`] since the last attempt, or none),
//! a word the ledger line carries; no row and no trigger reads it, and each row is decided by
//! its own probe. The base layer keeps its own record ([`BaseRecord`]). Its items reach the
//! machine only through [`Ports`], so each has an inert test arm. The pass applies the Relay and
//! Install classes alone: a Person, Preference, Elevate or Asked item is probed and planned,
//! never applied, and nothing asks. The base layer is the installed release app's alone, laid
//! once per build it installs ([`BaseRecord`]), whichever runners of other builds pass between.

use crate::init::{Base, BaseLock};
use aterm_converge::{
    Class, Explain, FileJournal, Item, ItemReport, Layer, Mode, Outcome, Probe, Remedy, Scope,
    StoodAside,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// The desired state's revision: a pass is due on the same build once it moves, as when an item
/// joins or leaves the pass (3: `sync.identity`; 4: the ALab steward's fleet item; 5:
/// `sync.steward.revoke`; 6: `sync.node`; 7: `sync.steward.lease` and
/// `sync.steward.server.standby`; 8: the steward's fleet item retired; 9: automatic signing).
pub const SPEC: u32 = 9;

/// The pass's record, in `<state_dir>/converge` ([`FileJournal::DIR`]).
pub const RECORD: &str = "aligned.json";

/// The try-lock one pass holds, beside the record.
pub const LOCK: &str = "align.lock";

/// How long after a pass's attempt the next one is due on the same build and routes.
pub const RECHECK: Duration = Duration::from_secs(30 * 60);

/// Maximum random jitter before an automatic pass's first `sync.fleet` attempt (§7.1, C18).
pub const FLEET_JOIN_JITTER_MAX: Duration = Duration::from_secs(120);

/// Total wait budget (initial jitter plus capacity retries) for one `sync.fleet` apply (§12.3, C18).
pub const FLEET_JOIN_RETRY_BUDGET: Duration = Duration::from_secs(10 * 60);

/// Per-retry backoff caps when the fleet listener or resolver is at capacity.
const FLEET_CAPACITY_BACKOFF_SECS: &[u64] = &[2, 4, 8, 16, 32, 60, 60, 60, 60, 60, 60];

/// The family every sync row carries (the `--only` merge key).
pub const FAMILY: &str = "sync";

/// The most builds a [`Record`] remembers running a pass ([`Record::builds`]).
pub const BUILDS_KEPT: usize = 8;

/// `sync.peers`: the vault join of a Mac that has none.
pub const PEERS: &str = "sync.peers";
/// `sync.vault`: the vault's subscriber (report only).
pub const VAULT: &str = "sync.vault";
/// `sync.identity`: this Mac's swarm label in `[ai] machine` and `[fleet] machine`.
pub const IDENTITY: &str = "sync.identity";
/// `sync.base`: the row that stands for the base layer a pass leaves to `aterm doctor`.
pub const BASE: &str = "sync.base";
/// `sync.steward.revoke`: the ALab steward finishes any revoke cascade left half done.
pub const STEWARD_REVOKE: &str = "sync.steward.revoke";
/// `sync.steward.lease`: the always-on ALab steward claims, renews, or takes over `aterm-swarm-lease`.
pub const STEWARD_LEASE: &str = "sync.steward.lease";
/// `sync.steward.server.standby`: the always-on ALab steward runs its standby vault server.
pub const STEWARD_SERVER_STANDBY: &str = "sync.steward.server.standby";
/// `sync.fleet`: the default ALab fleet.
pub const FLEET: &str = "sync.fleet";
/// `sync.node`: the `systems.alab.aterm-node` background job (`aterm node serve`, C20).
pub const NODE: &str = "sync.node";
/// `sync.shared-data`: ALab Commons, founded by the vault's only admin where it has none.
pub const SHARED_DATA: &str = "sync.shared-data";
/// `sync.requests`: this Mac's credential requests on the lab's table.
pub const REQUESTS: &str = "sync.requests";
/// `sync.accounts`: the AI accounts roster.
pub const ACCOUNTS: &str = "sync.accounts";
/// `sync.release`: automatic private keychain setup and a real signing proof.
pub const RELEASE: &str = "sync.release";
/// `sync.fabric`: the message bus (fabric), on by default, and its broker renewed on a new build.
pub const FABRIC: &str = "sync.fabric";

/// What every sync row says when `[swarm] enabled = false` turns swarm participation off.
pub use aterm_secrets::swarm::SWARM_OFF;

/// The classes a pass applies: what re-lays without a person, and the network.
const APPLIES: &[Class] = &[Class::Relay, Class::Install];

/// Why a pass is due ([`due`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// No pass the record remembers ran on this build (or this [`SPEC`]).
    NewBuild,
    /// This Mac's routes moved since the last pass.
    NetworkMoved,
    /// [`RECHECK`] passed since the last pass's attempt.
    Recheck,
    /// Nothing calls for a pass; one asked for runs all the same.
    NotDue,
}

impl Due {
    /// The word the ledger line and the record's reader name it by.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::NewBuild => "new-build",
            Self::NetworkMoved => "network-moved",
            Self::Recheck => "recheck",
            Self::NotDue => "not-due",
        }
    }
}

/// Who runs a pass, which decides what it may take on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The vault's subscriber (a launchd job, no window and no person).
    Subscriber,
    /// The aterm window's background lane.
    Window,
    /// The first launch's Secrets stage, in the window.
    Setup,
    /// A person's `aterm init --converge`.
    Init,
}

impl Trigger {
    /// The word `--trigger` takes and the ledger line names.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Subscriber => "subscriber",
            Self::Window => "window",
            Self::Setup => "setup",
            Self::Init => "init",
        }
    }

    /// The trigger `word` names, as [`Trigger::word`] spells it.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        [Self::Subscriber, Self::Window, Self::Setup, Self::Init]
            .into_iter()
            .find(|trigger| trigger.word() == word)
    }

    /// Whether an aterm window runs this pass: its upkeep, not the pass, keeps the accounts.
    const fn in_window(self) -> bool {
        matches!(self, Self::Window | Self::Setup)
    }
}

/// The last pass, as `<state_dir>/converge/aligned.json` keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The build that ran it (`aterm_types::version::build_id`).
    pub build: String,
    /// The [`SPEC`] it ran under.
    pub spec: u32,
    /// The builds that ran a pass under this [`SPEC`], the last one last, at most
    /// [`BUILDS_KEPT`]: two runners of different builds that take turns (the installed app and
    /// a subscriber's held copy) make neither one's next pass due as new.
    #[serde(default)]
    pub builds: Vec<String>,
    /// When it started, in ms since the epoch.
    pub attempted_ms: u64,
    /// When a pass last ended with no row failed or unmeasured, in ms since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converged_ms: Option<u64>,
    /// This Mac's routes as it read them ([`Ports::routes`]).
    #[serde(default)]
    pub routes: Vec<String>,
    /// Each row's id, outcome and why.
    #[serde(default)]
    pub rows: Vec<RecordRow>,
    /// The base layer as the installed release app last laid it, carried unchanged by a pass
    /// that lays none (a held copy's, a build's, or one on a build already laid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<BaseRecord>,
}

impl Record {
    /// Whether a pass this record remembers ran on `build`.
    fn ran(&self, build: &str) -> bool {
        self.build == build || self.builds.iter().any(|ran| ran == build)
    }
}

/// The base layer as the installed release app last laid it ([`Record::base`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRecord {
    /// The installed build that laid it.
    pub build: String,
    /// The repair items that pass did not lay (each failed, stood aside on a held store or was
    /// held back): the next pass from that build lays them, and only them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<String>,
}

/// One row of a [`Record`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRow {
    /// The item's id.
    pub id: String,
    /// What happened to it.
    pub outcome: Outcome,
    /// Why, where the row says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// Why a pass would be due by its record: a build no remembered pass ran on, or a new [`SPEC`]
/// (or no record), then routes that moved, then [`RECHECK`] since the last attempt (or a clock
/// now behind it, which leaves the record's age unknown). It labels a pass (the ledger line's
/// word, [`Pass::due`]); whether a pass runs is its trigger's, and what each row does its probe's.
#[must_use]
pub fn due(record: Option<&Record>, build: &str, routes: &[String], now_ms: u64) -> Due {
    let Some(record) = record else {
        return Due::NewBuild;
    };
    if record.spec != SPEC || !record.ran(build) {
        return Due::NewBuild;
    }
    if record.routes != routes {
        return Due::NetworkMoved;
    }
    let recheck = u64::try_from(RECHECK.as_millis()).unwrap_or(u64::MAX);
    if now_ms < record.attempted_ms || now_ms - record.attempted_ms >= recheck {
        return Due::Recheck;
    }
    Due::NotDue
}

/// The record under `state_dir`, or `None` where there is none or it cannot be read (the pass is
/// then due as on a new build, the direction that leaves nothing out).
#[must_use]
pub fn read_record(state_dir: &Path) -> Option<Record> {
    let bytes =
        aterm_uds::private_file::read_private(&state_dir.join(FileJournal::DIR), RECORD, 1 << 20)
            .ok()?;
    aterm_json::from_slice(&bytes).ok()
}

fn write_record(dir: &Path, record: &Record) -> Result<(), String> {
    let mut text = aterm_json::to_string(record).map_err(|why| why.to_string())?;
    text.push('\n');
    aterm_uds::private_file::write_private(dir, RECORD, text.as_bytes())
        .map_err(|why| format!("{}: {why}", dir.join(RECORD).display()))
}

/// The pass's lock ([`try_lock`]), let go when dropped.
pub struct AlignLock {
    _held: aterm_uds::private_file::PrivateLock,
}

/// Take `<state_dir>/converge/align.lock` without waiting, making the folder 0700 where it is
/// missing: `None` while another pass holds it.
///
/// # Errors
/// The folder cannot be made private, or the lock file cannot be opened.
pub fn try_lock(state_dir: &Path) -> Result<Option<AlignLock>, String> {
    let dir = state_dir.join(FileJournal::DIR);
    aterm_uds::private_file::ensure_private_dir(&dir).map_err(|why| why.to_string())?;
    match aterm_uds::private_file::PrivateLock::take(
        &dir,
        LOCK,
        Duration::ZERO,
        "another converge pass runs",
    ) {
        Ok(held) => Ok(Some(AlignLock { _held: held })),
        Err(why) if why.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(why) => Err(format!("{}: {why}", dir.join(LOCK).display())),
    }
}

/// What the vault's subscriber reads as (`sync.vault`, report only: the window's mend owns it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultRead {
    /// No vault is configured.
    NoVault,
    /// The person turned the subscriber off (`aterm secrets agent off`).
    Off,
    /// It runs: what it runs, in one line.
    Runs(String),
    /// It does not run: why, in one line.
    Idle(String),
}

/// What this Mac's swarm label in `aterm.toml` reads as (`sync.identity`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityRead {
    /// No swarm label applies on this Mac (no ALab vault or swarm roster, or swarm is off): why.
    NotApplicable(String),
    /// `[ai] machine` and `[fleet] machine` already carry the resolved label: what they read.
    Current(String),
    /// One or both keys differ from the resolved label: what they read now and what a pass writes.
    Due {
        /// What `aterm.toml` reads now.
        from: String,
        /// What a pass writes.
        to: String,
    },
}

/// What the default ALab fleet reads as before a sync (`sync.fleet`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetRead {
    /// A sync does nothing here (off after a leave, no vault, not ALab's, no code): why.
    Idle(String),
    /// This Mac is in the vault's fleet: what it reads, in one line.
    Current(String),
    /// A sync would act: what it finds, in one line (never the secret).
    Due(String),
}

/// What a sync of the default ALab fleet did (`sync.fleet`'s apply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetDone {
    /// It joined, granted or recorded: its report.
    Synced(String),
    /// It left the fleet this Mac holds as it is: why, and what a person does about it.
    Blocked(String),
    /// Every dialled listener or the DNS resolver was at connection capacity: why.
    AtCapacity(String),
}

/// What `systems.alab.aterm-node` reads as (`sync.node`, C20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRead {
    /// Not applicable on this Mac (turned off, not in a fleet, CI, or no installed binary): why.
    NotApplicable(String),
    /// Installed and running the expected release binary: what it runs.
    Current(String),
    /// Missing, stopped, or running a replaced binary: what it reads now and what a pass installs.
    Due {
        /// Current state.
        from: String,
        /// Target state after `sync.node` applies.
        to: String,
    },
}

/// Whose network a pass's publication to the vault goes out on (the steward's revoke repair or
/// lease, a founding of Commons), which decides whether macOS's Local Network answer stands
/// before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publisher {
    /// The vault subscriber's launchd job, whose runner this pass is: the job's own rounds reach
    /// the vault, so a publication to it raises no question they did not, whatever aterm.app
    /// last heard of Local Network.
    Subscriber,
    /// aterm's own (a window's pass, the first launch's, a person's `aterm init --converge`,
    /// or a subscriber pass handed to the installed app): Local Network as aterm last heard it.
    Aterm,
}

/// What the ALab steward's revoke-repair step reads as (`sync.steward.revoke`, §7.6, C25).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StewardRevokeRead {
    /// Not applicable on this Mac (not an ALab steward, or vault sync is off).
    Idle(String),
    /// Every revoked Mac's roster and pool cascade is already complete.
    Current(String),
    /// A half-done revoke cascade was found and will be finished on apply.
    Due {
        /// What it finds unfinished.
        found: String,
        /// What its apply publishes.
        makes: String,
    },
    /// It would finish the cascade once Local Network permission is granted: why.
    Deferred(String),
}

/// What the always-on ALab steward's lease step reads as (`sync.steward.lease`, §3.4, §8.3, C7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StewardLeaseRead {
    /// Not applicable on this Mac (not an `always_on` steward, off the ALab network, or no policy).
    Idle(String),
    /// Existing lease stands (`>= 3 h` remaining for holder, or unexpired for standby).
    Current(String),
    /// A claim, renewal, or takeover publish is due.
    Due {
        /// What it finds.
        found: String,
        /// What its apply publishes.
        makes: String,
    },
    /// It would publish once Local Network permission is granted: why.
    Deferred(String),
}

/// What the always-on ALab steward's standby server step reads as (`sync.steward.server.standby`, §8.3, C7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StewardStandbyServerRead {
    /// Not applicable on this Mac (not an `always_on` steward, off the ALab network, or no policy).
    Idle(String),
    /// A vault server job is already running on this steward.
    Current(String),
    /// No vault server job is running on this always-on steward yet.
    Due {
        /// What it finds.
        found: String,
        /// What its apply starts.
        makes: String,
    },
}

/// What ALab Commons reads as (`sync.shared-data`): a member enrolls itself once the control
/// record reaches it, and an open vault's only admin founds the vault's Commons where it has
/// none ([`Ports::found_commons`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedRead {
    /// `[commons] enabled = false`.
    Off,
    /// No control record reaches this Mac, and it is not this Mac's to found: why.
    Unavailable(String),
    /// This Mac, its open vault's only admin, founds the vault's Commons: what it finds.
    Foundable(String),
    /// It would, once a person gives what the founding waits on (Local Network, or an approver
    /// that shares the Commons root on an open approval): why. It is never asked here.
    Deferred(String),
    /// The control record is here: what it reads, in one line.
    Visible(String),
}

/// What this Mac's credential requests read as (`sync.requests`,
/// docs/DESIGN-credential-requests-2026-10-27.md §9.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestsRead {
    /// A pass cannot reach the lab's table here (Commons off, not visible, or this Mac not a
    /// Writer): why, and where its needs stand.
    Idle(String),
    /// A pass posts this Mac's needs, takes in the lab's answers and sweeps the other Macs'
    /// requests: where its needs stand, in one line.
    Due(String),
}

/// What the AI accounts roster reads as (`sync.accounts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountsRead {
    /// `[harness] accounts = "off"`.
    Off,
    /// No fleet accounts source is configured or found: why.
    NoSource(String),
    /// The roster is in step with its sources: what it reads.
    Current(String),
    /// Its sources changed or it is missing: what it finds.
    Due(String),
}

/// Value-free readiness of the shared Developer ID material on this member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseRead {
    /// This platform or vault does not participate.
    NotApplicable(String),
    /// A real signing proof covers the current material and local keychain.
    Current(String),
    /// Automatic credential delivery or keychain repair remains due.
    Due(String),
}

/// What the message bus reads as (`sync.fabric`): on by default, as `aterm fabric on` turns it on,
/// where the person never turned it off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FabricRead {
    /// Nothing of the fabric is this pass's: why (the person turned it off, say).
    NotApplicable(String),
    /// The broker runs this build: what it reads.
    Current(String),
    /// The broker is due a restart by the job `aterm fabric on` installed: it runs a build an
    /// update replaced, or none holds its socket while that job is not loaded or runs none (an
    /// update's restart that left it unloaded): which.
    Due(String),
    /// It is off and the person never turned it off (no `aterm fabric off` record): what was
    /// found. A pass with an aterm window turns it on ([`Ports::turn_on_fabric`]).
    Unset(String),
}

/// What this Mac's vault membership reads as (`sync.peers`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeersRead {
    /// A vault is configured: which, in one line.
    Joined(String),
    /// An open vault answered and asks nothing to join: which.
    Joinable(String),
    /// A join waits on a person (a password, Local Network): why. It is never asked here.
    Deferred(String),
    /// No vault is joined here, and none is this pass's to join: why (a person's choice, or no
    /// ALab vault on this network).
    Off(String),
}

/// How `sync.peers`' join ended ([`peers::join`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeersDone {
    /// The vault let this Mac in: what was joined.
    Joined(String),
    /// The vault keeps the request and asks more than a pass gives (a join password, an admin's
    /// approval): why. The pass stands aside, waiting on that person.
    Waiting(String),
}

/// What the pass reads and changes, each through one port so a test runs it inert.
pub trait Ports: Sync {
    /// Lower this pass's scheduling to background work ([`background_self`]): a runner a window
    /// or the subscriber spawned does not inherit their QoS, so it lowers its own as it starts.
    fn background(&self);
    /// Whether this process runs as its account's own login, in its home (not root, a private
    /// state root or another home): a pass refuses otherwise, as `aterm init` does.
    fn own_account(&self) -> bool;
    /// Whether this login owns this Mac's console (`aterm init`'s context).
    ///
    /// # Errors
    /// This login's context cannot be read.
    fn at_console(&self) -> Result<bool, String>;
    /// The account's state directory, which holds the record, the lock and the journal.
    fn state_dir(&self) -> Option<PathBuf>;
    /// The running build (`<version>+g<commit>`).
    fn build(&self) -> String;
    /// The addresses this Mac's traffic leaves by, each rendered (`-` without a route).
    fn routes(&self) -> Vec<String>;
    /// Now, in ms since the epoch.
    fn now_ms(&self) -> u64;
    /// Draw a uniform random wait in `0..=max` for `sync.fleet`'s join jitter and capacity
    /// backoff. Overridden in tests for deterministic virtual-clock simulation.
    fn random_wait(&self, max: Duration) -> Duration {
        random_duration_up_to(max, self.now_ms())
    }
    /// Pause this pass for `duration` (real sleep in `Live`, recorded or virtual-clock in tests).
    fn sleep(&self, duration: Duration);
    /// Whether this process is the installed release aterm.app's (not a build, a renamed or held
    /// copy): only it re-lays the base layer.
    fn installed_release(&self) -> bool;
    /// Move an unpinned managed vault subscriber onto this installed release when due.
    /// Called only by an apply pass, after its account/lock checks, with swarm enabled.
    /// `Some` describes a completed move; `None` means no move was needed.
    ///
    /// # Errors
    /// The job could not be safely read or moved.
    fn refresh_vault_subscriber(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Whether an aterm window runs for this login now: its upkeep then keeps the AI accounts
    /// roster, so the subscriber's pass leaves it alone.
    fn window_runs(&self) -> bool;
    /// Whether `[swarm] enabled` in `aterm.toml` leaves swarm participation on: `false` makes
    /// every sync row read `NotApplicable` before touching its subsystem.
    ///
    /// # Errors
    /// `aterm.toml` cannot be read or has an invalid `[swarm]` section.
    fn swarm_enabled(&self) -> Result<bool, String>;
    /// The base layer's repair items, the store lock as `lock` says.
    fn base(&self, lock: BaseLock) -> Base;
    /// `sync.vault`'s read.
    ///
    /// # Errors
    /// The vault's local state cannot be read.
    fn vault(&self) -> Result<VaultRead, String>;
    /// `sync.identity`'s read.
    ///
    /// # Errors
    /// The vault's local state, swarm roster or `aterm.toml` cannot be read.
    fn identity(&self) -> Result<IdentityRead, String>;
    /// `sync.identity`'s apply: writes the resolved swarm label into `[ai] machine` and `[fleet]
    /// machine` in `aterm.toml`.
    ///
    /// # Errors
    /// The label could not be resolved or `aterm.toml` could not be updated.
    fn sync_identity(&self) -> Result<String, String>;
    /// `sync.fleet`'s read.
    ///
    /// # Errors
    /// The vault's copy or the fleet's state cannot be read.
    fn fleet(&self) -> Result<FleetRead, String>;
    /// `sync.fleet`'s apply: a sync from the vault copy already applied.
    ///
    /// # Errors
    /// The sync did not finish (the issuer did not answer, say).
    fn sync_fleet(&self) -> Result<FleetDone, String>;

    /// `sync.node`'s read (`systems.alab.aterm-node`, C20).
    ///
    /// # Errors
    /// Local state or supervisor status cannot be read.
    fn node(&self) -> Result<NodeRead, String> {
        Ok(NodeRead::NotApplicable(
            "this Mac is in no fleet (sync.fleet)".to_owned(),
        ))
    }

    /// `sync.node`'s apply: install, update, or start `systems.alab.aterm-node`.
    ///
    /// # Errors
    /// Writing or starting the LaunchAgent/systemd user unit failed.
    fn ensure_node(&self) -> Result<String, String> {
        Ok("nothing changed".to_owned())
    }

    /// What the steward's revoke-repair step finds (`sync.steward.revoke`), writing nothing; `by`
    /// says whose network its publish goes out on.
    ///
    /// # Errors
    /// Local vault state, roster, or pool entries cannot be read.
    fn steward_revoke(&self, _by: Publisher) -> Result<StewardRevokeRead, String> {
        Ok(StewardRevokeRead::Idle(
            "this Mac holds no admin key of the ALab vault".to_owned(),
        ))
    }

    /// Finish any half-done revoke cascade in one vault publish (`sync.steward.revoke`).
    ///
    /// # Errors
    /// Vault open or publication did not finish, for the next pass.
    fn apply_steward_revoke(&self) -> Result<String, String> {
        Ok("every revoked Mac's roster and pool cascade is complete".to_owned())
    }

    /// What the always-on steward's lease step finds (`sync.steward.lease`), writing nothing; `by`
    /// says whose network its publish goes out on.
    ///
    /// # Errors
    /// Local vault state, policy, or lease entry cannot be read.
    fn steward_lease(&self, _by: Publisher) -> Result<StewardLeaseRead, String> {
        Ok(StewardLeaseRead::Idle(
            "this Mac holds no admin key of the ALab vault".to_owned(),
        ))
    }

    /// Claim, renew, or take over `aterm-swarm-lease` via parent-digest CAS (`sync.steward.lease`).
    ///
    /// # Errors
    /// Vault open or CAS publication did not finish, for the next pass.
    fn apply_steward_lease(&self) -> Result<String, String> {
        Ok("lease stands".to_owned())
    }

    /// What the always-on steward's standby vault server step finds (`sync.steward.server.standby`).
    ///
    /// # Errors
    /// Local vault state, policy, or service state cannot be read.
    fn steward_standby_server(&self) -> Result<StewardStandbyServerRead, String> {
        Ok(StewardStandbyServerRead::Idle(
            "this Mac holds no admin key of the ALab vault".to_owned(),
        ))
    }

    /// Ensure the standby vault server runs on this always-on steward (`sync.steward.server.standby`).
    ///
    /// # Errors
    /// Service installation or startup did not finish, for the next pass.
    fn apply_steward_standby_server(&self) -> Result<String, String> {
        Ok("standby vault server runs".to_owned())
    }
    /// `sync.shared-data`'s read; `by` says whose network a founding's publish goes out on.
    ///
    /// # Errors
    /// The Commons setting or the vault's copy cannot be read.
    fn shared_data(&self, by: Publisher) -> Result<SharedRead, String>;
    /// `sync.requests`'s read: the need ledger and whether this Mac is a Commons Writer.
    ///
    /// # Errors
    /// The Commons setting or the need ledger cannot be read.
    fn requests(&self) -> Result<RequestsRead, String>;
    /// `sync.requests`'s apply: this Mac's pass over the lab's table, and the custodian's on a
    /// Mac that can sign with the vault's key.
    ///
    /// # Errors
    /// The table or the ledger cannot be read or written.
    fn pass_requests(&self) -> Result<String, String>;
    /// `sync.shared-data`'s apply: the founding of the vault's Commons by its only admin, under
    /// the authority the admin already holds; what it did, naming each Mac it approved.
    ///
    /// # Errors
    /// The founding did not finish (the vault did not take the publication, say).
    fn found_commons(&self) -> Result<String, String>;
    /// `sync.accounts`'s read.
    ///
    /// # Errors
    /// The roster or its sources cannot be read.
    fn accounts(&self) -> Result<AccountsRead, String>;
    /// `sync.accounts`'s apply: a resync with no sign-in.
    ///
    /// # Errors
    /// The resync failed.
    fn resync_accounts(&self) -> Result<String, String>;
    /// Read the current automatic signing proof without changing local state.
    fn release(&self) -> Result<ReleaseRead, String> {
        Ok(ReleaseRead::NotApplicable(
            "release signing is unavailable".into(),
        ))
    }
    /// Prepare the delivered signing pair and prove an unattended signature.
    fn sync_release(&self) -> Result<String, String> {
        Ok("release signing is unavailable".into())
    }
    /// `sync.fabric`'s read.
    ///
    /// # Errors
    /// The fabric's config or broker cannot be read.
    fn fabric(&self) -> Result<FabricRead, String>;
    /// `sync.fabric`'s apply where the broker is [`FabricRead::Due`]: the broker renewed on this
    /// build.
    ///
    /// # Errors
    /// The renewal failed.
    fn renew_broker(&self) -> Result<String, String>;
    /// `sync.fabric`'s apply where the fabric is [`FabricRead::Unset`]: `aterm fabric on`'s steps,
    /// which ask nothing.
    ///
    /// # Errors
    /// A step failed.
    fn turn_on_fabric(&self) -> Result<String, String>;
    /// `sync.peers`'s read; `look` says whether it may look for the open vault on this network
    /// (the window's pass, the one that joins it, where Local Network is aterm.app's, when it
    /// applies: a look keeps what it heard, which a plan never writes).
    ///
    /// # Errors
    /// The vault's local state cannot be read.
    fn peers(&self, look: bool) -> Result<PeersRead, String>;
    /// `sync.peers`'s apply: the open vault's join, with nothing typed.
    ///
    /// # Errors
    /// The join did not finish.
    fn join_peers(&self) -> Result<PeersDone, String>;
    /// Write `line` to aterm.log: a pass's ledger line, or why it was refused.
    fn log(&self, line: &str);
    /// The installed release aterm.app's program where it is installed and this process is not
    /// it (a held copy a vault subscriber's job runs, say): the subscriber's pass runs from there
    /// (`aterm init --converge`'s hand-off), so the rows a held copy applies are the installed
    /// app's, as the window's are. `None` where this process is it, or none is found.
    fn installed_release_elsewhere(&self) -> Option<PathBuf>;
    /// Run `program` with `args` and wait for it: its exit status, `None` where a signal ended it.
    ///
    /// # Errors
    /// It could not be started or waited for.
    fn run_elsewhere(&self, program: &Path, args: &[String]) -> Result<Option<i32>, String>;
}

/// Lower the calling thread, and the threads it starts after, to the utility QoS class (macOS;
/// elsewhere nothing): the pass is work nobody waits on, so it yields to a person's typing.
pub fn background_self() {
    #[cfg(target_os = "macos")]
    // SAFETY: sets this thread's own QoS class. Takes no pointers and mutates no shared state,
    // so there is nothing for another thread to observe.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

/// One sync kind's item over [`Ports`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Peers,
    Vault,
    Identity,
    StewardRevoke,
    StewardLease,
    StewardServerStandby,
    Fleet,
    Node,
    SharedData,
    Requests,
    Accounts,
    Release,
    Fabric,
}

struct Synced<'a> {
    kind: Kind,
    ports: &'a dyn Ports,
    trigger: Trigger,
    /// Whether this pass applies: a plan (`--check`) writes nothing, so it does not look.
    mode: Mode,
    /// What the last apply said, the row's detail.
    said: Mutex<Option<String>>,
    /// Why the last apply stood aside.
    aside: Mutex<Option<StoodAside>>,
}

impl<'a> Synced<'a> {
    fn new(kind: Kind, ports: &'a dyn Ports, trigger: Trigger, mode: Mode) -> Self {
        Self {
            kind,
            ports,
            trigger,
            mode,
            said: Mutex::new(None),
            aside: Mutex::new(None),
        }
    }

    fn say(&self, said: String) {
        *self.said.lock().unwrap_or_else(PoisonError::into_inner) = Some(said);
    }

    fn stand_aside(&self, aside: StoodAside) {
        *self.aside.lock().unwrap_or_else(PoisonError::into_inner) = Some(aside);
    }

    /// Whether this pass turns on a fabric nobody turned off: one an aterm window runs beside,
    /// never the vault subscriber's. That program can be a held copy (on the vault's admin Mac,
    /// a snapshot outside any app), which must never become the fabric's program, and a Mac with
    /// no window has no bridge for a broker to serve. Headless, CI and private-state-root
    /// instances start no window's pass, and a pass refuses a private state root outright.
    fn turns_fabric_on(&self) -> bool {
        self.trigger != Trigger::Subscriber && self.ports.window_runs()
    }

    /// Whose network this runner's publications use: a held subscriber fallback keeps its
    /// job's network; the installed app uses aterm's, even with a subscriber trigger.
    fn publisher(&self) -> Publisher {
        if self.trigger == Trigger::Subscriber && !self.ports.installed_release() {
            Publisher::Subscriber
        } else {
            Publisher::Aterm
        }
    }
}

fn not_applicable(why: impl Into<String>) -> Probe {
    Probe::NotApplicable { why: why.into() }
}

fn read<T>(read: Result<T, String>, probe: impl FnOnce(T) -> Probe) -> Probe {
    read.map_or_else(|why| Probe::Unusable { why }, probe)
}

fn random_duration_up_to(max: Duration, fallback_seed: u64) -> Duration {
    let max_ms = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
    if max_ms == 0 {
        return Duration::ZERO;
    }
    let mut bytes = [0u8; 8];
    let raw = if aterm_uds::rand::fill(&mut bytes).is_ok() {
        u64::from_le_bytes(bytes)
    } else {
        fallback_seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    };
    let span = max_ms.saturating_add(1);
    let pick = if span == 0 { raw } else { raw % span };
    Duration::from_millis(pick)
}

fn sync_fleet_with_retry(ports: &dyn Ports, trigger: Trigger) -> Result<FleetDone, String> {
    let mut waited = Duration::ZERO;
    if trigger != Trigger::Init {
        let jitter = ports
            .random_wait(FLEET_JOIN_JITTER_MAX)
            .min(FLEET_JOIN_JITTER_MAX);
        if !jitter.is_zero() {
            ports.sleep(jitter);
            waited += jitter;
        }
    }
    let mut attempt = 0;
    loop {
        match ports.sync_fleet() {
            Ok(FleetDone::AtCapacity(why)) => {
                let remaining = FLEET_JOIN_RETRY_BUDGET.saturating_sub(waited);
                let Some(&cap_secs) = FLEET_CAPACITY_BACKOFF_SECS.get(attempt) else {
                    return Err(why);
                };
                if remaining.is_zero() {
                    return Err(why);
                }
                let cap = Duration::from_secs(cap_secs);
                let half = cap / 2;
                let extra = ports.random_wait(cap - half).min(cap - half);
                let delay = (half + extra).min(remaining);
                ports.sleep(delay);
                waited += delay;
                attempt += 1;
            }
            other => return other,
        }
    }
}

impl Item for Synced<'_> {
    fn id(&self) -> String {
        match self.kind {
            Kind::Peers => PEERS,
            Kind::Vault => VAULT,
            Kind::Identity => IDENTITY,
            Kind::StewardRevoke => STEWARD_REVOKE,
            Kind::StewardLease => STEWARD_LEASE,
            Kind::StewardServerStandby => STEWARD_SERVER_STANDBY,
            Kind::Fleet => FLEET,
            Kind::Node => NODE,
            Kind::SharedData => SHARED_DATA,
            Kind::Requests => REQUESTS,
            Kind::Accounts => ACCOUNTS,
            Kind::Release => RELEASE,
            Kind::Fabric => FABRIC,
        }
        .to_owned()
    }

    fn family(&self) -> &'static str {
        FAMILY
    }

    fn layer(&self) -> Layer {
        Layer::Base
    }

    fn class(&self) -> Class {
        match self.kind {
            Kind::Vault
            | Kind::Identity
            | Kind::StewardRevoke
            | Kind::StewardLease
            | Kind::StewardServerStandby
            | Kind::Node
            | Kind::Requests
            | Kind::Release
            | Kind::Fabric => Class::Relay,
            Kind::Peers | Kind::Fleet | Kind::SharedData | Kind::Accounts => Class::Install,
        }
    }

    fn title(&self) -> String {
        match self.kind {
            Kind::Peers => "this Mac's vault",
            Kind::Vault => "the vault's subscriber",
            Kind::Identity => "this Mac's swarm label",
            Kind::StewardRevoke => "the revoke cascade, finished by the ALab steward",
            Kind::StewardLease => "the ALab steward lease",
            Kind::StewardServerStandby => "the standby ALab vault server",
            Kind::Fleet => "the default ALab fleet",
            Kind::Node => "the fleet and swarm node job",
            Kind::SharedData => "shared data (ALab Commons)",
            Kind::Requests => "this Mac's credential requests",
            Kind::Accounts => "the AI accounts roster",
            Kind::Release => "shared release signing",
            Kind::Fabric => "the message bus (fabric)",
        }
        .to_owned()
    }

    fn probe(&self) -> Probe {
        match self.ports.swarm_enabled() {
            Ok(true) => {}
            Ok(false) => return not_applicable(SWARM_OFF),
            Err(why) => return Probe::Unusable { why },
        }
        match self.kind {
            Kind::Peers => {
                // Only the window's pass joins, so only it looks for an open vault, and only
                // when it applies: the look keeps what it heard (the network's verdict, the
                // Local Network answer), and a plan (`--check`) writes nothing.
                let look = self.trigger == Trigger::Window && self.mode == Mode::Apply;
                read(self.ports.peers(look), |peers| match peers {
                    PeersRead::Joined(now) => Probe::Ok { now },
                    PeersRead::Deferred(why) => Probe::Deferred { why },
                    PeersRead::Off(why) => not_applicable(why),
                    PeersRead::Joinable(from) if look => Probe::Change {
                        from,
                        to: "joined, nothing typed".to_owned(),
                    },
                    PeersRead::Joinable(from) => Probe::Deferred {
                        why: format!(
                            "{from}: the aterm window joins it, where Local Network is aterm.app's"
                        ),
                    },
                })
            }
            Kind::Vault => read(self.ports.vault(), |vault| match vault {
                VaultRead::NoVault => not_applicable("no vault is configured (sync.peers)"),
                VaultRead::Off => not_applicable(
                    "sync is off on this Mac by your choice: `aterm secrets agent on` turns it on",
                ),
                VaultRead::Runs(now) => Probe::Ok { now },
                VaultRead::Idle(why) => Probe::Deferred {
                    why: format!(
                        "{why}: the aterm window's mend repairs it at launch and every 30 minutes"
                    ),
                },
            }),
            Kind::Identity => read(self.ports.identity(), |identity| match identity {
                IdentityRead::NotApplicable(why) => not_applicable(why),
                IdentityRead::Current(now) => Probe::Ok { now },
                IdentityRead::Due { from, to } => Probe::Change { from, to },
            }),
            Kind::StewardRevoke => {
                read(
                    self.ports.steward_revoke(self.publisher()),
                    |revoke| match revoke {
                        StewardRevokeRead::Idle(why) => not_applicable(why),
                        StewardRevokeRead::Current(now) => Probe::Ok { now },
                        StewardRevokeRead::Due { found, makes } => Probe::Change {
                            from: found,
                            to: makes,
                        },
                        StewardRevokeRead::Deferred(why) => Probe::Deferred { why },
                    },
                )
            }
            Kind::StewardLease => {
                read(
                    self.ports.steward_lease(self.publisher()),
                    |lease| match lease {
                        StewardLeaseRead::Idle(why) => not_applicable(why),
                        StewardLeaseRead::Current(now) => Probe::Ok { now },
                        StewardLeaseRead::Due { found, makes } => Probe::Change {
                            from: found,
                            to: makes,
                        },
                        StewardLeaseRead::Deferred(why) => Probe::Deferred { why },
                    },
                )
            }
            Kind::StewardServerStandby => {
                read(
                    self.ports.steward_standby_server(),
                    |standby| match standby {
                        StewardStandbyServerRead::Idle(why) => not_applicable(why),
                        StewardStandbyServerRead::Current(now) => Probe::Ok { now },
                        StewardStandbyServerRead::Due { found, makes } => Probe::Change {
                            from: found,
                            to: makes,
                        },
                    },
                )
            }
            Kind::Fleet => read(self.ports.fleet(), |fleet| match fleet {
                FleetRead::Idle(why) => not_applicable(why),
                FleetRead::Current(now) => Probe::Ok { now },
                FleetRead::Due(from) => Probe::Change {
                    from,
                    to: "in the vault's default fleet".to_owned(),
                },
            }),
            Kind::Node => read(self.ports.node(), |node| match node {
                NodeRead::NotApplicable(why) => not_applicable(why),
                NodeRead::Current(now) => Probe::Ok { now },
                NodeRead::Due { from, to } => Probe::Change { from, to },
            }),
            Kind::SharedData => read(
                self.ports.shared_data(self.publisher()),
                |shared| match shared {
                    SharedRead::Off => not_applicable("off: [commons] enabled = false"),
                    SharedRead::Unavailable(why) => not_applicable(why),
                    SharedRead::Foundable(from) => Probe::Change {
                        from,
                        to: "founded by this Mac, under the authority it holds".to_owned(),
                    },
                    SharedRead::Deferred(why) => Probe::Deferred { why },
                    SharedRead::Visible(now) => Probe::Ok { now },
                },
            ),
            Kind::Requests => read(self.ports.requests(), |requests| match requests {
                RequestsRead::Idle(why) => not_applicable(why),
                RequestsRead::Due(now) if self.detail().is_some() => Probe::Ok { now },
                RequestsRead::Due(from) => Probe::Change {
                    from,
                    to: "posted, answered and swept on the lab's table".to_owned(),
                },
            }),
            Kind::Accounts if self.trigger.in_window() => {
                not_applicable("the aterm window's upkeep keeps the roster in step")
            }
            // One writer of the roster: the subscriber's pass resyncs only with no window.
            Kind::Accounts if self.trigger == Trigger::Subscriber && self.ports.window_runs() => {
                not_applicable("an aterm window runs, and its upkeep keeps the roster in step")
            }
            Kind::Accounts => read(self.ports.accounts(), |accounts| match accounts {
                AccountsRead::Off => not_applicable("off: [harness] accounts = \"off\""),
                AccountsRead::NoSource(why) => not_applicable(why),
                AccountsRead::Current(now) => Probe::Ok { now },
                AccountsRead::Due(from) => Probe::Change {
                    from,
                    to: "synced from its sources, with no sign-in".to_owned(),
                },
            }),
            Kind::Release => read(self.ports.release(), |release| match release {
                ReleaseRead::NotApplicable(why) => not_applicable(why),
                ReleaseRead::Current(now) => Probe::Ok { now },
                ReleaseRead::Due(from) => Probe::Change {
                    from,
                    to: "private keychain prepared and unattended signature proved".into(),
                },
            }),
            Kind::Fabric => read(self.ports.fabric(), |fabric| match fabric {
                FabricRead::NotApplicable(why) => not_applicable(why),
                FabricRead::Current(now) => Probe::Ok { now },
                FabricRead::Due(from) => Probe::Change {
                    from,
                    to: "the broker runs this build, under the job `aterm fabric on` installed"
                        .to_owned(),
                },
                FabricRead::Unset(from) if self.turns_fabric_on() => Probe::Change {
                    from,
                    to: "on, as `aterm fabric on` turns it on".to_owned(),
                },
                FabricRead::Unset(from) => not_applicable(format!(
                    "{from}: a pass an aterm window starts turns it on (this one runs {})",
                    if self.trigger == Trigger::Subscriber {
                        "from the vault's subscriber"
                    } else {
                        "with no aterm window"
                    }
                )),
            }),
        }
    }

    fn apply(&self) -> Result<(), String> {
        let said = match self.kind {
            Kind::Peers => match self.ports.join_peers()? {
                PeersDone::Joined(said) => said,
                PeersDone::Waiting(why) => {
                    self.stand_aside(StoodAside::Waiting(why));
                    return Ok(());
                }
            },
            Kind::Identity => self.ports.sync_identity()?,
            Kind::StewardRevoke => self
                .ports
                .apply_steward_revoke()
                .map_err(|why| format!("{why}; the next pass tries again"))?,
            Kind::StewardLease => self
                .ports
                .apply_steward_lease()
                .map_err(|why| format!("{why}; the next pass tries again"))?,
            Kind::StewardServerStandby => self
                .ports
                .apply_steward_standby_server()
                .map_err(|why| format!("{why}; the next pass tries again"))?,
            Kind::Fleet => match sync_fleet_with_retry(self.ports, self.trigger) {
                Ok(FleetDone::Synced(said)) => said,
                Ok(FleetDone::Blocked(why)) => {
                    self.stand_aside(StoodAside::Waiting(why));
                    return Ok(());
                }
                Ok(FleetDone::AtCapacity(why)) | Err(why) => {
                    return Err(format!("{why}; the next pass tries again"));
                }
            },
            Kind::Node => self
                .ports
                .ensure_node()
                .map_err(|why| format!("{why}; the next pass tries again"))?,
            Kind::Requests => self
                .ports
                .pass_requests()
                .map_err(|why| format!("{why}; the next pass tries again"))?,
            Kind::SharedData => self
                .ports
                .found_commons()
                .map_err(|why| format!("{why}; the next pass tries again"))?,
            Kind::Accounts => self.ports.resync_accounts()?,
            Kind::Release => self
                .ports
                .sync_release()
                .map_err(|why| format!("{why}; the next pass tries again"))?,
            // Read again: the probe planned one of the two, and only it changes the fabric.
            Kind::Fabric => match self.ports.fabric()? {
                FabricRead::Due(_) => self.ports.renew_broker()?,
                FabricRead::Unset(_) if self.turns_fabric_on() => self.ports.turn_on_fabric()?,
                FabricRead::Unset(_) | FabricRead::Current(_) | FabricRead::NotApplicable(_) => {
                    return Ok(());
                }
            },
            // Report only: its probe never plans a change.
            Kind::Vault => return Ok(()),
        };
        self.say(said);
        Ok(())
    }

    fn explain(&self, probe: &Probe) -> Explain {
        let why = match probe {
            Probe::Ok { now } => now.clone(),
            Probe::Change { from, to } => format!("{from}; a pass makes it {to}"),
            Probe::Person { how, .. } => how.clone(),
            Probe::Blocked { why }
            | Probe::Waits { why, .. }
            | Probe::NotApplicable { why }
            | Probe::Deferred { why }
            | Probe::Unusable { why } => why.clone(),
        };
        let remedy = match self.kind {
            Kind::Peers => Remedy::Command("aterm init".to_owned()),
            Kind::Vault => Remedy::Command("aterm secrets agent on".to_owned()),
            Kind::Identity => Remedy::Command("aterm init --converge".to_owned()),
            Kind::StewardRevoke => Remedy::Command("aterm init --converge".to_owned()),
            Kind::StewardLease => Remedy::Command("aterm init --converge".to_owned()),
            Kind::StewardServerStandby => Remedy::Command("aterm init --converge".to_owned()),
            Kind::Fleet => Remedy::Command("aterm fleet sync".to_owned()),
            Kind::Node => Remedy::Command("aterm node on".to_owned()),
            Kind::SharedData => Remedy::None,
            Kind::Requests => Remedy::Command("aterm secrets need pass".to_owned()),
            Kind::Accounts => Remedy::Init {
                only: "base.ai.accounts".to_owned(),
            },
            Kind::Release => Remedy::None,
            Kind::Fabric => Remedy::Command("aterm fabric on".to_owned()),
        };
        Explain { why, remedy }
    }

    fn detail(&self) -> Option<String> {
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The row that stands for a base layer the pass leaves as it is, and why.
struct BaseLeft(&'static str);

impl Item for BaseLeft {
    fn id(&self) -> String {
        BASE.to_owned()
    }
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn layer(&self) -> Layer {
        Layer::Base
    }
    fn class(&self) -> Class {
        Class::Relay
    }
    fn title(&self) -> String {
        "the base layer".to_owned()
    }
    fn probe(&self) -> Probe {
        not_applicable(self.0)
    }
    fn apply(&self) -> Result<(), String> {
        Ok(())
    }
    fn explain(&self, _probe: &Probe) -> Explain {
        Explain {
            why: self.0.to_owned(),
            remedy: Remedy::Repair,
        }
    }
}

/// Why a pass leaves the base layer to `aterm doctor` on a build the installed app laid it on.
const BASE_ONCE: &str =
    "the base layer is re-laid once per new build: `aterm doctor` repairs it now";

/// Why a pass leaves one repair item as it is on a build that laid it, while it lays another
/// that an earlier pass on that build left.
const BASE_LAID: &str = "laid once on this build already: `aterm doctor` repairs it now";

/// Why a pass leaves the base layer to `aterm doctor` from a build or a held copy.
#[cfg(target_os = "macos")]
const BASE_NOT_INSTALLED: &str = "this aterm is not the installed release app (a build or a held \
                                  copy), so the base layer is `aterm doctor`'s";

/// Why a pass leaves the base layer to `aterm doctor` off macOS, where no aterm is the installed
/// release app (`atpkg::hooks::is_installed_release_exe` admits an aterm.app's alone).
#[cfg(not(target_os = "macos"))]
const BASE_NOT_INSTALLED: &str = "a pass re-lays the base layer from the installed aterm.app \
                                  alone, so on this system it is `aterm doctor`'s";

/// A run's items.
type Items<'a> = Vec<Box<dyn Item + 'a>>;

/// The store lock a base layer holds while its items run ([`Base::held`]).
type Held = Option<Box<dyn std::any::Any>>;

/// What the base layer still owes a build ([`owed`]).
enum Owed<'r> {
    /// The installed app laid none on it.
    All,
    /// A pass on it left these repair items.
    Left(&'r [String]),
    /// It is laid.
    Nothing,
}

/// What the base layer owes `build` by `record`'s [`Record::base`].
fn owed<'r>(record: Option<&'r Record>, build: &str) -> Owed<'r> {
    match record.and_then(|record| record.base.as_ref()) {
        Some(base) if base.build == build && base.pending.is_empty() => Owed::Nothing,
        Some(base) if base.build == build => Owed::Left(&base.pending),
        _ => Owed::All,
    }
}

/// The base layer a pass runs ([`base_layer`]).
struct BaseRun<'a> {
    /// Its items, or the one row that says why it is left.
    items: Items<'a>,
    /// The store lock, held while the items run.
    held: Held,
    /// Whether the repair items run, so the pass records what they leave ([`BaseRecord`]).
    lays: bool,
}

/// The base layer a pass runs, with the store lock that holds while it does: from the installed
/// release app alone, what `owed` says its build still lacks. That is every repair item on a
/// build it laid none on, or the items an earlier pass on that build left (the rest are left as
/// they are, so a block a person took out of their shell rc is never put back on the same build).
/// It never runs the Install class (the store's own builds, which only an install puts back).
/// Otherwise one row says why the layer is left.
fn base_layer<'a>(ports: &'a dyn Ports, owed: &Owed<'_>, mode: Mode) -> BaseRun<'a> {
    let left = |why: &'static str| BaseRun {
        items: vec![Box::new(BaseLeft(why)) as Box<dyn Item>],
        held: None,
        lays: false,
    };
    if !ports.installed_release() {
        return left(BASE_NOT_INSTALLED);
    }
    let pending = match owed {
        Owed::Nothing => return left(BASE_ONCE),
        Owed::All => None,
        Owed::Left(pending) => Some(*pending),
    };
    let lock = match mode {
        Mode::Apply => BaseLock::Try,
        Mode::Plan => BaseLock::Read,
    };
    let Base { items, held, .. } = ports.base(lock);
    let items = items
        .into_iter()
        .filter(|item| item.class() != Class::Install)
        .flat_map(|item| match pending {
            // Laid on this build already. An item of a class the pass never applies is a
            // person's and stays planned.
            Some(pending) if APPLIES.contains(&item.class()) && !pending.contains(&item.id()) => {
                aterm_converge::left_to_another(vec![item], BASE_LAID)
            }
            _ => vec![item],
        })
        .map(|item| item as Box<dyn Item + 'a>)
        .collect();
    BaseRun {
        items,
        held,
        lays: true,
    }
}

/// The base rows a pass leaves for the next one on its build: each of a class the pass applies
/// that did not end right (it failed, stood aside on a held store, or was held back). A row of a
/// class the pass never applies waits on a person, not on a pass.
fn left_for_next(rows: &[ItemReport]) -> Vec<String> {
    rows.iter()
        .filter(|row| row.class.is_some_and(|class| APPLIES.contains(&class)))
        .filter(|row| {
            !matches!(
                row.outcome,
                Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
            )
        })
        .map(|row| row.id.clone())
        .collect()
}

/// [`Record::builds`] once `build` ran a pass after `record`: each build once, where it last
/// ran, the oldest dropped past [`BUILDS_KEPT`]; a new [`SPEC`] starts it again.
fn builds_after(record: Option<&Record>, build: &str) -> Vec<String> {
    let mut builds = record
        .filter(|record| record.spec == SPEC)
        .map(|record| {
            let mut builds = record.builds.clone();
            if !builds.contains(&record.build) {
                builds.push(record.build.clone());
            }
            builds
        })
        .unwrap_or_default();
    builds.retain(|ran| ran != build);
    builds.push(build.to_owned());
    let over = builds.len().saturating_sub(BUILDS_KEPT);
    builds.drain(..over);
    builds
}

/// The sync kinds, in run order: the vault before what it brings.
fn sync_items<'a>(ports: &'a dyn Ports, trigger: Trigger, mode: Mode) -> Items<'a> {
    [
        Kind::Peers,
        Kind::Vault,
        Kind::Identity,
        Kind::StewardRevoke,
        Kind::StewardLease,
        Kind::StewardServerStandby,
        Kind::Fleet,
        Kind::Node,
        Kind::SharedData,
        Kind::Requests,
        Kind::Accounts,
        Kind::Release,
        Kind::Fabric,
    ]
    .into_iter()
    .map(|kind| Box::new(Synced::new(kind, ports, trigger, mode)) as Box<dyn Item + 'a>)
    .collect()
}

/// Why a pass did not run ([`converge`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// Another pass holds the lock (exit 75).
    Held,
    /// It cannot run here: why.
    Refused(String),
}

/// One pass that ran ([`converge`]).
#[derive(Debug)]
pub struct Pass {
    /// Why it was due.
    pub due: Due,
    /// Each row, in run order.
    pub rows: Vec<ItemReport>,
    /// Why the record could not be written, where it could not.
    pub unrecorded: Option<String>,
}

/// Run one pass for `trigger`: `mode` [`Mode::Apply`] under [`try_lock`] (a held lock stops it
/// with [`Stop::Held`]), its record written and one aterm.log line; [`Mode::Plan`] probes and
/// writes nothing (no lock, record, journal or log line, and no look for the open vault, which
/// keeps what it heard). The base layer runs first, under the store lock it alone holds
/// ([`base_layer`]), which is let go before the sync kinds run. Each row reaches `on_row` as it
/// finishes.
///
/// # Errors
/// [`Stop::Held`] while another pass runs; [`Stop::Refused`] outside the account's own login,
/// where its context or state directory cannot be read, or when the items cannot be ordered. A
/// refused [`Mode::Apply`] pass writes its own aterm.log line (`refusal`): a runner's stderr goes
/// nowhere, so a Mac whose every pass is refused leaves that trace.
pub fn converge(
    ports: &dyn Ports,
    trigger: Trigger,
    mode: Mode,
    on_row: &mut dyn FnMut(&ItemReport),
) -> Result<Pass, Stop> {
    let pass = run_pass(ports, trigger, mode, on_row);
    if mode == Mode::Apply
        && let Err(Stop::Refused(why)) = &pass
    {
        ports.log(&refusal(&ports.build(), trigger, why));
    }
    pass
}

/// A refused pass's one aterm.log line: the build, the trigger and why.
fn refusal(build: &str, trigger: Trigger, why: &str) -> String {
    format!(
        "converge build={build} trigger={} refused: {why}",
        trigger.word()
    )
}

/// [`converge`]'s pass.
fn run_pass(
    ports: &dyn Ports,
    trigger: Trigger,
    mode: Mode,
    on_row: &mut dyn FnMut(&ItemReport),
) -> Result<Pass, Stop> {
    if !ports.own_account() {
        return Err(Stop::Refused(
            "run in your own login and home directory".to_owned(),
        ));
    }
    let at_console = ports.at_console().map_err(Stop::Refused)?;
    let state = ports.state_dir().ok_or_else(|| {
        Stop::Refused("this account has no state directory to record a pass in".to_owned())
    })?;
    let _lock = match mode {
        Mode::Apply => Some(try_lock(&state).map_err(Stop::Refused)?.ok_or(Stop::Held)?),
        Mode::Plan => None,
    };
    let record = read_record(&state);
    let (build, routes, now) = (ports.build(), ports.routes(), ports.now_ms());
    let due = due(record.as_ref(), &build, &routes, now);
    let scope = Scope {
        applies: APPLIES,
        at_console,
        not_own_account: None,
    };
    let mut file = FileJournal::under(&state);
    let mut read_only = aterm_converge::ReadOnly;
    let journal: &mut dyn aterm_converge::Journal = match mode {
        Mode::Apply => &mut file,
        Mode::Plan => &mut read_only,
    };
    let refused = |why: aterm_converge::Refusal| Stop::Refused(why.to_string());
    let BaseRun {
        items: base,
        held,
        lays,
    } = base_layer(ports, &owed(record.as_ref(), &build), mode);
    let mut rows =
        aterm_converge::run_scoped(&base, mode, scope, journal, on_row).map_err(refused)?;
    drop(held);
    let base = if lays {
        Some(BaseRecord {
            build: build.clone(),
            pending: left_for_next(&rows),
        })
    } else {
        record.as_ref().and_then(|record| record.base.clone())
    };
    if mode == Mode::Apply
        && ports.installed_release()
        && ports.swarm_enabled().unwrap_or(false)
        && let Some(line) = ports.refresh_vault_subscriber().map_err(Stop::Refused)?
    {
        ports.log(&format!("converge vault subscriber: {line}"));
    }
    let items = sync_items(ports, trigger, mode);
    rows.extend(aterm_converge::run_scoped(&items, mode, scope, journal, on_row).map_err(refused)?);
    if mode == Mode::Plan {
        return Ok(Pass {
            due,
            rows,
            unrecorded: None,
        });
    }
    let failed = rows.iter().any(|row| row.outcome.is_failure());
    let record = Record {
        build: build.clone(),
        spec: SPEC,
        builds: builds_after(record.as_ref(), &build),
        attempted_ms: now,
        converged_ms: if failed {
            record.and_then(|record| record.converged_ms)
        } else {
            Some(now)
        },
        routes,
        rows: rows
            .iter()
            .map(|row| RecordRow {
                id: row.id.clone(),
                outcome: row.outcome,
                why: row.why.clone(),
            })
            .collect(),
        base,
    };
    let unrecorded = write_record(&state.join(FileJournal::DIR), &record).err();
    ports.log(&ledger(&build, trigger, due, &rows, unrecorded.as_deref()));
    Ok(Pass {
        due,
        rows,
        unrecorded,
    })
}

/// The most rows the ledger line names, and the longest why it gives each: a failed row's is
/// longer, since its error is what the line is read for.
const LEDGER_ROWS: usize = 6;
const LEDGER_WHY: usize = 160;
const LEDGER_FAILED_WHY: usize = 480;

/// `why` in `max` chars at most: whole where it fits, else its head and its tail around ` … `,
/// so the error a long why ends on (a step's lines, then what failed it) is kept.
fn clipped(why: &str, max: usize) -> String {
    const CUT: &str = " … ";
    let chars = why.chars().count();
    if chars <= max {
        return why.to_owned();
    }
    let keep = max.saturating_sub(CUT.chars().count());
    let head = keep / 4;
    let mut out: String = why.chars().take(head).collect();
    out.push_str(CUT);
    out.extend(why.chars().skip(chars - (keep - head)));
    out
}

/// A pass's one aterm.log line: the build and trigger, a tally, and the rows that did not end
/// right, each with its why (`[sync.fleet: the issuer did not answer | ...]`), [`clipped`] to
/// [`LEDGER_WHY`] chars, or [`LEDGER_FAILED_WHY`] for a failed row's.
fn ledger(
    build: &str,
    trigger: Trigger,
    due: Due,
    rows: &[ItemReport],
    unrecorded: Option<&str>,
) -> String {
    let count = |pick: fn(Outcome) -> bool| rows.iter().filter(|row| pick(row.outcome)).count();
    let ok = count(|outcome| matches!(outcome, Outcome::Ok | Outcome::NotApplicable));
    let changed = count(|outcome| outcome == Outcome::Changed);
    let failed = count(Outcome::is_failure);
    let deferred = rows.len() - ok - changed - failed;
    let open: Vec<String> = rows
        .iter()
        .filter(|row| {
            !matches!(
                row.outcome,
                Outcome::Ok | Outcome::NotApplicable | Outcome::Changed
            )
        })
        .take(LEDGER_ROWS)
        .map(|row| {
            let max = if row.outcome.is_failure() {
                LEDGER_FAILED_WHY
            } else {
                LEDGER_WHY
            };
            let why = clipped(row.why.as_deref().unwrap_or("no reason given"), max);
            format!("{}: {why}", row.id)
        })
        .collect();
    let mut line = format!(
        "converge build={build} trigger={}/{} ok={ok} changed={changed} deferred={deferred} \
         failed={failed}",
        trigger.word(),
        due.word()
    );
    if !open.is_empty() {
        line.push_str(&format!(" [{}]", open.join(" | ")));
    }
    if let Some(why) = unrecorded {
        line.push_str(&format!("; record not written: {why}"));
    }
    line
}

pub mod peers;

#[cfg(test)]
pub(crate) mod tests;
