// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_cache_keeps_every_pose_on_both_grounds_at_two_scales() {
    assert!(crate::widget::IMAGES_KEPT >= POSES.len() * 4);
    // The working set fits: bake it all, and every pose is still there.
    let mut keys = Vec::new();
    for dark in [false, true] {
        for scale in [1.0, 2.0] {
            for pose in POSES {
                let art = ImageArt::Kitty { pose, dark };
                let (w, h) = device_size(art, 12.0, scale);
                keys.push((art, w, h));
            }
        }
    }
    for key in &keys {
        bake_now(*key);
    }
    for key in &keys {
        assert!(baked(*key), "{key:?} was dropped");
    }
}

#[test]
fn a_pose_waits_for_its_bake_and_the_last_one_stays_meanwhile() {
    // A size nothing else bakes, so neither pose is ready yet.
    let (h, scale) = (13.0, 1.0);
    let stand = PetGlyphId::PetStand;
    let lookup = PetGlyphId::PetSitLookup;
    // Nothing baked: the wanted pose is drawn (the raster bakes it).
    assert_eq!(pose_to_draw(lookup, Some(stand), h, scale), lookup);
    bake_now(key(stand, true, h, scale));
    bake_now(key(stand, false, h, scale));
    // The last pose is ready and the wanted one is not: the last one stays.
    assert_eq!(pose_to_draw(lookup, Some(stand), h, scale), stand);
    // Baked on one ground only, it still waits: the view does not know which one paints.
    bake_now(key(lookup, true, h, scale));
    assert_eq!(pose_to_draw(lookup, Some(stand), h, scale), stand);
    bake_now(key(lookup, false, h, scale));
    assert_eq!(pose_to_draw(lookup, Some(stand), h, scale), lookup);
}

#[test]
fn a_kitty_is_baked_at_whole_device_pixels() {
    for pose in POSES {
        let art = ImageArt::Kitty { pose, dark: true };
        for scale in [1.0_f32, 2.0, 3.0] {
            let (w, h) = device_size(art, 29.0, scale);
            assert_eq!(h as f32, (29.0 * scale).round());
            assert!(w >= 1);
        }
    }
}
