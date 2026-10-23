// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The licence seam and its derived-model conformance.

use super::*;

#[test]
fn rainbow_ribbon_survives_word_boundaries_and_keeps_its_head() {
    use std::fmt::Write as _;
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut scratch = typed_scratch();
    let mut observed = String::new();

    let mut glow = CursorGlow::default();
    glow.note_context(true);
    let t0 = Instant::now();
    glow.tick(Some((2, 0)), t0, &c, g, &mut scratch);
    let mut stored: Vec<char> = Vec::new();
    let mut segments_after_space = 0usize;
    for (i, ch) in ['a', 'b', ' ', 'c', 'd'].into_iter().enumerate() {
        let col = i as u16;
        let at = t0 + Duration::from_millis(i as u64 * 120 + 1);
        glow.note_typed_cells(at, 1);
        if ch != ' ' {
            while stored.len() < i {
                stored.push(' ');
            }
            stored.push(ch);
        }
        glow.observe_row_with_trust(2, col + 1, &stored, at, ProbeTrust::ContentOnly);
        glow.tick(Some((2, col + 1)), at, &c, g, &mut scratch);
        let ring = glow.admission_log().last().map(|r| (r.phase, r.reason));
        let _ = write!(
            observed,
            "key{i}({ch:?}): ring={ring:?} segments={}; ",
            glow.ribbon_segments()
        );
        if ch == ' ' {
            segments_after_space = glow.ribbon_segments();
        }
    }
    let segments_after_word = glow.ribbon_segments();
    assert!(
        segments_after_space >= 3,
        "the ribbon must SURVIVE the word boundary — the heads of 'a' and 'b' plus \
         the space's own — got {segments_after_space} segment(s). Observed: [{observed}]"
    );
    assert!(
        segments_after_word >= 4,
        "'ab cd' must leave a multi-word ribbon, got {segments_after_word} segment(s). \
         Observed: [{observed}]"
    );
}

/// Mash typing keeps the ribbon: several presses inside one frame slip stamp as
/// many cell credits, and the coalesced multi-cell echo is priced against that
/// budget, so it classifies as continued typing (`rainbow_coalesce`) and lays
/// every swept cell.
#[test]
fn rainbow_ribbon_survives_a_three_per_frame_mash() {
    use std::fmt::Write as _;
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut scratch = typed_scratch();
    let mut observed = String::new();

    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 0)), t0, &c, g, &mut scratch);

    let glyphs = ['m', 'a', 's', 'h', 't', 'y', 'p', 'i', 'n', 'g', 'o', 'k'];
    let mut stored: Vec<char> = Vec::new();
    let mut segments_by_frame = Vec::new();
    for frame in 0..4usize {
        let base_col = (frame * 3) as u16;
        let frame_t = t0 + Duration::from_millis(frame as u64 * 16 + 1);
        for k in 0..3usize {
            glow.note_typed_cells(frame_t + Duration::from_millis(k as u64 * 3), 1);
        }
        stored.extend_from_slice(&glyphs[frame * 3..frame * 3 + 3]);
        let probe_at = frame_t + Duration::from_millis(12);
        glow.observe_row_with_trust(2, base_col + 3, &stored, probe_at, ProbeTrust::ContentOnly);
        glow.tick(Some((2, base_col + 3)), probe_at, &c, g, &mut scratch);
        let _ = write!(
            observed,
            "frame{frame}: segments={}; ",
            glow.ribbon_segments()
        );
        segments_by_frame.push(glow.ribbon_segments());
    }

    let tally = glow.admission_tally();
    assert_eq!(
        tally.declined, 0,
        "the mash must decline NOTHING (last_decline_reason={:?}). Observed: [{observed}]",
        tally.last_decline_reason
    );
    assert_eq!(
        tally.licensed, 4,
        "each frame's coalesced echo is one licensed move. Observed: [{observed}]"
    );
    for pair in segments_by_frame.windows(2) {
        assert!(
            pair[1] > pair[0],
            "ribbon_segments must climb across the mash, got {segments_by_frame:?}. \
             Observed: [{observed}]"
        );
    }
    assert!(
        segments_by_frame.last().copied().unwrap_or(0) >= 9,
        "12 mashed presses must leave a long live band, got {segments_by_frame:?}. \
         Observed: [{observed}]"
    );
}

/// Tier 1 for the licence: the real `CursorGlow` driven through the model's four
/// states (press, echo, cold echo, expiry), with every observed step validated
/// as a transition of `CursorHintLicense`.
#[test]
fn the_real_glow_engine_conforms_to_the_licence_model() {
    let model = aterm_models_effects::cursor_hint_license_model();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);

    let project = |glow: &CursorGlow, now: Instant, arms: i64, consumed: i64, forfeited: i64| {
        let tally = glow.admission_tally();
        let live = glow.move_licensed(now);
        let pool = i64::try_from(glow.typed_credits_within(now)).expect("bounded ring");
        let mut st = model.init_state();
        st.insert(
            "hint",
            if live {
                1
            } else if glow.type_hint.armed() {
                2
            } else {
                0
            },
        );
        st.insert("arms", arms);
        st.insert("credit_arms", arms);
        st.insert("consumed", consumed);
        st.insert("forfeited", forfeited);
        st.insert("spent", arms - pool - forfeited);
        st.insert("expired", arms - consumed - i64::from(live));
        let licensed = i64::try_from(tally.licensed).expect("bounded tally");
        let declined = i64::try_from(tally.declined).expect("bounded tally");
        st.insert("admissions", licensed);
        st.insert("spawns", licensed + declined);
        st.insert("licensed_tally", licensed);
        st.insert("declined_tally", declined);
        st.insert("births", licensed);
        st.insert("resident", i64::from(glow.is_active()));
        st
    };
    let bind = |prev: &aterm_spec::interp::State,
                next: &aterm_spec::interp::State,
                action: &str,
                label: &str| {
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            prev,
            next,
            Some(action),
            label,
        );
        assert!(ok, "{label}: real engine step rejected by the model: {why}");
    };
    let refuse = |prev: &aterm_spec::interp::State,
                  next: &aterm_spec::interp::State,
                  action: &str,
                  label: &str| {
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            prev,
            next,
            Some(action),
            label,
        );
        assert!(!ok, "{label}: the model must refuse this step");
    };

    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    out.clear();
    let seeded = project(&glow, t0, 0, 0, 0);
    assert_eq!(seeded["hint"], 0, "an untouched engine holds no licence");
    assert_eq!(seeded["resident"], 0, "and no light");
    assert_eq!(seeded["spawns"], 0, "a seed tick is not a move");

    let press_at = t0 + Duration::from_millis(4);
    glow.note_typed_cells(press_at, 1);
    let armed = project(&glow, press_at, 1, 0, 0);
    assert_eq!(armed["hint"], 1, "a press arms the licence");
    assert_eq!(armed["spent"], 0, "the press's credit is in flight");
    bind(&seeded, &armed, "PressArmsLicense", "CursorGlow press");

    let echo_at = press_at + Duration::from_millis(8);
    glow.tick(Some((2, 1)), echo_at, &c, g, &mut out);
    let echoed = project(&glow, echo_at, 1, 1, 0);
    assert_eq!(echoed["licensed_tally"], 1, "the echo is licensed");
    assert_eq!(echoed["declined_tally"], 0);
    assert_eq!(echoed["resident"], 1, "and mints light");
    assert_eq!(
        echoed["hint"], 0,
        "the paired echo consumed the stamp — one hint, one echo"
    );
    assert_eq!(
        echoed["spent"], 1,
        "…and SPENT the press's credit: the pool is empty after an on-time echo"
    );
    bind(
        &armed,
        &echoed,
        "LicensedTypedMoveMintsLight",
        "CursorGlow echo",
    );
    let mut reused = echoed.clone();
    reused.insert("hint", 1);
    refuse(
        &armed,
        &reused,
        "LicensedTypedMoveMintsLight",
        "CursorGlow one-hint-one-echo control",
    );

    let cold_at = echo_at + Duration::from_millis(8);
    glow.tick(Some((2, 2)), cold_at, &c, g, &mut out);
    let cold = project(&glow, cold_at, 1, 1, 0);
    assert_eq!(
        glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "the ring blames the licence seam"
    );
    assert_eq!(cold["spent"], 1, "a PAID press funds nothing more");
    assert_eq!(cold["licensed_tally"], 1, "the cold move mints nothing");
    assert_eq!(cold["declined_tally"], 1, "…and is scored as a decline");
    assert_eq!(cold["resident"], 1, "…and destroys nothing it did not earn");
    bind(
        &echoed,
        &cold,
        "ColdMoveOverEarnedLight",
        "CursorGlow cold echo",
    );
    let mut wiped = cold.clone();
    wiped.insert("resident", 0);
    refuse(
        &echoed,
        &wiped,
        "ColdMoveOverEarnedLight",
        "CursorGlow retention control",
    );

    let u0 = Instant::now();
    let mut idle = CursorGlow::default();
    idle.tick(Some((2, 0)), u0, &c, g, &mut out);
    out.clear();
    let idle_seeded = project(&idle, u0, 0, 0, 0);
    let idle_press = u0 + Duration::from_millis(4);
    idle.note_typed_cells(idle_press, 1);
    let idle_armed = project(&idle, idle_press, 1, 0, 0);
    bind(
        &idle_seeded,
        &idle_armed,
        "PressArmsLicense",
        "CursorGlow idle press",
    );

    let stale_at = idle_press
        + Duration::from_secs_f32(CursorGlow::TYPE_HINT_FRESH)
        + Duration::from_millis(10);
    assert!(
        idle.type_hint.armed(),
        "an unechoed stamp is not withdrawn — it goes stale in place"
    );
    assert!(
        !idle.move_licensed(stale_at),
        "and a stale stamp is not a licence"
    );
    let idle_stale = project(&idle, stale_at, 1, 0, 0);
    assert_eq!(idle_stale["hint"], 2);
    assert_eq!(idle_stale["spent"], 0, "the press is still in flight");
    bind(
        &idle_armed,
        &idle_stale,
        "LicenseExpires",
        "CursorGlow licence expiry",
    );

    let declined_before = idle.admission_tally().last_decline_reason;
    idle.tick(Some((2, 1)), stale_at, &c, g, &mut out);
    let idle_echoed = project(&idle, stale_at, 1, 0, 0);
    assert_eq!(
        idle.admission_tally().last_decline_reason,
        declined_before,
        "the press's own +1 echo is not refused"
    );
    assert_eq!(idle.admission_tally().licensed, 1, "it is licensed");
    assert_eq!(
        idle.in_flight_tally().licensed,
        1,
        "…by the in-flight pool alone"
    );
    let last = idle
        .admission_log()
        .last()
        .map(|r| r.line(stale_at))
        .expect("one ring row");
    assert!(
        last.ends_with(" licence=inflight"),
        "the ring names the in-flight licence: {last}"
    );
    assert_eq!(idle_echoed["resident"], 1, "light was born");
    assert_eq!(
        idle_echoed["hint"], 2,
        "the stale stamp is left in place — it was never the licence"
    );
    assert_eq!(
        idle_echoed["spent"], 1,
        "the press is spent: one press, one cell"
    );
    bind(
        &idle_stale,
        &idle_echoed,
        "InFlightPressEchoMintsLight",
        "CursorGlow one-press echo",
    );
    let mut unspent = idle_echoed.clone();
    unspent.insert("spent", 0);
    refuse(
        &idle_stale,
        &unspent,
        "InFlightPressEchoMintsLight",
        "CursorGlow non-spending control",
    );
    let w0 = Instant::now();
    let mut wide = CursorGlow::default();
    wide.tick(Some((2, 0)), w0, &c, g, &mut out);
    out.clear();
    wide.note_typed_cells(w0 + Duration::from_millis(4), 1);
    let wide_stale_at = w0 + Duration::from_millis(260);
    let wide_stale = project(&wide, wide_stale_at, 1, 0, 0);
    assert_eq!(wide_stale["hint"], 2);
    assert_eq!(wide_stale["spent"], 0, "one press in flight");
    wide.tick(Some((2, 2)), wide_stale_at, &c, g, &mut out);
    assert_eq!(
        wide.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "one press cannot buy two cells"
    );
    assert_eq!(wide.in_flight_tally().forgotten, 0, "…and is kept");
    let wide_declined = project(&wide, wide_stale_at, 1, 0, 0);
    assert_eq!(wide_declined["declined_tally"], 1);
    assert_eq!(
        wide_declined["spent"], 0,
        "the kept press is still in flight"
    );
    bind(
        &wide_stale,
        &wide_declined,
        "StaleStampMoveDeclines",
        "CursorGlow one press, a wider hop: refused and kept",
    );

    let s0 = Instant::now();
    let mut stale = CursorGlow::default();
    stale.tick(Some((2, 5)), s0, &c, g, &mut out);
    out.clear();
    stale.note_typed_cells(s0 + Duration::from_millis(4), 1);
    let back_at = s0 + Duration::from_millis(260);
    let stale_armed = project(&stale, back_at, 1, 0, 0);
    assert_eq!(stale_armed["hint"], 2);
    assert_eq!(stale_armed["spent"], 0);
    let mut stale_in_flight = stale_armed.clone();
    stale_in_flight.insert("expired", 1);
    stale.tick(Some((2, 3)), back_at, &c, g, &mut out);
    let flushed_at = back_at + past_park_window();
    stale.tick(Some((2, 3)), flushed_at, &c, g, &mut out);
    assert_eq!(
        stale.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "a keyless backward hop is refused"
    );
    let stale_pool = stale.typed_credits_within(flushed_at);
    assert_eq!(stale_pool, 0, "…and the pool is forgotten with it");
    let stale_forgot = project(&stale, flushed_at, 1, 0, 1);
    assert_eq!(stale_forgot["declined_tally"], 1);
    assert_eq!(stale_forgot["spent"], 0);
    let mut stale_forgot_model = stale_in_flight.clone();
    stale_forgot_model.insert("spawns", 1);
    stale_forgot_model.insert("declined_tally", 1);
    stale_forgot_model.insert("forfeited", 1);
    stale_forgot_model.insert("just_forgot", 1);
    bind(
        &stale_in_flight,
        &stale_forgot_model,
        "UnexplainedHopForgetsCredits",
        "CursorGlow stale stamp, backward hop",
    );
    let fwd_at = flushed_at + Duration::from_millis(10);
    stale.tick(Some((2, 4)), fwd_at, &c, g, &mut out);
    let stale_declined = project(&stale, fwd_at, 1, 0, 1);
    assert_eq!(stale_declined["declined_tally"], 2);
    assert_eq!(stale_declined["licensed_tally"], 0);
    assert_eq!(stale_declined["resident"], 0, "nothing was born");
    let mut stale_declined_model = stale_forgot_model.clone();
    stale_declined_model.insert("spawns", 2);
    stale_declined_model.insert("declined_tally", 2);
    bind(
        &stale_forgot_model,
        &stale_declined_model,
        "StaleStampMoveDeclines",
        "CursorGlow stale stamp, empty pool",
    );
    let mut stale_light = stale_declined_model.clone();
    stale_light.insert("licensed_tally", 1);
    stale_light.insert("births", 1);
    stale_light.insert("resident", 1);
    stale_light.insert("declined_tally", 1);
    refuse(
        &stale_forgot_model,
        &stale_light,
        "StaleStampMoveDeclines",
        "CursorGlow expiry control",
    );

    let f0 = Instant::now();
    let mut hop = CursorGlow::default();
    hop.tick(Some((2, 9)), f0, &c, g, &mut out);
    out.clear();
    hop.note_typed_cells(f0 + Duration::from_millis(4), 1);
    hop.note_typed_cells(f0 + Duration::from_millis(9), 1);
    let hop_at = f0 + Duration::from_millis(400);
    let pool = |glow: &CursorGlow, now: Instant| {
        i64::try_from(glow.typed_credits_within(now)).expect("bounded ring")
    };
    assert_eq!(pool(&hop, hop_at), 2, "two presses in flight");
    let mut in_flight = model.init_state();
    in_flight.insert("hint", 2);
    in_flight.insert("arms", 2);
    in_flight.insert("credit_arms", 2);
    in_flight.insert("superseded", 1);
    in_flight.insert("expired", 1);
    hop.tick(Some((2, 2)), hop_at, &c, g, &mut out);
    let hop_at = hop_at + past_park_window();
    hop.tick(Some((2, 2)), hop_at, &c, g, &mut out);
    assert_eq!(
        hop.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "a keyless backward hop is refused"
    );
    assert_eq!(pool(&hop, hop_at), 0, "…and the pool is forgotten with it");
    let mut forgot = in_flight.clone();
    forgot.insert("spawns", 1);
    forgot.insert("declined_tally", 1);
    forgot.insert("forfeited", 2 - pool(&hop, hop_at));
    forgot.insert("just_forgot", 1);
    bind(
        &in_flight,
        &forgot,
        "UnexplainedHopForgetsCredits",
        "CursorGlow unexplained hop",
    );
    let mut kept = forgot.clone();
    kept.insert("forfeited", 0);
    refuse(
        &in_flight,
        &kept,
        "UnexplainedHopForgetsCredits",
        "CursorGlow keep-the-pool control",
    );
}

/// Typing while a program streams: a `less`/`vi`/ESC 7-ESC 8 streamer saves,
/// parks, writes a status line elsewhere and restores inside one parser batch,
/// several times a second, while the user types.
#[test]
fn typing_paints_through_the_esc7_esc8_streamer_cadence() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut scratch = typed_scratch();
    let mut glow = CursorGlow::default();
    glow.note_context(true);
    let t0 = Instant::now();
    let row = 3u16;
    glow.tick(Some((row, 0)), t0, &c, g, &mut scratch);

    let mut status = [' '; 40];
    status[..12].fill('#');
    let mut typed: Vec<char> = Vec::new();
    let mut segments = Vec::new();
    for (i, ch) in "streaming".chars().enumerate() {
        let col = i as u16;
        let at = t0 + Duration::from_millis(i as u64 * 120 + 1);

        glow.note_typed_cells(at, 1);

        let streamed = at + Duration::from_millis(20);
        glow.observe_row_with_trust(0, 12, &status, streamed, ProbeTrust::ContentOnly);
        glow.tick(Some((row, col)), streamed, &c, g, &mut scratch);

        typed.push(ch);
        let echo = at + Duration::from_millis(40);
        glow.observe_row_with_trust(row, col + 1, &typed, echo, ProbeTrust::ContentOnly);
        glow.tick(Some((row, col + 1)), echo, &c, g, &mut scratch);
        segments.push(glow.ribbon_segments());

        let cold = at + Duration::from_millis(100);
        let resident = |glow: &CursorGlow| glow.v2_status().map_or(0, |s| s.cells);
        let before = resident(&glow);
        let lit_before = glow.ribbon_segments();
        glow.tick(Some((0, 12)), cold, &c, g, &mut scratch);
        glow.tick(Some((row, col + 1)), cold, &c, g, &mut scratch);
        assert_eq!(
            resident(&glow),
            before,
            "key {i}: a cold streamer walk retired earned ribbon cells"
        );
        glow.tick(Some((row, col + 1)), cold, &c, g, &mut scratch);
        assert!(
            glow.ribbon_segments() >= lit_before,
            "key {i}: the band under the hand lost lit boundaries to a cold walk ({} < {lit_before})",
            glow.ribbon_segments()
        );
    }

    assert!(
        segments.last().copied().unwrap_or(0) >= 6,
        "typing through the streamer must leave a multi-cell ribbon, got {segments:?}"
    );
    assert!(
        segments.windows(2).all(|pair| pair[1] >= pair[0]),
        "the ribbon must never SHRINK across a streamer batch, got {segments:?}"
    );
    assert_eq!(
        glow.spawns(),
        "streaming".len() as u64,
        "every keystroke's echo is licensed and lays light; nothing else is"
    );
    let tally = glow.admission_tally();
    assert_eq!(
        tally.licensed,
        "streaming".len() as u64,
        "one licensed move per key — the streamer buys none"
    );
    assert_eq!(
        tally.last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "and the cold walks were refused at the license seam"
    );
    assert!(glow.is_active(), "live ribbon ⇒ live engine");
}

/// The licence, denial side (`docs/design/EFFECTS-LICENSE-REDESIGN.md`). An
/// unlicensed move is a cursor delta with no fresh key hint behind it.
#[test]
fn an_unlicensed_move_mints_nothing_and_retires_nothing_in_every_style() {
    let g = geom();
    let t0 = Instant::now();
    let styles = [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
    ];
    let shapes: [((u16, u16), (u16, u16)); 6] = [
        ((2, 4), (2, 5)),
        ((2, 4), (2, 7)),
        ((2, 39), (3, 0)),
        ((2, 4), (2, 3)),
        ((2, 4), (3, 4)),
        ((2, 4), (5, 30)),
    ];
    let trail_cfg = TrailConfig {
        enabled: true,
        color: 0x0050_FA7B,
        duration: Duration::from_millis(240),
        max_len: 18,
        intensity: 0.7,
        warmth: 0.0,
    };

    for style in styles {
        let c = cfg(style, true);
        for (from, to) in shapes {
            let mut glow = CursorGlow::default();
            let mut out = Vec::new();
            let mut trail = CursorTrail::default();
            let mut trail_out = Vec::new();
            glow.tick(Some(from), t0, &c, g, &mut out);
            trail.tick(Some(from), t0, &trail_cfg, &mut trail_out);
            let cold = t0 + Duration::from_millis(16);
            glow.tick(Some(to), cold, &c, g, &mut out);
            trail.tick(Some(to), cold, &trail_cfg, &mut trail_out);
            let shape = format!("{style:?} {from:?}→{to:?}");
            assert!(
                !frame_has_output(&out, &glow),
                "{shape}: an unlicensed move emitted geometry"
            );
            assert_eq!(glow.spawns(), 0, "{shape}: an unlicensed move spawned");
            assert_eq!(glow.live_sparks(), 0, "{shape}: unlicensed sparks");
            assert_eq!(
                glow.drain_sound_cues().count(),
                0,
                "{shape}: an unlicensed move cued sound"
            );
            assert_eq!(
                [glow.heat, glow.coal, glow.flare, glow.quench,],
                [0.0f32; 4],
                "{shape}: an unlicensed move bought thermals"
            );
            assert_eq!(
                glow.typing_momentum(cold),
                0.0,
                "{shape}: an unlicensed move earned momentum"
            );
            assert!(
                glow.crown_until.is_none(),
                "{shape}: an unlicensed move armed the crown"
            );
            assert!(
                trail_out.is_empty(),
                "{shape}: an unlicensed move laid a classic comet"
            );
        }
    }

    for style in styles {
        let c = cfg(style, true);
        for (_, to) in shapes {
            let mut glow = CursorGlow::default();
            let mut out = Vec::new();
            let mut trail = CursorTrail::default();
            let mut trail_out = Vec::new();
            glow.tick(Some((2, 3)), t0, &c, g, &mut out);
            trail.tick(Some((2, 3)), t0, &trail_cfg, &mut trail_out);
            let typed = t0 + Duration::from_millis(8);
            glow.note_synthetic_typed(typed, 1);
            trail.note_synthetic_typed(typed);
            glow.tick(Some((2, 4)), typed, &c, g, &mut out);
            trail.tick(Some((2, 4)), typed, &trail_cfg, &mut trail_out);
            let shape = format!("{style:?} (2,4)→{to:?}");
            let earned_sparks = glow.live_sparks();
            let earned_births = glow.spawns();
            let earned_heat = glow.heat.to_bits();
            let earned_trail: Vec<(usize, usize)> =
                trail_out.iter().map(|cell| (cell.row, cell.col)).collect();
            assert!(
                earned_births > 0,
                "{shape}: the licensed typed move must earn light to begin with"
            );
            glow.tick(Some(to), typed, &c, g, &mut out);
            trail.tick(Some(to), typed, &trail_cfg, &mut trail_out);
            assert_eq!(
                glow.spawns(),
                earned_births,
                "{shape}: the unlicensed move minted light"
            );
            assert_eq!(
                glow.live_sparks(),
                earned_sparks,
                "{shape}: the unlicensed move retired earned light"
            );
            assert_eq!(
                glow.heat.to_bits(),
                earned_heat,
                "{shape}: the unlicensed move moved the thermals"
            );
            assert_eq!(
                trail_out
                    .iter()
                    .map(|cell| (cell.row, cell.col))
                    .collect::<Vec<_>>(),
                earned_trail,
                "{shape}: the unlicensed move disturbed the earned comet"
            );
        }
    }
}

/// The phantom-poof fence: a ContentOnly probe (plain alt screen) never licenses
/// the kill/poof detector.
#[test]
fn content_only_probes_never_license_a_kill_poof() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    let mut glow = CursorGlow::default();
    glow.note_context(true);
    glow.observe_row_with_trust(2, 7, &row("$ hello world"), t0, ProbeTrust::ContentOnly);
    glow.tick(Some((2, 7)), t0, &c, g, &mut out);
    glow.note_kill(t0 + Duration::from_millis(30), true);
    let t1 = t0 + Duration::from_millis(46);
    glow.observe_row_with_trust(2, 7, &row("$ h"), t1, ProbeTrust::ContentOnly);
    glow.tick(Some((2, 7)), t1, &c, g, &mut out);
    assert!(
        glow.vapor.is_empty(),
        "a ContentOnly shrink must not poof — Ctrl-U paged, it did not kill"
    );
    let t2 = t0 + Duration::from_millis(180);
    glow.observe_row_with_trust(2, 7, &row("$ h"), t2, ProbeTrust::ContentOnly);
    glow.tick(Some((2, 7)), t2, &c, g, &mut out);
    assert!(
        glow.vapor.is_empty(),
        "the caret fallback must refuse a ContentOnly probe too"
    );
    let mut glow = CursorGlow::default();
    glow.observe_row(2, 7, &row("$ hello world"), t0);
    glow.tick(Some((2, 7)), t0, &c, g, &mut out);
    glow.note_kill(t0 + Duration::from_millis(30), true);
    glow.observe_row(2, 7, &row("$ h"), t1);
    glow.tick(Some((2, 7)), t1, &c, g, &mut out);
    assert!(
        !glow.vapor.is_empty(),
        "the same shrink under Full trust still poofs — the fence, not the feature, moved"
    );
}
