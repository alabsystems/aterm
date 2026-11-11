// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A tall bar's caps are its key's: the art a bar of any height past
/// `w + 2` draws is the one computed at `w + 2`, top and bottom.
#[test]
fn a_tall_bars_caps_are_its_keys_caps() {
    for w in 1..=16u32 {
        let key = cap_key(w, 400).1;
        let (_, cap) = radius_and_cap(w, key);
        for h in [key, key + 1, key + 7, 3 * key + 5, 400] {
            assert_eq!(radius_and_cap(w, h).1, cap, "w{w} h{h}: the cap's rows");
            for py in 0..cap {
                for px in 0..w {
                    assert_eq!(hits(w, h, px, py), hits(w, key, px, py), "top");
                    assert_eq!(
                        hits(w, h, px, h - cap + py),
                        hits(w, key, px, key - cap + py),
                        "w{w} h{h}: bottom"
                    );
                }
            }
        }
    }
}

/// The art holds the coverage counts and nothing past a full pixel; the
/// caps of a bar as wide as tall meet with no body between them.
#[test]
fn a_cap_art_is_its_coverage() {
    let art = cap_art(6, 60).expect("an art");
    assert_eq!((art.w(), art.h()), (6, 6));
    assert!(art.max_index() <= 16 && art.max_index() > 0);
    assert_eq!(art.texel(0, 0), 0, "a corner is shaved");
    assert_eq!(art.texel(2, 2), 16, "the cap's heart is solid");
    let mut items = Vec::new();
    let mut palette = Vec::new();
    let bar = RoundedBar {
        x: 0,
        y: 0,
        w: 6,
        h: 6,
        rgb: [9; 3],
        alpha: 200,
    };
    push_items(bar, &mut CapArts::default(), &mut items, &mut palette);
    assert_eq!(palette.len(), BAR_INKS);
    assert!(
        items.iter().all(|i| matches!(i, FxItem::Sprite(_))),
        "6 × 6: two caps, no body"
    );
}
