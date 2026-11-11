// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;

use super::*;
use crate::recover::{RecoverPolicy, Recovery, RingFault, recover};
use crate::store::{RegionView, VecStore};
use crate::writer::{JournalWriter, KeyframeInput, WriterIdentity};
use crate::{Geometry, HistoryIdentity};

const CONFIG: u64 = 0x5EED;

/// A v1 region the real writer built: 300 lines of history in its ring, then a keyframe.
fn region() -> VecStore {
    let geometry = Geometry::v1();
    let mut writer = JournalWriter::create(
        VecStore::new(geometry.region_len),
        geometry,
        &WriterIdentity {
            pid: 7,
            birth_us: 1,
            local_id: 2,
            build: 105,
            sid: "s-plant",
        },
    )
    .expect("created");
    let mut term = Terminal::new(24, 80);
    for i in 0..300u64 {
        term.process(format!("history line {i}\r\n").as_bytes());
    }
    let fence = term.history_fence();
    writer
        .reset_history(
            HistoryIdentity::of(&fence),
            fence.oldest,
            geometry.hist_cap_phys,
            false,
        )
        .expect("reset");
    let mut row = fence.oldest;
    while row < fence.end {
        let lines = term
            .history_lines_since_fence(&fence, row, 64)
            .expect("holds");
        writer.append_history(row, &lines).expect("fits");
        row += lines.len() as u64;
    }
    let checkpoint = term.checkpoint_journal(512).expect("ground");
    writer
        .write_keyframe(&KeyframeInput {
            checkpoint: &checkpoint,
            fence: term.history_fence(),
            resize_lifetime: term.resize_lifetime(),
            resize_ordinal: term.resize_ordinal(),
            bridge_lines: 512,
            config_fp: CONFIG,
            integration_on: false,
        })
        .expect("keyframe");
    writer.store().snapshot()
}

fn ring(view: &VecStore) -> (Option<RingFault>, usize) {
    match recover(
        view,
        RecoverPolicy {
            tail: true,
            config_fp: CONFIG,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ) {
        Recovery::Restored(r) => (r.ring.fault, r.ring.chunks.len()),
        Recovery::Meta(why) => panic!("the region restores: {why:?}"),
    }
}

/// The plant is the bench's damage in a live region: the newest chunk is lost to a
/// `CorruptFrame` and every older one kept, the commit moved past the look-alikes, capped at
/// the ring's lap. Negative controls: the unplanted region's ring is clean with every chunk;
/// a region with no history is refused and left byte for byte as it was.
#[test]
fn a_planted_tail_loses_the_newest_chunk_and_keeps_the_rest() {
    let clean = region();
    let (fault, chunks) = ring(&clean);
    assert_eq!(fault, None, "premise: the unplanted ring is clean");
    assert!(chunks >= 2, "premise: chunks to keep: {chunks}");
    let planted = clean.snapshot();
    let Planted::Tail { damaged, from, to } = lookalike_tail(&planted, 64) else {
        panic!("planted");
    };
    assert!(
        damaged < from && to == from + 64 * 1024,
        "{damaged} {from} {to}"
    );
    let (fault, kept) = ring(&planted);
    assert!(
        matches!(fault, Some(RingFault::Chunk(_))),
        "the damaged chunk ends the walk: {fault:?}"
    );
    assert_eq!(kept, chunks - 1, "every chunk before the newest is kept");
    let huge = clean.snapshot();
    let Planted::Tail { to, .. } = lookalike_tail(&huge, usize::MAX / 2048) else {
        panic!("planted");
    };
    let cap = Geometry::v1().hist_cap_phys as u64;
    assert_eq!(to % cap, 0, "capped at the end of the ring's lap: {to}");

    let empty = VecStore::new(Geometry::v1().region_len);
    assert_eq!(lookalike_tail(&empty, 64), Planted::Refused("header"));
    let geometry = Geometry::v1();
    let bare = JournalWriter::create(
        VecStore::new(geometry.region_len),
        geometry,
        &WriterIdentity {
            pid: 7,
            birth_us: 1,
            local_id: 2,
            build: 105,
            sid: "s-bare",
        },
    )
    .expect("created")
    .store()
    .snapshot();
    let before = bare.snapshot();
    assert_eq!(lookalike_tail(&bare, 64), Planted::Refused("empty ring"));
    let mut a = vec![0u8; geometry.region_len];
    let mut b = vec![0u8; geometry.region_len];
    bare.read(0, &mut a).expect("read");
    before.read(0, &mut b).expect("read");
    assert!(a == b, "a refused plant writes nothing");
}

#[test]
fn overflowing_logical_ring_positions_refuse_before_any_write() {
    for cap in [1, 2] {
        let view = region();
        view.store_u64_release(OFF_HIST_CAP, cap)
            .expect("capacity word");
        view.store_u64_release(OFF_HIST_LO, u64::MAX - 1)
            .expect("low word");
        view.store_u64_release(OFF_HIST_HI, u64::MAX)
            .expect("high word");
        let before = view.snapshot();
        assert!(matches!(lookalike_tail(&view, 64), Planted::Refused(_)));
        let mut original = vec![0; view.len()];
        let mut after = vec![0; view.len()];
        before.read(0, &mut original).expect("snapshot");
        view.read(0, &mut after).expect("region");
        assert_eq!(after, original, "cap={cap}");
    }
}

#[test]
fn plant_primitives_handle_zero_capacity_and_max_positions() {
    let store = VecStore::new(64);
    assert_eq!(ring_offset(16, 0, u64::MAX), 16);
    assert_eq!(ring_offset(usize::MAX, 8, 5), usize::MAX);
    assert_eq!(
        flip(&store, 16, 0, 0),
        Err(StoreError::OutOfBounds { at: 16, len: 1 })
    );
    assert_eq!(
        lookalikes(&store, 16, 64, u64::MAX, u64::MAX, u64::MAX),
        Ok(())
    );
    assert_eq!(
        lookalikes(
            &store,
            16,
            0,
            0,
            HEADER_SIZE as u64 + 1,
            HEADER_SIZE as u64 + 1
        ),
        Err(StoreError::OutOfBounds {
            at: 16,
            len: HEADER_SIZE
        })
    );
}
