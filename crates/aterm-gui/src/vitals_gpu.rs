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
    /// No accelerator answered (every OS but macOS, or a Mac without one): never read again, and
    /// nothing to warn about.
    Unsupported,
}

/// The lane's reader and what it keeps between reads.
pub(crate) struct GpuLane {
    read: fn() -> Option<aterm_sysprobe::GpuClients>,
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
            check: GpuCheck::Unchecked,
            last: None,
        }
    }

    /// The lane the host's reader starts with: [`Self::KERNEL`] on macOS. Off macOS no
    /// accelerator has user clients to read, and a unit test's reader has the inert arm, so there
    /// it starts unsupported: never read, never cold.
    pub(crate) const fn host() -> Self {
        let mut lane = Self::new(if cfg!(test) {
            Self::INERT
        } else {
            Self::KERNEL
        });
        if cfg!(test) || !cfg!(target_os = "macos") {
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
    /// say the lane is off.
    pub(crate) fn cold(&self, now: Instant) -> bool {
        !matches!(self.check, GpuCheck::Off | GpuCheck::Unsupported)
            && self.last.is_none_or(|t| {
                now.saturating_duration_since(t) + SPLIT_COLD_MARGIN > SPLIT_WINDOW_MAX
            })
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
            if self.check != GpuCheck::On {
                self.check = GpuCheck::Unsupported;
            }
            return None;
        };
        let (rows, entries, complete) = (read.rows, read.entries, read.complete);
        if rows.is_empty() {
            self.check = match self.check {
                GpuCheck::Unchecked => GpuCheck::Recheck,
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
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::vitals_host::{MIN_WINDOW, Reader};

    thread_local! {
        static READS: Cell<u32> = const { Cell::new(0) };
        static CLIENTS: Cell<u32> = const { Cell::new(1) };
        static NAME: RefCell<String> = RefCell::new("stand-in".into());
        static COMPLETE: Cell<bool> = const { Cell::new(true) };
    }

    /// A counting stand-in: [`CLIENTS`] clients of this process named [`NAME`], each with 1 ms of
    /// GPU time per read so far.
    fn counting() -> Option<aterm_sysprobe::GpuClients> {
        let n = READS.with(|r| {
            r.set(r.get() + 1);
            r.get()
        });
        let rows = (0..CLIENTS.with(Cell::get))
            .map(|i| aterm_sysprobe::GpuClient {
                entry: u64::from(i) + 1,
                pid: std::process::id(),
                name: NAME.with(|n| n.borrow().clone()),
                gpu_ns: u64::from(n) * 1_000_000,
            })
            .collect();
        Some(aterm_sysprobe::GpuClients {
            rows,
            entries: (0..CLIENTS.with(Cell::get))
                .map(|i| u64::from(i) + 1)
                .collect(),
            children: CLIENTS.with(Cell::get),
            truncated: false,
            complete: COMPLETE.with(Cell::get),
        })
    }

    fn reads() -> u32 {
        READS.with(Cell::get)
    }

    fn bars() -> Request {
        Request {
            self_pid: std::process::id(),
            cadence_ms: 1_000,
            deck: true,
            ..Request::default()
        }
    }

    #[test]
    fn the_clients_are_read_on_split_ticks_and_pulls_only() {
        let t0 = Instant::now();
        let mut lane = GpuLane::new(counting);
        let bars = bars();
        let hidden = Request {
            machine_only: true,
            ..bars.clone()
        };
        let engine = Request {
            jobs_now: true,
            ..bars.clone()
        };
        let pull = Request {
            pull: true,
            deck: false,
            ..bars.clone()
        };
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(
            lane.cold(at(0)),
            "a lane that never read: a first pull reads twice"
        );
        assert!(lane.read(&bars, at(0)).is_some(), "the first tick");
        assert_eq!(lane.check, GpuCheck::On);
        assert!(lane.read(&bars, at(1)).is_none(), "inside the split period");
        assert!(lane.read(&hidden, at(10)).is_none(), "a hidden deck's");
        assert!(lane.read(&engine, at(10)).is_none(), "the engine's");
        assert_eq!(reads(), 1);
        assert!(lane.read(&bars, at(10)).is_some(), "the next tick");
        assert!(lane.read(&pull, at(10) + MIN_WINDOW).is_some(), "a pull");
        assert!(
            lane.read(&pull, at(10) + MIN_WINDOW).is_none(),
            "not twice at once"
        );
        assert_eq!(reads(), 3);
        assert!(!lane.cold(at(11)));
        assert!(
            lane.cold(at(10) + SPLIT_WINDOW_MAX),
            "near the model's window"
        );
    }

    #[test]
    fn the_lane_turns_off_after_two_reads_with_no_client() {
        CLIENTS.with(|c| c.set(0));
        let t0 = Instant::now();
        let mut lane = GpuLane::new(counting);
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(lane.read(&bars(), at(0)).is_none());
        assert_eq!(lane.check, GpuCheck::Recheck, "one re-check owed");
        CLIENTS.with(|c| c.set(1));
        assert!(lane.read(&bars(), at(5)).is_some(), "a client appeared");
        assert_eq!(lane.check, GpuCheck::On);

        CLIENTS.with(|c| c.set(0));
        let mut lane = GpuLane::new(counting);
        assert!(lane.read(&bars(), at(0)).is_none());
        assert!(lane.read(&bars(), at(5)).is_none());
        assert_eq!(lane.check, GpuCheck::Off);
        let before = reads();
        assert!(lane.read(&bars(), at(10)).is_none());
        assert!(!lane.due(&bars(), at(10)) && !lane.cold(at(10)));
        assert_eq!(reads(), before, "an off lane reads nothing");
    }

    /// A reader that answers nothing (no accelerator: every OS but macOS) turns the lane off at
    /// once, as unsupported rather than as keys gone missing, and is never read again.
    #[test]
    fn no_accelerator_turns_the_lane_off_as_unsupported() {
        let mut lane = GpuLane::new(GpuLane::INERT);
        let t0 = Instant::now();
        assert!(lane.read(&bars(), t0).is_none());
        assert_eq!(lane.check, GpuCheck::Unsupported);
        let later = t0 + Duration::from_secs(10);
        assert!(!lane.due(&bars(), later) && !lane.cold(later));
    }

    /// The read's completeness reaches the sample the model takes.
    #[test]
    fn a_read_says_whether_it_saw_every_client() {
        fn with_unread_child() -> Option<aterm_sysprobe::GpuClients> {
            counting().map(|mut read| {
                read.entries.push(99);
                read
            })
        }
        let t0 = Instant::now();
        let mut lane = GpuLane::new(with_unread_child);
        let (rows, entries, complete) = lane.read(&bars(), t0).expect("a reading");
        assert_eq!(rows.len(), 1);
        assert_eq!(entries, [1, 99], "keep children without counter rows");
        assert!(complete);
        COMPLETE.with(|c| c.set(false));
        let next = t0 + Duration::from_secs(5);
        assert_eq!(lane.read(&bars(), next).map(|(_, _, c)| c), Some(false));
    }

    /// The host reads the clients in a whole reading on a split tick, never in a hidden deck's
    /// machine reading; the model turns two reads into this process's share; the inert arm a
    /// test's reader starts with reads nothing.
    #[test]
    fn the_reader_reads_the_clients_in_whole_readings_only() {
        let mut reader = Reader::new(Arc::default());
        assert_eq!(reader.gpu.check, GpuCheck::Unsupported, "the inert arm");
        assert!(!reader.gpu.cold(Instant::now()), "never makes a pull cold");
        assert!(reader.gpu.read(&bars(), Instant::now()).is_none());

        let me = std::process::id();
        let sweep = aterm_sysprobe::processes().expect("a sweep");
        let comm = sweep
            .procs
            .iter()
            .find(|p| p.pid == me)
            .expect("this process");
        NAME.with(|n| n.borrow_mut().clone_from(&comm.name));
        reader.gpu = GpuLane::new(counting);
        let hidden = Request {
            machine_only: true,
            ..bars()
        };
        let _ = reader.read_machine(&hidden);
        assert_eq!(reads(), 0, "a hidden deck's reading");
        let _ = reader.read(&bars());
        assert_eq!(reads(), 1, "the first split tick");
        let _ = reader.read(&bars());
        assert_eq!(reads(), 1, "inside the split period");
        reader.gpu.last = reader.gpu.last.and_then(|t| t.checked_sub(MIN_WINDOW));
        let pull = Request {
            pull: true,
            ..bars()
        };
        let snap = reader.read(&pull);
        assert_eq!(reads(), 2, "a pull");
        let row = snap
            .procs
            .iter()
            .find(|p| p.pid == me)
            .expect("this process's row");
        assert!(row.gpu.is_some_and(|g| g > 0.0), "{row:?}");
        assert!(
            snap.aterm.as_ref().is_some_and(|a| a.gpu == row.gpu),
            "{snap:?}"
        );
    }
}
