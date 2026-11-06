// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained leadership for one explicitly prepared private capacity namespace.
//! This is not computer-wide installation, cross-account policy or child custody.

use crate::{
    Claim,
    cohort_namespace::{self as namespace, Namespace},
    host_protocol::Generation,
};
use aterm_dirfd::{self as fd, AtFlags, RenameFlags};
use std::{
    cell::Cell,
    fs::File,
    io::{self, Read as _, Write as _},
    time::Instant,
};

pub(crate) const LOCK: &str = ".host-owner.lock";
const STATE: &str = ".host-owner.epoch";
const PENDING: &str = ".host-owner.pending";
const MAGIC: &[u8] = b"aterm-capacity-host-epoch-v1\0";
const RECORD_LEN: usize = MAGIC.len() + 32 + 8 + 16;

// These files carry host leadership only. Even a partial publication must not
// make the ledger forget or invalidate the custody of already-owned children.
pub(crate) fn metadata_name(name: &[u8]) -> bool {
    name == LOCK.as_bytes() || name == STATE.as_bytes() || name == PENDING.as_bytes()
}

fn refused(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Epoch {
    sequence: u64,
    generation: Generation,
}

impl Epoch {
    fn encode(self, nonce: &str) -> [u8; RECORD_LEN] {
        let mut bytes = [0u8; RECORD_LEN];
        bytes[..MAGIC.len()].copy_from_slice(MAGIC);
        bytes[MAGIC.len()..MAGIC.len() + 32].copy_from_slice(nonce.as_bytes());
        bytes[MAGIC.len() + 32..MAGIC.len() + 40].copy_from_slice(&self.sequence.to_be_bytes());
        bytes[MAGIC.len() + 40..].copy_from_slice(&self.generation.0);
        bytes
    }
    fn read(parent: &File, namespace: &Namespace) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let (_, private) = aterm_uds::private_file::open_private(&namespace.backend(), STATE)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let (file, actual) = namespace::regular(parent, STATE)?;
        if actual != (private.dev(), private.ino()) {
            return Err(refused("private host epoch changed during open"));
        }
        let mut buf = [0u8; RECORD_LEN + 1];
        let mut len = 0;
        let mut reader = file.take((RECORD_LEN + 1) as u64);
        while len < buf.len() {
            match reader.read(&mut buf[len..])? {
                0 => break,
                n => len += n,
            }
        }
        let bytes = &buf[..len];
        if bytes.len() != RECORD_LEN
            || !bytes.starts_with(MAGIC)
            || &bytes[MAGIC.len()..MAGIC.len() + 32] != namespace.nonce().as_bytes()
        {
            return Err(refused("host epoch record is malformed or foreign"));
        }
        let sequence = u64::from_be_bytes(
            bytes[MAGIC.len() + 32..MAGIC.len() + 40]
                .try_into()
                .expect("fixed epoch width"),
        );
        let generation = Generation(
            bytes[MAGIC.len() + 40..]
                .try_into()
                .expect("fixed generation width"),
        );
        if (sequence == 0) != (generation.0 == [0; 16]) {
            return Err(refused("host epoch bootstrap and generation disagree"));
        }
        Ok(Self {
            sequence,
            generation,
        })
    }
}

pub(crate) fn prepare(parent: &File, nonce: &str) -> io::Result<()> {
    let mut file = namespace::create(parent, STATE)?;
    file.write_all(
        &Epoch {
            sequence: 0,
            generation: Generation([0; 16]),
        }
        .encode(nonce),
    )?;
    aterm_durable::durable(&file)
}

fn no_pending(parent: &File) -> io::Result<()> {
    match fd::fstatat(parent, PENDING, AtFlags::SYMLINK_NOFOLLOW) {
        Err(error) if error == fd::Errno::NOENT => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(refused(
            "host epoch publication is incomplete; no rollback or reset",
        )),
    }
}

/// One process's retained, namespace-bound leadership. The lock inode is anchored
/// in namespace birth evidence, never created or replaced by acquisition.
/// Dropping it releases leadership, never releases a job's reservation.
#[derive(Debug)]
pub struct Fence {
    namespace: Namespace,
    lock: File,
    backend: File,
    process: u32,
    epoch: Epoch,
    revoked: Cell<bool>,
}

/// Captured only from actual, current leadership and original claim custody.
/// Keeping this separate lets a controller own its fence beside its jobs.
#[derive(Debug)]
pub(crate) struct Binding {
    namespace: Namespace,
    epoch: Epoch,
    process: u32,
}

impl Binding {
    pub(crate) fn check(&self, fence: &Fence) -> io::Result<()> {
        fence.check_current()?;
        if self.namespace != fence.namespace
            || self.epoch != fence.epoch
            || self.process != fence.process
        {
            return Err(refused(
                "retained cohort belongs to different host leadership",
            ));
        }
        Ok(())
    }
}

impl Fence {
    /// Acquire existing leadership, durably advance its incarnation and retain
    /// its exact lock. No old claim is removed, rebound or interpreted as drained.
    /// # Errors
    /// Busy at the deadline, wrong namespace/lock identity, missing/malformed/
    /// incomplete/overflowed epoch, private-file, random or durability failure.
    pub fn acquire(namespace: &Namespace, deadline: Instant) -> io::Result<Self> {
        let backend = namespace.open_backend()?;
        let lock =
            aterm_uds::kept_out(|| namespace.open_host_lock_in(&backend).map(|(lock, _)| lock))?;
        aterm_uds::file_lock::lock_exclusive_until(
            &lock,
            deadline,
            "capacity host already has an owner",
        )?;
        let mut fence = Self {
            namespace: namespace.clone(),
            lock,
            backend,
            process: std::process::id(),
            epoch: Epoch {
                sequence: 0,
                generation: Generation([0; 16]),
            },
            revoked: Cell::new(false),
        };
        fence.check_namespace()?;
        no_pending(&fence.backend)?;
        let prior = Epoch::read(&fence.backend, namespace)?;
        let sequence = prior
            .sequence
            .checked_add(1)
            .ok_or_else(|| refused("host epoch exhausted"))?;
        let mut generation = [0; 16];
        aterm_uds::rand::fill(&mut generation)?;
        if generation == [0; 16] || generation == prior.generation.0 {
            return Err(refused("host incarnation did not advance"));
        }
        fence.epoch = Epoch {
            sequence,
            generation: Generation(generation),
        };
        let mut pending = namespace::create(&fence.backend, PENDING)?;
        pending.write_all(&fence.epoch.encode(namespace.nonce()))?;
        aterm_durable::durable(&pending)?;
        // Flush the pending name before replacement. An interrupted attempt can
        // leave evidence that refuses acquisition; it is never silently reset.
        aterm_durable::durable(&fence.backend)?;
        fence.check_namespace()?;
        if Epoch::read(&fence.backend, namespace)? != prior {
            return Err(refused("host epoch changed while leadership was held"));
        }
        fd::renameat_with(
            &fence.backend,
            PENDING,
            &fence.backend,
            STATE,
            RenameFlags::empty(),
        )?;
        aterm_durable::durable(&fence.backend)?;
        fence.check_current()?;
        Ok(fence)
    }

    fn check_namespace(&self) -> io::Result<()> {
        let current_backend = self.namespace.open_backend()?;
        // Validate ACL/privacy through the shared reader as well as retained
        // dirfd/inode identity. Neither a new lock nor a path alias is authority.
        aterm_uds::private_file::open_private(&self.namespace.backend(), LOCK)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let (_, named) = self.namespace.open_host_lock_in(&current_backend)?;
        use std::os::unix::fs::MetadataExt as _;
        let original = self.lock.metadata()?;
        let backend = self.backend.metadata()?;
        let current_backend = current_backend.metadata()?;
        if (original.dev(), original.ino()) != named
            || (backend.dev(), backend.ino()) != (current_backend.dev(), current_backend.ino())
        {
            return Err(refused("host leadership physical identity changed"));
        }
        Ok(())
    }

    /// Revalidate this live incarnation. A refusal permanently revokes new-launch
    /// authority for this object, even if someone later restores a pathname.
    /// Actual child custody and existing reservations remain separate.
    /// # Errors
    /// Foreign process, revoked owner, namespace/lock/state replacement or read failure.
    pub fn check_current(&self) -> io::Result<()> {
        if self.process != std::process::id() || self.revoked.get() {
            return Err(refused(
                "host leadership is stale or belongs to another process",
            ));
        }
        let checked = (|| {
            self.check_namespace()?;
            no_pending(&self.backend)?;
            if Epoch::read(&self.backend, &self.namespace)? != self.epoch {
                return Err(refused("host leadership incarnation changed"));
            }
            Ok(())
        })();
        if checked.is_err() {
            self.revoked.set(true);
        }
        checked
    }

    /// The committed incarnation, not a credential or reconstructed child handle.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.epoch.generation
    }

    /// Durable monotonic incarnation count within this prepared namespace.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.epoch.sequence
    }

    /// Require the original retained cohort custody in this exact namespace.
    /// A legacy claim, checkout path or decoded record cannot supply it.
    /// # Errors
    /// Stale leadership, foreign/legacy claim or invalid actual custody.
    pub fn check_claim(&self, claim: &Claim) -> io::Result<()> {
        self.check_current()?;
        claim.check_cohort_namespace(&self.namespace)
    }

    pub(crate) fn capture_binding(&self, claim: &Claim) -> io::Result<Binding> {
        self.check_claim(claim)?;
        Ok(Binding {
            namespace: self.namespace.clone(),
            epoch: self.epoch,
            process: self.process,
        })
    }
}

impl Drop for Fence {
    fn drop(&mut self) {
        // A post-fork copy must not unlock its parent's shared description.
        if self.process == std::process::id() {
            let _ = self.lock.unlock();
        }
    }
}

#[cfg(test)]
#[path = "leadership_tests.rs"]
mod tests;
