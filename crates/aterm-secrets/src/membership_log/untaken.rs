// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a server sent a pull that this store could not take, kept per server so the next pull
//! to it does not bring the same records again (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.2,
//! F13).
//!
//! A pull names [`Store::have`], and a server answers every complete record past the ids named
//! ([`Store::missing`]). A record this store cannot take (a kind only a newer build writes, a
//! store at one of its budgets ([`super::RecordBudgets`]) or [`super::MAX_RECORDS_STREAM`], a
//! record [`super::Log::insert`] refuses) never enters `have`, so every server answered it again on
//! every pass, up to [`super::MAX_RECORDS_STREAM`] each. [`Untaken`] keeps, for each server, the
//! heads of what it sent that the store did not take, and the next pull to it names those heads
//! first ([`Untaken::have`]), so the server counts their causal closure as known.
//!
//! Why naming the heads hides nothing the store could take. A pass offers the store every frame
//! of a stream it reads: the whole stream when it verifies, and otherwise the frames this build
//! reads that descend from none it cannot read ([`super::store::readable_mask`]). A frame counts
//! when the store does not hold it complete after the pass, and then it can never be taken:
//! - this build cannot read it ([`Record::decode`] is a function of its bytes), or it descends
//!   from a frame this build cannot read;
//! - or the store refused it, or keeps it waiting on a parent. A parent outside the stream is
//!   one the server counted known: under the store's own ids, which the store held complete when
//!   the pull named them (a note against a store that no longer holds them is dropped), or under
//!   a memo's heads, which by induction cover only records the store holds or can never take. A
//!   refusal holds for good for a store whose complete part only grows: the bounds count held
//!   records, which only grow, [`super::Log::insert`] judges a complete record against its causal
//!   past, which its id fixes, and another network stays another network.
//!
//! The heads named are the counting frames no other counting frame names as a parent, so their
//! closure is the counting frames and their ancestors, which the store holds or can never take.
//!
//! The one way the store stops holding records (leave and rejoin, a records file replaced) is
//! checked: a memo applies only while the store holds every id of its floor, the store's frontier
//! when the memo was last written. A memo also lapses [`UNTAKEN_AGAIN`] of awake time after it
//! was first made (extending it does not renew it) and with the process, which is when an
//! upgraded build could take the records, and it never applies to a store with no genesis.

use super::store::framed::{FrameReadError, FrameReader, LayoutFrame};
#[cfg(test)]
use super::store::frames;
use super::store::{Frame, readable_mask};
use super::{MAX_HELD_RECORDS, MAX_RECORDS_STREAM, Record, RecordId, Store};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long a memo holds after it was first made: past it, what the server sent travels once
/// more, to a store that may since take it. `Instant` stops while a Mac sleeps, so this is an
/// hour of awake time.
pub(crate) const UNTAKEN_AGAIN: Duration = Duration::from_secs(60 * 60);

/// The most heads a memo names. Each costs 65 bytes of the pull's URL, which names them before
/// the store's own ids ([`Untaken::have`]); past this many, the rest of what the server sent
/// travels again.
pub(crate) const UNTAKEN_HEADS: usize = 32;
const _: () = assert!(UNTAKEN_HEADS * 65 < aterm_http::uri::MAX_URI_LEN / 2);

/// The most servers a memo table keeps for one store. A pass asks the vault's source and up to
/// [`crate::discover::MAX_CANDIDATES`] others, rotating over every server it heard, so a peer is
/// asked again only once the rotation has been round the rest: a memo must outlive a round of a
/// fleet of this many servers (ALab's is about 86), or every peer sends the whole gap on every
/// visit. Past it the memo used longest ago goes first, never the vault's source's ([`Noted`]),
/// so a LAN host that advertises fresh URLs pushes out its own memos before ones in use. A memo
/// holds its heads (at most [`UNTAKEN_HEADS`]) and shares its floor with every memo written
/// against the same frontier.
pub(crate) const UNTAKEN_URLS: usize = 256;
const _: () = assert!(UNTAKEN_URLS >= 8 * (crate::discover::MAX_CANDIDATES + 1));

/// What one server sent a pull, frame by frame. A stream this layout cannot frame
/// (truncated, another layout version) names nothing.
pub(crate) struct Sent(Vec<Frame>);

impl Sent {
    /// `bytes`, as a server sent them.
    #[cfg(test)]
    pub(crate) fn read(bytes: &[u8]) -> Self {
        Self(frames(bytes).unwrap_or_default())
    }

    /// Whether `store` does not hold every frame of what the server sent complete.
    pub(crate) fn left_in(&self, store: &Store) -> bool {
        self.0
            .iter()
            .any(|frame| store.complete_bytes(&frame.id).is_none())
    }
}

/// One response's complete-framing result, with each record decoded at most once.
/// Layout failure retains no prefix; unreadable kinds/signatures still retain their frame ids.
pub(crate) struct ReceivedFrames {
    sent: Sent,
    decoded: Vec<Option<Record>>,
    byte_len: usize,
    complete: bool,
}

impl ReceivedFrames {
    pub(crate) fn empty() -> Self {
        Self {
            sent: Sent(Vec::new()),
            decoded: Vec::new(),
            byte_len: 0,
            complete: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn read(bytes: &[u8]) -> Self {
        if bytes.len() > MAX_RECORDS_STREAM {
            let mut received = Self::empty();
            received.complete = false;
            received.byte_len = bytes.len();
            return received;
        }
        Self::read_from(&mut io::Cursor::new(bytes))
            .expect("an in-memory membership stream has no I/O failures")
    }

    /// Consume the actual input to EOF, preserving I/O failures even after layout refusal.
    /// The reader supplies transport deadlines and bounds; no artificial EOF is introduced.
    #[cfg(test)]
    pub(crate) fn read_from<R: Read + ?Sized>(reader: &mut R) -> io::Result<Self> {
        Self::read_from_with_known(reader, |_| None)
    }

    /// Reuse an already verified immutable owner only after hashing the complete frame.
    /// The lookup must not refresh files or retain a lock across another reader call.
    pub(crate) fn read_from_with_known<R: Read + ?Sized>(
        reader: &mut R,
        mut known: impl FnMut(&RecordId) -> Option<Record>,
    ) -> io::Result<Self> {
        let mut input = Counted { reader, bytes: 0 };
        let mut received = Self::empty();
        let mut frames = FrameReader::new();
        loop {
            if received.sent.0.len() == MAX_HELD_RECORDS || input.bytes == MAX_RECORDS_STREAM {
                let mut extra = [0];
                if read_once(&mut input, &mut extra)? == 0 {
                    break;
                }
                received.invalidate();
                drain(&mut input)?;
                break;
            }
            let start = input.bytes;
            match frames.read_layout(&mut input, MAX_RECORDS_STREAM - start) {
                Ok(None) => break,
                Ok(Some(frame)) => {
                    received.push(frame, start..input.bytes, &mut known);
                }
                Err(FrameReadError::Invalid(_)) => {
                    received.invalidate();
                    drain(&mut input)?;
                    break;
                }
                Err(FrameReadError::Read(error)) => return Err(error),
            }
        }
        received.byte_len = input.bytes;
        Ok(received)
    }

    fn invalidate(&mut self) {
        self.sent.0.clear();
        self.decoded.clear();
        self.complete = false;
    }

    fn push(
        &mut self,
        frame: LayoutFrame,
        _range: std::ops::Range<usize>,
        known: &mut impl FnMut(&RecordId) -> Option<Record>,
    ) {
        let id = aterm_digest::Sha256::digest(&frame.wire);
        let decoded = known(&id).or_else(|| Record::decode(&frame.wire).ok());
        self.sent.0.push(Frame {
            id,
            parents: frame.parents,
            #[cfg(test)]
            range: _range,
        });
        self.decoded.push(decoded);
    }

    pub(crate) const fn byte_len(&self) -> usize {
        self.byte_len
    }

    pub(crate) fn whole(&self) -> Option<Vec<Record>> {
        if !self.complete {
            return None;
        }
        self.decoded.iter().cloned().collect()
    }

    pub(crate) fn readable(&self) -> Vec<Record> {
        let readable = readable_mask(&self.sent.0, |i, _| self.decoded[i].is_some());
        self.decoded
            .iter()
            .zip(readable)
            .filter(|(_, readable)| *readable)
            .filter_map(|(record, _)| record.clone())
            .collect()
    }

    pub(crate) fn into_sent(self) -> Sent {
        self.sent
    }
}

struct Counted<'a, R: ?Sized> {
    reader: &'a mut R,
    bytes: usize,
}

impl<R: Read + ?Sized> Read for Counted<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.reader.read(bytes)?;
        self.bytes = self.bytes.saturating_add(read);
        Ok(read)
    }
}

fn read_once(reader: &mut impl Read, bytes: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

fn drain(reader: &mut impl Read) -> io::Result<()> {
    let mut bytes = [0; 16 * 1024];
    while read_once(reader, &mut bytes)? != 0 {}
    Ok(())
}

/// What a pull names ([`Untaken::have`]): `ids`, of which the first `heads` are a memo's.
pub(crate) struct Named {
    pub(crate) ids: Vec<RecordId>,
    pub(crate) heads: usize,
}

/// One pass's notes ([`Untaken::note`]): `store`, the store the pass offered what servers sent
/// to, after the pass; `named_from`, the frontier of the store its pulls named `have` from; when;
/// and `source`, the server whose memo is never dropped for room (a Mac's vault source, which it
/// asks on every pass; a vault server names none).
pub(crate) struct Noted<'a> {
    pub(crate) store: &'a Store,
    pub(crate) named_from: &'a [RecordId],
    pub(crate) now: Instant,
    pub(crate) source: Option<&'a str>,
}

/// One server's memo.
#[derive(Clone)]
struct Gap {
    /// The store's frontier when the memo was last written, shared with the store's other memos
    /// written against it.
    floor: Arc<[RecordId]>,
    /// The heads of what the server sent that the store did not take, newest first.
    heads: Vec<RecordId>,
    /// When the memo was first made.
    since: Instant,
    /// When a pull last named its heads, or a pass last wrote it.
    used: Instant,
    /// The memo is the vault source's, never dropped for room.
    source: bool,
    /// The last pull that named these heads heard the server say it lacks a record named.
    needy: bool,
}

impl Gap {
    /// Whether the memo still applies to `store` at `now`: within [`UNTAKEN_AGAIN`] of when it
    /// was made, and to a store that still holds every id of its floor complete.
    fn holds(&self, store: &Store, now: Instant) -> bool {
        now.saturating_duration_since(self.since) < UNTAKEN_AGAIN
            && self
                .floor
                .iter()
                .all(|id| store.complete_bytes(id).is_some())
    }
}

/// One store's memos.
#[derive(Default)]
struct Memos {
    /// Each server's memo, by URL.
    gaps: BTreeMap<String, Gap>,
    /// The floor the last memo was written against, which the next shares while the store's
    /// frontier stands.
    floor: Option<Arc<[RecordId]>>,
    /// When [`Untaken::heard`] last dropped each server's memo, for [`UNTAKEN_AGAIN`].
    dropped: BTreeMap<String, Instant>,
}

impl Memos {
    fn is_empty(&self) -> bool {
        self.gaps.is_empty() && self.dropped.is_empty()
    }

    /// Forget the drops older than [`UNTAKEN_AGAIN`] at `now`.
    fn lapse_drops(&mut self, now: Instant) {
        self.dropped
            .retain(|_, at| now.saturating_duration_since(*at) < UNTAKEN_AGAIN);
    }
}

/// Memos per store `G` and server URL, held in memory. Its lock is a leaf: nothing is called
/// while holding it, and the store is read outside it.
pub(crate) struct Untaken<G>(Mutex<BTreeMap<G, Memos>>);

impl<G: Ord + Clone> Untaken<G> {
    /// An empty table.
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<G, Memos>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn gap(&self, group: &G, url: &str) -> Option<Gap> {
        self.lock()
            .get(group)
            .and_then(|memos| memos.gaps.get(url))
            .cloned()
    }

    /// Drop `url`'s memo for `group`.
    fn forget(&self, group: &G, url: &str) {
        let mut table = self.lock();
        if let Some(memos) = table.get_mut(group) {
            memos.gaps.remove(url);
            if memos.is_empty() {
                table.remove(group);
            }
        }
    }

    /// A pull to `url` named its memo's heads at `now`.
    fn used(&self, group: &G, url: &str, now: Instant) {
        if let Some(gap) = self
            .lock()
            .get_mut(group)
            .and_then(|memos| memos.gaps.get_mut(url))
        {
            gap.used = now;
        }
    }

    /// What a pull from `url` names for `store`, whose own ids are `have` ([`Store::have`]): the
    /// heads of a memo that still applies and that the store does not hold complete, then `have`
    /// without them. The heads go first: the URL holds about 125 ids
    /// (`transport::records_have_url`) and `have` can name 256, so heads after it would be cut.
    /// A memo that no longer applies is dropped.
    pub(crate) fn have(
        &self,
        group: &G,
        url: &str,
        store: &Store,
        have: &[RecordId],
        now: Instant,
    ) -> Named {
        let heads: Vec<RecordId> = match self.gap(group, url) {
            Some(gap) if gap.holds(store, now) => {
                self.used(group, url, now);
                gap.heads
                    .into_iter()
                    .filter(|id| store.complete_bytes(id).is_none())
                    .collect()
            }
            Some(_) => {
                self.forget(group, url);
                Vec::new()
            }
            None => Vec::new(),
        };
        let mut seen: BTreeSet<RecordId> = heads.iter().copied().collect();
        let count = heads.len();
        let mut ids = heads;
        ids.extend(have.iter().filter(|id| seen.insert(**id)).copied());
        Named { ids, heads: count }
    }

    /// A pull from `url` that named a memo's heads heard at `now` whether the server lacks a
    /// record named (`X-Aterm-Records-Need`). A server that lacks a head (the URL reaches another
    /// server now, or one that reset its store) says so on every pull while the memo lives; the
    /// second such answer in a row drops the memo, so the pull names the store's own ids again.
    /// Not the first: a server one pass behind this Mac (a record this Mac minted, or pulled from
    /// another server first, which in a busy swarm is most passes) says it lacks a record named
    /// until it takes the post that follows, and dropping the memo then would bring the gap
    /// again. And at most once per server in [`UNTAKEN_AGAIN`]: the answer says only that the
    /// server lacks some id named, and a server that cannot take a record of this Mac's own
    /// (another network's, one its store refuses) says so on every pull whatever the memo names;
    /// dropping the memo on each second answer brought it the gap every third pass. Kept, the
    /// memo costs that server the store this Mac posts it, once per `REPOST_AGAIN`. A `404` (no
    /// store for the vault there) sends nothing, so a memo kept for it costs nothing; a store
    /// that appears there later and lacks a head says so.
    pub(crate) fn heard(&self, group: &G, url: &str, needs: bool, now: Instant) {
        let mut table = self.lock();
        let Some(memos) = table.get_mut(group) else {
            return;
        };
        memos.lapse_drops(now);
        let dropped_lately = memos.dropped.contains_key(url);
        let forget = match memos.gaps.get_mut(url) {
            Some(gap) if needs && gap.needy && !dropped_lately => true,
            Some(gap) => {
                gap.needy = needs;
                false
            }
            None => false,
        };
        if forget {
            memos.gaps.remove(url);
            while memos.dropped.len() >= UNTAKEN_URLS {
                let Some(oldest) = memos
                    .dropped
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(url, _)| url.clone())
                else {
                    break;
                };
                memos.dropped.remove(&oldest);
            }
            memos.dropped.insert(url.to_owned(), now);
        }
        if memos.is_empty() {
            table.remove(group);
        }
    }

    /// `url` sent `sent` to a pull of the pass `noted` describes: its `have` came from a store
    /// whose frontier was `noted.named_from`, and `noted.store` is the store that pass offered it
    /// to, after the pass. Keep the heads of what that store did not take (see the module doc),
    /// with the earlier memo's heads that still apply and that nothing sent now names as a
    /// parent, newest first, at most [`UNTAKEN_HEADS`]. The memo is dropped instead when the
    /// store has no genesis, does not hold every id of `named_from` complete (it lost records
    /// while the pull was out), or keeps no head. A table at [`UNTAKEN_URLS`] drops the memo used
    /// longest ago that is not the vault source's.
    pub(crate) fn note(&self, group: G, url: &str, sent: &Sent, noted: &Noted<'_>) {
        let Noted {
            store,
            named_from,
            now,
            source,
        } = *noted;
        if sent.0.is_empty() {
            return;
        }
        let Some(log) = store
            .log()
            .filter(|log| named_from.iter().all(|id| log.get(id).is_some()))
        else {
            self.forget(&group, url);
            return;
        };
        let complete = |id: &RecordId| log.get(id).is_some();
        let untaken: Vec<&Frame> = sent.0.iter().filter(|frame| !complete(&frame.id)).collect();
        let covered: BTreeSet<RecordId> = untaken
            .iter()
            .flat_map(|frame| frame.parents.iter().copied())
            .collect();
        let old = self.gap(&group, url).filter(|gap| gap.holds(store, now));
        let mut seen = BTreeSet::new();
        let heads: Vec<RecordId> = untaken
            .iter()
            .rev()
            .map(|frame| frame.id)
            .chain(old.iter().flat_map(|gap| gap.heads.iter().copied()))
            .filter(|id| !covered.contains(id) && !complete(id) && seen.insert(*id))
            .take(UNTAKEN_HEADS)
            .collect();
        if heads.is_empty() {
            self.forget(&group, url);
            return;
        }
        let frontier = store.frontier();
        let mut table = self.lock();
        let memos = table.entry(group).or_default();
        let floor = match &memos.floor {
            Some(floor) if **floor == *frontier => Arc::clone(floor),
            _ => {
                let floor: Arc<[RecordId]> = frontier.into();
                memos.floor = Some(Arc::clone(&floor));
                floor
            }
        };
        let gap = Gap {
            floor,
            heads,
            since: old.as_ref().map_or(now, |gap| gap.since),
            used: now,
            source: source == Some(url),
            needy: old.is_some_and(|gap| gap.needy),
        };
        memos
            .gaps
            .retain(|_, gap| now.saturating_duration_since(gap.since) < UNTAKEN_AGAIN);
        memos.lapse_drops(now);
        if !memos.gaps.contains_key(url) {
            while memos.gaps.len() >= UNTAKEN_URLS {
                let Some(stalest) = memos
                    .gaps
                    .iter()
                    .min_by_key(|(_, gap)| (gap.source, gap.used))
                    .map(|(url, _)| url.clone())
                else {
                    break;
                };
                memos.gaps.remove(&stalest);
            }
        }
        memos.gaps.insert(url.to_owned(), gap);
    }

    /// How many servers `group`'s table keeps a memo for.
    #[cfg(test)]
    fn kept(&self, group: &G) -> usize {
        self.lock().get(group).map_or(0, |memos| memos.gaps.len())
    }
}

#[cfg(test)]
pub(crate) mod chain;
#[cfg(test)]
mod tests;
