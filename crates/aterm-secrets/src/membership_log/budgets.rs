// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record budgets a membership [`Log`](super::Log) holds its records within: two disjoint
//! ones by what the quorum weighing reads, and the held total.
//!
//! - **Weighed** records ([`Kind::weighed`]: `Genesis`, `Admit`, `Remove`) are what the quorum
//!   weighing reads. Its candidates and their 1 MiB relation payload (`eval/weigh/relations.rs`)
//!   are sized by [`MAX_WEIGHED_RECORDS`], so no flood of other records grows the weighing past
//!   it.
//! - **Keyed** records (every other kind: `Epoch`, `Wrap`, `Put`, `Delete`, `Node`, `Name`) carry
//!   keys, secrets and names; the weighing never reads them. They hold at most
//!   [`MAX_KEYED_RECORDS`].
//! - **Held** is both together, [`MAX_HELD_RECORDS`]: also the record bound of every stream a
//!   member reads (a records file, a pull's answer, a POST).
//!
//! The two budgets keep a flood of one side from taking the other's room (no flood of keyed
//! records takes the place of a `Remove`, and no flood of `Admit`s and `Remove`s shuts out a
//! rotation's `Wrap`s) only where the held total is the two together. While it is less, it binds
//! first and refuses every kind alike once full; at [`LEGACY_RECORDS`], no more than either
//! budget, neither budget refuses a record before it does.
//!
//! [`RecordBudgets::PRODUCTION`] is the one set every member holds; a test makes smaller ones
//! with [`RecordBudgets::within`], so each budget can be filled alone.
//!
//! The byte bound ([`held_bytes_bound`]) is a budget by who authors ([`Class`]) where this Mac
//! authors a record, and only there: an automatic writer stops at seven eighths of it, a
//! person's write at fifteen sixteenths, and membership (a removal with the re-keys it owes, an
//! admission, a delete) runs to the bound, so what fills the log first is never what a lab needs
//! to take a Mac out or let one in ([`author_stop`], [`Authoring`]). A credential is a person's
//! write: the sixteenth above the person's stop is for removals alone. An automatic writer also
//! signs no `Put` over [`AUTOMATIC_PUT_LIMIT`] at any fill, and no more than
//! [`AUTOMATIC_PUT_BUDGET`] of `Put`s a day from one seat. One act is one class, its most
//! critical part's: what a membership write cannot land without is never stopped before it. So
//! a heal's `Wrap`s and a re-key a writer or a reader waits on are asked no class (their own
//! reserve bounds them, as before), and a write that waits for a key is asked once, where the
//! command queues it, never again where a vault pass signs it. [`Log::insert`](super::Log),
//! a store's ingest and every decode take a valid record at any fill, as before: a store that
//! refused a peer's record by its class would hold other records than that peer. A build that
//! knows no classes holds and serves the same records, and does not obey the stops when it
//! writes. [`Fill`] is the one reading of all of it: the bytes of the bound, by kind, what each
//! seat signed in a day, which classes are open and when the bound comes at that rate.

use super::{Kind, MAX_RECORDS_STREAM, MemberId, Record};

/// The most weighed records ([`Kind::weighed`]) a [`Log`](super::Log) holds, the genesis among
/// them. Each quorum candidate is a group of at least two `Remove`s, so the weighing has at most
/// `(MAX_WEIGHED_RECORDS - 1) / 2` candidates, whose relations its 1 MiB payload is sized for.
pub const MAX_WEIGHED_RECORDS: usize = 4096;
/// The most keyed records (every kind [`Kind::weighed`] is not) a [`Log`](super::Log) holds.
pub const MAX_KEYED_RECORDS: usize = 12_288;
/// The keyed records past which an automatic batch no writer waits on waits for checkpoints
/// (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §8, "The 8,500 stop"): the ranked tender's re-key
/// of an audience that no held `Remove` made due (a coalesce of concurrent heads after no
/// removal, an F21 reseal's re-`Put`s) is not signed where the log's keyed records and the
/// batch's planned ones together would pass it, or where it would take the room the re-keys a
/// held `Remove` made due still owe, and stays due. A heal, a person's act and every batch a held
/// `Remove` made due never wait on the stop: writers wait on those (one-mesh M4), and their room
/// is kept instead, as each way a person removes a Mac (`members remove`, `secrets revoke`, a
/// `code-only` line) refuses up front where its own batch and the worst case of every re-key owed
/// after it would pass [`MAX_KEYED_RECORDS`] or the byte bound (`client/remove.rs`'s
/// `RemovalFill`), a heal signs only the `Wrap`s that leave that room free, and a seat move is
/// signed only below the proof-admission stop. The stop leaves 3,788 keyed records under that bound,
/// about 57 removals' worth at the 66 records one costs once the fold lands; the proof-admission
/// stop (`client/readmit.rs`'s `PROVEN_FILL` of the keyed budget, 9,216) lies between.
/// [`RecordBudgets::automatic_keyed_stop`] scales it to a test's smaller budgets.
pub const AUTOMATIC_KEYED_STOP: usize = 8_500;
const _: () = assert!(AUTOMATIC_KEYED_STOP < MAX_KEYED_RECORDS);
/// The most records a 0.146-0.152 build takes: it reads a records stream (a pull's answer, a
/// POST, a records file) and a Commons control inventory (`commons::journal::MAX_NODES`) of at
/// most this many, and refuses a longer one whole.
pub const LEGACY_RECORDS: usize = 4096;
#[cfg(unix)]
const _: () = assert!(LEGACY_RECORDS == crate::commons::journal::MAX_NODES);
/// The most records a [`Log`](super::Log) holds in all, which a [`Store`](super::Store) counts
/// its waiting records in too, and the record bound of every stream it reads or writes.
///
/// Modern peers negotiate this capacity before exchanging crossed history. Legacy wires keep
/// [`LEGACY_RECORDS`], and the local storage layout pins its required reader capacity before
/// publishing larger custody. Both disjoint budgets can therefore use their full share.
pub const MAX_HELD_RECORDS: usize = MAX_WEIGHED_RECORDS + MAX_KEYED_RECORDS;

impl Kind {
    /// Every kind, in the order a [`Fill`] lists them.
    pub const ALL: [Self; 9] = [
        Kind::Genesis,
        Kind::Admit,
        Kind::Remove,
        Kind::Name,
        Kind::Epoch,
        Kind::Wrap,
        Kind::Put,
        Kind::Delete,
        Kind::Node,
    ];

    /// What a [`Fill`] calls records of this kind.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Kind::Genesis => "genesis",
            Kind::Admit => "admit",
            Kind::Remove => "remove",
            Kind::Name => "name",
            Kind::Epoch => "epoch",
            Kind::Wrap => "wrap",
            Kind::Put => "put",
            Kind::Delete => "delete",
            Kind::Node => "node",
        }
    }

    /// Whether the quorum weighing reads records of this kind, which count against the weighed
    /// budget ([`MAX_WEIGHED_RECORDS`]); every other kind counts against the keyed one
    /// ([`MAX_KEYED_RECORDS`]). Every kind is named, so a new one is classified where it is
    /// added.
    #[must_use]
    pub const fn weighed(self) -> bool {
        match self {
            Kind::Genesis | Kind::Admit | Kind::Remove => true,
            Kind::Name | Kind::Epoch | Kind::Wrap | Kind::Put | Kind::Delete | Kind::Node => false,
        }
    }
}

/// One bound a [`Log`](super::Log) holds its records within: a [`RecordBudgets`] budget, or the
/// byte bound of [`MAX_RECORDS_STREAM`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Budget {
    /// Every held record ([`RecordBudgets::held`]).
    Held,
    /// The weighed records ([`RecordBudgets::weighed`]).
    Weighed,
    /// The keyed records ([`RecordBudgets::keyed`]).
    Keyed,
    /// The held records' wire bytes ([`MAX_RECORDS_STREAM`]).
    Bytes,
}

impl Budget {
    /// What the budget counts, as a refusal names it.
    #[must_use]
    pub const fn counts(self) -> &'static str {
        match self {
            Budget::Held => "records",
            Budget::Weighed => "weighed records (Genesis, Admit, Remove)",
            Budget::Keyed => "keyed records (Epoch, Wrap, Put, Delete, Node, Name)",
            Budget::Bytes => "record bytes",
        }
    }
}

/// How many records count against each budget: held ones, or a batch about to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordCounts {
    /// Records of a kind [`Kind::weighed`] names.
    pub weighed: usize,
    /// Records of every other kind.
    pub keyed: usize,
}

impl RecordCounts {
    /// One record of `kind`.
    #[must_use]
    pub const fn of(kind: Kind) -> Self {
        if kind.weighed() {
            Self {
                weighed: 1,
                keyed: 0,
            }
        } else {
            Self {
                weighed: 0,
                keyed: 1,
            }
        }
    }

    /// Both counts together.
    #[must_use]
    pub const fn held(self) -> usize {
        self.weighed.saturating_add(self.keyed)
    }

    /// These counts and `more`'s.
    #[must_use]
    pub const fn plus(self, more: Self) -> Self {
        Self {
            weighed: self.weighed.saturating_add(more.weighed),
            keyed: self.keyed.saturating_add(more.keyed),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// MUTANT of [`counted`], on for one test thread: every record counts as weighed, as a log
    /// that judged the keyed kinds by the weighed budget would.
    pub(crate) static KEYED_AS_WEIGHED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// What one record of `kind` adds to a [`Log`](super::Log)'s counts, held or judged against its
/// budgets: [`RecordCounts::of`]. Every count the log keeps, and every record it judges as it
/// inserts or lands one, goes through it; a batch a caller counts by hand and judges against
/// the log's counts ([`Log::passed_by`](super::Log::passed_by) for a proof's admission,
/// [`RecordBudgets::passed_by`] for a Commons recovery) does not.
pub(super) fn counted(kind: Kind) -> RecordCounts {
    #[cfg(test)]
    if KEYED_AS_WEIGHED.with(std::cell::Cell::get) {
        return RecordCounts::of(Kind::Admit);
    }
    RecordCounts::of(kind)
}

/// The record budgets of one [`Log`](super::Log), which it keeps for its life (and its clones
/// keep): the weighed and keyed budgets, disjoint by [`Kind::weighed`], and the held total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordBudgets {
    /// The most weighed records held ([`MAX_WEIGHED_RECORDS`]).
    pub weighed: usize,
    /// The most keyed records held ([`MAX_KEYED_RECORDS`]).
    pub keyed: usize,
    /// The most records held in all ([`MAX_HELD_RECORDS`]); a [`Store`](super::Store) counts
    /// its records waiting on a parent in it too.
    pub held: usize,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The budgets [`RecordBudgets::within`] set on this thread.
    static WITHIN: std::cell::Cell<Option<RecordBudgets>> = const { std::cell::Cell::new(None) };
    /// The byte bound [`RecordBudgets::within_bytes`] set on this thread.
    static WITHIN_BYTES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The wire bytes a [`Log`](super::Log)'s held records stay within ([`Budget::Bytes`]):
/// [`MAX_RECORDS_STREAM`], the bound of every stream a member reads, or in a test or debug build
/// the smaller one [`RecordBudgets::within_bytes`] set on this thread, so a test fills it with a
/// few records.
#[must_use]
pub fn held_bytes_bound() -> usize {
    #[cfg(any(test, debug_assertions))]
    if let Some(bytes) = WITHIN_BYTES.with(std::cell::Cell::get) {
        return bytes;
    }
    MAX_RECORDS_STREAM
}

impl RecordBudgets {
    /// The budgets every member holds its log within.
    pub const PRODUCTION: Self = Self {
        weighed: MAX_WEIGHED_RECORDS,
        keyed: MAX_KEYED_RECORDS,
        held: MAX_HELD_RECORDS,
    };

    /// The budgets a log or store made now takes: [`RecordBudgets::PRODUCTION`], or in a test or
    /// debug build the ones [`RecordBudgets::within`] set on this thread.
    #[must_use]
    pub fn current() -> Self {
        #[cfg(any(test, debug_assertions))]
        if let Some(budgets) = WITHIN.with(std::cell::Cell::get) {
            return budgets;
        }
        Self::PRODUCTION
    }

    /// Run `run` with every [`Log`](super::Log) and [`Store`](super::Store) it makes on this
    /// thread held within these budgets: a test's small ones, so each budget fills alone, through
    /// whatever loads or builds a log (a records file, a removal plan, a proof's admission). The
    /// budgets before come back when `run` returns or unwinds.
    ///
    /// The held total stays within the two budgets together; tests can choose a limit on either
    /// side of [`LEGACY_RECORDS`] (`crate::crossing`). Network streams stay bounded by
    /// [`MAX_HELD_RECORDS`]. Local-file recovery uses the receiving store's held budget, so such
    /// a log can be written and reopened without losing its tail.
    ///
    /// # Panics
    /// The weighed or keyed budget is larger than [`RecordBudgets::PRODUCTION`]'s (the weighing
    /// is sized for those), or the held total is larger than the two together.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn within<T>(self, run: impl FnOnce() -> T) -> T {
        struct Restore(Option<RecordBudgets>);
        impl Drop for Restore {
            fn drop(&mut self) {
                WITHIN.with(|within| within.set(self.0));
            }
        }
        assert!(
            self.weighed <= MAX_WEIGHED_RECORDS
                && self.keyed <= MAX_KEYED_RECORDS
                && self.held <= MAX_WEIGHED_RECORDS + MAX_KEYED_RECORDS,
            "a test's record budgets stay within the production weighed and keyed ones"
        );
        let _restore = Restore(WITHIN.with(|within| within.replace(Some(self))));
        run()
    }

    /// Run `run` with every [`Log`](super::Log) on this thread holding its records within
    /// `bytes` wire bytes ([`held_bytes_bound`]) rather than [`MAX_RECORDS_STREAM`]: a test's
    /// smaller byte bound, which a few large records fill. The class stops ([`author_stop`]) are
    /// the same shares of it, so a few records pass each stop in turn. The bound before comes
    /// back when `run` returns or unwinds.
    ///
    /// # Panics
    /// `bytes` is larger than [`MAX_RECORDS_STREAM`].
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn within_bytes<T>(bytes: usize, run: impl FnOnce() -> T) -> T {
        struct Restore(Option<usize>);
        impl Drop for Restore {
            fn drop(&mut self) {
                WITHIN_BYTES.with(|within| within.set(self.0));
            }
        }
        assert!(
            bytes <= MAX_RECORDS_STREAM,
            "a test's byte bound stays within the production one"
        );
        let _restore = Restore(WITHIN_BYTES.with(|within| within.replace(Some(bytes))));
        run()
    }

    /// [`AUTOMATIC_KEYED_STOP`] scaled to this keyed budget as it is to [`MAX_KEYED_RECORDS`],
    /// rounded down: exactly 8,500 under [`RecordBudgets::PRODUCTION`], and the same share of a
    /// test's smaller budget ([`RecordBudgets::within`]), so the stop lies as far below the hard
    /// bound there as it does in the lab.
    #[must_use]
    pub const fn automatic_keyed_stop(self) -> usize {
        self.keyed.saturating_mul(AUTOMATIC_KEYED_STOP) / MAX_KEYED_RECORDS
    }

    /// The first budget `more` records past `held` would pass, the held total first; `None` where
    /// they fit every one. A budget `more` adds nothing to is not judged.
    #[must_use]
    pub const fn passed_by(self, held: RecordCounts, more: RecordCounts) -> Option<Budget> {
        let after = held.plus(more);
        if more.held() > 0 && after.held() > self.held {
            Some(Budget::Held)
        } else if more.weighed > 0 && after.weighed > self.weighed {
            Some(Budget::Weighed)
        } else if more.keyed > 0 && after.keyed > self.keyed {
            Some(Budget::Keyed)
        } else {
            None
        }
    }

    /// [`RecordBudgets::passed_by`], and [`Budget::Bytes`] where `held_bytes` and `more_bytes`
    /// together pass [`held_bytes_bound`] ([`MAX_RECORDS_STREAM`]).
    #[must_use]
    pub fn passed_with_bytes(
        self,
        held: RecordCounts,
        more: RecordCounts,
        held_bytes: usize,
        more_bytes: usize,
    ) -> Option<Budget> {
        match self.passed_by(held, more) {
            Some(budget) => Some(budget),
            None if held_bytes.saturating_add(more_bytes) > held_bytes_bound() => {
                Some(Budget::Bytes)
            }
            None => None,
        }
    }
}

/// The most wire bytes of a `Put` record an automatic writer signs, at any fill: a size limit,
/// not a class ([`Authoring::refusal`]). The repository inventory that filled a lab's log was one
/// 299 KB `Put`, signed again at every look.
pub const AUTOMATIC_PUT_LIMIT: usize = 64 * 1024;
/// The `Put` bytes a seat signs in [`AUTOMATIC_WINDOW_MS`] past which an automatic writer on it
/// signs no more ([`Log::author_ask`]). A person's own write is never held to it.
pub const AUTOMATIC_PUT_BUDGET: usize = 1024 * 1024;
/// The window of [`AUTOMATIC_PUT_BUDGET`] and of a [`Fill`]'s rate: 24 hours.
pub const AUTOMATIC_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
/// How far past the reader's clock a record's own clock may read and still be in that window:
/// ten minutes of skew between two Macs. A record stamped later is in no window until the
/// reader's clock reaches it.
pub const WINDOW_SKEW_MS: u64 = 10 * 60 * 1000;

/// Who authors a record on this Mac: what an authoring call is told, never guessed from the
/// record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Author<'a> {
    /// A person's own command (`aterm secrets set`, an edit, `members remove`, an approval).
    Person,
    /// A writer no person started, by the name a refusal gives it: the repository inventory's
    /// upkeep, the account broker's publish.
    Automatic(&'a str),
}

/// The class of a batch this Mac authors, by who authors it and whether it changes who is in
/// the lab, which sets how full the log may be when it is signed ([`author_stop`]). The order is
/// the order they stop in, automatic first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// What no person started: every batch of an automatic writer ([`Author::Automatic`]),
    /// whatever it holds, and a re-key no held `Remove` made due that neither a writer nor a
    /// reader waits on (an F21 reseal of an audience every seat already opens whole,
    /// [`RekeyDue::waited_on`](super::RekeyDue::waited_on)).
    Automatic,
    /// A person's own write: every `Put` an edit or a `set` changes, a credential's and a
    /// control record's too, and a `Name`.
    Person,
    /// What takes a Mac out or lets one in: a `Remove` and the re-keys it owes, an `Admit`, its
    /// `Wrap`s and the `Node` that binds the admitted Mac's key, a `Delete` and the reseal it
    /// owes, and a person's edit that only changes who opens an audience, with the entries it
    /// seals again for them.
    Membership,
}

impl Class {
    /// Every class, in the order they stop.
    pub const ALL: [Self; 3] = [Self::Automatic, Self::Person, Self::Membership];

    /// The class's place in [`Class::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Automatic => 0,
            Self::Person => 1,
            Self::Membership => 2,
        }
    }

    /// The class as a [`Fill`] names it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Person => "person",
            Self::Membership => "membership",
        }
    }

    /// A write of the class, as a refusal names it.
    #[must_use]
    pub const fn write(self) -> &'static str {
        match self {
            Self::Automatic => "an automatic write",
            Self::Person => "a person's write",
            Self::Membership => "a membership write",
        }
    }

    /// What a refusal says the class holds.
    #[must_use]
    pub const fn holds(self) -> &'static str {
        match self {
            Self::Automatic => {
                "one no person started: the repository inventory's upkeep, the account broker's \
                 publish, a re-key nothing waits on"
            }
            Self::Person => "a person's own Put, a credential's too, or a name",
            Self::Membership => {
                "a removal and the re-keys it owes, an admission and its node key, a delete"
            }
        }
    }

    /// The share of the byte bound the class stops at: seven eighths, fifteen sixteenths, all
    /// of it.
    const fn share(self) -> (usize, usize) {
        match self {
            Self::Automatic => (7, 8),
            Self::Person => (15, 16),
            Self::Membership => (1, 1),
        }
    }

    /// Where the class stops in a byte bound of `bound`.
    #[must_use]
    pub const fn stop_within(self, bound: usize) -> usize {
        let (share, of) = self.share();
        bound / of * share
    }

    /// The class of what `author` writes: a `Put`, of any name and size. A credential's `Put`
    /// is a person's write as any other is: it never draws on the room above the person's stop,
    /// which is kept for removals.
    #[must_use]
    pub const fn of_author(author: Author<'_>) -> Self {
        match author {
            Author::Automatic(_) => Self::Automatic,
            Author::Person => Self::Person,
        }
    }

    /// The class a person's record has by its kind alone; `None` for a `Put`, which is a
    /// person's write where the edit changed it and carries its batch where the edit only
    /// seals it again ([`Authoring`]), and for an `Epoch` and a `Wrap`, which carry what their
    /// batch seals and take its class. A `Node` is membership: it binds the fleet key of a Mac
    /// the log let in, and an admission whose Mac no peer admits is half done. Every kind is
    /// named, so a new one is classed where it is added.
    #[must_use]
    pub const fn of_kind(kind: Kind) -> Option<Self> {
        match kind {
            Kind::Genesis | Kind::Admit | Kind::Remove | Kind::Delete | Kind::Node => {
                Some(Self::Membership)
            }
            Kind::Name => Some(Self::Person),
            Kind::Epoch | Kind::Wrap | Kind::Put => None,
        }
    }
}

/// Where an automatic writer stops in the production bound: 56 MiB of [`MAX_RECORDS_STREAM`]'s
/// 64.
pub const AUTOMATIC_STOP: usize = Class::Automatic.stop_within(MAX_RECORDS_STREAM);
/// Where a person's write stops in the production bound: 60 MiB of its 64.
pub const PERSON_STOP: usize = Class::Person.stop_within(MAX_RECORDS_STREAM);
const _: () = assert!(AUTOMATIC_STOP < PERSON_STOP && PERSON_STOP < MAX_RECORDS_STREAM);

/// The held bytes past which this Mac authors no batch of `class`: its share of
/// [`held_bytes_bound`] ([`Class::stop_within`]: [`AUTOMATIC_STOP`], [`PERSON_STOP`] and the
/// bound itself in production; the same shares of a test's smaller bound).
#[must_use]
pub fn author_stop(class: Class) -> usize {
    class.stop_within(held_bytes_bound())
}

/// A batch this Mac is about to author, as the class stops weigh it: judged whole, since a
/// batch is kept whole or not at all. An automatic writer's batch is automatic whatever it
/// holds; a person's is judged by the least critical record the person changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Authoring {
    /// The wire bytes of its records.
    pub bytes: usize,
    /// The wire bytes of its `Put`s, which an automatic writer's day budget counts.
    pub put_bytes: usize,
    /// The wire bytes of the largest `Put` its author changed, which an automatic writer's size
    /// limit judges ([`AUTOMATIC_PUT_LIMIT`]). A `Put` that only seals again an entry the edit
    /// left alone is not in it: its size is no choice of this writer's.
    pub largest_put: usize,
    /// The least critical class among its classed records; `None` while it holds only `Epoch`s
    /// and `Wrap`s, and `Put`s that seal again what the edit left alone.
    least: Option<Class>,
    /// The least wire bytes of this Mac's `[everyone]` writes that wait for a key beside this
    /// batch ([`Self::beside_waiting`]), which a vault pass signs later with no ask.
    waiting: usize,
}

impl Authoring {
    /// `records`, each of `class`: a batch whose reason classes it whole (a removal's, an
    /// admission's, a re-key no removal made due).
    #[must_use]
    pub fn all(records: &[Record], class: Class) -> Self {
        Self::classed(records, |_| Some(class))
    }

    /// `records`, each of the class `class_of` gives it. One it gives none (an `Epoch` or a
    /// `Wrap` of a batch that seals entries, a `Put` that seals again an entry the edit did not
    /// change) carries the batch and takes its class.
    #[must_use]
    pub fn classed(records: &[Record], mut class_of: impl FnMut(&Record) -> Option<Class>) -> Self {
        let mut batch = Self::default();
        for record in records {
            let bytes = record.bytes().len();
            let class = class_of(record);
            batch.bytes = batch.bytes.saturating_add(bytes);
            if record.kind() == Kind::Put {
                batch.put_bytes = batch.put_bytes.saturating_add(bytes);
                if class.is_some() {
                    batch.largest_put = batch.largest_put.max(bytes);
                }
            }
            if let Some(class) = class {
                batch.least = Some(batch.least.map_or(class, |least| least.min(class)));
            }
        }
        batch
    }

    /// One more `Put` of at most `bytes` wire bytes and of `class`, not signed yet: a write that
    /// waits for a key, weighed where its command queues it (`client::log_entries`' waiting
    /// writes), since the pass that signs it later asks nothing.
    #[must_use]
    pub fn with_put(mut self, bytes: usize, class: Class) -> Self {
        self.bytes = self.bytes.saturating_add(bytes);
        self.put_bytes = self.put_bytes.saturating_add(bytes);
        self.largest_put = self.largest_put.max(bytes);
        self.least = Some(self.least.map_or(class, |least| least.min(class)));
        self
    }

    /// The class a person's batch is judged by: its least critical record's, and a person's
    /// write where it only mints or wraps keys and its author said no more ([`Self::keys_as`]).
    #[must_use]
    pub fn class(&self) -> Class {
        self.least.unwrap_or(Class::Person)
    }

    /// The class the batch is judged by from `author`: automatic from an automatic writer,
    /// whatever it holds, and [`Self::class`] from a person.
    #[must_use]
    pub fn class_from(&self, author: Author<'_>) -> Class {
        match author {
            Author::Automatic(_) => Class::Automatic,
            Author::Person => self.class(),
        }
    }

    /// This batch, judged as `class` where it holds nothing classed: what its `Epoch`s and
    /// `Wrap`s are for, which only its author knows. A person's edit that only wraps keys to a
    /// Mac the person listed lets that Mac in, and is membership as an admission is.
    #[must_use]
    pub fn keys_as(mut self, class: Class) -> Self {
        self.least.get_or_insert(class);
        self
    }

    /// This batch, beside `bytes` of this Mac's `[everyone]` writes that wait for a key and that
    /// it does not replace: the vault pass signs them later and asks nothing, so a write that
    /// waits holds its room from when its command queued it. An automatic writer's or a person's
    /// batch counts them as if the log held them already, so writes queued one by one never
    /// pass the stop together. A membership batch does not: it runs to the bound whatever
    /// waits.
    #[must_use]
    pub const fn beside_waiting(mut self, bytes: usize) -> Self {
        self.waiting = bytes;
        self
    }

    /// Why a log that holds `held` bytes, whose signer's seat signed `signed` bytes of `Put`s in
    /// the last [`AUTOMATIC_WINDOW_MS`], does not take this batch from `author`: an automatic
    /// writer's size limit first, at any fill, then the class stop, then an automatic writer's
    /// day budget. `None` where it takes it.
    #[must_use]
    pub fn refusal(&self, held: usize, signed: usize, author: Author<'_>) -> Option<String> {
        if self.bytes == 0 {
            return None;
        }
        let who = match author {
            Author::Person => "this write".to_owned(),
            Author::Automatic(name) => format!("{name}'s write"),
        };
        if matches!(author, Author::Automatic(_)) && self.largest_put > AUTOMATIC_PUT_LIMIT {
            return Some(format!(
                "{who} was not signed: one Put of it is {}, and an automatic writer signs no Put \
                 over {}, at any fill of the vault log",
                size_words(self.largest_put),
                size_words(AUTOMATIC_PUT_LIMIT),
            ));
        }
        let class = self.class_from(author);
        let stop = author_stop(class);
        // What a class's stop is weighed against: the log, and for every class but membership
        // what waits here for a key too ([`Self::beside_waiting`]).
        let filled = |class: Class| match class {
            Class::Membership => held,
            Class::Automatic | Class::Person => held.saturating_add(self.waiting),
        };
        if filled(class).saturating_add(self.bytes) > stop {
            // What still fits: this class's own room, then each class that stops later.
            let fits: Vec<String> = Class::ALL
                .into_iter()
                .filter(|other| *other >= class)
                .map(|other| {
                    let room = size_words(author_stop(other).saturating_sub(filled(other)));
                    if other == class {
                        format!("{} has {room} left", other.write())
                    } else {
                        format!("{} ({}) {room}", other.write(), other.holds())
                    }
                })
                .collect();
            let waits = if self.waiting == 0 || class == Class::Membership {
                String::new()
            } else {
                format!(
                    " and this Mac's writes waiting for a key {}",
                    size_words(self.waiting)
                )
            };
            return Some(format!(
                "{who} was not signed: it is {} ({}) of {}, and {} stops at {} of the vault \
                 log's {}, which holds {}{waits}; {}",
                class.write(),
                class.holds(),
                size_words(self.bytes),
                class.write(),
                size_words(stop),
                size_words(held_bytes_bound()),
                size_words(held),
                fits.join(", "),
            ));
        }
        if matches!(author, Author::Automatic(_))
            && self.put_bytes > 0
            && signed.saturating_add(self.put_bytes) > AUTOMATIC_PUT_BUDGET
        {
            return Some(format!(
                "{who} was not signed: this Mac's seat signed {} of Puts in the last 24 hours, \
                 and this one's {} would pass the {} an automatic writer may sign in a day, which \
                 leaves it {}; a person's own write is not held to it",
                size_words(signed),
                size_words(self.put_bytes),
                size_words(AUTOMATIC_PUT_BUDGET),
                size_words(AUTOMATIC_PUT_BUDGET.saturating_sub(signed)),
            ));
        }
        None
    }
}

/// `bytes` as a person reads a size: whole bytes below 1 KiB, tenths of a KiB below 1 MiB,
/// hundredths of a MiB from there, with no trailing zeros (`64 MiB`, `50.61 MiB`, `298.6 KiB`).
#[must_use]
pub fn size_words(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;
    let trimmed = |text: String| text.trim_end_matches('0').trim_end_matches('.').to_owned();
    // Hundredths (or tenths) by integer division, rounded down: a size is never read as more
    // room, or less fill, than there is by a float's rounding.
    if bytes < KIB {
        format!("{bytes} bytes")
    } else if bytes < MIB {
        let tenths = bytes.saturating_mul(10) / KIB;
        format!(
            "{} KiB",
            trimmed(format!("{}.{}", tenths / 10, tenths % 10))
        )
    } else {
        let hundredths = bytes / MIB * 100 + bytes % MIB * 100 / MIB;
        format!(
            "{} MiB",
            trimmed(format!("{}.{:02}", hundredths / 100, hundredths % 100))
        )
    }
}

/// What one seat signed in a [`Fill`]'s window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeatBytes {
    /// The seat's member id.
    pub seat: MemberId,
    /// The wire bytes of the held records it signed in the window.
    pub bytes: usize,
}

/// What a log's held records signed in the last [`AUTOMATIC_WINDOW_MS`], read from their headers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signed {
    /// Their wire bytes.
    pub bytes: usize,
    /// Each seat that signed any, the one that signed most first, ties by member id.
    pub seats: Vec<SeatBytes>,
}

/// When a log reaches its byte bound at the rate it grew in the last [`AUTOMATIC_WINDOW_MS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projected {
    /// Not measured: the clock did not read, or no held record carries a time to measure from.
    Unread,
    /// Nothing was signed in the window: at that rate, never.
    Never,
    /// In this many milliseconds; `0` where the log is at its bound.
    In(u64),
}

/// How full a log is ([`Log::fill`](super::Log::fill)): the one reading a refusal, `aterm
/// secrets status` and its JSON share. Every number is of held records, read from their headers;
/// no body is opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fill {
    /// The wire bytes of the held records.
    pub bytes: usize,
    /// The byte bound ([`held_bytes_bound`]).
    pub bound: usize,
    /// Each kind's held records and their wire bytes, in [`Kind::ALL`]'s order.
    pub by_kind: [(Kind, usize, usize); 9],
    /// Where each class stops ([`author_stop`]), in [`Class::ALL`]'s order.
    pub stops: [usize; 3],
    /// What was signed in the last [`AUTOMATIC_WINDOW_MS`]; `None` where that is not measured
    /// ([`Projected::Unread`]).
    pub signed: Option<Signed>,
}

impl Fill {
    /// Whether this Mac still authors a record of `class`: the log holds less than its stop.
    #[must_use]
    pub fn open(&self, class: Class) -> bool {
        self.bytes < self.stops[class.index()]
    }

    /// The bytes a write of `class` still has.
    #[must_use]
    pub fn room(&self, class: Class) -> usize {
        self.stops[class.index()].saturating_sub(self.bytes)
    }

    /// When the log reaches its bound at the rate of the window.
    #[must_use]
    pub fn projected(&self) -> Projected {
        match &self.signed {
            None => Projected::Unread,
            Some(signed) if signed.bytes == 0 => Projected::Never,
            Some(signed) => {
                let wide = |bytes: usize| u128::try_from(bytes).unwrap_or(u128::MAX);
                let left = wide(self.bound.saturating_sub(self.bytes));
                let ms = left.saturating_mul(u128::from(AUTOMATIC_WINDOW_MS)) / wide(signed.bytes);
                Projected::In(u64::try_from(ms).unwrap_or(u64::MAX))
            }
        }
    }

    /// Which classes are open, as the status line says it.
    #[must_use]
    pub fn classes_words(&self) -> String {
        let refused: Vec<&str> = Class::ALL
            .into_iter()
            .filter(|class| !self.open(*class))
            .map(Class::word)
            .collect();
        match refused.as_slice() {
            [] => "every class open".to_owned(),
            [_, _, _] => "full: every class refused".to_owned(),
            some => format!("{} refused", some.join(" and ")),
        }
    }

    /// `aterm secrets status`' line, with `label` naming a seat (its `m<N>`): the log's bytes of
    /// its bound, which classes are open, the bytes signed in 24 hours with the two seats that
    /// signed most, and the time to the bound at that rate; `unread` for what is not measured.
    #[must_use]
    pub fn line(&self, label: impl Fn(&MemberId) -> String) -> String {
        let signed = match &self.signed {
            None => "unread".to_owned(),
            Some(signed) if signed.seats.is_empty() => size_words(signed.bytes),
            Some(signed) => {
                let seats: Vec<String> = signed
                    .seats
                    .iter()
                    .take(2)
                    .map(|seat| format!("{} {}", label(&seat.seat), size_words(seat.bytes)))
                    .collect();
                format!("{} ({})", size_words(signed.bytes), seats.join(", "))
            }
        };
        let full = match self.projected() {
            Projected::Unread => "unread".to_owned(),
            Projected::Never => "not at this rate".to_owned(),
            Projected::In(ms) => span_words(ms),
        };
        // One unit where the two sizes share it (`50.61 of 64 MiB`), each its own where they do
        // not (`12.3 KiB of 64 MiB`).
        let (bytes, bound) = (size_words(self.bytes), size_words(self.bound));
        let unit = bound.rsplit(' ').next().unwrap_or_default();
        let bytes = bytes
            .strip_suffix(unit)
            .map_or(bytes.as_str(), str::trim_end);
        format!(
            "log: {bytes} of {bound} · {} · signed in 24 h: {signed} · full in: {full}",
            self.classes_words(),
        )
    }
}

/// `ms` as a span a person reads: minutes below an hour, hours below two days, days from there,
/// each rounded down.
fn span_words(ms: u64) -> String {
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if ms < HOUR {
        format!("{} min", ms / MINUTE)
    } else if ms < 2 * DAY {
        format!("{} h", ms / HOUR)
    } else {
        format!("{} d", ms / DAY)
    }
}

/// A test's mutant of [`Log::land_waiting`](super::Log), on for one test thread through
/// [`LANDING`].
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Landing {
    /// The landing as written.
    Sound,
    /// A ready record that did not land goes back to waiting, as a landing that only skips a
    /// record would leave it.
    KeepUnfit,
    /// Each pass takes its ready records in reverse id order.
    Reversed,
}

#[cfg(test)]
thread_local! {
    /// The [`Landing`] this test thread's logs land waiting records by.
    pub(super) static LANDING: std::cell::Cell<Landing> =
        const { std::cell::Cell::new(Landing::Sound) };
}

/// The order a pass of [`Log::land_waiting`](super::Log) takes its `ready` records in, given in
/// id order: reversed under [`Landing::Reversed`].
#[cfg(test)]
pub(super) fn mutant_order(mut ready: Vec<super::RecordId>) -> Vec<super::RecordId> {
    if LANDING.with(std::cell::Cell::get) == Landing::Reversed {
        ready.reverse();
    }
    ready
}

/// Under [`Landing::KeepUnfit`], the ready `record` that did not land goes back to waiting.
#[cfg(test)]
pub(super) fn mutant_keeps_unfit(log: &mut super::Log, record: super::Record) {
    if LANDING.with(std::cell::Cell::get) == Landing::KeepUnfit {
        log.wait(record);
    }
}

impl Default for RecordBudgets {
    /// [`RecordBudgets::current`].
    fn default() -> Self {
        Self::current()
    }
}
