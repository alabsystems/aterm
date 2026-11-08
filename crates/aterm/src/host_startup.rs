// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A constructor retained by its process host until it publishes its owner. The constructor
//! keeps its own admission policy; the existing host loop supplies every retry deadline.

use aterm_secrets::converge::{Last, RunnerRetry};
use std::time::Instant;

pub(crate) struct Constructor {
    start: Option<Box<dyn FnMut() -> bool + Send>>,
    clock: Instant,
    due: Option<Instant>,
    retry: RunnerRetry,
}

impl Constructor {
    pub(crate) fn new(start: impl FnMut() -> bool + Send + 'static) -> Self {
        Self {
            start: Some(Box::new(start)),
            clock: Instant::now(),
            due: None,
            retry: RunnerRetry::default(),
        }
    }

    pub(crate) fn pending(&self) -> bool {
        self.start.is_some()
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.due
    }

    pub(crate) fn maintain(&mut self) -> Option<Instant> {
        if !self.pending() {
            return None;
        }
        self.maintain_at(Instant::now())
    }

    pub(crate) fn maintain_at(&mut self, now: Instant) -> Option<Instant> {
        let start = self.start.as_mut()?;
        if self.due.is_some_and(|due| now < due) {
            return self.due;
        }
        self.retry.started();
        if start() {
            self.start = None;
            self.due = None;
            return None;
        }
        let elapsed = now.saturating_duration_since(self.clock).as_millis();
        let elapsed = u64::try_from(elapsed).unwrap_or(u64::MAX);
        self.retry.reaped(Last::Failed, elapsed);
        self.due = self.retry.remaining(elapsed).map(|pause| now + pause);
        self.due
    }
}
