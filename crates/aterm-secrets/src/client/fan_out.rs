// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One exchange with each of many vault servers, all of them at once.
//!
//! The servers a vault command reaches are the source, the swarm policy's servers, the
//! compiled-in defaults and up to [`crate::discover::MAX_CANDIDATES`] a browse heard, which
//! nothing has verified, and any of them can take a connection and never read it (a wedged
//! aterm). One after another, each such server held the command for one exchange deadline: on
//! 2026-11-02 `aterm secrets approve`, its version stored and applied, went on for minutes, one
//! 120 s records post after another to the servers of Macs that had hung. At once, sharing one
//! deadline ([`each_at_once`], [`post_records`], [`reads_within`]), they hold it for one
//! deadline however many of them hang.

use super::*;
use crate::crossing::RecordsBound;
use crate::server::{ENVELOPE_PEEK, ENVELOPE_PULLS_AT_ONCE, PullSlots, RECORDS_PULLS_AT_ONCE};
use crate::transport::Pulled;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

/// The most threads one [`each_at_once`] runs, its caller's among them: one per item, up to eight
/// times the servers a browse names. That leaves room for the source, the defaults, a full
/// browse ([`crate::discover::MAX_CANDIDATES`]) and a policy naming about a hundred servers,
/// each dialed at once. A longer list's later items wait for a thread to come free, and where
/// every thread's exchange runs to the deadline they share, those items end at it undialed. The
/// bound is there because each thread holds a socket, and macOS's default limit is 256
/// descriptors a process.
pub(super) const MOST_AT_ONCE: usize = 8 * crate::discover::MAX_CANDIDATES;

/// `exchange` run on every one of `items` at once, on up to [`MOST_AT_ONCE`] threads named
/// `name` (this one among them): each item's result in `items`' order, `None` for one whose
/// exchange panicked. It returns once every exchange has ended, so no worker outlives the call.
/// Where the system starts fewer threads, the ones it started take the rest of the items, this
/// one at least.
pub(super) fn each_at_once<I: Sync, T: Send>(
    name: &str,
    items: &[I],
    exchange: impl Fn(&I) -> T + Sync,
) -> Vec<Option<T>> {
    let slots: Vec<Mutex<Option<T>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let work = || {
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(index) else {
                return;
            };
            // A panic ends this item's exchange alone: the thread goes on to the next item.
            if let Ok(result) = std::panic::catch_unwind(AssertUnwindSafe(|| exchange(item))) {
                let fan_out_slot = &slots[index];
                *fan_out_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
            }
        }
    };
    // `work` holds only shared borrows, so each thread takes a copy of it.
    std::thread::scope(|scope| {
        for _ in 1..items.len().min(MOST_AT_ONCE) {
            let spawned = std::thread::Builder::new()
                .name(name.to_owned())
                .spawn_scoped(scope, work);
            if spawned.is_err() {
                break;
            }
        }
        work();
    });
    slots
        .into_iter()
        .map(|slot| slot.into_inner().unwrap_or_else(PoisonError::into_inner))
        .collect()
}

/// The deadline the exchanges of one fan-out share where its caller set none: one
/// [`transport::EXCHANGE`] from now.
pub(super) fn one_exchange() -> Instant {
    Instant::now() + transport::EXCHANGE
}

/// `POST <url>/records` of each `(url, records)` of `posts` at once ([`each_at_once`]), every post
/// stating `bound` ([`crate::crossing`]) and ending by `by`: what each server answered, in
/// `posts`' order, as [`transport::post_records_answered`] tells it. One that starts past `by`
/// dials nothing and answers [`crate::ErrorKind::TimedOut`], so every post a caller makes under
/// one `by` ends by it.
pub(super) fn post_records(
    posts: &[(&str, &[u8])],
    bound: RecordsBound,
    by: Instant,
) -> Vec<crate::Result<crate::Result<()>>> {
    each_at_once("vault-records", posts, |(url, records)| {
        transport::post_records_answered_until(url, records, bound, Some(by))
    })
    .into_iter()
    .map(|answered| answered.unwrap_or_else(|| Err(Error::new("a records post panicked"))))
    .collect()
}

/// Post each immutable record selection, stating `bound`, under the one fan-out deadline.
pub(super) fn post_record_selections(
    posts: &[(&str, &[crate::membership_log::Record])],
    bound: RecordsBound,
    by: Instant,
) -> Vec<crate::Result<crate::Result<()>>> {
    each_at_once("vault-records", posts, |(url, records)| {
        transport::post_record_selection_answered_until(url, records, bound, Some(by))
    })
    .into_iter()
    .map(|answered| answered.unwrap_or_else(|| Err(Error::new("a records post panicked"))))
    .collect()
}

/// [`post_records`] of the same `records` to every one of `urls`, stating `bound`, by `by`: each
/// server's answer as [`transport::post_records`] gives it, in `urls`' order.
pub(super) fn post_records_to(
    urls: &[String],
    records: &[u8],
    bound: RecordsBound,
    by: Instant,
) -> Vec<crate::Result<()>> {
    let posts: Vec<(&str, &[u8])> = urls.iter().map(|url| (url.as_str(), records)).collect();
    post_records(&posts, bound, by)
        .into_iter()
        .map(|answered| answered.and_then(|judged| judged))
        .collect()
}

/// How a fan-out ([`reads_within`]) reads each server's answer: the most of it read first, from
/// every server at once; the most of a longer answer read when it is asked again and read whole;
/// and how many such whole reads run at once.
#[derive(Clone, Copy, Debug)]
pub(super) struct Reads {
    /// The most of each server's answer read first, from every server at once.
    pub(super) peek: usize,
    /// The most of an answer longer than `peek` read when it is asked again.
    pub(super) whole: usize,
    /// How many answers longer than `peek` are read whole at once.
    pub(super) at_once: usize,
}

/// A vault server's envelope: [`ENVELOPE_PEEK`] of every answer at once, which a `204` and the
/// ALab vault's envelope (144 KB on 2026-11-02) fit, and [`ENVELOPE_PULLS_AT_ONCE`] longer ones
/// whole at once, up to [`envelope::MAX_ENVELOPE`], as a server's pulls from its peers read them.
pub(super) const ENVELOPES: Reads = Reads {
    peek: ENVELOPE_PEEK,
    whole: envelope::MAX_ENVELOPE,
    at_once: ENVELOPE_PULLS_AT_ONCE,
};

/// The records a server answers a pull ([`transport::fetch_records_within`]): [`RECORDS_PEEK`] of
/// every answer at once, and [`RECORDS_PULLS_AT_ONCE`] longer ones whole at once, up to
/// [`crate::membership_log::MAX_RECORDS_STREAM`], as many as a server's pulls of its peers'
/// records read at once.
pub(super) const RECORDS: Reads = Reads {
    peek: RECORDS_PEEK,
    whole: crate::membership_log::MAX_RECORDS_STREAM,
    at_once: RECORDS_PULLS_AT_ONCE,
};

/// How much of each server's answer to a records pull a pass reads first, from every server at
/// once ([`RECORDS`]): a `204` and a `404` fit, and so do the records a pull most often brings,
/// which are only those past what it named the store holds (`have`).
const RECORDS_PEEK: usize = 256 << 10;

/// `read` of each of `urls` at once ([`each_at_once`]), as `reads` says, every read ending within
/// `within` of now, and what `judge` makes of each server's answer: in `urls`' order, `None` for a
/// server whose read or judgment panicked.
///
/// `read(url, left, limit)` asks `url` for its answer, all of it within `left`, reading a body of
/// at most `limit` bytes ([`Pulled::Larger`] past it). Every server's answer is read first under
/// `reads.peek`, all at once. One longer than that is asked again and read whole, up to
/// `reads.whole`, under one of `reads.at_once` slots, held until `judge` has made what it makes of
/// it, as a server's pull from its peers reads them. Records can be verified incrementally by
/// `read`; their Store judgment still runs before the slot is released. Up to
/// [`crate::discover::MAX_CANDIDATES`] servers are a browse's, which any host on the LAN can
/// advertise: read whole at once, 16 instances of one host, each answering with a 16 MiB envelope,
/// had a fan-out hold 256 MiB of them before any was refused.
///
/// A compiled-in default address ([`crate::ALAB_DEFAULT_URLS`]), the first time `urls` names it,
/// reads its longer answer whole without a slot. A browse cannot add one, so the whole reads at
/// once stay `reads.at_once` and that handful, and servers a browse named, which can take every
/// slot and hold it until `within` ends (a LAN host's instances that answer the peek with a long
/// length and then send it slowly), cannot keep the vault's own address from being read. They can
/// still keep one another's longer answers waiting for a slot until no time is left, the cost of
/// holding only so many unverified bodies at once.
///
/// `judge(url, read)` takes what the last read of `url` came to: its answer, read whole;
/// [`Pulled::Larger`] for one longer than `reads.whole`; or the error `read` gave, as it gave it,
/// or a [`crate::ErrorKind::TimedOut`] one for a read `within` left no time for.
pub(super) fn reads_within<A, T: Send>(
    reads: Reads,
    urls: &[String],
    within: Duration,
    read: impl Fn(&str, Duration, usize) -> crate::Result<Pulled<A>> + Sync,
    judge: impl Fn(&str, crate::Result<Pulled<A>>) -> T + Sync,
) -> Vec<Option<T>> {
    let slots = PullSlots::new(reads.at_once);
    #[cfg(test)]
    held::watched(&slots);
    let by = Instant::now() + within;
    let read_by = |url: &str, limit: usize| {
        let left = by.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::of_kind(
                crate::ErrorKind::TimedOut,
                format!("reading {}: no time was left", transport::describe(url)),
            ));
        }
        read(url, left, limit)
    };
    // Each server, with whether it is the first naming in `urls` of a default address.
    let servers: Vec<(&str, bool)> = urls
        .iter()
        .enumerate()
        .map(|(at, url)| {
            let default = crate::is_alab_default_url(url) && !urls[..at].contains(url);
            (url.as_str(), default)
        })
        .collect();
    each_at_once("vault-read", &servers, |&(url, default)| {
        match read_by(url, reads.peek) {
            Ok(Pulled::Larger) if default => judge(url, read_by(url, reads.whole)),
            Ok(Pulled::Larger) => {
                // Every slot's read ends by `by`, so a wait for one ends by then too, give or take
                // what `judge` takes, and finds no time left to read in.
                let _slot = slots.take();
                judge(url, read_by(url, reads.whole))
            }
            peeked => judge(url, peeked),
        }
    })
}

/// [`reads_within`] of the envelope each of `urls` serves, and what `keep` makes of each one read
/// whole, given the URL that served it: in `urls`' order, `None` for a server that answered nothing in time, failed, or served
/// nothing `keep` kept.
pub(super) fn envelopes_within<K: Send>(
    urls: &[String],
    within: Duration,
    read: impl Fn(&str, Duration, usize) -> crate::Result<Pulled> + Sync,
    keep: impl Fn(&str, Fetched) -> Option<K> + Sync,
) -> Vec<Option<K>> {
    reads_within(ENVELOPES, urls, within, read, |url, read| match read {
        Ok(Pulled::Fetched(fetched)) => keep(url, fetched),
        Ok(Pulled::Larger) | Err(_) => None,
    })
    .into_iter()
    .map(Option::flatten)
    .collect()
}

/// [`envelopes_within`] of the envelope each of `urls` serves `reader`: what `keep` makes of each
/// body, in `urls`' order, `None` for a server that served none in time (or served nothing newer,
/// or failed) and for one whose body `keep` kept nothing of.
pub(super) fn bodies_within<K: Send>(
    urls: &[String],
    within: Duration,
    reader: Option<&str>,
    keep: impl Fn(Vec<u8>) -> Option<K> + Sync,
) -> Vec<Option<K>> {
    envelopes_within(
        urls,
        within,
        |url, left, limit| transport::get_within_as(url, left, None, reader, limit),
        |_, fetched| match fetched {
            Fetched::Body(bytes) => keep(bytes),
            Fetched::Missing | Fetched::NoNewer(_) => None,
        },
    )
}

/// Servers whose every answer is longer than a fan-out's peek, each holding its whole read until
/// the test lets them all go, so a test can count the whole reads a fan-out makes at once
/// ([`reads_within`]'s bound), and the slots the fan-out it runs waits on.
#[cfg(test)]
#[path = "fan_out_held_tests.rs"]
pub(super) mod held;

#[cfg(test)]
#[path = "fan_out_tests.rs"]
mod tests;
