// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit CPU observations for positive admission fixtures; never the live machine.

use aterm_capacity::{CpuPoint, Machine, Procs, Reading};
use std::{
    cell::Cell,
    path::Path,
    time::{Duration, Instant},
};

pub(crate) struct Measured<'a> {
    machine: &'a dyn Machine,
    origin: Instant,
    points: Cell<u64>,
}

impl<'a> Measured<'a> {
    pub(crate) fn new(machine: &'a dyn Machine) -> Self {
        Self {
            machine,
            origin: Instant::now(),
            points: Cell::new(0),
        }
    }
}

impl Machine for Measured<'_> {
    fn read(&self, path: Option<&Path>) -> Reading {
        self.machine.read(path)
    }
    fn procs(&self) -> Option<Procs> {
        self.machine.procs()
    }
    fn cpu_point(&self) -> Option<CpuPoint> {
        let n = self.points.get() + 1;
        self.points.set(n);
        Some(CpuPoint {
            at: self.origin + Duration::from_secs(n),
            unix_ms: aterm_time::unix_millis()?,
            ticks: vec![aterm_sysprobe::CoreTicks {
                user: n,
                idle: n * 99,
                ..Default::default()
            }],
        })
    }
}
