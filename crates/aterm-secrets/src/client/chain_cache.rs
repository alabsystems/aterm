// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the exact bytes of a vault file parse to, whether their signature verifies and the public
//! signer lineage they carry: pure functions of those bytes, compared whole. A read of the held
//! vault ([`held`]) whose opened file shows a [`FileStamp`] an entry's bytes were read under takes
//! that entry without reading the file again, as the membership records' cache does
//! (`records_cache`): the stamp tells one file's contents from another's, since a writer of the
//! held vault that changes its bytes renames a new file over it (`fsx::write_atomic`), and one
//! that finds its bytes in place leaves the file (`fsx::write_atomic_if_changed`). Any other
//! read reads the file whole, and each answer that also depends on a name, a pin, a floor or the
//! retired keys is judged again on every use.
//!
//! Each fresh read of a vault's state (`Reads`, `config`, a floor, a held copy) read the held
//! vault whole: 26 times in one five-minute census refresh of a Linux arm64 node whose held vault
//! is 1.18 MB (30.8 of the refresh's 32.4 MB), and five times at each deny-list read the node's
//! watch makes again after a vault round (2026-11-09), of a file that had not changed.

use super::{Floor, Pin, SecretBytes, Verdict, envelope, fsx};
use crate::{Error, Result, envelope::Envelope, fsx::FileStamp};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

const MAX_ENTRIES: usize = 16;
const MAX_CACHED_BYTES: usize = 2 * envelope::MAX_ENVELOPE;
/// The most files an entry is known by ([`Entry::stamps`]): two vault folders that hold the same
/// bytes in one process (two Macs of a test's lab) each keep theirs.
const STAMPS: usize = 4;

static CACHE: Cache = Cache::new();

/// The vault file at `path` (the held vault, [`Paths::cache_file`](super::Paths::cache_file)),
/// admitted and bounded as [`fsx::read_private`] reads it at [`envelope::MAX_ENVELOPE`], parsed:
/// `None` where there is none. Where the opened file's stamp is one an entry's bytes were read
/// under, that entry, read from no byte of the file; otherwise the file read whole, and its
/// entry known by that stamp where the file showed the same stamp after the read as before it.
///
/// # Errors
/// As [`fsx::read_private`].
pub(super) fn held(path: &Path) -> Result<Option<Arc<Parsed>>> {
    CACHE.held(path)
}

/// What one exact input to [`envelope::parse`] gave. It holds no authority:
/// [`Self::verify_for_reader`] asks [`envelope::judge_for_reader`] anew for every vault, pin and
/// floor, and [`Self::chain`] checks only the signer and the signature, so what it returns is
/// lineage no name, floor or retired key has judged.
pub(super) struct Parsed {
    input: Input,
    signature: OnceLock<Result<()>>,
    prior_keys: OnceLock<Vec<[u8; 32]>>,
    /// How many times this entry verified its signature.
    #[cfg(test)]
    signatures: std::sync::atomic::AtomicUsize,
}

enum Input {
    Envelope(Box<Envelope>),
    Refused(SecretBytes, Error),
}

impl Parsed {
    fn new(bytes: SecretBytes) -> Self {
        Self {
            input: match envelope::parse(bytes.as_slice()) {
                Ok(envelope) => Input::Envelope(Box::new(envelope)),
                Err(error) => Input::Refused(bytes, error),
            },
            signature: OnceLock::new(),
            prior_keys: OnceLock::new(),
            #[cfg(test)]
            signatures: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The bytes it was parsed from.
    pub(super) fn bytes(&self) -> &[u8] {
        match &self.input {
            Input::Envelope(envelope) => envelope.raw(),
            Input::Refused(bytes, _) => bytes.as_slice(),
        }
    }

    /// What this entry holds: an envelope's raw bytes and the lockers parsed from them, or the
    /// refused input.
    fn charge(&self) -> usize {
        let held = self.bytes().len();
        match self.input {
            Input::Envelope(_) => held.saturating_mul(2),
            Input::Refused(..) => held,
        }
    }

    /// The parsed envelope, or [`envelope::parse`]'s error.
    pub(super) fn envelope(&self) -> Result<&Envelope> {
        match &self.input {
            Input::Envelope(envelope) => Ok(envelope),
            Input::Refused(_, error) => Err(error.clone()),
        }
    }

    fn signature(&self, envelope: &Envelope) -> Result<()> {
        self.signature
            .get_or_init(|| {
                #[cfg(test)]
                self.signatures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                envelope.verify_signature()
            })
            .clone()
    }

    /// How many times this entry verified its signature, whichever thread asked.
    #[cfg(test)]
    pub(super) fn signature_checks(&self) -> usize {
        self.signatures.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// [`envelope::verify_for_reader`] of these bytes.
    pub(super) fn verify_for_reader(
        &self,
        expect_name: &str,
        pin: &[u8; 32],
        followed: &[[u8; 32]],
        held: Option<&Floor>,
    ) -> Result<(&Envelope, Pin, Verdict)> {
        let envelope = self.envelope()?;
        let (pin, verdict) = envelope::judge_for_reader(
            envelope,
            expect_name,
            pin,
            followed,
            held,
            &envelope::Retired::compiled(),
            || self.signature(envelope),
        )?;
        Ok((envelope, pin, verdict))
    }

    /// [`Envelope::reached_from`] of the parsed envelope, or none where the parse failed.
    pub(super) fn prior_keys(&self) -> &[[u8; 32]] {
        self.prior_keys.get_or_init(|| match &self.input {
            Input::Envelope(envelope) => envelope.reached_from(),
            Input::Refused(..) => Vec::new(),
        })
    }

    /// The keys `pin` was reached from, where `pin` signed these bytes and the signature verifies.
    pub(super) fn chain(&self, pin: &[u8; 32]) -> Vec<[u8; 32]> {
        match self.envelope() {
            Ok(envelope) if envelope.signer == *pin && self.signature(envelope).is_ok() => {
                self.prior_keys().to_vec()
            }
            _ => Vec::new(),
        }
    }
}

struct Cache {
    entries: Mutex<VecDeque<Entry>>,
    #[cfg(test)]
    parses: std::sync::atomic::AtomicUsize,
    /// How many [`Cache::held`] reads read their file's bytes.
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
}

struct Entry {
    parsed: Arc<Parsed>,
    /// The stamps of the last [`STAMPS`] files [`Cache::held`] read these bytes from, newest last.
    stamps: VecDeque<FileStamp>,
}

impl Cache {
    const fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            parses: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn held(&self, path: &Path) -> Result<Option<Arc<Parsed>>> {
        let Some(reader) = fsx::open_private_reader(path)? else {
            return Ok(None);
        };
        let stamp = reader.stamp();
        if let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries.iter().find(|entry| entry.stamps.contains(&stamp))
        {
            return Ok(Some(entry.parsed.clone()));
        }
        #[cfg(test)]
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bytes = reader.read_private(envelope::MAX_ENVELOPE)?;
        // A file written in place during the read (no writer of it does) keeps its bytes from
        // this stamp.
        let unchanged = reader.check_unchanged().is_ok();
        Ok(Some(self.parsed(bytes, unchanged.then_some(stamp))))
    }

    /// `bytes`, parsed, from the entry of the same bytes where one is kept, which is then known by
    /// `stamp` too, where one is given.
    fn parsed(&self, bytes: SecretBytes, stamp: Option<FileStamp>) -> Arc<Parsed> {
        if let Ok(mut entries) = self.entries.lock()
            && let Some(entry) = same_bytes(&mut entries, bytes.as_slice())
        {
            if let Some(stamp) = stamp {
                know(entry, stamp);
            }
            return entry.parsed.clone();
        }
        #[cfg(test)]
        self.parses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Parsing and verifying never hold the mutex.
        let fresh = Arc::new(Parsed::new(bytes));
        if let Ok(mut entries) = self.entries.lock() {
            if let Some(entry) = same_bytes(&mut entries, fresh.bytes()) {
                if let Some(stamp) = stamp {
                    know(entry, stamp);
                }
                return entry.parsed.clone();
            }
            let charge = fresh.charge();
            while entries.len() >= MAX_ENTRIES
                || entries
                    .iter()
                    .map(|entry| entry.parsed.charge())
                    .sum::<usize>()
                    .saturating_add(charge)
                    > MAX_CACHED_BYTES
            {
                if entries.pop_front().is_none() {
                    return fresh;
                }
            }
            entries.push_back(Entry {
                parsed: fresh.clone(),
                stamps: VecDeque::new(),
            });
            if let (Some(stamp), Some(entry)) = (stamp, entries.back_mut()) {
                know(entry, stamp);
            }
        }
        fresh
    }
}

/// Know `entry` by `stamp`, newest last, letting the oldest go past [`STAMPS`].
fn know(entry: &mut Entry, stamp: FileStamp) {
    entry.stamps.retain(|known| *known != stamp);
    if entry.stamps.len() >= STAMPS {
        entry.stamps.pop_front();
    }
    entry.stamps.push_back(stamp);
}

fn same_bytes<'a>(entries: &'a mut VecDeque<Entry>, bytes: &[u8]) -> Option<&'a mut Entry> {
    entries
        .iter_mut()
        .find(|entry| entry.parsed.bytes() == bytes)
}

#[cfg(test)]
#[path = "chain_cache_tests.rs"]
mod tests;
