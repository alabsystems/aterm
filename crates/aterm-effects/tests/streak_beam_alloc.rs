// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact allocation regression for the cursor streak (`CursorGlow::emit_comet`).

use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_render::{BeamClip, CometSample, GlowQuad, comet_glow_quads};

fn allocations_during(f: impl FnOnce()) -> usize {
    crate::alloc_count::allocations_during(f).1
}

fn geom() -> Geom {
    Geom {
        cw: 13,
        ch: 26,
        rows: 8,
        cols: 60,
        origin_x: 0,
        origin_y: 0,
        win_w: 780,
        win_h: 208,
        head: 0,
    }
}

fn cfg(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 1.0,
        audible: false,
        radius: 0.6,
        ring: true,
        beam: true,
        head_dx: 0.5,
    }
}

const FRAMES_PER_KEY: u32 = 7;
const FRAME: Duration = Duration::from_micros(8_333);

struct Typist {
    glow: CursorGlow,
    c: GlowConfig,
    g: Geom,
    out: Vec<GlowQuad>,
    t: Instant,
}

impl Typist {
    fn burst(
        &mut self,
        (row, col): (u16, u16),
        keys: u16,
        between: &mut dyn FnMut(&mut dyn FnMut()),
    ) -> usize {
        let Self { glow, c, g, out, t } = self;
        let mut lit = 0;
        glow.tick(Some((row, col)), *t, c, *g, out);
        for k in 1..=keys {
            *t += Duration::from_millis(60) - FRAME * FRAMES_PER_KEY;
            glow.note_synthetic_typed(*t, 1);
            glow.tick(Some((row, col + k)), *t, c, *g, out);
            between(&mut || {
                for _ in 0..FRAMES_PER_KEY {
                    *t += FRAME;
                    glow.tick(Some((row, col + k)), *t, c, *g, out);
                    lit += usize::from(!out.is_empty());
                }
            });
        }
        lit
    }
}

#[test]
fn a_warm_streak_frame_is_allocation_free() {
    let g = geom();
    let mut report = Vec::new();
    for style in [
        GlowStyle::Phaser,
        GlowStyle::Sparkle,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Comet,
        GlowStyle::Lumen,
    ] {
        let mut typist = Typist {
            glow: CursorGlow::default(),
            c: cfg(style),
            g,
            out: Vec::new(),
            t: Instant::now(),
        };
        typist.burst((2, 2), 24, &mut |f| f());
        typist.t += Duration::from_secs(5);
        let Typist {
            glow, c, out, t, ..
        } = &mut typist;
        glow.tick(Some((2, 26)), *t, c, g, out);
        let mut allocations = 0;
        let lit = typist.burst((2, 2), 24, &mut |f| {
            allocations += allocations_during(&mut *f);
        });
        report.push((style, lit, allocations));
    }
    for &(style, lit, allocations) in &report {
        assert!(
            lit > 100,
            "{style:?}: the counted frames must be live (lit {lit})"
        );
        assert_eq!(allocations, 0, "{style:?}: a warm streak frame allocated");
    }
    let run: Vec<CometSample> = (0..12u16)
        .map(|i| CometSample {
            x: 8.0 + f32::from(i) * 13.0,
            y: 60.0 + f32::from(i % 3),
            cov: 200,
            pos: 1.0,
        })
        .collect();
    let mut quads: Vec<GlowQuad> = Vec::with_capacity(4_096);
    let clip = BeamClip::grid(usize::from(g.win_w), usize::from(g.win_h), g.ch);
    let control = allocations_during(|| {
        comet_glow_quads(&mut quads, clip, &run, 3.4, 20.8, &|_| 0x0050_FA7B);
    });
    assert!(!quads.is_empty(), "the control run must draw");
    assert!(
        control > 0,
        "the allocating wrapper must register, or the zero above is vacuous"
    );
}
