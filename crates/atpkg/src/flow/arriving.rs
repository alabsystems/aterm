// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! STAGE AS IT ARRIVES: a tar archive extracted into its build's staging scratch while its
//! download writes it, the bytes hashed as they are read, so the stage is done about when the
//! download is instead of starting then. The scratch is used only when the bytes the stage read
//! hash to the row's signed digest: the stage then goes on as a verified download's does
//! ([`crate::install::finish_streamed`]). Anything short of that removes the scratch, and the
//! completed download stages from its file, digest first, exactly as it did before.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::install::Streamed;
use crate::manifest::{Artifact, DigestAlg};

/// Whether this platform stages a download as it arrives. Off Unix the download lands first and
/// stages from its file: a reader beside the downloader's open file is not exercised there.
const STREAMS: bool = cfg!(unix);

/// The shortest and longest wait between two looks at a download that has not grown.
const IDLE_MIN: Duration = Duration::from_millis(1);
const IDLE_MAX: Duration = Duration::from_millis(16);

/// Whether `artifact`'s download stages as it arrives ([`crate::install::streams`]).
pub(crate) fn streams(artifact: &Artifact) -> bool {
    STREAMS && crate::install::streams(artifact) && artifact.digest().is_some()
}

/// Whether the fetch has ended, and how ([`Arriving`]).
#[derive(Default)]
struct Arrival {
    /// `None` while the fetch runs; then whether it succeeded.
    ended: Mutex<Option<bool>>,
    changed: Condvar,
}

impl Arrival {
    fn end(&self, ok: bool) {
        *self
            .ended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ok);
        self.changed.notify_all();
    }

    fn ended(&self) -> Option<bool> {
        *self
            .ended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait up to `idle`, or until the fetch ends.
    fn wait(&self, idle: Duration) {
        let ended = self
            .ended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ended.is_none() {
            drop(
                self.changed
                    .wait_timeout(ended, idle)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }
}

/// One file a download's bytes may arrive in, opened once it is there.
struct Source {
    path: PathBuf,
    file: Option<std::fs::File>,
    /// The file was there before the fetch began: an earlier attempt's, which the fetch may
    /// keep or remove. It is opened only once the fetch has ended, so it is the one it left.
    after_fetch: bool,
}

impl Source {
    /// How many bytes the file holds now, opening it first if it has appeared (and, for a file
    /// that predates the fetch, once the fetch has `ended`).
    fn len(&mut self, ended: bool) -> u64 {
        if self.file.is_none() && (ended || !self.after_fetch) {
            self.file = crate::metadata_io::open_regular(&self.path).ok();
        }
        self.file
            .as_ref()
            .and_then(|file| file.metadata().ok())
            .map_or(0, |meta| meta.len())
    }
}

/// The bytes of `dl`'s download as its fetch writes them, read as they grow until the fetch
/// ends, and hashed with the row's digest algorithm on the way. They may arrive in any of
/// [`crate::peers::arrival_paths`]: the release host's partial, a LAN peer's partial, or `dl`
/// itself. Every source carries the same signed bytes at the same offsets, so each read takes
/// the next bytes from whichever source holds them first; a source that carries other bytes
/// only makes the digest of what was read miss the signed one.
struct Arriving<'a> {
    sources: Vec<Source>,
    arrival: &'a Arrival,
    /// The most bytes the row's signed size lets through.
    cap: u64,
    taken: u64,
    digest: ring::digest::Context,
    idle: Duration,
}

impl<'a> Arriving<'a> {
    fn new(dl: &Path, arrival: &'a Arrival, artifact: &Artifact) -> Self {
        let alg = match artifact.digest() {
            Some((DigestAlg::Sha512, _)) => &ring::digest::SHA512,
            _ => &ring::digest::SHA256,
        };
        Self {
            sources: crate::peers::arrival_paths(dl)
                .into_iter()
                .enumerate()
                .map(|(i, path)| Source {
                    // The release host's partial resumes with the same bytes at the same
                    // offsets; any other file already there is left over.
                    after_fetch: i > 0 && std::fs::symlink_metadata(&path).is_ok(),
                    path,
                    file: None,
                })
                .collect(),
            arrival,
            cap: if artifact.size == 0 {
                u64::MAX
            } else {
                artifact.size
            },
            taken: 0,
            digest: ring::digest::Context::new(alg),
            idle: IDLE_MIN,
        }
    }

    /// The source that holds the most bytes, if it holds any past what was read.
    fn ahead(&mut self, ended: bool) -> Option<usize> {
        let taken = self.taken;
        self.sources
            .iter_mut()
            .map(|source| source.len(ended))
            .enumerate()
            .filter(|(_, len)| *len > taken)
            .max_by_key(|(_, len)| *len)
            .map(|(i, _)| i)
    }

    /// Read into `out` from source `i` at the next unread offset.
    fn read_from(&mut self, i: usize, out: &mut [u8]) -> io::Result<usize> {
        let taken = self.taken;
        let Some(file) = self.sources.get_mut(i).and_then(|s| s.file.as_mut()) else {
            return Ok(0);
        };
        file.seek(SeekFrom::Start(taken))?;
        let n = file.read(out)?;
        self.taken = self.taken.saturating_add(n as u64);
        if self.taken > self.cap {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the download is larger than its signed size",
            ));
        }
        if let Some(bytes) = out.get(..n) {
            self.digest.update(bytes);
        }
        Ok(n)
    }

    /// The digest of every byte read, as lowercase hex.
    fn finish(self) -> String {
        aterm_codec::hex::encode_lower(self.digest.finish().as_ref())
    }
}

impl Read for Arriving<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            // Read the fetch's state before the files: a look that finds nothing new after the
            // fetch ended is the true end, since every byte was written before it ended.
            let ended = self.arrival.ended();
            if let Some(i) = self.ahead(ended.is_some()) {
                let n = self.read_from(i, out)?;
                if n > 0 {
                    self.idle = IDLE_MIN;
                    return Ok(n);
                }
            }
            match ended {
                Some(true) if self.sources.iter().any(|s| s.file.is_some()) => return Ok(0),
                Some(true) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "the download left no file",
                    ));
                }
                Some(false) => return Err(io::Error::other("the download failed")),
                None => {
                    self.arrival.wait(self.idle);
                    self.idle = self.idle.saturating_mul(2).min(IDLE_MAX);
                }
            }
        }
    }
}

/// Run `fetch` into `dl` while `artifact`'s tar stages, as its bytes arrive, into a fresh
/// staging scratch beside `build_dir`. Hands back the fetch's answer, and the stage when the
/// fetch succeeded and every byte the stage read — the whole download — hashes to the signed
/// digest. Otherwise the scratch is gone and nothing is handed back: the caller stages the
/// completed download from its file, its digest checked first, as a download that did not
/// stream.
pub(crate) fn fetch_staging<T>(
    artifact: &Artifact,
    dl: &Path,
    build_dir: &Path,
    fetch: impl FnOnce(&Path) -> Result<T, String>,
) -> (Result<T, String>, Option<Streamed>) {
    let Some(incoming) = crate::install::open_streamed(build_dir) else {
        return (fetch(dl), None);
    };
    let arrival = Arrival::default();
    let (fetched, staged) = std::thread::scope(|scope| {
        let staging = std::thread::Builder::new()
            .name("atpkg-stage-arriving".into())
            .spawn_scoped(scope, || {
                let mut arriving = Arriving::new(dl, &arrival, artifact);
                let staged =
                    crate::install::extract_streamed(artifact, &mut arriving, incoming.dir());
                // The tar can end before its compressed stream does: the digest covers it all.
                let read_whole = staged.is_ok() && io::copy(&mut arriving, &mut io::sink()).is_ok();
                (staged.ok(), read_whole.then(|| arriving.finish()))
            });
        // End the arrival however `fetch` returns: were it to panic, the staging thread would
        // otherwise wait on a `None` end forever ([`Arriving::read`]), and the scope's join of
        // it would never return — a hang in place of the panic.
        struct EndOnDrop<'a>(&'a Arrival, std::cell::Cell<bool>);
        impl Drop for EndOnDrop<'_> {
            fn drop(&mut self) {
                if self.1.get() {
                    self.0.end(false);
                }
            }
        }
        let armed = EndOnDrop(&arrival, std::cell::Cell::new(true));
        let fetched = fetch(dl);
        arrival.end(fetched.is_ok());
        armed.1.set(false);
        let staged = staging
            .ok()
            .and_then(|staging| staging.join().ok())
            .and_then(|(staged, read)| {
                let signed = artifact.digest().map_or("", |(_, hex)| hex);
                let verified = fetched.is_ok()
                    && !signed.is_empty()
                    && read.is_some_and(|read| read.eq_ignore_ascii_case(signed));
                staged.filter(|_| verified)
            });
        (fetched, staged)
    });
    (
        fetched,
        staged.map(|(root, modes)| incoming.staged(root, modes)),
    )
}

#[cfg(test)]
#[path = "arriving_tests.rs"]
mod tests;
