// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vault reads for a reader that asks per connection, per round or every few seconds, whatever
//! it found: the swarm deny list (a fleet listener, which checks each proved dialer against it,
//! its node job's watch, which checks every live connection each tick, and the node's Commons
//! peer checks and census), the Commons observation (the Commons runtime's presence poll,
//! the members desk), this Mac's standing as an ALab member (the node's LAN responder and
//! advert), the vault's configuration (the vault the node's fleet state is of, at each tick and
//! handshake) and the stamps of what the members desk reads (a window's desk lane and the node's
//! automatic admission). [`swarm_denied_nodes`](super::swarm_denied_nodes) and
//! [`observe_commons`](super::observe_commons) read, decrypt and evaluate the vault and its
//! membership log at every call, tens of milliseconds of CPU on a busy Mac, so each keeps its
//! last settled answer and reads again when one of the files it reads, or the vault directory's
//! identity or access metadata, changes.

use super::{Paths, StateLock, StateObservation};
use crate::fsx;
#[cfg(test)]
use crate::swarm_roster::SwarmRoster;
use std::{
    cell::{Cell, Ref, RefCell},
    collections::VecDeque,
    os::unix::fs::MetadataExt as _,
    path::PathBuf,
    sync::Mutex,
};

/// How many answers a memo keeps: a process reads one vault folder (a keyed read, a few logs of
/// it). This crate's own test process (`cfg(test)`), whose tests each read their own folders at
/// once, keeps as many for each folder, so one test's reads never let go of another's answers
/// ([`Memo::keep`]).
const MAX_FOLDERS: usize = 16;

/// [`swarm_denied_nodes`](super::swarm_denied_nodes), read again only when one of the files it
/// reads ([`inputs`]) was added, removed or changed, or the directory's identity or access
/// metadata changed since its last settled answer, so a removal the vault or the log takes in
/// is refused at the next call, with no window.
///
/// An answer is settled when it was read while no writer held the vault's state lock: the memo
/// holds a shared observation of it across the read. That keeps `Ok(None)` for what the inputs
/// themselves say (this Mac removed in the log, a held envelope that does not verify), which
/// they say again until they change. An `Ok(None)` read while a writer held the lock (mid-apply)
/// is returned as it is and read again at the next call, and so is every `Err`: an error does not
/// say whether the inputs caused it or the moment did (no descriptor left while the listener's
/// sockets hold them all, an interrupted read), and keeping a passing one taken just after a
/// revocation would leave a caller on its list from before it until the next vault write.
///
/// One read at a time: callers that miss together after a change wait for the first one's read
/// and take its answer, and a caller whose files are unchanged never waits on a read.
///
/// # Errors
/// As [`swarm_denied_nodes`](super::swarm_denied_nodes).
pub fn swarm_denied_nodes_now(paths: &Paths) -> crate::Result<Option<Vec<String>>> {
    Ok(swarm_denial_snapshot_now(paths)?.into_nodes())
}

/// The same memoized observation as [`swarm_denied_nodes_now`], retaining its typed
/// unavailable reason. Both APIs share one memo and the same input stamps, lock observations
/// and refresh serialization. This is a retained observation, not an additional live probe:
/// unchanged inputs reuse the settled snapshot, including a settled unavailable reason.
/// Reads made under a writer and read errors are retried on the next call as before.
///
/// # Errors
/// As [`super::swarm_denial_snapshot`].
pub fn swarm_denial_snapshot_now(paths: &Paths) -> crate::Result<super::SwarmDenialSnapshot> {
    DENIAL.read(paths, super::swarm_denial_snapshot)
}

/// What [`swarm_denial_snapshot_now`] and [`VaultLook::swarm_denied_nodes`] keep.
static DENIAL: Memo<super::SwarmDenialSnapshot> = Memo::of(Set::Vault, false);

/// [`observe_commons`](super::observe_commons), read again only when one of the files it reads
/// ([`observation_inputs`]) was added, removed or changed, or the directory's identity or access
/// metadata changed, since its last settled answer, as [`swarm_denied_nodes_now`] keeps the deny
/// list. Every verdict is kept, an unavailable one too: one the inputs say (control not visible
/// to this Mac, left out, revoked) is what they say again until they change, and a revocation
/// rewrites the state, the envelope or the records, so it is read at the next call, with no
/// window. A vault a writer held (`Busy`, mid-apply) and an error are read again at the next call.
///
/// # Errors
/// As [`observe_commons`](super::observe_commons).
pub(crate) fn observe_commons_now(paths: &Paths) -> crate::Result<crate::commons::Observation> {
    static MEMO: Memo<crate::commons::Observation> = Memo::of(Set::Observation, false);
    MEMO.read(paths, super::observe_commons)
}

/// [`observe_commons`](super::observe_commons) for a check of Commons custody, read again only
/// when one of the files it reads ([`observation_inputs`]) moved; quiet, as
/// [`local_swarm_roster_for_log_now`], so a vault a writer holds is the fresh read's `Busy`, as
/// before, never the observation kept from before that writer. A Commons repair round checks its
/// custody against the vault at every step it opens a table session
/// (`Bootstrap::revalidate_custody`), 24 times a round on each side of a loopback pair, and each
/// check read and verified the vault, its log and this Mac's denial again: 14% of a quiet round's
/// CPU (2026-11-08), and nearly half its reads of the vault, which on a fleet node holds a
/// megabyte or more. A table-repair replica judges by it the bootstrap it keeps from round to
/// round, where each round's open read the vault afresh: through its round's opening batch's look
/// (`Bootstrap::stands_in`, [`VaultLook::observe_commons`]), or, where that try does not take it
/// (no try, its admission refused, or the bootstrap fallen through its look), on its own calls
/// after the try (`Bootstrap::stands`, in `Replica::bootstrap`).
///
/// # Errors
/// As [`observe_commons`](super::observe_commons).
pub(crate) fn observe_commons_quiet(paths: &Paths) -> crate::Result<crate::commons::Observation> {
    OBSERVATION.read_keyed(paths, &(), super::observe_commons)
}

/// What [`observe_commons_quiet`] and [`VaultLook::observe_commons`] keep.
static OBSERVATION: Memo<crate::commons::Observation> = Memo::of(Set::Observation, true);

/// [`local_recipient_denied`](super::steward::local_recipient_denied) of a fresh read, read again
/// only when one of the files it reads ([`observation_inputs`]) moved; quiet, as
/// [`local_swarm_roster_for_log_now`], so a writer holding the vault (the caller's own exclusive
/// take included) meets a fresh read. Every table session a repair round opens checks this Mac's
/// own denial under its access (`Bootstrap::access_in_observed_with`), 18 times a round on each
/// side of a loopback pair, reading the vault, its log and roster each time: 7% of a quiet round's
/// CPU (2026-11-08) and a third of its reads of the vault. That access asks it under its caller's
/// shared observation of the state lock, `under`'s (the access's own, or that of the look a
/// session's admission takes: [`VaultLook`]), which the memo takes for its own as
/// [`trusted_commons_log_under`] does, and looks up what it kept through that caller's look at
/// each file, as that log's memo does next (`under`'s [`fsx::Glance`]); a miss reads and stamps
/// afresh, and keeps its answer under those stamps. The swarm's checks of this Mac
/// ([`local_recipient_denied_in_roster`](super::local_recipient_denied_in_roster)), but for its
/// LAN presence ([`swarm_member_heard_now`]), read afresh, so one that cannot read a file it was
/// settled on takes this Mac as denied. This one, like every
/// memo here, gives the answer it kept through a read fault on files that have not moved: the
/// judgement those files gave, so a Mac keeps the table access it held, as a server keeps the
/// role it holds while its device cannot be read
/// (`a_device_it_cannot_read_gives_a_mac_still_in_no_new_authority`).
///
/// # Errors
/// As [`local_recipient_denied`](super::steward::local_recipient_denied).
pub(crate) fn local_recipient_denied_under(paths: &Paths, under: Under<'_>) -> crate::Result<bool> {
    static MEMO: Memo<bool> = Memo::of(Set::Observation, true);
    MEMO.read_under(paths, &(), under, |paths| {
        super::steward::local_recipient_denied(&super::Reads::new(paths))
    })
}

/// [`swarm_member_heard`](super::steward::swarm_member_heard): its judgement (this Mac in the ALab
/// vault, and no evidence denying it) read again only when one of the files it reads
/// ([`observation_inputs`]) moved, `state.toml` by what it holds but `heard` and the rounds'
/// receipts ([`stamp_memo_inputs_in`]), and `heard` taken from `state.toml` at each call
/// ([`state_look`]: one `lstat` while the file stands). The node's LAN responder asks it at each
/// of its refreshes (one a second), at each route change and before each answer it sends, and its
/// `_alab._tcp` advert at each tick of the node's watch: each ask read the vault's status, its
/// envelope again for the signer's chain and its denial evidence again, about a millisecond of
/// CPU on a vault the owner's size (measured on Linux arm64), of files the vault's subscriber
/// then rewrote about twice a minute; and kept with `heard` under `state.toml`'s metadata, the
/// judgement was read again at each of those rewrites, which moved `heard` alone: the held vault
/// twice, 2.38 MB, every 25 s on a Linux arm64 node (2026-11-09). The subscriber now moves
/// `heard` about once a minute and leaves a held vault that holds its bytes as it is.
///
/// Quiet, as [`local_swarm_roster_for_log_now`]: while a writer holds the vault's state lock the
/// fresh read meets it and this Mac is out, as before. What it keeps is a judgement of files that
/// have not moved and when the vault last heard this Mac, never a verdict at a time: each caller
/// judges that age against its own clock at each call. A removal, a revocation or a denial
/// rewrites the state, the envelope or the records, so it is judged again at the next call; a
/// round that rewrote `state.toml` between the judgement's stamps and this call's `heard` gives
/// the `heard` it wrote, only where that state still holds this Mac in. A read that fails is never
/// kept: it is this call's error, and the next call reads again; a `state.toml` that cannot be
/// read for `heard` is read afresh, judgement and all.
///
/// The trade: once an answer is kept, a read fault on files whose stamps have not moved (a
/// descriptor the process ran out of, an I/O error) is never met, and the kept answer, a member's
/// included, stands, where a fresh read would have failed and taken this Mac as out. It stands
/// only until one of those files changes. The LAN presence is a hint whose dial a handshake
/// judges; the swarm's other checks of this Mac
/// ([`local_recipient_denied_in_roster`](super::local_recipient_denied_in_roster)) read afresh and
/// take a fault as a denial.
///
/// Keyed by whether a test reads this vault's standing as the ALab vault's
/// (`client::test_alab_standing`, which a release build lacks), so a registration after a first
/// read is read afresh.
///
/// # Errors
/// As [`swarm_member_heard`](super::steward::swarm_member_heard).
pub fn swarm_member_heard_now(paths: &Paths) -> crate::Result<Option<u64>> {
    static MEMO: Memo<bool, bool> = Memo::of(Set::Observation, true);
    let member = MEMO.read_keyed(
        paths,
        &super::steward::alab_standing_for_tests(paths),
        |paths| super::steward::swarm_member_heard(paths).map(|heard| heard.is_some()),
    )?;
    if !member {
        return Ok(None);
    }
    match state_look(paths, None) {
        Some(look) => Ok(look.heard),
        None => super::steward::swarm_member_heard(paths),
    }
}

/// [`config`](super::config), read again only when `source.toml` or the held envelope it takes
/// the signer's chain from ([`config_inputs`]) was added, removed or changed, or the vault
/// directory's identity or access metadata changed. The node asks it for the vault this Mac's
/// fleet state is of (aterm-agent's `fleet_vault::tenure`) at each watch tick and as fleet
/// connections under drive grants are offered and served, and for the vault configured
/// (`fleet_vault::Vault::configured`) at each heartbeat, and each ask read the whole envelope
/// again where `source.toml` names no `followed`: 1.25 MB every 2 s on the watch's thread alone
/// on a Linux arm64 member whose envelope is 1.18 MB, and 0.68 ms of CPU an ask on a fixture's
/// 1.2 MB one, where an answer kept takes 5 µs and reads nothing (2026-11-09).
///
/// Quiet, as [`local_swarm_roster_for_log_now`]: while a writer holds the vault's state lock (a
/// join writing `source.toml`) the read is fresh, as before. Each answer is what `config` gives
/// for the same files: a read whose envelope cannot be read (which `config` takes as no chain)
/// is never kept, and is answered by `config` itself, so a fault is that call's alone. It keeps
/// no answer that depends on the time.
///
/// # Errors
/// As [`config`](super::config).
pub fn config_now(paths: &Paths) -> crate::Result<Option<super::Config>> {
    static MEMO: Memo<Option<super::Config>> = Memo::unrecorded(config_inputs, true);
    MEMO.read_keyed(paths, &(), super::config_whole)
        .or_else(|_| super::config(paths))
}

/// The files [`config`](super::config) reads: `source.toml`, and the held envelope (`vault`) whose
/// handoff chain is the keys its signer was reached from where it names no `followed`.
fn config_inputs(paths: &Paths) -> Option<Vec<PathBuf>> {
    Some(vec![paths.source_file(), paths.cache_file()])
}

/// [`local_swarm_roster_for_log`](super::local_swarm_roster_for_log), read again only when one
/// of the files it reads ([`inputs`]) moved, or `log` is another log than the one it was read for
/// (by its heads, which name every record it holds, and its budgets). Every Commons peer check
/// asks it twice (for the acknowledged log and for that log with the peer's evidence), four or
/// five checks a session: 144 reads in 8 quiet rounds of a loopback pair, 57% of their checks'
/// time (2026-11-06). Quiet: what it kept is given only while no writer holds the vault's state
/// lock, so a writer mid-apply meets the read, which refuses, as before.
///
/// # Errors
/// As [`local_swarm_roster_for_log`](super::local_swarm_roster_for_log).
pub fn local_swarm_roster_for_log_now(
    paths: &Paths,
    log: &crate::membership_log::Log,
) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
    LOG_ROSTERS.read_keyed(paths, &log_key(log), |paths| {
        super::local_swarm_roster_for_log(paths, log)
    })
}

/// What [`local_swarm_roster_for_log_now`] and [`VaultLook::local_swarm_roster_for_log`] keep, each
/// for the log it was read for ([`log_key`]).
static LOG_ROSTERS: Memo<Option<crate::swarm_roster::SwarmRoster>, LogKey> =
    Memo::of(Set::Vault, true);

/// A log as [`LOG_ROSTERS`] tells it from another: by its heads, which name every record it
/// holds, and its budgets.
type LogKey = ([u8; 32], crate::membership_log::RecordBudgets);

fn log_key(log: &crate::membership_log::Log) -> LogKey {
    (super::log_entries::records_heads_digest(log), log.budgets())
}

/// [`acknowledged_commons_log`](super::acknowledged_commons_log) for `trust`, read again only when
/// one of the files it reads ([`inputs`]) moved; quiet, as [`local_swarm_roster_for_log_now`].
///
/// # Errors
/// As [`acknowledged_commons_log`](super::acknowledged_commons_log).
pub fn acknowledged_commons_log_now(
    paths: &Paths,
    trust: crate::commons::journal::Trust,
) -> crate::Result<super::CommonsLog> {
    #[cfg(any(test, debug_assertions))]
    LOGS_READ.with(|reads| reads.set(reads.get() + 1));
    ACKNOWLEDGED.read_keyed(paths, &trust, |paths| {
        super::acknowledged_commons_log(paths, trust)
    })
}

/// What [`acknowledged_commons_log_now`], [`trusted_commons_log_under`] and
/// [`VaultLook::acknowledged_commons_log`] keep.
static ACKNOWLEDGED: Memo<super::CommonsLog, crate::commons::journal::Trust> =
    Memo::of(Set::Vault, true);

/// [`trusted_commons_log`](super::trusted_commons_log) through
/// [`acknowledged_commons_log_now`]. A Commons repair round checks its roster access, the log
/// behind it and the session's members at nearly every step: hundreds of whole reads of the vault
/// and its log a round, half a second of CPU on a node with fifty peers (2026-11-08), of files
/// that had not moved.
///
/// # Errors
/// As [`trusted_commons_log`](super::trusted_commons_log).
pub fn trusted_commons_log_now(
    paths: &Paths,
    trust: crate::commons::journal::Trust,
) -> crate::Result<Option<(crate::membership_log::Log, [u8; 32])>> {
    acknowledged_commons_log_now(paths, trust).map(super::CommonsLog::trusted)
}

/// [`trusted_commons_log_now`] for a caller that holds `under`'s lock: a roster access check
/// (`commons::roster::log_observation_is_current`) and a table session's access
/// (`Bootstrap::access_in_observed_with`), each under its caller's shared observation of the
/// vault's state lock (the check's own, or a table session look's: [`VaultLook`]), ask it at
/// nearly every step of a repair round, and each ask opened, observed and closed that
/// lock again: with this Mac's own denial check under the same access
/// ([`local_recipient_denied_under`]), 7.8% of a sustained quiet round's samples over a long log
/// and 10.4% over a short one (2026-11-08). While that lock is a shared observation that guards
/// the lock at the vault's path as `under`'s glance saw that path ([`StateLock::observes_in`]), it
/// is the observation the memo would take, so what it kept is given without another. A writer's
/// own take, or a lock that guards another file, is [`trusted_commons_log_now`]'s call. It looks
/// up what it kept through the caller's check's look, `under`'s ([`fsx::Glance`]); a miss reads as
/// [`trusted_commons_log_now`] does, stamping afresh.
///
/// # Errors
/// As [`trusted_commons_log`](super::trusted_commons_log).
pub(crate) fn trusted_commons_log_under(
    paths: &Paths,
    trust: crate::commons::journal::Trust,
    under: Under<'_>,
) -> crate::Result<Option<(crate::membership_log::Log, [u8; 32])>> {
    ACKNOWLEDGED
        .read_under(paths, &trust, under, |paths| {
            super::acknowledged_commons_log(paths, trust)
        })
        .map(super::CommonsLog::trusted)
}

/// [`swarm_roster_until`](super::swarm_roster_until), read again only when one of the files it
/// reads ([`inputs`]) moved; quiet, as [`local_swarm_roster_for_log_now`], so a writer holding
/// the vault is waited for until `deadline` as before, and what it kept is never a permissive
/// answer under that writer.
///
/// # Errors
/// As [`swarm_roster_until`](super::swarm_roster_until).
pub fn swarm_roster_now_until(
    paths: &Paths,
    deadline: std::time::Instant,
) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
    ROSTER.read_keyed(paths, &(), |paths| {
        super::swarm_roster_until(paths, deadline)
    })
}

/// What [`swarm_roster_now_until`] and [`VaultLook::swarm_roster_until`] keep.
static ROSTER: Memo<Option<crate::swarm_roster::SwarmRoster>> = Memo::of(Set::Vault, true);

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// How many looks this thread opened on an observation of their own ([`VaultLook::new`]).
    static LOOKS_OBSERVED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many times this thread asked for the acknowledged log on its own call
    /// ([`acknowledged_commons_log_now`]).
    static LOGS_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread asked for the acknowledged Commons log on its own call
/// ([`acknowledged_commons_log_now`], [`trusted_commons_log_now`]), whether its memo answered or
/// read it; an ask under a caller's observation or through a look
/// ([`trusted_commons_log_under`], [`VaultLook::acknowledged_commons_log`]) adds none.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_logs_read() -> usize {
    LOGS_READ.with(std::cell::Cell::get)
}

/// How many looks this thread opened on an observation of their own ([`VaultLook::new`]); one
/// standing on its caller's ([`super::CommonsSourceRead::into_look`]) adds none.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_looks_observed() -> usize {
    LOOKS_OBSERVED.with(std::cell::Cell::get)
}

/// What this thread's looks derived of the moments they stand for ([`Moment`]), since this
/// thread began: each value computed, and each computed while its look already kept a value of
/// that field. A value served from what the look kept moves neither.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LookDerivations {
    /// Every value a look computed, kept or not.
    pub derived: Derivations,
    /// Every value a look computed again while it kept a value of that field (for a keyed one,
    /// under any key; for the stamps, under any record bounds): a look that gives what it kept,
    /// asked for one key of each field, makes none.
    pub rederived_after_kept: Derivations,
}

/// One count for each value a look derives ([`LookDerivations`]).
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Derivations {
    /// Judgements of whether the look's observation guards the lock at the vault's path, through
    /// its glance ([`StateLock::observes_in`], [`VaultLook::held`]).
    pub lock_judgments: usize,
    /// Stamps of the files the deny list's, the roster's, the logs' and the records' memos read
    /// (`Set::Vault`), for an ask through a look that holds the vault.
    pub vault_stamps: usize,
    /// Stamps of the files the Commons observation's and this Mac's own denial's memos read
    /// (`Set::Observation`), for an ask through a look that holds the vault.
    pub observation_stamps: usize,
    /// Looks at `state.toml` as a memo stamps it, for an ask through a look that holds the vault
    /// ([`Under::state_look`]).
    pub state_looks: usize,
    /// Digests of the state a Commons log observation stands on, one for each log's heads
    /// ([`Under::log_state`]).
    pub log_states: usize,
    /// Judgements of a log-backed access's currency, one for each access and record bounds
    /// ([`Under::currency`]).
    pub currency: usize,
}

#[cfg(any(test, debug_assertions))]
impl Derivations {
    const NONE: Self = Self {
        lock_judgments: 0,
        vault_stamps: 0,
        observation_stamps: 0,
        state_looks: 0,
        log_states: 0,
        currency: 0,
    };

    /// The count of `value`.
    fn of(&mut self, value: Derived) -> &mut usize {
        match value {
            Derived::LockJudgment => &mut self.lock_judgments,
            Derived::Stamps(Set::Vault) => &mut self.vault_stamps,
            Derived::Stamps(Set::Observation) => &mut self.observation_stamps,
            Derived::StateLook => &mut self.state_looks,
            Derived::LogState => &mut self.log_states,
            Derived::Currency => &mut self.currency,
        }
    }

    fn since(self, before: Self) -> Self {
        Self {
            lock_judgments: self.lock_judgments - before.lock_judgments,
            vault_stamps: self.vault_stamps - before.vault_stamps,
            observation_stamps: self.observation_stamps - before.observation_stamps,
            state_looks: self.state_looks - before.state_looks,
            log_states: self.log_states - before.log_states,
            currency: self.currency - before.currency,
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl LookDerivations {
    /// What was derived after `before`, a reading of this same thread's counts.
    #[must_use]
    pub fn since(self, before: Self) -> Self {
        Self {
            derived: self.derived.since(before.derived),
            rederived_after_kept: self.rederived_after_kept.since(before.rederived_after_kept),
        }
    }
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// What this thread's looks derived ([`test_look_derivations`]).
    static LOOK_DERIVATIONS: std::cell::Cell<LookDerivations> = const {
        std::cell::Cell::new(LookDerivations {
            derived: Derivations::NONE,
            rederived_after_kept: Derivations::NONE,
        })
    };
}

/// What this thread's looks derived of the moments they stand for ([`LookDerivations`]).
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_look_derivations() -> LookDerivations {
    LOOK_DERIVATIONS.with(std::cell::Cell::get)
}

/// Count one computation of `value` through a look, made while the look kept a value of that
/// field where `kept` ([`LookDerivations`]).
#[cfg(any(test, debug_assertions))]
fn count_derivation(value: Derived, kept: bool) {
    LOOK_DERIVATIONS.with(|counts| {
        let mut now = counts.get();
        *now.derived.of(value) += 1;
        if kept {
            *now.rederived_after_kept.of(value) += 1;
        }
        counts.set(now);
    });
}

/// A value of the moment a look stands for ([`Moment`]), as [`LookDerivations`] counts them.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy)]
enum Derived {
    /// Whether the look's observation guards the lock at the vault's path ([`VaultLook::held`]).
    LockJudgment,
    /// The stamps of a set of files ([`Under::stamps`]).
    Stamps(Set),
    /// `state.toml` as a memo stamps it ([`Under::state_look`]).
    StateLook,
    /// The digest of the state a Commons log observation stands on ([`Under::log_state`]).
    LogState,
    /// Whether a log-backed access is current ([`Under::currency`]).
    Currency,
}

/// One look at this Mac's vault, for reads that stand for one moment of it: a shared observation
/// of its state lock, held while the look lives ([`super::Observing`]), and one `lstat` of each
/// file the memos it asks stamp ([`fsx::Glance`], in the look's [`Moment`]). Each ask gives what
/// its memo kept for the files as the look first saw them, and one that misses reads and stamps
/// afresh, as the memo's own call does. While the look observes the lock no writer holds it and
/// none can take it; a file that changes all the same (a write that takes no lock, a test's own)
/// keeps the stamps the look first took of it until the look ends, and the next look stamps it
/// afresh. A look taken while a writer holds the lock, or where the lock cannot be observed,
/// observes nothing, and each ask is its memo's own call, which meets that writer as it always
/// did. Whether its observation guards the lock at the vault's path it judges once too, at its
/// first ask, through its glance ([`StateLock::observes_in`]), and once it found that it does it
/// judges it no more while it lives ([`Self::held`]); one that found it does not judges again at
/// the next ask, through the same glance: no writer takes the lock while the look observes it,
/// and a lock file renamed or replaced by something that does not hold the lock at the vault's
/// path is seen at the next look, as any such file is. Drop it before network I/O or any vault
/// mutation: an exclusive take of the lock on this thread under it would wait for good, and in a
/// build with debug assertions panics.
///
/// It stamps each set of files its memos read once too ([`Set`], [`Under::stamps`]), at the first
/// ask of a memo of that set, and gives those stamps to every later ask of one: a file's stamp is
/// the `lstat` its glance took first, so stamping it again through the same glance gives the same
/// stamp, and what else the stamps hold (the records' selection, what `state.toml` holds) comes
/// from caches keyed by what that glance showed. It looks at `state.toml` once the same way
/// ([`Under::state_look`]), and digests the state each Commons log observation stands on once for
/// each log ([`Under::log_state`]): the log's heads, what `state.toml` holds, the digest kept for
/// what the glance showed of `source.toml`, and what it showed of five more files. It judges each
/// log-backed access's currency once for what the access was opened for and holds
/// ([`Under::currency`]), and keeps only whether it is current. It keeps what it took only where
/// taking it looked at nothing afresh ([`fsx::fresh_looks`]); what it took from a fresh read it
/// takes again at the next ask, as before. While the file at `.lock` is the inode the look holds,
/// each writer of the files the state lock guards takes it exclusively, so none of them changes
/// while the look lives, and a cache refilled after it let an entry go reads them unchanged and
/// gives what it gave. One ask can still be given what an earlier one took where taking it again
/// would give otherwise: where a cache entry the first took from was let go or replaced since (by
/// another thread, or past its bound) and taking it again would meet a transient read fault, a file
/// changed during the look without the state lock (the device age identity and claim, the fold
/// mark, the Commons claim, the default pin, the control anchor, a mode or owner change, a process
/// that ignores the lock), or the lock file replaced after the look's first ask, with a writer
/// under the new lock. Such an ask could refuse before, and gives the look's first answer now, as
/// an ask with the cache warm already did; the next look judges afresh. A retained subscriber's
/// look, which lives across a long-running subscriber's checks (`aterm-db`'s
/// `with_retained_session`), lets go of what it derived at the start of each of its revalidations
/// (each `ReadValidation::current`, [`Self::forget_derived`]), so each revalidation derives
/// afresh and takes a value once only within it; the checks its callers make outside a
/// revalidation (the currency of the store contexts `Contributor::observe` and
/// `Contributor::check` judge, which `with_retained_store` makes before its
/// `ReadValidation::current`) are given what the look last derived, as any ask through it may be
/// given what an earlier one took. A read batch's look keeps what it derived for the whole batch.
///
/// A Commons peer check takes one for every stage it reads itself and keeps it from its first
/// stage to its last (`aterm-db`'s `peer_check`): the roster and its deny list, every record this
/// Mac holds, the acknowledged log and that log's roster, and the roster of that log with the
/// peer's evidence, all asked under one observation of the lock and one look at each of their
/// files. A stage after a look that holds nothing ([`Self::holds_vault`]) takes one of its own,
/// and the check lets its look go before an enrollment's own judgement, which reads the vault
/// itself and may open its Commons custody. Its kernel's model reads each view at its own stage,
/// and the kept look keeps that true: what moves the records the Held stage reads is written only
/// under the state lock, which no writer takes while a look that holds the vault lives, so such a
/// writer's removal lands after the stages read through that look and the next check reads it,
/// and one that lands before that look is taken is read through it; what changes without the lock
/// is seen no later than the next check: at the next check where a stage's read stands for the
/// stamps the look first took, as it is at the next look here, and at the stage that reads afresh
/// on a memo miss, as before; each such read is made no later than its stage.
///
/// A table repair round's local batch takes one for its session's open and the checks it makes at
/// once after it (`aterm-db`'s `Replica::admit`): the open's custody
/// (`Bootstrap::revalidate_custody_in`) and access (`Bootstrap::access_observed_in_look`), the
/// currency of its peer and of its observation (`Access::is_current_in`), and its peer's roster
/// ([`Self::swarm_roster_until`]); a round's opening batch on the bootstrap its replica keeps
/// judges through it that bootstrap's vault observation and pinned files' digests, too, as it asks
/// whether the bootstrap still stands (`Bootstrap::stands_in`, `Replica::opening`), and the rest of
/// it (the vault's folders, its custody locks and its creation claim) on calls of its own as the
/// batch runs. A read batch's look is the batch's own source read of the vault
/// ([`super::CommonsSourceRead::into_look`]), which holds the state lock shared from before the
/// admission until the batch returns, and its session keeps it (`Session::keep_look`). A round on
/// a stream shares one look among its read admissions with no call on that stream between them
/// (`aterm-db`'s `GroupLook`): its group's first admission's source read's, which the round keeps
/// after that batch returns, where the batch succeeded and its glance first saw the control
/// anchor settled (`GroupLook::keep`), and lends to each read admission after it
/// (`Session::lend_look`) until the round's next call on its stream, write admission, witness
/// read or error, or its end. Each of those
/// followers reads the control journal itself and asks after that read whether the look's
/// observation still guards the lock at the vault's path ([`Self::guards_source`]) and whether the
/// look still shows the control anchor as it first saw it (`Bootstrap::anchor_stands_in`), and
/// where either does not admits on a look of its own. So every currency, context and peer check
/// the batch makes judges the vault as its admission, or its group's first, did: each writer of
/// the files the state lock guards takes it exclusively, so none of them changes within the batch
/// or the group, and a writer that would take it between a group's admissions waits for the
/// group to end (`GroupLook`'s BC-34a). What changes without it (the device age identity and
/// claim, under the Commons custody locks; the fold mark, under none; any other file the custody
/// pins but the control anchor, which its digests judge through the look as it first saw it) is
/// seen at the next admission outside a group, and within one at the next group's first admission
/// or sooner at a fresh check, a follower's reuse of a digest judged against the group's first
/// `lstat` of the file (`GroupLook`'s BC-34b), as the admission's look already saw them once. As
/// the batch decodes a record and probes the history
/// it needs, the record's history and its key judge through it too: the lease of the log's
/// history, its membership check and its read of the log (`Bootstrap::historicals_in`,
/// `Bootstrap::holds_histories_in`), and the key's check as the batch takes it
/// (`Historical::with_key_in`); a handoff a basis falls back to opens on its own calls, as it did.
/// Each store open's and store read batch's source read of the vault is the batch's own or, for a
/// follower, its group's first admission's, which the look stands on, its
/// custody judged through the look (`Bootstrap::local_source_read_in`): the lock file and the
/// vault's folder are judged as that source read found them. A store read batch's opening
/// fence (`Session::read_context`) and its closing check of the keys it reused
/// (`ReadBatch::revalidate`) read afresh, on their own calls; its closing (`Session::close_read`)
/// judges the custody (`Bootstrap::revalidate_custody_in`) and the session's currency
/// (`Access::is_current_in`) through one look of its own, made after the read, so that look's
/// first look at each file follows the read; the closing's fence (`Session::fenced`) then asks
/// afresh, on its own call, under a generation or an access that is not log-backed. A write
/// batch's look observes the lock beside its source reads and ends before the batch runs, whose
/// checks each observe the vault again.
pub struct VaultLook<'a> {
    paths: &'a Paths,
    observation: Option<super::Observing>,
    moment: Moment<'a>,
}

impl<'a> VaultLook<'a> {
    /// A look at the vault at `paths`, observing its state lock where no writer holds it.
    #[must_use]
    pub fn new(paths: &'a Paths) -> Self {
        #[cfg(any(test, debug_assertions))]
        LOOKS_OBSERVED.with(|looks| looks.set(looks.get() + 1));
        let observation = match StateLock::observe_classified(paths) {
            Ok(StateObservation::Held(lock)) => Some(super::Observing::new(lock, paths)),
            Ok(StateObservation::Absent | StateObservation::Busy) | Err(_) => None,
        };
        Self::of(paths, observation)
    }

    /// A look at the vault at `paths` standing on `lock`, a shared observation of its state lock
    /// its caller already holds ([`super::CommonsSourceRead::into_look`]), with no observation of
    /// its own.
    pub(super) fn standing_on(paths: &'a Paths, lock: StateLock) -> Self {
        Self::of(paths, Some(super::Observing::new(lock, paths)))
    }

    fn of(paths: &'a Paths, observation: Option<super::Observing>) -> Self {
        Self {
            paths,
            observation,
            moment: Moment::new(paths),
        }
    }

    /// [`Self::new`], its glance begun at `began` ([`fsx::Glance::begun_at`]): a look that began
    /// as a file it reads changed, which the settled rule judges by when the glance began.
    #[cfg(test)]
    pub(crate) fn begun_at(paths: &'a Paths, began: std::time::SystemTime) -> Self {
        let mut look = Self::new(paths);
        look.moment = Moment::begun_at(paths, began);
        look
    }

    /// Let go of every value this look derived of its moment ([`Moment`]): the stamps it took of
    /// each memo's files once for every memo of their set, its look at `state.toml`, the digests of
    /// the state each Commons log observation stands on, and its judgements of each access's
    /// currency. It keeps its glance, the `lstat` of each file it looked at first, and whether its
    /// observation guards the lock at the vault's path ([`Self::held`]), which, once found, every
    /// later judgement through the same glance finds again. A retained table session calls it at
    /// the start of each of its revalidations (each `ReadValidation::current`, `aterm-db`'s
    /// `with_retained_session`), whose look lives across many checks: each revalidation then
    /// derives what it judges afresh through that glance, as it did before the look kept any of
    /// it, and a value is taken once only within one revalidation. The checks its callers make
    /// outside a revalidation (the currency of the store contexts `Contributor::observe` and
    /// `Contributor::check` judge, which `with_retained_store` makes before its
    /// `ReadValidation::current`) are given what the look last derived, as the look gives any ask
    /// what an earlier one took ([`VaultLook`]).
    pub fn forget_derived(&self) {
        self.moment.forget();
    }

    /// The vault this looks at.
    #[must_use]
    pub const fn paths(&self) -> &'a Paths {
        self.paths
    }

    /// Check that this look still retains the state lock at its current path.
    /// This rechecks the filesystem rather than the look's memo and acquires
    /// no replacement lock. Custody alone authenticates no data or membership.
    #[must_use]
    pub fn guards_source(&self) -> bool {
        self.observation
            .as_ref()
            .is_some_and(|observation| observation.lock().observes(self.paths))
    }

    /// The shared observation of the state lock this look holds, while it guards the lock at the
    /// vault's path as the look first saw that path ([`StateLock::observes_in`], through its
    /// glance), with the moment the look stands for ([`Under`]): what a read under its caller's
    /// observation takes ([`trusted_commons_log_under`], [`local_recipient_denied_under`]). `None`
    /// where the look observes nothing. It judges that path once: once it found its observation
    /// guards the lock there, every later call through the same glance would find it again (the
    /// glance gives the `lstat` of the lock's path it took first, and the observation's descriptor
    /// names the same file while it lives), so it gives the observation without judging again; a
    /// judgement that found it does not, or whose `lstat` failed, is made again at the next call.
    pub(crate) fn held(&self) -> Option<Under<'_>> {
        let lock = self.observation.as_ref()?.lock();
        self.moment.hold(lock).then_some(Under {
            lock,
            moment: &self.moment,
        })
    }

    /// Whether this look's observation guards the lock at its vault's path, as [`Self::held`]
    /// judges it: through the look's glance until it found that it does, and then without judging
    /// again. `false` for a look that observes nothing (a writer held the lock when it was taken,
    /// or the lock could not be observed), or whose observation guards no lock at that path as its
    /// glance saw it.
    #[must_use]
    pub fn holds_vault(&self) -> bool {
        self.held().is_some()
    }

    /// [`Self::held`] where this look is at the vault at `paths`; `None` at another vault.
    pub(crate) fn held_at(&self, paths: &Paths) -> Option<Under<'_>> {
        self.held().filter(|_| self.paths == paths)
    }

    /// [`observe_commons_quiet`] through this look: a table session's check of its custody at its
    /// open, and at each store open and store read batch of a read batch that keeps the look
    /// (`Bootstrap::revalidate_custody_in`), and a table-repair round's judgement of the bootstrap
    /// its replica keeps through its opening batch's look (`Bootstrap::stands_in`).
    ///
    /// # Errors
    /// As [`observe_commons`](super::observe_commons).
    pub(crate) fn observe_commons(&self) -> crate::Result<crate::commons::Observation> {
        self.ask(&OBSERVATION, &(), super::observe_commons)
    }

    /// `memo`'s answer for `key` through this look: under its observation and its moment
    /// ([`Memo::read_under`]), or, where it observes nothing or its observation guards no lock
    /// at the vault's path ([`Self::held`]), the memo's own call.
    fn ask<T: Clone, K: Clone + PartialEq>(
        &self,
        memo: &Memo<T, K>,
        key: &K,
        read: impl FnOnce(&Paths) -> crate::Result<T>,
    ) -> crate::Result<T> {
        match self.held() {
            Some(under) => memo.read_under(self.paths, key, under, read),
            None => memo.read_keyed(self.paths, key, read),
        }
    }

    /// [`swarm_roster_now_until`] through this look.
    ///
    /// # Errors
    /// As [`swarm_roster_until`](super::swarm_roster_until).
    pub fn swarm_roster_until(
        &self,
        deadline: std::time::Instant,
    ) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
        self.ask(&ROSTER, &(), |paths| {
            super::swarm_roster_until(paths, deadline)
        })
    }

    /// [`swarm_denied_nodes_now`] through this look.
    ///
    /// # Errors
    /// As [`swarm_denied_nodes`](super::swarm_denied_nodes).
    pub fn swarm_denied_nodes(&self) -> crate::Result<Option<Vec<String>>> {
        Ok(self
            .ask(&DENIAL, &(), super::swarm_denial_snapshot)?
            .into_nodes())
    }

    /// [`acknowledged_commons_log_now`] for `trust` through this look.
    ///
    /// # Errors
    /// As [`acknowledged_commons_log`](super::acknowledged_commons_log).
    pub fn acknowledged_commons_log(
        &self,
        trust: crate::commons::journal::Trust,
    ) -> crate::Result<super::CommonsLog> {
        self.ask(&ACKNOWLEDGED, &trust, |paths| {
            super::acknowledged_commons_log(paths, trust)
        })
    }

    /// [`trusted_commons_log_now`] for `trust` through this look: a table-repair read batch's
    /// probe of the log's Commons and fold epochs (`aterm-db`'s `Session::history_needs`).
    ///
    /// # Errors
    /// As [`trusted_commons_log`](super::trusted_commons_log).
    pub fn trusted_commons_log(
        &self,
        trust: crate::commons::journal::Trust,
    ) -> crate::Result<Option<(crate::membership_log::Log, [u8; 32])>> {
        self.acknowledged_commons_log(trust)
            .map(super::CommonsLog::trusted)
    }

    /// [`local_swarm_roster_for_log_now`] for `log` through this look.
    ///
    /// # Errors
    /// As [`local_swarm_roster_for_log`](super::local_swarm_roster_for_log).
    pub fn local_swarm_roster_for_log(
        &self,
        log: &crate::membership_log::Log,
    ) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
        self.ask(&LOG_ROSTERS, &log_key(log), |paths| {
            super::local_swarm_roster_for_log(paths, log)
        })
    }

    /// Every record this Mac holds ([`load_records`](super::load_records)), read again when a file
    /// [`inputs`] names or the records' selection and data, stamped apart, moved: those it reads
    /// (`source.toml` and the held envelope that configure it, the retained history that pins its
    /// root) and the lock, the state and the identity the other memos read, so a pass that
    /// rewrites only the state reads them again too. Quiet, as [`local_swarm_roster_for_log_now`],
    /// so a writer holding the vault meets a fresh read, as before.
    ///
    /// # Errors
    /// As [`load_records`](super::load_records).
    pub fn records(&self) -> crate::Result<crate::membership_log::Store> {
        self.ask(&RECORDS, &(), super::load_records)
    }
}

/// What one look at the vault ([`VaultLook`]) takes of the moment it stands for: one `lstat` of
/// each file its checks look at ([`fsx::Glance`]), whether its observation guards the lock at the
/// vault's path ([`VaultLook::held`]), the stamps of each set of files its memos read
/// ([`Under::stamps`]), its look at `state.toml` ([`Under::state_look`]), the digest of the state
/// each Commons log observation stands on ([`Under::log_state`]), and whether each access judged
/// through it is current ([`Under::currency`]). It lives and dies with its look. A check made on
/// its own call, under an observation of its own, takes a moment of its own ([`Moment::new`],
/// through [`Under::of`]), which judges nothing for later asks: each of them is judged where it is
/// asked, as before.
pub(crate) struct Moment<'p> {
    paths: &'p Paths,
    glance: fsx::Glance,
    /// Whether the look's observation was judged to guard the lock at the vault's path through
    /// `glance` ([`Self::hold`]); set only to `true`, and only by its look.
    held: Cell<bool>,
    /// Where the look's observation (its [`StateLock`]) stood when its look last gave it out
    /// ([`VaultLook::held`]): an [`Under`] stands for this moment only beside that very lock
    /// ([`Self::caching_for`]), whatever other lock a caller puts beside it.
    lock: Cell<usize>,
    /// The stamps of [`Set::Vault`]'s files ([`Under::stamps`]) as the look first took them, with
    /// the record bounds they were taken under; kept only where taking them looked at nothing
    /// afresh ([`fsx::fresh_looks`]).
    vault: RefCell<Option<(RecordBounds, InputStamps)>>,
    /// The stamps of [`Set::Observation`]'s files, kept as `vault`'s are.
    observation: RefCell<Option<(RecordBounds, InputStamps)>>,
    /// `state.toml` as a memo stamps it ([`Under::state_look`]), kept as `vault` is.
    state: Cell<Option<StateLook>>,
    /// The digest of the state each Commons log observation stands on, by the digest of the log's
    /// heads ([`Under::log_state`]), kept as `vault` is: [`MAX_LOG_STATES`] of them, the oldest
    /// let go first.
    log_states: RefCell<VecDeque<([u8; 32], [u8; 32])>>,
    /// Whether each log-backed access judged through the look is current, by what it was judged
    /// for and the record bounds it was judged under ([`Under::currency`]), kept as `vault` is:
    /// [`MAX_LOG_STATES`] of them, the oldest let go first.
    currency: RefCell<VecDeque<((Currency, RecordBounds), bool)>>,
}

/// How many logs' state digests, and how many accesses' currency, a look keeps
/// ([`Moment::log_states`], [`Moment::currency`]): a check judges one log or two and an access or
/// two, and one let go is derived again.
const MAX_LOG_STATES: usize = 8;

/// What a judgement of a log-backed access's currency (`commons::roster`'s
/// `log_observation_is_current`) is judged for: the trust and recipient it was opened for, the
/// head and basis it holds, and the digest of the state it remembers. A look keeps a judgement by
/// these and this thread's record bounds ([`Under::currency`]); it holds no key and no access.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Currency {
    pub(crate) trust: crate::commons::journal::Trust,
    pub(crate) recipient: Option<crate::age::Recipient>,
    pub(crate) head: [u8; 32],
    pub(crate) basis: [u8; 32],
    pub(crate) remembered: Option<[u8; 32]>,
}

impl<'p> Moment<'p> {
    /// A moment of the vault at `paths` that has looked at nothing and judged nothing yet.
    pub(crate) fn new(paths: &'p Paths) -> Self {
        Self::looking(paths, fsx::Glance::new())
    }

    /// [`Self::new`], its glance begun at `began` ([`fsx::Glance::begun_at`]).
    #[cfg(test)]
    fn begun_at(paths: &'p Paths, began: std::time::SystemTime) -> Self {
        Self::looking(paths, fsx::Glance::begun_at(began))
    }

    fn looking(paths: &'p Paths, glance: fsx::Glance) -> Self {
        Self {
            paths,
            glance,
            held: Cell::new(false),
            lock: Cell::new(0),
            vault: RefCell::new(None),
            observation: RefCell::new(None),
            state: Cell::new(None),
            log_states: RefCell::new(VecDeque::new()),
            currency: RefCell::new(VecDeque::new()),
        }
    }

    /// Where the stamps of `set`'s files are kept.
    const fn stamps_of(&self, set: Set) -> &RefCell<Option<(RecordBounds, InputStamps)>> {
        match set {
            Set::Vault => &self.vault,
            Set::Observation => &self.observation,
        }
    }

    /// Let go of every value derived of this moment but its glance and its judgement of its
    /// look's observation ([`VaultLook::forget_derived`]).
    fn forget(&self) {
        self.vault.take();
        self.observation.take();
        self.state.take();
        self.log_states.take();
        self.currency.take();
    }

    /// Whether `lock`, the observation of this moment's look, guards the lock at the vault's path
    /// through this moment's glance ([`StateLock::observes_in`]): judged until it does, and then
    /// never again ([`VaultLook::held`]). It records where `lock` stands, which a borrow of the
    /// look keeps there while the [`Under`] given with it lives.
    fn hold(&self, lock: &StateLock) -> bool {
        if !self.held.get() {
            #[cfg(any(test, debug_assertions))]
            count_derivation(Derived::LockJudgment, self.held.get());
            if !lock.observes_in(self.paths, Some(&self.glance)) {
                return false;
            }
            self.held.set(true);
        }
        self.lock.set(address(lock));
        true
    }

    /// Whether what this moment judged stands for an ask of the vault at `paths` made under
    /// `lock`: its look found its observation guards the lock at the vault's path, `lock` is that
    /// very observation (the same address, as [`std::ptr::eq`] compares), and `paths` are the
    /// look's.
    fn caching_for(&self, paths: &Paths, lock: &StateLock) -> bool {
        self.held.get()
            && self.lock.get() == address(lock)
            && (std::ptr::eq(self.paths, paths) || self.paths == paths)
    }
}

/// Where `lock` stands in memory: one lock's, while it lives, and no other's.
fn address(lock: &StateLock) -> usize {
    std::ptr::from_ref(lock).addr()
}

/// A shared observation of the vault's state lock and the moment a check judges under it: a
/// look's own ([`VaultLook::held`]), whose moment stands for every check made through the look,
/// or a check's own observation with a moment of its own ([`Self::of`]), which judges each ask
/// where it is asked.
#[derive(Clone, Copy)]
pub(crate) struct Under<'m> {
    lock: &'m StateLock,
    moment: &'m Moment<'m>,
}

impl<'m> Under<'m> {
    /// `lock`, a caller's own observation, with `moment`, one of its own ([`Moment::new`]).
    pub(crate) const fn of(lock: &'m StateLock, moment: &'m Moment<'m>) -> Self {
        Self { lock, moment }
    }

    /// The observation.
    pub(crate) const fn lock(self) -> &'m StateLock {
        self.lock
    }

    /// The moment's look at each file.
    pub(crate) const fn glance(self) -> &'m fsx::Glance {
        &self.moment.glance
    }

    /// Whether the moment's judgement of its look's observation stands for an ask of the vault
    /// at `paths` under this observation ([`Moment::caching_for`]).
    fn caching_for(self, paths: &Paths) -> bool {
        self.moment.caching_for(paths, self.lock)
    }

    /// The stamps of `set`'s files ([`Set::inputs`]) through the moment's glance, as a memo of
    /// that set stamps them ([`stamp_memo_inputs_in`], with the records' selection): those the
    /// moment kept under this thread's record bounds, and otherwise taken now, and kept where
    /// taking them looked at nothing afresh ([`fsx::fresh_looks`]: every `lstat` a first one of
    /// the glance, every other input a hit of a cache keyed by what those showed) and none are
    /// kept yet. [`Set::Observation`]'s are [`Set::Vault`]'s, from the moment where it kept them,
    /// with the device files' [`device_files`] after them: the files [`observation_inputs`] names
    /// in its order, so the same stamps the flat ones are. For an ask whose [`Under`] stands for
    /// its look's moment ([`Self::caching_for`]).
    fn stamps(self, set: Set) -> Option<Stamps<'m>> {
        let moment = self.moment;
        let cell = moment.stamps_of(set);
        let bounds = record_bounds();
        let kept = |cell: Ref<'m, Option<(RecordBounds, InputStamps)>>| {
            Ref::filter_map(cell, |kept| {
                kept.as_ref()
                    .filter(|(at, _)| *at == bounds)
                    .map(|(_, stamps)| stamps)
            })
            .ok()
        };
        if let Some(stamps) = kept(cell.borrow()) {
            return Some(Stamps::Kept(stamps));
        }
        #[cfg(any(test, debug_assertions))]
        count_derivation(Derived::Stamps(set), cell.borrow().is_some());
        let before = fsx::fresh_looks();
        let stamps = match set {
            Set::Vault => stamp_memo_inputs_with(
                moment.paths,
                set.inputs(),
                true,
                Some(&moment.glance),
                || self.state_look(),
            ),
            Set::Observation => self
                .stamps(Set::Vault)
                .and_then(|base| with_device_files(&base, moment.paths, &moment.glance)),
        }?;
        if fsx::fresh_looks() != before || cell.borrow().is_some() {
            return Some(Stamps::Taken(stamps));
        }
        *cell.borrow_mut() = Some((bounds, stamps));
        kept(cell.borrow()).map(Stamps::Kept)
    }

    /// `state.toml` as a memo stamps it ([`state_look`]) through the moment's glance: the look the
    /// moment kept, and otherwise taken now, and kept where taking it looked at nothing afresh
    /// ([`fsx::fresh_looks`]: the file's first `lstat` the glance's, and a look kept for that
    /// stamp). For an ask whose [`Under`] stands for its look's moment ([`Self::caching_for`]).
    fn state_look(self) -> Option<StateLook> {
        let moment = self.moment;
        if let Some(look) = moment.state.get() {
            return Some(look);
        }
        #[cfg(any(test, debug_assertions))]
        count_derivation(Derived::StateLook, moment.state.get().is_some());
        let before = fsx::fresh_looks();
        let look = state_look(moment.paths, Some(&moment.glance))?;
        if fsx::fresh_looks() == before {
            moment.state.set(Some(look));
        }
        Some(look)
    }

    /// The digest of the state a Commons log observation of the vault at `paths` whose heads
    /// digest to `heads` stands on, `derive`d through this observation and its glance
    /// (`commons_log_state_digest`): where this [`Under`] stands for its look's moment
    /// ([`Self::caching_for`]), the digest the moment kept for `heads`, and otherwise derived now,
    /// and kept where deriving it looked at nothing afresh ([`fsx::fresh_looks`]: each file's
    /// first `lstat` the glance's, each digest one kept for what that `lstat` showed). An error is
    /// never kept.
    ///
    /// # Errors
    /// As `derive`.
    pub(crate) fn log_state(
        self,
        paths: &Paths,
        heads: [u8; 32],
        derive: impl FnOnce() -> crate::Result<[u8; 32]>,
    ) -> crate::Result<[u8; 32]> {
        if !self.caching_for(paths) {
            return derive();
        }
        let states = &self.moment.log_states;
        if let Some(digest) = held(states, &heads) {
            return Ok(digest);
        }
        #[cfg(any(test, debug_assertions))]
        count_derivation(Derived::LogState, !states.borrow().is_empty());
        let before = fsx::fresh_looks();
        let digest = derive()?;
        if fsx::fresh_looks() == before {
            keep(states, heads, digest);
        }
        Ok(digest)
    }

    /// Whether a log-backed access judged for `currency` is current, `judge`d through this
    /// observation and its glance (`commons::roster`'s `log_observation_is_current`): where this
    /// [`Under`] stands for its look's moment ([`Self::caching_for`]), the judgement the moment
    /// kept for `currency` under this thread's record bounds, and otherwise judged now, and kept
    /// where judging it looked at nothing afresh ([`fsx::fresh_looks`]: each file's first `lstat`
    /// the glance's, each read a hit of a cache keyed by what those showed). An error is never
    /// kept, nor any key or access: only whether it is current.
    ///
    /// # Errors
    /// As `judge`.
    pub(crate) fn currency(
        self,
        paths: &Paths,
        currency: Currency,
        judge: impl FnOnce() -> crate::Result<bool>,
    ) -> crate::Result<bool> {
        if !self.caching_for(paths) {
            return judge();
        }
        let judged = &self.moment.currency;
        let key = (currency, record_bounds());
        if let Some(current) = held(judged, &key) {
            return Ok(current);
        }
        #[cfg(any(test, debug_assertions))]
        count_derivation(Derived::Currency, !judged.borrow().is_empty());
        let before = fsx::fresh_looks();
        let current = judge()?;
        if fsx::fresh_looks() == before {
            keep(judged, key, current);
        }
        Ok(current)
    }
}

/// The value `kept` holds for `key`.
fn held<K: PartialEq, V: Copy>(kept: &RefCell<VecDeque<(K, V)>>, key: &K) -> Option<V> {
    kept.borrow()
        .iter()
        .find(|(at, _)| at == key)
        .map(|(_, value)| *value)
}

/// Keep `value` for `key` in `kept` where nothing is kept for it yet (a derivation within the one
/// that derived `value` may have kept one), the oldest let go past [`MAX_LOG_STATES`].
fn keep<K: PartialEq, V: Copy>(kept: &RefCell<VecDeque<(K, V)>>, key: K, value: V) {
    if held(kept, &key).is_some() {
        return;
    }
    let mut kept = kept.borrow_mut();
    if kept.len() >= MAX_LOG_STATES {
        kept.pop_front();
    }
    kept.push_back((key, value));
}

/// `base`, the stamps of [`Set::Vault`]'s files through `glance`, with the stamps of the device
/// files ([`device_files`]) through it after them: the stamps of [`Set::Observation`]'s, as
/// [`stamp_memo_inputs_in`] takes them (`None` where an `lstat` fails, as there).
fn with_device_files(
    base: &InputStamps,
    paths: &Paths,
    glance: &fsx::Glance,
) -> Option<InputStamps> {
    let device = device_files(paths)
        .iter()
        .map(|path| match fsx::lstat_in(Some(glance), path) {
            Ok(meta) => Some(meta.as_ref().map(Stamp::of)),
            Err(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(InputStamps {
        stamps: FileStamps {
            directory: base.stamps.directory,
            files: base.stamps.files.iter().copied().chain(device).collect(),
        },
        records: base.records.clone(),
        state: base.state,
    })
}

/// A memo's stamps of its inputs through a look ([`Memo::stamps_in`]): those its moment kept, or
/// those taken for this ask.
#[expect(
    clippy::large_enum_variant,
    reason = "one lives on the stack for one ask; boxing the taken stamps would allocate for every ask a look does not keep, which stamps taken by value never did"
)]
enum Stamps<'m> {
    Kept(Ref<'m, InputStamps>),
    Taken(InputStamps),
}

impl std::ops::Deref for Stamps<'_> {
    type Target = InputStamps;

    fn deref(&self) -> &InputStamps {
        match self {
            Self::Kept(stamps) => stamps,
            Self::Taken(stamps) => stamps,
        }
    }
}

/// Every record this Mac holds ([`load_records`](super::load_records)) from the memo a look's
/// records ask ([`VaultLook::records`]), as that memo's own call: read again when a file it stamps
/// moved, and afresh while a writer holds the vault. A Commons frontier pass read them afresh
/// twice, and a push its witness did not settle once more: 3.2% of a sustained quiet round over a
/// log of 9,605 records and 3.8% over a short one (2026-11-09).
///
/// # Errors
/// As [`load_records`](super::load_records).
pub fn records_now(paths: &Paths) -> crate::Result<crate::membership_log::Store> {
    RECORDS.read_keyed(paths, &(), super::load_records)
}

/// What [`VaultLook::records`] and [`records_now`] keep.
static RECORDS: Memo<crate::membership_log::Store> = Memo::of(Set::Vault, true);

/// The files [`swarm_denied_nodes`](super::swarm_denied_nodes) and
/// [`swarm_roster_until`](super::swarm_roster_until) take their answer from: the configured source
/// (`source.toml`), the state lock the deny list observes (`.lock`, whose presence and mode decide
/// that read), the applied state (`state.toml`, which a memo stamps by what it holds but `heard`
/// and the rounds' receipts: [`stamp_memo_inputs_in`]), this Mac's identity, the held envelope
/// (`vault`) and the membership history this Mac acknowledged
/// ([`Paths::acknowledged_history_files`]); and, stamped apart ([`stamp_inputs`]), every file the
/// selection of the membership records' data is judged by.
/// The vault folder holds others that change every few seconds and that these reads never open
/// (the vault server's `served/`, the rest of `commons/`, the jobs' logs, the needs and answers
/// queues), so a change to them reads nothing again.
pub(super) fn inputs(paths: &Paths) -> Option<Vec<PathBuf>> {
    let [claim, anchor, receipts, checkpoint] = paths.acknowledged_history_files();
    Some(vec![
        paths.source_file(),
        paths.lock_file(),
        paths.state_file(),
        paths.identity_file(),
        paths.cache_file(),
        claim,
        anchor,
        receipts,
        checkpoint,
    ])
}

/// The files [`observe_commons`](super::observe_commons) and its check of this Mac's own denial
/// ([`local_recipient_denied`](super::steward::local_recipient_denied)) take their answers from:
/// [`inputs`], and the Commons device files that denial check reads to bind this Mac's own node.
pub(super) fn observation_inputs(paths: &Paths) -> Option<Vec<PathBuf>> {
    let mut files = inputs(paths)?;
    files.extend(device_files(paths));
    Some(files)
}

/// The Commons device files [`observation_inputs`] adds to [`inputs`], in its order: this Mac's
/// device age identity and the claim it is held to.
fn device_files(paths: &Paths) -> [PathBuf; 2] {
    let device = paths.dir.join("commons");
    [device.join("device.age"), device.join("device.claim")]
}

/// The two sets of files the vault's memos stamp ([`Memo::of`]), which a look stamps once each for
/// every memo it asks ([`Under::stamps`]).
#[derive(Clone, Copy)]
enum Set {
    /// [`inputs`]: the deny list's, the roster's, the logs' and the records'.
    Vault,
    /// [`observation_inputs`]: the Commons observation's and this Mac's own denial's.
    Observation,
}

impl Set {
    /// The files this set names.
    const fn inputs(self) -> fn(&Paths) -> Option<Vec<PathBuf>> {
        match self {
            Self::Vault => inputs,
            Self::Observation => observation_inputs,
        }
    }
}

/// The files the members desk reads this Mac's membership from ([`desk_files`]), stamped as the
/// memo stamps its inputs, and `state.toml` by what it holds but for the telemetry of the vault's
/// rounds ([`state_standing`](super::state_standing)): a desk lane, or the node's automatic
/// admission, that read them to a settled answer ([`Local::of`]'s member or a Mac that left,
/// [`undo_offers`]'s rows) need not read them again while they stand as they were. Equal only to
/// the stamps of the same files unchanged, and the same state but for when the vault last heard
/// this Mac and the receipts of its last rounds, which no part of the desk's answer is read from:
/// the vault's subscriber rewrites `state.toml` for each of those about once a minute (a round
/// rewrote it about every 25 s when this was measured, Linux arm64, 2026-11-09), and each
/// rewrite read the desk again: 26 MB on that member's automatic admission, 14.4 MB and 10 ms of
/// CPU on a fixture member whose envelope is 1.2 MB, where taking these stamps reads 1.3 KB in
/// 30 µs.
///
/// [`Local::of`]: crate::members::desk::Local::of
/// [`undo_offers`]: super::undo_offers
#[derive(PartialEq, Eq)]
pub struct DeskInputs {
    stamps: InputStamps,
    state: Option<[u8; 32]>,
}

/// The stamps of the files the members desk reads this Mac's membership from, now; `None` where a
/// file's metadata, the vault folder or `state.toml` cannot be read safely (read again).
#[must_use]
pub fn desk_inputs(paths: &Paths) -> Option<DeskInputs> {
    Some(DeskInputs {
        stamps: stamp_inputs(paths, desk_files)?,
        state: super::state_standing(paths).ok()?,
    })
}

/// [`desk_inputs`] for a reader about to give what it read under them, taken as a quiet memo
/// takes its stamps ([`local_swarm_roster_for_log_now`]): under a shared observation of the
/// vault's state lock, so `None` while a writer holds it (mid-apply, its files not yet renamed
/// in) or where it cannot be observed, and that reader reads afresh and meets the writer, as
/// before.
#[must_use]
pub fn desk_inputs_quiet(paths: &Paths) -> Option<DeskInputs> {
    match StateLock::observe_classified(paths) {
        Ok(StateObservation::Held(lock)) => {
            let inputs = desk_inputs(paths);
            drop(lock);
            inputs
        }
        Ok(StateObservation::Absent) => desk_inputs(paths),
        Ok(StateObservation::Busy) | Err(_) => None,
    }
}

/// The files [`Local::of`](crate::members::desk::Local::of) and
/// [`undo_offers`](super::undo_offers) take their answer from, but `state.toml`, which
/// [`desk_inputs`] stamps by what it holds: [`observation_inputs`], the vault slot's creation
/// claim, the Commons custody that seats this Mac (the retained claim, the custody locks, the
/// pinned anchor), in a debug build a lab vault's home LAN file, and the control journal, by its
/// directory (an entry came or went) and each entry in it and in the folders it holds (an
/// append). `None` where the journal cannot be listed.
pub(super) fn desk_files(paths: &Paths) -> Option<Vec<PathBuf>> {
    let mut files = observation_inputs(paths)?;
    let state = paths.state_file();
    files.retain(|file| *file != state);
    let commons = paths.dir.join("commons");
    let control = commons.join("control");
    files.extend([
        super::creation::claim_file(paths),
        paths.dir.join("commons.claim"),
        paths.dir.join("commons.lock"),
        paths.dir.join("commons-device.lock"),
        commons.join("default.pin"),
    ]);
    // A debug build's lab vault names its home LAN in its own file (`members::lab_lan`).
    #[cfg(any(test, debug_assertions))]
    files.push(paths.dir.join(crate::members::lab_lan::FILE));
    let mut folders = vec![control];
    while let Some(folder) = folders.pop() {
        fsx::look_afresh();
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                files.push(folder);
                continue;
            }
            Err(_) => return None,
        };
        let (mut inside, mut below) = (Vec::new(), Vec::new());
        for entry in entries {
            let entry = entry.ok()?;
            if entry.file_type().ok()?.is_dir() {
                below.push(entry.path());
            } else {
                inside.push(entry.path());
            }
        }
        inside.sort();
        below.sort_unstable_by(|a, b| b.cmp(a));
        files.push(folder);
        files.extend(inside);
        folders.extend(below);
    }
    Some(files)
}

/// What one input's metadata says of it. A vault write replaces the whole file
/// (`fsx::write_atomic`: a new inode), and any write moves its change time, which nothing can set
/// back.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            inode: meta.ino(),
            len: meta.size(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

/// The directory's identity and access metadata, without the content timestamps that an
/// unrelated job or server write moves. A symlink here is refused by [`StateLock`], even when
/// every input path still resolves to the files from an earlier read.
#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectoryStamp {
    dev: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

/// The vault directory and each of a set of files as they stood ([`stamp_files`]).
#[derive(PartialEq, Eq)]
#[cfg_attr(test, derive(Clone))]
pub(super) struct FileStamps {
    directory: Option<DirectoryStamp>,
    files: Vec<Option<Stamp>>,
}

/// [`FileStamps`] of a memo's inputs, with the membership records' selection ([`stamp_inputs`]).
#[derive(PartialEq, Eq)]
#[cfg_attr(test, derive(Clone))]
pub(super) struct InputStamps {
    stamps: FileStamps,
    /// Every file the selection of the membership records' data is judged by, which each read
    /// these stamps stand for reads ([`inputs`] and the sets that extend it); `None` for a read
    /// that opens no record ([`config_now`], [`Memo::unrecorded`]).
    records: Option<crate::records_storage::Standing>,
    /// `state.toml` by what it holds but when the vault last heard this Mac and the receipts of
    /// its rounds ([`StateLook::standing`]), where a memo's inputs name it ([`Memo::stamps`]);
    /// `None` where `stamps` stamp it by its metadata or the inputs do not name it.
    state: Option<Option<[u8; 32]>>,
}

/// Source inputs and the record limits of a stage-owned materialization selection.
#[derive(PartialEq, Eq)]
pub(super) struct SelectionInputs {
    inputs: InputStamps,
    bounds: RecordBounds,
}

pub(super) fn selection_inputs(paths: &Paths) -> Option<SelectionInputs> {
    Some(SelectionInputs {
        inputs: stamp_inputs(paths, observation_inputs)?,
        bounds: record_bounds(),
    })
}

/// Every file `inputs`' stamps stand for: its own and the membership records' selection's
/// ([`records_storage::standing_files`](crate::records_storage::standing_files)).
#[cfg(test)]
pub(super) fn stamped_files(
    paths: &Paths,
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
) -> Option<Vec<PathBuf>> {
    let mut files = inputs(paths)?;
    files.extend(crate::records_storage::standing_files(&paths.records_file()).ok()?);
    Some(files)
}

/// Every file [`desk_inputs`] stands for: [`stamped_files`] of [`desk_files`], and `state.toml`,
/// which it stamps by what it holds.
#[cfg(test)]
pub(super) fn desk_stamped_files(paths: &Paths) -> Option<Vec<PathBuf>> {
    let mut files = stamped_files(paths, desk_files)?;
    files.push(paths.state_file());
    Some(files)
}

/// [`stamp_files`] of `inputs`, with the membership records' selection as it stands
/// ([`records_storage::standing`](crate::records_storage::standing)). Records the selection
/// refuses require a fresh read.
pub(super) fn stamp_inputs(
    paths: &Paths,
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
) -> Option<InputStamps> {
    stamp_inputs_in(paths, inputs, true, None)
}

/// [`stamp_inputs`], with the records' selection where `records` (a read that opens none stamps
/// none), and each `lstat` `glance`'s where the caller looks through one ([`fsx::Glance`]): the
/// stamps of a check that asks several memos of the same files.
fn stamp_inputs_in(
    paths: &Paths,
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
    records: bool,
    glance: Option<&fsx::Glance>,
) -> Option<InputStamps> {
    Some(InputStamps {
        stamps: stamp_files_in(paths, inputs, glance)?,
        records: records_standing(paths, records, glance)?,
        state: None,
    })
}

/// [`stamp_inputs_in`] as a memo stamps its inputs ([`Memo::stamps`]): `state.toml`, where they
/// name it, by what it holds but `heard` and the rounds' receipts ([`state_look`]), which no
/// memo's answer is read from (a memo that gives `heard`, [`swarm_member_heard_now`], reads it
/// apart), and every other file by its metadata. The vault's subscriber rewrites `state.toml`
/// for each of those fields alone about once a minute (a round rewrote it about every 25 s when
/// this was measured, Linux arm64, 2026-11-09), and each rewrite read every memo here again: on a
/// fleet node, the deny list its watch asks every two seconds read the held vault five times
/// over, and its LAN presence twice.
fn stamp_memo_inputs_in(
    paths: &Paths,
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
    records: bool,
    glance: Option<&fsx::Glance>,
) -> Option<InputStamps> {
    stamp_memo_inputs_with(paths, inputs, records, glance, || state_look(paths, glance))
}

/// [`stamp_memo_inputs_in`], `state.toml`'s look `state`'s where the inputs name it: a look's own
/// ([`Under::state_look`]), or [`state_look`] through `glance`.
fn stamp_memo_inputs_with(
    paths: &Paths,
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
    records: bool,
    glance: Option<&fsx::Glance>,
    state: impl FnOnce() -> Option<StateLook>,
) -> Option<InputStamps> {
    let mut files = inputs(paths)?;
    let state_file = paths.state_file();
    let named = files.len();
    files.retain(|file| *file != state_file);
    Some(InputStamps {
        stamps: stamp_paths_in(paths, &files, glance)?,
        records: records_standing(paths, records, glance)?,
        state: if files.len() < named {
            Some(state()?.standing)
        } else {
            None
        },
    })
}

/// The membership records' selection as it stands, where `records`; `Some(None)` where not, and
/// `None` where it cannot be stamped.
fn records_standing(
    paths: &Paths,
    records: bool,
    glance: Option<&fsx::Glance>,
) -> Option<Option<crate::records_storage::Standing>> {
    if records {
        Some(Some(crate::records_storage::standing(
            &paths.records_file(),
            glance,
        )?))
    } else {
        Some(None)
    }
}

/// The directory and each of `files`' stamps, without following a final symlink. A missing path
/// has no stamp. An unsafe directory or a metadata error requires a fresh read.
pub(super) fn stamp_files(
    paths: &Paths,
    files: fn(&Paths) -> Option<Vec<PathBuf>>,
) -> Option<FileStamps> {
    stamp_files_in(paths, files, None)
}

/// [`stamp_files`], each `lstat` `glance`'s where the caller looks through one ([`fsx::lstat_in`]).
fn stamp_files_in(
    paths: &Paths,
    files: fn(&Paths) -> Option<Vec<PathBuf>>,
    glance: Option<&fsx::Glance>,
) -> Option<FileStamps> {
    stamp_paths_in(paths, &files(paths)?, glance)
}

/// [`stamp_files_in`] of `files`.
fn stamp_paths_in(
    paths: &Paths,
    files: &[PathBuf],
    glance: Option<&fsx::Glance>,
) -> Option<FileStamps> {
    let directory = match fsx::lstat_in(glance, &paths.dir) {
        Ok(Some(meta)) if meta.is_dir() => Some(DirectoryStamp {
            dev: meta.dev(),
            inode: meta.ino(),
            uid: meta.uid(),
            gid: meta.gid(),
            mode: meta.mode(),
        }),
        Ok(Some(_)) | Err(_) => return None,
        Ok(None) => None,
    };
    let files = files
        .iter()
        .map(|path| match fsx::lstat_in(glance, path) {
            Ok(Some(meta)) => Some(Some(Stamp::of(&meta))),
            Ok(None) => Some(None),
            Err(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(FileStamps { directory, files })
}

/// [`state_standing`](super::state_standing) at `paths`, its `lstat` the glance's of `under`
/// where the caller looks through one: what the file read to while it shows the stamp it shows
/// now ([`state_look_in`]), and a fresh read otherwise.
///
/// # Errors
/// `state.toml` cannot be read.
pub(crate) fn state_standing_in(
    paths: &Paths,
    under: Option<Under<'_>>,
) -> crate::Result<Option<[u8; 32]>> {
    if let Some(look) = state_look_in(paths, under) {
        return Ok(look.standing);
    }
    fsx::look_afresh();
    Ok(super::state_standing_heard(paths)?.map(|(standing, _)| standing))
}

/// `state.toml` at `paths` as a memo stamps it ([`state_look`]), under `under` where the caller
/// judges under one: its look's own where it stands for its look's moment
/// ([`Under::state_look`]), and otherwise taken through its glance, or afresh with none.
fn state_look_in(paths: &Paths, under: Option<Under<'_>>) -> Option<StateLook> {
    match under {
        Some(under) if under.caching_for(paths) => under.state_look(),
        Some(_) | None => state_look(paths, under.map(Under::glance)),
    }
}

/// What a memo stamps `state.toml` by ([`stamp_memo_inputs_in`]), and when the vault last heard
/// this Mac, from one read of the file.
#[derive(Clone, Copy)]
struct StateLook {
    /// What it holds but `heard` and the receipts of the vault's rounds
    /// ([`state_standing`](super::state_standing)); `None` with no file.
    standing: Option<[u8; 32]>,
    /// `heard`, where it holds this Mac in its vault.
    heard: Option<u64>,
}

/// The last [`StateLook`] of each vault folder's `state.toml` read in this process, with the
/// stamp of the file it was read from, oldest first: [`MAX_FOLDERS`] of them, the oldest let go
/// first. In this crate's test process every live folder keeps its one, and a folder that is
/// gone (its test ended) lets it go, as [`Memo::keep`] does: a law that counts the reads its own
/// warm memo makes holds whatever other tests read meanwhile.
static STATE_LOOKS: Mutex<Vec<(PathBuf, Stamp, StateLook)>> = Mutex::new(Vec::new());

/// `state.toml` at `paths` as a memo stamps it ([`StateLook`]), its `lstat` `glance`'s where the
/// caller looks through one: what the file showing that stamp was read to, where one was, and
/// otherwise the file read now, kept for that stamp where the file showed it after the read as
/// well. `None` where the file cannot be stamped or read, or moved during the read (a fresh read
/// then).
fn state_look(paths: &Paths, glance: Option<&fsx::Glance>) -> Option<StateLook> {
    let file = paths.state_file();
    let stamp = |glance| match fsx::lstat_in(glance, &file) {
        Ok(meta) => Some(meta.as_ref().map(Stamp::of)),
        Err(_) => None,
    };
    let Some(before) = stamp(glance)? else {
        return Some(StateLook {
            standing: None,
            heard: None,
        });
    };
    let looks = || {
        STATE_LOOKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some((_, _, look)) = looks()
        .iter()
        .find(|(at, stamp, _)| *at == file && *stamp == before)
    {
        return Some(*look);
    }
    let (standing, heard) = match super::state_standing_heard(paths) {
        Ok(Some((standing, heard))) => (Some(standing), heard),
        Ok(None) | Err(_) => return None,
    };
    if stamp(None)? != Some(before) {
        return None;
    }
    let look = StateLook { standing, heard };
    let mut looks = looks();
    looks.retain(|(at, _, _)| *at != file);
    #[cfg(not(test))]
    if looks.len() >= MAX_FOLDERS {
        looks.remove(0);
    }
    #[cfg(test)]
    looks.retain(|(at, _, _)| at.parent().is_some_and(std::path::Path::exists));
    looks.push((file, before, look));
    Some(look)
}

struct Held<T, K> {
    paths: Paths,
    key: K,
    stamps: InputStamps,
    /// The record bounds the answer was read under ([`record_bounds`]).
    bounds: RecordBounds,
    answer: T,
}

/// The record budgets and byte bound every log and store made on this thread now takes.
type RecordBounds = (crate::membership_log::RecordBudgets, usize);

/// This thread's record bounds ([`RecordBudgets::current`], [`held_bytes_bound`]): the
/// production ones outside a test, and under a test's own (`RecordBudgets::within`,
/// `RecordBudgets::within_bytes`) the same files recover to other records, or are refused.
///
/// [`RecordBudgets::current`]: crate::membership_log::RecordBudgets::current
/// [`held_bytes_bound`]: crate::membership_log::budgets::held_bytes_bound
fn record_bounds() -> RecordBounds {
    (
        crate::membership_log::RecordBudgets::current(),
        crate::membership_log::budgets::held_bytes_bound(),
    )
}

/// The last settled answer of one read for each vault folder and key read, oldest first.
struct Memo<T, K = ()> {
    /// The files the read takes its answer from.
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
    /// The set `inputs` is, where it is one ([`Memo::of`]), whose stamps a look takes once for
    /// every memo of that set it asks ([`Memo::stamps_in`]).
    set: Option<Set>,
    /// Whether the read opens the membership records, whose selection is stamped beside
    /// `inputs` ([`stamp_inputs`]): every memo's but [`config_now`]'s.
    records: bool,
    /// Whether a kept answer is given only while no writer holds the vault's state lock: one
    /// that a peer check takes, whose rule is that a busy writer never yields a permissive
    /// verdict, so a writer mid-apply (its files not yet renamed in) reads as busy as before.
    quiet: bool,
    held: Mutex<Vec<Held<T, K>>>,
    /// Held across a read, so callers that miss together wait for one read instead of each
    /// making their own. A caller that finds its answer in `held` never takes it.
    refresh: Mutex<()>,
    /// How many callers have come to wait for `refresh`: the test's witness that they are queued.
    #[cfg(test)]
    waiting: std::sync::atomic::AtomicUsize,
}

impl<T: Clone> Memo<T> {
    #[cfg(test)]
    const fn new(inputs: fn(&Paths) -> Option<Vec<PathBuf>>) -> Self {
        Self::keyed(inputs, false)
    }

    fn read(
        &self,
        paths: &Paths,
        read: impl FnOnce(&Paths) -> crate::Result<T>,
    ) -> crate::Result<T> {
        self.read_keyed(paths, &(), read)
    }
}

impl<T: Clone, K: Clone + PartialEq> Memo<T, K> {
    /// A memo of a read whose answer depends on the files of `set` and on a key its caller names
    /// (the membership log a roster is read for); `quiet` as [`Memo::quiet`] says.
    const fn of(set: Set, quiet: bool) -> Self {
        Self::reading(set.inputs(), Some(set), true, quiet)
    }

    /// [`Memo::of`] for files of no set: a test's own.
    #[cfg(test)]
    const fn keyed(inputs: fn(&Paths) -> Option<Vec<PathBuf>>, quiet: bool) -> Self {
        Self::reading(inputs, None, true, quiet)
    }

    /// A memo of a read that depends on `inputs` (files of no set) and opens no membership
    /// record, so its records' selection is not stamped: a record that arrives reads nothing
    /// again; `quiet` as [`Memo::quiet`] says.
    const fn unrecorded(inputs: fn(&Paths) -> Option<Vec<PathBuf>>, quiet: bool) -> Self {
        Self::reading(inputs, None, false, quiet)
    }

    const fn reading(
        inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
        set: Option<Set>,
        records: bool,
        quiet: bool,
    ) -> Self {
        Self {
            inputs,
            set,
            records,
            quiet,
            held: Mutex::new(Vec::new()),
            refresh: Mutex::new(()),
            #[cfg(test)]
            waiting: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The stamps of this memo's inputs now ([`stamp_memo_inputs_in`]).
    fn stamps(&self, paths: &Paths, glance: Option<&fsx::Glance>) -> Option<InputStamps> {
        stamp_memo_inputs_in(paths, self.inputs, self.records, glance)
    }

    /// The stamps of this memo's inputs through `under`'s glance: where the memo's inputs are a
    /// set and `under` stands for its look's moment ([`Under::caching_for`]), those the look
    /// takes once for every memo of that set ([`Under::stamps`]), and otherwise taken for this ask
    /// ([`Self::stamps`]).
    fn stamps_in<'m>(&self, paths: &Paths, under: Under<'m>) -> Option<Stamps<'m>> {
        match self.set {
            Some(set) if under.caching_for(paths) => under.stamps(set),
            Some(_) | None => self.stamps(paths, Some(under.glance())).map(Stamps::Taken),
        }
    }

    /// The answer kept for `paths` and `key` under exactly `stamps`, read under this thread's
    /// record bounds ([`record_bounds`]).
    fn kept(&self, paths: &Paths, key: &K, stamps: &InputStamps) -> Option<T> {
        let bounds = record_bounds();
        let held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.iter()
            .find(|held| {
                held.paths == *paths
                    && held.key == *key
                    && held.stamps == *stamps
                    && held.bounds == bounds
            })
            .map(|held| held.answer.clone())
    }

    /// [`Memo::read_keyed`] for a caller that holds `under`'s lock. While that is a shared
    /// observation of the state lock that guards the lock at the vault's path as `under`'s glance
    /// saw it ([`StateLock::observes_in`]: judged here, or, for a look's own, once for the look,
    /// [`VaultLook::held`]), no writer holds that lock and none can write until the caller drops
    /// it: a quiet memo takes it for the one `read_keyed` takes before its stamps, and any memo
    /// gives what it kept under the stamps of that glance, the look of the caller's check, which
    /// stamps each file once for every memo the check asks ([`fsx::Glance`]), and, through a
    /// look's own, each set of files once for every memo of that set ([`Self::stamps_in`]).
    /// Anything else, a miss included, is `read_keyed`'s, which stamps afresh, and so looks
    /// afresh ([`fsx::look_afresh`]).
    fn read_under(
        &self,
        paths: &Paths,
        key: &K,
        under: Under<'_>,
        read: impl FnOnce(&Paths) -> crate::Result<T>,
    ) -> crate::Result<T> {
        if (under.caching_for(paths) || under.lock().observes_in(paths, Some(under.glance())))
            && let Some(stamps) = self.stamps_in(paths, under)
            && let Some(answer) = self.kept(paths, key, &stamps)
        {
            return Ok(answer);
        }
        fsx::look_afresh();
        self.read_keyed(paths, key, read)
    }

    fn read_keyed(
        &self,
        paths: &Paths,
        key: &K,
        read: impl FnOnce(&Paths) -> crate::Result<T>,
    ) -> crate::Result<T> {
        // A quiet memo answers from what it kept only under a shared observation of the state
        // lock, taken before the stamps: no writer holds it, and none can write until the answer
        // is given. Where one holds it, the read meets it as it always did.
        let quiet = if self.quiet {
            match StateLock::observe_classified(paths) {
                Ok(StateObservation::Held(lock)) => Some(Some(lock)),
                Ok(StateObservation::Absent) => Some(None),
                Ok(StateObservation::Busy) | Err(_) => return read(paths),
            }
        } else {
            None
        };
        // Taken before the read: a write after this moves the next stamps, so it is read again.
        let Some(stamps) = self.stamps(paths, None) else {
            return read(paths);
        };
        if let Some(answer) = self.kept(paths, key, &stamps) {
            return Ok(answer);
        }
        drop(quiet);
        #[cfg(test)]
        self.waiting
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _refresh = self
            .refresh
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Another caller may have read these same files while this one waited (a quiet memo
        // looks under the observation below).
        let Some(stamps) = self.stamps(paths, None) else {
            return read(paths);
        };
        if !self.quiet
            && let Some(answer) = self.kept(paths, key, &stamps)
        {
            return Ok(answer);
        }
        // A lock an older build left wide is narrowed before it is observed, as the read itself
        // narrows it, so the first read after an upgrade can settle. The narrowing moved the
        // lock's change time, so its stamps are taken again, still before the read.
        let narrowed = StateLock::narrow_owned(paths);
        let stamps = match narrowed {
            Ok(true) => match self.stamps(paths, None) {
                Some(stamps) => stamps,
                None => return read(paths),
            },
            Ok(false) | Err(_) => stamps,
        };
        // Held across the read: no writer can start while it is, so a writer that held the lock
        // first is the only way to a mid-apply answer.
        let observation = narrowed.and_then(|_| StateLock::observe_classified(paths));
        let settled = matches!(
            observation,
            Ok(StateObservation::Held(_) | StateObservation::Absent)
        );
        if self.quiet
            && settled
            && let Some(answer) = self.kept(paths, key, &stamps)
        {
            return Ok(answer);
        }
        let observed = read(paths)?;
        // A file's metadata never comes back, but what `state.toml` holds can: from A to B
        // between the stamps and the observation (a writer then takes the lock) and back to A
        // after the read, which then read B. Its standing is taken again under the observation,
        // and an answer is kept only where it is the one the stamps hold.
        let unmoved = stamps.state.is_none_or(|standing| {
            state_look(paths, None).is_some_and(|look| look.standing == standing)
        });
        drop(observation);
        if settled && unmoved {
            self.keep(paths, key, stamps, observed.clone());
        }
        Ok(observed)
    }

    /// Keep `answer` for `paths` and `key` under `stamps`, in place of the one kept for them, and
    /// let the oldest go past [`MAX_FOLDERS`]. In this crate's test process the bound is each
    /// folder's, and a folder whose home is gone (its test ended) lets its answers go: a law that
    /// counts the reads its own warm memo makes holds whatever other tests read meanwhile.
    fn keep(&self, paths: &Paths, key: &K, stamps: InputStamps, answer: T) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.retain(|held| held.paths != *paths || held.key != *key);
        #[cfg(not(test))]
        if held.len() >= MAX_FOLDERS {
            held.remove(0);
        }
        #[cfg(test)]
        {
            held.retain(|held| held.paths.home.exists());
            if held.iter().filter(|held| held.paths == *paths).count() >= MAX_FOLDERS
                && let Some(oldest) = held.iter().position(|held| held.paths == *paths)
            {
                held.remove(oldest);
            }
        }
        held.push(Held {
            paths: paths.clone(),
            key: key.clone(),
            stamps,
            bounds: record_bounds(),
            answer,
        });
    }
}

#[cfg(test)]
#[path = "vault_memo_tests.rs"]
mod tests;
