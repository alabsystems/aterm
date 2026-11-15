// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One owned, bounded discovery worker; network work never occupies the authorization watcher.

use aterm_uds::stop::Stop;
use std::{sync::Arc, thread::JoinHandle, time::Instant};

#[cfg(not(test))]
pub(super) const WITHIN: std::time::Duration = std::time::Duration::from_secs(3);
#[cfg(test)]
pub(super) const WITHIN: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

pub(super) struct RouteRefresh {
    next: Instant,
    worker: Option<JoinHandle<()>>,
    stopped: Arc<Stop>,
}

impl RouteRefresh {
    pub(super) fn new(stopped: Arc<Stop>) -> Self {
        Self {
            next: Instant::now(),
            worker: None,
            stopped,
        }
    }

    pub(super) fn due(&mut self, now: Instant) -> bool {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
        self.worker.is_none() && now >= self.next
    }

    pub(super) fn start(&mut self, now: Instant, work: impl FnOnce() + Send + 'static) -> bool {
        if !self.due(now) {
            return false;
        }
        self.next = now + aterm_agent::node_service::DISCOVERY_PERIOD;
        let started = std::thread::Builder::new()
            .name("aterm-fleet-routes".into())
            .spawn(move || {
                if !cfg!(test) {
                    crate::qos::set_self(crate::qos::Role::Background);
                }
                work();
            });
        match started {
            Ok(worker) => {
                self.worker = Some(worker);
                true
            }
            Err(error) => {
                aterm_log::warn!("node route discovery could not start: {error}");
                false
            }
        }
    }
}

#[cfg(test)]
#[path = "route_refresh_tests.rs"]
mod tests;

impl Drop for RouteRefresh {
    fn drop(&mut self) {
        self.stopped.set();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
