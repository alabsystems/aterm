// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The helper's detectors (`docs/DESIGN-vz-guest-2026-10-23.md` §10): every wait is a
//! one-minute no-progress detector, never a latency budget, and progress is never a console
//! byte alone. They run on the clock the helper hands them (a `Duration` since its start),
//! so the tests step it.
//!
//! | Phase | Fires when | Progress is |
//! | --- | --- | --- |
//! | before the agent's `Hello` | [`SPAN`] with no new console line | a console line no earlier one matched ([`crate::console`]) |
//! | after `Hello`, before `Ready` | the `Booting` frames name the same start jobs for [`SPAN`] | a change in the job set |
//! | after `Ready` | [`SPAN`] with no `Beat` | a `Beat` |
//!
//! The free-space read ([`Space`]) runs every [`TICK`] while the VM runs.

use std::time::Duration;

use aterm_vz_agent::wire::Job;

/// The no-progress span: one minute.
pub const SPAN: Duration = Duration::from_secs(60);

/// How often the helper reads the free space and the agent beats: 5 s.
pub const TICK: Duration = Duration::from_secs(5);

/// Which detector fired, with what the report names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fired {
    /// No `Hello` and no new console line for [`SPAN`].
    BootBeforeAgent,
    /// The agent's `Booting` frames named the same start jobs for [`SPAN`], with no `Ready`.
    BootAfterAgent {
        /// The jobs the last frame named.
        jobs: Vec<Job>,
    },
    /// No `Beat` for [`SPAN`] after `Ready`.
    Heartbeat,
}

impl Fired {
    /// The words every in-flight `Out` ends with (§10's last column).
    #[must_use]
    pub fn why(&self) -> String {
        let secs = SPAN.as_secs();
        match self {
            Self::BootBeforeAgent => format!(
                "NOT RUN — the boot hung before the agent: no hello and no new console line \
                 for {secs} s; VM stopped"
            ),
            Self::BootAfterAgent { jobs } => {
                let named = if jobs.is_empty() {
                    "no start job (systemd called the boot over, and the agent waits on the \
                     lane user's manager)"
                        .to_string()
                } else {
                    jobs.iter()
                        .map(|j| {
                            format!(
                                "{} {} {} ({} s)",
                                j.unit,
                                j.kind,
                                j.state,
                                j.elapsed_ms / 1000
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!(
                    "NOT RUN — the boot hung after the agent: the same start jobs for {secs} s \
                     with no Ready: {named}; VM stopped"
                )
            }
            Self::Heartbeat => {
                format!("the guest stopped answering (no heartbeat for {secs} s); VM stopped")
            }
        }
    }
}

/// The key two `Booting` frames share when they name the same jobs: each job's id, unit and
/// state, in id order (the elapsed times always grow, so they are no part of it).
fn job_key(jobs: &[Job]) -> Vec<(u32, String, String)> {
    let mut key: Vec<_> = jobs
        .iter()
        .map(|j| (j.id, j.unit.clone(), j.state.clone()))
        .collect();
    key.sort();
    key
}

/// The boot and heartbeat detectors over one boot.
#[derive(Debug)]
pub struct Watch {
    started: Duration,
    hello: Option<Duration>,
    jobs: Vec<Job>,
    jobs_key: Vec<(u32, String, String)>,
    jobs_since: Duration,
    ready: Option<Duration>,
    beat: Option<Duration>,
    span: Duration,
}

impl Watch {
    /// Watch a boot that started at `now`.
    #[must_use]
    pub const fn new(now: Duration) -> Self {
        Self {
            started: now,
            hello: None,
            jobs: Vec::new(),
            jobs_key: Vec::new(),
            jobs_since: now,
            ready: None,
            beat: None,
            span: SPAN,
        }
    }

    /// The agent said hello at `now`.
    pub fn hello(&mut self, now: Duration) {
        if self.hello.is_none() {
            self.hello = Some(now);
            self.jobs_since = now;
        }
    }

    /// A `Booting` frame naming `jobs` arrived at `now`.
    pub fn booting(&mut self, jobs: &[Job], now: Duration) {
        let key = job_key(jobs);
        if key != self.jobs_key {
            self.jobs_key = key;
            self.jobs_since = now;
        }
        self.jobs = jobs.to_vec();
    }

    /// The agent said `Ready` at `now`.
    pub fn ready(&mut self, now: Duration) {
        if self.ready.is_none() {
            self.ready = Some(now);
        }
    }

    /// A `Beat` arrived at `now`.
    pub const fn beat(&mut self, now: Duration) {
        self.beat = Some(now);
    }

    /// When the last `Beat` (or `Ready`) came.
    #[must_use]
    pub fn last_beat(&self) -> Option<Duration> {
        self.beat.or(self.ready)
    }

    /// Whether the agent has said `Ready`.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.ready.is_some()
    }

    /// Whether the agent has said hello.
    #[must_use]
    pub const fn has_hello(&self) -> bool {
        self.hello.is_some()
    }

    /// The detector that fires at `now`, if any; `console` is when the console last made
    /// progress ([`crate::console::Console::last_progress`]).
    #[must_use]
    pub fn check(&self, now: Duration, console: Option<Duration>) -> Option<Fired> {
        if let Some(ready) = self.ready {
            let last = self.beat.unwrap_or(ready).max(ready);
            return (now.saturating_sub(last) >= self.span).then_some(Fired::Heartbeat);
        }
        if self.hello.is_some() {
            return (now.saturating_sub(self.jobs_since) >= self.span).then(|| {
                Fired::BootAfterAgent {
                    jobs: self.jobs.clone(),
                }
            });
        }
        let last = console.map_or(self.started, |c| c.max(self.started));
        (now.saturating_sub(last) >= self.span).then_some(Fired::BootBeforeAgent)
    }
}

/// The free-space watch during a run: one read every [`TICK`].
#[derive(Debug)]
pub struct Space {
    next: Duration,
    last: Option<crate::space::Free>,
}

impl Space {
    /// The first read is due at `now`.
    #[must_use]
    pub const fn new(now: Duration) -> Self {
        Self {
            next: now,
            last: None,
        }
    }

    /// Whether a read is due at `now`; if so, the next is a [`TICK`] later.
    pub fn due(&mut self, now: Duration) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + TICK;
        true
    }

    /// A read came back: the words to stop the VM with when it is below
    /// [`crate::space::RUN_FLOOR`] ([`crate::space::below_run`]).
    pub fn read(&mut self, free: crate::space::Free) -> Option<String> {
        let words = crate::space::below_run(&free);
        self.last = Some(free);
        words
    }

    /// The last read's free bytes.
    #[must_use]
    pub fn last_bytes(&self) -> Option<u64> {
        self.last.as_ref().map(|f| f.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn job(id: u32, unit: &str, elapsed_s: u64) -> Job {
        Job {
            id,
            unit: unit.into(),
            kind: "start".into(),
            state: "running".into(),
            elapsed_ms: elapsed_s * 1000,
        }
    }

    #[test]
    fn before_the_agent_only_a_new_console_line_is_progress() {
        let w = Watch::new(S * 10);
        assert_eq!(w.check(S * 69, None), None);
        assert_eq!(w.check(S * 70, None), Some(Fired::BootBeforeAgent));
        // A console line at 40 s moves the span; a line older than the start does not.
        assert_eq!(w.check(S * 99, Some(S * 40)), None);
        assert_eq!(w.check(S * 100, Some(S * 40)), Some(Fired::BootBeforeAgent));
        assert_eq!(w.check(S * 70, Some(S)), Some(Fired::BootBeforeAgent));
    }

    #[test]
    fn the_same_jobs_for_the_span_fire_it_while_console_lines_keep_arriving() {
        let mut w = Watch::new(Duration::ZERO);
        w.hello(S * 2);
        let wait = job(117, "systemd-networkd-wait-online.service", 0);
        for t in (2..=62).step_by(5) {
            let mut j = wait.clone();
            j.elapsed_ms = (t - 2) * 1000;
            w.booting(&[j], S * u32::try_from(t).unwrap());
        }
        // Console lines at every second count for nothing once the agent speaks.
        assert_eq!(w.check(S * 61, Some(S * 61)), None);
        let fired = w.check(S * 62, Some(S * 62)).expect("fired");
        let Fired::BootAfterAgent { jobs } = &fired else {
            panic!("{fired:?}")
        };
        assert_eq!(jobs[0].unit, "systemd-networkd-wait-online.service");
        let why = fired.why();
        assert!(
            why.starts_with("NOT RUN — the boot hung after the agent")
                && why.contains("systemd-networkd-wait-online.service start running (60 s)")
                && why.ends_with("VM stopped"),
            "{why}"
        );
    }

    #[test]
    fn the_negative_control_a_job_set_that_changes_within_each_span_never_fires() {
        let mut w = Watch::new(Duration::ZERO);
        w.hello(Duration::ZERO);
        for t in 0..600u32 {
            // A different job every 50 s: each span sees a change.
            let id = t / 50;
            w.booting(&[job(id, &format!("unit-{id}.service"), 0)], S * t);
            assert_eq!(w.check(S * t, None), None, "at {t} s");
        }
    }

    #[test]
    fn an_agent_that_goes_quiet_after_hello_fires_the_boot_detector() {
        let mut w = Watch::new(Duration::ZERO);
        w.hello(S * 5);
        assert_eq!(w.check(S * 64, None), None);
        assert_eq!(
            w.check(S * 65, None),
            Some(Fired::BootAfterAgent { jobs: Vec::new() })
        );
        assert!(
            w.check(S * 65, None)
                .unwrap()
                .why()
                .contains("no start job")
        );
    }

    #[test]
    fn after_ready_only_a_missing_beat_fires() {
        let mut w = Watch::new(Duration::ZERO);
        w.hello(S);
        w.ready(S * 30);
        assert_eq!(w.check(S * 89, None), None);
        for t in (35..300).step_by(5) {
            w.beat(S * t);
            assert_eq!(w.check(S * (t + 4), None), None);
        }
        assert_eq!(w.check(S * (295 + 59), None), None);
        assert_eq!(w.check(S * (295 + 60), None), Some(Fired::Heartbeat));
        assert_eq!(
            Fired::Heartbeat.why(),
            "the guest stopped answering (no heartbeat for 60 s); VM stopped"
        );
    }

    #[test]
    fn free_space_is_read_every_tick_and_stops_below_the_run_floor() {
        let mut s = Space::new(Duration::ZERO);
        assert!(s.due(Duration::ZERO));
        assert!(!s.due(S * 4));
        assert!(s.due(S * 5));
        let free = |bytes| crate::space::Free {
            bytes,
            volume: "/System/Volumes/Data (container:disk3)".into(),
        };
        assert_eq!(s.read(free(12 << 30)), None);
        assert_eq!(s.last_bytes(), Some(12 << 30));
        assert_eq!(
            s.read(free(3 << 30)).as_deref(),
            Some("the volume holding aterm-linux.img has 3.0 GiB free; VM stopped")
        );
    }
}
