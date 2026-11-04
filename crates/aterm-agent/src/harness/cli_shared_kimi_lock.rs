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
    deadline: Instant,
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
        self.active
            && Instant::now() < self.deadline
            && self.updated.elapsed() < STALE
            && self.owns_path()
    }

    fn heartbeat(&mut self) -> bool {
        if !self.current() {
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
}

#[derive(Debug)]
struct Guard {
    authority: Arc<dyn aterm_http::Guard>,
    lease: Arc<Mutex<Lease>>,
}

impl aterm_http::Guard for Guard {
    fn is_authorized(&self) -> bool {
        // Never invoke an authority callback while holding the heartbeat mutex.
        self.authority.is_authorized() && self.lease.lock().is_ok_and(|lease| lease.current())
    }
}

impl NativeLock {
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
        deadline: Instant,
    ) -> Result<Self, NativeError> {
        let started = Instant::now();
        if Instant::now() >= deadline {
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
            deadline,
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
        })
    }

    pub(super) fn guard(
        &self,
        authority: Arc<dyn aterm_http::Guard>,
    ) -> Arc<dyn aterm_http::Guard> {
        Arc::new(Guard {
            authority,
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
