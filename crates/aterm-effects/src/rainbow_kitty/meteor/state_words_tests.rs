// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::super::Dir;
use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::{Duration, Instant};

fn words(m: &Meteors, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    m.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_meteor_field_moves_the_meteor_words() {
    let t0 = Instant::now();
    let mut pool = Meteors::new();
    pool.live.push(Meteor {
        x0: 10.0,
        y0: 20.0,
        x1: 200.0,
        y1: 20.0,
        t0: t0 + Duration::from_millis(4),
        t_flight: Duration::from_millis(180),
        w: 6.0,
        grade: 0.5,
        retired_at: None,
        chained: false,
        t_land: 0.3,
        band: None,
        cells: 12.0,
        dir: Dir::Right,
        seed: 9,
        shed: 0,
        landed: false,
        ring: true,
    });
    let base = words(&pool, t0);
    assert_eq!(base, words(&pool.clone(), t0));

    let mut shed = pool.clone();
    shed.live[0].shed |= 1;
    assert_ne!(
        words(&shed, t0),
        base,
        "a meteor's shed stations are hashed"
    );
    let mut retired = pool.clone();
    retired.live[0].retired_at = Some(t0 + Duration::from_millis(50));
    assert_ne!(
        words(&retired, t0),
        base,
        "a meteor's retire clock is hashed"
    );
    let mut ordered = pool.clone();
    ordered.ord += 1;
    assert_ne!(
        words(&ordered, t0),
        base,
        "the pool's order clock is hashed"
    );
}
