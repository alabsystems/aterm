// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vault reads for a reader that asks per connection, per round or every few seconds, whatever
//! it found: the swarm deny list (a fleet listener, which checks each proved dialer against it,
//! its node job's watch, which checks every live connection each tick, and the node's Commons
//! peer checks and census) and the Commons observation (the Commons runtime's presence poll,
//! the members desk). [`swarm_denied_nodes`](super::swarm_denied_nodes) and
//! [`observe_commons`](super::observe_commons) read, decrypt and evaluate the vault and its
//! membership log at every call, tens of milliseconds of CPU on a busy Mac, so each keeps its
//! last settled answer and reads again when one of the files it reads, or the vault directory's
//! identity or access metadata, changes.

use super::{Paths, StateLock, StateObservation};
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
    static MEMO: Memo<super::SwarmDenialSnapshot> = Memo::new(inputs);
    MEMO.read(paths, super::swarm_denial_snapshot)
}

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
    static MEMO: Memo<crate::commons::Observation> = Memo::keyed(observation_inputs, true);
    MEMO.read_keyed(paths, &(), super::observe_commons)
}

/// [`local_recipient_denied`](super::steward::local_recipient_denied) of a fresh read, read again
/// only when one of the files it reads ([`observation_inputs`]) moved; quiet, as
/// [`local_swarm_roster_for_log_now`], so a writer holding the vault (the caller's own exclusive
/// take included) meets a fresh read. Every table session a repair round opens checks this Mac's
/// own denial under its access (`Bootstrap::access_in_observed`), 18 times a round on each side of
/// a loopback pair, reading the vault, its log and roster each time: 7% of a quiet round's CPU
/// (2026-11-08) and a third of its reads of the vault. That access asks it under its own
/// observation of the state lock, `lock`, which the memo takes for its own as
/// [`trusted_commons_log_under`] does. The swarm's checks of this Mac
/// ([`local_recipient_denied_in_roster`](super::local_recipient_denied_in_roster)) read afresh, so
/// one that cannot read a file it was settled on takes this Mac as denied. This one, like every
/// memo here, gives the answer it kept through a read fault on files that have not moved: the
/// judgement those files gave, so a Mac keeps the table access it held, as a server keeps the
/// role it holds while its device cannot be read
/// (`a_device_it_cannot_read_gives_a_mac_still_in_no_new_authority`).
///
/// # Errors
/// As [`local_recipient_denied`](super::steward::local_recipient_denied).
pub(crate) fn local_recipient_denied_under(paths: &Paths, lock: &StateLock) -> crate::Result<bool> {
    static MEMO: Memo<bool> = Memo::keyed(observation_inputs, true);
    MEMO.read_keyed_under(paths, &(), lock, |paths| {
        super::steward::local_recipient_denied(&super::Reads::new(paths))
    })
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
    type Key = ([u8; 32], crate::membership_log::RecordBudgets);
    static MEMO: Memo<Option<crate::swarm_roster::SwarmRoster>, Key> = Memo::keyed(inputs, true);
    let key = (super::log_entries::records_heads_digest(log), log.budgets());
    MEMO.read_keyed(paths, &key, |paths| {
        super::local_swarm_roster_for_log(paths, log)
    })
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

/// What [`acknowledged_commons_log_now`] and [`trusted_commons_log_under`] keep.
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
/// (`Bootstrap::access_in_observed`), each under its own observation of the vault's state lock,
/// ask it at nearly every step of a repair round, and each ask opened, observed and closed that
/// lock again: with this Mac's own denial check under the same access
/// ([`local_recipient_denied_under`]), 7.8% of a sustained quiet round's samples over a long log
/// and 10.4% over a short one (2026-11-08). While `lock` is a shared observation
/// that still guards the lock at the vault's path ([`StateLock::observes`]), it is the observation
/// the memo would take, so what it kept is given without another. A writer's own take, or a lock
/// that guards another file, is [`trusted_commons_log_now`]'s call.
///
/// # Errors
/// As [`trusted_commons_log`](super::trusted_commons_log).
pub(crate) fn trusted_commons_log_under(
    paths: &Paths,
    trust: crate::commons::journal::Trust,
    lock: &StateLock,
) -> crate::Result<Option<(crate::membership_log::Log, [u8; 32])>> {
    ACKNOWLEDGED
        .read_keyed_under(paths, &trust, lock, |paths| {
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
    static MEMO: Memo<Option<crate::swarm_roster::SwarmRoster>> = Memo::keyed(inputs, true);
    MEMO.read_keyed(paths, &(), |paths| {
        super::swarm_roster_until(paths, deadline)
    })
}

/// The files [`swarm_denied_nodes`](super::swarm_denied_nodes) and
/// [`swarm_roster_until`](super::swarm_roster_until) take their answer from: the configured source
/// (`source.toml`), the state lock the deny list observes (`.lock`, whose presence and mode decide
/// that read), the applied state (`state.toml`), this Mac's identity, the held envelope (`vault`),
/// the membership records entry, the layout pin that selects its data (`records.layout`) and that
/// data, and the membership history this Mac acknowledged ([`Paths::acknowledged_history_files`]).
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
        paths.records_file(),
        crate::records_storage::pin_path(&paths.records_file()).ok()?,
        paths.records_data_file().ok()?,
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
/// memo stamps its inputs: a desk lane that read them to a settled answer ([`Local::of`]'s member
/// or a Mac that left, [`undo_offers`]'s rows) need not read them again while they stand as they
/// were. Equal only to the stamps of the same files unchanged.
///
/// [`Local::of`]: crate::members::desk::Local::of
/// [`undo_offers`]: super::undo_offers
#[derive(PartialEq, Eq)]
pub struct DeskInputs(InputStamps);

/// The stamps of the files the members desk reads this Mac's membership from, now; `None` where a
/// file's metadata or the vault folder cannot be read safely (read again).
#[must_use]
pub fn desk_inputs(paths: &Paths) -> Option<DeskInputs> {
    stamp_inputs(paths, desk_files).map(DeskInputs)
}

/// The files [`Local::of`](crate::members::desk::Local::of) and
/// [`undo_offers`](super::undo_offers) take their answer from: [`observation_inputs`], the vault
/// slot's creation claim, the Commons custody that seats this Mac (the retained claim, the custody
/// locks, the pinned anchor), in a debug build a lab vault's home LAN file, and the control
/// journal, by its directory (an entry came or went) and each entry in it and in the folders it
/// holds (an append). `None` where the journal cannot be listed.
pub(super) fn desk_files(paths: &Paths) -> Option<Vec<PathBuf>> {
    let mut files = observation_inputs(paths)?;
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

#[derive(PartialEq, Eq)]
pub(super) struct InputStamps {
    directory: Option<DirectoryStamp>,
    files: Vec<Option<Stamp>>,
}

/// The directory and each of `inputs`' stamps, without following a final symlink. A missing
/// path has no stamp. An unsafe directory or metadata error requires a fresh read.
pub(super) fn stamp_inputs(
    paths: &Paths,
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
) -> Option<InputStamps> {
    let directory = match std::fs::symlink_metadata(&paths.dir) {
        Ok(meta) if meta.is_dir() => Some(DirectoryStamp {
            dev: meta.dev(),
            inode: meta.ino(),
            uid: meta.uid(),
            gid: meta.gid(),
            mode: meta.mode(),
        }),
        Ok(_) => return None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return None,
    };
    let files = inputs(paths)?
        .iter()
        .map(|path| match std::fs::symlink_metadata(path) {
            Ok(meta) => Some(Some(Stamp {
                dev: meta.dev(),
                inode: meta.ino(),
                len: meta.size(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(None),
            Err(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(InputStamps { directory, files })
}

struct Held<T, K> {
    paths: Paths,
    key: K,
    stamps: InputStamps,
    answer: T,
}

/// The last settled answer of one read for each vault folder and key read, oldest first.
struct Memo<T, K = ()> {
    /// The files the read takes its answer from.
    inputs: fn(&Paths) -> Option<Vec<PathBuf>>,
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
        Self {
            inputs,
            quiet,
            held: Mutex::new(Vec::new()),
            refresh: Mutex::new(()),
            #[cfg(test)]
            waiting: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The answer kept for `paths` and `key` under exactly `stamps`.
    fn kept(&self, paths: &Paths, key: &K, stamps: &InputStamps) -> Option<T> {
        let held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.iter()
            .find(|held| held.paths == *paths && held.key == *key && held.stamps == *stamps)
            .map(|held| held.answer.clone())
    }

    /// [`Memo::read_keyed`] for a caller that holds `lock`. A quiet memo takes the caller's shared
    /// observation of the state lock for the one `read_keyed` takes before its stamps, while it
    /// still guards the lock at the vault's path ([`StateLock::observes`]): no writer holds that
    /// lock, and none can write until the caller drops it. Anything else, a miss included, is
    /// `read_keyed`'s.
    fn read_keyed_under(
        &self,
        paths: &Paths,
        key: &K,
        lock: &StateLock,
        read: impl FnOnce(&Paths) -> crate::Result<T>,
    ) -> crate::Result<T> {
        if self.quiet
            && lock.observes(paths)
            && let Some(stamps) = stamp_inputs(paths, self.inputs)
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
        let Some(stamps) = stamp_inputs(paths, self.inputs) else {
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
        let Some(stamps) = stamp_inputs(paths, self.inputs) else {
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
            Ok(true) => match stamp_inputs(paths, self.inputs) {
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
        drop(observation);
        if settled {
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

        let writer = StateLock::take(&paths).unwrap();
        client::save_state(&paths, &original).unwrap();
        assert_eq!(now().unwrap(), Snapshot::Unavailable(Why::WriterBusy));
        drop(writer);
        assert_eq!(
            now().unwrap(),
            expected,
            "a busy observation is not kept after its writer leaves"
        );

        let failed = FailedReads(paths.cache_file());
        fsx::fail_reads(&failed.0, usize::MAX);
        client::save_state(&paths, &original).unwrap();
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
    /// kept without opening the lock again, and reads a changed file as `read_keyed` does.
    ///
    /// NEGATIVE CONTROL (2026-11-08): with `read_keyed_under` making `read_keyed`'s call (the
    /// shape before), each kept answer opens the lock again (`left: 2, right: 0`).
    #[test]
    fn a_quiet_memo_under_its_callers_observation_opens_no_lock_of_its_own() {
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
        let under = || memo.read_keyed_under(&paths, &(), &lock, |p| lines(p, &reads));

        let opens = super::super::lock_opens_of(&paths);
        assert_eq!(under().unwrap(), Some(vec!["m60".to_owned()]));
        assert_eq!(under().unwrap(), Some(vec!["m60".to_owned()]));
        assert_eq!(super::super::lock_opens_of(&paths) - opens, 0);
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
                    memo.read_keyed_under(&paths, &(), &writer, |p| lines(p, &reads))
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
            memo.read_keyed_under(&paths, &(), &renamed_away, |p| lines(p, &reads))
                .unwrap(),
            Some(vec!["m60".to_owned()])
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            5,
            "a lock renamed away guards nothing"
        );
    }
}
