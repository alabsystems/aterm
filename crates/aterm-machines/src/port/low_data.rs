// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::low_data_mode`] on the live system: whether the network this Mac uses now
//! is in Low Data Mode, as Network.framework's path monitor reads it (`nw_path_is_constrained`
//! of the path its first update hands over). The monitor is aterm-objc's
//! [`aterm_objc::PathMonitor`], which opens the framework at its first start (`dlopen`), so a
//! run that never asks loads nothing more.

use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use std::time::Instant;

use aterm_objc::{PathMonitor, PathReading, PathStatus};

/// How long the look waits for the monitor's first path, which it hands over at once on a Mac
/// with a network.
pub(super) const PATIENCE: Duration = Duration::from_secs(2);

/// Whether the current network path is constrained (Low Data Mode), waiting at most
/// `patience` for the monitor's first update; `None` when no path that carries traffic was
/// read.
pub(super) fn read(patience: Duration) -> Option<bool> {
    let (tx, rx) = mpsc::sync_channel::<PathReading>(1);
    let monitor = PathMonitor::start(c"aterm.machines.low-data", move |reading| {
        let _ = tx.try_send(reading);
    })?;
    let first = rx.recv_timeout(patience).ok();
    // Cancels and releases the monitor, once, before the reading is judged.
    drop(monitor);
    first
        .filter(|reading| reading.status == PathStatus::Satisfied)
        .map(|reading| reading.constrained)
}

/// One observer remains alive through network outages and Low Data Mode.
pub(super) struct Watch {
    _monitor: PathMonitor,
    state: Arc<Observations>,
}

pub(super) fn watch() -> Option<Watch> {
    let state = Arc::new(Observations::default());
    let updates = Arc::clone(&state);
    let monitor = PathMonitor::start(c"aterm.machines.install-network", move |reading| {
        updates.publish(reading);
    })?;
    Some(Watch {
        _monitor: monitor,
        state,
    })
}

impl super::InstallNetworkWatch for Watch {
    fn reading(&self) -> Option<super::InstallNetworkReading> {
        self.state.reading()
    }

    fn wait(&mut self, revision: Option<u64>, budget: Duration) -> Duration {
        self.state.wait(revision, budget)
    }
}

struct ObservedPath {
    revision: u64,
    path: PathReading,
}

#[derive(Default)]
struct Observations {
    latest: Mutex<Option<ObservedPath>>,
    changed: Condvar,
}

impl Observations {
    fn publish(&self, path: PathReading) {
        if let Ok(mut latest) = self.latest.lock()
            && latest.as_ref().is_none_or(|previous| previous.path != path)
        {
            let revision = latest
                .as_ref()
                .map_or(1, |previous| previous.revision.saturating_add(1));
            *latest = Some(ObservedPath { revision, path });
            self.changed.notify_all();
        }
    }

    fn reading(&self) -> Option<super::InstallNetworkReading> {
        self.latest
            .lock()
            .ok()?
            .as_ref()
            .map(|observed| super::InstallNetworkReading {
                revision: observed.revision,
                available: observed.path.status == PathStatus::Satisfied,
                constrained: observed.path.constrained,
            })
    }

    fn wait(&self, revision: Option<u64>, budget: Duration) -> Duration {
        let started = Instant::now();
        match self.latest.lock() {
            Ok(latest) => {
                let _waited = self.changed.wait_timeout_while(latest, budget, |latest| {
                    latest.as_ref().map(|observed| observed.revision) == revision
                });
            }
            Err(_) => std::thread::sleep(budget),
        }
        started.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usable() -> PathReading {
        PathReading {
            status: PathStatus::Satisfied,
            constrained: false,
            expensive: false,
            link: aterm_objc::path_monitor::PathLink::Wifi,
        }
    }

    #[test]
    fn a_return_to_the_same_usable_path_preserves_the_recovery_edge() {
        let observations = Observations::default();
        assert_eq!(observations.reading(), None);
        observations.publish(usable());
        let first = observations.reading().unwrap();
        observations.publish(usable());
        assert_eq!(observations.reading(), Some(first));
        observations.publish(PathReading {
            status: PathStatus::Unsatisfied,
            ..usable()
        });
        assert!(!observations.reading().unwrap().available);
        observations.publish(usable());
        let recovered = observations.reading().unwrap();
        assert!(recovered.available);
        assert!(recovered.revision > first.revision);
    }

    #[test]
    fn low_data_and_interface_changes_publish_new_retry_evidence() {
        let observations = Observations::default();
        observations.publish(usable());
        let initial = observations.reading().unwrap();
        observations.publish(PathReading {
            constrained: true,
            ..usable()
        });
        let held = observations.reading().unwrap();
        assert!(held.constrained && held.revision > initial.revision);
        observations.publish(PathReading {
            link: aterm_objc::path_monitor::PathLink::Wired,
            ..usable()
        });
        let changed = observations.reading().unwrap();
        assert!(changed.available && !changed.constrained && changed.revision > held.revision);
    }
}
