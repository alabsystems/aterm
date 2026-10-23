// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Rainbow kitty v2 at the seam.

use super::*;

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

fn under_light_at(quads: &[GlowQuad], g: Geom, row: u16, col: u16) -> u32 {
    let (cw, ch) = (g.cw as u32, g.ch as u32);
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

#[test]
fn the_frame_that_echoes_the_key_already_shows_its_ribbon_cell() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 4)), t0, &c, g, &mut out);
    assert!(glow.v2.engaged());
    let mut now = t0;
    for k in 1..=3u16 {
        now = t0 + Duration::from_millis(90 * u64::from(k));
        glow.note_typed_cells(now, 1);
        glow.tick(Some((2, 4 + k)), now, &c, g, &mut out);
    }
    let t_key = now + Duration::from_millis(90);
    glow.note_typed_cells(t_key, 1);
    let echo = t_key + Duration::from_micros(300);
    let fp = glow.tick(Some((2, 8)), echo, &c, g, &mut out);
    assert_ne!(fp, 0, "the echo frame is on glass");
    let on_echo = under_light_at(glow.under_quads(), g, 2, 7);
    let next = echo + Duration::from_millis(17);
    glow.tick(Some((2, 8)), next, &c, g, &mut out);
    let on_next = under_light_at(glow.under_quads(), g, 2, 7);
    assert!(on_next > 0, "the settled cell is lit at all: {on_next}");
    assert!(
        on_echo > 0 && on_echo * 4 >= on_next,
        "the frame that echoes the key shows its ribbon cell: echo tick light {on_echo} \
         vs next tick {on_next} — the glyph presents without its colour"
    );
}

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

#[test]
fn only_the_rainbow_kitty_engages_v2() {
    let g = geom();
    for style in BUILTINS {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((2, 4)), Instant::now(), &cfg(style, true), g, &mut out);
        let kitty = matches!(style, GlowStyle::RainbowKitty);
        assert_eq!(glow.v2.engaged(), kitty, "{style:?}");
        assert_eq!(glow.v2_status().is_some(), kitty, "{style:?}");
    }
}

#[test]
fn reset_and_curtain_clear_kill_retreat_caret_seam_and_v2_sound_cues() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    for is_curtain in [false, true] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        glow.tick(Some((2, 2)), t0, &c, g, &mut out);
        let t2 = t0 + ms(16);
        glow.note_synthetic_move(t2);
        glow.tick(Some((2, 22)), t2, &c, g, &mut out);
        assert!(
            glow.caret_flare_at().is_some(),
            "fixture: meteor jump stamps caret_flare_at"
        );
        let t2b = t2 + ms(220);
        glow.tick(Some((2, 22)), t2b, &c, g, &mut out);
        assert!(
            !glow.under_quads().is_empty() && !glow.halos().is_empty() && !glow.v2_beams.is_empty(),
            "fixture: landed meteor populates under_quads, halos, and v2_beams"
        );
        let t3 = t2b + ms(8);
        glow.note_word_kill(t3, true);
        if is_curtain {
            glow.curtain(t3);
        } else {
            glow.reset();
        }
        assert_eq!(
            glow.caret_flare_at(),
            None,
            "is_curtain={is_curtain}: caret_flare_at must clear at the space seam"
        );
        assert!(
            glow.under_quads().is_empty() && glow.halos().is_empty() && glow.v2_beams.is_empty(),
            "is_curtain={is_curtain}: cached under_quads, halos, and v2_beams must clear before the next tick"
        );
        assert!(
            glow.drain_sound_cues().next().is_none(),
            "is_curtain={is_curtain}: sound ledger must drain v2 cues too"
        );
        let t4 = t3 + ms(16);
        glow.tick(Some((4, 15)), t4, &c, g, &mut out);
        let t5 = t4 + ms(16);
        glow.tick(Some((4, 10)), t5, &c, g, &mut out);
        assert_eq!(
            glow.v2_status().expect("engaged").meteors,
            0,
            "is_curtain={is_curtain}: pre-seam kill_retreat_pending must not license a post-seam move"
        );
    }
}

#[test]
fn seated_caret_stop_and_licensed_row_follow_scroll_band_and_hand_lift() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((5, 2)), t0, &c, g, &mut out);

    let mut now = t0;
    for col in 3..=7u16 {
        now += ms(40);
        glow.note_typed_cells(now, 1);
        glow.tick(Some((5, col)), now, &c, g, &mut out);
    }
    let seated_t = glow.rainbow_field();
    let seated_rgb = glow.rainbow_head_rgb(&c);
    assert!(seated_t > 0.05, "fixture: caret must settle past red");

    now += Duration::from_secs(3);
    glow.tick(Some((5, 7)), now, &c, g, &mut out);
    assert_eq!(glow.v2.ribbon().live_cells(), 0, "fixture: ribbon expired");
    assert_eq!(glow.rainbow_field(), seated_t);

    glow.note_scroll(2);
    now += ms(16);
    glow.tick(Some((3, 7)), now, &c, g, &mut out);
    assert_eq!(glow.rainbow_field(), seated_t);
    assert_eq!(glow.rainbow_head_rgb(&c), seated_rgb);

    glow.note_band_move(2, 6, 1);
    now += ms(16);
    glow.tick(Some((4, 7)), now, &c, g, &mut out);
    assert_eq!(glow.rainbow_field(), seated_t);
    assert_eq!(glow.rainbow_head_rgb(&c), seated_rgb);

    glow.last_licensed_row = Some((4, now));
    glow.lift_hand_row(4, 3);
    assert_eq!(glow.last_licensed_row, Some((3, now)));
    now += ms(16);
    glow.tick(Some((3, 7)), now, &c, g, &mut out);
    assert_eq!(glow.rainbow_field(), seated_t);
    assert_eq!(glow.rainbow_head_rgb(&c), seated_rgb);
}
