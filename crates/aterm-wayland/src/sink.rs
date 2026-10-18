// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where the events this crate's seats and selections decode go.
//!
//! [`Seats`](crate::seat::Seats), a selection
//! [`Device`](crate::selection::Device) and a selection
//! [`Source`](crate::selection::Source) hand each event they decode to the
//! caller's [`Sink`] as their listener hears it: on the dispatching thread,
//! inside the dispatch, with no lock of this crate held. A queue dispatches
//! one event at a time, in the order the compositor sent them, so a sink that
//! several objects share, and the caller's own listeners on the same queue,
//! see every event in that order: a key before the input method's commit
//! that followed it, a drag's drop before the pointer's return.
//!
//! Until LW.7's review each object kept its events in a book of its own,
//! which the caller took whole after the dispatch: each object's order held,
//! and the order between objects was lost (docs/AUDIT-third-party-2026-10-15.md,
//! LW.7).

use std::sync::Arc;

/// A caller's sink: called once per event, in dispatch order, inside the
/// dispatch. It records the event and returns.
pub type Sink<T> = Arc<dyn Fn(T) + Send + Sync>;
