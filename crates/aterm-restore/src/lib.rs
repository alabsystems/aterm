// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's crash restore, headless: exact screens and scrollback after an unplanned window
//! death.
//!
//! A planned handoff (a seamless update) carries each session's checkpoint, its
//! history and its control state across. After a SIGKILL, a jetsam kill, a Force
//! Quit or a crash there is nothing to carry from, because the process that would
//! write them is gone. So the window W writes, continuously and at bounded cost,
//! everything a restore needs into a place that outlives it: one anonymous POSIX
//! shared-memory REGION per session, which the PTY keeper holds (read-only, never
//! mapped) beside the session's master. After W dies the keeper offers that region
//! to the next window W' with the master; W' maps it read-only, rebuilds the screen
//! in a scratch engine, and adopts the result through the same landing an update
//! uses.
//!
//! This crate is everything about the region that is not the operating system:
//!
//! - [`layout`]: the v1 offsets (an 8 MiB region of 16 KiB pages, a 4 KiB header
//!   of little-endian `u64` words, two keyframe slots, the tail ring, the control
//!   slot and the history ring).
//! - [`records`]: the three record kinds and their strict decoders. A TAIL record
//!   is raw engine input (bytes, a resize, a count of resizes lost); a KEYFRAME
//!   (KFR v1) and a HISTORY CHUNK (HCR v1) are astream step records
//!   (`astream_wire::steps`: dense, versioned, CRC-framed) around an LZ4 body.
//! - [`store`]: the [`store::RegionStore`] a writer and a reader go through, with an
//!   in-memory [`store::VecStore`] and, for tests, a [`store::CountdownStore`] that
//!   drops every store after the k-th: a death at instruction granularity. A window's
//!   region goes through [`store::Touched`], which counts the pages its stores made
//!   resident, and [`store::Shared`], which its writing threads share and a memory shed
//!   swaps a fresh region into ([`store::Shared::swap_in`]).
//! - [`writer`]: [`writer::JournalWriter`], the single-writer protocols, and
//!   [`writer::HistoryWriter`], the history ring's (the window's journal worker owns one
//!   per session apart from the parse thread's writer; [`writer::adaptive_hist_cap`] sizes
//!   it). Every one publishes its data before the commit word that admits it (a release
//!   store, read with an acquire load), so a writer that dies between any two stores
//!   leaves a region a reader can trust up to its commit words.
//! - [`policy`]: when to keyframe (the plan's K1–K6, and K7), a pure state machine the parse thread
//!   hands the time.
//! - [`recover`]: [`recover::recover`], which checks the header, picks the newest
//!   valid keyframe, restores it into a scratch engine the caller builds, replays the
//!   bounds-checked tail past it, drains and drops every reply, OSC event and bell
//!   the replay produced, and takes the result with every loss counted, and with what the
//!   history join reads ([`recover::Restored::join_facts`]: the head the replay moved on,
//!   the carried lines, the ring as an export). It is panic-free on any bytes.
//!
//! An ENGINE crate (aterm-census's `engine-platform`): no file system, thread,
//! socket, process or clock. The window creates and maps the region (aterm-uds) and
//! hands this crate a store; the parse thread hands the policy the time. Nothing
//! here reaches a network or the fabric: a region is local to the host, and no
//! control verb reads one.
//!
//! The formal model is `CrashRestoreJournal`
//! (`aterm_models_session::crash_restore_journal_model`): Tier-0 in aterm-models-session, Tier-1
//! in `conformance_region_tests.rs`, which drives the real writer over a store that dies at
//! every primitive store and runs the real recovery.

#![forbid(unsafe_code)]

pub mod layout;
pub mod policy;
pub mod records;
pub mod recover;
pub mod store;
pub mod writer;

// Named as a test file (`*_tests.rs`), which is how the source guards know one;
// the module keeps the name the `#[refines]` anchors project through.
#[cfg(test)]
#[path = "conformance_region_tests.rs"]
mod conformance_region;

pub use layout::{Geometry, LayoutError, REGION_LEN};
pub use policy::{Decision, KeyframePolicy, Reason};
pub use records::{
    HistoryChunk, HistoryIdentity, Keyframe, KeyframeFault, KeyframeHead, TailFault, TailRecord,
};
pub use recover::{
    HeadLost, HeaderFacts, JoinFacts, Losses, MetaWhy, RecoverPolicy, Recovery, Restored,
    RingExport, RingRun, read_header, recover,
};
#[cfg(any(test, debug_assertions))]
pub use store::CountdownStore;
pub use store::{RegionStore, RegionView, Shared, StoreError, Touched, VecStore};
pub use writer::{
    HistoryWriter, JournalWriter, KeyframeError, KeyframeInput, WriteError, WriterIdentity,
    adaptive_hist_cap,
};
