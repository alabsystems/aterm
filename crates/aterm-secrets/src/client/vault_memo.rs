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
use std::{os::unix::fs::MetadataExt as _, path::PathBuf, sync::Mutex};

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
static DENIAL: Memo<super::SwarmDenialSnapshot> = Memo::new(inputs);

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
    static MEMO: Memo<crate::commons::Observation> = Memo::new(observation_inputs);
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
/// megabyte or more.
///
/// # Errors
/// As [`observe_commons`](super::observe_commons).
pub(crate) fn observe_commons_quiet(paths: &Paths) -> crate::Result<crate::commons::Observation> {
    OBSERVATION.read_keyed(paths, &(), super::observe_commons)
}

/// What [`observe_commons_quiet`] and [`VaultLook::observe_commons`] keep.
static OBSERVATION: Memo<crate::commons::Observation> = Memo::keyed(observation_inputs, true);

/// [`local_recipient_denied`](super::steward::local_recipient_denied) of a fresh read, read again
/// only when one of the files it reads ([`observation_inputs`]) moved; quiet, as
/// [`local_swarm_roster_for_log_now`], so a writer holding the vault (the caller's own exclusive
/// take included) meets a fresh read. Every table session a repair round opens checks this Mac's
/// own denial under its access (`Bootstrap::access_in_observed_with`), 18 times a round on each
/// side of a loopback pair, reading the vault, its log and roster each time: 7% of a quiet round's
/// CPU (2026-11-08) and a third of its reads of the vault. That access asks it under its caller's
/// shared observation of the state lock, `lock` (the access's own, or that of the look a session's
/// admission takes: [`VaultLook`]), which the memo takes for its own as
/// [`trusted_commons_log_under`] does, and looks up what it kept through that caller's `glance`,
/// as that log's memo does next ([`fsx::Glance`]); a miss reads and stamps afresh, and keeps its
/// answer under those stamps. The swarm's checks of this Mac
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
pub(crate) fn local_recipient_denied_under(
    paths: &Paths,
    lock: &StateLock,
    glance: &fsx::Glance,
) -> crate::Result<bool> {
    static MEMO: Memo<bool> = Memo::keyed(observation_inputs, true);
    MEMO.read_keyed_under(paths, &(), lock, glance, |paths| {
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
/// rewrites about twice a minute; and kept with `heard` under `state.toml`'s metadata, the
/// judgement was read again at each of those rewrites, which move `heard` alone: the held vault
/// twice, 2.38 MB, every 25 s on a Linux arm64 node (2026-11-09).
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
    static MEMO: Memo<bool, bool> = Memo::keyed(observation_inputs, true);
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
    Memo::keyed(inputs, true);

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
    ACKNOWLEDGED.read_keyed(paths, &trust, |paths| {
        super::acknowledged_commons_log(paths, trust)
    })
}

/// What [`acknowledged_commons_log_now`], [`trusted_commons_log_under`] and
/// [`VaultLook::acknowledged_commons_log`] keep.
static ACKNOWLEDGED: Memo<super::CommonsLog, crate::commons::journal::Trust> =
    Memo::keyed(inputs, true);

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

/// [`trusted_commons_log_now`] for a caller that holds `lock`: a roster access check
/// (`commons::roster::log_observation_is_current`) and a table session's access
/// (`Bootstrap::access_in_observed_with`), each under its caller's shared observation of the
/// vault's state lock (the check's own, or a table session look's: [`VaultLook`]), ask it at
/// nearly every step of a repair round, and each ask opened, observed and closed that
/// lock again: with this Mac's own denial check under the same access
/// ([`local_recipient_denied_under`]), 7.8% of a sustained quiet round's samples over a long log
/// and 10.4% over a short one (2026-11-08). While `lock` is a shared observation
/// that still guards the lock at the vault's path ([`StateLock::observes`]), it is the observation
/// the memo would take, so what it kept is given without another. A writer's own take, or a lock
/// that guards another file, is [`trusted_commons_log_now`]'s call. It looks up what it kept
/// through the caller's check's look, `glance` ([`fsx::Glance`]); a miss reads as
/// [`trusted_commons_log_now`] does, stamping afresh.
///
/// # Errors
/// As [`trusted_commons_log`](super::trusted_commons_log).
pub(crate) fn trusted_commons_log_under(
    paths: &Paths,
    trust: crate::commons::journal::Trust,
    lock: &StateLock,
    glance: &fsx::Glance,
) -> crate::Result<Option<(crate::membership_log::Log, [u8; 32])>> {
    ACKNOWLEDGED
        .read_keyed_under(paths, &trust, lock, glance, |paths| {
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
static ROSTER: Memo<Option<crate::swarm_roster::SwarmRoster>> = Memo::keyed(inputs, true);

/// One look at this Mac's vault, for reads that stand for one moment of it: a shared observation
/// of its state lock, held while the look lives ([`super::Observing`]), and one `lstat` of each
/// file the memos it asks stamp ([`fsx::Glance`]). Each ask gives what its memo kept for the
/// files as the look first saw them, and one that misses reads and stamps afresh, as the memo's
/// own call does. While the look observes the lock no writer holds it and none can take it; a
/// file that changes all the same (a write that takes no lock, a test's own) keeps the stamps the
/// look first took of it until the look ends, and the next look stamps it afresh. A look taken
/// while a writer holds the lock, or where the lock cannot be observed, observes nothing, and each
/// ask is its memo's own call, which meets that writer as it always did. Drop it before network
/// I/O or any vault mutation: an exclusive take of the lock on this thread under it would wait for
/// good, and in a build with debug assertions panics.
///
/// A Commons peer check takes one for each stage's reads, as its kernel's model reads each view
/// at its own stage (`aterm-db`'s `peer_check`): the roster and its deny list, every record this
/// Mac holds, the acknowledged log and that log's roster, and the roster of that log with the
/// peer's evidence. Each of those reads but the records stamped the same files itself
/// ([`inputs`]), each but the deny list's (a memo that is not quiet) observing the lock first, and
/// the records were read afresh, with the configuration, the retained history and the records'
/// selection behind them: the checks took 13.0% of a sustained quiet round's on-CPU samples over
/// a long log and 10.9% over a short one, their records reads 4.3% and 4.9% (2026-11-08).
///
/// A table repair round's local batch takes one for its session's open and the checks it makes at
/// once after it (`aterm-db`'s `Replica::observe`): the open's custody
/// (`Bootstrap::revalidate_custody_in`) and access (`Bootstrap::access_observed_in_look`), the
/// currency of its peer and of its observation (`Access::is_current_in`), and its peer's roster
/// ([`Self::swarm_roster_until`]).
pub struct VaultLook<'a> {
    paths: &'a Paths,
    observation: Option<super::Observing>,
    glance: fsx::Glance,
}

impl<'a> VaultLook<'a> {
    /// A look at the vault at `paths`, observing its state lock where no writer holds it.
    #[must_use]
    pub fn new(paths: &'a Paths) -> Self {
        let observation = match StateLock::observe_classified(paths) {
            Ok(StateObservation::Held(lock)) => Some(super::Observing::new(lock, paths)),
            Ok(StateObservation::Absent | StateObservation::Busy) | Err(_) => None,
        };
        Self {
            paths,
            observation,
            glance: fsx::Glance::new(),
        }
    }

    /// The vault this looks at.
    #[must_use]
    pub const fn paths(&self) -> &'a Paths {
        self.paths
    }

    /// The shared observation of the state lock this look holds, while it still guards the lock
    /// at the vault's path ([`StateLock::observes`]), and the look at each file it takes: what a
    /// read under its caller's observation takes ([`trusted_commons_log_under`],
    /// [`local_recipient_denied_under`]). `None` where the look observes nothing.
    pub(crate) fn held(&self) -> Option<(&StateLock, &fsx::Glance)> {
        self.observation
            .as_ref()
            .map(super::Observing::lock)
            .filter(|lock| lock.observes(self.paths))
            .map(|lock| (lock, &self.glance))
    }

    /// [`observe_commons_quiet`] through this look: a table session's check of its custody at its
    /// open (`Bootstrap::revalidate_custody_in`).
    ///
    /// # Errors
    /// As [`observe_commons`](super::observe_commons).
    pub(crate) fn observe_commons(&self) -> crate::Result<crate::commons::Observation> {
        self.ask(&OBSERVATION, &(), super::observe_commons)
    }

    /// `memo`'s answer for `key` through this look: under its observation and its glance
    /// ([`Memo::read_keyed_under`]), or, where it observes nothing, the memo's own call.
    fn ask<T: Clone, K: Clone + PartialEq>(
        &self,
        memo: &Memo<T, K>,
        key: &K,
        read: impl FnOnce(&Paths) -> crate::Result<T>,
    ) -> crate::Result<T> {
        match &self.observation {
            Some(observing) => {
                memo.read_keyed_under(self.paths, key, observing.lock(), &self.glance, read)
            }
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
static RECORDS: Memo<crate::membership_log::Store> = Memo::keyed(inputs, true);

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
    let device = paths.dir.join("commons");
    files.extend([device.join("device.age"), device.join("device.claim")]);
    Some(files)
}

/// The files the members desk reads this Mac's membership from ([`desk_files`]), stamped as the
/// memo stamps its inputs, and `state.toml` by what it holds but for the telemetry of the vault's
/// rounds ([`state_standing`](super::state_standing)): a desk lane, or the node's automatic
/// admission, that read them to a settled answer ([`Local::of`]'s member or a Mac that left,
/// [`undo_offers`]'s rows) need not read them again while they stand as they were. Equal only to
/// the stamps of the same files unchanged, and the same state but for when the vault last heard
/// this Mac and the receipts of its last rounds, which no part of the desk's answer is read from:
/// a round of the vault's subscriber rewrites `state.toml` about every 25 s (Linux arm64,
/// 2026-11-09), and each rewrite read the desk again: 26 MB on that member's automatic admission,
/// 14.4 MB and 10 ms of CPU on a fixture member whose envelope is 1.2 MB, where taking these
/// stamps reads 1.3 KB in 30 µs.
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
pub(super) struct FileStamps {
    directory: Option<DirectoryStamp>,
    files: Vec<Option<Stamp>>,
}

/// [`FileStamps`] of a memo's inputs, with the membership records' selection ([`stamp_inputs`]).
#[derive(PartialEq, Eq)]
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
/// apart), and every other file by its metadata. A round of the vault's subscriber rewrites
/// `state.toml` about every 25 s for those fields alone (Linux arm64, 2026-11-09), and each
/// rewrite read every memo here again: on a fleet node, the deny list its watch asks every two
/// seconds read the held vault five times over, and its LAN presence twice.
fn stamp_memo_inputs_in(
    paths: &Paths,
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
    records: bool,
    glance: Option<&fsx::Glance>,
) -> Option<InputStamps> {
    let mut files = inputs(paths)?;
    let state_file = paths.state_file();
    let named = files.len();
    files.retain(|file| *file != state_file);
    Some(InputStamps {
        stamps: stamp_paths_in(paths, &files, glance)?,
        records: records_standing(paths, records, glance)?,
        state: if files.len() < named {
            Some(state_look(paths, glance)?.standing)
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

/// [`state_standing`](super::state_standing) at `paths`, its `lstat` `glance`'s where the caller
/// looks through one: what the file read to while it shows the stamp it shows now
/// ([`state_look`]), and a fresh read otherwise.
///
/// # Errors
/// `state.toml` cannot be read.
pub(crate) fn state_standing_in(
    paths: &Paths,
    glance: Option<&fsx::Glance>,
) -> crate::Result<Option<[u8; 32]>> {
    match state_look(paths, glance) {
        Some(look) => Ok(look.standing),
        None => Ok(super::state_standing_heard(paths)?.map(|(standing, _)| standing)),
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
    /// A memo of a read whose answer depends on `inputs` and on a key its caller names (the
    /// membership log a roster is read for); `quiet` as [`Memo::quiet`] says.
    const fn keyed(inputs: fn(&Paths) -> Option<Vec<PathBuf>>, quiet: bool) -> Self {
        Self::reading(inputs, true, quiet)
    }

    /// [`Memo::keyed`] for a read that opens no membership record, so its records' selection is
    /// not stamped: a record that arrives reads nothing again.
    const fn unrecorded(inputs: fn(&Paths) -> Option<Vec<PathBuf>>, quiet: bool) -> Self {
        Self::reading(inputs, false, quiet)
    }

    const fn reading(
        inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
        records: bool,
        quiet: bool,
    ) -> Self {
        Self {
            inputs,
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

    /// [`Memo::read_keyed`] for a caller that holds `lock`. While that is a shared observation of
    /// the state lock that still guards the lock at the vault's path ([`StateLock::observes`]), no
    /// writer holds that lock and none can write until the caller drops it: a quiet memo takes it
    /// for the one `read_keyed` takes before its stamps, and any memo gives what it kept under the
    /// stamps of `glance`, the look of the caller's check, which stamps each file once for every
    /// memo the check asks ([`fsx::Glance`]). Anything else, a miss included, is `read_keyed`'s,
    /// which stamps afresh.
    fn read_keyed_under(
        &self,
        paths: &Paths,
        key: &K,
        lock: &StateLock,
        glance: &fsx::Glance,
        read: impl FnOnce(&Paths) -> crate::Result<T>,
    ) -> crate::Result<T> {
        if lock.observes(paths)
            && let Some(stamps) = self.stamps(paths, Some(glance))
            && let Some(answer) = self.kept(paths, key, &stamps)
        {
            return Ok(answer);
        }
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
mod tests {
    use super::*;
    use crate::fsx;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// A test's wait for what must happen: a hang detector, not a latency budget.
    const HANG: Duration = Duration::from_secs(60);

    fn signed_denial(home: &std::path::Path, label: &str, denied: &str) -> Paths {
        let paths = Paths::for_home(home);
        let source = home.join("fixture.vault");
        crate::client::create(&paths, source.to_str().unwrap(), None, Some(label)).unwrap();
        let admin = crate::client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &crate::text::Audience::Everyone,
            crate::client::SWARM_ROSTER_ENTRY,
            format!("deny_nodes = [\"{denied}\"]\n").as_bytes(),
        )
        .unwrap();
        crate::client::admin_publish(&paths, &admin, &doc).unwrap();
        paths
    }

    #[test]
    fn a_typed_denial_memo_recovers_after_busy_and_failed_reads() {
        use crate::client::{self, SwarmDenialSnapshot as Snapshot, SwarmDenialUnavailable as Why};
        struct FailedReads(PathBuf);
        impl Drop for FailedReads {
            fn drop(&mut self) {
                fsx::fail_reads(&self.0, 0);
            }
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = signed_denial(root.path(), "m60", "m99");
        // Keep this fault-injected observation independent of the process-wide memo's
        // bounded eviction as other tests read their own vault folders concurrently.
        let memo = Memo::<Snapshot>::new(inputs);
        let now = || memo.read(&paths, client::swarm_denial_snapshot);
        let expected = Snapshot::Available(vec!["m99".to_owned()]);
        assert_eq!(now().unwrap(), expected);
        let failed = FailedReads(paths.cache_file());
        fsx::fail_reads(&failed.0, usize::MAX);
        assert!(
            client::swarm_denial_snapshot(&paths).is_err(),
            "the read fault is active"
        );
        assert_eq!(
            now().unwrap().into_nodes(),
            Some(vec!["m99".to_owned()]),
            "the listener projection preserves the typed memo's settled read"
        );
        drop(failed);

        let original = client::load_state(&paths).unwrap();
        let mut revoked = original.clone();
        revoked.membership = client::Stored::Revoked;
        client::save_state(&paths, &revoked).unwrap();
        assert_eq!(now().unwrap(), Snapshot::Unavailable(Why::Revoked));
        assert_eq!(now().unwrap().into_nodes(), None);
        client::save_state(&paths, &original).unwrap();
        assert_eq!(now().unwrap(), expected);

        // A state rewritten with what it held but the time it asked the vault (which no deny
        // list is read from) moves the memo's stamps, as a round that heard nothing new does not.
        let mut asked = original.clone();
        let writer = StateLock::take(&paths).unwrap();
        asked.requested += 1;
        client::save_state(&paths, &asked).unwrap();
        assert_eq!(now().unwrap(), Snapshot::Unavailable(Why::WriterBusy));
        drop(writer);
        assert_eq!(
            now().unwrap(),
            expected,
            "a busy observation is not kept after its writer leaves"
        );

        let failed = FailedReads(paths.cache_file());
        fsx::fail_reads(&failed.0, usize::MAX);
        asked.requested += 1;
        client::save_state(&paths, &asked).unwrap();
        assert!(now().is_err());
        drop(failed);
        assert_eq!(
            now().unwrap(),
            expected,
            "an I/O failure does not poison the memo until another write"
        );
    }

    #[test]
    fn typed_denial_memo_keeps_vaults_separate_and_refuses_an_unapplied_source() {
        use crate::client::{self, SwarmDenialSnapshot as Snapshot};
        let root = aterm_tempfile::tempdir().unwrap();
        let one = signed_denial(&root.path().join("one"), "m60", "m99");
        let two = signed_denial(&root.path().join("two"), "m61", "m98");
        let first = Snapshot::Available(vec!["m99".to_owned()]);
        let second = Snapshot::Available(vec!["m98".to_owned()]);
        assert_eq!(swarm_denial_snapshot_now(&one).unwrap(), first);
        assert_eq!(swarm_denial_snapshot_now(&two).unwrap(), second);
        let original = client::config(&one).unwrap().unwrap();
        client::save_config(&one, &client::config(&two).unwrap().unwrap()).unwrap();
        assert!(
            !matches!(swarm_denial_snapshot_now(&one), Ok(Snapshot::Available(_))),
            "a replacement source cannot inherit the old source's verified list"
        );
        assert!(!matches!(swarm_denied_nodes_now(&one), Ok(Some(_))));
        assert_eq!(swarm_denial_snapshot_now(&two).unwrap(), second);
        client::save_config(&one, &original).unwrap();
        assert_eq!(swarm_denial_snapshot_now(&one).unwrap(), first);
    }

    /// A reader standing in for the vault's: the lines of the vault folder's `records`.
    fn lines(paths: &Paths, reads: &AtomicUsize) -> crate::Result<Option<Vec<String>>> {
        reads.fetch_add(1, Ordering::SeqCst);
        let text = std::fs::read_to_string(paths.records_data_file()?)?;
        Ok(Some(text.lines().map(str::to_owned).collect()))
    }

    fn vault(home: &std::path::Path) -> Paths {
        let paths = Paths::for_home(home);
        fsx::ensure_private_dir(&paths.dir).unwrap();
        fsx::write_atomic(&paths.records_file(), b"m60\n", 0o600).unwrap();
        paths
    }

    /// A memo gives what it kept only under the record bounds it was read under
    /// (`record_bounds`): under a test's own budgets or byte bound (`RecordBudgets::within`,
    /// `RecordBudgets::within_bytes`) the same files recover to other records, so a read under
    /// other bounds reads again.
    ///
    /// NEGATIVE CONTROLS (2026-11-09): with `Memo::kept` not matching the bounds, the read under
    /// smaller budgets is given the answer kept under the production ones (`read again under
    /// other budgets`); with `record_bounds` leaving out the byte bound, so is the read under a
    /// smaller one (`read again under another byte bound`).
    #[test]
    fn a_memo_reads_again_under_other_record_budgets() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = vault(root.path());
        let memo = Memo::new(inputs);
        let reads = AtomicUsize::new(0);
        let read = || memo.read(&paths, |p| lines(p, &reads)).unwrap();
        assert_eq!(read(), Some(vec!["m60".to_owned()]));
        assert_eq!(read(), Some(vec!["m60".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 1, "read once");
        let small = crate::membership_log::RecordBudgets {
            weighed: 2,
            keyed: 2,
            held: 4,
        };
        small.within(|| {
            read();
            read();
        });
        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "read again under other budgets"
        );
        read();
        assert_eq!(
            reads.load(Ordering::SeqCst),
            3,
            "and again under the production ones"
        );
        crate::membership_log::RecordBudgets::within_bytes(1 << 20, || {
            read();
            read();
        });
        assert_eq!(
            reads.load(Ordering::SeqCst),
            4,
            "read again under another byte bound"
        );
    }

    /// An exclusive take of the state lock on a thread under its own look ([`VaultLook`]) panics
    /// at once in a build with debug assertions rather than wait on the look's observation for
    /// good, which flock(2) would.
    ///
    /// NEGATIVE CONTROL (2026-11-08): with `Observing::new` registering no lock file in
    /// `OBSERVING`, the take waits on the look's observation and the hang detector fails "the
    /// take refuses at once".
    #[test]
    fn a_take_of_the_state_lock_under_a_look_panics_rather_than_wait_on_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = vault(root.path());
        drop(StateLock::take(&paths).unwrap());
        let (done, finished) = std::sync::mpsc::channel();
        let looking = paths.clone();
        std::thread::spawn(move || {
            let taken = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let look = VaultLook::new(&looking);
                let observed = look.observation.is_some();
                let _held = StateLock::take(&looking);
                observed
            }));
            let _ = done.send(taken.map_err(|panic| panic.downcast_ref::<String>().cloned()));
        });
        let outcome = finished
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the take refuses at once rather than wait on its own look");
        let message = outcome
            .expect_err("the take under a look that observes the lock panics")
            .expect("a panic message");
        assert!(
            message.contains("under this thread's own observation"),
            "{message}"
        );
    }

    #[test]
    fn a_memo_observes_current_data_and_refuses_a_missing_layout_file() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = vault(root.path());
        let data = crate::records_storage::write_path(&paths.records_file(), true)
            .unwrap()
            .to_path_buf();
        let memo = Memo::new(inputs);
        let reads = AtomicUsize::new(0);
        assert_eq!(
            memo.read(&paths, |p| lines(p, &reads)).unwrap(),
            Some(vec!["m60".to_owned()])
        );
        let directory = fsx::stamp(&paths.records_file());
        std::fs::write(&data, b"m61-longer\n").unwrap();
        assert_eq!(fsx::stamp(&paths.records_file()), directory);
        assert_eq!(
            memo.read(&paths, |p| lines(p, &reads)).unwrap(),
            Some(vec!["m61-longer".to_owned()])
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        std::fs::rename(&data, paths.records_file().join("saved-current")).unwrap();
        assert!(memo.read(&paths, |p| lines(p, &reads)).is_err());
    }

    /// A pinned layout standing as it was is read once, and read again by a reader that holds
    /// fewer records than the layout pins, which the selection refuses (`check_capacity`).
    /// NEGATIVE CONTROL: with the capacity left out of a layout's judgement
    /// (`records_storage::Judged`), the memo answers the records it read under the larger one.
    #[test]
    fn a_pinned_layout_is_read_again_under_a_smaller_record_capacity() {
        fn budget(held: usize) -> crate::membership_log::RecordBudgets {
            crate::membership_log::RecordBudgets {
                weighed: 16,
                keyed: 128,
                held,
            }
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = vault(root.path());
        let memo = Memo::new(inputs);
        let reads = AtomicUsize::new(0);
        let expected = Some(vec!["m60".to_owned()]);
        budget(64).within(|| {
            drop(crate::records_storage::write_path(&paths.records_file(), true).unwrap());
            assert_eq!(memo.read(&paths, |p| lines(p, &reads)).unwrap(), expected);
            assert_eq!(memo.read(&paths, |p| lines(p, &reads)).unwrap(), expected);
        });
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "a standing layout is read once"
        );
        assert!(
            budget(32)
                .within(|| memo.read(&paths, |p| lines(p, &reads)))
                .is_err(),
            "a reader holding fewer records than the layout pins reads it again and refuses it"
        );
    }

    /// Records the selection refuses stamp nothing, as the selection reads nothing from them: a
    /// pinned layout whose header is not one, and a records entry left a directory with no pin (an
    /// unfinished transition). So no read keeps an answer by them, and nothing takes their stamps
    /// for settled (`client::MembersAsked::still_asking`). Records it reads through are stamped,
    /// a plain records file with a preparation's intent beside it too. NEGATIVE CONTROLS: a pinned
    /// layout stood without its judgement (`records_storage::Judgements::layout`) stamps the header
    /// that is not one; every shape stood as a plain records file stamps the directory with no pin;
    /// with a plain file only where no intent stands beside it (eb0e73e0f1), a file with an intent
    /// beside it is unjudged.
    #[test]
    fn records_the_selection_refuses_are_unjudged() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = vault(root.path());
        assert!(
            stamp_inputs(&paths, inputs).is_some(),
            "a records file is judged"
        );
        let intent = crate::records_storage::intent_path(&paths.records_file()).unwrap();
        fsx::write_atomic(&intent, b"a preparation cut short\n", 0o600).unwrap();
        assert!(
            crate::records_storage::data_path(&paths.records_file()).is_ok(),
            "the selection reads a records file through an intent beside it"
        );
        assert!(
            stamp_inputs(&paths, inputs).is_some(),
            "a records file with an intent beside it is judged"
        );
        std::fs::remove_file(&intent).unwrap();
        drop(crate::records_storage::write_path(&paths.records_file(), true).unwrap());
        assert!(
            stamp_inputs(&paths, inputs).is_some(),
            "a pinned layout is judged"
        );
        fsx::write_atomic(
            &paths.records_file().join("layout"),
            b"not a layout\n",
            0o600,
        )
        .unwrap();
        assert!(
            stamp_inputs(&paths, inputs).is_none(),
            "a layout whose header is not one is unjudged"
        );
        let unfinished = Paths::for_home(&root.path().join("unfinished"));
        fsx::ensure_private_dir(&unfinished.dir).unwrap();
        std::fs::create_dir(unfinished.records_file()).unwrap();
        assert!(
            stamp_inputs(&unfinished, inputs).is_none(),
            "an entry left a directory with no pin is unjudged"
        );
    }

    /// A signed vault's warm deny list has the same directory refusal as a fresh read, and
    /// recovers when the original directory returns. Negative control on 02da6d5d7c: the fresh
    /// read refused the symlink but the memo returned its earlier list, failing this test.
    #[test]
    fn a_warm_deny_list_refuses_a_symlinked_vault_directory() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        let source = root.path().join("roster.vault");
        crate::client::create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
        let admin = crate::client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &crate::text::Audience::Everyone,
            crate::client::SWARM_ROSTER_ENTRY,
            b"deny_nodes = [\"m99\"]\n",
        )
        .unwrap();
        crate::client::admin_publish(&paths, &admin, &doc).unwrap();
        crate::client::sync(&paths, None).unwrap();
        let expected = Some(vec!["m99".to_owned()]);
        assert_eq!(crate::client::swarm_denied_nodes(&paths).unwrap(), expected);
        assert_eq!(
            crate::client::swarm_denied_nodes_now(&paths).unwrap(),
            expected
        );

        // All leaf files remain the same inodes with the same bytes and metadata.
        // Only the directory through which the read reaches them has changed.
        let held = paths.dir.with_extension("held");
        std::fs::rename(&paths.dir, &held).unwrap();
        std::os::unix::fs::symlink(&held, &paths.dir).unwrap();
        assert!(crate::client::swarm_denied_nodes(&paths).is_err());
        assert!(
            crate::client::swarm_denied_nodes_now(&paths).is_err(),
            "a cached membership answer must preserve the fresh reader's refusal"
        );
        std::fs::rename(&paths.dir, paths.dir.with_extension("held-link")).unwrap();
        std::fs::rename(&held, &paths.dir).unwrap();
        assert_eq!(
            crate::client::swarm_denied_nodes_now(&paths).unwrap(),
            expected
        );
    }

    /// A temporary failure to read the signed envelope is not a settled answer. Clearing
    /// that failure changes no input metadata, so the same memo must read the deny list again.
    #[test]
    fn a_failed_cache_read_does_not_poison_the_memo_until_a_vault_write() {
        struct FailedReads(PathBuf);
        impl Drop for FailedReads {
            fn drop(&mut self) {
                fsx::fail_reads(&self.0, 0);
            }
        }

        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        let source = root.path().join("roster.vault");
        crate::client::create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
        let admin = crate::client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &crate::text::Audience::Everyone,
            crate::client::SWARM_ROSTER_ENTRY,
            b"deny_nodes = [\"m99\"]\n",
        )
        .unwrap();
        crate::client::admin_publish(&paths, &admin, &doc).unwrap();
        crate::client::sync(&paths, None).unwrap();
        let expected = Some(vec!["m99".to_owned()]);
        assert_eq!(crate::client::swarm_denied_nodes(&paths).unwrap(), expected);

        let before = stamp_inputs(&paths, inputs).unwrap();
        let memo = Memo::<Option<Vec<String>>>::new(inputs);
        let failure = FailedReads(paths.cache_file());
        // Keep failing until explicitly cleared, independently of how many reads the
        // authorization path makes. The guard also clears the fault on an assertion panic.
        fsx::fail_reads(&failure.0, usize::MAX);
        let failed = memo.read(&paths, crate::client::swarm_denied_nodes);
        drop(failure);
        assert!(
            stamp_inputs(&paths, inputs).unwrap() == before,
            "clearing a read fault must not change the memo's input metadata"
        );
        assert_eq!(crate::client::swarm_denied_nodes(&paths).unwrap(), expected);
        assert_eq!(
            memo.read(&paths, crate::client::swarm_denied_nodes)
                .unwrap(),
            expected,
            "a transient read failure must not remain cached after the read recovers"
        );
        assert!(
            failed.is_err(),
            "the unread envelope must remain an I/O error"
        );
    }

    /// The roster memo must also recover after a cache read fails, without waiting for a
    /// vault write to change its input stamps.
    #[test]
    fn a_failed_cache_read_does_not_poison_the_roster_memo_until_a_vault_write() {
        struct FailedReads(PathBuf);
        impl Drop for FailedReads {
            fn drop(&mut self) {
                fsx::fail_reads(&self.0, 0);
            }
        }

        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        let source = root.path().join("roster.vault");
        crate::client::create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
        let admin = crate::client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &crate::text::Audience::Everyone,
            crate::client::SWARM_ROSTER_ENTRY,
            b"deny_nodes = [\"m99\"]\n",
        )
        .unwrap();
        crate::client::admin_publish(&paths, &admin, &doc).unwrap();
        crate::client::sync(&paths, None).unwrap();
        let expected = crate::client::swarm_roster_until(
            &paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert_eq!(expected.as_ref().unwrap().deny_nodes, ["m99"]);

        let before = stamp_inputs(&paths, inputs).unwrap();
        let memo = Memo::<Option<SwarmRoster>>::new(inputs);
        let failure = FailedReads(paths.cache_file());
        // The read fault lasts until explicitly cleared; no implementation read count
        // decides when the actual signed roster becomes available again.
        fsx::fail_reads(&failure.0, usize::MAX);
        let failed = memo.read(&paths, |paths| {
            crate::client::swarm_roster_until(
                paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            )
        });
        drop(failure);
        assert!(
            stamp_inputs(&paths, inputs).unwrap() == before,
            "clearing a read fault must not change the memo's input metadata"
        );
        assert_eq!(
            crate::client::swarm_roster_until(
                &paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            memo.read(&paths, |paths| crate::client::swarm_roster_until(
                paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
            ))
            .unwrap(),
            expected,
            "a transient read failure must not remain cached after the roster recovers"
        );
        assert!(
            failed.is_err(),
            "the unread roster envelope must remain an I/O error"
        );
    }

    /// Two reads of unchanged inputs read once; a rewritten input, and another input appearing
    /// beside it, are each read again at the next call, and the answer is the new list. Files in
    /// the vault folder the read never opens (the vault server's `served/`, `commons/`, a job's
    /// log), which change every few seconds on a live Mac, read nothing again.
    ///
    /// NEGATIVE CONTROLS (2026-11-01): with the stamp comparison dropped so the memo always reads
    /// (`held.stamps == stamps` replaced by `false` in `kept`), the second unchanged read reads
    /// again (`left: 2, right: 1`); with it dropped so the memo never reads again (`true`), the
    /// rewritten records' answer is the old list (`left: Some(["m60"]), right: Some(["m60",
    /// "m61"])`); with every entry directly in the vault folder stamped (the shape before), a
    /// write to `served/` reads again (`left: 4, right: 3`).
    #[test]
    fn a_deny_list_is_read_again_only_when_one_of_its_inputs_changed() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        let memo = Memo::<Option<Vec<String>>>::new(inputs);
        let reads = AtomicUsize::new(0);
        let now = || memo.read(&paths, |p| lines(p, &reads)).unwrap();

        assert_eq!(now(), Some(vec!["m60".to_owned()]));
        assert_eq!(now(), Some(vec!["m60".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        fsx::write_atomic(&paths.records_file(), b"m60\nm61\n", 0o600).unwrap();
        assert_eq!(now(), Some(vec!["m60".to_owned(), "m61".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 2);

        fsx::write_atomic(&paths.identity_file(), b"new input", 0o600).unwrap();
        assert_eq!(now(), Some(vec!["m60".to_owned(), "m61".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 3);

        for dir in [paths.served(), paths.dir.join("commons")] {
            fsx::ensure_private_dir(&dir).unwrap();
            fsx::write_atomic(&dir.join("heard"), b"heard", 0o600).unwrap();
        }
        fsx::write_atomic(
            &paths.dir.join("systems.alab.aterm-secrets.log"),
            b"line",
            0o600,
        )
        .unwrap();
        assert_eq!(now(), Some(vec!["m60".to_owned(), "m61".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 3);
    }

    /// What `state.toml` holds can come back where its metadata cannot, and a memo stamps it by
    /// what it holds but `heard` and the rounds' receipts (`stamp_memo_inputs_in`): a state that
    /// moved from A to B between a read's stamps and its read, which then read B, and back to A
    /// after it, would have B's answer given under A's stamps. The answer of a read the state
    /// moved under is not kept, so the next call under A reads A.
    ///
    /// NEGATIVE CONTROL (2026-11-09): with every settled answer kept whatever the state's
    /// standing after the read (`unmoved` always true), the call under A is given B's answer
    /// (`left: 4, right: 3`).
    #[test]
    fn an_answer_read_after_the_state_moved_is_not_kept_under_its_old_standing() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        let state = |seq: u64| {
            let text = format!("membership = \"in\"\nseq = {seq}\n");
            fsx::write_atomic(&paths.state_file(), text.as_bytes(), 0o600).unwrap();
        };
        let seq = |paths: &Paths| crate::client::load_state(paths).map(|state| state.seq);
        state(3);
        let memo = Memo::<u64>::new(inputs);
        let moved = memo.read(&paths, |paths| {
            state(4);
            seq(paths)
        });
        assert_eq!(moved.unwrap(), 4);
        state(3);
        assert_eq!(memo.read(&paths, seq).unwrap(), 3);
    }

    /// Callers that miss together after a change wait for one read and take its answer: while
    /// the first caller's read is held open, seven more ask, and once it returns every one of
    /// them has the list and the folder was read once.
    ///
    /// NEGATIVE CONTROLS (2026-11-01): with the second look in `held` after `refresh` dropped,
    /// each waiting caller reads in turn once the first returns (`left: 8, right: 1`); with
    /// `refresh` not taken at all, the first of the seven reads beside the first caller and the
    /// rest take its answer without queueing, so this fails at its one-minute hang detector
    /// ("the other callers never queued").
    #[test]
    fn callers_that_miss_together_wait_for_one_read() {
        const CALLERS: usize = 8;
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        let memo = Memo::<Option<Vec<String>>>::new(inputs);
        let reads = AtomicUsize::new(0);
        let answers = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                memo.read(&paths, |p| {
                    reads.fetch_add(1, Ordering::SeqCst);
                    let started = Instant::now();
                    // Every caller, this one included, has missed and come to `refresh`.
                    while memo.waiting.load(Ordering::SeqCst) < CALLERS {
                        assert!(started.elapsed() < HANG, "the other callers never queued");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    let text = std::fs::read_to_string(p.records_file()).unwrap();
                    Ok(Some(text.lines().map(str::to_owned).collect()))
                })
            });
            let started = Instant::now();
            while reads.load(Ordering::SeqCst) == 0 {
                assert!(started.elapsed() < HANG, "the first caller never read");
                std::thread::sleep(Duration::from_millis(1));
            }
            let others: Vec<_> = (1..CALLERS)
                .map(|_| scope.spawn(|| memo.read(&paths, |p| lines(p, &reads))))
                .collect();
            std::iter::once(first)
                .chain(others)
                .map(|caller| caller.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(
            answers
                .iter()
                .all(|answer| *answer == Some(vec!["m60".to_owned()]))
        );
    }

    /// A read made while a writer holds the vault's state lock (mid-apply), and a failed read,
    /// are never kept: the next call on the same folder reads again, and the next settled answer
    /// is kept.
    ///
    /// NEGATIVE CONTROL (2026-11-01): with every `Ok` answer kept (`settled` always true), the
    /// call after the mid-apply one is answered from the memo and never reads (`assertion failed:
    /// failed.is_err()`).
    #[test]
    fn a_mid_apply_answer_is_read_again_at_the_next_call() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        let memo = Memo::<Option<Vec<String>>>::new(inputs);
        let reads = AtomicUsize::new(0);

        let writer = StateLock::take(&paths).unwrap();
        let mid_apply = memo.read(&paths, |_| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        });
        assert_eq!(mid_apply.unwrap(), None);
        drop(writer);
        let failed = memo.read(&paths, |_| {
            reads.fetch_add(1, Ordering::SeqCst);
            Err(crate::Error::new("torn read"))
        });
        assert!(failed.is_err());
        assert_eq!(reads.load(Ordering::SeqCst), 2);

        let now = || memo.read(&paths, |p| lines(p, &reads)).unwrap();
        assert_eq!(now(), Some(vec!["m60".to_owned()]));
        assert_eq!(now(), Some(vec!["m60".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 3);
    }

    /// An `Ok(None)` read while no writer held the state lock is what the inputs say (this Mac
    /// removed in the log, an envelope that does not verify), and is kept until they change:
    /// with no lock file yet, and again once one appears and nobody holds it.
    ///
    /// NEGATIVE CONTROL (2026-11-01): with only `Ok(Some(_))` kept (the shape before), every call
    /// on that Mac reads again (`left: 2, right: 1`).
    /// A lock an older build left wide is narrowed by the memo before it observes it, so the
    /// first read after an upgrade settles and is kept: a reader that does not narrow it itself
    /// reads once.
    ///
    /// NEGATIVE CONTROL (2026-11-01): with the memo's `StateLock::narrow_owned` call removed (the
    /// observation alone, as before), the wide lock fails the observation, no answer settles and
    /// every call reads again (`left: 2, right: 1`, `a read past a wide lock is kept`).
    #[test]
    fn a_read_past_a_wide_lock_narrows_it_and_is_kept() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        drop(StateLock::take(&paths).unwrap());
        std::fs::set_permissions(paths.lock_file(), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let memo = Memo::<Option<Vec<String>>>::new(inputs);
        let reads = AtomicUsize::new(0);
        let now = || memo.read(&paths, |p| lines(p, &reads)).unwrap();

        assert_eq!(now(), Some(vec!["m60".to_owned()]));
        assert_eq!(now(), Some(vec!["m60".to_owned()]));
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "a read past a wide lock is kept"
        );
        assert_eq!(
            std::fs::metadata(paths.lock_file()).unwrap().mode() & 0o7777,
            0o600
        );
    }

    #[test]
    fn a_settled_answer_that_is_not_a_list_is_kept_until_the_inputs_change() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        let memo = Memo::<Option<Vec<String>>>::new(inputs);
        let reads = AtomicUsize::new(0);
        let none = || {
            memo.read(&paths, |_| {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            })
            .unwrap()
        };

        assert_eq!(none(), None);
        assert_eq!(none(), None);
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        drop(StateLock::take(&paths).unwrap());
        assert_eq!(none(), None);
        assert_eq!(none(), None);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    /// A quiet memo asked under its caller's shared observation of the state lock gives what it
    /// kept without observing the lock again, and reads a changed file as `read_keyed` does.
    ///
    /// NEGATIVE CONTROL (2026-11-08): with `read_keyed_under` making `read_keyed`'s call (the
    /// shape before), each kept answer observes the lock again (`left: 2, right: 0`).
    #[test]
    fn a_quiet_memo_under_its_callers_observation_takes_none_of_its_own() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        drop(StateLock::take(&paths).unwrap());
        let memo = Memo::<Option<Vec<String>>>::keyed(inputs, true);
        let reads = AtomicUsize::new(0);
        assert_eq!(
            memo.read_keyed(&paths, &(), |p| lines(p, &reads)).unwrap(),
            Some(vec!["m60".to_owned()])
        );
        let lock = StateLock::observe(&paths).unwrap().expect("no writer");
        let under = || {
            memo.read_keyed_under(&paths, &(), &lock, &fsx::Glance::new(), |p| {
                lines(p, &reads)
            })
        };

        let observations = super::super::lock_observations_of(&paths);
        assert_eq!(under().unwrap(), Some(vec!["m60".to_owned()]));
        assert_eq!(under().unwrap(), Some(vec!["m60".to_owned()]));
        assert_eq!(super::super::lock_observations_of(&paths) - observations, 0);
        assert_eq!(reads.load(Ordering::SeqCst), 1, "read once");

        fsx::write_atomic(&paths.records_file(), b"m61-longer\n", 0o600).unwrap();
        assert_eq!(under().unwrap(), Some(vec!["m61-longer".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 2, "a changed file is read");
    }

    /// Only a shared observation that still guards the lock at the vault's path stands in for the
    /// memo's own: under a writer's own exclusive take, and under an observation of a lock file
    /// another was renamed over while a writer holds the new one, the memo reads afresh, as
    /// `read_keyed` meets that writer, and keeps nothing.
    ///
    /// NEGATIVE CONTROLS (2026-11-08): with `StateLock::observes` taking any lock (no `shared`),
    /// the writer's own check is given the answer kept from before its take (`left: 1, right:
    /// 3`, `a writer's own take reads afresh`); with it taking a shared one without `guards`, the
    /// observation of the lock renamed away is given the answer kept under the new lock while its
    /// writer holds it (`left: 4, right: 5`, `a lock renamed away guards nothing`).
    #[test]
    fn a_writers_take_or_a_lock_renamed_away_is_no_observation_for_a_memo() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        drop(StateLock::take(&paths).unwrap());
        let memo = Memo::<Option<Vec<String>>>::keyed(inputs, true);
        let reads = AtomicUsize::new(0);
        let kept = || memo.read_keyed(&paths, &(), |p| lines(p, &reads)).unwrap();
        assert_eq!(kept(), Some(vec!["m60".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        {
            let writer = StateLock::take(&paths).unwrap();
            for _ in 0..2 {
                assert_eq!(
                    memo.read_keyed_under(&paths, &(), &writer, &fsx::Glance::new(), |p| lines(
                        p, &reads
                    ))
                    .unwrap(),
                    Some(vec!["m60".to_owned()])
                );
            }
            assert_eq!(
                reads.load(Ordering::SeqCst),
                3,
                "a writer's own take reads afresh"
            );
        }

        let renamed_away = StateLock::observe(&paths).unwrap().expect("no writer");
        fsx::write_atomic(&paths.lock_file(), b"", 0o600).unwrap();
        assert_eq!(kept(), Some(vec!["m60".to_owned()]));
        assert_eq!(kept(), Some(vec!["m60".to_owned()]));
        assert_eq!(reads.load(Ordering::SeqCst), 4, "kept under the new lock");
        let _writer = StateLock::take(&paths).unwrap();
        assert_eq!(
            memo.read_keyed_under(&paths, &(), &renamed_away, &fsx::Glance::new(), |p| lines(
                p, &reads
            ))
            .unwrap(),
            Some(vec!["m60".to_owned()])
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            5,
            "a lock renamed away guards nothing"
        );
    }

    /// An observation takes the descriptor an earlier one left idle instead of opening the lock
    /// file again, and holds no lock once it drops: a writer takes the lock at once.
    ///
    /// NEGATIVE CONTROLS (2026-11-08): with `StateLock::open_observed` opening every time (the
    /// shape before), each observation opens the lock (`left: 2, right: 0`, `observed on the
    /// idle descriptor`); with the drop putting the descriptor back before its unlock, a writer
    /// finds the lock held (`a writer takes the lock an observation left`).
    #[test]
    fn an_observation_takes_the_descriptor_an_earlier_one_left_unlocked() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        drop(StateLock::take(&paths).unwrap());
        drop(StateLock::observe(&paths).unwrap().expect("no writer"));
        let opens = super::super::lock_opens_of(&paths);
        for _ in 0..2 {
            let lock = StateLock::observe(&paths).unwrap().expect("no writer");
            assert!(lock.observes(&paths));
        }
        assert_eq!(
            super::super::lock_opens_of(&paths) - opens,
            0,
            "observed on the idle descriptor"
        );
        assert!(
            StateLock::try_take(&paths).unwrap().is_some(),
            "a writer takes the lock an observation left"
        );
    }

    /// An idle descriptor is judged as a file just opened is: a lock given a mode others can read
    /// or a second link is refused, a lock file another was renamed over is observed by its new
    /// file, and a vault folder made a symlink to itself moved aside is refused, each as a fresh
    /// open finds it.
    ///
    /// NEGATIVE CONTROLS (2026-11-08): with `open_observed` taking an idle descriptor without
    /// comparing it with the file at the lock's path, the observation after a lock file was
    /// renamed over judges the old one and refuses it (`has 0 hard links`); without judging the
    /// folder, the symlinked folder is observed (`a symlinked folder is refused`).
    #[test]
    fn an_idle_descriptor_is_judged_as_a_file_just_opened() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = vault(home.path());
        drop(StateLock::take(&paths).unwrap());
        let idle = || drop(StateLock::observe(&paths).unwrap().expect("no writer"));

        idle();
        std::fs::set_permissions(paths.lock_file(), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(
            StateLock::observe(&paths).is_err(),
            "a wide lock is refused"
        );
        std::fs::set_permissions(paths.lock_file(), std::fs::Permissions::from_mode(0o600))
            .unwrap();

        idle();
        let second = home.path().join("second-link");
        std::fs::hard_link(paths.lock_file(), &second).unwrap();
        assert!(
            StateLock::observe(&paths).is_err(),
            "a linked lock is refused"
        );
        std::fs::remove_file(&second).unwrap();

        idle();
        fsx::write_atomic(&paths.lock_file(), b"", 0o600).unwrap();
        let renamed_over = StateLock::observe(&paths).unwrap();
        assert!(
            renamed_over.is_some_and(|lock| lock.observes(&paths)),
            "a lock file renamed over is observed by its new file"
        );

        idle();
        let aside = home.path().join("vault-aside");
        std::fs::rename(&paths.dir, &aside).unwrap();
        std::os::unix::fs::symlink(&aside, &paths.dir).unwrap();
        assert!(
            StateLock::observe(&paths).is_err(),
            "a symlinked folder is refused"
        );
        std::fs::remove_file(&paths.dir).unwrap();
        std::fs::rename(&aside, &paths.dir).unwrap();

        idle();
        std::fs::rename(&paths.dir, &aside).unwrap();
        assert!(
            StateLock::observe(&paths).unwrap().is_none(),
            "a missing folder is no observation"
        );
    }
}
