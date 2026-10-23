// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The word move sounds every time.

use super::*;

fn wordnav_row(fill: u16) -> [char; 40] {
    let mut row = [' '; 40];
    for (i, cell) in row.iter_mut().enumerate() {
        if (i as u16) < fill {
            *cell = 'x';
        }
    }
    row
}

#[test]
fn twenty_word_keys_in_zsh_sound_twenty_times() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 42)), t0, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);

    let mut cues = WordNavCues::default();
    for i in 0..20u32 {
        let key = t0 + Duration::from_millis(150 * u64::from(i) + 150);
        let col = if i % 2 == 0 { 38 } else { 42 };
        glow.note_motion(key);
        let echo = key + Duration::from_millis(12);
        glow.tick(Some((2, col)), echo, &c, g, &mut typed_scratch());
        let drained = wordnav_drain(&mut glow);
        cues.nav += drained.nav;
        cues.meteor += drained.meteor;
    }
    assert_eq!(
        (cues.nav, cues.meteor),
        (20, 0),
        "a 4-cell word hop is the mini-fan's nav tick (D17), one per key"
    );
    assert_eq!(glow.admission_tally().declined, 0);
}

#[test]
fn twenty_word_keys_through_an_ink_repaint_sound_twenty_times() {
    for paned in [true, false] {
        twenty_word_keys_through_an_ink_repaint(paned);
    }
}

fn twenty_word_keys_through_an_ink_repaint(paned: bool) {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.note_context(true);
    if paned {
        glow.note_pane_columns(0, g.cols);
    }
    glow.tick(Some((2, 42)), t0, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);

    let mut cues = WordNavCues::default();
    for i in 0..20u32 {
        let key = t0 + Duration::from_millis(150 * u64::from(i) + 150);
        let col = if i % 2 == 0 { 37 } else { 42 };
        glow.note_motion(key);
        glow.note_repaint_blink(key + Duration::from_millis(8));
        glow.tick(
            None,
            key + Duration::from_millis(10),
            &c,
            g,
            &mut typed_scratch(),
        );
        glow.tick(
            Some((2, col)),
            key + Duration::from_millis(40),
            &c,
            g,
            &mut typed_scratch(),
        );
        let drained = wordnav_drain(&mut glow);
        cues.nav += drained.nav;
        cues.meteor += drained.meteor;
    }
    assert_eq!(
        (cues.nav, cues.meteor),
        (20, 0),
        "paned={paned}: the key was pressed: a repaint that hid the caret is \
         not a reason to swallow its echo"
    );
}

#[test]
fn a_word_pair_sixty_milliseconds_apart_sounds_twice() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 30)), t0, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);

    let mut cues = WordNavCues::default();
    for (i, col) in [25u16, 20].into_iter().enumerate() {
        let key = t0 + Duration::from_millis(60 * i as u64 + 20);
        glow.note_motion(key);
        glow.tick(
            Some((2, col)),
            key + Duration::from_millis(14),
            &c,
            g,
            &mut typed_scratch(),
        );
        let drained = wordnav_drain(&mut glow);
        cues.nav += drained.nav;
        cues.meteor += drained.meteor;
    }
    assert_eq!((cues.nav, cues.meteor), (2, 0), "two keys, two ticks");
}

#[test]
fn a_hidden_program_relocation_with_no_fresh_key_still_sounds_nothing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.note_context(true);
    glow.tick(Some((2, 42)), t0, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);

    glow.tick(
        None,
        t0 + Duration::from_millis(10),
        &c,
        g,
        &mut typed_scratch(),
    );
    glow.tick(
        Some((2, 6)),
        t0 + Duration::from_millis(40),
        &c,
        g,
        &mut typed_scratch(),
    );
    assert_eq!(
        wordnav_drain(&mut glow),
        WordNavCues::default(),
        "no keystroke, no sound (T1)"
    );
    assert_eq!(glow.spawns(), 0);

    let stale = t0 + Duration::from_millis(100);
    glow.note_motion(stale);
    glow.tick(
        Some((2, 6)),
        stale + Duration::from_millis(10),
        &c,
        g,
        &mut typed_scratch(),
    );
    let _ = wordnav_drain(&mut glow);
    glow.tick(
        None,
        stale + Duration::from_millis(300),
        &c,
        g,
        &mut typed_scratch(),
    );
    glow.tick(
        Some((2, 30)),
        stale + Duration::from_millis(340),
        &c,
        g,
        &mut typed_scratch(),
    );
    assert_eq!(
        wordnav_drain(&mut glow),
        WordNavCues::default(),
        "a stale stamp is not a licence — the pinned bridge law stands"
    );
}

#[test]
fn the_nav_bridge_never_reaches_back_to_a_source_the_key_did_not_move_from() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 42)), t0, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);

    for step in 1..=8u64 {
        glow.tick(
            None,
            t0 + Duration::from_millis(400 * step),
            &c,
            g,
            &mut typed_scratch(),
        );
    }
    let key = t0 + Duration::from_millis(3_400);
    glow.note_motion(key);
    glow.tick(
        Some((5, 2)),
        key + Duration::from_millis(20),
        &c,
        g,
        &mut typed_scratch(),
    );
    assert_eq!(
        wordnav_drain(&mut glow),
        WordNavCues::default(),
        "a 3.4 s-old source is not the cell this key moved from"
    );

    let mut glow = CursorGlow::default();
    let seen = t0;
    glow.tick(Some((2, 42)), seen, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);
    let key = seen + Duration::from_millis(140);
    glow.note_motion(key);
    glow.tick(
        None,
        key + Duration::from_millis(8),
        &c,
        g,
        &mut typed_scratch(),
    );
    glow.tick(
        Some((2, 37)),
        key + Duration::from_millis(30),
        &c,
        g,
        &mut typed_scratch(),
    );
    let cues = wordnav_drain(&mut glow);
    assert_eq!((cues.nav, cues.meteor), (1, 0));
}

#[test]
fn each_word_kill_sounds_once_and_never_eats_the_next_word_move() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.observe_row(2, 30, &wordnav_row(30), t0);
    glow.tick(Some((2, 30)), t0, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);

    let mut kills = 0u32;
    let mut col = 30u16;
    for i in 0..4u32 {
        let key = t0 + Duration::from_millis(200 * u64::from(i) + 200);
        glow.note_word_kill(key, true);
        col -= 5;
        let echo = key + Duration::from_millis(15);
        glow.observe_row(2, col, &wordnav_row(col), echo);
        glow.tick(Some((2, col)), echo, &c, g, &mut typed_scratch());
        let drained = wordnav_drain(&mut glow);
        kills += drained.kill_word;
        assert_eq!(
            (drained.nav, drained.meteor),
            (0, 0),
            "the kill's own retreat is the kill's, not a navigation gesture"
        );
    }
    assert_eq!(kills, 4, "one word poof per word kill");

    let kill = t0 + Duration::from_millis(1200);
    glow.note_word_kill(kill, true);
    col -= 5;
    glow.tick(
        Some((2, col)),
        kill + Duration::from_millis(15),
        &c,
        g,
        &mut typed_scratch(),
    );
    let _ = wordnav_drain(&mut glow);
    let hop = kill + Duration::from_millis(200);
    glow.note_motion(hop);
    glow.tick(
        Some((2, col - 4)),
        hop + Duration::from_millis(14),
        &c,
        g,
        &mut typed_scratch(),
    );
    let after = wordnav_drain(&mut glow);
    assert_eq!(
        (after.nav, after.meteor),
        (1, 0),
        "a real nav press supersedes a kill's unpaid retreat"
    );
}

#[test]
fn a_kill_retreat_landing_with_a_band_move_or_a_scroll_is_not_a_navigation_meteor() {
    #[derive(Clone, Copy, Debug)]
    enum Fence {
        None,
        Band,
        BandThenFrame,
        Scroll,
    }
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    for fence in [
        Fence::None,
        Fence::Band,
        Fence::BandThenFrame,
        Fence::Scroll,
    ] {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((13, 2)), t0, &c, g, &mut out);
        for i in 0..12u16 {
            let key = at(100 * u64::from(i) + 100);
            glow.note_typed(key);
            glow.tick(
                Some((13, 3 + i)),
                key + Duration::from_millis(8),
                &c,
                g,
                &mut out,
            );
        }
        assert_eq!(glow.admission_tally().licensed, 12, "{fence:?}");
        let _ = wordnav_drain(&mut glow);
        let meteors_before = glow.v2_status().map_or(0, |s| s.meteors);
        let kill = at(2000);
        glow.note_kill(kill, true);
        let row = match fence {
            Fence::None => 13,
            Fence::Band | Fence::BandThenFrame => {
                glow.note_band_move(11, 56, 1);
                glow.drop_row_probe();
                14
            }
            Fence::Scroll => {
                glow.note_scroll(1);
                glow.drop_row_probe();
                12
            }
        };
        if matches!(fence, Fence::BandThenFrame) {
            glow.tick(
                Some((row, 14)),
                kill + Duration::from_millis(8),
                &c,
                g,
                &mut out,
            );
        }
        glow.tick(
            Some((row, 2)),
            kill + Duration::from_millis(20),
            &c,
            g,
            &mut out,
        );
        let cues = wordnav_drain(&mut glow);
        assert_eq!(
            (cues.meteor, cues.nav),
            (0, 0),
            "{fence:?}: the kill's own retreat flew a meteor"
        );
        assert_eq!(
            glow.v2_status().map_or(0, |s| s.meteors),
            meteors_before,
            "{fence:?}: no meteor minted"
        );
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("licensed", "key", (row, 14), (row, 2))),
            "{fence:?}: the retreat is admitted as the kill's: {:?}",
            ring_rows(&glow).last()
        );
        assert!(
            glow.kill_retreat_pending.is_none(),
            "{fence:?}: the one retreat is spent"
        );
    }
}

#[test]
fn a_word_key_judges_a_held_park_before_its_own_landing_is_bridged() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();

    let pre = pre_roll(&mut glow, 11, t0, &c, g);
    let rows_before = ring_rows(&glow).len();
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    let park_at = k + Duration::from_millis(10);
    glow.tick(Some((11, 3)), park_at, &c, g, &mut out);
    assert_eq!(ring_rows(&glow).len(), rows_before, "held: no verdict yet");
    assert_eq!(glow.in_flight_tally().park_flushed, 0);
    let _ = wordnav_drain(&mut glow);

    let hop = park_at + Duration::from_millis(60);
    glow.note_motion(hop);
    assert_eq!(
        glow.in_flight_tally().park_flushed,
        1,
        "the park is judged AT THE PRESS, before the nav licence exists"
    );
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (11, 5), (11, 3))),
        "…as its own move, from its own origin"
    );

    glow.note_repaint_blink(hop + Duration::from_millis(8));
    glow.tick(
        None,
        hop + Duration::from_millis(10),
        &c,
        g,
        &mut typed_scratch(),
    );
    glow.tick(
        Some((11, 12)),
        hop + Duration::from_millis(40),
        &c,
        g,
        &mut typed_scratch(),
    );
    let cues = wordnav_drain(&mut glow);
    assert_eq!(
        cues.nav + cues.meteor,
        1,
        "the bridged landing is the word key's gesture, and it sounds once"
    );
    assert_eq!(
        ring_rows(&glow).last().map(|r| (r.0, r.3)),
        Some(("licensed", (11, 12))),
        "…and it is on the ledger as a licensed move, not held dark"
    );
    glow.tick(
        Some((11, 12)),
        hop + Duration::from_millis(400),
        &c,
        g,
        &mut typed_scratch(),
    );
    assert_eq!(
        glow.in_flight_tally().park_flushed,
        1,
        "nothing was parked behind the nav hint: no second flush exists"
    );
}

#[test]
fn forward_word_hop_after_unspent_typed_credit_still_sounds_nav() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let g = wide_geom();
    glow.tick(Some((5, 10)), t0, &c, g, &mut typed_scratch());

    for i in 0..4 {
        glow.note_typed(t0 + Duration::from_millis(10 + i * 10));
    }
    let hop = t0 + Duration::from_millis(600);
    glow.note_motion(hop);
    glow.tick(
        Some((5, 4)),
        hop + Duration::from_millis(16),
        &c,
        g,
        &mut typed_scratch(),
    );
    let back_cues = wordnav_drain(&mut glow);
    assert_eq!(back_cues.nav + back_cues.meteor, 1, "Option+b sounds");

    let fwd_hop = hop + Duration::from_millis(120);
    glow.note_motion(fwd_hop);
    glow.tick(
        Some((5, 8)),
        fwd_hop + Duration::from_millis(16),
        &c,
        g,
        &mut typed_scratch(),
    );
    let fwd_cues = wordnav_drain(&mut glow);
    assert_eq!(
        fwd_cues.nav + fwd_cues.meteor,
        1,
        "Option+f forward word hop must sound as nav even when old unspent typed credits exist"
    );
}
