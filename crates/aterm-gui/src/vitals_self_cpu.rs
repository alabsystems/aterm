// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The own-process reader at the vitals sampler's existing cadence.

use aterm_vitals::self_cpu::{
    OwnCpuCounter, OwnCpuCoverage, OwnCpuCut, OwnCpuProcess, OwnCpuThread,
};
use std::time::Instant;

pub(super) struct Probe {
    pub(super) read: fn(Instant) -> Result<OwnCpuCut, &'static str>,
}

impl Probe {
    pub(super) const NATIVE: Self = Self { read: native };
    pub(super) const INERT: Self = Self {
        read: |_| Err("not_sampled"),
    };

    pub(super) fn sample(
        &self,
        epoch: Instant,
        recorded: bool,
    ) -> Option<Result<OwnCpuCut, &'static str>> {
        // A recorded scene supplies all its own evidence. Do not mix in this process.
        (!recorded).then(|| (self.read)(epoch))
    }
}

fn native(epoch: Instant) -> Result<OwnCpuCut, &'static str> {
    let cut = aterm_sysprobe::self_cpu::read(epoch).map_err(|why| why.word())?;
    Ok(project(cut))
}

fn project(cut: aterm_sysprobe::self_cpu::Cut) -> OwnCpuCut {
    let counter = |c: aterm_sysprobe::self_cpu::CpuCounter| OwnCpuCounter {
        ns: c.ns,
        precision_ns: c.precision_ns,
    };
    OwnCpuCut {
        process: OwnCpuProcess {
            pid: cut.process.pid,
            start: cut.process.start,
            version: cut.process.version,
        },
        begin_ns: cut.begin_ns,
        end_ns: cut.end_ns,
        process_before: counter(cut.process_before),
        process_after: counter(cut.process_after),
        threads: cut
            .threads
            .into_iter()
            .map(|thread| OwnCpuThread {
                id: thread.id,
                name: thread.name,
                cpu: thread.cpu.map(counter),
            })
            .collect(),
        coverage: OwnCpuCoverage {
            listed: cut.coverage.listed,
            inspected: cut.coverage.inspected,
            unread: cut.coverage.unread,
            capped: cut.coverage.capped,
        },
    }
}

#[cfg(test)]
#[path = "vitals_self_cpu_tests.rs"]
mod tests;
