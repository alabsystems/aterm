// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Functional desktop admission, without sampled Timing or a load-dependent verdict.

use std::cell::Cell;
use std::path::Path;

use aterm_capacity::{Admit, Claim, Ledger, Machine, Need, Pace};

const MARGIN: u64 = 128 * 1024 * 1024;

/// Keep this original claim until the instance and its observed helpers have been retired.
/// Native ledger/ancestry evidence reuses a verifier ancestor's existing quiet reservation;
/// no environment label or reopened file grants coverage. Positive memory and a known
/// observation remain required even when an ancestor covers the desktop exclusion.
pub fn acquire(target: &str, memory: u64, mode: Admit) -> Result<Claim, String> {
    acquire_in(
        Ledger::user().as_ref(),
        &aterm_capacity::Live,
        Need::new(format!("measure {target}: functional capture"))
            .mem(memory)
            .margin(MARGIN)
            .quiet(f64::INFINITY),
        mode,
        Pace::default(),
        &mut |line| eprintln!("{line}"),
    )
}

struct KnownMemory<'a> {
    inner: &'a dyn Machine,
    unknown: Cell<bool>,
}

impl Machine for KnownMemory<'_> {
    fn person(&self) -> bool {
        self.inner.person()
    }
    fn release_run(&self) -> bool {
        self.inner.release_run()
    }
    fn read(&self, path: Option<&Path>) -> aterm_capacity::Reading {
        let mut reading = self.inner.read(path);
        if !matches!((reading.available, reading.physical), (Some(available), Some(physical)) if physical > 0 && available <= physical)
        {
            self.unknown.set(true);
            // Positive margin prevents auto-ALONE while the admission loop observes this stop.
            reading.available = Some(0);
        }
        reading
    }
    fn cpu_point(&self) -> Option<aterm_capacity::CpuPoint> {
        self.inner.cpu_point()
    }
    fn gpu_busy(&self, stopped: &dyn Fn() -> bool) -> Option<u8> {
        self.inner.gpu_busy(stopped)
    }
    fn procs(&self) -> Option<aterm_capacity::Procs> {
        self.inner.procs()
    }
    fn legacy_slots(
        &self,
        ledger: &Ledger,
        cores: u32,
    ) -> aterm_capacity::build_slot_wait::held::HeldSlots {
        self.inner.legacy_slots(ledger, cores)
    }
    fn legacy_lineage(&self, pid: u32) -> Vec<aterm_sysprobe::slots::Holder> {
        self.inner.legacy_lineage(pid)
    }
    fn lineage(&self, pid: u32) -> Option<aterm_capacity::Procs> {
        self.inner.lineage(pid)
    }
}

fn acquire_in(
    ledger: Option<&Ledger>,
    machine: &dyn Machine,
    need: Need,
    mode: Admit,
    pace: Pace,
    say: &mut dyn FnMut(&str),
) -> Result<Claim, String> {
    if mode != Admit::Wait {
        return Err("functional windows require --admit wait".into());
    }
    if need.mem == 0
        || need.margin == 0
        || need.alone
        || need.patience.is_some()
        || need.quiet_policy != aterm_capacity::QuietPolicy::Load
        || need.quiet != Some(f64::INFINITY)
    {
        return Err("functional windows require a positive memory budget and margin, Load exclusion and no ALONE or Timing".into());
    }
    let ledger = ledger.ok_or("no account capacity ledger is available")?;
    let machine = KnownMemory {
        inner: machine,
        unknown: Cell::new(false),
    };
    let _ = machine.read(None);
    if machine.unknown.get() {
        return Err("usable memory observation is unavailable".into());
    }
    let result = aterm_capacity::admit_until(ledger, &machine, need, mode, pace, say, &|| {
        machine.unknown.get()
    });
    if machine.unknown.get() {
        return Err("memory observation changed to unknown before admission".into());
    }
    result.map_err(|error| format!("functional window admission: {error}"))
}

#[cfg(test)]
#[path = "functional_tests.rs"]
mod tests;
