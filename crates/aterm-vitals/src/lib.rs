// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure system-awareness telemetry model, trends, rings, and `sys` query formatting.

#![warn(missing_docs)]

pub mod blockage;
pub mod delegated;
pub mod events;
pub mod explain;
pub mod gpu_clients;
pub mod hardware;
pub mod heat;
pub mod history;
pub mod ink;
pub mod jobs;
pub mod left;
pub mod model;
pub mod peaks;
pub mod program_check;
pub mod ring;
pub mod runq;
pub mod scheduling;
pub mod services;
pub mod situation;
pub mod starts;
pub mod strain;
pub mod sys;
pub mod trend;
pub mod units;
pub mod words;
pub mod writes;

pub use gpu_clients::{GpuClientRow, GpuClientsSample, GpuSplit};
pub use history::{Filling, FreeHistory, Rates};
pub use model::{
    AccountIx, AneCheck, AneSample, COMMIT_NEAR_PCT, CapBasis, ClusterSpec, ClusterView,
    CommitSample, CoreView, CpuView, DiskFacts, DiskLevel, GpuSample, GpuView, HeatCell, HeatSlot,
    Hog, LinkKind, LinkRates, LinkSample, LinkView, MemCounters, MemRates, MemSample, MemView,
    NetState, NetView, NoFigure, PlaceKind, PlaceValue, PlaceView, PlanFact, Pressure, ProcSample,
    ProcState, ProcView, Resource, ResourceView, RowFacts, RowScope, STRIP_RINGS, Sample,
    SavedStrip, ScopeFig, ScopeView, Series, Snapshot, Split, State, StripEvent, StripEventKind,
    StripLine, TabRoot, TabTokens, TabView, TaskRoom, TcpCounts, Thermal, Throttle, Ticks, Vitals,
    VolumeSample, Writer,
};
pub use peaks::{DailyMax, DiskPeaks, PeakRow, Peaks};
pub use ring::{
    Agg, BucketRing, Cell, GapKind, Lands, MultiScaleSpec, ProgSlot, Reading, SavedBucket,
    SavedReading, SavedRing, ScaleBand, ScaleCell, SlotGrid, Taken, Tiers, TimeGap, lands,
};
pub use starts::{StartCount, TaskFlow};
pub use strain::Strain;
pub use trend::{History, Motion, Trend, TrendView};
pub use units::{BlockSample, DiskLocation, Units};
pub use writes::{ProgramHour, WriteHour};
