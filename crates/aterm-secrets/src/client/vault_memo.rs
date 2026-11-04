// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The swarm deny list and roster for a reader that asks per connection or per round: a fleet
//! listener, which checks each proved dialer against the deny list, its node job's watch, which
//! checks every live connection each tick, and the node's Commons peer checks and census.
//! [`swarm_denied_nodes`](super::swarm_denied_nodes) and [`swarm_roster`](super::swarm_roster)
//! decrypt the vault's roster entry and evaluate the membership log at every call, tens of
//! milliseconds of CPU on a busy Mac, so this keeps each one's last settled answer and reads
//! again when an input file or the vault directory's identity or access metadata changes.

use super::{Paths, StateLock, StateObservation};
use crate::swarm_roster::SwarmRoster;
use std::{os::unix::fs::MetadataExt as _, path::PathBuf, sync::Mutex};

/// How many vault folders the memo keeps answers for: a process reads one; tests read many.
const MAX_FOLDERS: usize = 8;

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
    static MEMO: Memo<Vec<String>> = Memo::new();
    MEMO.read(paths, super::swarm_denied_nodes)
}

/// [`swarm_roster`](super::swarm_roster), kept and read again exactly as
/// [`swarm_denied_nodes_now`] keeps and reads the deny list: a removal the log takes in is in the
/// next call's answer.
///
/// # Errors
/// As [`swarm_roster`](super::swarm_roster).
pub fn swarm_roster_now(paths: &Paths) -> crate::Result<Option<SwarmRoster>> {
    static MEMO: Memo<SwarmRoster> = Memo::new();
    MEMO.read(paths, super::swarm_roster)
}

/// The files [`swarm_denied_nodes`](super::swarm_denied_nodes) and
/// [`swarm_roster`](super::swarm_roster) take their answer from: the configured source
/// (`source.toml`), the state lock the deny list observes (`.lock`, whose presence and mode decide
/// that read), the applied state (`state.toml`), this Mac's identity, the held envelope (`vault`)
/// and the membership records entry and selected data. The vault folder holds others that
/// change every few seconds and that these reads never open (the vault server's `served/`,
/// `commons/`, the jobs' logs, the
/// needs and answers queues), so a change to them reads nothing again.
fn inputs(paths: &Paths) -> Option<[PathBuf; 7]> {
    Some([
        paths.source_file(),
        paths.lock_file(),
        paths.state_file(),
        paths.identity_file(),
        paths.cache_file(),
        paths.records_file(),
        paths.records_data_file().ok()?,
    ])
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
/// all seven input paths still resolve to the files from an earlier read.
#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectoryStamp {
    dev: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

#[derive(PartialEq, Eq)]
struct InputStamps {
    directory: Option<DirectoryStamp>,
    files: Vec<Option<Stamp>>,
}

/// The directory and each of [`inputs`]' stamps, without following a final symlink. A missing
/// path has no stamp. An unsafe directory or metadata error requires a fresh read.
fn stamp_inputs(paths: &Paths) -> Option<InputStamps> {
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

struct Held<T> {
    paths: Paths,
    stamps: InputStamps,
    answer: Option<T>,
}

/// The last settled answer of one read for each vault folder read, oldest first.
struct Memo<T> {
    held: Mutex<Vec<Held<T>>>,
    /// Held across a read, so callers that miss together wait for one read instead of each
    /// making their own. A caller that finds its answer in `held` never takes it.
    refresh: Mutex<()>,
    /// How many callers have come to wait for `refresh`: the test's witness that they are queued.
    #[cfg(test)]
    waiting: std::sync::atomic::AtomicUsize,
}

impl<T: Clone> Memo<T> {
    const fn new() -> Self {
        Self {
            held: Mutex::new(Vec::new()),
            refresh: Mutex::new(()),
            #[cfg(test)]
            waiting: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The answer kept for `paths` under exactly `stamps`.
    fn kept(&self, paths: &Paths, stamps: &InputStamps) -> Option<Option<T>> {
        let held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.iter()
            .find(|held| held.paths == *paths && held.stamps == *stamps)
            .map(|held| held.answer.clone())
    }

    fn read(
        &self,
        paths: &Paths,
        read: impl FnOnce(&Paths) -> crate::Result<Option<T>>,
    ) -> crate::Result<Option<T>> {
        // Taken before the read: a write after this moves the next stamps, so it is read again.
        let Some(stamps) = stamp_inputs(paths) else {
            return read(paths);
        };
        if let Some(answer) = self.kept(paths, &stamps) {
            return Ok(answer);
        }
        #[cfg(test)]
        self.waiting
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _refresh = self
            .refresh
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Another caller may have read these same files while this one waited.
        let Some(stamps) = stamp_inputs(paths) else {
            return read(paths);
        };
        if let Some(answer) = self.kept(paths, &stamps) {
            return Ok(answer);
        }
        // A lock an older build left wide is narrowed before it is observed, as the read itself
        // narrows it, so the first read after an upgrade can settle. The narrowing moved the
        // lock's change time, so its stamps are taken again, still before the read.
        let narrowed = StateLock::narrow_owned(paths);
        let stamps = match narrowed {
            Ok(true) => match stamp_inputs(paths) {
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
        let observed = read(paths)?;
        drop(observation);
        if settled {
            let mut held = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            held.retain(|held| held.paths != *paths);
            if held.len() >= MAX_FOLDERS {
                held.remove(0);
            }
            held.push(Held {
                paths: paths.clone(),
                stamps,
                answer: observed.clone(),
            });
        }
        Ok(observed)
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
        let memo = Memo::new();
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

        let before = stamp_inputs(&paths).unwrap();
        let memo = Memo::<Vec<String>>::new();
        let failure = FailedReads(paths.cache_file());
        // Keep failing until explicitly cleared, independently of how many reads the
        // authorization path makes. The guard also clears the fault on an assertion panic.
        fsx::fail_reads(&failure.0, usize::MAX);
        let failed = memo.read(&paths, crate::client::swarm_denied_nodes);
        drop(failure);
        assert!(
            stamp_inputs(&paths).unwrap() == before,
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
        let expected = crate::client::swarm_roster(&paths).unwrap();
        assert_eq!(expected.as_ref().unwrap().deny_nodes, ["m99"]);

        let before = stamp_inputs(&paths).unwrap();
        let memo = Memo::<SwarmRoster>::new();
        let failure = FailedReads(paths.cache_file());
        // The read fault lasts until explicitly cleared; no implementation read count
        // decides when the actual signed roster becomes available again.
        fsx::fail_reads(&failure.0, usize::MAX);
        let failed = memo.read(&paths, crate::client::swarm_roster);
        drop(failure);
        assert!(
            stamp_inputs(&paths).unwrap() == before,
            "clearing a read fault must not change the memo's input metadata"
        );
        assert_eq!(crate::client::swarm_roster(&paths).unwrap(), expected);
        assert_eq!(
            memo.read(&paths, crate::client::swarm_roster).unwrap(),
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
        let memo = Memo::<Vec<String>>::new();
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
        let memo = Memo::<Vec<String>>::new();
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
        let memo = Memo::<Vec<String>>::new();
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
        let memo = Memo::<Vec<String>>::new();
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
        let memo = Memo::<Vec<String>>::new();
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
}
