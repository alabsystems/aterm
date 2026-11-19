// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sorted immutable run files: the keyed lookup a table's derived files make over an
//! append-mostly set (docs/design-commons-at-scale/journal.md §2.6, "Run files"; tracker LT-E39).
//! The id index (id to record number), the chain index ((actor ordinal, sequence) to record
//! number) and the view (cell address to cell) are each one run set in a directory of their own.
//!
//! **A run** (`<sequence:016x>.run`, magic `ATRN1`) is written once and never changed:
//!
//! ```text
//! head:    "ATRN1" | kind u8 | level u8 | sequence u64 | valid_through u64 | salt (32)
//! pages:   PAGE (64 KiB) each: entry count u16, then entries in ascending key order, each
//!          shared-prefix varint | suffix length varint | value length varint | suffix | value,
//!          the prefix shared with the entry before it in the same page (the first shares none)
//! index:   the first key of each page, each a length varint and the key
//! filter:  a Bloom filter of 10 bits a key and 7 probes, keyed by the head's salt
//! foot:    SHA-256 of each page | pages u64 | entries u64 | index length u64 | filter length u64
//!          | HMAC-SHA-256 over the head, index, filter, page hashes and those four counts
//! ```
//!
//! Opening a run checks the HMAC once; it keeps the index, filter and page hashes in memory. A
//! lookup asks the filter, finds the one page whose first key is the greatest at or below the key
//! by the index, reads that page alone and checks it against its hash. So a lookup reads at most
//! one page of each run, and none of a run whose filter says no (`Run::pages_read` counts them).
//!
//! **The delta** (`<sequence:016x>.delta`, magic `ATRD1`) collects new entries as appended frames,
//! each carrying a running HMAC over the one before it. When it passes [`Policy::delta_bytes`]
//! (4 MiB unless the machine's policy says otherwise) it is written as a level-0 run and a new
//! delta starts. When a level holds [`FANOUT`] runs they merge into one of the next level; an
//! equal key keeps the newer run's value. Newer data always sits in a run of a higher sequence,
//! so a lookup asks the delta and then the runs from the highest sequence down, and stops at the
//! first that holds the key.
//!
//! **A mark** ([`Runs::mark`]) makes the delta and every run it names durable and replaces the
//! set's manifest (`manifest`, magic `ATRM1`, HMAC-SHA-256 over its body) by rename. The manifest
//! names each live run's sequence, level, length, entry count and foot hash, and the delta's
//! length and running HMAC there; [`Manifest::root`] is the SHA-256 the journal's mark (`ATMK1`)
//! holds for this component. A [`Reader`] needs no lock: it reads the newest manifest, reads the
//! delta to the length that manifest names and checks its running HMAC, and opens each run the
//! first time a lookup reaches it.
//!
//! **Grace.** A run or delta that a merge or flush superseded is removed only after two later
//! marks and after [`Policy::grace_ms`] (`reader_grace`, 60 seconds unless the machine's policy
//! says otherwise) counted from the first mark that no longer names it, the last moment a new
//! reader could still have read a manifest naming it. At the design rate a mark is written about
//! once a second, so the first design's rule (two marks alone) took a reader's files after two
//! seconds and every longer read lost them and started again (the journal design's attack B7).
//! A read that lasts longer than the grace finds a file gone and opens the newest mark again.
//!
//! The key that authenticates every file here is the state key, derived by the caller from the
//! device seed and the table's scope with a domain of its own; this module takes it as bytes.

use crate::{Error, Result};
use ring::{digest, hmac, rand::SecureRandom};
use std::cell::{Cell, OnceCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The size of one run page.
pub(crate) const PAGE: usize = 64 * 1024;
/// The delta's size at which it is written as a level-0 run, unless the policy says otherwise.
pub(crate) const DELTA_BYTES: u64 = 4 * 1024 * 1024;
/// The count of runs of one level that merge into one of the next.
pub(crate) const FANOUT: usize = 4;
/// `reader_grace`: how long a superseded file stays after the first mark that no longer names it,
/// unless the policy says otherwise.
pub(crate) const READER_GRACE_MS: u64 = 60_000;
/// The count of later marks a superseded file outlives, besides the grace.
pub(crate) const MARKS_KEPT: u64 = 2;
/// The largest key and value together, so that one entry always fits a page.
pub(crate) const MAX_ENTRY: usize = 4096;

const FILTER_BITS_PER_KEY: u64 = 10;
const FILTER_PROBES: u64 = 7;
const RUN_MAGIC: &[u8; 5] = b"ATRN1";
const DELTA_MAGIC: &[u8; 5] = b"ATRD1";
const MANIFEST_MAGIC: &[u8; 5] = b"ATRM1";
const HEAD_LEN: usize = 5 + 1 + 1 + 8 + 8 + 32;
const MAC_LEN: usize = 32;
const COUNTS_LEN: usize = 4 * 8;
const TRAILER_LEN: usize = COUNTS_LEN + MAC_LEN;
const DELTA_HEAD_LEN: usize = 5 + 1 + 8;
const RUN_DOMAIN: &[u8] = b"aterm-db derived run ATRN1\0";
const DELTA_DOMAIN: &[u8] = b"aterm-db derived delta ATRD1\0";
const MANIFEST_DOMAIN: &[u8] = b"aterm-db derived manifest ATRM1\0";
const ROOT_DOMAIN: &[u8] = b"aterm-db derived run set root\0";
const MANIFEST: &str = "manifest";
const PART: &str = ".part";
const RUN_SUFFIX: &str = ".run";
const DELTA_SUFFIX: &str = ".delta";

/// Which derived component a run set holds; written in every head so a file of one component
/// cannot stand for another's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `state/ids`: record id to record number.
    Ids,
    /// `state/chains`: (actor ordinal, sequence) to record number.
    Chains,
    // `state/view` (byte 3) comes with the view's sealed pages (LT-E42).
}

impl Kind {
    fn byte(self) -> u8 {
        match self {
            Self::Ids => 1,
            Self::Chains => 2,
        }
    }
}

/// The key every run, delta and manifest of a run set is authenticated under.
#[derive(Clone)]
pub(crate) struct StateKey(hmac::Key);

impl StateKey {
    pub(crate) fn new(bytes: &[u8; 32]) -> Self {
        Self(hmac::Key::new(hmac::HMAC_SHA256, bytes))
    }

    fn mac(&self, parts: &[&[u8]]) -> [u8; MAC_LEN] {
        let mut context = hmac::Context::with_key(&self.0);
        for part in parts {
            context.update(part);
        }
        let mut out = [0; MAC_LEN];
        out.copy_from_slice(context.sign().as_ref());
        out
    }
}

/// The machine's policy for one run set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Policy {
    /// The delta's size at which it is written as a level-0 run.
    pub(crate) delta_bytes: u64,
    /// `reader_grace`, in milliseconds.
    pub(crate) grace_ms: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            delta_bytes: DELTA_BYTES,
            grace_ms: READER_GRACE_MS,
        }
    }
}

/// One run as a manifest names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunRef {
    pub(crate) sequence: u64,
    pub(crate) level: u8,
    /// The journal offset the run's entries are valid through.
    pub(crate) valid_through: u64,
    pub(crate) entries: u64,
    pub(crate) bytes: u64,
    /// SHA-256 over the run's page hashes and trailer.
    pub(crate) foot: [u8; 32],
}

/// The delta as a manifest names it: read to `len`, its running HMAC there is `mac`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeltaRef {
    pub(crate) sequence: u64,
    pub(crate) len: u64,
    pub(crate) mac: [u8; 32],
}

/// The run set at one mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub(crate) mark: u64,
    pub(crate) at_ms: u64,
    pub(crate) next_sequence: u64,
    /// In ascending sequence order.
    pub(crate) runs: Vec<RunRef>,
    pub(crate) delta: DeltaRef,
}

impl Manifest {
    fn body(&self, kind: Kind) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.runs.len() * 65);
        out.extend_from_slice(MANIFEST_MAGIC);
        out.push(kind.byte());
        out.extend_from_slice(&self.mark.to_le_bytes());
        out.extend_from_slice(&self.at_ms.to_le_bytes());
        out.extend_from_slice(&self.next_sequence.to_le_bytes());
        out.extend_from_slice(&(self.runs.len() as u64).to_le_bytes());
        for run in &self.runs {
            out.extend_from_slice(&run.sequence.to_le_bytes());
            out.push(run.level);
            out.extend_from_slice(&run.valid_through.to_le_bytes());
            out.extend_from_slice(&run.entries.to_le_bytes());
            out.extend_from_slice(&run.bytes.to_le_bytes());
            out.extend_from_slice(&run.foot);
        }
        out.extend_from_slice(&self.delta.sequence.to_le_bytes());
        out.extend_from_slice(&self.delta.len.to_le_bytes());
        out.extend_from_slice(&self.delta.mac);
        out
    }

    /// SHA-256 over the file names, lengths and foot hashes this mark names: the value the
    /// journal's mark holds for this component.
    pub(crate) fn root(&self, kind: Kind) -> [u8; 32] {
        sha256(&[ROOT_DOMAIN, &self.body(kind)])
    }

    fn read(dir: &Path, key: &StateKey, kind: Kind) -> Result<Option<Self>> {
        let bytes = match fs::read(dir.join(MANIFEST)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let damaged = || Error::new("a derived run set's manifest is damaged");
        let split = bytes.len().checked_sub(MAC_LEN).ok_or_else(damaged)?;
        let (body, mac) = bytes.split_at(split);
        if !same(&key.mac(&[MANIFEST_DOMAIN, body]), mac) {
            return Err(damaged());
        }
        let mut at = Bytes { bytes: body, at: 0 };
        if at.take(5).ok_or_else(damaged)? != MANIFEST_MAGIC
            || at.u8().ok_or_else(damaged)? != kind.byte()
        {
            return Err(damaged());
        }
        let mark = at.u64().ok_or_else(damaged)?;
        let at_ms = at.u64().ok_or_else(damaged)?;
        let next_sequence = at.u64().ok_or_else(damaged)?;
        let count = at.u64().ok_or_else(damaged)?;
        let mut runs = Vec::new();
        for _ in 0..count {
            runs.push(RunRef {
                sequence: at.u64().ok_or_else(damaged)?,
                level: at.u8().ok_or_else(damaged)?,
                valid_through: at.u64().ok_or_else(damaged)?,
                entries: at.u64().ok_or_else(damaged)?,
                bytes: at.u64().ok_or_else(damaged)?,
                foot: at.array().ok_or_else(damaged)?,
            });
        }
        let delta = DeltaRef {
            sequence: at.u64().ok_or_else(damaged)?,
            len: at.u64().ok_or_else(damaged)?,
            mac: at.array().ok_or_else(damaged)?,
        };
        if at.at != body.len() {
            return Err(damaged());
        }
        Ok(Some(Self {
            mark,
            at_ms,
            next_sequence,
            runs,
            delta,
        }))
    }

    fn write(&self, dir: &Path, key: &StateKey, kind: Kind) -> Result<()> {
        let body = self.body(kind);
        let mac = key.mac(&[MANIFEST_DOMAIN, &body]);
        let part = dir.join(format!("{MANIFEST}{PART}"));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&part)?;
        file.write_all(&body)?;
        file.write_all(&mac)?;
        aterm_durable::durable(&file)?;
        fs::rename(&part, dir.join(MANIFEST))?;
        aterm_durable::durable_dir(dir)?;
        Ok(())
    }
}

/// The file name of run `sequence`.
pub(crate) fn run_name(sequence: u64) -> String {
    format!("{sequence:016x}{RUN_SUFFIX}")
}

/// The file name of delta `sequence`.
pub(crate) fn delta_name(sequence: u64) -> String {
    format!("{sequence:016x}{DELTA_SUFFIX}")
}

fn sequence_of(name: &str) -> Option<u64> {
    let stem = name
        .strip_suffix(RUN_SUFFIX)
        .or_else(|| name.strip_suffix(DELTA_SUFFIX))?;
    if stem.len() != 16 {
        return None;
    }
    u64::from_str_radix(stem, 16).ok()
}

/// A superseded file kept for readers of older marks.
#[derive(Debug)]
struct Retired {
    name: String,
    /// The last mark that named the file.
    last_named: u64,
    /// When the first mark that no longer names it was written.
    retired_ms: Option<u64>,
}

struct Delta {
    sequence: u64,
    file: File,
    len: u64,
    mac: [u8; 32],
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
    through: u64,
}

/// The writer of one run set: the owner process's, under the table's lease.
pub(crate) struct Runs {
    dir: PathBuf,
    key: StateKey,
    kind: Kind,
    policy: Policy,
    /// In ascending sequence order.
    live: Vec<RunRef>,
    delta: Delta,
    next_sequence: u64,
    mark: u64,
    /// The sequences the newest manifest names.
    named: BTreeSet<u64>,
    retired: Vec<Retired>,
    /// The delta's file holds a partial frame it could not cut off.
    broken: bool,
}

impl Runs {
    /// Opens the run set in `dir`, or starts an empty one. A delta frame torn by a crash after the
    /// last mark is cut off; the journal records since the mark are replayed into it again. A
    /// file the manifest does not name is kept as superseded, for the readers of older marks.
    ///
    /// # Errors
    /// The directory could not be read or written, or the manifest or the delta it names is
    /// damaged (the caller rebuilds the component from primary files).
    pub(crate) fn open(
        dir: &Path,
        key: StateKey,
        kind: Kind,
        policy: Policy,
        now_ms: u64,
    ) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let manifest = Manifest::read(dir, &key, kind)?;
        let mark = manifest.as_ref().map_or(0, |manifest| manifest.mark);
        let named: BTreeSet<u64> = manifest
            .as_ref()
            .map(|manifest| {
                let runs = manifest.runs.iter().map(|run| run.sequence);
                runs.chain([manifest.delta.sequence]).collect()
            })
            .unwrap_or_default();
        // A sequence at or above the manifest's next was issued after its mark, so no mark named it
        // and no reader holds it; it is issued again below.
        let unissued = manifest
            .as_ref()
            .map_or(0, |manifest| manifest.next_sequence);
        let mut retired = Vec::new();
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(PART) {
                remove(&dir.join(&name))?;
                continue;
            }
            let Some(sequence) = sequence_of(&name) else {
                continue;
            };
            if named.contains(&sequence) {
                continue;
            }
            if sequence >= unissued {
                remove(&dir.join(&name))?;
            } else {
                // A reader of an older mark may hold it.
                retired.push(Retired {
                    name,
                    last_named: mark,
                    retired_ms: Some(now_ms),
                });
            }
        }
        let (live, next_sequence, delta) = match manifest {
            Some(manifest) => (
                manifest.runs,
                manifest.next_sequence,
                reopen_delta(dir, &key, kind, manifest.delta)?,
            ),
            None => (Vec::new(), 2, new_delta(dir, &key, kind, 1)?),
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            key,
            kind,
            policy,
            live,
            delta,
            next_sequence,
            mark,
            named,
            retired,
            broken: false,
        })
    }

    /// Writes a run straight from sorted entries, as a rebuild from primary files does, into a set
    /// that holds nothing yet. `entries` bounds the count the iterator yields.
    ///
    /// # Errors
    /// The set already holds entries, the entries are not strictly ascending or pass `entries`,
    /// an entry passes [`MAX_ENTRY`], or the run could not be written.
    pub(crate) fn rebuild<I>(
        &mut self,
        level: u8,
        valid_through: u64,
        entries: u64,
        sorted: I,
    ) -> Result<()>
    where
        I: IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    {
        if !self.live.is_empty() || !self.delta.entries.is_empty() {
            return Err(Error::new(
                "a derived run set is rebuilt only while it is empty",
            ));
        }
        let sequence = self.allocate();
        let mut writer = RunWriter::create(
            &self.dir,
            &self.key,
            self.kind,
            level,
            sequence,
            valid_through,
            entries,
        )?;
        for (key, value) in sorted {
            writer.push(&key, &value)?;
        }
        let run = writer.finish()?;
        self.live.push(run);
        self.cascade()
    }

    /// Appends `entries` to the delta as one frame, valid through journal offset `through`. A key
    /// already present takes the new value. Writes the delta as a level-0 run once it passes the
    /// policy's size, and merges each level that reaches [`FANOUT`] runs.
    ///
    /// # Errors
    /// An entry passes [`MAX_ENTRY`], or a file could not be written.
    pub(crate) fn insert(&mut self, through: u64, entries: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
        let mut body = Vec::with_capacity(
            16 + entries
                .iter()
                .map(|(k, v)| k.len() + v.len() + 4)
                .sum::<usize>(),
        );
        body.extend_from_slice(&through.to_le_bytes());
        put_varint(&mut body, entries.len() as u64);
        for (key, value) in entries {
            if key.len() + value.len() > MAX_ENTRY {
                return Err(Error::new("a derived run entry passes its bound"));
            }
            put_varint(&mut body, key.len() as u64);
            body.extend_from_slice(key);
            put_varint(&mut body, value.len() as u64);
            body.extend_from_slice(value);
        }
        let len = u32::try_from(body.len())
            .map_err(|_| Error::new("a derived delta frame passes its bound"))?;
        let mac = self
            .key
            .mac(&[DELTA_DOMAIN, &self.delta.mac, &len.to_le_bytes(), &body]);
        let mut frame = Vec::with_capacity(4 + body.len() + MAC_LEN);
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&body);
        frame.extend_from_slice(&mac);
        self.usable()?;
        if let Err(error) = self.delta.file.write_all(&frame) {
            // A frame cut short would sit under every later one: cut the file back to the last whole
            // frame, or refuse every later write if that fails too.
            if self.delta.file.set_len(self.delta.len).is_err() {
                self.broken = true;
            }
            return Err(error.into());
        }
        self.delta.len += frame.len() as u64;
        self.delta.mac = mac;
        self.delta.through = self.delta.through.max(through);
        for (key, value) in entries {
            self.delta.entries.insert(key.clone(), value.clone());
        }
        if self.delta.len >= self.policy.delta_bytes {
            self.flush()?;
        }
        Ok(())
    }

    /// Writes the delta as a level-0 run and starts a new delta, then merges.
    ///
    /// # Errors
    /// A file could not be written or a merged run did not open.
    pub(crate) fn flush(&mut self) -> Result<()> {
        if self.delta.entries.is_empty() {
            return Ok(());
        }
        let sequence = self.allocate();
        let count = self.delta.entries.len() as u64;
        let mut writer = RunWriter::create(
            &self.dir,
            &self.key,
            self.kind,
            0,
            sequence,
            self.delta.through,
            count,
        )?;
        for (key, value) in &self.delta.entries {
            writer.push(key, value)?;
        }
        let run = writer.finish()?;
        self.live.push(run);
        let next = self.allocate();
        let old = std::mem::replace(
            &mut self.delta,
            new_delta(&self.dir, &self.key, self.kind, next)?,
        );
        self.retire(old.sequence, delta_name(old.sequence))?;
        self.cascade()
    }

    fn cascade(&mut self) -> Result<()> {
        loop {
            let mut levels: BTreeMap<u8, usize> = BTreeMap::new();
            for run in &self.live {
                *levels.entry(run.level).or_default() += 1;
            }
            let Some((&level, _)) = levels.iter().find(|(_, count)| **count >= FANOUT) else {
                return Ok(());
            };
            let next_level = level
                .checked_add(1)
                .ok_or_else(|| Error::new("a derived run set has no level left"))?;
            // Newest first, so an equal key keeps the newer run's value.
            let mut inputs: Vec<RunRef> = self
                .live
                .iter()
                .filter(|run| run.level == level)
                .copied()
                .collect();
            inputs.sort_by_key(|run| std::cmp::Reverse(run.sequence));
            let opened = inputs
                .iter()
                .map(|run| Run::open(&self.dir, &self.key, self.kind, run))
                .collect::<Result<Vec<_>>>()?;
            let bound = inputs.iter().map(|run| run.entries).sum();
            let through = inputs
                .iter()
                .map(|run| run.valid_through)
                .max()
                .unwrap_or(0);
            let sequence = self.allocate();
            let mut writer = RunWriter::create(
                &self.dir, &self.key, self.kind, next_level, sequence, through, bound,
            )?;
            let mut cursors = opened
                .iter()
                .map(RunCursor::new)
                .collect::<Result<Vec<_>>>()?;
            loop {
                let mut least: Option<usize> = None;
                for (at, cursor) in cursors.iter().enumerate() {
                    let Some((key, _)) = cursor.peek() else {
                        continue;
                    };
                    if least
                        .is_none_or(|best| key < cursors[best].peek().map_or(&[][..], |(k, _)| k))
                    {
                        least = Some(at);
                    }
                }
                let Some(best) = least else { break };
                let (key, value) = cursors[best].take()?;
                for cursor in &mut cursors {
                    while cursor.peek().is_some_and(|(k, _)| k == key.as_slice()) {
                        cursor.take()?;
                    }
                }
                writer.push(&key, &value)?;
            }
            let merged = writer.finish()?;
            self.live.retain(|run| run.level != level);
            self.live.push(merged);
            for run in inputs {
                self.retire(run.sequence, run_name(run.sequence))?;
            }
        }
    }

    fn retire(&mut self, sequence: u64, name: String) -> Result<()> {
        if self.named.contains(&sequence) {
            self.retired.push(Retired {
                name,
                last_named: self.mark,
                retired_ms: None,
            });
            Ok(())
        } else {
            // No manifest ever named it, so no reader can hold it.
            remove(&self.dir.join(name))
        }
    }

    fn usable(&self) -> Result<()> {
        if self.broken {
            return Err(Error::new(
                "a derived delta holds a frame it could not cut off; rebuild the component",
            ));
        }
        Ok(())
    }

    fn allocate(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }

    /// Makes the delta and every live run durable, writes the next manifest, and removes the
    /// superseded files whose two later marks and grace have passed. Returns the manifest's root.
    ///
    /// # Errors
    /// A flush or the manifest's write failed.
    pub(crate) fn mark(&mut self, now_ms: u64) -> Result<[u8; 32]> {
        self.usable()?;
        aterm_durable::durable_data(&self.delta.file)?;
        aterm_durable::durable_dir(&self.dir)?;
        self.mark += 1;
        let manifest = Manifest {
            mark: self.mark,
            at_ms: now_ms,
            next_sequence: self.next_sequence,
            runs: self.live.clone(),
            delta: DeltaRef {
                sequence: self.delta.sequence,
                len: self.delta.len,
                mac: self.delta.mac,
            },
        };
        manifest.write(&self.dir, &self.key, self.kind)?;
        for retired in &mut self.retired {
            retired.retired_ms.get_or_insert(now_ms);
        }
        self.named = self.live.iter().map(|run| run.sequence).collect();
        self.named.insert(self.delta.sequence);
        self.collect(now_ms)?;
        Ok(manifest.root(self.kind))
    }

    /// Removes each superseded file that two later marks have passed and whose grace, counted
    /// from the first mark that no longer named it, has run out. Returns the count removed.
    ///
    /// # Errors
    /// A file could not be removed.
    pub(crate) fn collect(&mut self, now_ms: u64) -> Result<usize> {
        let (mark, grace) = (self.mark, self.policy.grace_ms);
        let mut removed = 0;
        let mut kept = Vec::with_capacity(self.retired.len());
        for retired in std::mem::take(&mut self.retired) {
            let marks_passed = mark >= retired.last_named.saturating_add(MARKS_KEPT);
            let grace_passed = retired
                .retired_ms
                .is_some_and(|at| now_ms.saturating_sub(at) >= grace);
            if marks_passed && grace_passed {
                remove(&self.dir.join(&retired.name))?;
                removed += 1;
            } else {
                kept.push(retired);
            }
        }
        self.retired = kept;
        Ok(removed)
    }

    /// The number of the newest mark written.
    pub(crate) fn mark_number(&self) -> u64 {
        self.mark
    }

    /// The live runs, in ascending sequence order.
    pub(crate) fn live(&self) -> &[RunRef] {
        &self.live
    }

    /// The count of entries in the delta, not yet in a run.
    pub(crate) fn pending(&self) -> usize {
        self.delta.entries.len()
    }
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

fn delta_head(kind: Kind, sequence: u64) -> [u8; DELTA_HEAD_LEN] {
    let mut head = [0; DELTA_HEAD_LEN];
    head[..5].copy_from_slice(DELTA_MAGIC);
    head[5] = kind.byte();
    head[6..].copy_from_slice(&sequence.to_le_bytes());
    head
}

fn new_delta(dir: &Path, key: &StateKey, kind: Kind, sequence: u64) -> Result<Delta> {
    let head = delta_head(kind, sequence);
    let mut file = OpenOptions::new()
        .append(true)
        .create_new(true)
        .open(dir.join(delta_name(sequence)))?;
    file.write_all(&head)?;
    Ok(Delta {
        sequence,
        file,
        len: DELTA_HEAD_LEN as u64,
        mac: key.mac(&[DELTA_DOMAIN, &head]),
        entries: BTreeMap::new(),
        through: 0,
    })
}

/// What a walk over a delta's frames found.
struct Walked {
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
    through: u64,
    /// The end of the last frame that verified.
    len: u64,
    mac: [u8; 32],
    /// The running HMAC at offset `at`, if a frame ended there.
    mac_at: Option<[u8; 32]>,
}

/// Walks `bytes`, a delta file's prefix, frame by frame, verifying each frame's running HMAC, and
/// stops at the first frame that is short or does not verify.
fn walk_delta(bytes: &[u8], key: &StateKey, kind: Kind, sequence: u64, at: u64) -> Result<Walked> {
    let head = delta_head(kind, sequence);
    if bytes.get(..DELTA_HEAD_LEN) != Some(&head[..]) {
        return Err(Error::new("a derived delta's head is damaged"));
    }
    let mut walked = Walked {
        entries: BTreeMap::new(),
        through: 0,
        len: DELTA_HEAD_LEN as u64,
        mac: key.mac(&[DELTA_DOMAIN, &head]),
        mac_at: None,
    };
    let mut offset = DELTA_HEAD_LEN;
    loop {
        if offset as u64 == at {
            walked.mac_at = Some(walked.mac);
        }
        let Some(len) = bytes.get(offset..offset + 4) else {
            break;
        };
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let body_at = offset + 4;
        let Some(body) = bytes.get(body_at..body_at + len) else {
            break;
        };
        let Some(mac) = bytes.get(body_at + len..body_at + len + MAC_LEN) else {
            break;
        };
        let expected = key.mac(&[DELTA_DOMAIN, &walked.mac, &bytes[offset..body_at], body]);
        if !same(&expected, mac) {
            break;
        }
        let mut cursor = Bytes { bytes: body, at: 0 };
        let damaged = || Error::new("a derived delta frame does not decode");
        let through = cursor.u64().ok_or_else(damaged)?;
        let count = cursor.varint().ok_or_else(damaged)?;
        for _ in 0..count {
            let key_len = cursor.varint().ok_or_else(damaged)?;
            let entry_key = cursor
                .take(usize::try_from(key_len).map_err(|_| damaged())?)
                .ok_or_else(damaged)?;
            let value_len = cursor.varint().ok_or_else(damaged)?;
            let value = cursor
                .take(usize::try_from(value_len).map_err(|_| damaged())?)
                .ok_or_else(damaged)?;
            walked.entries.insert(entry_key.to_vec(), value.to_vec());
        }
        walked.through = walked.through.max(through);
        walked.mac = expected;
        offset = body_at + len + MAC_LEN;
        walked.len = offset as u64;
    }
    Ok(walked)
}

fn reopen_delta(dir: &Path, key: &StateKey, kind: Kind, named: DeltaRef) -> Result<Delta> {
    let path = dir.join(delta_name(named.sequence));
    let bytes = fs::read(&path)?;
    let walked = walk_delta(&bytes, key, kind, named.sequence, named.len)?;
    if walked.mac_at.is_none_or(|mac| mac != named.mac) {
        return Err(Error::new(
            "a derived delta does not reach the point its mark names",
        ));
    }
    let file = OpenOptions::new().append(true).open(&path)?;
    if walked.len < bytes.len() as u64 {
        file.set_len(walked.len)?;
    }
    Ok(Delta {
        sequence: named.sequence,
        file,
        len: walked.len,
        mac: walked.mac,
        entries: walked.entries,
        through: walked.through,
    })
}

struct RunWriter<'a> {
    file: File,
    part: PathBuf,
    path: PathBuf,
    key: &'a StateKey,
    head: [u8; HEAD_LEN],
    salt: [u8; 32],
    page: Vec<u8>,
    page_entries: u16,
    previous: Vec<u8>,
    index: Vec<u8>,
    hashes: Vec<u8>,
    pages: u64,
    filter: Vec<u8>,
    entries: u64,
    bound: u64,
    scratch: Vec<u8>,
    run: RunRef,
}

impl<'a> RunWriter<'a> {
    fn create(
        dir: &Path,
        key: &'a StateKey,
        kind: Kind,
        level: u8,
        sequence: u64,
        valid_through: u64,
        bound: u64,
    ) -> Result<Self> {
        let mut salt = [0; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| Error::new("no randomness for a derived run's salt"))?;
        let mut head = [0; HEAD_LEN];
        head[..5].copy_from_slice(RUN_MAGIC);
        head[5] = kind.byte();
        head[6] = level;
        head[7..15].copy_from_slice(&sequence.to_le_bytes());
        head[15..23].copy_from_slice(&valid_through.to_le_bytes());
        head[23..].copy_from_slice(&salt);
        let path = dir.join(run_name(sequence));
        let part = dir.join(format!("{}{PART}", run_name(sequence)));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&part)?;
        file.write_all(&head)?;
        let bits = bound
            .saturating_mul(FILTER_BITS_PER_KEY)
            .max(64)
            .next_multiple_of(8);
        let filter_len = usize::try_from(bits / 8)
            .map_err(|_| Error::new("a derived run's filter passes its bound"))?;
        Ok(Self {
            file,
            part,
            path,
            key,
            head,
            salt,
            page: Vec::with_capacity(PAGE),
            page_entries: 0,
            previous: Vec::new(),
            index: Vec::new(),
            hashes: Vec::new(),
            pages: 0,
            filter: vec![0; filter_len],
            entries: 0,
            bound,
            scratch: Vec::new(),
            run: RunRef {
                sequence,
                level,
                valid_through,
                entries: 0,
                bytes: 0,
                foot: [0; 32],
            },
        })
    }

    fn push(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        if key.len() + value.len() > MAX_ENTRY {
            return Err(Error::new("a derived run entry passes its bound"));
        }
        if self.entries > 0 && key <= self.previous.as_slice() {
            return Err(Error::new(
                "a derived run's entries are not strictly ascending",
            ));
        }
        if self.entries == self.bound {
            return Err(Error::new(
                "a derived run holds more entries than it was sized for",
            ));
        }
        self.entries += 1;
        let bits = self.filter.len() as u64 * 8;
        for probe in probes(&self.salt, key, bits) {
            self.filter[(probe / 8) as usize] |= 1 << (probe % 8);
        }
        let mut shared = if self.page_entries == 0 {
            0
        } else {
            shared_prefix(&self.previous, key)
        };
        self.encode(shared, key, value);
        if self.page_entries > 0 && self.page.len() + self.scratch.len() > PAGE {
            self.finish_page()?;
            shared = 0;
            self.encode(shared, key, value);
        }
        if self.page_entries == 0 {
            self.page.extend_from_slice(&[0, 0]);
            put_varint(&mut self.index, key.len() as u64);
            self.index.extend_from_slice(key);
        }
        self.page.extend_from_slice(&self.scratch);
        self.page_entries += 1;
        self.previous.clear();
        self.previous.extend_from_slice(key);
        Ok(())
    }

    fn encode(&mut self, shared: usize, key: &[u8], value: &[u8]) {
        self.scratch.clear();
        put_varint(&mut self.scratch, shared as u64);
        put_varint(&mut self.scratch, (key.len() - shared) as u64);
        put_varint(&mut self.scratch, value.len() as u64);
        self.scratch.extend_from_slice(&key[shared..]);
        self.scratch.extend_from_slice(value);
    }

    fn finish_page(&mut self) -> Result<()> {
        self.page[..2].copy_from_slice(&self.page_entries.to_le_bytes());
        self.page.resize(PAGE, 0);
        self.hashes.extend_from_slice(&sha256(&[&self.page]));
        self.file.write_all(&self.page)?;
        self.pages += 1;
        self.page.clear();
        self.page_entries = 0;
        Ok(())
    }

    fn finish(mut self) -> Result<RunRef> {
        if self.page_entries > 0 {
            self.finish_page()?;
        }
        let mut counts = [0; COUNTS_LEN];
        counts[..8].copy_from_slice(&self.pages.to_le_bytes());
        counts[8..16].copy_from_slice(&self.entries.to_le_bytes());
        counts[16..24].copy_from_slice(&(self.index.len() as u64).to_le_bytes());
        counts[24..].copy_from_slice(&(self.filter.len() as u64).to_le_bytes());
        let mac = self.key.mac(&[
            RUN_DOMAIN,
            &self.head,
            &self.index,
            &self.filter,
            &self.hashes,
            &counts,
        ]);
        self.file.write_all(&self.index)?;
        self.file.write_all(&self.filter)?;
        self.file.write_all(&self.hashes)?;
        self.file.write_all(&counts)?;
        self.file.write_all(&mac)?;
        aterm_durable::durable(&self.file)?;
        fs::rename(&self.part, &self.path)?;
        self.run.entries = self.entries;
        self.run.bytes = self.file.metadata()?.len();
        self.run.foot = sha256(&[&self.hashes, &counts, &mac]);
        Ok(self.run)
    }
}

/// One opened run: its index, filter and page hashes in memory, its pages on disk.
pub(crate) struct Run {
    file: File,
    run: RunRef,
    salt: [u8; 32],
    index: Vec<Vec<u8>>,
    filter: Vec<u8>,
    hashes: Vec<[u8; 32]>,
    pages_read: Cell<u64>,
}

impl Run {
    /// Opens run `named` and checks it is the run the manifest names: its length, its HMAC and its
    /// foot hash.
    fn open(dir: &Path, key: &StateKey, kind: Kind, named: &RunRef) -> Result<Self> {
        let name = run_name(named.sequence);
        let file = match File::open(dir.join(&name)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(Error::new(&format!(
                    "derived run {name} is gone: its mark is older than the reader grace; open the newest mark"
                )));
            }
            Err(error) => return Err(error.into()),
        };
        let damaged = || Error::new(&format!("derived run {name} is damaged"));
        let len = file.metadata()?.len();
        if len != named.bytes || len < (HEAD_LEN + TRAILER_LEN) as u64 {
            return Err(damaged());
        }
        let mut trailer = [0; TRAILER_LEN];
        read_at(&file, len - TRAILER_LEN as u64, &mut trailer)?;
        let mut counts = Bytes {
            bytes: &trailer,
            at: 0,
        };
        let (pages, entries, index_len, filter_len) = (
            counts.u64().ok_or_else(damaged)?,
            counts.u64().ok_or_else(damaged)?,
            counts.u64().ok_or_else(damaged)?,
            counts.u64().ok_or_else(damaged)?,
        );
        let data_end = pages
            .checked_mul(PAGE as u64)
            .and_then(|bytes| bytes.checked_add(HEAD_LEN as u64));
        let meta_len = pages
            .checked_mul(32)
            .and_then(|hashes| hashes.checked_add(index_len))
            .and_then(|meta| meta.checked_add(filter_len));
        let (Some(data_end), Some(meta_len)) = (data_end, meta_len) else {
            return Err(damaged());
        };
        if data_end
            .checked_add(meta_len)
            .and_then(|end| end.checked_add(TRAILER_LEN as u64))
            != Some(len)
            || filter_len == 0
            || entries != named.entries
        {
            return Err(damaged());
        }
        let mut head = [0; HEAD_LEN];
        read_at(&file, 0, &mut head)?;
        let mut meta = vec![0; usize::try_from(meta_len).map_err(|_| damaged())?];
        read_at(&file, data_end, &mut meta)?;
        let (counts_bytes, mac) = trailer.split_at(COUNTS_LEN);
        let index_len = index_len as usize;
        let filter_end = index_len + filter_len as usize;
        let (index_bytes, filter, hashes) = (
            &meta[..index_len],
            &meta[index_len..filter_end],
            &meta[filter_end..],
        );
        if !same(
            &key.mac(&[RUN_DOMAIN, &head, index_bytes, filter, hashes, counts_bytes]),
            mac,
        ) || sha256(&[hashes, &trailer]) != named.foot
        {
            return Err(damaged());
        }
        let mut header = Bytes {
            bytes: &head,
            at: 0,
        };
        let fields_match = header.take(5) == Some(&RUN_MAGIC[..])
            && header.u8() == Some(kind.byte())
            && header.u8() == Some(named.level)
            && header.u64() == Some(named.sequence)
            && header.u64() == Some(named.valid_through);
        let salt = header.array().ok_or_else(damaged)?;
        if !fields_match {
            return Err(damaged());
        }
        let mut index = Vec::with_capacity(pages as usize);
        let mut cursor = Bytes {
            bytes: index_bytes,
            at: 0,
        };
        for _ in 0..pages {
            let key_len =
                usize::try_from(cursor.varint().ok_or_else(damaged)?).map_err(|_| damaged())?;
            index.push(cursor.take(key_len).ok_or_else(damaged)?.to_vec());
        }
        if cursor.at != index_bytes.len() {
            return Err(damaged());
        }
        let hashes = hashes.as_chunks::<32>().0.to_vec();
        Ok(Self {
            file,
            run: *named,
            salt,
            index,
            filter: filter.to_vec(),
            hashes,
            pages_read: Cell::new(0),
        })
    }

    /// The count of pages this run has read since it was opened.
    pub(crate) fn pages_read(&self) -> u64 {
        self.pages_read.get()
    }

    fn may_hold(&self, key: &[u8]) -> bool {
        let bits = self.filter.len() as u64 * 8;
        probes(&self.salt, key, bits)
            .all(|probe| self.filter[(probe / 8) as usize] & (1 << (probe % 8)) != 0)
    }

    fn page(&self, page: usize) -> Result<Vec<u8>> {
        let mut bytes = vec![0; PAGE];
        read_at(
            &self.file,
            HEAD_LEN as u64 + page as u64 * PAGE as u64,
            &mut bytes,
        )?;
        self.pages_read.set(self.pages_read.get() + 1);
        if self.hashes.get(page) != Some(&sha256(&[&bytes])) {
            return Err(Error::new(&format!(
                "a page of derived run {} is damaged",
                run_name(self.run.sequence)
            )));
        }
        Ok(bytes)
    }

    /// The value `key` has in this run: no page read when the filter rules it out, else the one
    /// page the index names.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if self.index.is_empty() || !self.may_hold(key) {
            return Ok(None);
        }
        let page = self.index.partition_point(|first| first.as_slice() <= key);
        let Some(page) = page.checked_sub(1) else {
            return Ok(None);
        };
        let bytes = self.page(page)?;
        let mut found = None;
        decode_page(&bytes, |entry, value| match entry.cmp(key) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Equal => {
                found = Some(value.to_vec());
                false
            }
            std::cmp::Ordering::Greater => false,
        })?;
        Ok(found)
    }
}

/// A run's entries in order, a page at a time, for a merge.
struct RunCursor<'a> {
    run: &'a Run,
    next_page: usize,
    entries: std::vec::IntoIter<(Vec<u8>, Vec<u8>)>,
    head: Option<(Vec<u8>, Vec<u8>)>,
}

impl<'a> RunCursor<'a> {
    fn new(run: &'a Run) -> Result<Self> {
        let mut cursor = Self {
            run,
            next_page: 0,
            entries: Vec::new().into_iter(),
            head: None,
        };
        cursor.advance()?;
        Ok(cursor)
    }

    fn advance(&mut self) -> Result<()> {
        loop {
            if let Some(entry) = self.entries.next() {
                self.head = Some(entry);
                return Ok(());
            }
            if self.next_page >= self.run.index.len() {
                self.head = None;
                return Ok(());
            }
            let bytes = self.run.page(self.next_page)?;
            self.next_page += 1;
            let mut decoded = Vec::new();
            decode_page(&bytes, |key, value| {
                decoded.push((key.to_vec(), value.to_vec()));
                true
            })?;
            self.entries = decoded.into_iter();
        }
    }

    fn peek(&self) -> Option<(&[u8], &[u8])> {
        self.head
            .as_ref()
            .map(|(key, value)| (key.as_slice(), value.as_slice()))
    }

    fn take(&mut self) -> Result<(Vec<u8>, Vec<u8>)> {
        let entry = self
            .head
            .take()
            .ok_or_else(|| Error::new("a derived run cursor is past its end"))?;
        self.advance()?;
        Ok(entry)
    }
}

/// Where a lookup found its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Source {
    Delta,
    /// The run of this sequence.
    Run(u64),
}

/// A key's value and where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) value: Vec<u8>,
    pub(crate) source: Source,
}

/// A reader of the run set at one mark. It takes no lock: runs never change, the delta is read
/// to the length the mark names, and a superseded file stays for the grace.
pub(crate) struct Reader {
    dir: PathBuf,
    key: StateKey,
    kind: Kind,
    manifest: Manifest,
    delta: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Newest first; each opened the first time a lookup reaches it.
    runs: Vec<(RunRef, OnceCell<Run>)>,
}

impl Reader {
    /// Reads the newest mark of the run set in `dir`.
    ///
    /// # Errors
    /// No mark was written yet, or the manifest or the delta it names is damaged or gone.
    pub(crate) fn open(dir: &Path, key: StateKey, kind: Kind) -> Result<Self> {
        let manifest = Manifest::read(dir, &key, kind)?
            .ok_or_else(|| Error::new("a derived run set has no mark yet"))?;
        let named = manifest.delta;
        let file = File::open(dir.join(delta_name(named.sequence)))?;
        let mut bytes = vec![
            0;
            usize::try_from(named.len)
                .map_err(|_| Error::new("a derived delta passes its bound"))?
        ];
        read_at(&file, 0, &mut bytes)?;
        let walked = walk_delta(&bytes, &key, kind, named.sequence, named.len)?;
        if walked.len != named.len || walked.mac != named.mac {
            return Err(Error::new(
                "a derived delta does not reach the point its mark names",
            ));
        }
        let mut runs: Vec<(RunRef, OnceCell<Run>)> = manifest
            .runs
            .iter()
            .map(|run| (*run, OnceCell::new()))
            .collect();
        runs.sort_by_key(|(run, _)| std::cmp::Reverse(run.sequence));
        Ok(Self {
            dir: dir.to_path_buf(),
            key,
            kind,
            manifest,
            delta: walked.entries,
            runs,
        })
    }

    /// The mark this reader reads.
    pub(crate) fn mark(&self) -> u64 {
        self.manifest.mark
    }

    /// The manifest this reader reads.
    pub(crate) fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, at: usize) -> Result<&Run> {
        let (named, cell) = &self.runs[at];
        if let Some(run) = cell.get() {
            return Ok(run);
        }
        let _ = cell.set(Run::open(&self.dir, &self.key, self.kind, named)?);
        cell.get()
            .ok_or_else(|| Error::new("a derived run did not stay open"))
    }

    /// The value of `key` at this reader's mark: the delta first, then each run from the newest,
    /// reading at most one page of each.
    ///
    /// # Errors
    /// A run is gone (the read outlived the grace) or damaged.
    pub(crate) fn get(&self, key: &[u8]) -> Result<Option<Hit>> {
        if let Some(value) = self.delta.get(key) {
            return Ok(Some(Hit {
                value: value.clone(),
                source: Source::Delta,
            }));
        }
        for at in 0..self.runs.len() {
            if self.runs[at].0.entries == 0 {
                continue;
            }
            if let Some(value) = self.run(at)?.get(key)? {
                return Ok(Some(Hit {
                    value,
                    source: Source::Run(self.runs[at].0.sequence),
                }));
            }
        }
        Ok(None)
    }

    /// Per run, newest first: its sequence and the pages read from it so far.
    pub(crate) fn pages_read(&self) -> Vec<(u64, u64)> {
        self.runs
            .iter()
            .map(|(named, cell)| (named.sequence, cell.get().map_or(0, Run::pages_read)))
            .collect()
    }
}

/// Decodes a page's entries in order, calling `each` with every key and value until it says stop.
fn decode_page(page: &[u8], mut each: impl FnMut(&[u8], &[u8]) -> bool) -> Result<()> {
    let damaged = || Error::new("a derived run page does not decode");
    let mut cursor = Bytes { bytes: page, at: 0 };
    let count = cursor.take(2).ok_or_else(damaged)?;
    let count = u16::from_le_bytes([count[0], count[1]]);
    let mut key = Vec::new();
    for _ in 0..count {
        let shared =
            usize::try_from(cursor.varint().ok_or_else(damaged)?).map_err(|_| damaged())?;
        let suffix =
            usize::try_from(cursor.varint().ok_or_else(damaged)?).map_err(|_| damaged())?;
        let value_len =
            usize::try_from(cursor.varint().ok_or_else(damaged)?).map_err(|_| damaged())?;
        if shared > key.len() {
            return Err(damaged());
        }
        key.truncate(shared);
        key.extend_from_slice(cursor.take(suffix).ok_or_else(damaged)?);
        let value = cursor.take(value_len).ok_or_else(damaged)?;
        if !each(&key, value) {
            break;
        }
    }
    Ok(())
}

/// The filter's probe positions for `key`: double hashing over two salted 64-bit hashes.
fn probes(salt: &[u8; 32], key: &[u8], bits: u64) -> impl Iterator<Item = u64> {
    let seed = |at: usize| {
        u64::from_le_bytes([
            salt[at],
            salt[at + 1],
            salt[at + 2],
            salt[at + 3],
            salt[at + 4],
            salt[at + 5],
            salt[at + 6],
            salt[at + 7],
        ])
    };
    let (mut a, mut b) = (seed(0) ^ key.len() as u64, seed(8));
    for chunk in key.chunks(8) {
        let mut word = [0; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        let word = u64::from_le_bytes(word);
        a = mix(a ^ word);
        b = mix(b.wrapping_add(word).rotate_left(29));
    }
    let (first, step) = (mix(a), mix(b) | 1);
    (0..FILTER_PROBES).map(move |probe| first.wrapping_add(probe.wrapping_mul(step)) % bits)
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn shared_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value & 0x7f) as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut context = digest::Context::new(&digest::SHA256);
    for part in parts {
        context.update(part);
    }
    let mut out = [0; 32];
    out.copy_from_slice(context.finish().as_ref());
    out
}

/// Compares two tags in time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn read_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(not(unix))]
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)
    }
}

/// A bounds-checked reader over a byte slice.
struct Bytes<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|byte| byte[0])
    }

    fn u64(&mut self) -> Option<u64> {
        self.array::<8>().map(u64::from_le_bytes)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Some(out)
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7f).checked_shl(shift)?;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
