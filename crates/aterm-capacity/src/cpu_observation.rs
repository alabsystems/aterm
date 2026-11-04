// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only observations of the core toolchain's cooperative CPU unit leases.
//! The core pool owns the file identities, record parser and lock protocol.
//! These observations do not grant capacity or implement allocation policy.

use std::path::Path;

/// A held slot's diagnostic record, which is not proof of process identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuHolder {
    /// The zero-based CPU lease slot.
    pub slot: u32,
    /// The process reported by the holder, absent when its record cannot be read.
    pub reported_pid: Option<u32>,
    /// The reported platform process-birth identity, if one was recorded.
    pub reported_birth: Option<u64>,
}

/// One nonblocking scan of a cooperative CPU lease pool.
///
/// Only [`Self::Ready`] provides occupied and available counts. Missing,
/// initializing and unreadable pools do not imply unused capacity. Slot states
/// are observations during the scan, not an atomic allocation transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CpuObservation {
    /// This platform has no implementation of the native CPU lease pool.
    Unsupported,
    /// No pool exists at the supplied account capacity directory.
    Absent,
    /// An initializer currently holds the pool's configuration lock.
    Initializing,
    /// The account directory or pool could not be read safely.
    Unreadable(String),
    /// A successfully observed pool; kernel locks determine its occupied slots.
    Ready {
        /// Total number of slots declared by the pool.
        total: usize,
        /// Slots observed locked, including holders with unreadable records.
        holders: Vec<CpuHolder>,
    },
}

impl CpuObservation {
    /// Read an existing pool without creating, repairing or claiming any file.
    ///
    /// `dir` is the account capacity directory, usually [`crate::ledger::user_dir`].
    /// Missing directory identity is unreadable on a supported platform. A busy
    /// initialization lock is reported immediately instead of waiting for it.
    #[must_use]
    pub fn read(dir: Option<&Path>) -> Self {
        #[cfg(unix)]
        {
            use crate::build_cpu::{Pool, SlotState};

            let Some(dir) = dir else {
                return Self::Unreadable("account capacity directory unavailable".to_owned());
            };
            let result = Pool::open_existing(dir)
                .and_then(|pool| pool.map(|pool| pool.snapshot()).transpose());
            match result {
                Ok(None) => Self::Absent,
                Ok(Some(slots)) => Self::Ready {
                    total: slots.len(),
                    holders: slots
                        .into_iter()
                        .filter_map(|slot| match slot {
                            SlotState::Available { .. } => None,
                            SlotState::Held {
                                slot,
                                reported_owner,
                            } => Some(CpuHolder {
                                slot,
                                reported_pid: reported_owner.map(|owner| owner.pid),
                                reported_birth: reported_owner.and_then(|owner| owner.birth),
                            }),
                        })
                        .collect(),
                },
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Self::Initializing,
                Err(error) => Self::Unreadable(error.to_string()),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = dir;
            Self::Unsupported
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::num::NonZeroU32;
    use std::time::{Duration, Instant};

    use crate::build_cpu::{Owner, Pool};

    use super::*;

    const HANG: Duration = Duration::from_secs(60);

    // A sibling test can fork while one of our descriptors is open. The
    // inherited lock lasts until that child execs or closes it; observe the
    // required state after that copy closes, rather than assuming one sample.
    fn eventually<T>(what: &str, mut step: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + HANG;
        loop {
            if let Some(value) = step() {
                return value;
            }
            assert!(Instant::now() < deadline, "{what} within the hang detector");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn pool(dir: &Path, slots: u32) -> Pool {
        Pool::open_in(dir, NonZeroU32::new(slots).unwrap()).unwrap()
    }

    #[test]
    fn an_absent_pool_is_reported_without_creating_files() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(CpuObservation::read(Some(&missing)), CpuObservation::Absent);
        assert!(!missing.exists());
        assert_eq!(
            CpuObservation::read(Some(dir.path())),
            CpuObservation::Absent
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(matches!(
            CpuObservation::read(None),
            CpuObservation::Unreadable(_)
        ));
    }

    #[test]
    fn held_slots_follow_kernel_locks_and_not_stale_owner_records() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let pool = pool(dir.path(), 2);
        let owner = Owner::current(Some(7));
        let lease = eventually("a lease after the opener's last copy closes", || match pool
            .try_acquire(owner)
        {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            result => result.unwrap(),
        });
        let expected = CpuObservation::Ready {
            total: 2,
            holders: vec![CpuHolder {
                slot: lease.slot(),
                reported_pid: Some(owner.pid),
                reported_birth: Some(7),
            }],
        };
        let observed = eventually("only the deliberately held lease remains", || {
            let observed = CpuObservation::read(Some(dir.path()));
            (observed == expected).then_some(observed)
        });
        assert_eq!(observed, expected);
        drop(lease);
        let released = eventually("the released lease's last copy to close", || {
            let observed = CpuObservation::read(Some(dir.path()));
            matches!(&observed, CpuObservation::Ready { total: 2, holders } if holders.is_empty())
                .then_some(observed)
        });
        assert_eq!(
            released,
            CpuObservation::Ready {
                total: 2,
                holders: vec![]
            }
        );
    }

    #[test]
    fn an_initializer_is_reported_without_waiting_for_its_lock() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let pool = pool(dir.path(), 1);
        let _initializer = eventually("the initializer's last inherited copy to close", || {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(pool.path().join("init.lock"))
                .unwrap();
            match aterm_toolchain::record_lock::RecordLock::try_lock(file, 0) {
                Err(std::fs::TryLockError::WouldBlock) => None,
                result => Some(result.unwrap()),
            }
        });
        let path = dir.path().to_owned();
        let (send, receive) = std::sync::mpsc::channel();
        let reader =
            std::thread::spawn(move || send.send(CpuObservation::read(Some(&path))).unwrap());
        assert_eq!(
            receive
                .recv_timeout(HANG)
                .expect("pool observation must not wait on an initializer"),
            CpuObservation::Initializing,
        );
        reader.join().unwrap();
    }

    #[test]
    fn a_corrupt_pool_is_unreadable_and_is_not_repaired() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let pool = pool(dir.path(), 1);
        let manifest = pool.path().join("pool");
        std::fs::write(&manifest, "invalid").unwrap();
        let observed = eventually("the corrupt pool is no longer initializing", || {
            let observed = CpuObservation::read(Some(dir.path()));
            (!matches!(observed, CpuObservation::Initializing)).then_some(observed)
        });
        assert!(matches!(observed, CpuObservation::Unreadable(_)));
        assert_eq!(std::fs::read_to_string(manifest).unwrap(), "invalid");
    }
}

#[cfg(all(test, not(unix)))]
mod unsupported_tests {
    use super::CpuObservation;

    #[test]
    fn unsupported_platform_is_not_reported_as_an_empty_pool() {
        assert_eq!(CpuObservation::read(None), CpuObservation::Unsupported);
    }
}
