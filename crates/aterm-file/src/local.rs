// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A requested local TSV follows the same overlay as `aterm file cat`. The pass and
//! outbox writers each refresh it after their own durable writes. Refreshes serialize
//! on a separate per-file lock and read both sources without taking either source lock.

use crate::{File, Status};
use aterm_uds::{atomic_file, file_lock::LockFile, trace};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

// A clean read witnessed both the sources and a byte-identical output. Forgetting a
// trace only costs one full refresh. Never hold this process-wide mutex across I/O.
static CLEAN: Mutex<BTreeMap<PathBuf, Arc<trace::Trace>>> = Mutex::new(BTreeMap::new());

std::thread_local! {
    static BATCHES: RefCell<Vec<File>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
std::thread_local! {
    pub(crate) static RENDERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl File {
    /// Materialize and maintain the TSV of this machine's local view. Subsequent passes
    /// and outbox writes refresh it. The ordinary face remains available for editing.
    ///
    /// # Errors
    /// No local view is readable, the TSV exceeds the file size limit, or the local file or
    /// its refresh lock cannot be read or written.
    pub fn local_tsv(&self) -> io::Result<(PathBuf, Status)> {
        self.make_dirs()?;
        let path = self.root().local_tsv(self.name());
        let _lock = LockFile::take(
            &self.root().local_lock(self.name()),
            Instant::now() + Duration::from_secs(10),
            "another writer is refreshing the local TSV",
        )?;
        // A lock file lives beyond its holder. From here on even a pass after a crash
        // can recreate a missing TSV. Never read or replace a nonregular destination.
        let current = match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => Some(meta),
            Ok(_) => return Err(io::Error::other("the local TSV path is not a regular file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let (result, reads) = trace::scope(|| self.write_local(&path, current.as_ref()));
        if result.is_ok() && reads.unkept().is_none() {
            let mut clean = CLEAN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if clean.len() >= 64 && !clean.contains_key(&path) {
                clean.pop_first();
            }
            clean.insert(path.clone(), Arc::new(reads));
        } else if result.is_err() && current.is_some() {
            // A prior overlay would silently claim to show a view we could not produce.
            // Keep the subscription so that the next pass retries.
            fs::remove_file(&path)?;
        }
        result.map(|status| (path, status))
    }

    fn write_local(&self, path: &Path, current: Option<&fs::Metadata>) -> io::Result<Status> {
        #[cfg(test)]
        RENDERS.set(RENDERS.get() + 1);
        let read = self.read();
        if let Some(problem) = &read.status.problem {
            return Err(io::Error::other(problem.to_owned()));
        }
        let snapshot = read.snapshot.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no snapshot or queued rows here")
        })?;
        let bytes = snapshot.full_tsv()?.into_bytes();
        let reusable = current.is_some_and(|meta| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if meta.permissions().mode() & 0o7777 != 0o400 {
                    return false;
                }
            }
            meta.len() == bytes.len() as u64
        });
        if !reusable || atomic_file::read_regular(path, crate::snapshot::MAX_BYTES)?.0 != bytes {
            atomic_file::write(path, &bytes, 0o400)?;
        }
        Ok(read.status)
    }

    /// Queue a group of this file's writes and refresh its local TSV once, including where
    /// the caller rolled back a partially accepted group. Each underlying write stays durable.
    pub fn batch_local_updates<R>(&self, writes: impl FnOnce() -> R) -> R {
        struct Batch<'a>(&'a File);
        impl Drop for Batch<'_> {
            fn drop(&mut self) {
                BATCHES.with_borrow_mut(|batches| batches.pop());
                self.0.refresh_local_if_requested();
            }
        }
        BATCHES.with_borrow_mut(|batches| batches.push(self.clone()));
        let _batch = Batch(self);
        writes()
    }

    pub(crate) fn refresh_local_if_requested(&self) {
        if BATCHES.with_borrow(|batches| {
            batches
                .iter()
                .any(|batch| batch.name() == self.name() && batch.root() == self.root())
        }) {
            return;
        }
        let path = self.root().local_tsv(self.name());
        // Accept an already materialized TSV from before this build as a request too.
        if !self.root().local_lock(self.name()).exists() && !path.exists() {
            return;
        }
        let cached = CLEAN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&path)
            .cloned();
        if let Some(cached) = cached
            && cached.stands(trace::Now::at_now(), Instant::now()).is_ok()
        {
            trace::absorb(&cached);
            return;
        }
        // The underlying write has already landed. A derived file's failure must
        // never turn an accepted intent into a reported refusal.
        let _ = self.local_tsv();
    }
}
