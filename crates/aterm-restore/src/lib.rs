// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! aterm's crash restore, headless: exact screens and scrollback after an unplanned window death.

#![forbid(unsafe_code)]

pub mod layout;
pub mod plant;
pub mod policy;
pub mod records;
pub mod recover;
pub mod store;
pub mod writer;

#[cfg(test)]
mod conformance_region;

pub use layout::{Geometry, LayoutError, REGION_LEN};
pub use policy::{Decision, KeyframePolicy, Reason};
pub use records::{
    ControlFault, HistoryChunk, HistoryIdentity, Keyframe, KeyframeFault, KeyframeHead, TailFault,
    TailRecord,
};
pub use recover::{
    ControlVerdict, HeadLost, HeaderFacts, JoinFacts, Losses, MetaWhy, RecoverPolicy, Recovery,
    Restored, RingExport, RingRun, read_control, read_erased_turn_count, read_header, recover,
};
#[cfg(any(test, debug_assertions))]
pub use store::CountdownStore;
pub use store::{RegionStore, RegionView, Shared, StoreError, Touched, VecStore};
pub use writer::{
    ControlWriter, HistoryWriter, JournalWriter, KeyframeError, KeyframeInput, WriteError,
    WriterIdentity, adaptive_hist_cap, erase_region,
};
