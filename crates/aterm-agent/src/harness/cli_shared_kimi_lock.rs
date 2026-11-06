// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kimi's native refresh exclusion protocol: an empty `.lock` directory with a
//! live mtime. No stale lock is reclaimed until the native five-second lease
//! expires. A descriptor and inode fence prevent a replaced lease from being
//! renewed or removed by its former owner.

use super::*;
use std::fs::{File, FileTimes, Metadata};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, SystemTime};

const STALE: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(1);

#[cfg(test)]
#[path = "cli_shared_kimi_lock_tests.rs"]
mod tests;

#[derive(Debug)]
struct Lease {
    path: PathBuf,
    file: File,
    parent: (u64, u64),
    inode: (u64, u64),
    modified: SystemTime,
    updated: Instant,
    updated_continuous: Option<u64>,
    deadline: crate::harness::auth_deadline::Bounds,
    #[cfg(test)]
    slept_ns: Arc<std::sync::atomic::AtomicU64>,
    active: bool,
}

fn owned_directory(path: &Path) -> Result<Metadata, NativeError> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| NativeError::Temporary)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != aterm_uds::peer::our_uid()
        || meta.mode() & 0o022 != 0
    {
        return Err(NativeError::Temporary);
    }
    Ok(meta)
}

fn identity(meta: &Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

impl Lease {
    fn continuous(&self) -> Option<u64> {
        let now = aterm_uds::process::continuous_ns();
        #[cfg(test)]
        let now = now.map(|now| {
            now.saturating_add(self.slept_ns.load(std::sync::atomic::Ordering::Acquire))
        });
        now
    }

    fn owns_path(&self) -> bool {
        self.path
            .parent()
            .and_then(|path| owned_directory(path).ok())
            .is_some_and(|meta| identity(&meta) == self.parent)
            && owned_directory(&self.path).is_ok_and(|meta| {
                identity(&meta) == self.inode && meta.modified().ok() == Some(self.modified)
            })
    }

    fn current(&self) -> bool {
        let continuous = self.continuous();
        !self
            .deadline
            .remaining(Instant::now(), continuous)
            .is_zero()
            && self.authority_current()
    }

    fn authority_current(&self) -> bool {
        let continuous_fresh = match (self.updated_continuous, self.continuous()) {
            (Some(then), Some(now)) => now
                .checked_sub(then)
                .is_some_and(|age| Duration::from_nanos(age) < STALE),
            _ => true,
        };
        self.active
            && continuous_fresh
            && self.updated.elapsed() < STALE
            && SystemTime::now()
                .duration_since(self.modified)
                .is_ok_and(|age| age < STALE)
            && self.owns_path()
    }

    fn heartbeat(&mut self) -> bool {
        if !self.authority_current() {
            self.active = false;
            return false;
        }
        // Updating the opened inode cannot touch a replacement path. Recheck
        // the pathname after updating before authorizing another HTTP operation.
        let result = self
            .file
            .set_times(FileTimes::new().set_modified(SystemTime::now()))
            .and_then(|()| self.file.metadata())
            .and_then(|meta| meta.modified());
        let Ok(modified) = result else {
            self.active = false;
            return false;
        };
        self.modified = modified;
        self.updated = Instant::now();
        self.updated_continuous = self.continuous();
        self.active = self.owns_path();
        self.active
    }

    fn release(&mut self) {
        self.active = false;
        if self.owns_path() {
            // The native protocol keeps the directory empty. Never recursively
            // remove a lease, even when an unexpected entry prevents release.
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}

pub(in crate::harness) struct NativeLock {
    lease: Arc<Mutex<Lease>>,
    stop: mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    authority: Arc<dyn Authority>,
}

#[derive(Debug)]
struct Guard {
    lease: Arc<Mutex<Lease>>,
    authority: Arc<dyn Authority>,
}

impl aterm_http::Guard for Guard {
    fn is_authorized(&self) -> bool {
        // Never invoke an authority callback while holding the heartbeat mutex.
        self.authority.is_authorized() && self.lease.lock().is_ok_and(|lease| lease.current())
    }
}

impl Authority for Guard {
    fn receipt_current(&self) -> bool {
        self.authority.receipt_current()
            && self
                .lease
                .lock()
                .is_ok_and(|lease| lease.authority_current())
    }
    fn binding_current(&self) -> bool {
        self.authority.binding_current()
    }
    fn recovery_context(&self) -> Result<RecoveryContext, String> {
        self.authority.recovery_context()
    }
    fn checkpoint_started(&self, config: [u8; 32], predecessor: [u8; 32]) -> Result<(), String> {
        if !aterm_http::Guard::is_authorized(self) {
            return Err("Kimi native lease is unavailable".into());
        }
        self.authority.checkpoint_started(config, predecessor)
    }
    fn checkpoint_completed(&self, status: u16, received: i64, body: &[u8]) -> Result<(), String> {
        if !self.binding_current() {
            return Err("Kimi account authority is unavailable".into());
        }
        self.authority.checkpoint_completed(status, received, body)
    }
    fn checkpoint_unstarted(&self) -> Result<(), String> {
        self.authority.checkpoint_unstarted()
    }
    fn checkpoint_dispatching(&self) -> Result<(), String> {
        self.authority.checkpoint_dispatching()
    }
    fn checkpoint_applied(&self) -> Result<(), String> {
        if !self.receipt_current() {
            return Err("Kimi native lease is unavailable".into());
        }
        self.authority.checkpoint_applied()
    }
}

impl NativeLock {
    #[cfg(test)]
    pub(super) fn observe_clock(&self, clock: Option<Arc<std::sync::atomic::AtomicU64>>) {
        if let Some(clock) = clock {
            self.lease.lock().unwrap().slept_ns = clock;
        }
    }
    pub(in crate::harness) fn check_current(&self) -> Result<(), String> {
        if self.lease.lock().is_ok_and(|lease| lease.current()) {
            Ok(())
        } else {
            Err("Kimi native refresh lease is no longer current".into())
        }
    }

    pub(super) fn acquire(
        staging: &Path,
        region: Region,
        deadline: login_drive::Deadline<'_>,
        authority: Arc<dyn Authority>,
    ) -> Result<Self, NativeError> {
        let started = Instant::now();
        if deadline.expired() {
            return Err(NativeError::Temporary);
        }
        check_directory(staging)?;
        let parent_path = staging.join("oauth");
        aterm_secrets::fsx::ensure_private_dir(&parent_path).map_err(|_| NativeError::Temporary)?;
        let parent = identity(&owned_directory(&parent_path)?);
        let credential = credential_path(staging, region);
        let name = credential.file_stem().ok_or(NativeError::Temporary)?;
        let mut name = name.to_os_string();
        name.push(".lock");
        let path = parent_path.join(name);
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let meta = owned_directory(&path)?;
                let modified = meta.modified().map_err(|_| NativeError::Temporary)?;
                if !SystemTime::now()
                    .duration_since(modified)
                    .is_ok_and(|age| age > STALE)
                {
                    return Err(NativeError::Temporary);
                }
                let current = owned_directory(&path)?;
                if identity(&current) != identity(&meta)
                    || current.modified().ok() != Some(modified)
                {
                    return Err(NativeError::Temporary);
                }
                std::fs::remove_dir(&path).map_err(|_| NativeError::Temporary)?;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .map_err(|_| NativeError::Temporary)?;
            }
            Err(_) => return Err(NativeError::Temporary),
        }
        let file = astream_sys::open_nonblocking(&path, astream_sys::Links::Refuse)
            .map_err(|_| NativeError::Temporary)?;
        let meta = file.metadata().map_err(|_| NativeError::Temporary)?;
        let mut lease = Lease {
            path,
            file,
            parent,
            inode: identity(&meta),
            modified: meta.modified().map_err(|_| NativeError::Temporary)?,
            updated: Instant::now(),
            updated_continuous: aterm_uds::process::continuous_ns(),
            deadline: deadline.bounds(),
            #[cfg(test)]
            slept_ns: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            active: true,
        };
        if started.elapsed() >= STALE || !lease.current() {
            lease.release();
            return Err(NativeError::Temporary);
        }
        let lease = Arc::new(Mutex::new(lease));
        let (stop, receive) = mpsc::channel();
        let kept = lease.clone();
        let worker = match std::thread::Builder::new()
            .name("kimi-refresh-lease".into())
            .spawn(move || {
                while matches!(
                    receive.recv_timeout(HEARTBEAT),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let Ok(mut lease) = kept.lock() else { break };
                    if !lease.heartbeat() {
                        break;
                    }
                }
            }) {
            Ok(worker) => worker,
            Err(_) => {
                if let Ok(mut lease) = lease.lock() {
                    lease.release();
                }
                return Err(NativeError::Temporary);
            }
        };
        Ok(Self {
            lease,
            stop,
            worker: Some(worker),
            authority,
        })
    }

    pub(super) fn guard(&self) -> Arc<dyn Authority> {
        Arc::new(Guard {
            authority: self.authority.clone(),
            lease: self.lease.clone(),
        })
    }
}

impl Drop for NativeLock {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Ok(mut lease) = self.lease.lock() {
            lease.release();
        }
    }
}
