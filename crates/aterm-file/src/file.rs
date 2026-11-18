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
//!   since this one looked ([`File::look`]). Where the old frontier cannot be read, only the
//!   newest home's read replaces the snapshot. It writes a file only where its bytes differ, so
//!   a pass that changes nothing writes nothing, unless another pass's look is outstanding:
//!   then it moves the count of passes alone, so that pass records nothing.
//!
//! A read answers this Mac's own writes too: [`File::read`] lays the intents waiting in the
//! outbox over the snapshot (`crate::outbox`), and [`File::published`] is the snapshot alone.
//!
//! With a trace scope armed on the calling thread (`aterm_uds::trace`), a read records what its
//! answer came from: each file's stamp, as `aterm_uds::atomic_file::read_regular` records it,
//! except the status file's, which is recorded by its standing ([`File::state`]): everything it
//! holds but the count of passes and the pass's time, which every pass moves.

use crate::{
    ACCOUNT_HANDLES, Face, Frontier, Kept, Name, Named, Root, Snapshot, State, Status, Why, Word,
    many_accounts, no_account, resolve,
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

    /// The rows `word` names, as [`resolve`] reads the snapshot's rows and its handle columns
    /// (an account's slug, name and address; a machine's label and nickname): none, one, or
    /// each of several.
    #[must_use]
    pub fn named(&self, word: &str) -> Vec<&Row> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let rows: Vec<Named<'_>> = snapshot
            .rows
            .iter()
            .map(|row| Named {
                pk: &row.pk,
                live: row.live,
                handles: snapshot
                    .handles
                    .iter()
                    .map(|handle| row.cell(handle))
                    .collect(),
            })
            .collect();
        resolve(word, &rows)
            .places()
            .into_iter()
            .map(|place| &snapshot.rows[place])
            .collect()
    }

    /// The one row `word` names ([`Self::named`]), or why it names none: a word that names no
    /// row lists none, and one that names several lists each with its handles and key. A file
    /// of accounts (its handles are [`ACCOUNT_HANDLES`]) says so in an account's words
    /// ([`no_account`], [`many_accounts`]).
    ///
    /// # Errors
    /// `word` names no row, or several.
    pub fn one(&self, word: &str) -> Result<&Row, String> {
        let handles = self
            .snapshot
            .as_ref()
            .map_or(&[][..], |snapshot| snapshot.handles.as_slice());
        let accounts = handles == ACCOUNT_HANDLES;
        match self.named(word).as_slice() {
            [row] => Ok(row),
            [] if accounts => Err(no_account(word)),
            [] => {
                let by: Vec<&str> = handles.iter().map(String::as_str).collect();
                Err(if by.is_empty() {
                    format!("no row is named `{word}`: name one by its key")
                } else {
                    format!(
                        "no row is named `{word}`: name one by its key or its {}",
                        by.join(" or ")
                    )
                })
            }
            many if accounts => Err(many_accounts(
                word,
                many.iter()
                    .map(|row| (row.cell("slug"), row.cell("name"), &*row.pk)),
            )),
            many => {
                let mut why = format!("`{word}` names {} rows; name one by its key:", many.len());
                for row in many {
                    why.push_str("\n ");
                    for handle in handles {
                        why.push(' ');
                        why.push_str(row.cell(handle));
                        why.push(' ');
                    }
                    why.push(' ');
                    why.push_str(&row.pk);
                }
                Err(why)
            }
        }
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

/// What a read of the status file at `path` answers: `None` where none is there, or where its
/// bytes are not a status this build reads.
///
/// # Errors
/// The file is there and could not be read: no regular file, past [`MAX_BYTES`], or the open or
/// the read failed.
fn state_at(path: &Path) -> std::io::Result<Option<State>> {
    match aterm_uds::atomic_file::read_regular(path, MAX_BYTES) {
        Ok((bytes, _)) => Ok(State::decode(&bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The status file at `path`, `None` where none is there, it does not read, or it is no status
/// this build reads. With a trace scope armed on this thread it records the file's standing
/// ([`standing_of`], replayed by [`traced_standing`]) and no stamp of it, and where `counted`,
/// the count of passes as well ([`count_of`], replayed by [`traced_count`]). A read that failed
/// records the retry `read_regular` records for it, where no stamp decides its error.
fn read_state(path: &Path, counted: bool) -> Option<State> {
    use aterm_uds::trace;
    if !trace::armed() {
        return state_at(path).ok().flatten();
    }
    // The read's own record, made apart so that no stamp of the file joins the armed scope: only
    // its retry is taken. A raw wall read within it still poisons the armed scope.
    let (read, apart) = trace::unrecorded(|| trace::scope(|| state_at(path)));
    if let Some(trace::Unkept::Retry(why)) = apart.unkept() {
        trace::retry(why);
    }
    let state = read.ok().flatten();
    trace::standing(path, state.as_ref().map(standing_of), traced_standing);
    if counted {
        trace::standing(path, Some(count_of(state.as_ref())), traced_count);
    }
    state
}

/// A status's standing: everything it holds but [`State::seq`] and [`State::pass_ms`],
/// digested. Every pass that writes the status moves those two, and a pass that changed nothing
/// writes nothing ([`Recorded::Settled`]) or, where another pass's look is outstanding, the
/// status with only those two moved ([`Recorded::Fenced`], [`File::record`]), which leaves the
/// standing as it was. No reader of the status but [`File::look`] and [`File::record`] (the
/// count) and [`crate::Status::json`] (both, and it poisons a scope) answers or decides by them.
fn standing_of(state: &State) -> [u8; 32] {
    let rest = State {
        seq: 0,
        pass_ms: 0,
        ..state.clone()
    };
    Sha256::digest(rest.encode())
}

/// The standing of the status file at `path` now ([`standing_of`]), for a trace's replay: `None`
/// where a read would answer none.
fn traced_standing(path: &Path) -> Option<[u8; 32]> {
    state_at(path).ok().flatten().as_ref().map(standing_of)
}

/// The count of passes a status says, digested: what [`File::look`] answers, 0 with none.
fn count_of(state: Option<&State>) -> [u8; 32] {
    Sha256::digest(state.map_or(0, |state| state.seq).to_le_bytes())
}

/// The count of passes the status file at `path` says now ([`count_of`]), for a trace's replay.
fn traced_count(path: &Path) -> Option<[u8; 32]> {
    Some(count_of(state_at(path).ok().flatten().as_ref()))
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
    ///
    /// With a trace scope armed on this thread (`aterm_uds::trace`), the read records the file's
    /// standing and no stamp of it: everything the status holds but [`State::seq`] and
    /// [`State::pass_ms`], which every pass that writes the status moves. So a pass that changed
    /// nothing, which writes nothing or, fencing another pass's look, the status with only those
    /// two moved ([`Self::record`]), leaves a run that read the status standing, while a pass that
    /// moves its word, its why, its revision or anything else it holds moves it. [`Self::read`],
    /// [`Self::published`], [`Self::status`] and [`Self::face`] read the status this way;
    /// [`Self::look`] records the count as well. A read that failed for a reason no stamp
    /// decides records a retry.
    #[must_use]
    pub fn state(&self) -> Option<State> {
        read_state(&self.root.state(&self.name), false)
    }

    /// [`Self::state`], read by one that decides by the count of passes too ([`Self::look`],
    /// [`Self::record`]): with a trace scope armed it records the count beside the standing.
    #[cfg(any(unix, windows))]
    fn counted(&self) -> Option<State> {
        read_state(&self.root.state(&self.name), true)
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

    /// The published table ([`Self::published`]) with the cells, by row key and column, this
    /// Mac's own writes would still change or hold back: each queued intent's, each published
    /// one's the snapshot does not hold ([`Self::projected_from`]), which [`Self::read`] lays
    /// over it, and each held one's. One read answers both, so the cells are the rows' beside
    /// them.
    #[must_use]
    pub fn published_pending(&self) -> (Read, std::collections::BTreeSet<(String, String)>) {
        let (read, outbox) = self.read_published();
        let pending = crate::outbox::pending(&outbox, || self.frontier_of(&read));
        (read, pending)
    }

    /// The frontier the snapshot here was projected from: the frontier file, where the status
    /// says so ([`Self::vouched`]). A pass writes the frontier before the snapshot and the
    /// status last, so a frontier read while a pass writes, or left by a pass cut short, can be
    /// newer than the snapshot and name operations its rows do not hold: the status does not
    /// name that one yet, and `None` answers, as it does under a status an earlier build wrote.
    #[must_use]
    pub fn projected_from(&self) -> Option<Frontier> {
        let state = self.state()?;
        let snapshot = self.snapshot().ok().flatten()?;
        self.vouched(&state, &snapshot)
    }

    /// [`Self::projected_from`], where the snapshot here is the one at revision `rev`: what the
    /// baseline at that revision holds, for a write read against it and for a done marker
    /// settled once the pass recorded that revision in its status.
    #[must_use]
    pub fn projected_at(&self, rev: u64) -> Option<Frontier> {
        let state = self.state()?;
        let snapshot = self
            .snapshot()
            .ok()
            .flatten()
            .filter(|held| held.rev == rev)?;
        self.vouched(&state, &snapshot)
    }

    /// The frontier file, where `state` says `snapshot` is its projection: the status records
    /// the snapshot's revision and content ([`State::snapshot`]) and the fingerprint of the
    /// frontier it was projected from, and the frontier file has that fingerprint. The three
    /// files are read at different moments; a snapshot with the revision and content the status
    /// names is the one it names, and a frontier with the fingerprint it names is that one, so
    /// no order of reads pairs a snapshot with a frontier it was not projected from.
    fn vouched(&self, state: &State, snapshot: &Snapshot) -> Option<Frontier> {
        let content = aterm_codec::hex::encode_lower(&snapshot.content());
        if state.rev != snapshot.rev || state.snapshot.as_deref() != Some(content.as_str()) {
            return None;
        }
        self.frontier().filter(|frontier| {
            aterm_codec::hex::encode_lower(&frontier.fingerprint()) == state.fingerprint
        })
    }

    /// [`Self::vouched`] for the status and the snapshot `read` took.
    fn frontier_of(&self, read: &Read) -> Option<Frontier> {
        self.vouched(read.status.state.as_ref()?, read.snapshot.as_ref()?)
    }

    fn read_as(&self, overlay: bool) -> Read {
        let (mut read, outbox) = self.read_published();
        if overlay {
            // Only an unseen published write is judged against the frontier: a read with none
            // hashes no snapshot and opens no frontier file.
            let frontier = outbox.unseen().then(|| self.frontier_of(&read)).flatten();
            read.snapshot =
                crate::outbox::overlay(&self.name, read.snapshot.take(), &outbox, || frontier);
        }
        read
    }

    /// The published table and its status, with this Mac's intents as the read found them,
    /// read first: a pass that marks a write seen does so once its snapshot holds it, so a
    /// snapshot read after the marker is at least that one.
    fn read_published(&self) -> (Read, crate::outbox::Outbox) {
        let outbox = self.outbox();
        let state = self.state();
        let (snapshot, mut problem) = match self.snapshot() {
            Ok(snapshot) => (snapshot, None),
            Err(why) => (None, Some(format!("snapshot not read: {why}"))),
        };
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
        let read = Read {
            status: Status {
                word,
                why,
                state,
                problem,
                queued,
                held,
            },
            snapshot,
        };
        (read, outbox)
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

    /// The extension the face is written with: the last pass's, else the snapshot's, else a
    /// table's.
    #[must_use]
    pub fn face_ext(&self) -> String {
        self.state()
            .map(|state| state.ext)
            .filter(|ext| !ext.is_empty())
            .or_else(|| {
                self.snapshot()
                    .ok()
                    .flatten()
                    .map(|snapshot| snapshot.ext)
                    .filter(|ext| !ext.is_empty())
            })
            .unwrap_or_else(|| crate::snapshot::TABLE_EXT.to_owned())
    }

    /// Where the face is.
    #[must_use]
    pub fn face_path(&self) -> std::path::PathBuf {
        self.root.face(&self.name, &self.face_ext())
    }

    /// The face's bytes as they are now; `None` where no face is there.
    ///
    /// # Errors
    /// The face is not a regular file, is larger than any face a pass writes, or cannot be read.
    pub fn face_bytes(&self) -> Result<Option<Vec<u8>>, String> {
        match aterm_uds::atomic_file::read_regular(&self.face_path(), crate::MAX_FACE_BYTES) {
            Ok((bytes, _)) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        }
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

    /// SHA-256 of the history file's bytes, lower-case hex, as the status records the history
    /// a pass wrote ([`State::history`]); `None` where it is missing or does not read.
    #[must_use]
    pub fn history_digest(&self) -> Option<String> {
        read_bounded(&self.root.history(&self.name))
            .ok()
            .flatten()
            .map(|bytes| sha_hex(&bytes))
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
    /// The digest of everything the projection reads (the materializer's key): a snapshot
    /// recorded at a key is that key's projection, and a read at the same key projects the
    /// same snapshot again.
    pub key: [u8; 32],
    /// What the pass made of the read.
    pub projection: Projection,
}

/// What a pass that opened custody made of its read.
#[derive(Clone, Debug)]
pub enum Projection {
    /// It projected the read.
    Fresh(Box<Projected>),
    /// It projected nothing: the status records the snapshot at the pass's key, and the
    /// snapshot the status names (its revision, extension and content, [`State::snapshot`])
    /// and the history it names are on disk. Recording checks the key against
    /// the status under the file's lock, and records `held` (custody) where it differs.
    Unchanged,
    /// It projected nothing: the last pass did not take its read for this reason, and nothing
    /// the reason rests on has moved since. Recording judges the reason again from the
    /// frontiers it compares and the key the status kept, and keeps the snapshot whatever it
    /// finds: a reason it cannot confirm is recorded as `held` (custody), with no reason kept.
    Kept(Kept),
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
    /// Cells left out for a credential-shaped value.
    pub withheld: u64,
    /// Records of the file's collection the replica holds inert: refused when the snapshot was
    /// built, kept in history with their authors.
    pub inert: u64,
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
    /// What the pass found is what the files here already say (its status but for the count of
    /// passes and their time, its frontier, snapshot, baseline, history and face), and no other
    /// pass's look was outstanding: it wrote nothing, the status included.
    Settled,
    /// What the pass found is what the files here already say, and another pass had looked
    /// without recording: it wrote the status alone, the count of passes moved, so that pass,
    /// whose read may be older than this one's, records nothing.
    Fenced,
    /// The count of passes moved since this one looked: another pass recorded or fenced it.
    /// This one recorded nothing, neither snapshot nor word.
    Raced,
}

/// What a restore did ([`File::restore`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restored {
    /// The face was rewritten from the snapshot; `kept` is where a person's bytes no commit took
    /// were kept first.
    Rewritten { kept: Option<std::path::PathBuf> },
    /// The face already is the snapshot's.
    Already,
    /// No pass has recorded a snapshot here: there is nothing to restore from.
    Nothing,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The durable writes ([`aterm_uds::atomic_file::write`]) this thread's recordings made: a
    /// frontier, baseline, snapshot, history, face or status each, counted (never timed).
    static PUTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The modes this thread set on a shared file's folders, made or found with another mode
    /// (`private_dir`): a look's or a recording's, and an outbox change's, which makes the same
    /// folders first.
    static CHMODS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many durable writes this thread's recordings ([`File::record`]) have made: `aterm-db`'s
/// idle-hour drive reads it before and after each materializer pass.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_puts() -> usize {
    PUTS.with(std::cell::Cell::get)
}

/// How many folder modes this thread has set on shared files' folders, a look's
/// ([`File::look`]), a recording's ([`File::record`]) or an outbox change's: `aterm-db`'s
/// idle-hour drive reads it before and after each materializer pass.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_chmods() -> usize {
    CHMODS.with(std::cell::Cell::get)
}

#[cfg(any(unix, windows))]
mod write {
    use super::{
        BASELINES, File, HISTORY, MAX_BYTES, Opened, Pass, Projected, Projection, Recorded,
        Restored, read_bounded, sha_hex,
    };
    use crate::{Frontier, Kept, Snapshot, State, Why, Word};
    use aterm_uds::file_lock::LockFile;
    use std::{
        io,
        path::Path,
        time::{Duration, Instant},
    };

    /// How long a pass waits for another that is recording the same file.
    const LOCK_WAIT: Duration = Duration::from_secs(10);

    /// What a pass notes before it opens custody ([`File::look`]) and hands to
    /// [`File::record`]: the count of passes that had recorded, and its lease, a shared lock of
    /// the file's look lock (`Root::looking`) held from the look until the record takes the
    /// note. While the lease is held, a recording that changes nothing still moves the count.
    #[derive(Debug)]
    pub struct Look {
        seq: u64,
        lease: LockFile,
    }

    impl Look {
        /// The count of passes that had recorded when the pass looked.
        #[must_use]
        pub const fn seq(&self) -> u64 {
            self.seq
        }
    }

    /// Make `dir` and every folder above it that is missing, each the owner's alone (0700),
    /// and set its mode only where it has another: a folder already private is not changed.
    /// The mode compared is the one a chmod sets, a linked folder's own (`set_permissions`
    /// follows the link, as `metadata` does). With a trace scope armed it records a write
    /// (`aterm_uds::trace::effect`) where it makes a folder or sets a mode, each of which moves
    /// what a look at the folder shows; a folder already there and private records nothing.
    fn private_dir(dir: &Path) -> io::Result<()> {
        use aterm_uds::trace::{Effect, effect};
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
            let mode = match std::fs::metadata(dir) {
                Ok(there) if there.is_dir() => there.permissions().mode(),
                _ => {
                    effect(Effect::Write);
                    std::fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(dir)?;
                    std::fs::metadata(dir)?.permissions().mode()
                }
            };
            if mode & 0o7777 != 0o700 {
                effect(Effect::Write);
                #[cfg(any(test, debug_assertions))]
                super::CHMODS.with(|chmods| chmods.set(chmods.get() + 1));
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        #[cfg(not(unix))]
        if !std::fs::metadata(dir).is_ok_and(|there| there.is_dir()) {
            effect(Effect::Write);
            std::fs::create_dir_all(dir)?;
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
        #[cfg(any(test, debug_assertions))]
        super::PUTS.with(|puts| puts.set(puts.get() + 1));
        aterm_uds::atomic_file::write(path, bytes, 0o600)
    }

    /// [`put`] `bytes` at `path` unless `there`, what the file held when the pass took the
    /// lock, is the same bytes; whether it wrote.
    fn put_new(path: &Path, bytes: &[u8], there: Option<&[u8]>) -> io::Result<bool> {
        if there == Some(bytes) {
            return Ok(false);
        }
        put(path, bytes)?;
        Ok(true)
    }

    impl File {
        /// Make this file's folders, each the owner's alone.
        ///
        /// # Errors
        /// A folder cannot be made or its mode set.
        pub fn make_dirs(&self) -> io::Result<()> {
            let (root, name) = (&self.root, &self.name);
            private_dir(root.path())?;
            private_dir(&root.face_dir(name))?;
            private_dir(&root.path().join(crate::layout::STATE))?;
            private_dir(&root.state_dir(name))
        }

        /// Note the count of passes that have recorded, before a pass opens custody, with the
        /// pass's lease on the file's look lock (`Root::looking`), which [`Self::record`]
        /// takes with the note. The lease is taken before the count is read, so a recording
        /// that changed nothing and found no lease ([`Recorded::Settled`]) is one this look
        /// comes after. It waits only for such a recording's try of the lock, which never
        /// waits.
        ///
        /// # Errors
        /// The file's folders cannot be made, or its look lock cannot be opened or locked.
        pub fn look(&self) -> io::Result<Look> {
            self.make_dirs()?;
            let lease = LockFile::share(
                &self.root.looking(&self.name),
                Instant::now() + LOCK_WAIT,
                "another pass is recording this shared file",
            )?;
            Ok(Look {
                // Read so a trace records the count beside the status's standing.
                seq: self.counted().map_or(0, |state| state.seq),
                lease,
            })
        }

        /// Write the baseline of revision `rev` where the one there holds other bytes, and then
        /// drop the ones older than the newest [`BASELINES`]; whether it wrote.
        fn write_baseline(&self, rev: u64, bytes: &[u8]) -> io::Result<bool> {
            let dir = self.root.baselines(&self.name);
            private_dir(&dir)?;
            let path = self.root.baseline(&self.name, rev);
            if !put_new(&path, bytes, read_bounded(&path).ok().flatten().as_deref())? {
                return Ok(false);
            }
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
            Ok(true)
        }

        /// Record one pass, which took `look` ([`File::look`]) before it opened custody.
        ///
        /// Under the file's lock: a pass whose look's count is no longer the recorded count
        /// records nothing ([`Recorded::Raced`]), so a view of live custody taken before a
        /// fence never turns the fenced pass's `held` into `live`. A refusal leaves the
        /// snapshot as it was and records its word. An open replaces the snapshot only where
        /// its frontier admits it over the old one and its bytes are ones a read takes, keeps
        /// the revision where the content is the same, and records `live` only where the
        /// snapshot now is that read's: a projection not taken ([`Recorded::Kept`]) records
        /// `held`, and so does a pass that projected nothing ([`Projection::Unchanged`]) at a
        /// key other than the one the status records the snapshot at. A projection too large to
        /// take records its key, and a pass at that key that projected nothing
        /// ([`Projection::Kept`]) keeps the snapshot for the same reason. The rule fails closed:
        /// a snapshot whose frontier file is missing or does not read is replaced only by the
        /// newest home's read, never by a fenced home's, and a frontier too large for the next
        /// pass to read back is not written, nor the snapshot with it. The face is written
        /// beside each snapshot written, and again where it is missing; an edited face is left
        /// alone.
        ///
        /// Each file is written only where its bytes differ from the ones there: the frontier,
        /// the baseline (whose folder is pruned only after one is written), the history and the
        /// face by their bytes, and the snapshot by its revision, its content and its columns
        /// (a column's type and nullability, which the content leaves out, included), so a
        /// projection of the same content and columns keeps the file and the time it was
        /// written. Where the next
        /// status would equal the one there but for the count of passes and their time, and no
        /// other file was written, the pass let go of its lease and tries the look lock alone:
        /// taken, no other pass's look is outstanding, and it writes nothing
        /// ([`Recorded::Settled`]); not taken (or not to be tried), it writes the status with
        /// the count moved ([`Recorded::Fenced`]), the `SharedFile` model's `Materialize`
        /// making an outstanding look stale. Neither lock waits on the other: a looker holds
        /// its lease and may wait for this lock, and a recorder holds this lock and only tries
        /// the look lock, never waiting.
        ///
        /// The writes are separate renames, in an order a pass killed between any two is safe
        /// in: the frontier first (a newer frontier beside an older snapshot only refuses
        /// more, and the next pass, whose status still names the older key, projects again),
        /// then the projection's baseline where it has one (a baseline beside an older
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
        pub fn record(&self, look: Look, pass: Pass, now_ms: u64) -> io::Result<Recorded> {
            let (root, name) = (&self.root, &self.name);
            self.make_dirs()?;
            let _lock = LockFile::take(
                &root.lock(name),
                Instant::now() + LOCK_WAIT,
                "another pass is recording this shared file",
            )?;
            let Look { seq, lease } = look;
            // It decides by the count: read so a trace records it ([`File::look`]).
            let before = self.counted().unwrap_or_default();
            if before.seq != seq {
                return Ok(Recorded::Raced);
            }
            let mut state = before.clone();
            // The snapshot and the frontier file here before this pass writes anything.
            let held = self.snapshot().ok().flatten();
            let sidecar_there = read_bounded(&root.frontier(name)).ok().flatten();
            // Whether this pass wrote or removed a file beside the status.
            let mut wrote = false;
            let mut replaced = false;
            // The snapshot this pass's projection left, where it took one.
            let mut taken = None;
            state.kept = None;
            let kept_key = state.kept_key.take();
            match pass {
                Pass::Refused { word, why } => {
                    state.word = Some(word);
                    state.why = Some(why);
                }
                Pass::Opened(opened) => {
                    let Opened {
                        newest,
                        frontier,
                        key,
                        projection,
                    } = *opened;
                    let key = aterm_codec::hex::encode_lower(&key);
                    // A snapshot whose frontier is not here to compare (removed, damaged,
                    // another build's) is replaced only by the newest home's read: a fenced
                    // home's cannot be shown to be no older, so it is behind.
                    let admitted = held.is_none()
                        || sidecar_there
                            .as_deref()
                            .and_then(Frontier::decode)
                            .map_or(newest, |old| frontier.admits_over(&old));
                    let sidecar = frontier.encode();
                    let mut written = None;
                    // The snapshot is this read's: the status names the read's key.
                    let mut current = false;
                    if !admitted {
                        state.kept = Some(Kept::Behind);
                    } else if sidecar.len() as u64 > MAX_BYTES {
                        // The next pass could not read it back, and would then compare its
                        // read with nothing.
                        state.kept = Some(Kept::TooLarge);
                    } else {
                        match projection {
                            Projection::Fresh(mut projected) => {
                                let snapshot = &mut projected.snapshot;
                                snapshot.rev = match &held {
                                    Some(old) if old.content() == snapshot.content() => old.rev,
                                    Some(old) => old.rev.saturating_add(1),
                                    None => state.rev.saturating_add(1),
                                };
                                let bytes = snapshot.encode();
                                if bytes.len() as u64 > MAX_BYTES {
                                    // A read would refuse it: the last snapshot a read takes
                                    // stays, and a pass at the same key does not project it
                                    // again.
                                    state.kept = Some(Kept::TooLarge);
                                    state.kept_key = Some(key.clone());
                                } else {
                                    written = Some((bytes, projected));
                                }
                            }
                            Projection::Unchanged => {
                                current = state.key.as_deref() == Some(key.as_str());
                            }
                            Projection::Kept(Kept::TooLarge)
                                if kept_key.as_deref() == Some(key.as_str()) =>
                            {
                                state.kept = Some(Kept::TooLarge);
                                state.kept_key = kept_key;
                            }
                            // The reason no longer holds (the frontier file was removed, or
                            // the kept key is not this read's), and nothing was projected: the
                            // snapshot is not this read's, and the next pass judges again.
                            Projection::Kept(_) => {}
                        }
                    }
                    if let Some((bytes, projected)) = written {
                        let Projected {
                            snapshot,
                            history,
                            baseline,
                            withheld,
                            inert,
                        } = *projected;
                        wrote |= put_new(&root.frontier(name), &sidecar, sidecar_there.as_deref())?;
                        if let Some(baseline) = baseline {
                            wrote |= self.write_baseline(snapshot.rev, &baseline)?;
                        }
                        // The content here already, at its revision, its columns stated as
                        // this pass states them: the file and the time it was written stay. A
                        // column's type and nullability are no part of the content
                        // (`Snapshot::content`), so a snapshot an earlier build wrote without
                        // them is written again here at the same revision. A keyed build
                        // before this one compares content alone, so it keeps one this build
                        // wrote; a build before the keyed pass rewrites every pass, without
                        // the types, and this build states them again at the same revision.
                        let same = held.as_ref().is_some_and(|old| {
                            old.rev == snapshot.rev
                                && old.ext == snapshot.ext
                                && old.content() == snapshot.content()
                                && old.columns == snapshot.columns
                        });
                        if !same {
                            put(&root.snapshot(name), &bytes)?;
                            (wrote, replaced) = (true, true);
                        }
                        let skip = history.len().saturating_sub(HISTORY);
                        let mut lines = String::new();
                        for entry in &history[skip..] {
                            lines.push_str(&entry.to_string());
                            lines.push('\n');
                        }
                        let path = root.history(name);
                        let there = read_bounded(&path).ok().flatten();
                        wrote |= put_new(&path, lines.as_bytes(), there.as_deref())?;
                        state.rev = snapshot.rev;
                        state.snapshot = Some(aterm_codec::hex::encode_lower(&snapshot.content()));
                        state.rows = snapshot.rows.len() as u64;
                        state.complete = snapshot.complete;
                        state.kind.clone_from(&snapshot.kind);
                        state.ext.clone_from(&snapshot.ext);
                        state.withheld = withheld;
                        state.inert = Some(inert);
                        state.key = Some(key);
                        state.history = Some(sha_hex(lines.as_bytes()));
                        wrote |= self.write_face(&mut state, &snapshot, held.as_ref())?;
                        taken = Some(snapshot);
                        current = true;
                    } else if current {
                        wrote |= put_new(&root.frontier(name), &sidecar, sidecar_there.as_deref())?;
                    }
                    if current {
                        state.fingerprint = aterm_codec::hex::encode_lower(&frontier.fingerprint());
                        state.era = frontier.era;
                        state.generation = frontier.generation;
                        state.records = frontier.ids.len() as u64;
                    }
                    (state.word, state.why) = match (newest, current) {
                        // A fenced home's records are held whatever became of the projection.
                        (false, _) => (Some(Word::Held), Some(Why::Succession)),
                        (true, true) => (Some(Word::Live), None),
                        // The snapshot is not this read's: it is not called live.
                        (true, false) => (Some(Word::Held), Some(Why::Custody)),
                    };
                }
            }
            if taken.is_none()
                && let Some(snapshot) = &held
                && self.face_missing(&state, snapshot)
            {
                state.ext.clone_from(&snapshot.ext);
                wrote |= self.write_face(&mut state, snapshot, held.as_ref())?;
            }
            // The count and the time are all that differ from the status there: the pass
            // changed nothing. It writes nothing where no other pass's look is outstanding.
            let mut fenced = false;
            if !wrote && state == before {
                drop(lease);
                match LockFile::try_take(&root.looking(name)) {
                    Ok(Some(_alone)) => return Ok(Recorded::Settled),
                    // Another pass's lease, or a look lock that could not be tried: the count
                    // moves, which every look still outstanding finds.
                    Ok(None) | Err(_) => fenced = true,
                }
            }
            state.seq = state.seq.saturating_add(1);
            state.pass_ms = now_ms;
            put(&root.state(name), &state.encode())?;
            Ok(if fenced {
                Recorded::Fenced
            } else if let Some(kept) = state.kept {
                Recorded::Kept(kept)
            } else {
                Recorded::Recorded { replaced }
            })
        }

        /// Rewrite the face from the snapshot here (`aterm file restore`), under the lock a pass
        /// records under: what a person wrote there and no commit took is kept first as
        /// `<leaf>.rejected.<ext>` ([`crate::Root::rejected`]), or the next free
        /// `<leaf>.rejected-<n>.<ext>`, never lost. A snapshot whose face
        /// is larger than the grid opens leaves no face, as a pass leaves none.
        ///
        /// # Errors
        /// A local file cannot be read or written, the face there is no regular file a face could
        /// be, or a pass held the lock too long.
        pub fn restore(&self) -> io::Result<Restored> {
            let (root, name) = (&self.root, &self.name);
            let _lock = self.pass_lock()?;
            let (Some(mut state), Some(snapshot)) = (self.state(), self.snapshot().ok().flatten())
            else {
                return Ok(Restored::Nothing);
            };
            let path = root.face(name, &snapshot.ext);
            let text = snapshot.face();
            let there = match read_bounded(&path) {
                Ok(there) => there,
                Err(why) => {
                    return Err(io::Error::other(format!(
                        "the face cannot be read ({why}); move it away and restore again"
                    )));
                }
            };
            let fits = text.len() as u64 <= crate::MAX_FACE_BYTES;
            if fits && there.as_deref() == Some(text.as_bytes()) {
                let sha = sha_hex(text.as_bytes());
                if state.face.as_deref() == Some(&sha) {
                    return Ok(Restored::Already);
                }
                state.face = Some(sha);
                state.ext.clone_from(&snapshot.ext);
                put(&root.state(name), &state.encode())?;
                return Ok(Restored::Already);
            }
            let kept = match there {
                // Bytes a pass or a commit vouches for hold nothing of a person's that is not
                // in the snapshot or the outbox already.
                Some(bytes) if state.face.as_deref() != Some(&sha_hex(&bytes)) => {
                    Some(self.keep_aside(&bytes, &snapshot.ext)?)
                }
                _ => None,
            };
            if fits {
                put(&path, text.as_bytes())?;
                state.face = Some(sha_hex(text.as_bytes()));
            } else {
                match std::fs::remove_file(&path) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
                state.face = None;
            }
            state.ext.clone_from(&snapshot.ext);
            put(&root.state(name), &state.encode())?;
            Ok(Restored::Rewritten { kept })
        }

        /// Take `bytes` as the face's own once a commit queued every write they ask for, under
        /// the lock a pass records under: the face then reads `written`, and the next pass that
        /// writes a new snapshot writes its face over them. Bytes that are no longer the face
        /// (a later save landed) are not taken: that save's own commit judges them.
        ///
        /// # Errors
        /// The status cannot be written, or a pass held the lock too long.
        pub fn adopt(&self, bytes: &[u8]) -> io::Result<bool> {
            let (root, name) = (&self.root, &self.name);
            let _lock = self.pass_lock()?;
            let Some(mut state) = self.state() else {
                return Ok(false);
            };
            let ext = if state.ext.is_empty() {
                crate::snapshot::TABLE_EXT.to_owned()
            } else {
                state.ext.clone()
            };
            if read_bounded(&root.face(name, &ext))
                .ok()
                .flatten()
                .as_deref()
                != Some(bytes)
            {
                return Ok(false);
            }
            state.face = Some(sha_hex(bytes));
            state.ext = ext;
            put(&root.state(name), &state.encode())?;
            Ok(true)
        }

        /// Keep a person's face bytes a commit refused as `<leaf>.rejected.<ext>` beside the face,
        /// or as the next `<leaf>.rejected-<n>.<ext>` where an earlier copy is still there.
        ///
        /// # Errors
        /// The file cannot be written.
        pub fn keep_rejected(&self, bytes: &[u8]) -> io::Result<std::path::PathBuf> {
            self.make_dirs()?;
            self.keep_aside(bytes, &self.face_ext())
        }

        /// The copies of refused face bytes kept beside the face now, each with when it was
        /// last written.
        #[must_use]
        pub fn rejected_kept(&self) -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
            let prefix = format!("{}.rejected", self.name.leaf());
            let Ok(entries) = std::fs::read_dir(self.root.face_dir(&self.name)) else {
                return Vec::new();
            };
            let mut kept: Vec<_> = entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|f| f.starts_with(&prefix))
                })
                .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?.modified().ok()?)))
                .collect();
            kept.sort();
            kept
        }

        /// Keep `bytes` at the first of `<leaf>.rejected.<ext>`, `<leaf>.rejected-2.<ext>`, …
        /// that holds them already or is free, so no copy kept earlier is written over.
        fn keep_aside(&self, bytes: &[u8], ext: &str) -> io::Result<std::path::PathBuf> {
            for n in 1..=u32::MAX {
                let path = self.root.rejected_at(&self.name, ext, n);
                match read_bounded(&path) {
                    Ok(Some(there)) if there == bytes => return Ok(path),
                    Ok(None) => {
                        put(&path, bytes)?;
                        return Ok(path);
                    }
                    _ => {}
                }
            }
            Err(io::Error::other("no free name to keep the face's bytes at"))
        }

        /// Write where a commit found bad values, `text` one `line:column: why` a line, as
        /// `<leaf>.problems` beside the face; empty `text` removes the file.
        ///
        /// # Errors
        /// The file cannot be written or removed.
        pub fn put_problems(&self, text: &str) -> io::Result<std::path::PathBuf> {
            let path = self.root.problems(&self.name);
            if text.is_empty() {
                match std::fs::remove_file(&path) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            } else {
                self.make_dirs()?;
                put(&path, text.as_bytes())?;
            }
            Ok(path)
        }

        /// The lock a pass records under.
        fn pass_lock(&self) -> io::Result<aterm_uds::file_lock::LockFile> {
            self.make_dirs()?;
            aterm_uds::file_lock::LockFile::take(
                &self.root.lock(&self.name),
                Instant::now() + LOCK_WAIT,
                "another pass is recording this shared file",
            )
        }

        /// Whether no face of `snapshot` is on disk: nothing there at its path, by the
        /// metadata a read of it follows (a link to nothing is missing too).
        fn face_missing(&self, state: &State, snapshot: &Snapshot) -> bool {
            let ext = if state.ext.is_empty() {
                &snapshot.ext
            } else {
                &state.ext
            };
            matches!(
                std::fs::metadata(self.root.face(&self.name, ext)),
                Err(error) if error.kind() == io::ErrorKind::NotFound
            )
        }

        /// Write `snapshot`'s face where the face there holds other bytes, unless it holds
        /// bytes no pass wrote: bytes that are neither the ones `state` records nor the
        /// rendering of `held`, the snapshot that was here when this pass took the lock. A face
        /// too large for the grid that opens it is not written, and the one a pass wrote before
        /// it is removed: programs still read the snapshot. Whether it wrote or removed one.
        fn write_face(
            &self,
            state: &mut State,
            snapshot: &Snapshot,
            held: Option<&Snapshot>,
        ) -> io::Result<bool> {
            let path = self.root.face(&self.name, &snapshot.ext);
            let Ok(there) = read_bounded(&path) else {
                // A face that cannot be read is no pass's to replace.
                return Ok(false);
            };
            let ours = there.as_deref().is_none_or(|bytes| {
                state.face.as_deref() == Some(&sha_hex(bytes))
                    || held.is_some_and(|held| held.face().as_bytes() == bytes)
            });
            if !ours {
                return Ok(false);
            }
            let text = snapshot.face();
            if text.len() as u64 > crate::MAX_FACE_BYTES {
                state.face = None;
                if there.is_none() {
                    return Ok(false);
                }
                match std::fs::remove_file(&path) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => {
                        return Err(error);
                    }
                    _ => {}
                }
                return Ok(true);
            }
            let wrote = put_new(&path, text.as_bytes(), there.as_deref())?;
            state.face = Some(sha_hex(text.as_bytes()));
            Ok(wrote)
        }
    }
}

#[cfg(any(unix, windows))]
pub use write::Look;

/// Where this crate's tests stop a pass: the writes of a recording, counted on the thread
/// that makes them.
#[cfg(test)]
#[path = "file_cut_tests.rs"]
pub(crate) mod cut;

#[cfg(test)]
#[path = "file_trace_tests.rs"]
mod trace_tests;

#[cfg(test)]
#[path = "file_resolve_tests.rs"]
mod resolve_tests;
