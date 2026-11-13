// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When this machine's books last heard each member, kept beside the books: a registration or a
//! heartbeat answer that moves nothing of an entry but `seen_ms` writes a line of
//! [`SEEN_FILE`], never the book whole.

use super::*;
use aterm_types::MutexExt as _;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// When the book ([`PEERS_FILE`]) and the copy of the issuer's book ([`ISSUER_BOOK_FILE`]) last
/// heard a member, in the state directory, where that is later than the entry's own `seen_ms`
/// in its file: one line per entry, `seen node=<id> ms=<unix ms>` for an entry of the book and
/// `copy node=<id> ms=<unix ms>` for a copy (`place=<host:port>` for an entry with no node id,
/// [`answer_key`]). Every read of either book through this module's readers ([`read_peers`],
/// [`read_issuer_book`]) raises each entry's `seen_ms` to its line's, so a contact that changes
/// nothing else of an entry writes these few bytes and not the book. A whole write of a book
/// carries the raised times and drops its lines ([`settle`]). Written under the book's lock,
/// and changed at most once in one hold of it: what a hold's writes note and settle is applied
/// together as the hold ends ([`finish`]). An older aterm never reads it, and reads the book's
/// own `seen_ms`, which no line is more than [`SEEN_BEHIND_MS`] later than.
pub const SEEN_FILE: &str = "fleet-seen";

/// The furthest a book's own `seen_ms` of an entry, in its file, may trail a time noted beside
/// it: a time later than that is the book's to write, whole, with every time noted so far
/// ([`trails`]). An aterm that reads the books and not [`SEEN_FILE`] (an older one that still
/// runs beside an updated node job) so reads a member that is heard every
/// [`HEARTBEAT_EVERY`] at most this and one such interval old, inside [`LIVE_WITHIN_MS`], which
/// this is half of; and a member heard that often brings a whole write at its second contact
/// after the last one, so a book is written every few minutes and not at every contact.
pub const SEEN_BEHIND_MS: u64 = LIVE_WITHIN_MS / 2;

/// The longest a time a served registration or a heartbeat round noted waits in this process
/// before [`SEEN_FILE`] holds it ([`HeldBook::update_paced`]): another process, and this one
/// after it is restarted or killed, reads a `seen_ms` at most this far behind. A ninth of
/// [`LIVE_WITHIN_MS`], so no reader's live window is off by more.
pub const SEEN_FLUSH_EVERY: Duration = Duration::from_secs(60);

/// Which book an entry's time is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Of {
    /// [`PEERS_FILE`].
    Book,
    /// [`ISSUER_BOOK_FILE`].
    Copy,
}

impl Of {
    /// The first word of its lines in [`SEEN_FILE`].
    const fn word(self) -> &'static str {
        match self {
            Self::Book => "seen",
            Self::Copy => "copy",
        }
    }
}

/// When a noted time reaches [`SEEN_FILE`].
#[derive(Clone, Copy, Debug)]
pub(super) enum Pace {
    /// In this call: a verb's write, whose process ends before any later one.
    Now,
    /// In this call where this process wrote none within [`SEEN_FLUSH_EVERY`] of this instant,
    /// else by the time that long has passed since its last write.
    Within(Instant),
}

/// Each entry's key ([`answer_key`]) under its book, with the latest time noted for it.
type Stamps = BTreeMap<(Of, String), u64>;

/// What this process holds of one state directory's times.
#[derive(Default)]
struct Kept {
    /// Times noted and not yet in [`SEEN_FILE`].
    pending: Stamps,
    /// When this process last wrote them out, or tried and could not.
    tried: Option<Instant>,
    /// Whether a thread waits to write `pending` out ([`arm`]); in a Cargo test binary, whether
    /// one would.
    timer: bool,
    /// A time noted in the lock hold now open is due in [`SEEN_FILE`] by its pace, as of this
    /// instant: the hold's end writes `pending` out ([`finish`]).
    due: Option<Instant>,
    /// The books written whole, or removed, whose lines [`SEEN_FILE`] may still hold
    /// ([`settle`]): no read of this process takes a line of them ([`times`]), and the end of
    /// the lock hold drops them from the file ([`finish`]).
    settled: Settled,
}

/// Which books' lines of [`SEEN_FILE`] are settled and not yet dropped from it.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Settled {
    book: bool,
    copy: bool,
}

impl Settled {
    const fn any(self) -> bool {
        self.book || self.copy
    }

    const fn has(self, of: Of) -> bool {
        match of {
            Of::Book => self.book,
            Of::Copy => self.copy,
        }
    }

    const fn set(&mut self, of: Of) {
        match of {
            Of::Book => self.book = true,
            Of::Copy => self.copy = true,
        }
    }
}

static KEPT: Mutex<BTreeMap<PathBuf, Kept>> = Mutex::new(BTreeMap::new());

/// The lines of [`SEEN_FILE`] in `dir`: empty where it is missing or refused; a line that is no
/// time is passed over.
fn read_file(dir: &Path) -> Stamps {
    static SEEN: KeptParse<Stamps> = KeptParse::new(KEPT_BOOKS);
    SEEN.read(dir, SEEN_FILE, PEERS_FILE_MAX, |bytes| {
        let mut stamps = Stamps::new();
        for line in String::from_utf8_lossy(bytes).lines() {
            for of in [Of::Book, Of::Copy] {
                if let Some((key, ms)) = stamp_line(of.word(), line) {
                    let at = stamps.entry((of, key.to_owned())).or_insert(ms);
                    *at = (*at).max(ms);
                }
            }
        }
        stamps
    })
    .unwrap_or_default()
}

/// The times noted beside the books of one state directory, as one instant held them.
pub(super) struct Times(Stamps);

/// The times [`SEEN_FILE`] in `dir` and this process hold now. This process's first, then the
/// file's: a write of the file ([`flush`]) puts a time there before this process lets it go.
/// The file's lines of a book this process wrote whole and has not yet dropped them for
/// ([`settle`]) are passed over: that write carried them.
pub(super) fn times(dir: &Path) -> Times {
    let (mut stamps, settled) = KEPT
        .lock_or_recover()
        .get(dir)
        .map(|kept| (kept.pending.clone(), kept.settled))
        .unwrap_or_default();
    for (key, ms) in read_file(dir) {
        if settled.has(key.0) {
            continue;
        }
        let at = stamps.entry(key).or_insert(ms);
        *at = (*at).max(ms);
    }
    Times(stamps)
}

/// What `read` reads of a book's file in `dir`, with the times noted beside the books as they
/// stood before it read: the order a reader that holds no lock needs. A whole write of a book
/// writes the file, carrying every time noted, and only then lets those times go ([`settle`]),
/// so times taken first are either still to be applied to the file `read` finds or already in
/// it; taken after, they could be gone while `read` had found the file of before that write,
/// and every entry would read as last heard when a whole write last said, however long ago.
pub(super) fn before<R>(dir: &Path, read: impl FnOnce() -> R) -> (Times, R) {
    let times = times(dir);
    (times, read())
}

impl Times {
    /// Raise the `seen_ms` of each of `peers`, entries of `of` as their file holds them, to the
    /// later time held for it. An entry with no `seen_ms` keeps none: a first time is the
    /// book's to write.
    pub(super) fn raise(&self, of: Of, peers: &mut [Peer]) {
        if self.0.is_empty() {
            return;
        }
        for peer in peers {
            let Some(seen) = peer.seen_ms else {
                continue;
            };
            let later = self.0.get(&(of, answer_key(peer))).copied();
            if later.is_some_and(|later| later > seen) {
                peer.seen_ms = later;
            }
        }
    }
}

/// Whether an entry of `after` was last heard more than [`SEEN_BEHIND_MS`] after `own` has it,
/// `own` being each entry's `seen_ms` as the book's file holds it, in `after`'s order: the
/// book is then written whole, not a line beside it.
pub(super) fn trails(own: &[Option<u64>], after: &[Peer]) -> bool {
    own.iter().zip(after).any(|(own, is)| {
        matches!((own, is.seen_ms), (Some(own), Some(new)) if new.saturating_sub(*own) > SEEN_BEHIND_MS)
    })
}

/// The key and new time of each entry of `after` that differs from its entry of `before`, where
/// the two books differ in nothing but a later `seen_ms` of entries that had one: what
/// [`SEEN_FILE`] can hold in the book's place. `None` for any other difference (an entry added,
/// taken out or moved, another field, a first or an earlier time), for a key two entries share,
/// and for a key no line of the file can name.
pub(super) fn advances(before: &[Peer], after: &[Peer]) -> Option<Vec<(String, u64)>> {
    if before.len() != after.len() {
        return None;
    }
    let mut stamps = Vec::new();
    for (was, is) in before.iter().zip(after) {
        if was == is {
            continue;
        }
        let (Some(old), Some(new)) = (was.seen_ms, is.seen_ms) else {
            return None;
        };
        let moved_only = new > old
            && Peer {
                seen_ms: was.seen_ms,
                ..is.clone()
            } == *was;
        let key = answer_key(is);
        if !moved_only || stamp_line("seen", &format!("seen {key} ms={new}")).is_none() {
            return None;
        }
        stamps.push((key, new));
    }
    let shared = stamps
        .iter()
        .any(|(key, _)| after.iter().filter(|peer| answer_key(peer) == *key).count() != 1);
    (!shared).then_some(stamps)
}

/// Note `stamps` ([`advances`]) of `of`'s entries in `dir`, under the book's lock: every read
/// of this process finds them at once ([`times`]), and [`SEEN_FILE`] takes them as `pace` says.
/// One due now is written as the lock hold ends ([`finish`]), with whatever else that hold
/// changes of the file; one not due is written by a thread of this process once
/// [`SEEN_FLUSH_EVERY`] has passed since its last write ([`flush_late`]).
pub(super) fn note(dir: &Path, of: Of, stamps: Vec<(String, u64)>, pace: Pace) {
    let due = {
        let mut kept = KEPT.lock_or_recover();
        // A directory with nothing noted, nothing owed to its file and no write within the
        // pace is as one never seen.
        if let Pace::Within(now) = pace {
            kept.retain(|_, kept| {
                kept.timer
                    || !kept.pending.is_empty()
                    || kept.due.is_some()
                    || kept.settled.any()
                    || kept
                        .tried
                        .is_some_and(|at| now.saturating_duration_since(at) < SEEN_FLUSH_EVERY)
            });
        }
        let kept = kept.entry(dir.to_owned()).or_default();
        for (key, ms) in stamps {
            let at = kept.pending.entry((of, key)).or_insert(ms);
            *at = (*at).max(ms);
        }
        let due = match pace {
            Pace::Now => Some(Instant::now()),
            Pace::Within(now) => kept
                .tried
                .is_none_or(|at| now.saturating_duration_since(at) >= SEEN_FLUSH_EVERY)
                .then_some(now),
        };
        if due.is_some() {
            kept.due = due;
        }
        due
    };
    if due.is_none() {
        arm(dir);
    }
}

/// Each entry's key in `peers` with its own `seen_ms`, as its file holds it.
fn own_times(of: Of, peers: &[Peer]) -> impl Iterator<Item = ((Of, String), u64)> + '_ {
    peers
        .iter()
        .filter_map(move |peer| Some(((of, answer_key(peer)), peer.seen_ms?)))
}

/// Write [`SEEN_FILE`] in `dir`, under the book's lock, as its lines and the times this process
/// noted make it, each key's later time standing: only the lines later than their entry's own
/// `seen_ms` in its file, so a line whose entry is gone, or whose book was written whole since,
/// goes; no write where that is what the file holds, and no file where no line is left. The
/// lines of a book that cannot be read are kept.
fn flush(dir: &Path, now: Instant) -> io::Result<()> {
    rewrite(dir, Some(now))
}

/// The one change of [`SEEN_FILE`] in `dir` a hold of the book's lock makes: the lines of the
/// books settled since its last change dropped ([`settle`]) and, where `flush` gives the
/// instant of a write that is due, the times this process noted written with them as
/// [`flush`] says. No write where the file holds that already.
fn rewrite(dir: &Path, flush: Option<Instant>) -> io::Result<()> {
    let (pending, settled) = KEPT
        .lock_or_recover()
        .get(dir)
        .map(|kept| {
            let pending = flush.map(|_| kept.pending.clone()).unwrap_or_default();
            (pending, kept.settled)
        })
        .unwrap_or_default();
    let file = read_file(dir);
    let mut next = file.clone();
    next.retain(|key, _| !settled.has(key.0));
    if flush.is_some() {
        for (key, ms) in &pending {
            let at = next.entry(key.clone()).or_insert(*ms);
            *at = (*at).max(*ms);
        }
        let book = read_book_as_written(dir, held_mark(dir)).map(|(mut held, mut others)| {
            held.append(&mut others);
            held
        });
        let copies = issuer_copy(dir)
            .and_then(|copy| copy.copies)
            .unwrap_or_default();
        let own: BTreeMap<(Of, String), u64> = book
            .iter()
            .flat_map(|book| own_times(Of::Book, book))
            .chain(own_times(Of::Copy, &copies))
            .fold(BTreeMap::new(), |mut own, (key, seen)| {
                // Of two entries under one key, a line is later than both or stands for neither.
                let at = own.entry(key).or_insert(seen);
                *at = (*at).max(seen);
                own
            });
        next.retain(|key, ms| {
            (key.0 == Of::Book && book.is_err()) || own.get(key).is_some_and(|seen| seen < ms)
        });
    }
    if next != file {
        write_file(dir, &next)?;
    }
    if let Some(kept) = KEPT.lock_or_recover().get_mut(dir) {
        kept.settled = Settled::default();
        if let Some(now) = flush {
            kept.pending.clear();
            kept.tried = Some(now);
            kept.due = None;
        }
    }
    Ok(())
}

/// A hold of the book's lock in `dir` ends ([`HeldBook`]'s drop, the lock still held): what its
/// writes owe [`SEEN_FILE`] is applied as one change of the file ([`rewrite`]), the lines of
/// the books it wrote whole dropped and the times it noted that are due written. Nothing is
/// read or written where the hold owes nothing. A change that fails keeps the times noted, to
/// be tried again [`SEEN_FLUSH_EVERY`] later ([`retry`]), and the settled lines passed over by
/// this process's reads until a later change drops them.
pub(super) fn finish(dir: &Path) {
    let owed = KEPT
        .lock_or_recover()
        .get(dir)
        .filter(|kept| kept.due.is_some() || kept.settled.any())
        .map(|kept| kept.due);
    let Some(due) = owed else {
        return;
    };
    if rewrite(dir, due).is_err()
        && let Some(now) = due
    {
        if let Some(kept) = KEPT.lock_or_recover().get_mut(dir) {
            kept.due = None;
        }
        retry(dir, now);
    }
}

/// [`SEEN_FILE`] in `dir` as `stamps`, whole, or no file for none.
fn write_file(dir: &Path, stamps: &Stamps) -> io::Result<()> {
    if stamps.is_empty() {
        return remove_private(dir, SEEN_FILE).map(|_| ());
    }
    let mut text = String::from(
        "# When this machine's book, and its copy of its issuer's book, last heard a member, \
         where that is later than the entry's own seen_ms: written by aterm, never by hand.\n",
    );
    for ((of, key), ms) in stamps {
        text.push_str(&format!("{} {key} ms={ms}\n", of.word()));
    }
    write_private(dir, SEEN_FILE, text.as_bytes())
}

/// A whole write of `of`'s book in `dir` was made under the book's lock, or the book removed:
/// what was written carries every time [`raise`] read, so the times this process noted for it
/// go, and its lines of [`SEEN_FILE`] are passed over by this process's reads from now
/// ([`times`]), so a `seen_ms` the write set earlier stands, and dropped from the file as the
/// lock hold ends ([`finish`]), in the one change that hold makes of it. After the book's own
/// write: until that change, and after a stop before it or its failure, the file's lines are no
/// later than the book's `seen_ms`, which raise nothing, but for an entry whose time that write
/// set earlier, which another process reads at its line's time until then, or until the next
/// whole write of its book.
pub(super) fn settle(dir: &Path, of: Of) {
    let mut kept = KEPT.lock_or_recover();
    let kept = kept.entry(dir.to_owned()).or_default();
    kept.pending.retain(|key, _| key.0 != of);
    kept.settled.set(of);
}

/// The state directory's books go with a leave: every time of `dir` this process noted, and
/// [`SEEN_FILE`]. Whether the file was there.
/// # Errors
/// The file is there and cannot be removed.
pub(super) fn forget(dir: &Path) -> io::Result<bool> {
    KEPT.lock_or_recover().remove(dir);
    remove_private(dir, SEEN_FILE)
}

/// A write of what this process noted of `dir` failed at `now`: the times stay noted, and a
/// thread tries again once [`SEEN_FLUSH_EVERY`] has passed ([`arm`]), whether or not anything
/// more is noted meanwhile, until a write is made.
fn retry(dir: &Path, now: Instant) {
    if let Some(kept) = KEPT.lock_or_recover().get_mut(dir) {
        kept.tried = Some(now);
    }
    arm(dir);
}

/// Whether a thread waits to write what this process noted of `dir`, or would outside a test.
#[cfg(test)]
pub(super) fn waits(dir: &Path) -> bool {
    KEPT.lock_or_recover()
        .get(dir)
        .is_some_and(|kept| kept.timer)
}

/// Have a thread of this process write what it noted of `dir` once [`SEEN_FLUSH_EVERY`] has
/// passed since its last write or try ([`flush_late`]); one such thread at a time. None in a
/// Cargo test binary, whose tests call [`flush_late`] at the instants they choose. Where the
/// thread cannot be started, the next time noted starts one.
fn arm(dir: &Path) {
    static IN_A_TEST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let due = {
        let mut kept = KEPT.lock_or_recover();
        let Some(kept) = kept.get_mut(dir) else {
            return;
        };
        if kept.timer {
            return;
        }
        kept.timer = true;
        kept.tried.and_then(|at| at.checked_add(SEEN_FLUSH_EVERY))
    };
    if *IN_A_TEST.get_or_init(|| {
        std::env::current_exe().is_ok_and(|exe| aterm_uds::test_binary::is_test_binary(&exe))
    }) {
        return;
    }
    let owned = dir.to_owned();
    let spawned = std::thread::Builder::new()
        .name("fleet-seen".to_owned())
        .spawn(move || {
            if let Some(due) = due {
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
            // One that fails keeps its times noted and arms a thread again ([`retry`]).
            let _ = flush_late(&owned, Instant::now());
        });
    if spawned.is_err()
        && let Some(kept) = KEPT.lock_or_recover().get_mut(dir)
    {
        kept.timer = false;
    }
}

/// Write what this process noted of `dir` to [`SEEN_FILE`], under the book's lock, where
/// [`SEEN_FLUSH_EVERY`] has passed at `now` since its last write or try: whether it was due.
/// What is noted and not due waits for another thread ([`arm`]).
/// # Errors
/// The book's lock cannot be taken, or the file cannot be written: the times stay noted, and a
/// thread tries again that long after `now` ([`retry`]).
pub(super) fn flush_late(dir: &Path, now: Instant) -> io::Result<bool> {
    let due = {
        let mut kept = KEPT.lock_or_recover();
        let Some(kept) = kept.get_mut(dir) else {
            return Ok(false);
        };
        kept.timer = false;
        if kept.pending.is_empty() {
            return Ok(false);
        }
        kept.tried
            .is_none_or(|at| now.saturating_duration_since(at) >= SEEN_FLUSH_EVERY)
    };
    if !due {
        arm(dir);
        return Ok(false);
    }
    let wrote = BookLock::take_default_at(dir, Instant::now()).and_then(|_lock| flush(dir, now));
    wrote.map(|()| true).inspect_err(|_| retry(dir, now))
}

/// Write what this process noted of `dir` to [`SEEN_FILE`] now, whatever its pace, under the
/// book's lock, waiting `within` for it: a process that stays calls this as it ends in order.
/// # Errors
/// The lock is not free in time, or the file cannot be written.
pub(super) fn flush_now(dir: &Path, within: Duration) -> io::Result<()> {
    let noted = KEPT
        .lock_or_recover()
        .get(dir)
        .is_some_and(|kept| !kept.pending.is_empty() || kept.settled.any());
    if !noted {
        return Ok(());
    }
    let _lock = BookLock::take(dir, within)?;
    flush(dir, Instant::now())
}
