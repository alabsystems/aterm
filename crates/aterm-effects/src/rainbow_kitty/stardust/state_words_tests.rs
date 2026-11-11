// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{StarClass, StarLane};
use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::{Duration, Instant};

fn words(s: &Stardust, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    s.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_stardust_field_moves_the_stardust_words() {
    let t0 = Instant::now();
    let born = t0 + Duration::from_millis(5);
    let mut sky = Stardust::new();
    sky.stars.push(Star {
        class: StarClass::M2,
        lane: StarLane::Strike,
        x: 40.0,
        y: 12.0,
        home_row: Some(3),
        born,
        lit: born,
        seed: 7,
        tint: 0x00FF_00FF,
        gold: false,
        v0: (0.0, -0.003),
        f: 4.0,
        stream: 2.0,
        pull: 0.0,
        finish: None,
        beside_caret: None,
        edge_cut: false,
    });
    sky.minted = 1;
    let base = words(&sky, t0);
    assert_eq!(base, words(&sky.clone(), t0));

    let mut finished = sky.clone();
    finished.stars[0].finish = Some(Finish {
        at: born + Duration::from_millis(40),
        span_s: 0.04,
    });
    assert_ne!(words(&finished, t0), base, "a star's finish is hashed");
    let mut latched = sky.clone();
    latched.stars[0].beside_caret = Some(true);
    assert_ne!(
        words(&latched, t0),
        base,
        "a star's read of the caret beside it is hashed"
    );
    let mut minted = sky.clone();
    minted.minted += 1;
    assert_ne!(words(&minted, t0), base, "the mint is hashed");
    let mut spent = sky.clone();
    spent.budget.empty();
    assert_ne!(words(&spent, t0), base, "the glint bucket is hashed");
    let veil = Veil {
        row: 4,
        col: 9,
        t: 0.3,
        gain: 0.8,
        born,
        lit: born,
        finish: None,
    };
    let mut veiled = sky.clone();
    veiled.veil.push(veil);
    let dealt = words(&veiled, t0);
    assert_ne!(dealt, base, "a dealt veil is hashed");
    let mut relit = veiled.clone();
    relit.veil[0].lit = born + Duration::from_millis(20);
    assert_ne!(words(&relit, t0), dealt, "a veil's last key is hashed");
    let mut retracting = veiled.clone();
    retracting.veil[0].finish_by(born + Duration::from_millis(30), 0.3);
    assert_ne!(words(&retracting, t0), dealt, "a veil's finish is hashed");
    let mut unmarked = sky.clone();
    unmarked.clear_marks();
    unmarked.mark_lights.push(1);
    assert_eq!(
        words(&unmarked, t0),
        base,
        "the painted marks are output, not state"
    );
}
