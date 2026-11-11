// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::rom::rasterize_master;
use super::*;

fn baker() -> RainBaker {
    let rom = rasterize_master();
    let mut b = RainBaker::default();
    b.begin_frame(8, 16);
    while !b.complete() {
        b.bake_tiles(&rom);
    }
    b
}

const fn cell(row: u16, col: u16, glyph: u8) -> RainCell {
    RainCell {
        row,
        x: col * 8,
        y: row * 16,
        w: 8,
        h: 16,
        glyph,
        tint: 0x0022_CC44,
        alpha: 90,
        flip_x: false,
    }
}

#[test]
fn one_layer_a_row_and_an_unchanged_row_is_an_unchanged_layer() {
    let tiles = baker();
    let mut rain = RainLayers::default();
    let mut a = Vec::new();
    let cells = [cell(1, 0, 3), cell(1, 2, 4), cell(4, 1, 5)];
    rain.publish(&cells, &tiles, &mut a);
    assert_eq!(a.len(), 2, "rows 1 and 4");
    assert_eq!(a[0].content.items().len(), 2);
    assert_eq!(a[0].content.palette().len(), 1, "one ink, deduplicated");
    let mut b = a.clone();
    rain.publish(
        &[cell(1, 0, 3), cell(1, 2, 4), cell(4, 1, 6)],
        &tiles,
        &mut b,
    );
    assert_eq!(a[0], b[0], "row 1 kept its cells: the same layer");
    assert_ne!(a[1], b[1], "row 4 changed glyph: a new layer");
}

#[test]
fn the_rain_sits_under_every_other_linear_under_layer() {
    let tiles = baker();
    let mut rain = RainLayers::default();
    let mut pet = aterm_render::sprites::SpriteLayers::new(9);
    let art = tiles.tile(0).expect("baked").clone();
    let body = aterm_render::sprites::FreeSprite {
        z: aterm_render::sprites::FreeZ::UnderText,
        ..aterm_render::sprites::FreeSprite::whole(0, 0, art)
    };
    let mut layers = Vec::new();
    pet.publish(std::slice::from_ref(&body), &mut layers);
    rain.publish(&[cell(0, 0, 1)], &tiles, &mut layers);
    assert!(layers[0].id == rain.ids()[0] && layers[1].id == pet.ids()[0]);
    assert!(
        is_rain_row(&layers[0]) && !is_rain_row(&layers[1]),
        "the pet knocks: no rain row"
    );
    assert!(
        !aterm_render::sprites::is_sprite_layer(&layers[0])
            && aterm_render::sprites::is_sprite_layer(&layers[1]),
        "a rain row is no companion sprite"
    );
    rain.publish(&[cell(0, 0, 2)], &tiles, &mut layers);
    assert!(
        layers[0].id == rain.ids()[0],
        "a republish stays under the pet"
    );
    pet.publish(std::slice::from_ref(&body), &mut layers);
    assert!(layers[0].id == rain.ids()[0], "…in either publish order");
    rain.withdraw(&mut layers);
    assert_eq!(layers.len(), 1);
}

#[test]
fn an_unbaked_tile_draws_nothing_and_a_rebake_rebuilds_the_row() {
    let rom = rasterize_master();
    let mut tiles = RainBaker::default();
    tiles.begin_frame(8, 16);
    tiles.bake_tiles(&rom);
    let mut rain = RainLayers::default();
    let mut a = Vec::new();
    rain.publish(&[cell(0, 0, 1), cell(0, 1, 63)], &tiles, &mut a);
    assert_eq!(a[0].content.items().len(), 1, "glyph 63 is not baked yet");
    while !tiles.complete() {
        tiles.bake_tiles(&rom);
    }
    let mut b = a.clone();
    rain.publish(&[cell(0, 0, 1), cell(0, 1, 63)], &tiles, &mut b);
    assert_eq!(
        b[0].content.items().len(),
        2,
        "a new tile set rebuilds the row"
    );
}
