// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use aterm_slog::journal::{Inst, Liveness, PRUNE_LOCK, lock_name, pending_lock_name};

use super::{Appender, Held, JournalFs, Meta};

/// One entry.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) bytes: Vec<u8>,
    pub(crate) ino: u64,
    pub(crate) mtime_ms: u64,
    /// Not a private file of this user (a foreign or hard-linked one).
    pub(crate) foreign: bool,
    /// A symlink: every open of it fails (`ELOOP`).
    pub(crate) symlink: bool,
}

/// A fault the next write meets.
#[derive(Clone, Debug)]
pub(crate) enum Fault {
    /// Writes after `n` more bytes fail with this errno (ENOSPC, EIO…);
    /// a write straddling the limit is short.
    After(u64, i32),
    /// Every write fails with this errno until cleared.
    Always(i32),
    /// The next `n` writes fail with EINTR.
    Eintr(u32),
    /// Writes block until the barrier is released.
    Block(Arc<(Mutex<bool>, std::sync::Condvar)>),
    /// The next write panics.
    Panic,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) files: BTreeMap<String, Entry>,
    pub(crate) locks: BTreeMap<String, usize>,
    pub(crate) next_ino: u64,
    pub(crate) clock_ms: u64,
    pub(crate) fault: Option<Fault>,
    pub(crate) writes: u64,
    /// Writes begun (counted before a barrier blocks one).
    pub(crate) entered: u64,
    pub(crate) free: Option<u64>,
    pub(crate) no_flock: bool,
    pub(crate) creates_refused: Option<i32>,
    /// Every flush (a segment's, the directory's) fails with this errno until cleared.
    pub(crate) flush_fault: Option<i32>,
    /// Names a scan visits after every other (a real directory's order
    /// is its own: a test puts a name past a listing's bound).
    pub(crate) listed_last: std::collections::BTreeSet<String>,
    /// Segments created.
    pub(crate) created: u64,
}

/// The in-memory directory, shareable across "processes" (two writers
/// and a reader in one test).
#[derive(Clone, Default)]
pub(crate) struct MemFs(pub(crate) Arc<Mutex<State>>);

fn err(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

impl MemFs {
    pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.state().files.keys().cloned().collect()
    }

    pub(crate) fn bytes(&self, name: &str) -> Vec<u8> {
        self.state()
            .files
            .get(name)
            .map(|e| e.bytes.clone())
            .unwrap_or_default()
    }

    /// Every byte of every file: what a leak test greps.
    pub(crate) fn all_bytes(&self) -> Vec<u8> {
        self.state()
            .files
            .values()
            .flat_map(|e| e.bytes.clone())
            .collect()
    }

    pub(crate) fn put(&self, name: &str, bytes: &[u8], foreign: bool, symlink: bool) {
        let mut s = self.state();
        s.next_ino += 1;
        let (ino, mtime_ms) = (s.next_ino, s.clock_ms);
        s.files.insert(
            name.to_string(),
            Entry {
                bytes: bytes.to_vec(),
                ino,
                mtime_ms,
                foreign,
                symlink,
            },
        );
    }

    pub(crate) fn fault(&self, f: Option<Fault>) {
        self.state().fault = f;
    }

    pub(crate) fn tick(&self, ms: u64) {
        self.state().clock_ms += ms;
    }
}

struct MemLock {
    fs: MemFs,
    name: String,
    ino: u64,
}

impl Held for MemLock {
    fn identity(&self) -> Option<(u64, u64)> {
        Some((1, self.ino))
    }
}

impl Drop for MemLock {
    fn drop(&mut self) {
        let mut s = self.fs.state();
        if let Some(n) = s.locks.get_mut(&self.name) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                s.locks.remove(&self.name);
            }
        }
    }
}

struct MemSeg {
    fs: MemFs,
    name: String,
}

impl Appender for MemSeg {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let fault = {
            let mut s = self.fs.state();
            s.entered += 1;
            s.fault.clone()
        };
        if let Some(Fault::Block(barrier)) = &fault {
            let (lock, cv) = &**barrier;
            let mut open = lock.lock().unwrap_or_else(|p| p.into_inner());
            while !*open {
                open = cv.wait(open).unwrap_or_else(|p| p.into_inner());
            }
        }
        let mut s = self.fs.state();
        s.writes += 1;
        let n = match s.fault.clone() {
            Some(Fault::Panic) => {
                s.fault = None;
                drop(s);
                panic!("an injected panic in the journal's write");
            }
            Some(Fault::Always(e)) => return Err(err(e)),
            Some(Fault::Eintr(k)) => {
                s.fault = if k > 1 {
                    Some(Fault::Eintr(k - 1))
                } else {
                    None
                };
                return Err(err(libc::EINTR));
            }
            Some(Fault::After(room, e)) => {
                if room == 0 {
                    return Err(err(e));
                }
                let n = usize::try_from(room).unwrap_or(usize::MAX).min(bytes.len());
                s.fault = Some(Fault::After(room - n as u64, e));
                n
            }
            Some(Fault::Block(_)) | None => bytes.len(),
        };
        let clock = s.clock_ms;
        let e = s
            .files
            .get_mut(&self.name)
            .ok_or_else(|| err(libc::ENOENT))?;
        e.bytes.extend_from_slice(&bytes[..n]);
        e.mtime_ms = clock;
        Ok(n)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.fs.state().flush_fault.map_or(Ok(()), |e| Err(err(e)))
    }
}

impl JournalFs for MemFs {
    fn lock_instance(&self, inst: &Inst) -> io::Result<Box<dyn Held>> {
        if self.state().no_flock {
            return Err(err(libc::ENOTSUP));
        }
        let name = lock_name(inst);
        let _ = pending_lock_name(inst);
        if self.state().files.contains_key(&name) {
            return Err(err(libc::EEXIST));
        }
        self.put(&name, b"", false, false);
        let ino = self.state().files.get(&name).map_or(0, |e| e.ino);
        *self.state().locks.entry(name.clone()).or_insert(0) += 1;
        Ok(Box::new(MemLock {
            fs: self.clone(),
            name,
            ino,
        }))
    }

    fn unlock_instance(&self, inst: &Inst, held: Box<dyn Held>) {
        self.state().files.remove(&lock_name(inst));
        drop(held);
    }

    fn create_segment(&self, name: &str) -> io::Result<Box<dyn Appender>> {
        if let Some(e) = self.state().creates_refused {
            return Err(err(e));
        }
        if self.state().files.contains_key(name) {
            return Err(err(libc::EEXIST));
        }
        self.put(name, b"", false, false);
        self.state().created += 1;
        Ok(Box::new(MemSeg {
            fs: self.clone(),
            name: name.to_string(),
        }))
    }

    fn scan(&self, max: usize, visit: &mut dyn FnMut(&str)) -> io::Result<bool> {
        let (mut names, last): (Vec<String>, Vec<String>) = {
            let s = self.state();
            s.files
                .keys()
                .cloned()
                .partition(|n| !s.listed_last.contains(n))
        };
        names.extend(last);
        for (i, name) in names.iter().enumerate() {
            if i == max {
                return Ok(true);
            }
            visit(name);
        }
        Ok(false)
    }

    fn meta(&self, name: &str) -> io::Result<Meta> {
        let s = self.state();
        let e = s.files.get(name).ok_or_else(|| err(libc::ENOENT))?;
        if e.symlink {
            return Err(err(libc::ELOOP));
        }
        Ok(Meta {
            size: e.bytes.len() as u64,
            mtime_ms: e.mtime_ms,
            dev: 1,
            ino: e.ino,
            private: !e.foreign,
        })
    }

    fn read(&self, name: &str, from: u64, max: u64) -> io::Result<(Vec<u8>, Meta)> {
        let meta = self.meta(name)?;
        if !meta.private {
            return Err(err(libc::EACCES));
        }
        let s = self.state();
        let e = s.files.get(name).ok_or_else(|| err(libc::ENOENT))?;
        let from = usize::try_from(from)
            .unwrap_or(usize::MAX)
            .min(e.bytes.len());
        let to = from
            .saturating_add(usize::try_from(max).unwrap_or(usize::MAX))
            .min(e.bytes.len());
        Ok((e.bytes[from..to].to_vec(), meta))
    }

    fn remove(&self, name: &str, expect: Meta) -> io::Result<()> {
        let now = self.meta(name)?;
        if !now.private || now.ino != expect.ino {
            return Err(err(libc::EACCES));
        }
        self.state().files.remove(name);
        Ok(())
    }

    fn probe(&self, name: &str) -> Liveness {
        let s = self.state();
        match s.files.get(name) {
            None => Liveness::Dead,
            Some(e) if e.symlink || e.foreign => Liveness::Unknown,
            Some(_) if s.locks.contains_key(name) => Liveness::Live,
            Some(_) => Liveness::Dead,
        }
    }

    fn try_prune_lock(&self) -> io::Result<Option<Box<dyn Held>>> {
        let mut s = self.state();
        if s.locks.contains_key(PRUNE_LOCK) {
            return Ok(None);
        }
        s.locks.insert(PRUNE_LOCK.to_string(), 1);
        drop(s);
        Ok(Some(Box::new(MemLock {
            fs: self.clone(),
            name: PRUNE_LOCK.to_string(),
            ino: 0,
        })))
    }

    fn sync_dir(&self) -> io::Result<()> {
        self.state().flush_fault.map_or(Ok(()), |e| Err(err(e)))
    }

    fn free_bytes(&self) -> Option<u64> {
        self.state().free
    }
}
