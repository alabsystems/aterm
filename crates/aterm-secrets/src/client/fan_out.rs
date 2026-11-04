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
use std::sync::atomic::AtomicUsize;
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
/// whole: in `urls`' order, `None` for a server that answered nothing in time, failed, or served
/// nothing `keep` kept.
pub(super) fn envelopes_within<K: Send>(
    urls: &[String],
    within: Duration,
    read: impl Fn(&str, Duration, usize) -> crate::Result<Pulled> + Sync,
    keep: impl Fn(Fetched) -> Option<K> + Sync,
) -> Vec<Option<K>> {
    reads_within(ENVELOPES, urls, within, read, |_, read| match read {
        Ok(Pulled::Fetched(fetched)) => keep(fetched),
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
        |fetched| match fetched {
            Fetched::Body(bytes) => keep(bytes),
            Fetched::Missing | Fetched::NoNewer(_) => None,
        },
    )
}

/// Servers whose every answer is longer than a fan-out's peek, each holding its whole read until
/// the test lets them all go, so a test can count the whole reads a fan-out makes at once
/// ([`reads_within`]'s bound), and the slots the fan-out it runs waits on.
#[cfg(test)]
pub(super) mod held {
    use super::*;
    use std::cell::RefCell;
    use std::sync::{Arc, Condvar};

    /// Where the slots' waiting count goes.
    type Sink = Arc<Mutex<Option<Arc<AtomicUsize>>>>;

    thread_local! {
        /// Where a [`reads_within`] run on this thread puts its slots' waiting count, while
        /// [`HeldReads::count`] watches the fan-out it runs on this thread.
        static WATCHING: RefCell<Option<Sink>> = const { RefCell::new(None) };
    }

    /// What [`reads_within`] calls with the slots it made: their waiting count, to the test that
    /// watches this thread.
    pub(in crate::client) fn watched(slots: &PullSlots) {
        WATCHING.with(|watching| {
            if let Some(sink) = &*watching.borrow() {
                *sink.lock().expect("the watched slots") = Some(Arc::clone(&slots.waiting));
            }
        });
    }

    /// The whole reads the servers hold, and the reads made under the peek.
    pub(in crate::client) struct HeldReads {
        reads: Reads,
        whole: Mutex<Whole>,
        released: Condvar,
        /// How many reads were made under the peek.
        pub(in crate::client) peeks: AtomicUsize,
    }

    #[derive(Default)]
    struct Whole {
        in_flight: usize,
        most: usize,
        released: bool,
    }

    /// What a fan-out [`HeldReads::count`] watches is doing.
    #[derive(Clone, Copy, Debug)]
    pub(in crate::client) struct Seen {
        /// Whole reads the servers hold.
        pub(in crate::client) in_flight: usize,
        /// Whole reads waiting for one of the fan-out's slots.
        pub(in crate::client) waiting: usize,
    }

    /// What [`HeldReads::count`] saw of a fan-out.
    pub(in crate::client) struct Counted<T> {
        /// What the fan-out was doing once it settled.
        pub(in crate::client) seen: Seen,
        /// The most whole reads in flight at once, the fan-out's whole run.
        pub(in crate::client) most: usize,
        /// What the fan-out returned.
        pub(in crate::client) result: T,
    }

    impl HeldReads {
        /// Servers read as `reads` says.
        pub(in crate::client) fn new(reads: Reads) -> Self {
            Self {
                reads,
                whole: Mutex::default(),
                released: Condvar::new(),
                peeks: AtomicUsize::new(0),
            }
        }

        /// One server's read of at most `limit` bytes: under the peek, [`Pulled::Larger`];
        /// whole, `answer`, once [`HeldReads::hold`] lets it go.
        pub(in crate::client) fn read<A>(
            &self,
            limit: usize,
            answer: A,
        ) -> crate::Result<Pulled<A>> {
            if limit == self.reads.peek {
                self.peeks.fetch_add(1, Ordering::SeqCst);
                return Ok(Pulled::Larger);
            }
            assert_eq!(
                limit, self.reads.whole,
                "a whole read takes the whole answer"
            );
            self.hold();
            Ok(Pulled::Fetched(answer))
        }

        /// One whole read, held in flight until the test lets every whole read go (or the hang
        /// detector ends this one's wait).
        pub(in crate::client) fn hold(&self) {
            let mut whole = self.whole.lock().expect("whole reads");
            whole.in_flight += 1;
            whole.most = whole.most.max(whole.in_flight);
            let (mut whole, _) = self
                .released
                .wait_timeout_while(whole, aterm_spec::wait::MUST_HAPPEN, |whole| {
                    !whole.released
                })
                .expect("whole reads");
            whole.in_flight -= 1;
        }

        /// The whole reads held now.
        pub(in crate::client) fn in_flight(&self) -> usize {
            self.whole.lock().expect("whole reads").in_flight
        }

        /// Whether the test has let the whole reads go.
        pub(in crate::client) fn let_go(&self) -> bool {
            self.whole.lock().expect("whole reads").released
        }

        /// `fan_out`, run on a thread of its own over `servers` of these servers: counted once
        /// [`Reads::at_once`] whole reads are in flight and the rest wait for a slot, or once
        /// more are in flight, then let go and waited for. Give the fan-out a deadline past the
        /// hang detector, so it can neither finish nor end a read while this counts.
        pub(in crate::client) fn count<T: Send>(
            &self,
            servers: usize,
            fan_out: impl FnOnce() -> T + Send,
        ) -> Counted<T> {
            let at_once = self.reads.at_once;
            self.count_until(
                "the slots' whole reads in flight and the rest waiting for a slot, or more in flight",
                |seen| {
                    seen.in_flight > at_once
                        || (seen.in_flight == at_once && seen.waiting == servers - at_once)
                },
                fan_out,
            )
        }

        /// `fan_out`, run on a thread of its own: counted once what it does is `settled`, then
        /// let go and waited for. The waiting count is that of the slots the last
        /// [`reads_within`] it ran made ([`watched`]), the ones a shipped caller reads under.
        pub(in crate::client) fn count_until<T: Send>(
            &self,
            what: &str,
            settled: impl Fn(Seen) -> bool,
            fan_out: impl FnOnce() -> T + Send,
        ) -> Counted<T> {
            let sink = Sink::default();
            let seen = || Seen {
                in_flight: self.in_flight(),
                waiting: sink
                    .lock()
                    .expect("the watched slots")
                    .as_ref()
                    .map_or(0, |waiting| waiting.load(Ordering::SeqCst)),
            };
            std::thread::scope(|scope| {
                let watching = Arc::clone(&sink);
                let running = scope.spawn(move || {
                    WATCHING.with(|slot| *slot.borrow_mut() = Some(watching));
                    fan_out()
                });
                aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
                    settled(seen()).then_some(())
                });
                let settled = seen();
                self.whole.lock().expect("whole reads").released = true;
                self.released.notify_all();
                let result = running.join().expect("the fan-out");
                Counted {
                    seen: settled,
                    most: self.whole.lock().expect("whole reads").most,
                    result,
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Condvar};

    /// A gate that opens once `count` callers stand at it: each caller waits for the rest, and
    /// says whether the gate opened. A caller the hang detector ends breaks it, and every later
    /// caller finds it broken at once, so a run one after another fails in one detector's time.
    struct Gate {
        /// How many callers arrived, and whether one gave up on the rest.
        state: Mutex<(usize, bool)>,
        changed: Condvar,
        count: usize,
    }

    impl Gate {
        fn new(count: usize) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new((0, false)),
                changed: Condvar::new(),
                count,
            })
        }

        fn arrive(&self) -> bool {
            let mut state = self.state.lock().expect("gate");
            state.0 += 1;
            self.changed.notify_all();
            let (mut state, waited) = self
                .changed
                .wait_timeout_while(state, aterm_spec::wait::MUST_HAPPEN, |(arrived, broken)| {
                    *arrived < self.count && !*broken
                })
                .expect("gate");
            if waited.timed_out() {
                state.1 = true;
                self.changed.notify_all();
            }
            !state.1
        }
    }

    /// Every item's exchange is under way before any of them ends, for a list as long as the
    /// source, the defaults, a full browse and a policy naming a hundred servers ([`MOST_AT_ONCE`]):
    /// each one waits at a gate that opens only once all of them stand at it, and every one of
    /// them sees it open.
    ///
    /// NEGATIVE CONTROL: run the items one after another (`items.iter().map(|item|
    /// Some(exchange(item))).collect()`): the first exchange waits alone until the hang
    /// detector ends it, every exchange answers `false`, and the assert fails. Start fewer
    /// threads than items (`MOST_AT_ONCE = 4 * MAX_CANDIDATES`, as this module did first): the
    /// gate never opens, and the assert fails the same way.
    #[test]
    fn every_exchange_is_under_way_at_once() {
        const POLICY_SERVERS: usize = 100;
        let servers =
            1 + crate::ALAB_DEFAULT_URLS.len() + crate::discover::MAX_CANDIDATES + POLICY_SERVERS;
        let items: Vec<usize> = (0..servers).collect();
        let gate = Gate::new(items.len());
        let opened = each_at_once("test-fan-out", &items, |&item| (item, gate.arrive()));
        let expected: Vec<Option<(usize, bool)>> =
            items.iter().map(|&item| Some((item, true))).collect();
        assert_eq!(
            opened, expected,
            "never: every exchange under way at once, each result in its item's place"
        );
    }

    /// A list longer than [`MOST_AT_ONCE`], which has fewer threads than items, still runs every
    /// item, each result in its item's place: a thread whose exchange ends goes on to the next
    /// item no thread has taken.
    ///
    /// NEGATIVE CONTROL: let `work` return after its first exchange: the items past the
    /// [`MOST_AT_ONCE`] the threads took first are never run, stay `None`, and the assert fails.
    #[test]
    fn a_list_past_the_bound_runs_every_item() {
        let items: Vec<usize> = (0..MOST_AT_ONCE * 2 + 3).collect();
        let results = each_at_once("test-fan-out", &items, |&item| item * 2);
        let expected: Vec<Option<usize>> = items.iter().map(|&item| Some(item * 2)).collect();
        assert_eq!(results, expected);
    }

    /// An exchange that panics leaves `None` in its place and every other item's result in
    /// its own.
    #[test]
    fn a_panicking_exchange_loses_only_its_own_result() {
        let items = [0usize, 1, 2, 3];
        let results = each_at_once("test-fan-out", &items, |&item| {
            assert_ne!(item, 2, "the test's panic");
            item
        });
        assert_eq!(results, vec![Some(0), Some(1), None, Some(3)]);
    }

    /// Nothing to do starts nothing; one item runs on this thread.
    #[test]
    fn no_items_and_one_item_start_no_thread() {
        let none: [usize; 0] = [];
        assert!(each_at_once("test-fan-out", &none, |_| ()).is_empty());
        let here = std::thread::current().id();
        assert_eq!(
            each_at_once("test-fan-out", &[()], |()| std::thread::current().id()),
            vec![Some(here)]
        );
    }

    /// A post whose shared deadline passed before it began dials nothing and says it timed out:
    /// the posts of one fan-out end by their deadline however long the list.
    #[test]
    fn a_records_post_past_its_deadline_dials_nothing() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
        let past = Instant::now();
        let answered = transport::post_records_answered_until(
            &url,
            b"records",
            RecordsBound::uncrossed(),
            Some(past),
        );
        assert_eq!(
            answered.expect_err("timed out").kind(),
            crate::ErrorKind::TimedOut
        );
        assert!(
            listener.accept().is_err(),
            "a post past its deadline connected"
        );
    }

    /// A fan-out reads at most [`ENVELOPE_PULLS_AT_ONCE`] envelopes longer than
    /// [`ENVELOPE_PEEK`] whole at once, however many servers it reads, and still reads each of
    /// them whole and keeps it in its place: every answer is read first under the peek, all at
    /// once, and only one longer than that is asked again and read whole, under a slot. Before,
    /// every server's envelope was read whole at once, so a LAN host that advertised 16 instances
    /// of itself, each answering with a 16 MiB body, had an admin command hold 256 MiB of bodies
    /// nothing had verified. Here the read of each of 16 servers answers its first try with more
    /// than the peek and holds its whole read until the test lets it go ([`held::HeldReads`]),
    /// and the slots counted are the ones [`reads_within`] makes for its callers.
    ///
    /// NEGATIVE CONTROL: no `slots.take()` in `reads_within` (every whole read at once, as
    /// `bodies_within` read before): more whole reads in flight at once, `left: 16, right: 2`,
    /// "whole reads in flight at once" (any count over 2 fails it). The slots `reads_within`
    /// makes for as many reads at once as a browse names (`PullSlots::new(
    /// crate::discover::MAX_CANDIDATES)`): the same.
    #[test]
    fn a_fan_out_reads_at_most_two_envelopes_whole_at_once() {
        const SERVERS: usize = 16;
        let held = held::HeldReads::new(ENVELOPES);
        let urls: Vec<String> = (0..SERVERS)
            .map(|server| format!("http://server-{server}/v1/alab"))
            .collect();
        let read = |url: &str, _left: Duration, limit: usize| {
            held.read(limit, Fetched::Body(url.as_bytes().to_vec()))
        };
        let keep = |_: &str, read: crate::Result<Pulled>| match read {
            Ok(Pulled::Fetched(Fetched::Body(body))) => Some(body),
            _ => None,
        };
        let counted = held.count(SERVERS, || {
            reads_within(
                ENVELOPES,
                &urls,
                aterm_spec::wait::MUST_HAPPEN * 2,
                read,
                keep,
            )
        });
        assert_eq!(
            counted.seen.in_flight, ENVELOPE_PULLS_AT_ONCE,
            "whole reads in flight at once"
        );
        assert_eq!(
            counted.seen.waiting,
            SERVERS - ENVELOPE_PULLS_AT_ONCE,
            "whole reads waiting for a slot"
        );
        assert_eq!(counted.most, ENVELOPE_PULLS_AT_ONCE);
        assert_eq!(
            held.peeks.load(Ordering::SeqCst),
            SERVERS,
            "every answer was peeked once"
        );
        let expected: Vec<Option<Option<Vec<u8>>>> = urls
            .iter()
            .map(|url| Some(Some(url.as_bytes().to_vec())))
            .collect();
        assert_eq!(
            counted.result, expected,
            "every server's envelope read whole, in its place"
        );
    }

    /// A loopback server that answers `times` requests, each with `body` in a `200`, and hands
    /// back the request heads it read.
    fn serving(body: &[u8], times: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
        let mut answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        answer.extend_from_slice(body);
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut heads = Vec::new();
            for _ in 0..times {
                let (mut stream, _) = listener.accept().expect("accept");
                stream
                    .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                    .expect("timeout");
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).expect("read") == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                heads.push(String::from_utf8_lossy(&head).into_owned());
                // A read under the peek hangs up once it sees the length: the rest goes nowhere.
                let _ = stream.write_all(&answer);
            }
            heads
        });
        (url, server)
    }

    /// An envelope longer than a first read takes ([`ENVELOPE_PEEK`]) is asked for again and read
    /// whole, and one within it is read once: a vault that outgrows the peek still opens from
    /// every server that serves it, through the read an admin command's open and a mirror's
    /// heads make ([`bodies_within`]).
    ///
    /// NEGATIVE CONTROL: hold the second read to [`ENVELOPE_PEEK`], as the first is
    /// (`read_by(url, reads.peek)` in `reads_within`'s slotted `Ok(Pulled::Larger)` arm): it is
    /// [`Pulled::Larger`] too, and the long body is lost: `left: [Some(64), None]`, "every body
    /// kept whole".
    #[test]
    fn an_envelope_longer_than_a_first_read_is_asked_again_and_read_whole() {
        let short = vec![7u8; 64];
        let long = vec![9u8; ENVELOPE_PEEK + 1];
        let (short_url, short_server) = serving(&short, 1);
        let (long_url, long_server) = serving(&long, 2);
        let kept = bodies_within(
            &[short_url, long_url],
            aterm_spec::wait::MUST_HAPPEN,
            Some("age1reader"),
            Some,
        );
        let lengths: Vec<Option<usize>> = kept
            .iter()
            .map(|body| body.as_ref().map(Vec::len))
            .collect();
        assert_eq!(
            lengths,
            [Some(short.len()), Some(long.len())],
            "every body kept whole"
        );
        assert!(kept == [Some(short), Some(long)], "each body as served");
        for (server, asked) in [(short_server, 1), (long_server, 2)] {
            let heads = server.join().expect("the server");
            assert_eq!(heads.len(), asked);
            for head in heads {
                assert!(head.starts_with("GET /v1/alab "), "{head}");
                assert!(
                    head.to_ascii_lowercase().contains("age1reader"),
                    "each read names its reader: {head}"
                );
            }
        }
    }
}
