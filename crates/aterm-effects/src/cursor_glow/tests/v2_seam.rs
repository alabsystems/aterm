// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow kitty v2 at the seam.

use super::*;

/// A 20-cell same-row nav move puts the meteor's train (`under`), white heat (`out`) and coma
/// (`halos`) on glass on the tick the caret is observed, with no spark, zoom, starburst, ring,
/// particle or Sweep/Glide cue beside the engine's one meteor cue.
#[test]
fn a_v2_nav_jump_draws_the_meteor_on_the_observed_frame() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    assert!(glow.v2.engaged());
    let jump = t0 + Duration::from_millis(300);
    glow.note_motion(jump);
    let fp = glow.tick(Some((2, 22)), jump, &c, g, &mut out);
    assert_ne!(fp, 0, "the observed frame is on glass");
    assert!(!glow.under_quads().is_empty(), "the train rides `under`");
    assert!(!out.is_empty(), "the white heat rides `out`");
    assert!(!glow.halos().is_empty(), "the coma rides `halos`");
    assert_eq!(glow.live_sparks(), 0, "v1 laid a spark under v2");
    assert!(glow.particles.is_empty(), "v1 laid particles under v2");
    assert!(glow.ring.is_none(), "v1 laid a ring under v2");
    let cues: Vec<_> = glow.drain_sound_cues().map(|c| c.kind).collect();
    assert!(
        cues.iter()
            .any(|k| matches!(k, crate::trail_sound::SoundKind::Meteor { .. })),
        "the meteor gesture is the one sound: {cues:?}"
    );
    assert!(
        !cues.iter().any(|k| matches!(
            k,
            crate::trail_sound::SoundKind::Sweep { .. }
                | crate::trail_sound::SoundKind::Glide { .. }
                | crate::trail_sound::SoundKind::Land
        )),
        "v1's nav cues never speak beside the meteor: {cues:?}"
    );
    assert!(
        glow.needs_frame_cadence(),
        "a live flight needs the frame train"
    );
    assert!(glow.is_active());
    assert!(
        glow.caret_flare_at() == Some(jump),
        "the caret flare fires on the credited spawn's own edge"
    );
    let s = glow.v2_status().expect("v2 owns the frame");
    assert!(s.quads > 0 && s.meteors == 1, "{s:?}");
    assert!(
        matches!(
            glow.take_companion_impulse(),
            Some(CompanionImpulse::Meteor { .. })
        ),
        "the companion takes the meteor impulse"
    );
    assert!(
        glow.take_companion_impulse().is_none(),
        "the take clears it"
    );
}

/// The cell `(row, col)`'s light on this frame: for every pixel column of the
/// cell, the brightest premultiplied channel of any `under` quad over it; the
/// cell's light is the darkest column. `ribbon_beam` emits nothing under one
/// coverage step, so an unlit head end reads `0`.
fn under_light_at(quads: &[GlowQuad], g: Geom, row: u16, col: u16) -> u32 {
    let (cw, ch) = (g.cw as u32, g.ch as u32);
    // The body reaches `1.10 ch` above the row and `0.305 ch` below it.
    let (y0, y1) = (
        (u32::from(row) * ch).saturating_sub(2 * ch),
        (u32::from(row) + 2) * ch,
    );
    (u32::from(col) * cw..(u32::from(col) + 1) * cw)
        .map(|x| {
            quads
                .iter()
                .filter(|q| {
                    u32::from(q.x) <= x
                        && u32::from(q.x) + u32::from(q.w) > x
                        && u32::from(q.y) < y1
                        && u32::from(q.y) + u32::from(q.h) > y0
                })
                .map(|q| {
                    (q.color >> 16 & 0xFF)
                        .max(q.color >> 8 & 0xFF)
                        .max(q.color & 0xFF)
                })
                .max()
                .unwrap_or(0)
        })
        .min()
        .unwrap_or(0)
}

/// `trail status` claims the ribbon it lays: `ribbon_segments=` and `ribbon_hue_bands=` count
/// v2's cells and determine `ribbon_active` (both ≥ 2).
#[test]
fn under_v2_trail_status_claims_the_ribbon_it_lays() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let hand = |glow: &mut CursorGlow| {
        let mut out = Vec::new();
        glow.tick(Some((2, 4)), t0, &c, g, &mut out);
        for k in 1..=8u16 {
            let now = t0 + Duration::from_millis(90 * u64::from(k));
            glow.note_typed_cells(now, 1);
            glow.tick(Some((2, 4 + k)), now, &c, g, &mut out);
        }
        (glow.ribbon_segments(), glow.ribbon_hue_bands())
    };
    let mut v2 = CursorGlow::default();
    let (segments, bands) = hand(&mut v2);
    assert!(v2.v2.engaged());
    assert!(
        segments >= 2 && bands >= 2,
        "v2 lays a ribbon the status row must claim: segments={segments} bands={bands}"
    );
    assert!(
        v2.v2_status().expect("v2 owns the frame").cells >= 2,
        "the claim rides v2's own resident cells"
    );
}

/// The frame that echoes a typed key already shows its ribbon cell: the host stamps the key, the
/// echo lands 0.3 ms later, and the same `tick` that observes the caret's move emits `under`
/// light over the new cell, at least a quarter of the settled cell's.
#[test]
fn the_frame_that_echoes_the_key_already_shows_its_ribbon_cell() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 4)), t0, &c, g, &mut out);
    assert!(glow.v2.engaged());
    // Warm the spine as a live hand does: the cell under test is the fourth of a 90 ms burst, so
    // `birth_disp` is not the cold floor.
    let mut now = t0;
    for k in 1..=3u16 {
        now = t0 + Duration::from_millis(90 * u64::from(k));
        glow.note_typed_cells(now, 1);
        glow.tick(Some((2, 4 + k)), now, &c, g, &mut out);
    }
    // The key, stamped by the host at the key.
    let t_key = now + Duration::from_millis(90);
    glow.note_typed_cells(t_key, 1);
    // The echo: the caret is observed at its landing 0.3 ms later.
    let echo = t_key + Duration::from_micros(300);
    let fp = glow.tick(Some((2, 8)), echo, &c, g, &mut out);
    assert_ne!(fp, 0, "the echo frame is on glass");
    let on_echo = under_light_at(glow.under_quads(), g, 2, 7);
    // The frame after it, one panel period on.
    let next = echo + Duration::from_millis(17);
    glow.tick(Some((2, 8)), next, &c, g, &mut out);
    let on_next = under_light_at(glow.under_quads(), g, 2, 7);
    eprintln!(
        "ribbon cell (2,7): echo tick (+0.3 ms) light={on_echo}, next tick (+17.3 ms) light={on_next}"
    );
    assert!(on_next > 0, "the settled cell is lit at all: {on_next}");
    assert!(
        on_echo > 0 && on_echo * 4 >= on_next,
        "the frame that echoes the key shows its ribbon cell: echo tick light {on_echo} \
         vs next tick {on_next} — the glyph presents without its colour"
    );
}

/// A line still being typed has no dark cell inside its live span. A 60-key sentence at 7 cps
/// (fast words, thinking pauses, one slow word), each echo observed on the tick after its key, at
/// 60 Hz: on every tick while the hand types, every glyph cell from the first key to the caret's
/// neighbour is live. The cohort shares one life, so neither a word typed at a dip nor the oldest
/// cells die under a hand that has not lifted.
#[test]
fn a_line_still_being_typed_has_no_dark_cell_inside_its_live_span() {
    let g = Geom {
        cols: 120,
        win_w: 960,
        ..geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let row = 2u16;
    let col0 = 2u16;
    glow.tick(Some((row, col0)), t0, &c, g, &mut out);
    // `(word, ms per key, thinking pause before the word)` — 60 keys,
    // 8.6 s: 7 cps on average.
    let words: [(&str, u64, u64); 12] = [
        ("what ", 125, 0),
        ("more ", 125, 0),
        ("can ", 125, 0),
        ("we ", 125, 0),
        ("do ", 125, 0),
        ("to ", 125, 0),
        ("make ", 125, 0),
        ("this ", 125, 0),
        ("rainbow ", 110, 0),
        ("theme ", 250, 700),
        ("truly ", 125, 0),
        ("magical", 110, 500),
    ];
    let tick = Duration::from_micros(16_667);
    let mut caret = col0;
    let mut now = t0;
    let mut key_at = t0;
    let mut keys = 0u16;
    let mut holes: Vec<String> = Vec::new();
    let mut echo_pending: Option<(Instant, u16)> = None;
    for (word, per_key_ms, pause_ms) in words {
        key_at += Duration::from_millis(pause_ms);
        for ch in word.chars() {
            key_at += Duration::from_millis(per_key_ms);
            keys += 1;
            while now + tick <= key_at {
                now += tick;
                if let Some((at, to)) = echo_pending
                    && now >= at
                {
                    caret = to;
                    echo_pending = None;
                }
                glow.tick(Some((row, caret)), now, &c, g, &mut out);
                let cols = v2_cols(&glow, row);
                let dark: Vec<u16> = (col0..caret).filter(|c| !cols.contains(c)).collect();
                if !dark.is_empty() && holes.len() < 8 {
                    holes.push(format!(
                        "+{:.3}s keys={keys} caret={caret} dark={dark:?}",
                        now.saturating_duration_since(t0).as_secs_f32()
                    ));
                }
            }
            let class = if ch == ' ' {
                rk::TypedClass::Space
            } else {
                rk::TypedClass::Glyph
            };
            glow.supersede_typed_press();
            glow.note_typed_glyph(key_at, 1, false, class);
            echo_pending = Some((key_at + Duration::from_millis(9), caret + 1));
        }
    }
    assert_eq!(keys, 60);
    assert!(
        holes.is_empty(),
        "a line still being typed has a dark cell inside its live span:\n{}",
        holes.join("\n")
    );
}

/// An engaged v2 with nothing live is exactly zero (fingerprint 0, empty streams, no cadence, no
/// deadline, inactive, status reports nothing drawn), so the loop parks.
#[test]
fn v2_idle_is_exactly_zero() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    assert!(glow.v2.engaged());
    let fp = glow.tick(
        Some((2, 2)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert_eq!(fp, 0);
    assert!(out.is_empty() && glow.under_quads().is_empty() && glow.halos().is_empty());
    assert!(!glow.needs_frame_cadence());
    assert!(
        glow.next_change_deadline(t0 + Duration::from_millis(16), Duration::from_millis(8))
            .is_none()
    );
    assert!(!glow.is_active());
    assert_eq!(glow.caret_flare_at(), None);
    assert_eq!(glow.caret_paint(t0 + Duration::from_millis(16)), 0.0);
    assert_eq!(glow.rainbow_field(), 0.0);
    let s = glow.v2_status().expect("engaged");
    assert_eq!(
        (s.quads, s.halos, s.stars, s.meteors, s.fp),
        (0, 0, 0, 0, 0)
    );
    assert!(glow.drain_sound_cues().next().is_none());
}

/// Every spelling naming the rainbow kitty (the default, the pet, the bare
/// word, `nyan`/`rainbow`, the flying/tall/underline presentations and the
/// `… v2` aliases) parses to `RainbowKitty`, and a fresh engine engages v2 on
/// its first drawing tick. `… v1` is an unknown word that falls back to
/// `Lumen`.
#[test]
fn every_rainbow_kitty_spelling_engages_v2() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    for raw in [
        "rainbow kitty pet",
        "rainbow kitty",
        "kitty",
        "nyan",
        "nyan rainbow",
        "rainbow",
        "rainbow kitty flying",
        "rainbow kitty tall",
        "rainbow kitty underline",
        "rainbow kitty v2",
        "Rainbow Kitty V2",
        "rainbow v2",
        "nyan v2",
        "kitty v2",
    ] {
        assert_eq!(GlowStyle::parse(raw), GlowStyle::RainbowKitty, "{raw}");
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((2, 4)), Instant::now(), &c, g, &mut out);
        assert!(glow.v2.engaged(), "{raw}: a fresh engine draws v2 untold");
        assert!(glow.v2_status().is_some(), "{raw}: the status row is v2's");
    }
    for raw in [
        "rainbow kitty v1",
        "Rainbow Kitty V1",
        " rainbow kitty v1 ",
        "rainbow v1",
        "nyan v1",
        "kitty v1",
        "comet",
        "phaser",
        "v1",
        "v2",
        "pack:kitty",
        "",
    ] {
        assert_ne!(
            GlowStyle::parse(raw),
            GlowStyle::RainbowKitty,
            "{raw}: no spelling names a second rainbow kitty"
        );
    }
    // …and the other nine styles never engage it, told or not.
    for style in BUILTINS
        .iter()
        .filter(|s| !matches!(s, GlowStyle::RainbowKitty))
    {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(
            Some((2, 4)),
            Instant::now(),
            &cfg(*style, true),
            g,
            &mut out,
        );
        assert!(!glow.v2.engaged(), "{style:?} engaged v2");
        assert!(glow.v2_status().is_none());
    }
}
