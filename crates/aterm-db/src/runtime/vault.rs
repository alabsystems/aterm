// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A selected writer's private vault home has no login subscriber of its own.
//! Refresh it under the transport owner without blocking that owner's LAN ticks.

use super::{Stop, client, custody, enabled, owner};
use crate::{Result, generation};
use aterm_secrets::client::Paths;
#[cfg(test)]
use std::sync::{Condvar, Mutex};
use std::{
    path::Path,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_secs(30);
const WITHIN: Duration = Duration::from_secs(20);

/// When a reader's next pass is due.
#[derive(Clone, Default)]
pub(super) enum Pace {
    /// [`INTERVAL`] after the reader started or its last pass ended.
    #[default]
    Interval,
    /// Each time a test asks for one ([`Passes::pass`]), and never on a timer.
    #[cfg(test)]
    Asked(Arc<Passes>),
}

impl Pace {
    /// Wait until a pass is due: `false` once `stop` was set instead.
    fn due(&self, stop: &Stop) -> bool {
        match self {
            Self::Interval => {
                stop.sleep(INTERVAL);
            }
            #[cfg(test)]
            Self::Asked(passes) => passes.take(stop),
        }
        !stop.is_set()
    }

    /// The pass [`Self::due`] gave ended, whether it read the vault or found it may not.
    fn passed(&self) {
        #[cfg(test)]
        if let Self::Asked(passes) = self {
            passes.passed();
        }
    }

    /// End a [`Self::due`] under way: its `stop` was set.
    fn stopped(&self) {
        #[cfg(test)]
        if let Self::Asked(passes) = self {
            passes.wake();
        }
    }
}

/// The passes a test asks of a runtime's vault readers ([`Pace::Asked`]). One reader runs at a
/// time; an ask a stopped reader left unanswered falls to the next one.
#[cfg(test)]
#[derive(Default)]
pub struct Passes {
    counts: Mutex<Counts>,
    changed: Condvar,
}

#[cfg(test)]
#[derive(Default)]
struct Counts {
    /// Passes asked for.
    asked: u64,
    /// The asks the pass under way answers.
    taken: u64,
    /// The asks a pass that ended answered.
    done: u64,
}

#[cfg(test)]
impl Passes {
    /// Ask for a pass and wait until one that began after this ask ended: `false` if none did
    /// `within`.
    pub fn pass(&self, within: Duration) -> bool {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        counts.asked += 1;
        let asked = counts.asked;
        self.changed.notify_all();
        !self
            .changed
            .wait_timeout_while(counts, within, |counts| counts.done < asked)
            .unwrap_or_else(|p| p.into_inner())
            .1
            .timed_out()
    }

    /// Wait until a pass is asked for or `stop` is set; the pass then due answers every
    /// ask so far.
    fn take(&self, stop: &Stop) {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        while counts.done >= counts.asked && !stop.is_set() {
            counts = self.changed.wait(counts).unwrap_or_else(|p| p.into_inner());
        }
        counts.taken = counts.asked;
    }

    fn passed(&self) {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        counts.done = counts.taken;
        self.changed.notify_all();
    }

    /// Under the lock, so a [`Self::take`] that has not seen the stop is already waiting.
    fn wake(&self) {
        let _counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        self.changed.notify_all();
    }
}

pub(super) struct Reader {
    stop: Arc<Stop>,
    pace: Pace,
    task: Option<JoinHandle<()>>,
}

impl Reader {
    pub(super) fn start(
        paths: &Paths,
        config: &Path,
        lease: &Arc<owner::Lease>,
        custody: &Arc<custody::Custody>,
        pace: Pace,
    ) -> Result<Option<Self>> {
        if !generation::is_candidate(paths) {
            return Ok(None);
        }
        let logical = generation::logical_paths(paths)?;
        let paths = paths.clone();
        let config = config.to_owned();
        let lease = lease.clone();
        let custody = custody.clone();
        let stop = Arc::new(Stop::default());
        let worker = stop.clone();
        let paced = pace.clone();
        let task = thread::Builder::new()
            .name("commons-vault-reader".into())
            .spawn(move || {
                while paced.due(&worker) {
                    if enabled(&config).unwrap_or(false)
                        && !aterm_secrets::service::subscriber_turned_off(&logical.home)
                        && lease.recheck().is_ok()
                        && custody.recheck().is_ok()
                    {
                        let deadline = Instant::now() + WITHIN;
                        if generation::guard_until(&paths, false, deadline).is_ok() {
                            if worker.is_set() {
                                break;
                            }
                            // This member-only pass shares one bound across discovery,
                            // requests and materialization. It neither joins nor mirrors
                            // a vault; failed reads remain retryable on the next pass.
                            let _ = client::sync_reader_until(&paths, deadline);
                        }
                    }
                    paced.passed();
                }
            })?;
        Ok(Some(Self {
            stop,
            pace,
            task: Some(task),
        }))
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.set();
        self.pace.stopped();
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
