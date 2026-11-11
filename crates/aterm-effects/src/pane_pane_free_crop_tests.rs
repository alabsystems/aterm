// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::render::{FxTexels, PxRect};

use super::{PanePlace, translate_free_into_pane};
use aterm_render::sprites::FreeSprite;

fn sprite(x: i32, w: u16, flip_x: bool) -> FreeSprite {
    let art =
        aterm_render::fx::bitmap(FxTexels::Rgba8Straight, 8, 4, vec![9; 8 * 4 * 4]).expect("art");
    FreeSprite {
        w,
        flip_x,
        ..FreeSprite::whole(x, 0, art)
    }
}

const fn place(cols: u16) -> PanePlace {
    PanePlace {
        row_off: 1,
        col_off: 4,
        rows: 4,
        cols,
        win_rows: 40,
        win_cols: 40,
        cell_w: 1,
        cell_h: 1,
    }
}

#[test]
fn a_pane_clips_a_sprite_and_keeps_its_art_and_box_whole() {
    for flip in [false, true] {
        let full = sprite(-2, 8, flip);
        let mut v = vec![full.clone()];
        translate_free_into_pane(&mut v, place(6));
        let s = &v[0];
        assert_eq!(
            (s.x, s.y, s.w, s.h),
            (2, 1, 8, 4),
            "flip_x={flip}: shifted, not shrunk"
        );
        assert_eq!(s.art.id(), full.art.id());
        assert_eq!(
            s.visible(),
            PxRect::new(4, 1, 10, 5),
            "flip_x={flip}: cut at the pane's interior edges, so the surviving pixels sample the texels they sampled uncut"
        );
    }
    let mut gone = vec![sprite(-20, 8, false)];
    translate_free_into_pane(&mut gone, place(6));
    assert!(
        gone.is_empty(),
        "a sprite wholly outside its pane is dropped"
    );
}
