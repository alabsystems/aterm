// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-vitals`: what the machine, aterm and each tab are doing
//! (`docs/DESIGN-system-awareness-2026-10-16.md` §4, §7, §12.1).
//!
//! The pure half of system awareness. The host (`aterm-gui`'s `vitals_host`)
//! reads the machine through `aterm-sysprobe` on its probe thread and hands
//! this crate plain numbers ([`Sample`]); [`Vitals::absorb`] turns them into
//! one immutable [`Snapshot`] consumed by the stats Deck and by the agent
//! interface, `aterm ctl sys` and `await sys` ([`sys`]).
//!
//! * [`trend`]: level (τ 2 s), momentum (fast − slow, τ 20 s) and variance
//!   (a 10 s median absolute deviation) for every quantity, and its history.
//! * [`model`]: cores, GPU, memory, the volume, and the three scopes, Global,
//!   aterm and Tab, attributed by parent chains in one O(N) pass; and the
//!   deck's seven rows (`docs/DESIGN-stats-deck-v2-2026-10-20.md`), each a
//!   [`ResourceView`].
//! * [`ring`]: the rows' hour of raw readings in 10 s wall-clock buckets, the
//!   strip's columns, and "now".
//! * [`history`]: the disk's 24 h free history, its rates and time to full.
//! * [`peaks`]: the network's and tokens' 7-day peaks, and their learning.
//! * [`words`]: the sentences and compact figures.
//! * [`ink`]: the load ramp, the Deck's heat ramp and the scope ladder,
//!   solved in OkLab from the ground they sit on.
//! * [`events`]: what changed between snapshots (pressure, throttle, swap,
//!   free space, load, tabs, readers).
//! * [`sys`]: the `sys` verb's grammar, its text and JSON views, deltas
//!   against an earlier reply, and the `await sys` predicates.
//! * [`jobs`]: stuck processes and jobs (§11): job units, progress, classes
//!   and named waits, who waits for each job, φ and the closed verdict set.
//! * [`program_check`]: whether macOS's program check (`syspolicyd`) still
//!   answers, from its thread pool's readings.
//!
//! No clock, no platform, no dependencies: the host owns time and the OS.

#![warn(missing_docs)]

pub mod delegated;
pub mod events;
pub mod history;
pub mod ink;
pub mod jobs;
pub mod model;
pub mod peaks;
pub mod program_check;
pub mod ring;
pub mod sys;
pub mod trend;
pub mod words;

pub use history::{Filling, FreeHistory, Rates};
pub use model::{
    AccountIx, COMMIT_NEAR_PCT, CapBasis, ClusterSpec, ClusterView, CommitSample, CoreView,
    CpuView, DiskFacts, DiskLevel, GpuSample, GpuView, HeatCell, Hog, LinkKind, LinkSample,
    MemCounters, MemRates, MemSample, MemView, NoFigure, PlanFact, Pressure, ProcLimits,
    ProcSample, ProcState, ProcView, Resource, ResourceView, RowFacts, RowScope, Sample, ScopeFig,
    ScopeView, Series, Snapshot, Split, State, TabRoot, TabTokens, TabView, Thermal, Throttle,
    Ticks, Vitals, VolumeSample, Writer,
};
pub use peaks::{DailyMax, PeakRow, Peaks};
pub use ring::{Bucket, BucketRing, Cell};
pub use trend::{History, Motion, Trend, TrendView};
