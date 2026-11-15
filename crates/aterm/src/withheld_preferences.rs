// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain deferred-preference intent on the window's existing input keeper.

use aterm_secrets::converge::{Last, RunnerRetry};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// How one finite follow-up attempt ended, for its keeper ([`Coordinator`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Attempted {
    pub(super) last: Last,
    /// The attempt left only changes that wait out their own pause
    /// (`aterm_cli::macos_doctor::FollowedUp::put_off_for`): the keeper keeps the request and
    /// starts no attempt before this long after it, when the first of them comes due.
    pub(super) put_off_for: Option<Duration>,
}

impl Attempted {
    pub(super) const fn plain(last: Last) -> Self {
        Self {
            last,
            put_off_for: None,
        }
    }
}

struct Requests(AtomicU64);

impl Requests {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    fn request(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn current(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

static REQUESTS: Requests = Requests::new();

pub(super) fn request() {
    REQUESTS.request();
}

/// What a [`Coordinator`] next does, and when ([`Coordinator::owed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owed {
    /// No follow-up is wanted.
    Idle,
    /// An attempt runs. Nothing tells its end: a round finds it ended.
    Running,
    /// A wanted follow-up starts no sooner than this long from now: at once for a request no
    /// round has seen, else after its put-off and its retry pause.
    In(Duration),
}

#[derive(Default)]
pub(super) struct Coordinator {
    seen: u64,
    wanted: Option<u64>,
    worker: Option<(u64, JoinHandle<Attempted>)>,
    retry: RunnerRetry,
    /// No attempt before this, on the keeper's clock: the last left only changes that wait out
    /// their own pause ([`Attempted::put_off_for`]). A new request lifts it.
    put_off_until: Option<u64>,
}

impl Coordinator {
    /// A coordinator that has seen every request made so far, and wants nothing.
    #[cfg(test)]
    pub(super) fn caught_up() -> Self {
        Self {
            seen: REQUESTS.current(),
            ..Self::default()
        }
    }

    pub(super) fn round(&mut self, now: u64) -> Result<(), String> {
        self.round_with(REQUESTS.current(), now, || {
            std::thread::Builder::new()
                .name("aterm-setup-follow-up".into())
                .spawn(super::finish_withheld_preferences)
        })
    }

    /// What the next [`Self::round`] is owed for, with `now` on the keeper's clock.
    pub(super) fn owed(&self, now: u64) -> Owed {
        self.owed_with(REQUESTS.current(), now)
    }

    fn owed_with(&self, requests: u64, now: u64) -> Owed {
        if self.worker.is_some() {
            return Owed::Running;
        }
        if self.seen != requests {
            return Owed::In(Duration::ZERO);
        }
        if self.wanted.is_none() {
            return Owed::Idle;
        }
        let put_off = self.put_off_until.map_or(Duration::ZERO, |until| {
            Duration::from_millis(until.saturating_sub(now))
        });
        Owed::In(put_off.max(self.retry.remaining(now).unwrap_or_default()))
    }

    fn round_with(
        &mut self,
        requests: u64,
        now: u64,
        start: impl FnOnce() -> std::io::Result<JoinHandle<Attempted>>,
    ) -> Result<(), String> {
        if self.seen != requests {
            self.seen = requests;
            self.wanted = Some(requests);
            self.put_off_until = None;
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|(_, worker)| worker.is_finished())
        {
            let (request, worker) = self.worker.take().expect("finished follow-up worker");
            let attempted = worker.join().unwrap_or(Attempted::plain(Last::Failed));
            self.retry.reaped(attempted.last, now);
            self.put_off_until = attempted.put_off_for.map(|pause| {
                now.saturating_add(u64::try_from(pause.as_millis()).unwrap_or(u64::MAX))
            });
            if attempted.last == Last::Ended && self.wanted == Some(request) {
                self.wanted = None;
            }
        }
        if self.worker.is_some()
            || self.wanted.is_none()
            || self.put_off_until.is_some_and(|until| now < until)
            || self
                .retry
                .remaining(now)
                .is_some_and(|pause| !pause.is_zero())
        {
            return Ok(());
        }
        let request = self.wanted.expect("wanted follow-up");
        self.retry.started();
        match start() {
            Ok(worker) => self.worker = Some((request, worker)),
            Err(error) => {
                // The request remains wanted. A failed start spent no effect attempt.
                self.retry.reaped(Last::Failed, now);
                return Err(error.to_string());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "withheld_preferences_tests.rs"]
mod tests;
