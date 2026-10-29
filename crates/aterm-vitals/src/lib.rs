// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure system-awareness telemetry model, trends, rings, and `sys` query formatting.

#![warn(missing_docs)]

pub mod delegated;
pub mod events;
pub mod explain;
pub mod gpu_clients;
pub mod hardware;
pub mod history;
pub mod ink;
pub mod jobs;
pub mod model;
pub mod peaks;
pub mod program_check;
pub mod ring;
pub mod scheduling;
pub mod services;
pub mod strain;
pub mod sys;
pub mod trend;
pub mod words;
pub mod writes;

pub use gpu_clients::{GpuClientRow, GpuClientsSample, GpuSplit};
pub use history::{Filling, FreeHistory, Rates};
pub use model::{
    AccountIx, COMMIT_NEAR_PCT, CapBasis, ClusterSpec, ClusterView, CommitSample, CoreView,
    CpuView, DiskFacts, DiskLevel, GpuSample, GpuView, HeatCell, HeatSlot, Hog, LinkKind,
    LinkRates, LinkSample, LinkView, MemCounters, MemRates, MemSample, MemView, NetState, NetView,
    NoFigure, PlanFact, Pressure, ProcLimits, ProcSample, ProcState, ProcView, Resource,
    ResourceView, RowFacts, RowScope, Sample, SavedStrip, ScopeFig, ScopeView, Series, Snapshot,
    Split, State, StripEvent, StripEventKind, TabRoot, TabTokens, TabView, TcpCounts, Thermal,
    Throttle, Ticks, Vitals, VolumeSample, Writer,
};
pub use peaks::{DailyMax, PeakRow, Peaks};
pub use ring::{
    Bucket, BucketRing, Cell, GapKind, MultiScaleSpec, ProgSlot, Reading, SavedBucket,
    SavedReading, SavedRing, ScaleBand, ScaleCell, SlotGrid, Tiers, TimeGap,
};
pub use strain::Strain;
pub use trend::{History, Motion, Trend, TrendView};
pub use writes::{ProgramHour, WriteHour};
