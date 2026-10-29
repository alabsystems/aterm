// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::low_data_mode`] on the live system: whether the network this Mac uses now
//! is in Low Data Mode, as Network.framework's path monitor reads it (`nw_path_is_constrained`
//! of the path its first update hands over). The monitor is aterm-objc's
//! [`aterm_objc::PathMonitor`], which opens the framework at its first start (`dlopen`), so a
//! run that never asks loads nothing more.

use std::sync::mpsc;
use std::time::Duration;

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
