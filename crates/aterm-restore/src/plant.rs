// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A measurement's damage, never a restore's: the frame look-alikes `benches/resync_walk.rs`
//! prices [`crate::recover`] over ([`lookalikes`], [`flip`]), and the same damage planted into a
//! region the real writer built ([`lookalike_tail`]), which the crash-restore lane's look-alike
//! arm (`verify perf restore-e2e`) has a recovering window plant into its copy of each offered
//! region. Built in every profile, so the bench builds under each (cargo's own `bench` profile
//! has no debug assertions); the one caller outside a bench, the window's seam, is a debug and
//! test build's, so nothing a shipped binary runs reaches it.

use astream_wire::frame::{HEADER_SIZE, MAGIC, MAX_PAYLOAD_LEN, VERSION};
use astream_wire::steps::scan_steps_from;

use crate::layout::{OFF_HIST_CAP, OFF_HIST_HI, OFF_HIST_LO, OFF_HIST_LO_SEQ};
use crate::recover::read_header;
use crate::store::{RegionStore, StoreError};

/// The region offset of ring position `pos` in a ring of `cap` bytes at `hist_off`.
#[must_use]
pub fn ring_offset(hist_off: usize, cap: u64, pos: u64) -> usize {
    let rem = pos.checked_rem(cap).unwrap_or(0);
    hist_off.saturating_add(usize::try_from(rem).unwrap_or(usize::MAX))
}

/// Flip the byte at ring position `pos`: a payload byte's flip fails its frame's CRC and leaves
/// its header whole.
///
/// # Errors
/// The store's refusal of the read or the write.
pub fn flip<S: RegionStore + ?Sized>(
    store: &S,
    hist_off: usize,
    cap: u64,
    pos: u64,
) -> Result<(), StoreError> {
    if cap == 0 {
        return Err(StoreError::OutOfBounds {
            at: hist_off,
            len: 1,
        });
    }
    let at = ring_offset(hist_off, cap, pos);
    let mut b = [0u8; 1];
    store.read(at, &mut b)?;
    store.write(at, &[b[0] ^ 0xFF])
}

/// Frame look-alikes across ring positions `from..until` (none reaching `seg_end`): a header
/// every [`HEADER_SIZE`] bytes, each naming the length up to `seg_end` (at most
/// [`MAX_PAYLOAD_LEN`]), so a search after a break runs a CRC over each one's claim. Written in
/// one store write; the positions must not wrap the ring.
///
/// # Errors
/// The store's refusal of the write.
pub fn lookalikes<S: RegionStore + ?Sized>(
    store: &S,
    hist_off: usize,
    cap: u64,
    from: u64,
    until: u64,
    seg_end: u64,
) -> Result<(), StoreError> {
    let header = HEADER_SIZE as u64;
    let end = until.min(seg_end);
    let mut bytes = Vec::new();
    let mut p = from;
    while p.saturating_add(header) < end {
        let len = (seg_end - p - header).min(MAX_PAYLOAD_LEN as u64);
        let len = u32::try_from(len).unwrap_or(u32::MAX);
        let mut h = [0u8; HEADER_SIZE];
        h[0] = MAGIC[0];
        h[1] = MAGIC[1];
        h[2] = VERSION;
        h[4..8].copy_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&h);
        p += header;
    }
    if bytes.is_empty() {
        return Ok(());
    }
    if cap == 0 {
        return Err(StoreError::OutOfBounds {
            at: hist_off,
            len: bytes.len(),
        });
    }
    store.write(ring_offset(hist_off, cap, from), &bytes)
}

/// What [`lookalike_tail`] did to a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Planted {
    /// The newest history chunk's payload byte at ring position `damaged` flipped, ring positions
    /// `from..to` filled with look-alikes, and the ring's commit (`hist_hi`) moved from `from` to
    /// `to`.
    Tail {
        /// The flipped byte's ring position.
        damaged: u64,
        /// The ring's commit before the plant.
        from: u64,
        /// The ring's commit after it.
        to: u64,
    },
    /// The plant did not happen: why. Every refusal but `store` comes before the first write; a
    /// `store` refusal (the store refused a write) can leave what was written before it.
    Refused(&'static str),
}

/// `benches/resync_walk.rs`'s `lookalike-tail` case in a region the real writer built: the
/// newest history chunk damaged (one payload byte flipped), then up to `kib` KiB past the ring's
/// commit filled with look-alikes ([`lookalikes`]), as many as fit before the ring wraps, and the
/// commit moved past them. A ring that is empty, already wraps, has no room left in its lap or
/// whose words do not read is left untouched ([`Planted::Refused`]).
pub fn lookalike_tail<S: RegionStore + ?Sized>(store: &S, kib: usize) -> Planted {
    let Ok(header) = read_header(store) else {
        return Planted::Refused("header");
    };
    let hist_off = header.geometry.hist_off;
    let word = |at| store.load_u64_acquire(at).ok();
    let (Some(lo), Some(hi), Some(lo_seq), Some(cap)) = (
        word(OFF_HIST_LO),
        word(OFF_HIST_HI),
        word(OFF_HIST_LO_SEQ),
        word(OFF_HIST_CAP),
    ) else {
        return Planted::Refused("words");
    };
    if cap == 0 || cap > header.geometry.hist_cap_phys as u64 {
        return Planted::Refused("words");
    }
    if lo >= hi {
        return Planted::Refused("empty ring");
    }
    let Some(lap_end) = (lo - lo % cap).checked_add(cap) else {
        return Planted::Refused("words");
    };
    if hi > lap_end {
        return Planted::Refused("ring wraps");
    }
    let to = hi
        .saturating_add((kib as u64).saturating_mul(1024))
        .min(lap_end);
    if to - hi <= HEADER_SIZE as u64 {
        return Planted::Refused("no room in the lap");
    }
    let Ok(len) = usize::try_from(hi - lo) else {
        return Planted::Refused("words");
    };
    let mut ring = vec![0u8; len];
    if store
        .read(ring_offset(hist_off, cap, lo), &mut ring)
        .is_err()
    {
        return Planted::Refused("words");
    }
    let scan = scan_steps_from(&ring, lo_seq);
    let Some((_, body)) = scan.steps.last().filter(|(_, body)| !body.is_empty()) else {
        return Planted::Refused("no chunk");
    };
    let damaged = lo + (body.start + body.len() / 2) as u64;
    let planted = flip(store, hist_off, cap, damaged)
        .and_then(|()| lookalikes(store, hist_off, cap, hi, to, to))
        .and_then(|()| store.store_u64_release(OFF_HIST_HI, to));
    match planted {
        Ok(()) => Planted::Tail {
            damaged,
            from: hi,
            to,
        },
        Err(_) => Planted::Refused("store"),
    }
}

#[cfg(test)]
#[path = "plant_tests.rs"]
mod tests;
