// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::RenderInput;
use aterm_grid::{ImageData, ImageFormat, ImageRef, ImageScaling, KittyPlacementTag};
use std::num::NonZeroU32;
use std::sync::Arc;

#[test]
fn images_eq_memo_matches_derived_equality() {
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8).collect();
    let mk = |bytes: Vec<u8>, z: i32| {
        Arc::new(ImageData {
            bytes,
            format: ImageFormat::Png,
            cols: 3,
            rows: 2,
            z_index: z,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        })
    };
    let fill = |img: &Arc<ImageData>| -> Vec<Vec<(usize, ImageRef)>> {
        (0..2u16)
            .map(|r| {
                (0..3u16)
                    .map(|c| {
                        (
                            usize::from(c),
                            ImageRef {
                                image: img.clone(),
                                cell_row: r,
                                cell_col: c,
                                kitty: None,
                            },
                        )
                    })
                    .collect()
            })
            .collect()
    };
    let base = mk(payload.clone(), 0);
    let a = fill(&base);
    let mut flipped = payload.clone();
    flipped[17] ^= 0x80;
    let tagged = |serial: u32| {
        let mut t = a.clone();
        for (_, iref) in t.iter_mut().flatten() {
            iref.kitty = Some(KittyPlacementTag {
                image_id: 1,
                placement_id: 0,
                image_serial: NonZeroU32::MIN,
                serial: NonZeroU32::new(serial).unwrap(),
            });
        }
        t
    };
    let untagged = |rows: &[Vec<(usize, ImageRef)>]| {
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|(c, iref)| {
                        (
                            *c,
                            ImageRef {
                                kitty: None,
                                ..iref.clone()
                            },
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let cases = [
        Vec::new(),
        a.clone(),
        fill(&mk(payload.clone(), 0)),
        fill(&mk(flipped, 0)),
        fill(&mk(payload.clone(), -1)),
        tagged(7),
        a[..1].to_vec(),
    ];
    for (i, b) in cases.iter().enumerate() {
        assert_eq!(
            super::images_eq(&a, b),
            untagged(&a) == untagged(b),
            "case {i}: memoized equality must match the derived equality"
        );
    }
    assert!(super::images_eq(&tagged(7), &tagged(8)));
    assert_ne!(tagged(7), tagged(8), "the tag is part of derived equality");

    let mut left = RenderInput::empty();
    left.images = a.clone();
    let mut right = RenderInput::empty();
    right.images = fill(&mk(payload.clone(), 0));
    assert_eq!(
        left, right,
        "a byte-identical re-transmit (fresh Arcs) must compare EQUAL"
    );
    let mut changed = payload;
    changed[4095] ^= 1;
    let mut unequal = RenderInput::empty();
    unequal.images = fill(&mk(changed, 0));
    assert_ne!(
        left, unequal,
        "a last-byte payload change must still compare UNEQUAL"
    );
}

fn image_ref(z: i32) -> ImageRef {
    ImageRef {
        image: Arc::new(ImageData {
            bytes: Vec::new(),
            format: ImageFormat::Png,
            cols: 1,
            rows: 1,
            z_index: z,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        }),
        cell_row: 0,
        cell_col: 0,
        kitty: None,
    }
}

#[test]
fn image_hides_glyph_only_when_z_is_nonnegative() {
    let mut input = RenderInput::empty();
    input.images = vec![vec![
        (0, image_ref(0)),
        (1, image_ref(-1)),
        (2, image_ref(5)),
    ]];
    assert!(
        input.image_hides_glyph_at(0, 0),
        "z=0 image hides the glyph"
    );
    assert!(
        !input.image_hides_glyph_at(0, 1),
        "z<0 image draws BEHIND text — glyph still paints"
    );
    assert!(
        input.image_hides_glyph_at(0, 2),
        "z>0 image hides the glyph"
    );
    assert!(
        !input.image_hides_glyph_at(0, 3),
        "no image at the column — nothing hides the glyph"
    );
}

#[test]
fn per_column_accessors_binary_search_sparse_sorted_lists() {
    let mut input = RenderInput::empty();
    input.clusters = vec![vec![
        (0, "a".into()),
        (5, "flag".into()),
        (100, "zwj".into()),
    ]];
    input.combining = vec![vec![
        (0, vec!['\u{0301}'].into()),
        (5, vec!['\u{0308}'].into()),
        (100, vec!['\u{0327}'].into()),
    ]];
    input.images = vec![vec![(5, image_ref(0)), (100, image_ref(2))]];

    assert_eq!(input.cluster_at(0, 0), Some("a"));
    assert_eq!(input.cluster_at(0, 5), Some("flag"));
    assert_eq!(input.cluster_at(0, 100), Some("zwj"));
    assert_eq!(input.cluster_at(0, 4), None, "gap column has no cluster");
    assert_eq!(input.cluster_at(0, 101), None, "past-the-end column");

    assert_eq!(input.combining_at(0, 5), Some(['\u{0308}'].as_slice()));
    assert_eq!(input.combining_at(0, 100), Some(['\u{0327}'].as_slice()));
    assert_eq!(input.combining_at(0, 50), None, "gap column has no marks");

    assert_eq!(input.image_at(0, 5).map(|r| r.image.z_index), Some(0));
    assert_eq!(input.image_at(0, 100).map(|r| r.image.z_index), Some(2));
    assert!(input.image_at(0, 0).is_none(), "col 0 has no image");
    assert!(input.image_hides_glyph_at(0, 100), "z=2 image hides glyph");
}
