// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU client lane (`gpu_clients`, design L5 in `docs/DESIGN-system-awareness-2026-10-16.md`):
//! each process's cumulative GPU time from the accelerator's user clients, read on the split
//! cadence ([`split_due`]) and on every pull, never by a hidden deck's reading.

use std::time::Instant;

use aterm_vitals::GpuClientRow;

use crate::vitals_host::{Request, SPLIT_COLD_MARGIN, SPLIT_WINDOW_MAX, split_due};

/// The lane's self-check: at least one client whose creator and `AppUsage` parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuCheck {
    /// Not read yet.
    Unchecked,
    /// The first read parsed no client (perhaps none existed yet): one more read decides.
    Recheck,
    /// A read parsed a client.
    On,
    /// Two reads parsed none: the keys are not this OS's. Never read again.
    Off,
    /// No accelerator answered where that lasts (Linux, or a Mac without one): never read again,
    /// and nothing to warn about.
    Unsupported,
    /// The reader answered nothing at its last read where that can pass ([`GpuLane::passing`]:
    /// Windows, whose reader answers nothing for a collection that failed, while a driver
    /// restarts, or while DirectX's adapter rows still name the adapters' previous ids after a
    /// boot): read again on the split cadence, but no pull reads twice for it, so a machine whose
    /// GPU never answers (one with only the software rasteriser) slows no pull.
    Unanswered,
}

/// The lane's reader and what it keeps between reads.
pub(crate) struct GpuLane {
    read: fn() -> Option<aterm_sysprobe::GpuClients>,
    /// Whether a read that answers nothing can pass (Windows), so it leaves the lane reading
    /// ([`GpuCheck::Unanswered`]); where it cannot (macOS: no accelerator entry), it marks the
    /// lane [`GpuCheck::Unsupported`].
    pub(crate) passing: bool,
    pub(crate) check: GpuCheck,
    pub(crate) last: Option<Instant>,
}

impl GpuLane {
    /// [`aterm_sysprobe::gpu_clients`] in production.
    pub(crate) const KERNEL: fn() -> Option<aterm_sysprobe::GpuClients> =
        aterm_sysprobe::gpu_clients;
    /// The inert arm unit tests run: reads nothing.
    pub(crate) const INERT: fn() -> Option<aterm_sysprobe::GpuClients> = || None;

    pub(crate) const fn new(read: fn() -> Option<aterm_sysprobe::GpuClients>) -> Self {
        Self {
            read,
            passing: false,
            check: GpuCheck::Unchecked,
            last: None,
        }
    }

    /// The lane the host's reader starts with: [`Self::KERNEL`] on macOS and on Windows (whose
    /// clients are the `GPU Engine` counters' instances, and where a read that answers nothing
    /// can pass: [`Self::passing`]). On Linux no accelerator has user clients to read, and a unit
    /// test's reader has the inert arm, so there it starts unsupported: never read, never cold.
    pub(crate) const fn host() -> Self {
        let mut lane = Self::new(if cfg!(test) {
            Self::INERT
        } else {
            Self::KERNEL
        });
        lane.passing = cfg!(windows);
        if cfg!(test) || !cfg!(any(target_os = "macos", windows)) {
            lane.check = GpuCheck::Unsupported;
        }
        lane
    }

    /// Whether a whole reading `req` at `now` reads the clients: on the bars' split ticks and on
    /// every pull at least [`crate::vitals_host::MIN_WINDOW`] after the last read.
    pub(crate) fn due(&self, req: &Request, now: Instant) -> bool {
        !matches!(self.check, GpuCheck::Off | GpuCheck::Unsupported)
            && split_due(self.last, req, true, now)
    }

    /// Whether a pull should read twice so its client window is fresh: the lane may still read and
    /// has not read yet, or its last read is near [`SPLIT_WINDOW_MAX`] old, past which the model
    /// drops the shares. One read alone is only a baseline, so without the pair a first pull would
    /// say the lane is off. A lane whose last read went unanswered is never cold.
    pub(crate) fn cold(&self, now: Instant) -> bool {
        !matches!(
            self.check,
            GpuCheck::Off | GpuCheck::Unsupported | GpuCheck::Unanswered
        ) && self
            .last
            .is_none_or(|t| now.saturating_duration_since(t) + SPLIT_COLD_MARGIN > SPLIT_WINDOW_MAX)
    }

    /// The clients and whether the read saw them all, when the reading is due and a read parses
    /// one.
    pub(crate) fn read(
        &mut self,
        req: &Request,
        now: Instant,
    ) -> Option<(Vec<GpuClientRow>, Vec<u64>, bool)> {
        if !self.due(req, now) {
            return None;
        }
        self.last = Some(now);
        let Some(read) = (self.read)() else {
            self.check = match self.check {
                GpuCheck::On => GpuCheck::On,
                _ if self.passing => GpuCheck::Unanswered,
                _ => GpuCheck::Unsupported,
            };
            return None;
        };
        let (rows, entries, complete) = (read.rows, read.entries, read.complete);
        if rows.is_empty() {
            self.check = match self.check {
                GpuCheck::Unchecked | GpuCheck::Unanswered => GpuCheck::Recheck,
                GpuCheck::Recheck => {
                    aterm_log::warn!(
                        "system vitals: the gpu_clients lane is off: no GPU user client's \
                         creator and AppUsage read, twice"
                    );
                    GpuCheck::Off
                }
                held => held,
            };
            return None;
        }
        self.check = GpuCheck::On;
        let rows = rows
            .into_iter()
            .map(|c| GpuClientRow {
                entry: c.entry,
                pid: c.pid,
                name: c.name,
                gpu_ns: c.gpu_ns,
            })
            .collect();
        Some((rows, entries, complete))
    }
}

#[cfg(test)]
#[path = "vitals_gpu_tests.rs"]
mod tests;
