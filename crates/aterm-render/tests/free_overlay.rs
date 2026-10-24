// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A companion's sprite layer under identity damage: a settled sprite is the
//! same layer (a frame that only re-presents it compares equal and gate-hits),
//! and a move, a z flip or a re-baked art marks exactly the bands the layer
//! left or reached.

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers};
use aterm_render::{DirtyDecision, compute_dirty_rows};

use crate::sprites::{SALT, patterned, solid, under};

const CELL_H: usize = 16;

fn marked(prev: &RenderInput, cur: &RenderInput) -> (bool, Vec<usize>) {
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        prev,
        cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, CELL_H),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    let rows = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    (d.is_gate_hit(), rows)
}

#[test]
fn render_input_eq_compares_a_sprites_layer_by_identity() {
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25l");
    let art = patterned(8, 8);
    let mut pubr = SpriteLayers::new(SALT);
    let mut frame = |s: &FreeSprite| {
        let mut input = term.cell_frame(3, 12);
        pubr.publish(std::slice::from_ref(s), &mut input.fx_layers);
        input
    };
    let s = under(2, 5, art.clone());
    let a = frame(&s);
    assert_eq!(a, frame(&s), "re-presenting a settled sprite stays EQUAL");
    assert_ne!(
        a,
        frame(&FreeSprite {
            z: FreeZ::OverText,
            ..s.clone()
        }),
        "a same-rect z flip is a real content change"
    );
    let mut rebuilt = term.cell_frame(3, 12);
    crate::sprites::put(&mut rebuilt, std::slice::from_ref(&s));
    assert_ne!(
        a, rebuilt,
        "a rebuilt publisher's content is new: it compares UNEQUAL"
    );
}

#[test]
fn free_dirty_gate_settled_hits_moved_marks_row_union() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let art = patterned(16, 40);
    let settled = under(4, 20, art.clone());
    // For the control below: a rebuilt publisher's frame of the same sprite.
    let mut fresh = term.cell_frame(6, 8);
    crate::sprites::put(&mut fresh, std::slice::from_ref(&settled));
    let mut pubr = SpriteLayers::new(SALT);
    let mut frame = |s: FreeSprite| {
        let mut input = term.cell_frame(6, 8);
        pubr.publish(&[s], &mut input.fx_layers);
        input
    };
    let prev = frame(settled.clone());
    let cur = frame(settled.clone());
    assert_eq!(
        marked(&prev, &cur),
        (true, vec![]),
        "a settled sprite gate-hits and marks no rows: steady state is free"
    );
    let moved = frame(under(4, 36, art.clone()));
    assert_eq!(
        marked(&cur, &moved),
        (false, vec![1, 2, 3, 4]),
        "a moved sprite marks the prev∪cur bands its pixel extent overlaps"
    );
    let back = frame(settled.clone());
    let flipped = frame(FreeSprite {
        z: FreeZ::OverText,
        ..settled.clone()
    });
    assert_eq!(
        marked(&back, &flipped),
        (false, vec![1, 2, 3]),
        "a z flip marks the (unmoved) sprite's own bands"
    );
    let back = frame(settled.clone());
    let rebaked = frame(FreeSprite {
        art: solid([9, 80, 200, 255], 16, 40),
        ..settled.clone()
    });
    assert_eq!(
        marked(&back, &rebaked),
        (false, vec![1, 2, 3]),
        "a re-baked art marks the (unmoved) sprite's bands"
    );
    // Negative control: a rebuilt publisher's layer is new even for the same
    // sprite, so the same picture is no longer a gate hit.
    let back = frame(settled);
    assert!(
        !marked(&back, &fresh).0,
        "CONTROL: identity, not value, is what damage reads"
    );
}

#[test]
fn an_unchanged_sprite_is_not_repainted_when_one_before_it_comes_or_goes() {
    let mut term = Terminal::new(8, 8);
    term.process(b"\x1b[?25l");
    let mut pubr = SpriteLayers::new(SALT);
    let mut frame = |sprites: &[FreeSprite]| {
        let mut input = term.cell_frame(8, 8);
        pubr.publish(sprites, &mut input.fx_layers);
        input
    };
    // A word cat on row 1 drawn before the pet on row 5, as the word pass
    // emits its sprites before the pet's.
    let cat = under(2, 16, solid([200, 90, 20, 255], 8, 8));
    let pet = under(4, 80, patterned(8, 8));
    let both = frame(&[cat.clone(), pet.clone()]);
    let pet_alone = frame(std::slice::from_ref(&pet));
    assert_eq!(
        marked(&both, &pet_alone),
        (false, vec![1]),
        "the cat's leaving repaints its row and not the pet's"
    );
    let again = frame(&[cat.clone(), pet.clone()]);
    assert_eq!(
        marked(&pet_alone, &again),
        (false, vec![1]),
        "the cat's coming back repaints its row and not the pet's"
    );
    // Control: a move of the pet itself repaints its bands.
    let moved = frame(&[cat, under(4, 96, patterned(8, 8))]);
    assert_eq!(marked(&again, &moved), (false, vec![5, 6]));
}
