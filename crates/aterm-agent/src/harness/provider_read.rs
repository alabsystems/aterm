// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account-bound authority for provider reads (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15).
//! A natural use mints one non-clonable [`Use`]. The read consumes it, checks its account and
//! records its own probe before contacting the provider. Launch picks share a finite allowance.
//! The census guard follows the minting paths as a backstop against timers. The composing
//! executable installs the journal sink; unit tests capture probes without publishing them.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The local account a single provider operation is allowed to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    read: Read,
    home: PathBuf,
    label: Option<String>,
}

impl Account {
    /// Bind an operation to its local credential home and optional roster label.
    #[must_use]
    pub fn in_home(read: Read, home: &Path, label: Option<&str>) -> Self {
        Self {
            read,
            home: home.to_owned(),
            label: label.map(str::to_owned),
        }
    }
}

/// One launch's allowance. Moving the allowance preserves its spent reads.
pub struct Launch {
    left: usize,
    /// The accounts it has read.
    read: Vec<Account>,
}

mod sealed {
    pub trait LaunchDoor {}
    impl LaunchDoor for super::super::accounts_pool::LaunchUse {}
    impl LaunchDoor for super::super::agy_quota_store::LaunchUse {}
    impl LaunchDoor for super::super::codex_rotation::LaunchUse {}
    pub trait WallDoor {}
    impl WallDoor for super::super::agy_quota_store::WallUse {}
    impl WallDoor for super::super::codex_rotation::WallUse {}
}

pub(super) trait LaunchDoor: sealed::LaunchDoor {}
impl<T: sealed::LaunchDoor> LaunchDoor for T {}

pub(super) trait WallDoor: sealed::WallDoor {
    fn account(self) -> Account;
}

/// Account evidence obtained from a running host or its first footer.
pub struct Running {
    account: Account,
}

/// A running process handle held by the footer reader. Implementations bind the observed
/// arguments and environment to that process, rather than probing a provider for identity.
pub trait FooterHost {
    /// Whether these are the arguments and environment held by this running process handle.
    #[must_use]
    fn owns(&self, argv: &[String], env: &[String]) -> bool;
}

impl Running {
    /// Capture the account held by the running host's probe claim.
    pub fn host_use(host: &dyn crate::supervise::IdleHost) -> Option<Self> {
        Some(Self {
            account: host.take_provider_read_account()?,
        })
    }

    pub(crate) fn footer_use(footer: super::profile::FooterUse<'_>) -> Option<Self> {
        Some(Self {
            account: footer.account()?,
        })
    }
}

impl Launch {
    /// Start the allowance at the natural launch door.
    #[must_use]
    pub(super) fn launch_use(_door: impl LaunchDoor) -> Self {
        Self {
            left: super::accounts_pool::LAUNCH_READS,
            read: Vec::new(),
        }
    }

    /// Whether this launch has already read `account`: an answer with no figure leaves it due
    /// again on its records, and one launch reads a row once.
    #[must_use]
    pub(super) fn has_read(&self, account: &Account) -> bool {
        self.read.contains(account)
    }
}

/// A single, account-bound provider operation. It cannot be copied or cloned.
#[must_use]
pub struct Use {
    account: Account,
    why: InUse,
    live: bool,
}

/// An already journaled operation; only consuming a matching `Use` makes one.
pub struct Permit {
    read: Read,
    home: PathBuf,
    account: Option<String>,
    login: Option<super::journal::Key>,
    why: InUse,
}

impl Permit {
    #[must_use]
    pub(super) fn matches(&self, read: Read, home: &Path) -> bool {
        self.read == read && self.home == home
    }

    /// Records one contact this permit's use makes with its own account (agy's `/usage`, then
    /// its `/model`), before it is made: every contact is on record, never only the first.
    pub(super) fn note_contact(&self, at: i64) {
        note(&ProbeEvent {
            at,
            read: self.read,
            account: self.account.clone(),
            login: self.login,
            why: self.why,
        });
    }
}

impl Use {
    #[must_use]
    pub(super) fn permits(&self, account: &Account) -> bool {
        self.live && self.account == *account
    }
    /// A pick made by this launch, sharing its finite allowance with earlier picks.
    pub(super) fn launch_read_use(launch: &mut Launch, account: Account) -> Option<Self> {
        launch.left = launch.left.checked_sub(1)?;
        launch.read.push(account.clone());
        Some(Self {
            account,
            why: InUse::Launching,
            live: true,
        })
    }

    /// The account held by a running session's turn-end or first-footer door.
    pub fn session_read_use(running: Running) -> Self {
        Self {
            account: running.account,
            why: InUse::LiveSession,
            live: true,
        }
    }

    /// The account held by a running session's screen-wall door.
    pub(super) fn wall_read_use(wall: impl WallDoor) -> Self {
        Self {
            account: wall.account(),
            why: InUse::AtWall,
            live: true,
        }
    }

    /// The one account selected by the person's command parser.
    pub(super) fn asked_read_use(asked: super::cli::Asked) -> Self {
        Self {
            account: asked.account(),
            why: InUse::Asked,
            live: true,
        }
    }

    /// Consume the authority at the read boundary, before contacting the provider.
    /// A mismatch consumes it without recording or making a probe.
    ///
    /// # Errors
    ///
    /// The account differs from the authorized account, or this is replay-only authority.
    pub fn begin(
        self,
        account: Account,
        at: i64,
        login: Option<super::journal::Key>,
    ) -> Result<Permit, &'static str> {
        if !self.live || self.account != account {
            return Err("provider read token belongs to another account");
        }
        let permit = Permit {
            read: account.read,
            home: account.home,
            account: account.label,
            login,
            why: self.why,
        };
        permit.note_contact(at);
        Ok(permit)
    }

    /// Consume authority for this provider and credential home, retaining its roster label.
    ///
    /// # Errors
    ///
    /// The provider or home differs, or this is replay-only authority.
    pub fn begin_home(self, read: Read, home: &Path, at: i64) -> Result<Permit, &'static str> {
        if self.account.read != read || self.account.home != home {
            return Err("provider read token belongs to another account");
        }
        let account = Account::in_home(read, home, self.account.label.as_deref());
        self.begin(account, at, None)
    }

    /// Fixture authority for an isolated account. No release build exposes it.
    #[cfg(any(test, debug_assertions))]
    pub fn fixture(read: Read, home: &Path, label: Option<&str>) -> Self {
        Self {
            account: Account::in_home(read, home, label),
            why: InUse::Launching,
            live: true,
        }
    }

    /// A replay follows the recorded claim but can authorize no provider contact.
    pub(crate) fn replay_use() -> Self {
        Self {
            account: Account::in_home(Read::ClaudePanel, Path::new(""), None),
            why: InUse::LiveSession,
            live: false,
        }
    }
}

/// Which provider read ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// `/usage` typed into a live Claude Code session.
    ClaudePanel,
    /// `/usage` read in Kimi's temporary tab, a person's `accounts refresh`.
    KimiPanel,
    /// Codex's `/wham/usage` or `/api/codex/usage`.
    CodexEndpoint,
    /// `agy -p /usage`, or `agy -p /model` in the same use of the same agy home.
    AgyUsage,
    /// `muse serve`'s usage report.
    MuseUsage,
}

impl Read {
    /// The journal's CLI word for this read.
    #[must_use]
    pub const fn cli(self) -> &'static str {
        match self {
            Self::ClaudePanel => "claude",
            Self::KimiPanel => "kimi",
            Self::CodexEndpoint => "codex",
            Self::AgyUsage => "agy",
            Self::MuseUsage => "muse",
        }
    }
}

/// Why the account read is in use: there is no other reason to read one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InUse {
    /// The row a launch is about to start on.
    Launching,
    /// A session running on the account ended a turn, or first drew its footer.
    LiveSession,
    /// A running session on the account stopped at a wall on its screen.
    AtWall,
    /// A person ran a command that names the read.
    Asked,
}

impl InUse {
    /// The journal's short reason word for this read.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Launching => "launching",
            Self::LiveSession => "live-session",
            Self::AtWall => "at-wall",
            Self::Asked => "asked",
        }
    }
}

/// One provider read, as the journal's `probe` event takes it. It carries no address, URL, path
/// or provider output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeEvent {
    /// When it was made, Unix seconds.
    pub at: i64,
    /// Which read.
    pub read: Read,
    /// The roster label read; the unpooled agy and Muse have none.
    pub account: Option<String>,
    /// An unpooled read's login, as its account key by realm (`journal::realm_key`), where the
    /// read saw one: never the address itself.
    pub login: Option<super::journal::Key>,
    /// Why the account is in use.
    pub why: InUse,
}

type Sink = Box<dyn Fn(&ProbeEvent) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Fills the hook, once per process: the journal's publisher, at the composing executable's start.
/// Returns whether this call filled it. No test calls it, so no test publishes.
pub fn install(sink: Sink) -> bool {
    SINK.set(sink).is_ok()
}

/// Reports one read. Callers report before reading, so a read that hangs or fails is still on
/// record.
pub fn note(event: &ProbeEvent) {
    #[cfg(any(test, debug_assertions))]
    CAPTURE.with(|held| {
        if let Some(seen) = held.borrow_mut().as_mut() {
            seen.push(event.clone());
        }
    });
    if let Some(sink) = SINK.get() {
        sink(event);
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Vec<ProbeEvent>>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: runs `body` and returns what it reported on this thread.
#[cfg(any(test, debug_assertions))]
pub fn captured<R>(body: impl FnOnce() -> R) -> (R, Vec<ProbeEvent>) {
    CAPTURE.with(|held| *held.borrow_mut() = Some(Vec::new()));
    let out = body();
    let seen = CAPTURE.with(|held| held.borrow_mut().take().unwrap_or_default());
    (out, seen)
}

#[cfg(test)]
#[path = "provider_read_tests.rs"]
mod tests;
