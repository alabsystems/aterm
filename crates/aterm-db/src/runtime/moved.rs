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

/// Where what a look found moved came from, as far as the rounds' receipts tell
/// ([`Moves::take`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// The look found it grown by more than any round it names brought: something no seat sent
    /// moved it too (a write here, by this process, a command or another process, or a round
    /// that told no receipt of it), which every seat may lack.
    Here,
    /// What the look found is no more than these seats' rounds brought; each holds what its own
    /// round carried both ways. None where it did not grow (a store taken up empty) or a look
    /// tells no growth of it (the membership records) and no round told a receipt of it.
    Seats(Vec<Actor>),
}

#[cfg(test)]
thread_local! {
    /// MUTANT, on for one test thread: the membership records moved whenever their file's stamp
    /// did, whatever it holds, so a rewrite of the same records reads as a move.
    pub(super) static STAMPS_TELL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: the seats a move was received from are named its sources
    /// however much more than they brought the look found ([`Moves::take`]), so a write here made
    /// before the look is not offered to them.
    pub(super) static SOURCES_HOLD_ALL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: every look stamps each store it reads with its own
    /// instant ([`Look::held_since`]), so every store looks younger than every round's lists.
    pub(super) static HELD_RESTAMPED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: a changed Commons configuration restamps no store
    /// ([`Look::held_since`]), so a table its policy selects again looks held since before every
    /// round's lists.
    pub(super) static CONFIG_UNSTAMPED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: a round's receipt counts whenever its round began
    /// ([`Moves::take`]), though an earlier look may have found what it added, so it can cover a
    /// write here.
    pub(super) static STALE_SOURCES_HOLD: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: a look keeps when it found each subject moved for as long
    /// as the transport runs ([`Moves::take`]), one entry for every subject that ever moved.
    pub(super) static MOVES_KEPT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: a look never tells that the configuration's policy
    /// selects more than it did ([`Look::widened`]), so no seat is owed a round for it.
    pub(super) static WIDENED_UNTOLD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: every change of the configuration's stamp tells a
    /// widening ([`Look::widened`]), whatever its policy says, so an unrelated setting's edit
    /// owes every seat a round.
    pub(super) static WIDENED_BY_STAMP: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
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
    /// The Commons configuration, whose subscription policy says which held tables this Mac's
    /// rounds list (`crate::catalog::policy::Policy`), and its stamp as the last look read it.
    config: (PathBuf, Option<FileStamp>),
    /// The policy the configuration gave when a look last read it whole; none until one did. A
    /// look reads it again only where the stamp changed, and keeps this one where that read
    /// fails.
    policy: Option<crate::catalog::policy::Policy>,
    /// A [`Self::moved`] since the transport last asked ([`Moves::widened`]) found the policy
    /// selecting what the one before it did not ([`Self::reread_policy`]).
    widened: bool,
    /// Each held store's witnessed length (`crate::table::store::held_lengths`), by table and
    /// region, with when this Mac took it up as far as a look tells ([`Self::held_since`]). A
    /// store a later look does not list keeps its entry here.
    tables: Option<BTreeMap<(Digest, bool), (u64, Instant)>>,
    /// How many nodes the control journal holds.
    control: Option<usize>,
    members: Option<Records>,
    /// By how much the last [`Self::moved`] found each table's stores (both regions together) and
    /// the control journal grown, of what it found moved ([`Self::grown`]).
    grown: BTreeMap<Subject, u64>,
}

impl Look {
    /// The state under `paths` and the control journal at `journal` under `trust`, with the
    /// Commons configuration at `config`, as it is now: what the first [`Self::moved`] compares
    /// with.
    pub(super) fn new(paths: &Paths, journal: &Path, config: &Path, trust: Trust) -> Self {
        let mut look = Self {
            paths: paths.clone(),
            journal: journal.into(),
            trust,
            config: (config.into(), FileStamp::of(config)),
            policy: crate::catalog::policy::Policy::read(config).ok(),
            widened: false,
            tables: None,
            control: None,
            members: None,
            grown: BTreeMap::new(),
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
        self.grown.clear();
        // A changed configuration may select again a table its policy excluded, whose store this
        // Mac kept (exclusion reclaims no custody) and whose rounds listed it with no seat: each
        // store counts as taken up now. A change that selects nothing new costs each seat, until
        // the next round with it lists anew, a walk dial for a write to a table this Mac did not
        // list with it.
        let stamp = FileStamp::of(&self.config.0);
        #[cfg(test)]
        let unstamped = CONFIG_UNSTAMPED.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let unstamped = false;
        let changed = stamp != self.config.1;
        if changed && !unstamped {
            let now = Instant::now();
            for (_, found) in self.tables.iter_mut().flat_map(BTreeMap::values_mut) {
                *found = now;
            }
        }
        // Kept until the transport takes it ([`Moves::widened`]).
        self.widened |= changed && self.reread_policy();
        self.config.1 = stamp;
        if let Ok(held) = crate::table::store::held_lengths(&self.paths) {
            let first = self.tables.is_none();
            let now = Instant::now();
            #[cfg(test)]
            let restamped = HELD_RESTAMPED.with(std::cell::Cell::get);
            #[cfg(not(test))]
            let restamped = false;
            let tables = self.tables.get_or_insert_with(BTreeMap::new);
            for (table, recovery, length) in held {
                let mut new = false;
                let (kept, found) = tables.entry((table, recovery)).or_insert_with(|| {
                    new = true;
                    (0, now)
                });
                if restamped {
                    *found = now;
                }
                let before = std::mem::replace(kept, length);
                if (new || before != length) && !first {
                    moved.insert(Subject::Table(table));
                    *self.grown.entry(Subject::Table(table)).or_default() +=
                        length.saturating_sub(before);
                }
            }
        }
        // A lockless verified view, kept by the process while the journal's files stand
        // (`Journal::read_existing`): an unchanged journal costs their stamps.
        if let Ok(journal) = Journal::read_existing(&self.journal, self.trust) {
            let nodes = journal.inventory().count();
            if let Some(held) = self.control.replace(nodes)
                && held != nodes
            {
                moved.insert(Subject::Control);
                self.grown.insert(
                    Subject::Control,
                    u64::try_from(nodes.saturating_sub(held)).unwrap_or(u64::MAX),
                );
            }
        }
        if self.members() {
            moved.insert(Subject::Members);
        }
        moved
    }

    /// Read the configuration's policy again, whose stamp changed: whether it selects what the
    /// last one read did not, sharing enabled where it was not or a table excluded no more. A
    /// read that fails keeps the last policy and tells nothing, and the first read, with no
    /// policy before it, tells nothing.
    ///
    /// The rounds this Mac had with each seat before the change listed what the old policy
    /// selected, and a seat that read those lists as this Mac leaving a table out passes this
    /// Mac over for writes to it until a round lists it (`super::discovery::may_take`): the
    /// transport owes each such seat that round (`super::discovery::Hints::relist_all`).
    fn reread_policy(&mut self) -> bool {
        let Ok(new) = crate::catalog::policy::Policy::read(&self.config.0) else {
            return false;
        };
        let old = self.policy.replace(new.clone());
        #[cfg(test)]
        if WIDENED_UNTOLD.with(std::cell::Cell::get) {
            return false;
        }
        #[cfg(test)]
        if WIDENED_BY_STAMP.with(std::cell::Cell::get) {
            return old.is_some();
        }
        old.is_some_and(|old| {
            new.enabled && (!old.enabled || old.excluded.difference(&new.excluded).next().is_some())
        })
    }

    /// By how much the last [`Self::moved`] found `subject` grown: a table's witnessed records
    /// across its stores, the control journal's nodes; none for the membership records, which a
    /// look tells by their digest alone, or for what that look did not find moved.
    pub(super) fn grown(&self, subject: &Subject) -> Option<u64> {
        self.grown.get(subject).copied()
    }

    /// When this Mac took up a store of `table` here, primary or recovery, as far as a look
    /// tells: when a look first found it (the first look's stores, when it read them), or the
    /// last look that found the Commons configuration changed since, whichever is later; of the
    /// two regions' stores, the earlier. None where no look has found one.
    pub(super) fn held_since(&self, table: &Digest) -> Option<Instant> {
        self.tables
            .as_ref()?
            .range((*table, false)..=(*table, true))
            .map(|(_, (_, found))| *found)
            .min()
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
    /// Which seat each round joined since the last look received from, of what, when that round
    /// began, and how much it brought ([`Self::received`]).
    sources: Vec<(Subject, Actor, Instant, u64)>,
    /// When a look last found each subject moved, by the instant [`Self::take`] was given: a
    /// round that began before then may have added what that look found ([`Self::take`]). Only
    /// such a round's receipt is judged by it, so an entry is kept only while a round that began
    /// before it is in flight: one entry at most for each subject a look found moved since the
    /// oldest round still in flight began.
    moved_at: BTreeMap<Subject, Instant>,
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
    /// Look at the state under `paths` and the control journal at `journal` under `trust`, with
    /// the Commons configuration at `config` ([`Look::held_since`]), and watch their files: a
    /// change rings `bell`, the transport's doorbell, so the serving loop looks at once rather
    /// than at its next polled round. With no doorbell, or where the platform's file events
    /// cannot start, nothing is watched and the polled round's look stands alone; it also covers
    /// what a watch does not hear. The configuration is not watched: a look reads its stamp
    /// before the stores, so the look that tells a write made after a change counts the change.
    /// The transport starts this before it begins any round, so no receipt it is told of comes
    /// from a round that began before this first look ([`Self::take`]).
    pub(super) fn start(
        paths: &Paths,
        journal: &Path,
        config: &Path,
        trust: Trust,
        bell: Option<Arc<Doorbell>>,
    ) -> Self {
        let mut moves = Self {
            look: Some(Look::new(paths, journal, config, trust)),
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
    pub(super) fn unwatched(paths: &Paths, journal: &Path, config: &Path, trust: Trust) -> Self {
        Self::start(paths, journal, config, trust, None)
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

    /// A round that began at `began` and was joined since the last look brought from `actor`
    /// `count` of what `subject` names that this Mac did not hold (a table's records, the
    /// control journal's nodes, or the membership records, which count none), by an exchange of
    /// it that completed both ways (`super::transport::Transport::finish` tells none that broke
    /// off): if the next look finds it moved by no more than such rounds brought together, and no
    /// look since the round began found it moved, that look names the seat a source of the move,
    /// which holds what its round carried both ways ([`Self::take`]).
    pub(super) fn received(&mut self, subject: Subject, actor: Actor, began: Instant, count: u64) {
        if self.look.is_some() && self.sources.len() < MAX_SOURCES {
            self.sources.push((subject, actor, began, count));
        }
    }

    /// Whether a look since the last call found the configuration's policy selecting what it did
    /// not before ([`Look::widened`]), told once: the next call says false until a look finds it
    /// again.
    pub(super) fn widened(&mut self) -> bool {
        self.look
            .as_mut()
            .is_some_and(|look| std::mem::take(&mut look.widened))
    }

    /// When this Mac took up a store of `table` here, as its look tells ([`Look::held_since`]);
    /// none where this looks at nothing or no look has found one.
    pub(super) fn held_since(&self, table: &Digest) -> Option<Instant> {
        self.look.as_ref()?.held_since(table)
    }

    /// Look at `now`, if a look is due (the polled round asked, a round received, the watch
    /// rang): what moved, each with where it came from ([`Source`]): the seats it names its
    /// sources, of those it was received from since the last look ([`Self::received`]), or this
    /// Mac. A round that began before an earlier look found the same subject moved is no source
    /// of this move: that look, run while the round was in flight, may have found what the round
    /// added before its receipt came, so its count says nothing of what this look found, and the
    /// round may have carried no earlier move (a long round that brought a control node while a
    /// revocation was written here). Such a seat is walked like any other
    /// ([`super::discovery::Hints::moved`]). Of the rest, where the look found a table or the
    /// control journal grown by more than they brought of it together ([`Look::grown`]),
    /// something else moved it too (a write here made before the look by this process, a command
    /// or another process, or a round that reported no receipt of it), which those seats may
    /// lack: the move names none of them ([`Source::Here`]), and the walk offers it to those
    /// seats as to any other. A record a round brought that this Mac already held (another of its
    /// rounds brought it first) brings nothing ([`Self::received`]), so it never covers such a
    /// write. Past [`MAX_SOURCES`] receipts the rest are not counted, which reads the same way.
    ///
    /// A seat named holds what its own round carried both ways: what it sent, and what this Mac
    /// held when that round read its inventory. Where two named seats' rounds ran at once, a
    /// record one of them brought after the other's round read the inventory may be missing
    /// from that other seat, which the walk then does not offer it to. That is the residue: the
    /// seat that brought the record offers it on in its own walk, and the next audit between
    /// this Mac and the seat that lacks it brings it at the latest.
    ///
    /// `now` is the turn's instant, which each round the turn begins after the look bears, so a
    /// round that began at it began after the look read. `oldest` is when the oldest round still
    /// in flight began, none where none is: every receipt still to come is from such a round or
    /// one that begins at `now` or later, so when a look found a subject moved judges no receipt
    /// once no round that began before it is in flight, and is not kept past then.
    pub(super) fn take(&mut self, now: Instant, oldest: Option<Instant>) -> Vec<(Subject, Source)> {
        let sources = std::mem::take(&mut self.sources);
        let rang = self.rang.swap(false, Ordering::AcqRel);
        let due = std::mem::take(&mut self.due);
        let Some(look) = &mut self.look else {
            return Vec::new();
        };
        if !(due || rang || !sources.is_empty()) {
            return Vec::new();
        }
        #[cfg(test)]
        let (hold_all, stale_hold, kept) = (
            SOURCES_HOLD_ALL.with(std::cell::Cell::get),
            STALE_SOURCES_HOLD.with(std::cell::Cell::get),
            MOVES_KEPT.with(std::cell::Cell::get),
        );
        #[cfg(not(test))]
        let (hold_all, stale_hold, kept) = (false, false, false);
        let mut told = Vec::new();
        for subject in look.moved() {
            let earlier = self.moved_at.insert(subject, now);
            let from: Vec<_> = sources
                .iter()
                .filter(|(received, _, began, _)| {
                    *received == subject
                        && (stale_hold || earlier.is_none_or(|seen| *began >= seen))
                })
                .collect();
            let brought = from
                .iter()
                .fold(0_u64, |sum, (.., count)| sum.saturating_add(*count));
            if !hold_all && look.grown(&subject).is_some_and(|grown| grown > brought) {
                told.push((subject, Source::Here));
                continue;
            }
            told.push((
                subject,
                Source::Seats(from.into_iter().map(|(_, actor, ..)| *actor).collect()),
            ));
        }
        if !kept {
            self.moved_at
                .retain(|_, seen| oldest.is_some_and(|began| *seen > began));
        }
        told
    }
}

#[cfg(test)]
#[path = "moved_tests.rs"]
mod tests;
