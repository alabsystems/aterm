// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What of this Mac's Commons state moved since it was last looked at: a table's held records,
//! the control journal, the membership records. A serving transport looks ([`Moves::take`]) in
//! its owner's polled round, in a tick that joined a round which received, and when a watch on
//! those files rings its doorbell, and tells its dial scheduler what moved
//! (`discovery::Hints::moved`), which offers it to peers that may lack it. A look reads this
//! Mac's files alone, whoever wrote them (this process's rounds, a command, another process),
//! and what it tells only orders dials: it is no evidence of any peer and grants nothing.

use astream_wire::sync::{Actor, Digest};
use aterm_secrets::{
    client::Paths,
    commons::journal::{Journal, Trust},
};
use aterm_uds::{
    file_watch::{FileStamp, FileWatch},
    ready::Doorbell,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read as _},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// What a look can find moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Subject {
    /// The control journal holds another node: every round repairs it.
    Control,
    /// The membership records changed: every round that negotiates them exchanges them.
    Members,
    /// A store of this table, primary or recovery, holds another witnessed record, an operation
    /// or a conflict proof.
    Table(Digest),
}

#[cfg(test)]
thread_local! {
    /// MUTANT, on for one test thread: the membership records moved whenever their file's stamp
    /// did, whatever it holds, so a rewrite of the same records reads as a move.
    pub(super) static STAMPS_TELL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The membership records as a look last read them.
struct Records {
    path: PathBuf,
    /// The file's stamp before the read: while it stands the file is not read again.
    stamp: Option<FileStamp>,
    /// SHA-256 of the file; none where there is none.
    digest: Option<Digest>,
}

/// SHA-256 of the file at `path`; none where there is none.
fn digest_of(path: &Path) -> io::Result<Option<Digest>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut sha = aterm_digest::Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        match file.read(&mut buffer)? {
            0 => return Ok(Some(sha.finalize())),
            read => sha.update(&buffer[..read]),
        }
    }
}

/// One Mac's Commons state as it was last looked at, by what it holds and never by a file's
/// times alone: a file rewritten with what it held is no move, so nothing a round rewrites
/// without changing can keep the walk going.
pub(super) struct Look {
    paths: Paths,
    journal: PathBuf,
    trust: Trust,
    /// Each held store's witnessed length (`crate::table::store::held_lengths`), by table and
    /// region. A store a later look does not list keeps its length here.
    tables: Option<BTreeMap<(Digest, bool), u64>>,
    /// How many nodes the control journal holds.
    control: Option<usize>,
    members: Option<Records>,
}

impl Look {
    /// The state under `paths` and the control journal at `journal` under `trust`, as it is now:
    /// what the first [`Self::moved`] compares with.
    pub(super) fn new(paths: &Paths, journal: &Path, trust: Trust) -> Self {
        let mut look = Self {
            paths: paths.clone(),
            journal: journal.into(),
            trust,
            tables: None,
            control: None,
            members: None,
        };
        look.moved();
        look
    }

    /// What moved since the last look. A part that cannot be read now (a claim directory that
    /// cannot be listed, a journal or a records layout mid-change) is neither moved nor
    /// forgotten: the next look that reads it compares with the last that did. The first read
    /// of a part is what later looks compare with, and tells nothing.
    pub(super) fn moved(&mut self) -> BTreeSet<Subject> {
        let mut moved = BTreeSet::new();
        if let Ok(held) = crate::table::store::held_lengths(&self.paths) {
            let first = self.tables.is_none();
            let tables = self.tables.get_or_insert_with(BTreeMap::new);
            for (table, recovery, length) in held {
                if tables.insert((table, recovery), length) != Some(length) && !first {
                    moved.insert(Subject::Table(table));
                }
            }
        }
        // A lockless verified view, kept by the process while the journal's files stand
        // (`Journal::read_existing`): an unchanged journal costs their stamps.
        if let Ok(journal) = Journal::read_existing(&self.journal, self.trust) {
            let nodes = journal.inventory().count();
            if self
                .control
                .replace(nodes)
                .is_some_and(|held| held != nodes)
            {
                moved.insert(Subject::Control);
            }
        }
        if self.members() {
            moved.insert(Subject::Members);
        }
        moved
    }

    /// Whether the membership records moved since the last look that read them.
    fn members(&mut self) -> bool {
        let path = self
            .paths
            .records_data_file()
            .unwrap_or_else(|_| self.paths.records_file());
        let stamp = FileStamp::of(&path);
        if self
            .members
            .as_ref()
            .is_some_and(|held| held.path == path && held.stamp == stamp)
        {
            return false;
        }
        // The stamp is read first: a file replaced during the read is read again next time.
        let Ok(digest) = digest_of(&path) else {
            return false;
        };
        #[cfg(test)]
        let stamps_tell = STAMPS_TELL.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let stamps_tell = false;
        let moved = self
            .members
            .as_ref()
            .is_some_and(|held| held.digest != digest || stamps_tell);
        self.members = Some(Records {
            path,
            stamp,
            digest,
        });
        moved
    }
}

/// The most rounds whose receipts one look takes as its moves' sources ([`Moves::received`]).
const MAX_SOURCES: usize = 16;

/// A transport's look at what moved here ([`Look`]) and what says when to take one. The default
/// looks at nothing: a transport that serves no seat, or an enrolling one.
#[derive(Default)]
pub(super) struct Moves {
    look: Option<Look>,
    /// The owner's polled round asked for a look ([`Self::poll`]).
    due: bool,
    /// The watch heard a file change since the last look.
    rang: Arc<AtomicBool>,
    /// Which seat each round joined since the last look received from, of what, and when that
    /// round began.
    sources: Vec<(Subject, Actor, Instant)>,
    /// The watch on the files a look reads; its thread ends as this drops.
    _watch: Option<FileWatch>,
    /// How many times the watch told the primary claim directory (tests).
    #[cfg(test)]
    claims_told: Arc<std::sync::atomic::AtomicUsize>,
}

/// The watch's key for the primary stores' claim directory.
const CLAIMS: u64 = 0;
/// The watch's key for the recovery stores' claim directory.
const RECOVERY_CLAIMS: u64 = 1;
/// The watch's key for the control journal's log.
const CONTROL: u64 = 2;
/// The watch's key for the membership records' data file.
const RECORDS: u64 = 3;

impl Moves {
    /// Look at the state under `paths` and the control journal at `journal` under `trust`, and
    /// watch their files: a change rings `bell`, the transport's doorbell, so the serving loop
    /// looks at once rather than at its next polled round. With no doorbell, or where the
    /// platform's file events cannot start, nothing is watched and the polled round's look
    /// stands alone; it also covers what a watch does not hear.
    pub(super) fn start(
        paths: &Paths,
        journal: &Path,
        trust: Trust,
        bell: Option<Arc<Doorbell>>,
    ) -> Self {
        let mut moves = Self {
            look: Some(Look::new(paths, journal, trust)),
            ..Self::default()
        };
        let Some(bell) = bell else {
            return moves;
        };
        let rang = moves.rang.clone();
        #[cfg(test)]
        let claims_told = moves.claims_told.clone();
        let Ok(watch) = FileWatch::platform(
            "commons-moved",
            Box::new(move |key| {
                // Which file changed is not kept: a look reads them all.
                let _ = key;
                rang.store(true, Ordering::Release);
                bell.ring();
                #[cfg(test)]
                if key == CLAIMS {
                    // Publish completion after both the state flag and the doorbell.
                    claims_told.fetch_add(1, Ordering::Release);
                }
            }),
        ) else {
            return moves;
        };
        // A table append renames a new claim over the old one, which its directory's entries
        // tell; the control journal is appended in place and the records are replaced by rename,
        // which a watch on each file tells.
        let [primary, recovery] = crate::table::store::claim_dirs(paths);
        watch.watch_directory(CLAIMS, primary);
        watch.watch_directory(RECOVERY_CLAIMS, recovery);
        watch.watch(CONTROL, journal.join("control.astream"));
        watch.watch(
            RECORDS,
            paths
                .records_data_file()
                .unwrap_or_else(|_| paths.records_file()),
        );
        moves._watch = Some(watch);
        moves
    }

    /// [`Self::start`] with no watch (tests).
    #[cfg(test)]
    pub(super) fn unwatched(paths: &Paths, journal: &Path, trust: Trust) -> Self {
        Self::start(paths, journal, trust, None)
    }

    /// How many times the watch told the primary claim directory (tests).
    #[cfg(test)]
    pub(super) fn claims_told(&self) -> usize {
        self.claims_told.load(Ordering::Acquire)
    }

    /// The owner's polled round: look at the next [`Self::take`].
    pub(super) fn poll(&mut self) {
        self.due = true;
    }

    /// A round that began at `began` and was joined since the last look received from `actor`
    /// what `subject` names: if the next look finds it moved, that seat holds what moved it, as
    /// of `began`.
    pub(super) fn received(&mut self, subject: Subject, actor: Actor, began: Instant) {
        if self.look.is_some() && self.sources.len() < MAX_SOURCES {
            self.sources.push((subject, actor, began));
        }
    }

    /// Look, if a look is due (the polled round asked, a round received, the watch rang): what
    /// moved, each with the seats it was received from since the last look and when each of
    /// those rounds began. Those seats are taken either way.
    pub(super) fn take(&mut self) -> Vec<(Subject, Vec<(Actor, Instant)>)> {
        let sources = std::mem::take(&mut self.sources);
        let rang = self.rang.swap(false, Ordering::AcqRel);
        let due = std::mem::take(&mut self.due);
        let Some(look) = &mut self.look else {
            return Vec::new();
        };
        if !(due || rang || !sources.is_empty()) {
            return Vec::new();
        }
        look.moved()
            .into_iter()
            .map(|subject| {
                let from = sources
                    .iter()
                    .filter(|(received, _, _)| *received == subject)
                    .map(|(_, actor, began)| (*actor, *began))
                    .collect();
                (subject, from)
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "moved_tests.rs"]
mod tests;
