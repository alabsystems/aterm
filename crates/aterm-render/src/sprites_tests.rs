// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::render::FxTexels;

use super::*;

fn art(seed: u8) -> Arc<FxArt> {
    crate::fx::bitmap(FxTexels::Rgba8Straight, 3, 2, vec![seed; 3 * 2 * 4]).expect("art")
}

fn sprite(x: i32, seed: u8, z: FreeZ) -> FreeSprite {
    FreeSprite {
        z,
        tint: 0x0080_4020,
        alpha: 200,
        ..FreeSprite::whole(x, 5, art(seed))
    }
}

#[test]
fn a_sprite_round_trips_through_its_layer() {
    let mut pubr = SpriteLayers::new(7);
    let mut layers = Vec::new();
    let mut s = sprite(4, 1, FreeZ::UnderText);
    s.clip = PxRect::new(0, 0, 6, 9);
    s.flip_x = true;
    let o = sprite(9, 2, FreeZ::OverText);
    pubr.publish(&[s.clone(), o.clone()], &mut layers);
    assert_eq!(layers.len(), 2);
    assert!(layers[0].knock && layers[0].slot == FxSlot::Under);
    assert!(!layers[1].knock && layers[1].slot == FxSlot::Over);
    assert!(layers.iter().all(is_sprite_layer));
    assert_eq!(published(&layers), vec![s, o]);
}

#[test]
fn an_unchanged_sprite_is_an_unchanged_layer_and_a_move_keeps_the_content() {
    let mut pubr = SpriteLayers::new(7);
    let mut a = Vec::new();
    let s = sprite(4, 1, FreeZ::UnderText);
    pubr.publish(std::slice::from_ref(&s), &mut a);
    let mut b = a.clone();
    pubr.publish(std::slice::from_ref(&s), &mut b);
    assert_eq!(a, b, "identity equality: the same layer");
    let mut c = b.clone();
    pubr.publish(&[FreeSprite { x: 5, ..s.clone() }], &mut c);
    assert_ne!(b, c, "a move is a changed layer");
    assert!(
        Arc::ptr_eq(&b[0].content, &c[0].content),
        "…over the same content"
    );
    let mut d = c.clone();
    pubr.publish(&[FreeSprite { alpha: 9, ..s }], &mut d);
    assert!(
        !Arc::ptr_eq(&c[0].content, &d[0].content),
        "a new look is new content"
    );
}

#[test]
fn content_is_shared_only_across_one_whole_look() {
    let mut pubr = SpriteLayers::new(7);
    let s = sprite(4, 1, FreeZ::OverText);
    for (what, changed) in [
        (
            "a mirror",
            FreeSprite {
                flip_x: true,
                ..s.clone()
            },
        ),
        (
            "a tint",
            FreeSprite {
                tint: 0x0010_2030,
                ..s.clone()
            },
        ),
        ("a width", FreeSprite { w: 6, ..s.clone() }),
        ("a height", FreeSprite { h: 4, ..s.clone() }),
    ] {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        pubr.publish(std::slice::from_ref(&s), &mut a);
        pubr.publish(std::slice::from_ref(&changed), &mut b);
        assert!(
            !Arc::ptr_eq(&a[0].content, &b[0].content),
            "{what} of the same art is new content"
        );
        assert_eq!(published(&b), vec![changed], "{what} draws as itself");
    }
}

#[test]
fn a_sprites_layer_id_is_its_look_not_its_place_in_the_order() {
    let mut pubr = SpriteLayers::new(7);
    let (a, b) = (
        sprite(0, 1, FreeZ::OverText),
        sprite(9, 2, FreeZ::UnderText),
    );
    let mut both = Vec::new();
    pubr.publish(&[a.clone(), b.clone()], &mut both);
    let mut alone = Vec::new();
    pubr.publish(std::slice::from_ref(&b), &mut alone);
    assert_eq!(
        alone[0], both[1],
        "b is the same layer with a gone from before it"
    );
    let mut moved = Vec::new();
    pubr.publish(&[FreeSprite { x: 3, ..b }], &mut moved);
    assert_eq!(moved[0].id, alone[0].id, "a move keeps the id");
    assert_ne!(both[0].id, both[1].id, "two looks, two ids");
}

#[test]
fn a_publish_replaces_only_its_own_layers() {
    let mut words = SpriteLayers::new(1);
    let mut other = SpriteLayers::new(2);
    let mut layers = Vec::new();
    words.publish(&[sprite(0, 1, FreeZ::OverText)], &mut layers);
    other.publish(&[sprite(1, 2, FreeZ::OverText)], &mut layers);
    words.publish(&[], &mut layers);
    assert_eq!(published(&layers).len(), 1);
    assert_eq!(
        published(&layers)[0].x,
        1,
        "the other publisher's sprite stays"
    );
    let blank = FreeSprite {
        alpha: 0,
        ..sprite(0, 1, FreeZ::OverText)
    };
    let hidden = FreeSprite {
        clip: PxRect::new(50, 50, 60, 60),
        ..sprite(0, 1, FreeZ::OverText)
    };
    words.publish(&[blank, hidden], &mut layers);
    assert_eq!(
        published(&layers).len(),
        1,
        "a blank sprite publishes no layer"
    );
    other.withdraw(&mut layers);
    assert!(layers.is_empty());
}
