// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An exported momentum trace as one file ([`crate::aos_effect::EffectTrace`] over
//! [`MomentumEngine`]), and its replay in another process.

use super::{KeyEcho, MomentumEngine, MomentumEvent};

pub use crate::aos_effect::TraceFault;

/// The bytes that open a momentum trace file.
pub const TRACE_FILE_MAGIC: [u8; 8] = *b"AOSMTR\0\x01";

/// The domain of the digest that closes a trace file.
pub const TRACE_FILE_DOMAIN: &[u8] = b"aterm-aos-momentum-trace-file-v1\0";

/// The most bytes a reader that checks traces reads, before it decodes one: at least twice a trace file of one full host log epoch ([`super::MOMENTUM_HOST_MAX_EVENTS`] inputs and its checkpoint).
pub const MOMENTUM_TRACE_MAX_BYTES: usize = 1 << 21;

/// An exported momentum trace.
pub type MomentumTrace = crate::aos_effect::EffectTrace<MomentumEngine>;

/// A momentum trace decoded on a reader's epoch ([`MomentumTrace::read`]).
pub type ReadTrace = crate::aos_effect::ReadTrace<MomentumEngine>;

/// What [`MomentumTrace::replay`] reached.
pub type TraceReplay = crate::aos_effect::TraceReplay<MomentumEngine>;

/// What [`MomentumTrace::check`] found: the replay and the contract's verdict on it.
pub type TraceCheck = crate::aos_effect::TraceCheck<MomentumEngine>;

/// How many inputs of each kind a log holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventCounts {
    /// Every key, seen or not.
    pub keys: usize,
    /// The keys among [`Self::keys`] logged [`KeyEcho::Unseen`].
    pub unseen: usize,
    pub resets: usize,
    pub ticks: usize,
    pub queries: usize,
}

impl EventCounts {
    /// The counts over `inputs`.
    #[inline]
    #[must_use]
    pub const fn of(inputs: &[MomentumEvent]) -> Self {
        let mut c = EventCounts {
            keys: 0,
            unseen: 0,
            resets: 0,
            ticks: 0,
            queries: 0,
        };
        let mut i = 0;
        while i < inputs.len() {
            if let MomentumEvent::Key {
                echo: KeyEcho::Unseen,
                ..
            } = inputs[i]
            {
                c.unseen = c.unseen.saturating_add(1);
            }
            let slot = match inputs[i] {
                MomentumEvent::Key { .. } => &mut c.keys,
                MomentumEvent::Reset => &mut c.resets,
                MomentumEvent::Tick { .. } => &mut c.ticks,
                MomentumEvent::Query { .. } => &mut c.queries,
            };
            *slot = slot.saturating_add(1);
            i += 1;
        }
        c
    }

    /// Every input counted (an unseen key once, among the keys).
    #[inline]
    #[must_use]
    pub const fn total(&self) -> usize {
        self.keys
            .saturating_add(self.resets)
            .saturating_add(self.ticks)
            .saturating_add(self.queries)
    }
}
