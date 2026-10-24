// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `System` port's Fake arm (the tests' seam): a scripted boot (`list-jobs` answers,
//! `is-system-running`'s word once they are spent, a user bus that answers after a few
//! asks) that records what the agent asked of it.

use std::sync::{Condvar, Mutex, MutexGuard};

use crate::system::{Booted, Facts, Identity, RawJob, System};
use crate::wire::Share;

#[derive(Debug)]
struct State {
    jobs: Vec<Vec<RawJob>>,
    jobs_asked: usize,
    booted: Booted,
    bus_after: usize,
    bus_asked: usize,
    now_ns: u64,
    clock_sets: Vec<u64>,
    mounts: Vec<Vec<Share>>,
    mount_refusal: Option<String>,
    poweroffs: usize,
    identity: Result<Identity, String>,
    uptime_ms: u64,
    said: Vec<String>,
}

/// A scripted guest.
#[derive(Debug)]
pub struct Fake {
    state: Mutex<State>,
    turned: Condvar,
}

impl Default for Fake {
    fn default() -> Self {
        Self::new()
    }
}

impl Fake {
    /// A guest already up (`running`, no jobs, the bus answering), whose clock reads
    /// 2026-10-14, running commands as the agent's own user.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                jobs: Vec::new(),
                jobs_asked: 0,
                booted: Booted {
                    state: "running".into(),
                    failed_units: Vec::new(),
                },
                bus_after: 0,
                bus_asked: 0,
                now_ns: 1_792_000_000_000_000_000,
                clock_sets: Vec::new(),
                mounts: Vec::new(),
                mount_refusal: None,
                poweroffs: 0,
                identity: Ok(Identity::Keep),
                uptime_ms: 1000,
                said: Vec::new(),
            }),
            turned: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `list-jobs` answers each of `script` in turn (the last one again after), and the
    /// boot is over only once every one was asked for.
    #[must_use]
    pub fn jobs(self, script: Vec<Vec<RawJob>>) -> Self {
        self.state().jobs = script;
        self
    }

    /// The boot ends `state`, with `failed` units.
    #[must_use]
    pub fn booted(self, state: &str, failed: &[&str]) -> Self {
        self.state().booted = Booted {
            state: state.into(),
            failed_units: failed.iter().map(|u| (*u).to_string()).collect(),
        };
        self
    }

    /// The user bus answers from its `n + 1`th ask.
    #[must_use]
    pub fn bus_after(self, n: usize) -> Self {
        self.state().bus_after = n;
        self
    }

    /// The guest's clock reads `ns`.
    #[must_use]
    pub fn now(self, ns: u64) -> Self {
        self.state().now_ns = ns;
        self
    }

    /// Every `Mount` fails with `why`.
    #[must_use]
    pub fn refuse_mounts(self, why: &str) -> Self {
        self.state().mount_refusal = Some(why.into());
        self
    }

    /// Commands run as `identity`.
    #[must_use]
    pub fn identity(self, identity: Result<Identity, String>) -> Self {
        self.state().identity = identity;
        self
    }

    /// Each time the clock was set, to what.
    #[must_use]
    pub fn clock_sets(&self) -> Vec<u64> {
        self.state().clock_sets.clone()
    }

    /// Each `Mount`'s shares.
    #[must_use]
    pub fn mounts(&self) -> Vec<Vec<Share>> {
        self.state().mounts.clone()
    }

    /// How many `Poweroff`s.
    #[must_use]
    pub fn poweroffs(&self) -> usize {
        self.state().poweroffs
    }

    /// How many times `list-jobs` was asked.
    #[must_use]
    pub fn jobs_asked(&self) -> usize {
        self.state().jobs_asked
    }

    /// Every console line the agent said, in order.
    #[must_use]
    pub fn said(&self) -> Vec<String> {
        self.state().said.clone()
    }
}

/// Why the Fake's user bus does not answer yet ([`Fake::bus_after`]).
pub const BUS_SILENT: &str = "the Fake's user bus does not answer yet";

impl System for Fake {
    fn facts(&self) -> Facts {
        Facts {
            boot_id: "fake-boot".into(),
            kernel: "6.8.0-142-generic".into(),
        }
    }

    fn uptime_ms(&self) -> u64 {
        let mut s = self.state();
        s.uptime_ms += 1;
        s.uptime_ms
    }

    fn jobs(&self) -> Vec<RawJob> {
        let mut s = self.state();
        let at = s.jobs_asked.min(s.jobs.len().saturating_sub(1));
        let answer = s.jobs.get(at).cloned().unwrap_or_default();
        s.jobs_asked += 1;
        drop(s);
        self.turned.notify_all();
        answer
    }

    fn booted(&self) -> Booted {
        let s = self
            .turned
            .wait_while(self.state(), |s| s.jobs_asked < s.jobs.len())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.booted.clone()
    }

    fn user_bus(&self) -> Result<(), String> {
        let mut s = self.state();
        s.bus_asked += 1;
        if s.bus_asked > s.bus_after {
            Ok(())
        } else {
            Err(BUS_SILENT.into())
        }
    }

    fn now_unix_ns(&self) -> u64 {
        self.state().now_ns
    }

    fn set_clock(&self, unix_ns: u64) -> Result<(), String> {
        let mut s = self.state();
        s.clock_sets.push(unix_ns);
        s.now_ns = unix_ns;
        Ok(())
    }

    fn mount(&self, shares: &[Share]) -> Result<(), String> {
        let mut s = self.state();
        if let Some(why) = &s.mount_refusal {
            return Err(why.clone());
        }
        s.mounts.push(shares.to_vec());
        Ok(())
    }

    fn poweroff(&self) -> Result<(), String> {
        self.state().poweroffs += 1;
        Ok(())
    }

    fn identity(&self) -> Result<Identity, String> {
        self.state().identity.clone()
    }

    fn say(&self, line: &str) {
        self.state().said.push(line.to_string());
    }
}
