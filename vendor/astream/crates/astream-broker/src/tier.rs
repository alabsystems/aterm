// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The retained records' RESIDENCY TIER: which committed records a [`BrokerLog`] keeps
//! decoded in memory, and where on the log file every one of them starts.
//!
//! Before this module the log held EVERY retained record decoded in RAM
//! (`records: Vec<Arc<BrokerRecord>>`) for its whole open, so a broker's resident
//! memory grew with everything it had ever been sent and not yet compacted away: a
//! fleet left running for days paid for its whole history in RAM, though readers
//! almost always want the newest records (a live tail, a resume a little behind it).
//!
//! The tier keeps every retained record's summary in PAGES of [`PAGE`] records
//! (`docs/DESIGN-aterm-fabric.md` §14, open question 8, part c). A page holds where its
//! first record's frame starts and the broker time (`bt=`) of its first record that has
//! one, and the ids of the subjects its records name; four column vectors, indexed as the
//! slots were, hold an entry a record: its frame start and its broker time as `u32`s from
//! its page's, its plaintext-frame bytes with the has-time and hidden flags, and its subject
//! as a `u8` index into its page's ids. That is 13 bytes a record where the slot vector
//! before it (`tier_reference.rs`) took 24, in four allocations rather than one a page, and
//! every answer is the slot's: a record's frame start is one column read from its page's,
//! so a cold read is still one frame. A value no column can hold (a frame start or broker time
//! below its page's first, or `u32::MAX` or more past it) is kept whole in a map by the
//! record's offset; a log's frames are contiguous and a page spans at most 255 of them, so
//! no frame start of a real log goes there. A record's offset is not stored: record `i` is
//! offset `base + i`, which every reader of the tier relies on.
//!
//! With a finite byte budget ([`Tier::with_budget`]), the tier keeps DECODED records only
//! for a hot window at the head; the newest always stays, even when it exceeds the budget.
//! The budget does not bound the pages or other broker memory, which grow with history. A
//! record outside the window is read from its frame on demand
//! ([`BrokerLog::record_arc`](crate::BrokerLog)), through the same frame decode, at-rest open
//! and record parse the open's recovery scan applies, so a cold read returns exactly the
//! record the open would have recovered.
//!
//! WHAT IS NEVER EVICTED: a record nothing has made durable is never written cold,
//! because every record the tier holds was promoted by a write that put its frame at
//! its recorded position first (`commit_end`, `append_frame`, the open's scan, the
//! retention rewrite). Positions are therefore always valid for the file the log
//! currently holds. A retention rewrite replaces the file and rebuilds the tier with
//! the new positions in the same step.
//!
//! The default budget is unbounded ([`Tier::RESIDENT_ALL`]), which is exactly the old
//! behaviour; an embedder opts into a bound through `OpenOptions`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use crate::brecord::{plaintext_frame_bytes, BrokerRecord};
use crate::store::is_hidden_record;

/// The slot vector the pages replaced, kept for the differential tests.
#[cfg(test)]
#[path = "tier_reference.rs"]
mod reference;

/// The pages against the slot vector over random streams, and the negative controls.
#[cfg(test)]
#[path = "tier_paged_tests.rs"]
mod paged_tests;

/// What a tier's readers and writers did, counted: the type in every build, the counting in a
/// test or debug build.
#[path = "tier_tally.rs"]
mod tally;
pub use tally::TierTally;

/// Count `n` more of `kind` in `tier`'s tally ([`TierTally`]); nothing in a shipped build,
/// which keeps no tally.
macro_rules! count {
    ($tier:expr, $kind:ident, $n:expr) => {
        #[cfg(any(test, debug_assertions))]
        $tier.counted.add(tally::Count::$kind, $n);
    };
}

/// A retained record's summary, read out of its page: its offset, its subject, its
/// plaintext-frame bytes, and whether it is broker-internal. Retention planning, filtered
/// reads (`Fetch`, `Subscribe`), wake planning and `Stats` walk cold history by it without
/// decoding non-matching frames from the log file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Summary<'a> {
    off: u64,
    subject: &'a str,
    bytes: u64,
    hidden: bool,
}

impl<'a> Summary<'a> {
    /// The record's absolute offset.
    pub(crate) fn offset(self) -> u64 {
        self.off
    }

    /// The record's subject.
    pub(crate) fn subject(self) -> &'a str {
        self.subject
    }

    /// Plaintext-frame byte count ([`plaintext_frame_bytes`]).
    pub(crate) fn bytes(self) -> u64 {
        self.bytes
    }

    /// Whether [`is_hidden_record`] holds for this record.
    pub(crate) fn is_hidden(self) -> bool {
        self.hidden
    }
}

/// The plaintext-frame bytes of a record's `meta`: its low 30 bits.
const BYTES_MASK: u32 = (1 << 30) - 1;
/// Set in a record's `meta` when it carries a broker time.
const HAS_BT: u32 = 1 << 30;
/// Set in a record's `meta` when it is broker-internal.
const HIDDEN: u32 = 1 << 31;

// Every record the tier holds went through the frame encode or decode, whose payload is at
// most `MAX_PAYLOAD_LEN`, and its plaintext-frame bytes are a header and part of that
// payload: so they fit the meta's 30 bits with room to spare.
const _: () =
    assert!(astream_wire::HEADER_SIZE + astream_wire::MAX_PAYLOAD_LEN <= BYTES_MASK as usize);

/// log2 of the records a page holds.
const PAGE_SHIFT: u32 = 8;
/// The records a page holds.
const PAGE: usize = 1 << PAGE_SHIFT;
/// A column entry that names no value: the record's value is in the tier's map of wide
/// values, by its offset.
const WIDE: u32 = u32::MAX;

// A page names at most one subject a record, so its subject index fits a `u8`.
const _: () = assert!(PAGE <= 1 << u8::BITS);
// A log's frames are contiguous, each at most a header and `MAX_PAYLOAD_LEN`: a page's last
// frame starts at most `PAGE - 1` frames after its first, below `WIDE`.
const _: () = assert!(
    ((PAGE - 1) as u64) * ((astream_wire::HEADER_SIZE + astream_wire::MAX_PAYLOAD_LEN) as u64)
        < WIDE as u64
);

/// The subjects the pages name, each held once and named by a `u32` id, with a count of
/// the records naming it so an id no record names is freed (and its string dropped) at once.
#[derive(Debug, Default)]
struct Subjects {
    /// `names[id]`: the subject `id` names, `None` once it is free.
    names: Vec<Option<Arc<str>>>,
    /// How many records name each id.
    counts: Vec<usize>,
    /// Each held subject's id.
    ids: HashMap<Arc<str>, u32>,
    /// Ids no record names, to reuse.
    free: Vec<u32>,
}

impl Subjects {
    /// The id of `subject`, counting one more record naming it. Ids are `u32`s: a tier holds
    /// fewer than 2^32 distinct subjects, each at least one retained record and a held
    /// string, long before which the process is out of memory.
    fn take(&mut self, subject: &str) -> u32 {
        if let Some(&id) = self.ids.get(subject) {
            if let Some(c) = self.counts.get_mut(id as usize) {
                *c = c.saturating_add(1);
            }
            return id;
        }
        let name: Arc<str> = Arc::from(subject);
        let id = match self.free.pop() {
            Some(id) => {
                if let Some(slot) = self.names.get_mut(id as usize) {
                    *slot = Some(Arc::clone(&name));
                }
                if let Some(c) = self.counts.get_mut(id as usize) {
                    *c = 1;
                }
                id
            }
            None => {
                let id = u32::try_from(self.names.len()).unwrap_or(u32::MAX);
                self.names.push(Some(Arc::clone(&name)));
                self.counts.push(1);
                id
            }
        };
        self.ids.insert(name, id);
        id
    }

    /// One record naming `id` went: free the id when it was the last.
    #[cfg(any(test, feature = "retention"))]
    fn give(&mut self, id: u32) {
        let Some(c) = self.counts.get_mut(id as usize) else {
            return;
        };
        *c = c.saturating_sub(1);
        if *c > 0 {
            return;
        }
        if let Some(slot) = self.names.get_mut(id as usize) {
            if let Some(name) = slot.take() {
                self.ids.remove(&name);
            }
        }
        self.free.push(id);
    }

    fn name(&self, id: u32) -> &str {
        match self.names.get(id as usize) {
            Some(Some(name)) => name,
            _ => "",
        }
    }

    /// How many distinct subjects are held.
    #[cfg(test)]
    fn live(&self) -> usize {
        self.ids.len()
    }
}

contract_fn! {
    /// Verified byte charge (`96 + subject_len + body_len`, saturating) for a resident
    /// decoded record.
    #[must_use]
    pub(crate) const fn tier_resident_cost(subject_len: usize, body_len: usize) -> usize
    clauses [ensures result >= 96 && result >= subject_len && result >= body_len]
    {
        96usize
            .saturating_add(subject_len)
            .saturating_add(body_len)
    }
}

contract_fn! {
    /// Amortized shrink target for `Tier::hot`: when the ring holds more than `16` slots
    /// and at most a quarter are occupied (`cap > 16 && len <= cap / 4`), shrink to
    /// `max(2 * len, 8)`; otherwise return `cap` unchanged (`target < cap` iff shrinking).
    #[must_use]
    const fn hot_shrink_target(cap: usize, len: usize) -> usize
    clauses [ensures result <= cap && (cap <= 16 || result >= 8)]
    {
        if cap > 16 && len <= cap / 4 {
            let doubled = len.saturating_mul(2);
            if doubled > 8 {
                doubled
            } else {
                8
            }
        } else {
            cap
        }
    }
}

/// The bytes a decoded record is charged against the hot budget: its subject and body
/// plus a fixed overhead for the record's own fields and its `Arc`.
pub(crate) fn resident_cost(rec: &BrokerRecord) -> usize {
    tier_resident_cost(rec.subject.len(), rec.body.len())
}

/// Start index of the suffix `records[hot_start..]` that stays resident under `budget`
/// (always keeping at least the newest record when `records` is non-empty).
fn hot_window_start(records: &[BrokerRecord], budget: usize) -> usize {
    let Some(mut hot_start) = records.len().checked_sub(1) else {
        return 0;
    };
    let Some(newest) = records.get(hot_start) else {
        return 0;
    };
    let mut acc = resident_cost(newest);
    while let Some(prev) = hot_start.checked_sub(1) {
        let Some(rec) = records.get(prev) else {
            break;
        };
        let next_acc = acc.saturating_add(resident_cost(rec));
        if next_acc > budget {
            break;
        }
        acc = next_acc;
        hot_start = prev;
    }
    hot_start
}

/// A record's `meta`: its plaintext-frame bytes, [`HAS_BT`] and [`HIDDEN`].
fn meta_of(rec: &BrokerRecord, bt: Option<u64>) -> u32 {
    let mut meta = u32::try_from(plaintext_frame_bytes(rec))
        .unwrap_or(BYTES_MASK)
        .min(BYTES_MASK);
    if bt.is_some() {
        meta |= HAS_BT;
    }
    if is_hidden_record(rec) {
        meta |= HIDDEN;
    }
    meta
}

/// `value` as a column entry from `base`: its distance above it, or [`WIDE`] when that is
/// negative or does not fit below `WIDE`.
fn column(value: u64, base: u64) -> u32 {
    match value.checked_sub(base).map(u32::try_from) {
        Some(Ok(d)) if d != WIDE => d,
        _ => WIDE,
    }
}

/// The retained records' column entries, entry `j` of each for the record at place `j`
/// (`head + i` for retained record `i`): four vectors, as the slots were one, so the
/// columns cost their entries and nothing a page.
#[derive(Default)]
struct Columns {
    /// Its frame start less its page's `at0`, or [`WIDE`].
    at: Vec<u32>,
    /// Its broker time less its page's `bt0`, or [`WIDE`]; `0` when it has none.
    bt: Vec<u32>,
    /// Its plaintext-frame bytes ([`BYTES_MASK`]), [`HAS_BT`] and [`HIDDEN`].
    meta: Vec<u32>,
    /// The index of its subject in its page's `subjects`.
    subject: Vec<u8>,
}

impl Columns {
    fn push(&mut self, at: u32, bt: u32, meta: u32, subject: u8) {
        self.at.push(at);
        self.bt.push(bt);
        self.meta.push(meta);
        self.subject.push(subject);
    }

    fn reserve_exact(&mut self, n: usize) {
        self.at.reserve_exact(n);
        self.bt.reserve_exact(n);
        self.meta.reserve_exact(n);
        self.subject.reserve_exact(n);
    }

    #[cfg(test)]
    fn truncate(&mut self, n: usize) {
        self.at.truncate(n);
        self.bt.truncate(n);
        self.meta.truncate(n);
        self.subject.truncate(n);
    }

    /// Drop the first `n` entries, and release capacity once a quarter or less is used.
    #[cfg(any(test, feature = "retention"))]
    fn drain_front(&mut self, n: usize) {
        fn drain<T>(v: &mut Vec<T>, n: usize) {
            v.drain(..n.min(v.len()));
            if v.capacity() > 64 && v.len() <= v.capacity() / 4 {
                v.shrink_to(v.len().saturating_mul(2));
            }
        }
        drain(&mut self.at, n);
        drain(&mut self.bt, n);
        drain(&mut self.meta, n);
        drain(&mut self.subject, n);
    }

    fn clear(&mut self) {
        self.at.clear();
        self.bt.clear();
        self.meta.clear();
        self.subject.clear();
    }
}

/// Up to [`PAGE`] consecutive retained records: where their frame starts and broker times
/// are counted from, and the subjects they name.
struct Page {
    /// Where the page's first record's frame starts.
    at0: u64,
    /// The broker time of the page's first record that has one.
    bt0: Option<u64>,
    /// The subjects the page's records name, as ids in [`Subjects`], each once.
    subjects: Vec<u32>,
}

impl Page {
    fn new(at0: u64) -> Page {
        Page {
            at0,
            bt0: None,
            subjects: Vec::new(),
        }
    }

    /// The page's index of subject `id`, added when the page names it first (`prev`: the
    /// index of the page's record before, if any). A page names at most one subject a record
    /// it was written, so the index fits a `u8`.
    fn local(&mut self, id: u32, prev: Option<u8>) -> u8 {
        if let Some(l) = prev {
            if self.subjects.get(usize::from(l)) == Some(&id) {
                return l;
            }
        }
        let l = match self.subjects.iter().position(|&s| s == id) {
            Some(l) => l,
            None => {
                self.subjects.push(id);
                self.subjects.len().saturating_sub(1)
            }
        };
        u8::try_from(l).unwrap_or(u8::MAX)
    }
}

/// A test's mutation of one page entry, which the differential and cold-read checks must
/// catch (the negative controls of `broker.paged-tier`).
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum PageMutant {
    /// The page's first frame start and first broker time are its second record's.
    Base(usize),
    /// The pages begin one record later: every record reads its successor's entry.
    Index,
    /// A walk keeps the subject name its last record read across a page boundary, so a
    /// page's records read the last page's name for their index.
    StaleName,
}

/// The retained records of one log: every one's summary in pages, and the decoded records
/// for the hot window at the head.
pub(crate) struct Tier {
    /// The pages, oldest first: retained record `i` is at place `head + i`, in page
    /// `(head + i) >> shift`.
    pages: Vec<Page>,
    /// The column entries: entry `head + i` of each is retained record `i`'s.
    cols: Columns,
    /// Places at the front of `pages[0]` (and of `cols`) whose records a cut dropped.
    head: usize,
    /// How many records are retained.
    len: usize,
    /// log2 of the records a page holds: [`PAGE_SHIFT`], or less in a test.
    shift: u32,
    /// The offset of record 0 (meaningless while the tier is empty).
    base: u64,
    /// The frame starts no column holds, by offset.
    wide_at: BTreeMap<u64, u64>,
    /// The broker times no column holds, by offset.
    wide_bt: BTreeMap<u64, u64>,
    /// The decoded hot window: `hot[j]` is record index `len - hot.len() + j`.
    hot: VecDeque<Arc<BrokerRecord>>,
    /// Sum of [`resident_cost`] over `hot`.
    hot_bytes: usize,
    /// The hot window's byte budget; the newest record is always kept resident.
    budget: usize,
    /// The subjects the pages name.
    subjects: Subjects,
    /// Today's slot vector beside the pages, fed the same changes ([`Tier::check_shadow`]).
    #[cfg(test)]
    shadow: reference::Slots,
    /// [`PageMutant::StaleName`] applied.
    #[cfg(test)]
    stale_names: bool,
    /// What its readers and writers did ([`TierTally`]).
    #[cfg(any(test, debug_assertions))]
    counted: tally::Counters,
}

/// Retained records `lo..hi`'s summaries, read from the tier with no allocation and no cold
/// reads.
#[derive(Clone, Copy)]
pub(crate) struct Summaries<'a> {
    tier: &'a Tier,
    lo: usize,
    hi: usize,
}

impl<'a> Summaries<'a> {
    /// The summaries, in offset order.
    pub(crate) fn iter(self) -> impl Iterator<Item = Summary<'a>> {
        self.walk().map(|row| row.summary)
    }

    /// The summaries with each record's index, in offset order.
    #[cfg(any(test, feature = "retention"))]
    pub(crate) fn indexed(self) -> impl Iterator<Item = (usize, Summary<'a>)> {
        self.walk().map(|row| (row.i, row.summary))
    }

    /// The records page by page ([`Walk`]).
    fn walk(self) -> Walk<'a> {
        count!(self.tier, Walk, 1);
        Walk {
            tier: self.tier,
            i: self.lo,
            hi: self.hi,
            page: None,
            metas: [].iter(),
            locals: [].iter(),
            last: None,
            #[cfg(any(test, debug_assertions))]
            rows: 0,
        }
    }
}

/// One record a [`Walk`] read: its summary and, for the retention planning, its index and its
/// page, from which its broker time is read without finding the page again.
struct Row<'a> {
    #[cfg(any(test, feature = "retention"))]
    i: usize,
    summary: Summary<'a>,
    #[cfg(feature = "retention")]
    page: &'a Page,
}

impl Row<'_> {
    /// The record's broker time ([`Tier::bt`]).
    #[cfg(feature = "retention")]
    fn bt(&self, tier: &Tier) -> Option<u64> {
        let j = tier.head.checked_add(self.i)?;
        if *tier.cols.meta.get(j)? & HAS_BT == 0 {
            return None;
        }
        match *tier.cols.bt.get(j)? {
            WIDE => {
                count!(tier, Wide, 1);
                tier.wide_bt.get(&self.summary.off).copied()
            }
            d => self.page.bt0?.checked_add(u64::from(d)),
        }
    }
}

/// A walk of retained records in offset order, page by page: a page is found once for its
/// records, its column entries are read as slices, and a run of records naming one subject
/// resolves the name once. In a tier whose pages and columns cover its records, as every
/// tier's do, it yields what [`Tier::summary`] answers for each index in its range; otherwise
/// it skips the records it cannot read, as `summary` answers none for them.
struct Walk<'a> {
    tier: &'a Tier,
    /// The next record's index, and the index the walk stops at.
    i: usize,
    hi: usize,
    /// The page the walk is in, and its `meta` and subject entries from the walk's next
    /// record to the walk's end in it.
    page: Option<&'a Page>,
    metas: std::slice::Iter<'a, u32>,
    locals: std::slice::Iter<'a, u8>,
    /// The subject index the walk's last record in this page named, and its name.
    last: Option<(u8, &'a str)>,
    /// The rows the walk read, added to the tier's tally when it ends.
    #[cfg(any(test, debug_assertions))]
    rows: u64,
}

#[cfg(any(test, debug_assertions))]
impl Drop for Walk<'_> {
    fn drop(&mut self) {
        count!(self.tier, Rows, self.rows);
    }
}

impl Walk<'_> {
    /// Move to the page holding record `i`, its entries up to the walk's end in it; `false`
    /// once the walk has reached its end.
    fn enter(&mut self) -> bool {
        let tier = self.tier;
        while self.i < self.hi.min(tier.len) {
            let j = tier.head.saturating_add(self.i);
            let p = j >> tier.shift;
            let end = (p.saturating_add(1) << tier.shift)
                .min(tier.head.saturating_add(self.hi.min(tier.len)));
            let page = tier.pages.get(p);
            let metas = tier.cols.meta.get(j..end);
            let locals = tier.cols.subject.get(j..end);
            let (Some(page), Some(metas), Some(locals)) = (page, metas, locals) else {
                // No record of this page can be read: past them.
                self.i = self.i.saturating_add(end.saturating_sub(j).max(1));
                continue;
            };
            self.page = Some(page);
            self.metas = metas.iter();
            self.locals = locals.iter();
            if !tier.stale_names() {
                self.last = None;
            }
            return true;
        }
        false
    }
}

impl<'a> Iterator for Walk<'a> {
    type Item = Row<'a>;

    fn next(&mut self) -> Option<Row<'a>> {
        loop {
            let (Some(&meta), Some(&l), Some(page)) =
                (self.metas.next(), self.locals.next(), self.page)
            else {
                if self.enter() {
                    continue;
                }
                return None;
            };
            let i = self.i;
            self.i = i.saturating_add(1);
            let subject = match self.last {
                Some((m, name)) if m == l => name,
                _ => {
                    let Some(&id) = page.subjects.get(usize::from(l)) else {
                        continue;
                    };
                    let name = self.tier.subjects.name(id);
                    self.last = Some((l, name));
                    name
                }
            };
            let Some(off) = self.tier.base.checked_add(i as u64) else {
                continue;
            };
            #[cfg(any(test, debug_assertions))]
            {
                self.rows = self.rows.saturating_add(1);
            }
            return Some(Row {
                #[cfg(any(test, feature = "retention"))]
                i,
                summary: Summary {
                    off,
                    subject,
                    bytes: u64::from(meta & BYTES_MASK),
                    hidden: meta & HIDDEN != 0,
                },
                #[cfg(feature = "retention")]
                page,
            });
        }
    }
}

impl Tier {
    /// The budget under which every retained record stays resident (the behaviour
    /// before the tier existed).
    pub(crate) const RESIDENT_ALL: usize = usize::MAX;

    /// An empty tier charging decoded records against `budget` bytes. The newest
    /// record always stays resident, even when its charge exceeds the budget.
    #[cfg(test)]
    pub(crate) fn with_budget(budget: usize) -> Tier {
        Tier::paged(budget, PAGE_SHIFT)
    }

    /// An empty tier of pages of `1 << shift` records (`shift` at most [`PAGE_SHIFT`]).
    fn paged(budget: usize, shift: u32) -> Tier {
        Tier {
            pages: Vec::new(),
            cols: Columns::default(),
            head: 0,
            len: 0,
            shift: shift.min(PAGE_SHIFT),
            base: 0,
            wide_at: BTreeMap::new(),
            wide_bt: BTreeMap::new(),
            hot: VecDeque::new(),
            hot_bytes: 0,
            budget,
            subjects: Subjects::default(),
            #[cfg(test)]
            shadow: reference::Slots::with_budget(budget),
            #[cfg(test)]
            stale_names: false,
            #[cfg(any(test, debug_assertions))]
            counted: tally::Counters::default(),
        }
    }

    /// What this tier's readers and writers did since its log was opened ([`TierTally`]).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn tally(&self) -> TierTally {
        self.counted.read()
    }

    /// The places a page holds minus one: the mask of a place within its page.
    fn mask(&self) -> usize {
        (1usize << self.shift).saturating_sub(1)
    }

    /// Whether a [`Walk`] keeps its last subject name across a page boundary: under a test's
    /// [`PageMutant::StaleName`].
    #[cfg(test)]
    fn stale_names(&self) -> bool {
        self.stale_names
    }

    /// Whether a [`Walk`] keeps its last subject name across a page boundary: never.
    #[cfg(not(test))]
    const fn stale_names(&self) -> bool {
        false
    }

    /// Record `i`'s page and its place (its entry in the columns), if `i` is in range.
    fn place(&self, i: usize) -> Option<(&Page, usize)> {
        if i >= self.len {
            return None;
        }
        let j = self.head.checked_add(i)?;
        let page = self.pages.get(j >> self.shift)?;
        Some((page, j))
    }

    /// Record `i`'s subject id.
    #[cfg(any(test, feature = "retention"))]
    fn subject_id(&self, i: usize) -> Option<u32> {
        let (page, j) = self.place(i)?;
        let l = *self.cols.subject.get(j)?;
        page.subjects.get(usize::from(l)).copied()
    }

    /// Drop every page, column entry and wide value (the tier holds no record).
    fn clear_pages(&mut self) {
        self.pages.clear();
        self.cols.clear();
        self.head = 0;
        self.wide_at.clear();
        self.wide_bt.clear();
    }

    /// Write record `base + len`'s summary (`subject`, `meta`, broker time `bt`, frame start
    /// `at`) as the newest entry, starting a page when the last one is full.
    fn append(&mut self, subject: &str, meta: u32, bt: Option<u64>, at: u64) {
        let off = self.base.saturating_add(self.len as u64);
        let j = self.head.saturating_add(self.len);
        debug_assert_eq!(
            j,
            self.cols.meta.len(),
            "a record's place is its column entry"
        );
        if j >> self.shift >= self.pages.len() {
            self.pages.push(Page::new(at));
        }
        let mask = self.mask();
        let k = j & mask;
        let prev = match k.checked_sub(1) {
            Some(_) => j
                .checked_sub(1)
                .and_then(|p| self.cols.subject.get(p))
                .copied(),
            None => None,
        };
        let id = self.subjects.take(subject);
        let Some(page) = self.pages.last_mut() else {
            return;
        };
        let at_col = column(at, page.at0);
        if at_col == WIDE {
            self.wide_at.insert(off, at);
        }
        let bt_col = match bt {
            None => 0,
            Some(b) => {
                let base = *page.bt0.get_or_insert(b);
                let c = column(b, base);
                if c == WIDE {
                    self.wide_bt.insert(off, b);
                }
                c
            }
        };
        let local = page.local(id, prev);
        self.cols.push(at_col, bt_col, meta, local);
        if k == mask {
            page.subjects.shrink_to_fit();
        }
        self.len = self.len.saturating_add(1);
    }

    /// Record `i`'s summary, if `i` is in range. A walk over many records reads them through
    /// [`summaries`](Self::summaries), which finds each page once.
    pub(crate) fn summary(&self, i: usize) -> Option<Summary<'_>> {
        count!(self, Lookup, 1);
        let (page, j) = self.place(i)?;
        let meta = *self.cols.meta.get(j)?;
        let l = *self.cols.subject.get(j)?;
        let id = *page.subjects.get(usize::from(l))?;
        Some(Summary {
            off: self.base.checked_add(i as u64)?,
            subject: self.subjects.name(id),
            bytes: u64::from(meta & BYTES_MASK),
            hidden: meta & HIDDEN != 0,
        })
    }

    /// Records `lo..hi`'s summaries (clamped to the records held), read page by page.
    pub(crate) fn summaries(&self, lo: usize, hi: usize) -> Summaries<'_> {
        let hi = hi.min(self.len);
        Summaries {
            tier: self,
            lo: lo.min(hi),
            hi,
        }
    }

    /// Records `lo..hi`'s summaries (clamped to the records held) one item an index, in offset
    /// order: what [`summary`](Self::summary) answers for each, `None` for a record it cannot
    /// read, but read page by page as a [`Walk`] reads them. A filtered read's scan passes
    /// every record of its range by its summary, so the scan finds each page once rather than
    /// once a record (aterm D10f: a bridge's refill scans a session's lane over the whole
    /// backlog behind its watermark).
    pub(crate) fn scan(&self, lo: usize, hi: usize) -> impl Iterator<Item = Option<Summary<'_>>> {
        let rows = self.summaries(lo, hi);
        let mut walk = rows.iter().peekable();
        let base = self.base;
        (rows.lo..rows.hi).map(move |i| {
            let off = base.checked_add(i as u64);
            match walk.peek() {
                Some(s) if Some(s.offset()) == off => walk.next(),
                _ => None,
            }
        })
    }

    /// The indices of the records in `lo..hi` (clamped to the records held) whose subject is
    /// `subject`, in offset order. A page whose ids do not name it is passed whole, and a
    /// subject no retained record names reads no page, so a walk for a subject few records
    /// name (the wills) reads only the pages that hold one.
    pub(crate) fn naming<'a>(
        &'a self,
        subject: &str,
        lo: usize,
        hi: usize,
    ) -> impl Iterator<Item = usize> + 'a {
        let hi = hi.min(self.len);
        let lo = lo.min(hi);
        let (from, to) = (self.head.saturating_add(lo), self.head.saturating_add(hi));
        let id = self.subjects.ids.get(subject).copied();
        count!(self, Naming, 1);
        let pages = match id {
            Some(_) if from < to => {
                (from >> self.shift)..((to - 1) >> self.shift).saturating_add(1)
            }
            _ => 0..0,
        };
        pages.flat_map(move |p| {
            let local = self.pages.get(p).and_then(|page| {
                let l = page.subjects.iter().position(|&s| Some(s) == id)?;
                u8::try_from(l).ok()
            });
            let start = (p << self.shift).max(from);
            let end = (p.saturating_add(1) << self.shift).min(to);
            let places = match local {
                Some(_) => {
                    count!(self, NamedPage, 1);
                    start..end
                }
                None => start..start,
            };
            places
                .filter(move |&j| self.cols.subject.get(j).copied() == local)
                .map(move |j| j - self.head)
        })
    }

    /// Release the subjects of the records in `range` (records being dropped).
    #[cfg(any(test, feature = "retention"))]
    fn give_subjects(&mut self, range: std::ops::Range<usize>) {
        for i in range {
            if let Some(id) = self.subject_id(i) {
                self.subjects.give(id);
            }
        }
    }

    /// How many records are retained (resident or not).
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// How many records are resident.
    pub(crate) fn resident(&self) -> usize {
        self.hot.len()
    }

    /// Bytes charged to the resident records.
    pub(crate) fn resident_bytes(&self) -> usize {
        self.hot_bytes
    }

    /// Change the budget, evicting at once if it shrank.
    pub(crate) fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.evict();
        #[cfg(test)]
        self.shadow.set_budget(budget);
    }

    /// Index of the first resident record.
    fn first_hot(&self) -> usize {
        self.len.saturating_sub(self.hot.len())
    }

    /// Append a promoted record whose frame starts at byte `at`. Its offset is the one
    /// after the newest record's (`base + len`), as every promotion's is.
    pub(crate) fn push(&mut self, rec: Arc<BrokerRecord>, bt: Option<u64>, at: u64) {
        if self.len == 0 {
            self.base = rec.seq.0;
            self.clear_pages();
        }
        debug_assert_eq!(
            Some(rec.seq.0),
            self.base.checked_add(self.len as u64),
            "the tier's records are base + i"
        );
        let meta = meta_of(&rec, bt);
        self.append(&rec.subject, meta, bt, at);
        count!(self, Push, 1);
        let cost = resident_cost(&rec);
        #[cfg(test)]
        self.shadow
            .push(rec.seq.0, &rec.subject, meta, bt, at, cost);
        self.hot_bytes = self.hot_bytes.saturating_add(cost);
        self.hot.push_back(rec);
        self.evict();
        #[cfg(test)]
        if let Err(e) = self.check_one(self.len.saturating_sub(1)) {
            panic!("the pages and the slot vector differ after a push: {e}");
        }
    }

    /// Drop the oldest resident records until the window fits the budget, always
    /// keeping the newest one.
    fn evict(&mut self) {
        while self.hot_bytes > self.budget && self.hot.len() > 1 {
            let Some(old) = self.hot.pop_front() else {
                break;
            };
            self.hot_bytes = self.hot_bytes.saturating_sub(resident_cost(&old));
        }
        self.maybe_shrink_hot();
    }

    /// Release excess `VecDeque` ring capacity when the resident window shrank below a
    /// quarter of its allocated buffer (`cap > 16 && len <= cap / 4`).
    fn maybe_shrink_hot(&mut self) {
        let cap = self.hot.capacity();
        let target = hot_shrink_target(cap, self.hot.len());
        if target < cap {
            self.hot.shrink_to(target);
        }
    }

    /// The resident record at index `i`, if it is resident.
    pub(crate) fn hot(&self, i: usize) -> Option<&Arc<BrokerRecord>> {
        let first = self.first_hot();
        let j = i.checked_sub(first)?;
        self.hot.get(j)
    }

    /// Where record `i`'s frame starts.
    pub(crate) fn at(&self, i: usize) -> Option<u64> {
        count!(self, Lookup, 1);
        let (page, j) = self.place(i)?;
        match *self.cols.at.get(j)? {
            WIDE => {
                count!(self, Wide, 1);
                self.wide_at.get(&self.base.checked_add(i as u64)?).copied()
            }
            d => page.at0.checked_add(u64::from(d)),
        }
    }

    /// Record `i`'s broker time.
    pub(crate) fn bt(&self, i: usize) -> Option<u64> {
        count!(self, Lookup, 1);
        let (page, j) = self.place(i)?;
        if *self.cols.meta.get(j)? & HAS_BT == 0 {
            return None;
        }
        match *self.cols.bt.get(j)? {
            WIDE => {
                count!(self, Wide, 1);
                self.wide_bt.get(&self.base.checked_add(i as u64)?).copied()
            }
            d => page.bt0?.checked_add(u64::from(d)),
        }
    }

    /// Drop every record at index `keep` and above (a rolled-back tail).
    #[cfg(test)]
    pub(crate) fn truncate(&mut self, keep: usize) {
        if keep >= self.len {
            return;
        }
        let first = self.first_hot();
        if keep <= first {
            self.hot.clear();
            self.hot_bytes = 0;
        } else {
            let keep_hot = keep - first;
            while self.hot.len() > keep_hot {
                if let Some(r) = self.hot.pop_back() {
                    self.hot_bytes = self.hot_bytes.saturating_sub(resident_cost(&r));
                }
            }
        }
        self.maybe_shrink_hot();
        self.give_subjects(keep..self.len);
        self.len = keep;
        if keep == 0 {
            self.clear_pages();
        } else {
            let end = self.head + keep;
            self.pages.truncate((end + self.mask()) >> self.shift);
            self.cols.truncate(end);
            let from = self.base + keep as u64;
            drop(self.wide_at.split_off(&from));
            drop(self.wide_bt.split_off(&from));
            self.renumber_last_page();
        }
        self.shadow.truncate(keep);
        if let Err(e) = self.check_shadow() {
            panic!("the pages and the slot vector differ after a truncation: {e}");
        }
    }

    /// Keep in the last page's subject ids only those its live entries name, so pushes that
    /// rewrite its entries after a truncation never take it past a `u8` index.
    #[cfg(test)]
    fn renumber_last_page(&mut self) {
        let end = self.head + self.len;
        let Some(p) = self.pages.len().checked_sub(1) else {
            return;
        };
        let from = (p << self.shift).max(self.head);
        let Some(page) = self.pages.last_mut() else {
            return;
        };
        let mut kept: Vec<u32> = Vec::new();
        for s in &mut self.cols.subject[from..end] {
            let id = page.subjects[usize::from(*s)];
            let n = match kept.iter().position(|&k| k == id) {
                Some(n) => n,
                None => {
                    kept.push(id);
                    kept.len() - 1
                }
            };
            *s = u8::try_from(n).unwrap_or(u8::MAX);
        }
        page.subjects = kept;
    }

    /// Iterate the resident records (test and accounting helper).
    #[cfg(test)]
    pub(crate) fn hot_iter(&self) -> impl Iterator<Item = &Arc<BrokerRecord>> {
        self.hot.iter()
    }

    /// How many distinct subjects the pages name (test helper).
    #[cfg(test)]
    pub(crate) fn live_subjects(&self) -> usize {
        self.subjects.live()
    }

    /// Write record `i`'s frame start as `at`, in its column or the wide map.
    #[cfg(test)]
    fn set_at(&mut self, i: usize, at: u64) {
        if i >= self.len {
            return;
        }
        let off = self.base.saturating_add(i as u64);
        let j = self.head.saturating_add(i);
        let Some(page) = self.pages.get(j >> self.shift) else {
            return;
        };
        let c = column(at, page.at0);
        if let Some(e) = self.cols.at.get_mut(j) {
            *e = c;
        }
        if c == WIDE {
            self.wide_at.insert(off, at);
        } else {
            self.wide_at.remove(&off);
        }
    }

    /// Move record `i`'s frame start one byte on: the negative control of the cold-read
    /// tests (test helper). The slot vector beside the pages moves with it.
    #[cfg(test)]
    pub(crate) fn skew_frame_start(&mut self, i: usize) {
        if let Some(at) = self.at(i) {
            self.set_at(i, at.saturating_add(1));
            self.shadow.set_at(i, at.saturating_add(1));
        }
    }

    /// Apply `m` to the pages alone (test helper): what a page entry off by one record
    /// reads, for the negative controls to catch.
    #[cfg(test)]
    pub(crate) fn mutate(&mut self, m: PageMutant) {
        match m {
            PageMutant::Base(p) => {
                let first = if p == 0 { self.head } else { 0 };
                let i = (p << self.shift)
                    .saturating_add(first)
                    .saturating_sub(self.head);
                let (at1, bt1) = (self.at(i + 1), self.bt(i + 1));
                if let Some(page) = self.pages.get_mut(p) {
                    if let Some(a) = at1 {
                        page.at0 = a;
                    }
                    if bt1.is_some() {
                        page.bt0 = bt1;
                    }
                }
            }
            PageMutant::Index => self.head = self.head.saturating_add(1),
            PageMutant::StaleName => self.stale_names = true,
        }
    }

    /// Rebuild the pages as pages of `1 << shift` records (`shift` at most [`PAGE_SHIFT`]),
    /// answering the same (test helper: small pages put many boundaries in a small log).
    #[cfg(test)]
    pub(crate) fn repage(&mut self, shift: u32) {
        let entries: Vec<(String, u32, Option<u64>, u64)> = (0..self.len)
            .map(|i| {
                let (_, j) = self.place(i).expect("a retained record's place");
                let s = self.summary(i).expect("a retained record's summary");
                (
                    s.subject().to_string(),
                    self.cols.meta[j],
                    self.bt(i),
                    self.at(i).expect("a retained record's frame start"),
                )
            })
            .collect();
        self.give_subjects(0..self.len);
        let len = self.len;
        self.clear_pages();
        self.len = 0;
        self.shift = shift.min(PAGE_SHIFT);
        for (subject, meta, bt, at) in &entries {
            self.append(subject, *meta, *bt, *at);
        }
        assert_eq!(self.len, len, "a repage keeps every record");
        if let Err(e) = self.check_shadow() {
            panic!("the pages and the slot vector differ after a repage: {e}");
        }
    }

    /// Record `i`'s answers against the slot vector's: its summary, frame start, broker time
    /// and residency.
    #[cfg(test)]
    fn check_one(&self, i: usize) -> Result<(), String> {
        let s = &self.shadow;
        let mine = (
            self.summary(i),
            self.at(i),
            self.bt(i),
            self.hot(i).map(|r| r.seq.0),
        );
        let theirs = (s.summary(i), s.at(i), s.bt(i), s.hot(i));
        if mine != theirs {
            return Err(format!(
                "record {i}: the pages answer {mine:?}, the slot vector {theirs:?}"
            ));
        }
        Ok(())
    }

    /// Every answer the pages give against the slot vector's (test helper): the counts, the
    /// subjects named, and each record's summary, frame start, broker time and residency.
    #[cfg(test)]
    pub(crate) fn check_shadow(&self) -> Result<(), String> {
        let s = &self.shadow;
        let mine = (
            self.len(),
            self.resident(),
            self.resident_bytes(),
            self.live_subjects(),
        );
        let theirs = (s.len(), s.resident(), s.resident_bytes(), s.live_subjects());
        if mine != theirs {
            return Err(format!(
                "(len, resident, resident bytes, subjects): the pages {mine:?}, the slot \
                 vector {theirs:?}"
            ));
        }
        for i in 0..=self.len {
            self.check_one(i)?;
        }
        self.check_walks()
    }

    /// The retention planning over the pages against the planning over the slot vector, by
    /// `rules` at `now` (test helper): the same plan.
    #[cfg(all(test, feature = "retention"))]
    pub(crate) fn check_plans(
        &self,
        rules: &crate::retention::Retention,
        now: u64,
    ) -> Result<(), String> {
        use std::collections::HashMap;
        let groups: HashMap<String, u64> = HashMap::new();
        let held = BTreeMap::new();
        let stamps: Vec<(u64, u64)> = Vec::new();
        let scene = crate::retention::Scene {
            base: self.base,
            ceiling: self.base.saturating_add(self.len as u64),
            stamps: &stamps,
            groups: &groups,
            held: &held,
            now,
        };
        let mine = format!(
            "{:?}",
            crate::retention::plan_over(rules, &scene, self, None)
        );
        let theirs = format!(
            "{:?}",
            crate::retention::plan_over(rules, &scene, &self.shadow, None)
        );
        if mine != theirs {
            return Err(format!(
                "the plans at {now} differ: the pages {mine}, the slot vector {theirs}"
            ));
        }
        Ok(())
    }

    /// The page-by-page walks against [`summary`](Self::summary) and [`bt`](Self::bt) an index
    /// (test helper): over every record, and from a third of the way in (mid-page, in pages of
    /// more than one record) to past the end; and [`naming`](Self::naming) each of up to eight
    /// subjects the tier holds and one it does not, from there to two thirds of the way in.
    #[cfg(test)]
    fn check_walks(&self) -> Result<(), String> {
        let mut names: Vec<&str> = self.subjects.ids.keys().map(|s| &**s).collect();
        names.sort_unstable();
        names.truncate(8);
        names.push("/f/no/record/names/this");
        for lo in [0, self.len / 3] {
            let hi = self.len.saturating_add(2);
            for name in &names {
                let to = if lo == 0 { hi } else { self.len - self.len / 3 };
                let named: Vec<usize> = self.naming(name, lo, to).collect();
                let each: Vec<usize> = (lo..to)
                    .filter(|&i| self.summary(i).is_some_and(|s| s.subject() == *name))
                    .collect();
                if named != each {
                    return Err(format!(
                        "the records in {lo}..{to} naming {name:?}: by page {named:?}, each \
                         record {each:?}"
                    ));
                }
            }
            let walked: Vec<(usize, Summary<'_>)> = self.summaries(lo, hi).indexed().collect();
            let each: Vec<(usize, Summary<'_>)> = (lo..hi)
                .filter_map(|i| Some((i, self.summary(i)?)))
                .collect();
            if walked != each {
                return Err(format!(
                    "the walk from {lo} answers {walked:?}, each record {each:?}"
                ));
            }
            let scanned: Vec<Option<Summary<'_>>> = self.scan(lo, hi).collect();
            let each: Vec<Option<Summary<'_>>> = (lo.min(self.len)..hi.min(self.len))
                .map(|i| self.summary(i))
                .collect();
            if scanned != each {
                return Err(format!(
                    "the scan from {lo} answers {scanned:?}, each record {each:?}"
                ));
            }
            #[cfg(feature = "retention")]
            {
                use crate::retention::Records;
                let walked: Vec<(usize, crate::retention::Entry<'_>)> =
                    self.entries(lo, hi).collect();
                let each: Vec<(usize, crate::retention::Entry<'_>)> =
                    crate::retention::ByIndex::new(self, lo, hi.min(self.len)).collect();
                if walked != each {
                    return Err(format!(
                        "the planning's walk from {lo} answers {walked:?}, each record {each:?}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Allocated slot capacity of the hot `VecDeque` (test helper).
    #[cfg(test)]
    pub(crate) fn hot_capacity(&self) -> usize {
        self.hot.capacity()
    }

    /// A tier over records an open recovered, with their broker times and frame starts;
    /// cold prefix records outside `budget` are summarized and dropped in place without
    /// allocating `Arc`s or growing the hot `VecDeque` past the hot suffix. The pages and
    /// columns are allocated once, at the count recovered.
    pub(crate) fn from_parts(
        records: Vec<BrokerRecord>,
        bts: Vec<Option<u64>>,
        at: Vec<u64>,
        budget: usize,
    ) -> Tier {
        let mut tier = Tier::paged(budget, PAGE_SHIFT);
        tier.fill(records, bts, at);
        tier
    }

    /// [`from_parts`](Self::from_parts) for a rebuild of this tier (a replica's truncation):
    /// the same budget and page size, and in a test or debug build its tally carried on.
    pub(crate) fn rebuilt(
        &self,
        records: Vec<BrokerRecord>,
        bts: Vec<Option<u64>>,
        at: Vec<u64>,
    ) -> Tier {
        let mut tier = Tier::paged(self.budget, self.shift);
        #[cfg(any(test, debug_assertions))]
        {
            tier.counted = self.counted.carried();
        }
        tier.fill(records, bts, at);
        tier
    }

    fn fill(&mut self, mut records: Vec<BrokerRecord>, bts: Vec<Option<u64>>, at: Vec<u64>) {
        let n = records.len().min(at.len());
        records.truncate(n);
        if let Some(first) = records.first() {
            self.base = first.seq.0;
        }
        self.pages
            .reserve_exact((n >> self.shift).saturating_add(1));
        self.cols.reserve_exact(n);
        let hot_start = hot_window_start(&records, self.budget);
        for (i, rec) in records.into_iter().enumerate() {
            let bt = bts.get(i).copied().flatten();
            let a = at.get(i).copied().unwrap_or(0);
            let meta = meta_of(&rec, bt);
            self.append(&rec.subject, meta, bt, a);
            #[cfg(test)]
            if i >= hot_start {
                self.shadow
                    .push_warm(rec.seq.0, &rec.subject, meta, bt, a, resident_cost(&rec));
            } else {
                self.shadow.push_cold(rec.seq.0, &rec.subject, meta, bt, a);
            }
            if i >= hot_start {
                self.hot_bytes = self.hot_bytes.saturating_add(resident_cost(&rec));
                self.hot.push_back(Arc::new(rec));
            }
        }
        #[cfg(test)]
        if let Err(e) = self.check_shadow() {
            panic!("the pages and the slot vector differ after an open's build: {e}");
        }
    }

    /// Drop the oldest `n` records (a retention cut) and move every kept frame start
    /// down by `shift` bytes (the bytes the cut removed from the front of the file).
    #[cfg(any(test, feature = "retention"))]
    pub(crate) fn drain_front(&mut self, n: usize, shift: u64) {
        let n = n.min(self.len);
        let first = self.first_hot();
        for _ in 0..n.saturating_sub(first) {
            if let Some(r) = self.hot.pop_front() {
                self.hot_bytes = self.hot_bytes.saturating_sub(resident_cost(&r));
            }
        }
        self.maybe_shrink_hot();
        self.give_subjects(0..n);
        let gone = self.head.saturating_add(n);
        let whole = (gone >> self.shift).min(self.pages.len());
        self.pages.drain(..whole);
        self.cols.drain_front(whole << self.shift);
        self.head = gone & self.mask();
        self.len = self.len.saturating_sub(n);
        self.base = self.base.saturating_add(n as u64);
        if self.len == 0 {
            self.clear_pages();
        } else {
            self.wide_at = self.wide_at.split_off(&self.base);
            self.wide_bt = self.wide_bt.split_off(&self.base);
            self.shift_frames(shift);
        }
        if self.pages.capacity() > 64 && self.pages.len() <= self.pages.capacity() / 4 {
            self.pages.shrink_to(self.pages.len().saturating_mul(2));
        }
        #[cfg(test)]
        {
            self.shadow.drain_front(n, shift);
            if let Err(e) = self.check_shadow() {
                panic!("the pages and the slot vector differ after a cut: {e}");
            }
        }
    }

    /// Move every retained frame start down by `by` bytes, saturating at 0. A page whose
    /// first frame start is at least `by` moves that alone; another (in a real log, the
    /// first page, whose cut entries start below the cut) has each live entry rewritten.
    #[cfg(any(test, feature = "retention"))]
    fn shift_frames(&mut self, by: u64) {
        let page = 1usize << self.shift;
        let end = self.head.saturating_add(self.len);
        let mut rewritten: BTreeMap<u64, u64> = BTreeMap::new();
        for p in 0..self.pages.len() {
            let Some(at0) = self.pages.get(p).map(|pg| pg.at0) else {
                continue;
            };
            if at0 >= by {
                if let Some(pg) = self.pages.get_mut(p) {
                    pg.at0 = at0 - by;
                }
                continue;
            }
            let lo = if p == 0 { self.head } else { 0 };
            let hi = end.saturating_sub(p.saturating_mul(page)).min(page);
            // Each live entry's frame start, read before the page moves, then written moved.
            let mut moved: Vec<(usize, u64, u64)> = Vec::new();
            for k in lo..hi {
                let j = p.saturating_mul(page) + k;
                let i = j.saturating_sub(self.head);
                let off = self.base.saturating_add(i as u64);
                if let Some(a) = self.at(i) {
                    moved.push((j, off, a.saturating_sub(by)));
                }
                self.wide_at.remove(&off);
            }
            let Some(pg) = self.pages.get_mut(p) else {
                continue;
            };
            if let Some(&(_, _, first)) = moved.first() {
                pg.at0 = first;
            }
            for (j, off, a) in moved {
                let c = column(a, pg.at0);
                if let Some(e) = self.cols.at.get_mut(j) {
                    *e = c;
                }
                if c == WIDE {
                    rewritten.insert(off, a);
                }
            }
        }
        for v in self.wide_at.values_mut() {
            *v = v.saturating_sub(by);
        }
        self.wide_at.append(&mut rewritten);
    }
}

/// The retention planning's walk of the tier: a [`Walk`]'s rows as entries, each with its
/// broker time read from the page the walk is in.
#[cfg(feature = "retention")]
pub(crate) struct Entries<'a>(Walk<'a>);

#[cfg(feature = "retention")]
impl<'a> Iterator for Entries<'a> {
    type Item = (usize, crate::retention::Entry<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let row = self.0.next()?;
        let bt = row.bt(self.0.tier);
        Some((
            row.i,
            crate::retention::Entry {
                offset: row.summary.offset(),
                subject: row.summary.subject(),
                bytes: row.summary.bytes(),
                bt,
            },
        ))
    }
}

/// The retention planning reads the tier's pages as its records: each one's summary and
/// broker time, walked a page at a time, and no whole record (the planning loads a carried
/// one through `BrokerLog::record_arc`).
#[cfg(feature = "retention")]
impl crate::retention::Records for Tier {
    type Entries<'a> = Entries<'a>;

    fn entry(&self, i: usize) -> Option<crate::retention::Entry<'_>> {
        let s = self.summary(i)?;
        Some(crate::retention::Entry {
            offset: s.offset(),
            subject: s.subject(),
            bytes: s.bytes(),
            bt: self.bt(i),
        })
    }

    fn entries(&self, from: usize, to: usize) -> Entries<'_> {
        Entries(self.summaries(from, to).walk())
    }

    fn whole(&self, _i: usize) -> Option<Arc<BrokerRecord>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astream_wire::Offset;

    fn rec(seq: u64, body: usize) -> Arc<BrokerRecord> {
        Arc::new(BrokerRecord {
            seq: Offset(seq),
            producer_id: 1,
            producer_seq: seq,
            subject: "/f/t".to_string(),
            body: vec![7u8; body],
            commit: None,
        })
    }

    #[test]
    fn unbounded_keeps_everything_resident() {
        let mut t = Tier::with_budget(Tier::RESIDENT_ALL);
        for i in 0..100 {
            t.push(rec(i, 1000), Some(i), i * 10);
        }
        assert_eq!((t.len(), t.resident()), (100, 100));
        assert!(t.hot(0).is_some());
    }

    #[test]
    fn budget_evicts_oldest_and_always_keeps_newest() {
        let cost = resident_cost(&rec(0, 1000));
        let mut t = Tier::with_budget(cost * 3);
        for i in 0..10 {
            t.push(rec(i, 1000), Some(i), i * 10);
            assert!(t.resident_bytes() <= cost * 3);
        }
        assert_eq!((t.len(), t.resident()), (10, 3));
        assert!(t.hot(6).is_none());
        assert_eq!(t.hot(7).map(|r| r.seq.0), Some(7));
        assert_eq!(t.at(2), Some(20));
        assert_eq!(t.bt(9), Some(9));
        // A record bigger than the whole budget still stays resident while newest.
        t.push(rec(10, 1 << 20), None, 100);
        assert_eq!(t.resident(), 1);
        assert_eq!(t.hot(10).map(|r| r.seq.0), Some(10));
    }

    #[test]
    fn truncate_and_drain_keep_indices_aligned() {
        let cost = resident_cost(&rec(0, 10));
        let mut t = Tier::with_budget(cost * 4);
        for i in 0..8 {
            t.push(rec(i, 10), Some(i), i * 50);
        }
        t.truncate(6);
        assert_eq!((t.len(), t.resident()), (6, 2));
        assert_eq!(t.hot(5).map(|r| r.seq.0), Some(5));
        t.drain_front(5, 250);
        assert_eq!(t.len(), 1);
        assert_eq!(t.at(0), Some(0));
        assert_eq!(t.hot(0).map(|r| r.seq.0), Some(5));
        assert_eq!(t.bt(0), Some(5));
    }

    #[test]
    fn shrinking_the_budget_evicts_at_once() {
        let mut t = Tier::with_budget(Tier::RESIDENT_ALL);
        for i in 0..5 {
            t.push(rec(i, 100), None, i);
        }
        t.set_budget(0);
        assert_eq!(t.resident(), 1);
        assert_eq!(t.resident_bytes(), resident_cost(&rec(4, 100)));
    }

    #[test]
    fn summaries_stay_aligned_and_subjects_prune_on_truncate_and_drain() {
        fn named(seq: u64, subject: &str, body: usize) -> BrokerRecord {
            BrokerRecord {
                seq: Offset(seq),
                producer_id: 1,
                producer_seq: seq,
                subject: subject.to_string(),
                body: vec![3u8; body],
                commit: None,
            }
        }

        let r0 = named(10, "/f/a", 12);
        let r0_bytes = plaintext_frame_bytes(&r0);
        let r1 = named(11, "/f/a", 24);
        let r2 = named(12, "/a/commit", 0);
        let r3 = Arc::new(named(13, "/f/tail", 8));
        let mut t = Tier::from_parts(
            vec![r0, r1, r2],
            vec![Some(100), Some(101), Some(102)],
            vec![0, 64, 128],
            0,
        );
        t.push(Arc::clone(&r3), Some(103), 192);
        assert_eq!((t.len(), t.resident()), (4, 1));
        assert_eq!(t.len(), 4);
        assert_eq!(t.subject_id(0), t.subject_id(1), "one id for one subject");
        assert_eq!(t.subjects.live(), 3);
        assert_eq!(
            (
                t.summary(0).map(Summary::offset),
                t.summary(0).map(Summary::subject),
                t.summary(0).map(Summary::bytes),
                t.summary(0).map(Summary::is_hidden),
            ),
            (Some(10), Some("/f/a"), Some(r0_bytes), Some(false)),
        );
        assert_eq!(t.summary(2).map(Summary::is_hidden), Some(true));

        t.truncate(3);
        assert_eq!(t.len(), 3);
        assert_eq!(t.subjects.live(), 2, "/f/tail pruned on truncate");

        t.drain_front(2, 128);
        assert_eq!(t.len(), 1);
        assert_eq!(t.summary(0).map(Summary::offset), Some(12));
        assert_eq!(t.summary(0).map(Summary::subject), Some("/a/commit"));
        assert_eq!(t.subjects.live(), 1, "/f/a pruned on drain_front");
    }

    #[test]
    fn from_parts_summarizes_cold_prefix_without_growing_hot_capacity() {
        let owned = |seq: u64, body: usize| BrokerRecord {
            seq: Offset(seq),
            producer_id: 1,
            producer_seq: seq,
            subject: "/f/t".to_string(),
            body: vec![7u8; body],
            commit: None,
        };
        let cost = resident_cost(&owned(0, 64));
        let records: Vec<BrokerRecord> = (0..1_000).map(|i| owned(i, 64)).collect();
        let bts: Vec<Option<u64>> = (0..1_000).map(|i| Some(10_000 + i)).collect();
        let at: Vec<u64> = (0..1_000).map(|i| i * 128).collect();

        let t = Tier::from_parts(records, bts, at, cost * 2);
        assert_eq!((t.len(), t.resident()), (1_000, 2));
        assert!(
            t.hot_capacity() <= 16,
            "hot VecDeque capacity must stay bounded by the budget window, got {}",
            t.hot_capacity()
        );
        assert!(t.hot(997).is_none());
        assert_eq!(t.hot(998).map(|r| r.seq.0), Some(998));
        assert_eq!(t.hot(999).map(|r| r.seq.0), Some(999));
        assert_eq!(t.at(0), Some(0));
        assert_eq!(t.at(999), Some(999 * 128));
        assert_eq!(t.bt(0), Some(10_000));
        assert_eq!(t.bt(999), Some(10_999));
        assert_eq!(t.len(), 1_000);

        // Parity against incremental `push` across varying payload sizes and budgets.
        let sizes = [0usize, 13, 250, 7, 512, 1, 80, 19];
        for budget in [0, 96, 200, 450, 900, 2_000, Tier::RESIDENT_ALL] {
            let recs: Vec<BrokerRecord> = sizes
                .iter()
                .enumerate()
                .map(|(i, &sz)| owned(i as u64, sz))
                .collect();
            let bts_v: Vec<Option<u64>> = (0..sizes.len()).map(|i| Some(i as u64)).collect();
            let at_v: Vec<u64> = (0..sizes.len()).map(|i| (i * 64) as u64).collect();
            let mut inc = Tier::with_budget(budget);
            for (i, r) in recs.iter().cloned().enumerate() {
                inc.push(Arc::new(r), bts_v[i], at_v[i]);
            }
            let bulk = Tier::from_parts(recs, bts_v, at_v, budget);
            assert_eq!(bulk.resident(), inc.resident(), "budget={budget}");
            assert_eq!(
                bulk.resident_bytes(),
                inc.resident_bytes(),
                "budget={budget}"
            );
            for i in 0..sizes.len() {
                assert_eq!(
                    bulk.hot(i).map(|r| r.seq),
                    inc.hot(i).map(|r| r.seq),
                    "budget={budget}, i={i}"
                );
            }
        }
    }

    #[test]
    fn shrinking_budget_draining_truncating_and_oversized_push_reclaim_hot_capacity() {
        // 1) `set_budget(0)` after 1,000 resident records reclaims ring slots.
        let mut t = Tier::with_budget(Tier::RESIDENT_ALL);
        for i in 0..1_000 {
            t.push(rec(i, 32), Some(i), i * 64);
        }
        assert!(t.hot_capacity() >= 1_000);
        t.set_budget(0);
        assert_eq!(t.resident(), 1);
        assert!(
            t.hot_capacity() <= 16,
            "set_budget(0) must shrink hot capacity, got {}",
            t.hot_capacity()
        );

        // 2) `drain_front` across a large resident prefix reclaims ring slots.
        let mut t2 = Tier::with_budget(Tier::RESIDENT_ALL);
        for i in 0..1_000 {
            t2.push(rec(i, 32), Some(i), i * 64);
        }
        t2.drain_front(999, 999 * 64);
        assert_eq!((t2.len(), t2.resident()), (1, 1));
        assert!(
            t2.hot_capacity() <= 16,
            "drain_front must shrink hot capacity, got {}",
            t2.hot_capacity()
        );

        // 3) `truncate` of a large resident suffix reclaims ring slots.
        let mut t3 = Tier::with_budget(Tier::RESIDENT_ALL);
        for i in 0..1_000 {
            t3.push(rec(i, 32), Some(i), i * 64);
        }
        t3.truncate(2);
        assert_eq!((t3.len(), t3.resident()), (2, 2));
        assert!(
            t3.hot_capacity() <= 16,
            "truncate must shrink hot capacity, got {}",
            t3.hot_capacity()
        );

        // 4) An oversized record that evicts hundreds of small resident records shrinks too.
        let small_cost = resident_cost(&rec(0, 16));
        let mut t4 = Tier::with_budget(small_cost * 512);
        for i in 0..512 {
            t4.push(rec(i, 16), None, i * 32);
        }
        assert_eq!(t4.resident(), 512);
        assert!(t4.hot_capacity() >= 512);
        t4.push(rec(512, small_cost * 600), None, 512 * 32);
        assert_eq!(t4.resident(), 1);
        assert!(
            t4.hot_capacity() <= 16,
            "evicting burst via oversized push must shrink hot capacity, got {}",
            t4.hot_capacity()
        );
    }
}

/// The tally counts what each of the tier's readers and writers ran, and only that (aterm
/// D10f's count of what an attach runs through the pages).
#[cfg(test)]
mod tally_tests {
    use super::*;
    use astream_wire::Offset;

    fn named(seq: u64, subject: &str) -> Arc<BrokerRecord> {
        Arc::new(BrokerRecord {
            seq: Offset(seq),
            producer_id: 1,
            producer_seq: seq,
            subject: subject.to_string(),
            body: vec![7u8; 16],
            commit: None,
        })
    }

    /// 600 records over three pages of 256, every 100th naming `w` (in pages 0, 1 and 2), and
    /// record 1's frame start below record 0's, so a column cannot hold it.
    fn tier() -> Tier {
        let mut t = Tier::with_budget(0);
        for i in 0..600u64 {
            let subject = if i % 100 == 50 { "w" } else { "s" };
            let at = if i == 1 { 0 } else { 1_000 + i * 64 };
            t.push(named(i, subject), Some(i), at);
        }
        t
    }

    #[test]
    fn each_reader_and_writer_counts_what_it_ran() {
        let mut t = tier();
        assert_eq!(t.tally().pushes, 600, "a push counts once");
        let before = t.tally();
        t.push(named(600, "s"), Some(600), 1_000 + 600 * 64);
        assert_eq!(t.tally().since(before).pushes, 1);

        let before = t.tally();
        assert!(t.summary(3).is_some());
        assert_eq!(t.at(1), Some(0));
        assert_eq!(t.bt(4), Some(4));
        let d = t.tally().since(before);
        assert_eq!((d.lookups, d.wide, d.walks, d.rows), (3, 1, 0, 0));

        let before = t.tally();
        assert_eq!(t.summaries(0, t.len()).iter().count(), 601);
        assert_eq!(t.summaries(10, 20).iter().take(4).count(), 4);
        let d = t.tally().since(before);
        assert_eq!(
            (d.walks, d.rows, d.lookups),
            (2, 605, 0),
            "a walk counts the rows it read"
        );

        let before = t.tally();
        let wills: Vec<usize> = t.naming("w", 0, t.len()).collect();
        assert_eq!(wills, vec![50, 150, 250, 350, 450, 550]);
        assert_eq!(t.naming("nobody", 0, t.len()).count(), 0);
        let d = t.tally().since(before);
        assert_eq!(
            (d.namings, d.named_pages, d.rows),
            (2, 3, 0),
            "a subject walk counts the pages naming it, and a subject no record names reads none"
        );
    }

    /// A test build checks every rebuilt tier against its slot vector, reading the pages
    /// through the counted readers: so the rebuilt tier's counts start at the old one's and
    /// only grow.
    #[test]
    fn a_rebuild_carries_the_tally_on() {
        let t = tier();
        let before = t.tally();
        let after = t.rebuilt(Vec::new(), Vec::new(), Vec::new()).tally();
        assert_eq!(after.pushes, before.pushes);
        assert_eq!(after.since(before).pushes, 0);
        for (now, was) in [
            (after.lookups, before.lookups),
            (after.wide, before.wide),
            (after.walks, before.walks),
            (after.rows, before.rows),
            (after.namings, before.namings),
            (after.named_pages, before.named_pages),
        ] {
            assert!(
                now >= was,
                "a rebuilt tier lost a count: {after:?} after {before:?}"
            );
        }
        assert!(before.lookups > 0, "the old tier had counted its reads");
    }
}
