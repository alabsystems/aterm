// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One shared file on this Mac: the read every program makes, and the one way a pass records
//! (docs/DESIGN-shared-files-2026-11-11.md §4).
//!
//! - [`File::read`] reads the snapshot and the status beside it. It has no error: whatever the
//!   replica's custody does (a pending succession, no membership, a busy vault, an excluded
//!   table), the read answers what the last pass left, with the word that says how current
//!   that is. Nothing here can open custody: this crate holds no code that does.
//! - [`File::record`] is the materializer's write. Under the file's lock it replaces the
//!   snapshot only by one whose frontier admits it over the old one ([`Frontier::admits_over`])
//!   and whose bytes a read takes, records the pass's word either way (`live` only where the
//!   snapshot is that pass's read), and records nothing at all once another pass has recorded
//!   since this one opened custody. Where the old frontier cannot be read, only the newest
//!   home's read replaces the snapshot.
//!
//! A read answers this Mac's own writes too: [`File::read`] lays the intents waiting in the
//! outbox over the snapshot (`crate::outbox`), and [`File::published`] is the snapshot alone.

use crate::{
    Face, Frontier, Kept, Name, Root, Snapshot, State, Status, Why, Word,
    snapshot::{Column, MAX_BYTES, Row},
};
use aterm_digest::Sha256;
use aterm_json::Value;
use std::path::Path;

/// The most operations a file's history keeps.
pub const HISTORY: usize = 4_096;

/// One shared file under one root.
#[derive(Clone, Debug)]
pub struct File {
    root: Root,
    name: Name,
}

/// What a read answers: the status, and the snapshot when one is here.
#[derive(Clone, Debug)]
pub struct Read {
    pub status: Status,
    pub snapshot: Option<Snapshot>,
}

impl Read {
    /// The table's columns in face order; none before anything was materialized.
    #[must_use]
    pub fn columns(&self) -> &[Column] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.columns.as_slice())
    }

    /// The table's rows; none before anything was materialized.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.rows.as_slice())
    }

    /// The rows `name` names: the row with that key, else every row one of the snapshot's
    /// handle columns names exactly (an account's slug, a machine's label). No prefix, no
    /// substring, no nearest match.
    #[must_use]
    pub fn named(&self, name: &str) -> Vec<&Row> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        if let Some(row) = snapshot.rows.iter().find(|row| row.pk == name) {
            return vec![row];
        }
        for handle in &snapshot.handles {
            let rows: Vec<&Row> = snapshot
                .rows
                .iter()
                .filter(|row| row.cell(handle) == name)
                .collect();
            if !rows.is_empty() {
                return rows;
            }
        }
        Vec::new()
    }
}

fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match aterm_uds::atomic_file::read_regular(path, MAX_BYTES) {
        Ok((bytes, _)) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn sha_hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&Sha256::digest(bytes))
}

impl File {
    /// The file `name` under `root`.
    #[must_use]
    pub fn at(root: &Root, name: &Name) -> Self {
        Self {
            root: root.clone(),
            name: name.clone(),
        }
    }

    /// Its name.
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }

    /// Its root.
    #[must_use]
    pub const fn root(&self) -> &Root {
        &self.root
    }

    /// The last recorded pass's status file, when one recorded and it reads.
    #[must_use]
    pub fn state(&self) -> Option<State> {
        read_bounded(&self.root.state(&self.name))
            .ok()
            .flatten()
            .and_then(|bytes| State::decode(&bytes))
    }

    /// The count of passes that have recorded: what a pass notes before it opens custody
    /// ([`Self::record`]).
    #[must_use]
    pub fn look(&self) -> u64 {
        self.state().map_or(0, |state| state.seq)
    }

    /// What the snapshot here was projected from.
    #[must_use]
    pub fn frontier(&self) -> Option<Frontier> {
        read_bounded(&self.root.frontier(&self.name))
            .ok()
            .flatten()
            .and_then(|bytes| Frontier::decode(&bytes))
    }

    fn snapshot(&self) -> Result<Option<Snapshot>, String> {
        let Some(bytes) = read_bounded(&self.root.snapshot(&self.name))? else {
            return Ok(None);
        };
        let snapshot = Snapshot::decode(&bytes).map_err(|error| error.to_string())?;
        if snapshot.name != self.name {
            return Err("it is another file's snapshot".to_owned());
        }
        Ok(Some(snapshot))
    }

    /// Read the file: its rows and its status, with this Mac's own queued writes laid over
    /// the snapshot (a held one marked `⚠ mine ‖ theirs`), so a writer reads what it wrote
    /// before any record carries it. There is no error for the replica's state, and none for
    /// this Mac's own files either: a snapshot that cannot be read answers as one that is not
    /// there, with the reason in [`Status::problem`]. Nothing here can open custody.
    ///
    /// The word is the last recorded pass's, but `live` only for the snapshot that pass
    /// recorded: one at another revision was written by a pass that never recorded its word
    /// (it was killed between the two writes), and reads `held` (`custody`) until a pass
    /// records.
    #[must_use]
    pub fn read(&self) -> Read {
        self.read_file()
    }

    /// [`Self::read`], whole. The `SharedFile` anchor sits here rather than on a function
    /// named `read`: the step audit tells anchored functions apart by their bare names, and
    /// every writer session enters another machine's anchored `read`
    /// (`aterm_db::table::store::replay`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SharedFile",
            action = "Read",
            project = "aterm_db::file::model_tests::read_transition"
        )
    )]
    fn read_file(&self) -> Read {
        self.read_as(true)
    }

    /// The published table alone: the snapshot the last pass left, with none of this Mac's
    /// queued writes over it (`aterm file cat --published`).
    #[must_use]
    pub fn published(&self) -> Read {
        self.read_as(false)
    }

    fn read_as(&self, overlay: bool) -> Read {
        let state = self.state();
        let (snapshot, mut problem) = match self.snapshot() {
            Ok(snapshot) => (snapshot, None),
            Err(why) => (None, Some(format!("snapshot not read: {why}"))),
        };
        let outbox = self.outbox();
        if let Some(why) = &outbox.problem {
            problem = Some(match problem {
                Some(first) => format!("{first}; {why}"),
                None => why.clone(),
            });
        }
        let (queued, held) = (outbox.waiting().count() as u64, outbox.held.len() as u64);
        let recorded = state.as_ref().and_then(|state| state.word);
        let why = state.as_ref().and_then(|state| state.why);
        // The status vouches for the snapshot whose revision it records. A pass writes the
        // snapshot before the status, so one killed between the two leaves a snapshot of
        // another revision beside the last pass's word.
        let vouched = |snapshot: &Snapshot| {
            state
                .as_ref()
                .is_some_and(|state| state.rev == snapshot.rev)
        };
        let (word, why) = match (&snapshot, recorded) {
            (None, Some(Word::Off)) => (Word::Off, why),
            (None, _) => (Word::Never, why),
            // A snapshot no pass vouches for is never called live: the rows may be a fenced
            // home's, which the pass that wrote them would have recorded `held`.
            (Some(snapshot), Some(Word::Live)) if !vouched(snapshot) => {
                (Word::Held, Some(Why::Custody))
            }
            (Some(_), Some(word)) => (word, why),
            (Some(_), None) => (Word::Held, Some(Why::Custody)),
        };
        let snapshot = if overlay {
            crate::outbox::overlay(&self.name, snapshot, &outbox, || self.frontier())
        } else {
            snapshot
        };
        Read {
            status: Status {
                word,
                why,
                state,
                problem,
                queued,
                held,
            },
            snapshot,
        }
    }

    /// The file's status alone.
    #[must_use]
    pub fn status(&self) -> Status {
        self.read_as(false).status
    }

    /// The baseline a pass wrote beside the snapshot at revision `rev`: what each cell showed
    /// and the operations that were its candidates, in the materializer's own encoding.
    #[must_use]
    pub fn baseline(&self, rev: u64) -> Option<Vec<u8>> {
        read_bounded(&self.root.baseline(&self.name, rev))
            .ok()
            .flatten()
    }

    /// The face against what a pass wrote there. A face is a pass's own when its bytes are the
    /// ones the status file records, or the rendering of the snapshot beside it: a pass writes
    /// the face before the status that records it, so a pass cut short between the two leaves
    /// a face only the snapshot vouches for.
    #[must_use]
    pub fn face(&self) -> Face {
        let state = self.state();
        let mut snapshot = None;
        let ext = match state
            .as_ref()
            .map(|state| state.ext.clone())
            .filter(|ext| !ext.is_empty())
        {
            Some(ext) => ext,
            None => {
                // No status names the face's extension: the snapshot does.
                snapshot = self.snapshot().ok().flatten();
                match &snapshot {
                    Some(snapshot) => snapshot.ext.clone(),
                    None => return Face::Missing,
                }
            }
        };
        match read_bounded(&self.root.face(&self.name, &ext)) {
            Ok(None) => Face::Missing,
            Ok(Some(bytes)) => {
                let recorded = state.and_then(|state| state.face);
                if recorded.as_deref() == Some(&sha_hex(&bytes))
                    || snapshot
                        .or_else(|| self.snapshot().ok().flatten())
                        .is_some_and(|snapshot| snapshot.face().as_bytes() == bytes)
                {
                    Face::Written
                } else {
                    Face::Edited
                }
            }
            Err(_) => Face::Edited,
        }
    }

    /// The file's newest operations, oldest first: one JSON object each, value-free.
    #[must_use]
    pub fn history(&self) -> Vec<Value> {
        read_bounded(&self.root.history(&self.name))
            .ok()
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|text| {
                text.lines()
                    .filter_map(|line| aterm_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// What one materializer pass found for one file.
#[derive(Clone, Debug)]
pub enum Pass {
    /// Custody refused, or policy turns the file off: the snapshot stays as it is.
    Refused { word: Word, why: Why },
    /// Custody opened.
    Opened(Box<Opened>),
}

/// What a pass that opened custody read.
#[derive(Clone, Debug)]
pub struct Opened {
    /// The home opened is the newest of its lineage: `live`. Otherwise its writer is fenced by
    /// a succession, unfinished or completed and then invalidated, and the pass is `held`,
    /// never live.
    pub newest: bool,
    pub frontier: Frontier,
    /// The projection rules' version.
    pub projection: u32,
    /// Cells left out for a credential-shaped value.
    pub withheld: u64,
    /// The projection, or `None` where the pass saw the frontier was the snapshot's and
    /// projected nothing again.
    pub projected: Option<Projected>,
}

/// One projection of the replica.
#[derive(Clone, Debug)]
pub struct Projected {
    /// The snapshot, its revision not yet set: recording sets it.
    pub snapshot: Snapshot,
    /// The file's operations, oldest first, one JSON object each; the newest [`HISTORY`] are
    /// kept.
    pub history: Vec<Value>,
    /// The baseline of this projection, written beside the snapshot under its revision
    /// ([`File::baseline`]): the candidates a write made against this snapshot observed.
    pub baseline: Option<Vec<u8>>,
}

/// How many revisions' baselines are kept.
pub const BASELINES: usize = 8;

/// What recording a pass did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// The word was recorded; `replaced` says whether the snapshot was written.
    Recorded { replaced: bool },
    /// The pass opened custody and its projection was not taken: the snapshot stays as it was
    /// and the word recorded is `held`, never `live`.
    Kept(Kept),
    /// Another pass recorded after this one opened custody: this one recorded nothing, neither
    /// snapshot nor word.
    Raced,
}

#[cfg(any(unix, windows))]
mod write {
    use super::{
        BASELINES, File, HISTORY, MAX_BYTES, Opened, Pass, Recorded, read_bounded, sha_hex,
    };
    use crate::{Kept, Snapshot, State, Why, Word};
    use std::{
        io,
        path::Path,
        time::{Duration, Instant},
    };

    /// How long a pass waits for another that is recording the same file.
    const LOCK_WAIT: Duration = Duration::from_secs(10);

    fn private_dir(dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn put(path: &Path, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if super::cut::stops() {
            return Err(io::Error::other(
                "the test stops the pass before this write",
            ));
        }
        aterm_uds::atomic_file::write(path, bytes, 0o600)
    }

    impl File {
        /// Make this file's folders, each the owner's alone.
        pub(crate) fn make_dirs(&self) -> io::Result<()> {
            let (root, name) = (&self.root, &self.name);
            private_dir(root.path())?;
            private_dir(&root.face_dir(name))?;
            private_dir(&root.path().join(crate::layout::STATE))?;
            private_dir(&root.state_dir(name))
        }

        /// Write the baseline of revision `rev` and drop the ones older than the newest
        /// [`BASELINES`].
        fn write_baseline(&self, rev: u64, bytes: &[u8]) -> io::Result<()> {
            let dir = self.root.baselines(&self.name);
            private_dir(&dir)?;
            put(&self.root.baseline(&self.name, rev), bytes)?;
            let mut revs: Vec<u64> = std::fs::read_dir(&dir)?
                .flatten()
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .to_str()?
                        .strip_suffix(".json")?
                        .parse()
                        .ok()
                })
                .collect();
            revs.sort_unstable();
            for old in revs.iter().rev().skip(BASELINES) {
                match std::fs::remove_file(self.root.baseline(&self.name, *old)) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            }
            Ok(())
        }

        /// Record one pass, which noted [`File::look`] as `look` before it opened custody.
        ///
        /// Under the file's lock: a pass whose `look` is no longer the recorded count records
        /// nothing ([`Recorded::Raced`]), so a view of live custody taken before a fence never
        /// turns the fenced pass's `held` into `live`. A refusal leaves the snapshot as it was
        /// and records its word. An open replaces the snapshot only where its frontier admits
        /// it over the old one and its bytes are ones a read takes, keeps the revision where the
        /// content is the same, and records `live` only where the snapshot now is that read's:
        /// a projection not taken ([`Recorded::Kept`]) records `held`. The rule fails closed: a
        /// snapshot whose frontier file is missing or does not read is replaced only by the
        /// newest home's read, never by a fenced home's, and a frontier too large for the next
        /// pass to read back is not written, nor the snapshot with it. The face is written
        /// beside each snapshot written, and again where it is missing; an edited face is left
        /// alone.
        ///
        /// The writes are separate renames, in an order a pass killed between any two is safe
        /// in: the frontier first (a newer frontier beside an older snapshot only refuses
        /// more, and the next pass, whose status still names the older fingerprint, projects
        /// again), then the projection's baseline where it has one (a baseline beside an older
        /// snapshot of the same revision names candidates that snapshot's writers did not
        /// observe, which only holds a write the next pass would have let through), then the
        /// snapshot, the history and the face, and the status last. The face
        /// is recognised as a pass's own by the snapshot beside it as well as by the status, so
        /// one written just before a cut is not taken for a person's edit. A snapshot written
        /// just before a cut is at a revision the status does not record, and a read never
        /// calls it `live` ([`File::read`]): the pass cut short may have been a fenced home's.
        ///
        /// # Errors
        /// A local file or folder cannot be written, or another pass held the lock too long.
        pub fn record(&self, look: u64, pass: Pass, now_ms: u64) -> io::Result<Recorded> {
            let (root, name) = (&self.root, &self.name);
            self.make_dirs()?;
            let _lock = aterm_uds::file_lock::LockFile::take(
                &root.lock(name),
                Instant::now() + LOCK_WAIT,
                "another pass is recording this shared file",
            )?;
            let mut state = self.state().unwrap_or_default();
            if state.seq != look {
                return Ok(Recorded::Raced);
            }
            // The snapshot here before this pass writes anything.
            let held = self.snapshot().ok().flatten();
            let mut replaced = None;
            state.kept = None;
            match pass {
                Pass::Refused { word, why } => {
                    state.word = Some(word);
                    state.why = Some(why);
                }
                Pass::Opened(opened) => {
                    let Opened {
                        newest,
                        frontier,
                        projection,
                        withheld,
                        projected,
                    } = *opened;
                    // A snapshot whose frontier is not here to compare (removed, damaged,
                    // another build's) is replaced only by the newest home's read: a fenced
                    // home's cannot be shown to be no older, so it is behind.
                    let admitted = held.is_none()
                        || self
                            .frontier()
                            .map_or(newest, |old| frontier.admits_over(&old));
                    let sidecar = frontier.encode();
                    let mut written = None;
                    if !admitted {
                        state.kept = Some(Kept::Behind);
                    } else if sidecar.len() as u64 > MAX_BYTES {
                        // The next pass could not read it back, and would then compare its
                        // read with nothing.
                        state.kept = Some(Kept::TooLarge);
                    } else if let Some(projected) = projected {
                        let mut snapshot = projected.snapshot;
                        snapshot.rev = match &held {
                            Some(old) if old.content() == snapshot.content() => old.rev,
                            Some(old) => old.rev.saturating_add(1),
                            None => state.rev.saturating_add(1),
                        };
                        let bytes = snapshot.encode();
                        if bytes.len() as u64 > MAX_BYTES {
                            // A read would refuse it: the last snapshot a read takes stays.
                            state.kept = Some(Kept::TooLarge);
                        } else {
                            written =
                                Some((snapshot, bytes, projected.history, projected.baseline));
                        }
                    }
                    if state.kept.is_none() {
                        put(&root.frontier(name), &sidecar)?;
                        if let Some((snapshot, bytes, history, baseline)) = written {
                            if let Some(baseline) = baseline {
                                self.write_baseline(snapshot.rev, &baseline)?;
                            }
                            put(&root.snapshot(name), &bytes)?;
                            let skip = history.len().saturating_sub(HISTORY);
                            let mut lines = String::new();
                            for entry in &history[skip..] {
                                lines.push_str(&entry.to_string());
                                lines.push('\n');
                            }
                            put(&root.history(name), lines.as_bytes())?;
                            state.rev = snapshot.rev;
                            state.rows = snapshot.rows.len() as u64;
                            state.complete = snapshot.complete;
                            state.kind.clone_from(&snapshot.kind);
                            state.ext.clone_from(&snapshot.ext);
                            state.withheld = withheld;
                            state.projection = projection;
                            self.write_face(&mut state, &snapshot, held.as_ref())?;
                            replaced = Some(snapshot);
                        }
                        state.fingerprint = aterm_codec::hex::encode_lower(&frontier.fingerprint());
                        state.era = frontier.era;
                        state.generation = frontier.generation;
                        state.records = frontier.ids.len() as u64;
                    }
                    (state.word, state.why) = match (newest, state.kept) {
                        // A fenced home's records are held whatever became of the projection.
                        (false, _) => (Some(Word::Held), Some(Why::Succession)),
                        (true, None) => (Some(Word::Live), None),
                        // The snapshot is not this read's: it is not called live.
                        (true, Some(_)) => (Some(Word::Held), Some(Why::Custody)),
                    };
                }
            }
            if replaced.is_none()
                && let Some(snapshot) = &held
                && self.face_missing(&state, snapshot)
            {
                state.ext.clone_from(&snapshot.ext);
                self.write_face(&mut state, snapshot, held.as_ref())?;
            }
            state.seq = state.seq.saturating_add(1);
            state.pass_ms = now_ms;
            put(&root.state(name), &state.encode())?;
            Ok(match state.kept {
                Some(kept) => Recorded::Kept(kept),
                None => Recorded::Recorded {
                    replaced: replaced.is_some(),
                },
            })
        }

        /// Whether no face of `snapshot` is on disk.
        fn face_missing(&self, state: &State, snapshot: &Snapshot) -> bool {
            let ext = if state.ext.is_empty() {
                &snapshot.ext
            } else {
                &state.ext
            };
            matches!(read_bounded(&self.root.face(&self.name, ext)), Ok(None))
        }

        /// Write `snapshot`'s face unless the face there holds bytes no pass wrote: bytes that
        /// are neither the ones `state` records nor the rendering of `held`, the snapshot that
        /// was here when this pass took the lock. A face too large for the grid that opens it
        /// is not written, and the one a pass wrote before it is removed: programs still read
        /// the snapshot.
        fn write_face(
            &self,
            state: &mut State,
            snapshot: &Snapshot,
            held: Option<&Snapshot>,
        ) -> io::Result<()> {
            let path = self.root.face(&self.name, &snapshot.ext);
            let (there, ours) = match read_bounded(&path) {
                Ok(None) => (false, true),
                Ok(Some(bytes)) => (
                    true,
                    state.face.as_deref() == Some(&sha_hex(&bytes))
                        || held.is_some_and(|held| held.face().as_bytes() == bytes),
                ),
                Err(_) => (true, false),
            };
            if !ours {
                return Ok(());
            }
            let text = snapshot.face();
            if text.len() as u64 > crate::MAX_FACE_BYTES {
                if there {
                    match std::fs::remove_file(&path) {
                        Err(error) if error.kind() != io::ErrorKind::NotFound => {
                            return Err(error);
                        }
                        _ => {}
                    }
                }
                state.face = None;
                return Ok(());
            }
            put(&path, text.as_bytes())?;
            state.face = Some(sha_hex(text.as_bytes()));
            Ok(())
        }
    }
}

/// Where this crate's tests stop a pass: the writes of a recording, counted on the thread
/// that makes them.
#[cfg(test)]
#[path = "file_cut_tests.rs"]
pub(crate) mod cut;
