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
    // The grid's stored row (what the trimmed probe capture shows): a glyph
    // echo stores its cell; a space echoed over an implicit blank leaves the
    // trimmed capture unchanged.
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
/// every swept cell. Drives 12 presses at 3 per frame and asserts the band grows
/// every frame, ends long, and records no decline.
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
        // THREE presses inside one frame slip, each banking its own cell
        // credit at the input boundary.
        for k in 0..3usize {
            glow.note_typed_cells(frame_t + Duration::from_millis(k as u64 * 3), 1);
        }
        // ONE coalesced echo batch materializes all three glyphs and one
        // rendered frame observes it.
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
    // The ribbon CLIMBS across the mash: every frame ends with more live
    // typing sparks than the one before, and 12 presses leave a long band.
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

/// Cold control for the mash: the same frame choreography with no presses. Every
/// hop is declined at the licence seam for want of a fresh hint, no ribbon
/// segment is born, and the ring says why.
#[test]
fn cold_three_cell_hops_lay_no_ribbon_and_leave_no_ring() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut scratch = typed_scratch();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 0)), t0, &c, g, &mut scratch);
    let glyphs = ['m', 'a', 's', 'h', 't', 'y', 'p', 'i', 'n', 'g', 'o', 'k'];
    let mut stored: Vec<char> = Vec::new();
    for frame in 0..4usize {
        let base_col = (frame * 3) as u16;
        let frame_t = t0 + Duration::from_millis(frame as u64 * 16 + 1);
        stored.extend_from_slice(&glyphs[frame * 3..frame * 3 + 3]);
        let probe_at = frame_t + Duration::from_millis(12);
        glow.observe_row_with_trust(2, base_col + 3, &stored, probe_at, ProbeTrust::ContentOnly);
        glow.tick(Some((2, base_col + 3)), probe_at, &c, g, &mut scratch);
        assert_eq!(
            glow.ribbon_segments(),
            0,
            "cold frame {frame} must lay no ribbon"
        );
    }
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (0, 4),
        "every cold hop is declined, none licensed"
    );
    assert_eq!(
        tally.last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "and the ring names the license seam as the refuser"
    );
}

/// The design's `synthetic_note_pending` licence term needs no field: both
/// scripted-preview notes already stamp a hint the licence reads. Pinned so an
/// edit that stops stamping either fails here rather than darkening previews,
/// examples and benches.
#[test]
fn a_synthetic_note_licenses_its_own_move() {
    let t0 = Instant::now();
    let mut typed = CursorGlow::default();
    typed.note_synthetic_typed(t0, 1);
    assert!(
        typed.move_licensed(t0),
        "a scripted TYPED note licenses its own move"
    );
    let mut gesture = CursorGlow::default();
    gesture.note_synthetic_move(t0);
    assert!(
        gesture.move_licensed(t0),
        "a scripted GESTURE note licenses its own move"
    );
    // …and both expire on the 0.25 s class, like every other press stamp.
    let stale = t0 + Duration::from_millis(251);
    assert!(!typed.move_licensed(stale), "a scripted typed note expires");
    assert!(
        !gesture.move_licensed(stale),
        "a scripted gesture note expires"
    );
    // The floor: an engine nobody has touched is never licensed. This is
    // the cold-output law in one line.
    assert!(
        !CursorGlow::default().move_licensed(t0),
        "an untouched engine licenses nothing"
    );
}

/// Tier 1 for the licence: the real `CursorGlow` driven through the model's four
/// states (press, echo, cold echo, expiry), with every observed step validated
/// as a transition of `CursorHintLicense`. Every projected term is read from the
/// engine (the seam `move_licensed` plus a stamp outliving its window, the
/// diagnosis ring, whether light is on glass), so a seam, ring or denial path
/// that drifted from the law fails here.
#[test]
fn the_real_glow_engine_conforms_to_the_licence_model() {
    let model = aterm_spec::derive::cursor_hint_license_model();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);

    // The engine → model projection. `arms`/`consumed` are the presses this
    // test made and the echoes it saw paired with stamps; `forfeited` the
    // credits it saw an edge forget; every other term, including the in-flight
    // pool (`typed_credits_within`), is read off the engine, so the decline
    // guards bind to the real ring.
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
        // Conservation: an arm this test made, not paired with an echo, whose
        // stamp is no longer live, expired.
        st.insert("expired", arms - consumed - i64::from(live));
        let licensed = i64::try_from(tally.licensed).expect("bounded tally");
        let declined = i64::try_from(tally.declined).expect("bounded tally");
        // Every licensed row in this drive passed the licence seam
        // (no starved coalesce is driven here).
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

    // ---- Press, echo, cold echo on one engine ----
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    out.clear();
    let seeded = project(&glow, t0, 0, 0, 0);
    assert_eq!(seeded["hint"], 0, "an untouched engine holds no licence");
    assert_eq!(seeded["resident"], 0, "and no light");
    assert_eq!(seeded["spawns"], 0, "a seed tick is not a move");

    // PRESS: the host prices the keystroke at the input boundary; the stamp
    // it writes IS the licence.
    let press_at = t0 + Duration::from_millis(4);
    glow.note_typed_cells(press_at, 1);
    let armed = project(&glow, press_at, 1, 0, 0);
    assert_eq!(armed["hint"], 1, "a press arms the licence");
    assert_eq!(armed["spent"], 0, "the press's credit is in flight");
    bind(&seeded, &armed, "PressArmsLicense", "CursorGlow press");

    // ECHO: the child echoes the glyph, the caret advances, and the move is
    // licensed — light is born and the stamp is spent (one hint, one echo).
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

    // Cold echo: the same caret advance with no finger behind it. The ring
    // names the licence seam, nothing is born, and the pressed ribbon stays.
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

    // ---- Expiry on a fresh engine: the press in flight ----
    // A late echo: the stamp is set but stale, and the press is still in
    // flight, so its own +1 echo is licensed by the pool
    // (`InFlightPressEchoMintsLight`) and the press is spent.
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
    // ---- One press in flight against a hop wider than one cell ----
    // The same stale stamp and one press, and a keyless +2: refused (one press
    // cannot buy two cells) with the credit kept for the insert receipt and
    // the next key's bridge (`StaleStampMoveDeclines` at a pool of one,
    // forfeiting nothing). The model has no hop width, so both this and the
    // one-press echo are enabled at one press; the engine picks by width.
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

    // ---- A stale stamp with no press in flight on a fresh engine ----
    // One press, then a keyless backward hop the echo shape refuses: the pool
    // is forgotten, and the following +1 finds a stale stamp over an empty pool
    // (`StaleStampMoveDeclines`). The backward hop is first held as a park
    // candidate for one stamp window, so the forget lands past
    // `TYPE_HINT_FRESH` (`past_park_window()`).
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

    // ---- The forget on a fresh engine (the in-flight law) ----
    // Two presses in flight, both stamps stale, then a keyless backward hop the
    // echo shape refuses: the ring forgets the pool with the refusal
    // (`UnexplainedHopForgetsCredits`). The pool is read off the engine, so a
    // ring that kept its credits (the model's `Buggy=1`) would project
    // `forfeited = 0` and be refused.
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
    // The model's state for two stale stamps in flight: the one-slot
    // stamp superseded once, expired once, nothing consumed.
    let mut in_flight = model.init_state();
    in_flight.insert("hint", 2);
    in_flight.insert("arms", 2);
    in_flight.insert("credit_arms", 2);
    in_flight.insert("superseded", 1);
    in_flight.insert("expired", 1);
    hop.tick(Some((2, 2)), hop_at, &c, g, &mut out);
    // The backward hop is held as a park candidate for one stamp window and
    // judged on the next tick past it, so the forget lands past
    // `TYPE_HINT_FRESH` (`past_park_window()`).
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

/// Typing while a program streams: a `less`/`vi`/ESC 7-ESC 8 streamer parks its
/// caret, writes a status line elsewhere and restores, several times a second,
/// while the user types in the same window.
///
/// Driven at the real frame cadence: the streamer's save/park/status/restore
/// lands inside one parser batch, so its frame shows the caret where the
/// restore left it plus a probe of the row it wrote. Per key: press, streamer
/// batch (no caret delta), echo; plus a cold streamer caret walk between keys,
/// which must be declined and destroy nothing. A batch moving no caret runs no
/// spawn, so the press's stamp survives to its echo; the ribbon grows across
/// the run.
#[test]
fn typing_paints_through_the_esc7_esc8_streamer_cadence() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut scratch = typed_scratch();
    let mut glow = CursorGlow::default();
    // The alt screen with no repaint blink — the probe class this lane
    // actually presents.
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

        // THE PRESS.
        glow.note_typed_cells(at, 1);

        // Before its echo, a whole streamer batch (ESC 7, park, status write,
        // ESC 8): the caret is back where it was, so the frame presents no
        // delta but does present a probe of the row the streamer touched.
        let streamed = at + Duration::from_millis(20);
        glow.observe_row_with_trust(0, 12, &status, streamed, ProbeTrust::ContentOnly);
        glow.tick(Some((row, col)), streamed, &c, g, &mut scratch);

        // THE ECHO, on the row the user is typing into.
        typed.push(ch);
        let echo = at + Duration::from_millis(40);
        glow.observe_row_with_trust(row, col + 1, &typed, echo, ProbeTrust::ContentOnly);
        glow.tick(Some((row, col + 1)), echo, &c, g, &mut scratch);
        segments.push(glow.ribbon_segments());

        // A cold streamer caret walk after the press's window closed: declined,
        // and the resident band (`Status::cells`) stays exactly as the keys
        // left it. (The band's attach under the caret follows the caret mirror,
        // which follows a declined row change at the end of its tick, so the
        // lit-boundary count lags a frame; resident cells are untouched.)
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
/// unlicensed move is a cursor delta with no fresh key hint behind it. Both
/// clauses hold:
///
/// 1. It mints nothing (no quad, spark, thermal, momentum, glide sample, sound
///    cue or crown) in every shape a cold program can present and every
///    selectable style, including the one-cell advance a token streamer walks.
/// 2. It retires nothing: earned light leaves only by decay, `note_scroll`
///    translation or reset, never because another program moved its caret.
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
        GlowStyle::Custom,
    ];
    // Every shape a cold program presents: the one-cell advance a token
    // streamer walks, a coalesced hop, a line wrap, a one-column retreat (a
    // spinner rewinding), a row change, and a full-screen warp.
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

    // CLAUSE 1 — a cold engine stays cold, whatever the shape or style.
    for style in styles {
        let mut c = cfg(style, true);
        if style == GlowStyle::Custom {
            c.pack = Some(TrailParams::defaults());
        }
        for (from, to) in shapes {
            let mut glow = CursorGlow::default();
            let mut out = Vec::new();
            let mut trail = CursorTrail::default();
            let mut trail_out = Vec::new();
            // Seed the anchor, then present the cold delta 16 ms later.
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

    // Clause 2: earned light survives someone else's output. One licensed typed
    // move lays light; each cold shape must then leave it exactly as it was.
    for style in styles {
        let mut c = cfg(style, true);
        if style == GlowStyle::Custom {
            c.pack = Some(TrailParams::defaults());
        }
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
            // The cold delta rides the same instant, so nothing may change by
            // decay; any difference is the denial path destroying earned light.
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
/// the kill/poof detector. Ctrl-U in `less`/`vi` is a page scroll that shrinks
/// the probed row without erasing anything; the probe flows for the confirm
/// proof but the poof branches refuse it (same kill hint and shrink, zero
/// vapor). The Full-trust twin (a real shell kill) still poofs.
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
    // Plain-alt shape: ContentOnly probes, a fresh kill hint, and a
    // page-scroll shrink of the cursor row.
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
    // …and waiting out the caret fallback's grace buys the hint nothing.
    let t2 = t0 + Duration::from_millis(180);
    glow.observe_row_with_trust(2, 7, &row("$ h"), t2, ProbeTrust::ContentOnly);
    glow.tick(Some((2, 7)), t2, &c, g, &mut out);
    assert!(
        glow.vapor.is_empty(),
        "the caret fallback must refuse a ContentOnly probe too"
    );
    // NON-VACUITY: the identical gesture over FULL-trust probes (primary
    // screen / blinking TUI) still answers with smoke.
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

/// Momentum is decoupled from admission: the spine advances on the press-hint
/// half of a typing-shaped echo, so a burst of real keystrokes whose candidates
/// all retire (the streaming-collision cadence of a busy TUI) still climbs the
/// metric and the starfield density sees `disp` rise. Amplitude, never
/// provenance: every denied spawn mints no light, and program output without
/// presses stays at zero (`program_output_alone_builds_no_momentum`).
#[test]
fn momentum_climbs_through_streaming_collisions_that_retire_every_candidate() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 0)), t, &c, g, &mut out);
    for col in 1..=30u16 {
        t += Duration::from_millis(40);
        // The press stamps the key-time hint, which licenses the echo.
        glow.note_typed(t);
        glow.tick(Some((3, col)), t, &c, g, &mut out);
    }
    assert!(
        glow.typing_momentum(t) > 0.5,
        "momentum climbs across the collision burst: {}",
        glow.typing_momentum(t)
    );
    assert!(
        glow.momentum_display() > 0.05,
        "the starfield density consumer sees the eased spine rise: {}",
        glow.momentum_display()
    );
}
