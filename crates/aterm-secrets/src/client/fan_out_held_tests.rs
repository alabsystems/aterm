// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    pub(in crate::client) fn read<A>(&self, limit: usize, answer: A) -> crate::Result<Pulled<A>> {
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
