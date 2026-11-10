// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The own-thread D-state census's worker (stats deck A4 §6.5, M-DSTATE): on macOS, which of
//! this user's processes hold a thread in an uninterruptible wait, read by
//! `aterm_sysprobe::dstate` on a thread of its own at utility QoS, never the probe thread.
//!
//! A whole reading asks for a pass once [`CENSUS_PERIOD_MS`] has passed since its last ask, and
//! takes the pass the worker finished since the reading before; the model keeps the newest
//! ([`aterm_vitals::blockage`]). The worker waits on its channel between asks, so it wakes nothing
//! at rest, and starts at the first ask: no whole reading, no thread. A pass costs at most 5 ms
//! of the worker's thread time (`aterm_sysprobe::dstate::PASS_BUDGET_NS`).

use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_vitals::blockage::{CENSUS_PERIOD_MS, CensusPass};

/// The finished pass a reading has not taken yet.
type Done = Arc<Mutex<Option<Arc<CensusPass>>>>;

/// The census's worker, as the probe thread holds it.
pub(crate) struct CensusLane {
    /// Whether this reader runs a census: macOS, outside unit tests, until its worker is lost.
    live: bool,
    ask: Option<SyncSender<()>>,
    done: Done,
    asked_at: Option<Instant>,
}

impl CensusLane {
    /// A lane that runs a census where `live` (its worker starts at the first ask), else none.
    pub(crate) fn new(live: bool) -> Self {
        Self {
            live,
            ask: None,
            done: Arc::new(Mutex::new(None)),
            asked_at: None,
        }
    }

    /// At a whole reading at `now`: the pass the worker finished since the last reading, where
    /// one did, and a new ask once [`CENSUS_PERIOD_MS`] has passed since the last. A pass still
    /// running when one is due is asked for again at the next reading. `epoch` is the reader's
    /// zero, the clock of [`aterm_vitals::Sample::mono_ms`] the pass is stamped on.
    pub(crate) fn step(&mut self, now: Instant, epoch: Instant) -> Option<Arc<CensusPass>> {
        if !self.live {
            return None;
        }
        let done = self
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let period = Duration::from_millis(CENSUS_PERIOD_MS);
        if self
            .asked_at
            .is_none_or(|at| now.saturating_duration_since(at) >= period)
        {
            if self.ask.is_none() {
                self.ask = spawn(epoch, Arc::clone(&self.done));
            }
            match self.ask.as_ref().map(|tx| tx.try_send(())) {
                Some(Ok(())) => self.asked_at = Some(now),
                Some(Err(TrySendError::Full(()))) => {}
                Some(Err(TrySendError::Disconnected(()))) | None => {
                    aterm_log::warn!(
                        "system vitals: the D-state census's worker is gone; uninterruptible \
                         waits read not_sampled from here"
                    );
                    self.live = false;
                    self.ask = None;
                }
            }
        }
        done
    }
}

/// Starts the worker: it runs a pass at each ask and leaves it in `done`; it ends when the lane
/// drops its sender.
#[cfg(target_os = "macos")]
fn spawn(epoch: Instant, done: Done) -> Option<SyncSender<()>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-vitals-census",
        crate::qos::Role::Background,
        move || {
            let mut census =
                aterm_sysprobe::dstate::Census::new(aterm_sysprobe::dstate::Kernel::new());
            while rx.recv().is_ok() {
                let pass = of_pass(census.pass(), epoch);
                *done.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(pass));
            }
        },
    );
    match spawned {
        Ok(_) => Some(tx),
        Err(e) => {
            aterm_log::warn!("system vitals: the D-state census's worker did not start: {e}");
            None
        }
    }
}

/// No census off macOS: Linux's sweep reads every process's state, and Windows has no reader.
#[cfg(not(target_os = "macos"))]
fn spawn(_epoch: Instant, _done: Done) -> Option<SyncSender<()>> {
    None
}

/// The model's copy of a pass, stamped with when it ended on the reader's clock.
#[cfg(target_os = "macos")]
fn of_pass(p: aterm_sysprobe::dstate::Pass, epoch: Instant) -> CensusPass {
    CensusPass {
        at_mono_ms: u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX),
        marked: p
            .marked
            .into_iter()
            .map(|m| aterm_vitals::blockage::CensusMark {
                pid: m.pid,
                name: m.name,
                uninterruptible: m.uninterruptible,
                held: m.again,
            })
            .collect(),
        listed: p.listed,
        read: p.read,
        refused: p.refused,
        cover_done: p.cover_done,
        passes_in_cover: p.passes_in_cover,
        thread_ns: p.thread_ns,
        list_refused: p.list_refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lane_that_runs_no_census_asks_nothing_and_starts_no_worker() {
        let mut lane = CensusLane::new(false);
        let now = Instant::now();
        assert_eq!(lane.step(now, now), None);
        assert!(lane.ask.is_none() && lane.asked_at.is_none());
    }

    #[test]
    fn a_pass_is_taken_once_and_an_ask_waits_out_its_period() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
        let t0 = Instant::now();
        let mut lane = CensusLane {
            live: true,
            ask: Some(tx),
            done: Arc::new(Mutex::new(None)),
            asked_at: None,
        };
        assert_eq!(lane.step(t0, t0), None);
        assert!(rx.try_recv().is_ok(), "the first reading asks");
        let pass = Arc::new(CensusPass {
            listed: 7,
            ..CensusPass::default()
        });
        *lane.done.lock().expect("unpoisoned") = Some(Arc::clone(&pass));
        let later = t0 + Duration::from_secs(1);
        assert_eq!(lane.step(later, t0), Some(pass));
        assert!(rx.try_recv().is_err(), "within the period, no ask");
        assert_eq!(lane.step(later, t0), None, "taken once");
        let due = t0 + Duration::from_millis(CENSUS_PERIOD_MS);
        let _ = lane.step(due, t0);
        assert!(rx.try_recv().is_ok(), "asked again at the period");
        // A worker gone ends the census rather than asking forever.
        drop(rx);
        let _ = lane.step(due + Duration::from_millis(CENSUS_PERIOD_MS), t0);
        assert!(!lane.live && lane.ask.is_none());
    }
}
