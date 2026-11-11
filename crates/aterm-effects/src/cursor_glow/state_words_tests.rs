// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::Duration;

fn words(g: &CursorGlow, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    g.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_glow_custody_field_moves_the_glow_words() {
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    glow.note_typed_cells(t0 + Duration::from_millis(2), 1);
    let base = words(&glow, t0);
    assert_eq!(
        base,
        words(&glow, t0),
        "the words are a function of the state"
    );

    glow.nav_hint = Some(t0 + Duration::from_millis(5));
    let hinted = words(&glow, t0);
    assert_ne!(hinted, base, "a type or gesture hint is hashed");
    glow.nav_hint = None;
    assert_eq!(words(&glow, t0), base);

    glow.recent_typed_run = Some(RecentTypedRun {
        row: 1,
        col0: 2,
        len: 1,
        glyphs: [' '; super::super::custody::RECENT_TYPED_RUN_CAP],
        at: t0,
        origin: None,
    });
    let run = words(&glow, t0);
    assert_ne!(run, base, "the recent typed run is hashed");
    if let Some(r) = glow.recent_typed_run.as_mut() {
        r.glyphs[0] = 'x';
    }
    assert_ne!(words(&glow, t0), run, "the run's glyphs are hashed");
    if let Some(r) = glow.recent_typed_run.as_mut() {
        r.glyphs[0] = ' ';
        r.glyphs[5] = 'x';
    }
    assert_eq!(
        words(&glow, t0),
        run,
        "glyphs past the run's length are not"
    );
    glow.recent_typed_run = None;

    glow.last_cfg = Some(GlowConfig::sample_for_test());
    let configured = words(&glow, t0);
    assert_ne!(configured, base, "the stored config is hashed");
    if let Some(cfg) = glow.last_cfg.as_mut() {
        cfg.theme_bg ^= 1;
    }
    assert_ne!(words(&glow, t0), configured, "through every field");
    glow.last_cfg = None;

    glow.under_out.push(aterm_render::GlowQuad::default());
    assert_eq!(words(&glow, t0), base, "the frame's output is not state");
}
