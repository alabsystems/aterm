// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-vitals`: what the machine, aterm and each tab are doing
//! (`docs/DESIGN-system-awareness-2026-10-16.md` §4, §7, §12.1).
//!
//! The pure half of system awareness. The host (`aterm-gui`'s `vitals_host`)
//! reads the machine through `aterm-sysprobe` on its probe thread and hands
//! this crate plain numbers ([`Sample`]); [`Vitals::absorb`] turns them into
//! one immutable [`Snapshot`] consumed by the stats Deck. Accessibility,
//! control and agent views are planned consumers of the same snapshot.
//!
//! * [`trend`]: level (τ 2 s), momentum (fast − slow, τ 20 s) and variance
//!   (a 10 s median absolute deviation) for every quantity, and its history.
//! * [`model`]: cores, GPU, memory, the volume, and the three scopes, Global,
//!   aterm and Tab, attributed by parent chains in one O(N) pass.
//! * [`words`]: the sentences and compact figures.
//! * [`ink`]: the load ramp and the scope ladder, solved in OkLab from the
//!   ground they sit on.
//! * [`jobs`]: stuck processes and jobs (§11): job units, progress, classes
//!   and named waits, who waits for each job, φ and the closed verdict set.
//!
//! No clock, no platform, no dependencies: the host owns time and the OS.

#![warn(missing_docs)]

pub mod ink;
pub mod jobs;
pub mod model;
pub mod trend;
pub mod words;

pub use model::{
    COMMIT_NEAR_PCT, ClusterSpec, ClusterView, CommitSample, CoreView, CpuView, GpuSample, GpuView,
    MemSample, MemView, Pressure, ProcSample, Sample, ScopeView, Snapshot, TabRoot, TabView, Ticks,
    Vitals, VolumeSample,
};
pub use trend::{History, Motion, Trend, TrendView};
