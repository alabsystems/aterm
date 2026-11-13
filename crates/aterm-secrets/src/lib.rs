// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets` ([`text`], [`age`], [`x25519`], [`envelope`], [`transport`], [`server`]), the
//! join phrase and its CPace ([`phrase`], [`pake`]), and the membership log ([`membership_log`])
//! with the line old builds cannot follow it past ([`crossing`]).
//! Client and service flows ([`materialize`], [`client`], [`service`], [`cli`]).

#![forbid(unsafe_code)]
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

pub mod age;
pub mod alab_join;
pub mod api_key;
pub mod b64;
pub mod bech32;
pub mod commons;
pub mod crossing;
pub mod entry_class;
pub mod envelope;
pub mod join;
pub mod mailbox_component;
pub mod membership_log;
pub mod pake;
pub mod phrase;
pub mod swarm;
pub mod swarm_roster;
pub mod text;
pub mod token_line;
pub mod transport;
pub mod x25519;

#[cfg(unix)]
pub mod claude_token;
pub mod cli;
#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod converge;
#[cfg(unix)]
pub mod discover;
#[cfg(unix)]
pub mod fsx;
#[cfg(unix)]
pub mod git_setup;
#[cfg(unix)]
pub mod heard;
#[cfg(unix)]
pub mod home_lan;
pub mod keychain_tools;
#[cfg(unix)]
pub mod materialize;
#[cfg(unix)]
pub mod members;
#[cfg(unix)]
pub mod need;
#[cfg(unix)]
pub mod network;
#[cfg(unix)]
pub mod operational;
#[cfg(unix)]
pub mod program_watch;
#[cfg(unix)]
mod records_storage;
#[cfg(unix)]
pub mod registry;
#[cfg(unix)]
pub mod release_keychain;
#[cfg(unix)]
pub(crate) mod release_ring;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod service;
#[cfg(all(unix, any(test, debug_assertions)))]
pub mod service_acceptance;
#[cfg(unix)]
pub mod service_health;
#[cfg(unix)]
pub mod service_repair;
pub mod sync_diagnostic;
#[cfg(unix)]
pub mod wake;

mod ed25519;
mod secret;
pub use secret::SecretBytes;

/// The ALab vault: what `aterm init` joins when it answers on this network.
pub const ALAB_DEFAULT_URL: &str = aterm_types::macos_setup::DEFAULT_VAULT_URL;

/// Every compiled-in ALab vault URL a client probes and joins across ([`ALAB_DEFAULT_URL`] first).
pub const ALAB_DEFAULT_URLS: &[&str] = aterm_types::macos_setup::DEFAULT_VAULT_URLS;

/// Whether `url` is one of [`ALAB_DEFAULT_URLS`].
#[must_use]
pub fn is_alab_default_url(url: &str) -> bool {
    ALAB_DEFAULT_URLS.contains(&url)
}

/// The ALab vault's name in a store ([`ALAB_DEFAULT_URL`]'s last segment).
pub(crate) const ALAB_VAULT: &str = "alab";

/// The ALab vault's signing key ([`ALAB_DEFAULT_URL`], [`ALAB_RETIRED`]).
pub const ALAB_SIGNER: &str = "gnUs9ywlj9oLJNA7jDsP6xGO34YTr7xR8ra22GXUoQ4=";

/// The keys the ALab vault rotated away from ([`ALAB_SIGNER`]).
pub const ALAB_RETIRED: &[&str] = &[];

/// ALab's genesis, by its digest in hex: what `aterm secrets genesis` printed when m22 signed
/// Genesis v2 with the released 0.146.0 on 2026-10-29, listing the vault's 40 machines
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F12, F12b). A build checks ALab's genesis by this
/// digest alone, never by a signer ([`membership_log::Log::alab`]): any other genesis for ALab is
/// refused, one signed with the vault's own key included.
pub const ALAB_GENESIS: Option<&str> =
    Some("a8ed22e8eb1ec410d8efc1e736d93dfe100191514584dc7fad8260b429d3a8ac");

/// Reserved `[everyone]` vault entry carrying swarm labels, aliases, hints, and deny-lists.
pub const SWARM_ROSTER_ENTRY: &str = swarm_roster::ROSTER_ENTRY;

/// Reserved admin vault entry carrying steward policy, network id and the server list.
pub const SWARM_POLICY_ENTRY: &str = swarm_roster::POLICY_ENTRY;

/// Reserved admin vault entry carrying the active steward lease (`holder`, `epoch`, `until`).
pub const SWARM_LEASE_ENTRY: &str = swarm_roster::LEASE_ENTRY;

/// Whether an entry is consumed in place and must never become a secret file, in any spelling
/// ([`entry_class::reserved`]): a control record or a Claude Code token.
#[must_use]
pub fn reserved_name(name: &str) -> bool {
    entry_class::reserved(name)
}

/// Why a vault identity is a lab identity: one that founds its own lab and never joins the live
/// ALab (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.2 M2, A12). The mark is written into the
/// identity file when the identity is made (`client::machine_identity`) and read from it ever
/// after, so an identity a release build made stays unmarked whatever build uses it later, and a
/// lab identity stays one whatever build reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabMark {
    /// A test binary made it (`aterm_uds::test_binary::is_test_binary`).
    TestBinary,
    /// A build with debug assertions on made it: a lab instance, a dev build, an example.
    DebugBuild,
    /// A process a lab lane marked made it (`ATERM_LAB_MEMBER_SERVICE`, which `lab mac nux
    /// --scenario vault-phrase` sets on its joiner).
    LabLane,
    /// A build running inside a virtual machine made it (macOS's `kern.hv_vmm_present` reads
    /// 1): the other `lab mac nux` guests run a release app, so only this catches them.
    VirtualMachine,
    /// A build after this one marked it for a reason this one does not name: marked all the same.
    Unnamed,
}

impl LabMark {
    /// The mark's word in the identity file.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::TestBinary => "test-binary",
            Self::DebugBuild => "debug-build",
            Self::LabLane => "lab-lane",
            Self::VirtualMachine => "virtual-machine",
            Self::Unnamed => "unnamed",
        }
    }

    /// The mark a word names; a word this build does not know is [`LabMark::Unnamed`].
    #[must_use]
    pub fn of_word(word: &str) -> Self {
        [
            Self::TestBinary,
            Self::DebugBuild,
            Self::LabLane,
            Self::VirtualMachine,
        ]
        .into_iter()
        .find(|mark| mark.word() == word)
        .unwrap_or(Self::Unnamed)
    }
}

/// What a process making a vault identity knows of itself ([`lab_identity`]): the live reading is
/// `Making::now` (Unix), whose virtual-machine probe is inert in every test binary; a test states
/// each fact instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Making {
    /// It is a test binary.
    pub test_binary: bool,
    /// It was built with debug assertions on.
    pub debug_build: bool,
    /// A lab lane marked it (`ATERM_LAB_MEMBER_SERVICE` or `ATERM_LAB_VAULT_SERVICE` is set).
    pub lab_lane: bool,
    /// It runs inside a virtual machine.
    pub virtual_machine: bool,
}

/// The lab mark an identity made by a process that is `making` carries, or `None` for a release
/// build on a Mac's own hardware, the one kind of process whose new identity may join the live
/// ALab. The first fact that holds names the mark. A process this says `None` for that finds a
/// lab-marked identity in this Mac's vault folder sets that folder aside and starts as a new Mac
/// (A16, `client::Paths::for_home`).
#[must_use]
pub const fn lab_identity(making: Making) -> Option<LabMark> {
    if making.test_binary {
        Some(LabMark::TestBinary)
    } else if making.debug_build {
        Some(LabMark::DebugBuild)
    } else if making.lab_lane {
        Some(LabMark::LabLane)
    } else if making.virtual_machine {
        Some(LabMark::VirtualMachine)
    } else {
        None
    }
}

/// The port `aterm secrets serve` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 7484;

/// Default mDNS name registered by `aterm secrets serve` on macOS ([`ALAB_DEFAULT_URL`]).
pub fn alab_advertised() -> &'static str {
    ALAB_DEFAULT_URL
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once(".local"))
        .map_or("", |(label, _)| label)
}

/// An error that names what failed, never a secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String, ErrorKind);

/// What an [`Error`] tells a caller beyond its text, where the caller acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorKind {
    /// Anything else.
    #[default]
    Other,
    /// The local Commons control journal is held by another writer. No current
    /// control observation was obtained; retry after that writer releases it.
    JournalBusy,
    /// Another local Commons custody observation held its lock past a bounded
    /// wait. Nothing was observed; retry after it finishes.
    CustodyBusy,
    /// A local vault writer held the state lock through the reader's wait for it (none, for a
    /// reader that does not wait), so nothing was read. Retry from a new observation after the
    /// writer releases its lock.
    VaultBusy,
    /// A valid local vault observation differs from the source or applied
    /// version captured earlier. The old observation grants no access; a
    /// retry must obtain and authorize the current source again.
    VaultChanged,
    /// A membership record or batch cannot fit the existing count or byte bounds.
    /// No local operation may treat a partial draft as the complete operation.
    MembershipCapacity,
    /// A locally retained causal frontier is not yet covered by complete raw records.
    /// Credential authority is unavailable; record intake may repair the missing history.
    MembershipHistoryUnavailable,
    /// A request that ran past its deadline: a long-poll the server still held, a lookup with
    /// no answer, a slow network or a host that sleeps, which nothing here tells apart.
    TimedOut,
    /// Some authorized join queues could not be observed. A missing identity is
    /// unknown, and opening a vault cannot hold every already-waiting request.
    JoinQueueIncomplete,
    /// A publish the vault refused before storing it: a version not newer than the one it holds
    /// (another admin's publish came first), or another publish holding its store or path (a
    /// server's 409). It cannot have landed.
    NotStored,
    /// A publish the vault would not take at all, decided before it stored anything: a server
    /// that refused what it carries or the Mac that sent it (its 403) or holds its limit of
    /// vaults (507), or a first write to a path that does not hold the bytes afterwards (a
    /// missing or unwritable folder, another publisher's lock or file). It cannot have landed,
    /// and it is not a race with a publish of the same bytes: asked again, it gets the same
    /// answer until someone changes the server or the path.
    Refused,
    /// A member's desk answered that a vault pass on it holds its state: its join run touched no
    /// phrase, so the same words go to it again once the pass ends.
    MemberBusy,
    /// This Mac's vault identity is a lab identity ([`LabMark`]), or the one this process would
    /// make is, and the join would ask the live ALab: refused before anything was dialed. A lab
    /// identity founds its own lab, or joins one by its URL; asked again, it is refused again.
    LabIdentity,
    /// A join of the vault that dropped this Mac ([`client::dropped`]: a rotation left it out
    /// with no `Remove`), refused under the vault's state lock before its key went anywhere:
    /// every member that read the rotation refuses that key, and the ask would have moved this
    /// Mac to pending, losing the reading that says how it comes back (one-mesh E2, f299). The
    /// message is [`client::dropped_words`]. Asked again while this Mac still reads dropped, it
    /// is refused again; with its vault folder set aside it joins as a new Mac.
    Dropped,
}

impl Error {
    /// An error with this message.
    #[inline]
    pub fn new(message: impl Into<String>) -> Self {
        Error(message.into(), ErrorKind::Other)
    }

    /// An error of `kind` with this message.
    pub fn of_kind(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error(message.into(), kind)
    }

    /// What kind of failure this is.
    pub fn kind(&self) -> ErrorKind {
        self.1
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::new(error.to_string())
    }
}

/// This crate's result.
pub type Result<T> = std::result::Result<T, Error>;

/// Write one line to stderr, the log a launchd or systemd job keeps. A log that cannot take it
/// (a full disk, a closed descriptor) loses the line rather than the job: `eprintln!` panics
/// when its write fails, which would end a server or subscriber over a log line.
#[cfg(unix)]
pub(crate) fn log_line(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
