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
                use std::fmt::Write as _;
                let mut out = format!(
                    "NOT RUN — the boot hung after the agent: the same start jobs for {secs} s \
                     with no Ready: "
                );
                if jobs.is_empty() {
                    out.push_str(
                        "no start job (systemd called the boot over, and the agent waits on the \
                         lane user's manager)",
                    );
                } else {
                    for (i, j) in jobs.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        let _ = write!(
                            out,
                            "{} {} {} ({} s)",
                            j.unit,
                            j.kind,
                            j.state,
                            j.elapsed_ms / 1000
                        );
                    }
                }
                out.push_str("; VM stopped");
                out
            }
            Self::Heartbeat => {
                format!("the guest stopped answering (no heartbeat for {secs} s); VM stopped")
            }
        }
    }
}

/// The key two `Booting` frames share when they name the same jobs: each job's id, unit and
/// state, in id order (the elapsed times always grow, so they are no part of it).
fn job_key_refs(jobs: &[Job]) -> Vec<(u32, &str, &str)> {
    let mut key: Vec<_> = jobs
        .iter()
        .map(|j| (j.id, j.unit.as_str(), j.state.as_str()))
        .collect();
    key.sort_unstable();
    key
}

fn job_key_matches(stored: &[(u32, String, String)], current: &[(u32, &str, &str)]) -> bool {
    stored.len() == current.len()
        && stored
            .iter()
            .zip(current)
            .all(|((id, u, s), (cid, cu, cs))| id == cid && u == *cu && s == *cs)
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
        let sorted_match = self.jobs_key.len() == jobs.len()
            && jobs.windows(2).all(|w| w[0].id <= w[1].id)
            && self
                .jobs_key
                .iter()
                .zip(jobs)
                .all(|((id, u, s), j)| *id == j.id && u == &j.unit && s == &j.state);
        if !sorted_match {
            let key = job_key_refs(jobs);
            if !job_key_matches(&self.jobs_key, &key) {
                self.jobs_key = key
                    .into_iter()
                    .map(|(id, u, s)| (id, u.to_owned(), s.to_owned()))
                    .collect();
                self.jobs_since = now;
            }
        }
        if self.jobs.len() == jobs.len() {
            for (dst, src) in self.jobs.iter_mut().zip(jobs) {
                dst.id = src.id;
                dst.unit.clone_from(&src.unit);
                dst.kind.clone_from(&src.kind);
                dst.state.clone_from(&src.state);
                dst.elapsed_ms = src.elapsed_ms;
            }
        } else {
            self.jobs.clear();
            self.jobs.extend_from_slice(jobs);
        }
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
#[path = "detect_tests.rs"]
mod tests;
