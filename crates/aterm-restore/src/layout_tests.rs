// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The v1 numbers are the plan's table, and they validate.
#[test]
fn the_v1_layout_is_the_plans_table() {
    let g = Geometry::v1();
    assert_eq!(g.validate(), Ok(()));
    assert_eq!(REGION_LEN % PAGE, 0, "a page multiple");
    assert_eq!(g.kf_off, 4_096);
    assert_eq!(g.tail_off, 790_528);
    assert_eq!(g.ctl_off(), 1_052_672);
    assert_eq!(g.ctl_cap(), 64 * 1024);
    assert_eq!(g.hist_off, 1_118_208);
    assert_eq!(g.hist_cap_phys, REGION_LEN - 1_118_208);
    for word in [
        OFF_MAGIC,
        OFF_REGION_LEN,
        OFF_WRITER_PID,
        OFF_WRITER_BIRTH,
        OFF_LOCAL_ID,
        OFF_WRITER_BUILD,
        OFF_SID,
        OFF_KF_OFF,
        OFF_KF_CAP,
        OFF_TAIL_OFF,
        OFF_TAIL_CAP,
        OFF_HIST_OFF,
        OFF_HIST_CAP_PHYS,
        OFF_HIST_CAP,
        OFF_CONFIG_FP,
        OFF_ENGINE_RESIZE_LIFETIME,
        OFF_ENGINE_RESIZE_ORDINAL,
        OFF_CELL_PX,
        kf_commit_word(0),
        kf_commit_word(1),
        kf_len_word(0),
        kf_len_word(1),
        OFF_TAIL_COMMIT,
        OFF_TAIL_FLOOR,
        OFF_STREAM_PROCESSED,
        OFF_TAIL_MARKS,
        OFF_STREAM_GATHERED,
        OFF_HIST_LO,
        OFF_HIST_HI,
        OFF_HIST_LO_SEQ,
        OFF_HIST_END_ROW,
        OFF_HIST_IDENTITY + 24,
        OFF_CTL_COMMIT,
        OFF_CTL_LEN,
        OFF_CTL_COUNT,
    ] {
        assert_eq!(word % 8, 0, "{word} is aligned");
        assert!(word + 8 <= HEADER_LEN, "{word} is inside the header");
    }
    assert_eq!(OFF_SID + SID_LEN, OFF_KF_OFF);
    assert_ne!(OFF_TAIL_COMMIT / 64, OFF_STREAM_GATHERED / 64);
    assert_ne!(OFF_STREAM_GATHERED / 64, OFF_HIST_HI / 64);
    assert_ne!(OFF_TAIL_COMMIT / 64, OFF_HIST_HI / 64);
    assert_ne!(OFF_HIST_HI / 64, OFF_CTL_COMMIT / 64);
    assert_ne!(OFF_CTL_COMMIT / 64, OFF_CTL_COUNT / 64);
}

/// A cell size round-trips through its word; none, a zero axis and bits this build
/// never writes read as none.
#[test]
fn a_cell_size_round_trips_through_its_word() {
    for px in [(1, 1), (9, 18), (18, 36), (u16::MAX, u16::MAX)] {
        assert_eq!(cell_px_of(cell_px_word(Some(px))), Some(px), "{px:?}");
    }
    assert_eq!(cell_px_word(None), 0);
    assert_eq!(cell_px_word(Some((0, 18))), 0);
    assert_eq!(cell_px_word(Some((9, 0))), 0);
    assert_eq!(cell_px_of(0), None);
    assert_eq!(cell_px_of(9 << 16), None, "a zero height");
    assert_eq!(cell_px_of(18), None, "a zero width");
    assert_eq!(cell_px_of(1 << 32 | 9 << 16 | 18), None, "high bits");
}

/// A header naming sections that overlap, run past the region or are not
/// aligned is refused.
#[test]
fn a_geometry_that_does_not_fit_is_refused() {
    let v1 = Geometry::v1();
    let cases = [
        (
            Geometry {
                region_len: REGION_LEN + 1,
                ..v1
            },
            LayoutError::RegionLen(REGION_LEN + 1),
        ),
        (
            Geometry {
                region_len: MAX_REGION_LEN + PAGE,
                ..v1
            },
            LayoutError::RegionLen(MAX_REGION_LEN + PAGE),
        ),
        (Geometry { kf_off: 8, ..v1 }, LayoutError::Overlap),
        (
            Geometry {
                tail_off: v1.tail_off - 8,
                ..v1
            },
            LayoutError::Overlap,
        ),
        (
            Geometry {
                hist_cap_phys: v1.hist_cap_phys + 8,
                ..v1
            },
            LayoutError::Overlap,
        ),
        (
            Geometry {
                tail_cap: TAIL_CAP_V1 + 4,
                ..v1
            },
            LayoutError::Misaligned,
        ),
        (Geometry { tail_cap: 16, ..v1 }, LayoutError::TooSmall),
        (
            Geometry {
                hist_off: usize::MAX - 7,
                ..v1
            },
            LayoutError::Overlap,
        ),
    ];
    for (geometry, refusal) in cases {
        assert_eq!(geometry.validate(), Err(refusal), "{geometry:?}");
    }
    let small = Geometry::custom(2048, 48, 4096).expect("a test region");
    assert_eq!(small.region_len, PAGE);
    assert_eq!(small.ctl_cap(), 0);
}
