// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cursor_glow::Geom;
use crate::state_words::{StateWords, Word};
use aterm_time::{Duration, Instant};

fn words(r: &CursorRainbow, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    r.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_rainbow_field_moves_the_rainbow_words() {
    let t0 = Instant::now();
    let mut caret = CursorRainbow::new();
    caret.energy = 0.5;
    caret.last = Some(t0 + Duration::from_millis(16));
    caret.settle_probe = Some(SettleProbe {
        cur: Some((3, 4)),
        energy: 0.5,
        energy_prev: 0.4,
        family: None,
        blink_phase: false,
        dark_theme: true,
        geom: Geom {
            cw: 15,
            ch: 28,
            rows: 20,
            cols: 100,
            origin_x: 0,
            origin_y: 0,
            win_w: 1500,
            win_h: 560,
            head: 0,
        },
        cfg: RainbowConfig {
            enabled: true,
            intensity: 1.0,
            paint: Some(0.5),
            ground: Some(0x0011_1318),
            ..RainbowConfig::default()
        },
        paint_prev: None,
        sample_dt: 0.016,
    });
    let base = words(&caret, t0);
    assert_eq!(base, words(&caret.clone(), t0));

    let mut charged = caret.clone();
    charged.energy = 0.6;
    assert_ne!(words(&charged, t0), base, "the energy latch is hashed");
    let mut probed = caret.clone();
    if let Some(p) = probed.settle_probe.as_mut() {
        p.cfg.ground = Some(0x00F6_F7F9);
    }
    assert_ne!(
        words(&probed, t0),
        base,
        "the settle probe's config is hashed"
    );
    let mut printed = caret.clone();
    printed.fp_last ^= 1;
    assert_eq!(
        words(&printed, t0),
        base,
        "the frame's fingerprint is not state"
    );
}
