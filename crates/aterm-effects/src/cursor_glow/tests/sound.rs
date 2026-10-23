// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing sound cues and the key-time ledger.

use super::*;

#[test]
fn sound_cues_mirror_licensed_spawn_classification() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed: no move, no cue
    assert_eq!(glow.drain_sound_cues().count(), 0);

    glow.note_synthetic_typed(t0, 1);
    glow.tick(Some((2, 1)), t0, &c, g, &mut out); // typed step
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
    assert_eq!(cues[0].col, 1);

    glow.note_synthetic_move(t0);
    glow.tick(Some((4, 30)), t0, &c, g, &mut out); // multi-cell jump
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Jump);

    arm_exact_backspace(&mut glow, t0, (4, 30), (4, 29));
    glow.tick(Some((4, 29)), t0, &c, g, &mut out); // deletion echo
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Backspace);

    glow.note_navigation(t0);
    glow.tick(Some((4, 0)), t0, &c, g, &mut out);
    assert_eq!(glow.heat, 0.0, "a scrub earns no heat, however it sounds");
    let _ = glow.drain_sound_cues().count();

    glow.note_synthetic_typed(t0, 1);
    glow.tick(Some((4, 2)), t0, &c, g, &mut out);
    glow.tick(Some((4, 3)), t0, &cfg(GlowStyle::Water, false), g, &mut out);
    assert_eq!(glow.drain_sound_cues().count(), 0);
}

#[test]
fn newest_erase_key_owns_the_poof_voice() {
    use crate::trail_sound::SoundKind;

    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    let erase_voice = |glow: &mut CursorGlow| -> Vec<SoundKind> {
        glow.drain_sound_cues()
            .map(|cue| cue.kind)
            .filter(|kind| matches!(kind, SoundKind::Kill | SoundKind::Poof))
            .collect()
    };

    let mut backspace = CursorGlow::default();
    let mut out = Vec::new();
    backspace.observe_row(2, 5, &row("hello"), t0);
    backspace.tick(Some((2, 5)), t0, &c, g, &mut out);
    let _ = backspace.drain_sound_cues().count();
    backspace.note_synthetic_typed(t0 + Duration::from_millis(1), 2);
    assert!(
        backspace.type_hint.armed(),
        "precondition: typed stamps banked"
    );
    backspace.note_kill(t0 + Duration::from_millis(5), false);
    let key = t0 + Duration::from_millis(30);
    backspace.note_backspace(key);
    assert!(
        !backspace.type_hint.armed(),
        "Backspace remains a class-changing typed-bank supersede"
    );
    assert!(
        backspace.kill_hint.is_none() && backspace.bs_poof_hint == Some(key),
        "the newer ordinary Backspace must own the pending poof"
    );
    let echo = t0 + Duration::from_millis(46);
    backspace.observe_row(2, 4, &row("hell"), echo);
    backspace.tick(Some((2, 4)), echo, &c, g, &mut out);
    assert_eq!(
        erase_voice(&mut backspace),
        [SoundKind::Poof],
        "the one-cell Backspace shrink puffs; the older kill cannot lend it a swoosh"
    );

    let mut kill = CursorGlow::default();
    let mut out = Vec::new();
    kill.observe_row(2, 5, &row("hello"), t0);
    kill.tick(Some((2, 5)), t0, &c, g, &mut out);
    let _ = kill.drain_sound_cues().count();
    kill.note_synthetic_typed(t0 + Duration::from_millis(1), 2);
    kill.note_backspace(key);
    kill.note_kill(key, true);
    assert!(
        !kill.type_hint.armed(),
        "the class change retires typed stamps"
    );
    assert!(
        kill.bs_poof_hint.is_none() && kill.kill_hint == Some(key),
        "the final kill classification must own the pending poof"
    );
    kill.observe_row(2, 4, &row("hell"), echo);
    kill.tick(Some((2, 4)), echo, &c, g, &mut out);
    assert_eq!(
        erase_voice(&mut kill),
        [SoundKind::Kill],
        "the same shrink under an actual kill retains its swoosh"
    );
}

#[test]
fn resume_after_pause_lays_no_interior_black_cell() {
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed
    let key = Duration::from_millis(350);
    let mut t = t0;
    for k in 1..=9u16 {
        t += key;
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, k)), t, &c, g, &mut out);
    }
    t += Duration::from_millis(4500);
    for k in 10..=18u16 {
        t += Duration::from_millis(150);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, k)), t, &c, g, &mut out);
    }
    let cols: Vec<u16> = glow
        .sparks
        .iter()
        .filter(|s| s.typing && s.row == 2)
        .map(|s| s.col)
        .collect();
    let mut sorted = cols.clone();
    sorted.sort_unstable();
    assert_eq!(
        cols, sorted,
        "the sparks Vec must walk the mark tail→head; a re-mint pushed \
         after the head hands the head the far feather's zero"
    );
    let mut lit: Vec<u16> = glow
        .under_quads()
        .iter()
        .filter(|q| q.row == 2)
        .flat_map(|q| {
            let c0 = q.x / g.cw as u16;
            let c1 = (q.x + q.w - 1) / g.cw as u16;
            c0..=c1
        })
        .collect();
    lit.sort_unstable();
    lit.dedup();
    assert!(
        !lit.is_empty(),
        "the resumed ribbon must draw (nothing under-emitted at all)"
    );
    for pair in lit.windows(2) {
        assert_eq!(
            pair[1],
            pair[0] + 1,
            "interior ribbon hole at col {}..{} — a resident cell was \
             culled between lit neighbours (lit cols: {lit:?})",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn flood_presses_bank_licenses_for_every_echo_sweep() {
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed
    let k1 = t0 + Duration::from_millis(10);
    let k2 = t0 + Duration::from_millis(12);
    let k3 = t0 + Duration::from_millis(14);
    glow.supersede_typed_press();
    glow.note_synthetic_typed(k1, 1);
    glow.supersede_typed_press();
    glow.note_synthetic_typed(k2, 1);
    glow.supersede_typed_press();
    glow.note_synthetic_typed(k3, 1);
    glow.tick(
        Some((2, 2)),
        t0 + Duration::from_millis(30),
        &c,
        g,
        &mut out,
    );
    glow.tick(
        Some((2, 3)),
        t0 + Duration::from_millis(46),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.admission_tally().declined,
        0,
        "every echo sweep of a real keypress must be licensed — a decline \
         here is a permanently unlit cell"
    );
    let owned = v2_cols(&glow, 2);
    for col in [0u16, 1, 2] {
        assert!(
            owned.contains(&col),
            "col {col} was typed and echoed but laid no light (owned: {owned:?})"
        );
    }
    assert_eq!(glow.typed_credits_within(t0 + Duration::from_millis(46)), 0);
    glow.tick(
        Some((2, 4)),
        t0 + Duration::from_millis(60),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.admission_tally().declined,
        1,
        "a move beyond the banked presses is program output and stays dark"
    );
    assert!(
        !v2_cols(&glow, 2).contains(&3),
        "the program cell laid no light: {:?}",
        v2_cols(&glow, 2)
    );
}

#[test]
fn a_coalesced_echos_surviving_stamps_license_nothing_once_its_presses_are_paid() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    for wide in [false, true] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((3, 5)), t0, &c, g, &mut out);
        glow.note_typed(t0 + ms(10));
        glow.note_typed(t0 + ms(20));
        glow.note_typed(t0 + ms(30));
        let echo = t0 + ms(50);
        glow.tick(Some((3, 8)), echo, &c, g, &mut out);
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("licensed", "key", (3, 5), (3, 8))),
            "wide={wide}: {:?}",
            ring_rows(&glow)
        );
        assert_eq!(glow.typed_credits_within(echo), 0, "all three spent");
        assert!(dark_in(&glow, 3, 5..8).is_empty());
        let later = echo + ms(100);
        let target = if wide { 40 } else { 9 };
        glow.tick(Some((3, target)), later, &c, g, &mut out);
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("no-fresh-hint", "none", (3, 8), (3, target))),
            "wide={wide}: a keyless hop with no press unpaid is refused: {:?}",
            ring_rows(&glow)
        );
        assert!(
            !v2_cols(&glow, 3).contains(&(target - 1)),
            "wide={wide}: a program cell lit on a dangling stamp: {:?}",
            v2_cols(&glow, 3)
        );
    }
}

#[test]
fn a_keyless_hop_the_in_flight_pool_cannot_pay_for_lights_no_landing() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    glow.note_typed(t0 + ms(10));
    glow.note_typed(t0 + ms(20));
    glow.tick(Some((3, 2)), t0 + ms(30), &c, g, &mut out);
    assert_eq!(glow.spawns(), 0);
    for (i, col) in [(1u64, 3u16), (2, 4)] {
        let key = t0 + ms(100 * i);
        glow.note_typed(key);
        glow.tick(Some((3, col)), key + ms(10), &c, g, &mut out);
    }
    assert_eq!(glow.spawns(), 2, "{:?}", ring_rows(&glow));
    let flood = t0 + ms(720);
    assert_eq!(
        glow.typed_credits_within(flood),
        2,
        "the two never-judged presses dangle on the ring"
    );
    assert!(
        !glow.type_hint.any_fresh(flood, CursorGlow::TYPE_HINT_FRESH),
        "no stamp is fresh at the flood"
    );
    glow.tick(Some((3, 16)), flood, &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-credits", "none", (3, 4), (3, 16))),
        "the share rule refuses the flood: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(
        glow.in_flight_tally().forgotten,
        1,
        "the pool that could not describe the flood is forgotten"
    );
    assert_eq!(glow.typed_credits_within(flood), 0);
    assert_eq!(
        v2_cols(&glow, 3).into_iter().collect::<Vec<_>>(),
        vec![2, 3],
        "the two echoed keys and nothing else — the flood's landing (15) is program output"
    );

    let mut keyed = CursorGlow::default();
    let k0 = Instant::now();
    keyed.tick(Some((3, 4)), k0, &c, g, &mut out);
    keyed.note_typed(k0 + ms(10));
    keyed.note_typed(k0 + ms(20));
    keyed.tick(Some((3, 4)), k0 + ms(30), &c, g, &mut out);
    let key = k0 + ms(500);
    keyed.note_typed(key);
    keyed.tick(Some((3, 16)), key + ms(8), &c, g, &mut out);
    assert_eq!(
        ring_rows(&keyed).last(),
        Some(&("no-credits", "none", (3, 4), (3, 16))),
        "{:?}",
        ring_rows(&keyed)
    );
    assert_eq!(
        v2_cols(&keyed, 3).into_iter().collect::<Vec<_>>(),
        vec![15],
        "a keyed re-anchor lays its landing and nothing else"
    );
}

#[test]
fn key_time_click_fires_at_the_key_and_mutes_its_own_echo() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    assert_eq!(glow.drain_sound_cues().count(), 0);

    let key = t0 + Duration::from_millis(1);
    assert!(glow.cue_keystroke(key), "a live engine clicks at the key");
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
    assert_eq!(
        cues[0].col, 0,
        "the key-time cue pans from the PRE-echo cursor cell"
    );

    let echo = t0 + Duration::from_millis(41);
    glow.note_synthetic_typed(echo, 1);
    glow.tick(Some((2, 1)), echo, &c, g, &mut out);
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "one character must click exactly once"
    );

    let out_echo = t0 + Duration::from_millis(60);
    glow.note_synthetic_typed(out_echo, 1);
    glow.tick(Some((2, 2)), out_echo, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
}

#[test]
fn take_key_cue_hands_back_the_click_without_touching_the_ledger() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    let echo = t0 + Duration::from_millis(1);
    glow.note_synthetic_typed(echo, 1);
    glow.tick(Some((2, 1)), echo, &c, g, &mut out);
    assert_eq!(
        glow.sound_cues.len(),
        1,
        "precondition: a cue is queued for the frame drain"
    );

    let key = t0 + Duration::from_millis(20);
    assert!(glow.cue_keystroke(key), "a live engine clicks at the key");
    let taken = glow.take_key_cue().expect("the key's own cue comes back");
    assert_eq!(taken.kind, SoundKind::Typed);
    assert_eq!(
        taken.col, 1,
        "…carrying the pre-echo cursor cell, exactly as the drain would have"
    );
    assert_eq!(
        glow.sound_cues.len(),
        1,
        "it pops ITS cue — the older one still belongs to the frame drain"
    );

    let key_echo = t0 + Duration::from_millis(45);
    glow.note_synthetic_typed(key_echo, 1);
    glow.tick(Some((2, 2)), key_echo, &c, g, &mut out);
    let drained: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(
        drained.len(),
        1,
        "only the pre-existing echo cue drains — the credit muted this echo"
    );

    assert!(
        glow.take_key_cue().is_none(),
        "an empty queue hands back no click"
    );
}

#[test]
fn a_key_whose_echo_jumps_still_clicks_exactly_once() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    let _ = glow.drain_sound_cues().count();

    let key = t0 + Duration::from_millis(1);
    assert!(glow.cue_keystroke(key));
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);

    let echo = t0 + Duration::from_millis(40);
    glow.note_synthetic_typed(key, 1);
    glow.tick(Some((6, 9)), echo, &c, g, &mut out);
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "one keypress must not produce Typed at the key AND Jump at the echo"
    );

    let later = t0 + Duration::from_millis(80);
    glow.note_synthetic_typed(later, 1);
    glow.tick(Some((6, 10)), later, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1, "the Jump must not leak the credit forward");
    assert_eq!(cues[0].kind, SoundKind::Typed);
}

#[test]
fn admitted_spawn_arms_handle_the_key_time_ledger() {
    use crate::trail_sound::SoundKind;
    type Arm = (
        &'static str,
        fn(&mut CursorGlow, Instant),
        (u16, u16),
        Option<SoundKind>,
    );
    let arms: [Arm; 3] = [
        (
            "deletion",
            |gl, t| arm_exact_backspace(gl, t, (2, 5), (2, 4)),
            (2, 4),
            Some(SoundKind::Backspace),
        ),
        (
            "typing",
            |gl, t| gl.note_synthetic_typed(t, 1),
            (2, 6),
            None,
        ),
        ("jump", |gl, t| gl.note_synthetic_move(t), (5, 30), None),
    ];
    for (name, arm_hint, land, expect) in arms {
        let mut glow = CursorGlow::default();
        let g = geom();
        let c = cfg(GlowStyle::Water, true);
        let t0 = Instant::now();
        let mut out = Vec::new();
        glow.tick(Some((2, 5)), t0, &c, g, &mut out);
        let _ = glow.drain_sound_cues().count();

        let key = t0 + Duration::from_millis(1);
        assert!(glow.cue_keystroke(key), "{name}: the key must click");
        assert_eq!(glow.drain_sound_cues().count(), 1, "{name}");

        let echo = t0 + Duration::from_millis(30);
        arm_hint(&mut glow, echo);
        glow.tick(Some(land), echo, &c, g, &mut out);
        let cues: Vec<_> = glow.drain_sound_cues().collect();
        match expect {
            Some(kind) => {
                assert_eq!(cues.len(), 1, "{name}: its own gesture must still speak");
                assert_eq!(cues[0].kind, kind, "{name}");
            }
            None => assert_eq!(
                cues.len(),
                0,
                "{name}: a printable key's own echo must not speak twice"
            ),
        }
        let settles = matches!(name, "typing" | "jump");
        assert_eq!(
            glow.keyed_clicks,
            u8::from(!settles),
            "{name}: wrong ledger disposition for this arm"
        );

        let later = echo + Duration::from_millis(120);
        glow.note_synthetic_typed(later, 1);
        let (lr, lc) = land;
        glow.tick(Some((lr, lc + 1)), later, &c, g, &mut out);
        let cues: Vec<_> = glow.drain_sound_cues().collect();
        if settles {
            assert_eq!(cues.len(), 1, "{name}: a later real keystroke was muted");
            assert_eq!(cues[0].kind, SoundKind::Typed, "{name}");
        } else {
            assert_eq!(
                cues.len(),
                0,
                "{name}: the standing credit \
                 silences exactly one later click, then expires"
            );
        }
    }
}

#[test]
fn the_key_time_click_carries_its_own_keystrokes_heat() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    let mut keyed = CursorGlow::default();
    let mut echo_only = CursorGlow::default();
    keyed.tick(Some((2, 0)), t0, &c, g, &mut out);
    echo_only.tick(Some((2, 0)), t0, &c, g, &mut out);

    let mut charged = 0usize;
    for i in 1..=6u64 {
        let key = t0 + Duration::from_millis(i * 100);
        let echo = key + Duration::from_millis(5);

        let stale = keyed.blaze();
        assert!(keyed.cue_keystroke(key));
        let keyed_cue: Vec<_> = keyed.drain_sound_cues().collect();
        assert_eq!(keyed_cue.len(), 1);
        let hot = keyed_cue[0].heat;

        keyed.note_synthetic_typed(echo, 1);
        keyed.tick(Some((2, i as u16)), echo, &c, g, &mut out);
        assert_eq!(
            keyed.drain_sound_cues().count(),
            0,
            "the echo spends the credit"
        );

        echo_only.note_synthetic_typed(echo, 1);
        echo_only.tick(Some((2, i as u16)), echo, &c, g, &mut out);
        let ref_cue: Vec<_> = echo_only.drain_sound_cues().collect();
        assert_eq!(ref_cue.len(), 1);

        if i == 1 {
            assert!(
                (hot - stale).abs() < 1e-6,
                "a cold first key must not invent heat: {hot} vs {stale}"
            );
        } else {
            charged += 1;
            assert!(
                hot > stale + 0.05,
                "key {i} cued the PRE-keystroke heat ({hot} vs stale {stale}) \
                 — the timbre is one keystroke behind"
            );
            assert!(
                (hot - ref_cue[0].heat).abs() < 0.02,
                "key {i}: key-time heat {hot} must track the echo-time \
                 engine's {}",
                ref_cue[0].heat
            );
        }
    }
    assert_eq!(charged, 5, "the ramp legs must actually have run");
}

#[test]
fn a_synthetic_gesture_jump_speaks_but_cold_output_is_dark() {
    use crate::trail_sound::SoundKind;
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    let jump = t0 + Duration::from_millis(40);
    let mut preview = CursorGlow::default();
    preview.tick(Some((2, 0)), t0, &c, g, &mut out);
    let _ = preview.drain_sound_cues().count();
    preview.note_synthetic_move(jump);
    preview.tick(Some((6, 9)), jump, &c, g, &mut out);
    let cues: Vec<_> = preview.drain_sound_cues().collect();
    assert_eq!(
        cues.iter().filter(|c| c.kind == SoundKind::Jump).count(),
        1,
        "a synthetic preview is still a Jump (beside v2's own meteor cues): {cues:?}"
    );

    let mut cold = CursorGlow::default();
    cold.tick(Some((2, 0)), t0, &c, g, &mut out);
    cold.tick(Some((6, 9)), jump, &c, g, &mut out);
    assert_eq!(cold.drain_sound_cues().count(), 0);
    assert!(!frame_has_output(&out, &cold));
}

#[test]
fn a_paste_strums_under_the_music_box_and_is_silent_under_the_nine() {
    use crate::trail_sound::SoundKind;
    let g = geom();
    let t0 = Instant::now();
    let mut out = Vec::new();

    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    assert!(
        glow.cue_paste(t0 + Duration::from_millis(1)),
        "the music box strums"
    );
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Strum);
    let echo = t0 + Duration::from_millis(20);
    glow.note_synthetic_typed(echo, 5);
    glow.tick(Some((2, 5)), echo, &c, g, &mut out);
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "the echo is the strum's"
    );

    for style in [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
        GlowStyle::Classic,
    ] {
        let c = cfg(style, true);
        let mut glow = CursorGlow::default();
        glow.tick(Some((2, 0)), t0, &c, g, &mut out);
        assert!(
            !glow.cue_paste(t0 + Duration::from_millis(1)),
            "{style:?} has no strum"
        );
        assert_eq!(glow.drain_sound_cues().count(), 0);
        assert_eq!(
            glow.keyed_clicks, 0,
            "{style:?} banks no credit for a paste"
        );
    }
}

#[test]
fn key_time_credits_expire_instead_of_banking_silence() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);

    for i in 0..3u64 {
        assert!(glow.cue_keystroke(t0 + Duration::from_millis(i)));
    }
    assert_eq!(glow.drain_sound_cues().count(), 3);

    let echo = t0 + Duration::from_millis(20);
    glow.note_synthetic_typed(echo, 3);
    glow.tick(Some((2, 3)), echo, &c, g, &mut out);
    assert_eq!(glow.drain_sound_cues().count(), 0);

    let late = echo + Duration::from_secs_f32(CursorGlow::KEYED_CLICK_FRESH + 0.1);
    glow.note_typed(late);
    glow.tick(Some((2, 4)), late, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
    assert_eq!(glow.keyed_clicks, 0, "stale credits are dropped wholesale");
}

#[test]
fn key_time_click_is_silent_wherever_the_sound_gates_close() {
    let mut glow = CursorGlow::default();
    let g = geom();
    let live = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    assert!(!glow.cue_keystroke(t0));
    assert_eq!(glow.drain_sound_cues().count(), 0);

    glow.tick(
        Some((2, 0)),
        t0,
        &cfg(GlowStyle::RainbowKitty, false),
        g,
        &mut out,
    );
    assert!(!glow.cue_keystroke(t0));
    assert_eq!(glow.drain_sound_cues().count(), 0);

    glow.tick(Some((2, 0)), t0, &live, g, &mut out);
    assert!(glow.cue_keystroke(t0));
    assert_eq!(glow.drain_sound_cues().count(), 1);

    let dark = GlowConfig {
        classic_mono: false,
        intensity: 0.0,
        audible: false,
        ..live
    };
    assert!(glow.cue_keystroke(t0));
    glow.tick(Some((2, 0)), t0, &dark, g, &mut out);
    assert_eq!(glow.keyed_clicks, 0);
    assert!(!glow.cue_keystroke(t0));
    assert_eq!(glow.drain_sound_cues().count(), 0);
}

#[test]
fn the_classic_wake_and_an_empty_grid_obey_the_one_key_seam_law() {
    let g = geom();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let classic = cfg(GlowStyle::Classic, true);
    let unfocused = |c: GlowConfig| GlowConfig {
        classic_mono: c.classic_mono,
        intensity: 0.0,
        audible: false,
        ..c
    };

    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 0)), t0, &classic, g, &mut out);
    assert!(glow.sound_seam_open(), "a fresh classic session is heard");
    assert!(glow.cue_keystroke(t0));
    assert!(glow.take_key_cue().is_some());
    assert_eq!(glow.heat_tau_live, CursorGlow::heat_tau(&classic));

    let shed = GlowConfig {
        classic_mono: false,
        intensity: 0.0,
        ..classic
    };
    glow.tick(Some((2, 1)), t0, &shed, g, &mut out);
    assert!(glow.sound_seam_open(), "a shed classic frame is heard");

    let mut glow = CursorGlow::default();
    glow.tick(
        Some((2, 0)),
        t0,
        &cfg(GlowStyle::RainbowKitty, true),
        g,
        &mut out,
    );
    assert!(glow.cue_keystroke(t0));
    assert!(glow.take_key_cue().is_some());
    glow.tick(Some((2, 0)), t0, &unfocused(classic), g, &mut out);
    assert!(
        !glow.sound_seam_open(),
        "an unfocused classic tick is silent"
    );
    assert_eq!(glow.keyed_clicks, 0, "and its credit went with the seam");
    assert!(!glow.cue_keystroke(t0));

    for (empty, what) in [
        (Geom { cols: 0, ..g }, "no columns"),
        (Geom { rows: 0, ..g }, "no rows"),
        (Geom { cw: 0, ..g }, "no cell width"),
    ] {
        let mut glow = CursorGlow::default();
        let live = cfg(GlowStyle::RainbowKitty, true);
        glow.tick(Some((2, 0)), t0, &live, g, &mut out);
        assert!(glow.cue_keystroke(t0));
        assert!(glow.take_key_cue().is_some());
        glow.tick(Some((2, 0)), t0, &live, empty, &mut out);
        assert!(out.is_empty(), "an empty grid draws nothing");
        assert!(glow.sound_seam_open(), "{what}: an empty grid is heard");
        assert_eq!(glow.keyed_clicks, 1, "{what}: the credit survives");
        assert!(!glow.dark_settled, "{what}: an open seam never latches");
        assert!(glow.cue_keystroke(t0));
        assert!(glow.take_key_cue().is_some());
        glow.tick(Some((2, 0)), t0, &unfocused(live), empty, &mut out);
        assert!(!glow.sound_seam_open());
        assert_eq!(glow.keyed_clicks, 0);
        glow.tick(Some((2, 0)), t0, &live, empty, &mut out);
        assert!(glow.cue_keystroke(t0));
        glow.tick(
            Some((2, 0)),
            t0,
            &cfg(GlowStyle::RainbowKitty, false),
            empty,
            &mut out,
        );
        assert!(!glow.sound_seam_open());
        assert_eq!(glow.keyed_clicks, 0, "master off wipes what the grid kept");
        assert!(glow.dark_settled, "and a shut seam latches");
    }
}

#[test]
fn rapid_synthetic_line_feeds_cue_jump_and_typed_gestures() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((0, 12)), t0, &c, g, &mut out); // seed: no move, no cue
    assert_eq!(glow.drain_sound_cues().count(), 0);
    let t1 = t0 + Duration::from_millis(60);
    glow.note_synthetic_move(t1);
    glow.tick(Some((1, 0)), t1, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    let jumps: Vec<_> = cues.iter().filter(|c| c.kind == SoundKind::Jump).collect();
    assert_eq!(
        jumps.len(),
        1,
        "a column-snapping line feed is ONE Jump gesture (beside v2's meteor cues) — \
         reclassifying it kills the brrrring: {cues:?}"
    );
    assert_eq!(jumps[0].col, 0, "the cue pans from the arrival column");
    for i in 2..=5u16 {
        let t = t0 + Duration::from_millis(60 * u64::from(i));
        glow.note_synthetic_move(t);
        glow.tick(Some((i, 0)), t, &c, g, &mut out);
        let cues: Vec<_> = glow.drain_sound_cues().collect();
        assert_eq!(
            cues.iter().filter(|c| c.kind == SoundKind::Typed).count(),
            1,
            "line feed {i}: a same-column line feed is ONE Typed pluck between \
             flourishes: {cues:?}"
        );
        assert!(
            cues.iter().all(|c| c.kind != SoundKind::Jump),
            "line feed {i} is not a Jump: {cues:?}"
        );
    }

    let mut cold = CursorGlow::default();
    cold.tick(Some((0, 12)), t0, &c, g, &mut out);
    cold.tick(Some((1, 0)), t1, &c, g, &mut out);
    assert_eq!(cold.drain_sound_cues().count(), 0);
    assert!(!frame_has_output(&out, &cold));
}
