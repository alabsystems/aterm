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
    // The grid's STORED row (what the trimmed probe capture shows): a
    // glyph echo stores its cell; a space echoed over an implicit blank
    // leaves the trimmed capture unchanged — the owner's shape.
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

/// THE OWNER'S MASH (stage 1 of the 2026-08 complaints): *"when I typed
/// that quickly, the cursor trail broke"* — a captured frame of the real
/// window showed the mash line with ZERO ribbon pixels. At mash speed
/// several presses land inside one frame slip, and the proof era's cohort
/// discipline retired the WHOLE pending cohort on the second press ("two
/// keys in flight across one frame slip kills both"): nothing ever
/// confirmed at exactly the speed where the ribbon should be most
/// glorious.
///
/// Under the LICENSE law a press is a press: three of them inside one
/// frame slip stamp three CELL CREDITS, and the coalesced 3-cell echo
/// that follows is priced against exactly that budget — so it classifies
/// as continued TYPING (`rainbow_coalesce`) and lays all three swept
/// cells. Drives 12 presses at 3 per frame and asserts the band CLIMBS
/// every frame, ends long, and never records a decline.
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

/// COLD CONTROL for the mash: the SAME frame choreography — same 3-cell
/// caret hops, same probes — with NO presses. Cold output buys nothing:
/// every hop is DECLINED at the license seam for want of a fresh hint,
/// not one ribbon segment is born, and the ring says exactly why.
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

/// The design's predicate names `synthetic_note_pending` as its seventh
/// license term. It needs no field of its own — both scripted-preview
/// notes already stamp a hint the license reads — and this pins that
/// equivalence, so a future edit that stops stamping either one fails
/// here instead of silently darkening every preview, example and bench.
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

/// TIER 1 for the licence: the REAL `CursorGlow`, driven through the four
/// states the model names — press, echo, cold echo, expiry — with every
/// observed step validated as a transition of `CursorHintLicense`.
///
/// Tier 0 proves the law over the whole bounded space; this is the half
/// that says the law is about THIS ENGINE. Every projected term is READ
/// from the engine — the licence seam itself (`move_licensed`, plus a stamp
/// that outlived its own window), the slimmed diagnosis ring, and whether
/// any light is on glass — so a seam that stopped answering the licence
/// question, a ring that misreported it, or a denial path that wiped earned
/// light would all fail here rather than in prose.
#[test]
fn the_real_glow_engine_conforms_to_the_licence_model() {
    let model = aterm_spec::derive::cursor_hint_license_model();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);

    // The engine -> model projection. `arms`/`consumed` are the presses this
    // test made and the echoes it watched the engine pair stamps with;
    // `forfeited` the credits it watched an edge forget; every other term
    // comes off the engine — including the in-flight POOL
    // (`typed_credits_within`), so `spent` is the real ring's reading and
    // the decline guards (an EMPTY pool, since the one-press echo) are
    // bound against the engine and not against a bookkeeping constant.
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
        // The conservation, projected: an arm this test made and did not
        // watch an echo pair with, whose stamp is no longer live, expired.
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

    // ---- PRESS, ECHO, COLD ECHO on one engine ----
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

    // COLD ECHO: the same caret advance with nobody's finger behind it. The
    // ring names the licence seam, nothing is born — and the ribbon the
    // press just earned is still there.
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

    // ---- EXPIRY on a fresh engine: THE PRESS IN FLIGHT ----
    // A press whose echo comes late: the stamp is still SET, and no longer
    // fresh — and the press is still IN FLIGHT, so its own +1 echo is
    // licensed by the pool (`InFlightPressEchoMintsLight`: the stalled
    // last key) and the press is spent. The stamp was never
    // the licence.
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
    // ---- ONE PRESS IN FLIGHT AGAINST A HOP WIDER THAN ONE CELL ----
    // The same stale stamp over the same one press, and a keyless +2:
    // the engine refuses it (one press cannot buy two cells) and KEEPS
    // the credit for the insert receipt and the next key's bridge —
    // `StaleStampMoveDeclines` at a pool of one, which forfeits nothing.
    // The model carries no hop width, so at one press both this and the
    // one-press echo are enabled; the engine picks by width.
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

    // ---- A STALE STAMP WITH NO PRESS IN FLIGHT on a fresh engine ----
    // One press, then a keyless BACKWARD hop the echo shape refuses: the
    // pool is forgotten with the refusal, and the +1 that follows finds a
    // stale stamp over an EMPTY pool — `StaleStampMoveDeclines`' true
    // shape. (The backward hop is held as a park candidate for one stamp
    // window first — the Ink rewrite's park-and-return — and lands its
    // verdict on the next tick past the window: the forget is
    // DEFERRED past `TYPE_HINT_FRESH`, `past_park_window()`.)
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

    // ---- THE FORGET on a fresh engine (the in-flight law) ----
    // Two presses in flight, both stamps stale, then a keyless BACKWARD
    // hop the echo shape refuses: the real ring forgets the pool with
    // the refusal, and the step is `UnexplainedHopForgetsCredits`. The
    // pool is READ off the engine (`typed_credits_within`), so a ring
    // that kept its credits across the hop — the model's `Buggy=1` —
    // would project `forfeited = 0` and be refused by the law.
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
    // The backward hop is a park candidate (the Ink rewrite's
    // park-and-return): held for one stamp window, then
    // judged exactly as before on the next tick past it — the forget is
    // DEFERRED past `TYPE_HINT_FRESH` (`past_park_window()`).
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

/// THE SHAPE THAT TOOK THREE FIXES — typing while a program streams.
///
/// A `less`/`vi`/ESC 7-ESC 8 streamer parks its caret, writes a status
/// line somewhere else, restores, and repeats — several batches a second,
/// forever, while the user types into the very same window. Under the
/// proof era each of those batches was an unowned generation that either
/// revoked the in-flight candidate (the split-batch collision) or judged
/// the echo an unowned RELOCATION and wiped the ribbon; the lane was
/// patched three separate times — streaming fence wipes, the observer
/// effect, split-batch collisions — and still went dark on a live box.
///
/// This drives the real frame cadence, which is what the old machinery
/// mishandled: the streamer's save/park/status/restore lands INSIDE one
/// parser batch, so the frame that observes it shows the caret exactly
/// where the restore left it and a probe of the row the streamer wrote.
/// Per key that is three frames — press, streamer batch (no caret delta),
/// then the echo — plus a genuine COLD caret walk from the streamer
/// between keys, which must be declined and must destroy nothing.
///
/// Under the LICENSE there is nothing to collide with: a batch that moves
/// no caret runs no spawn at all, so the press's stamp is still there when
/// its echo arrives, and the cold walk that does move one is declined at
/// the seam. The ribbon GROWS across the whole run.
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

        // …and before its echo lands, a whole streamer batch: ESC 7,
        // park, status write, ESC 8. The caret is back where it started,
        // so this frame presents NO delta — but it DOES present a probe
        // of the row the streamer touched, which is what the generation
        // fence used to judge and wipe on.
        let streamed = at + Duration::from_millis(20);
        glow.observe_row_with_trust(0, 12, &status, streamed, ProbeTrust::ContentOnly);
        glow.tick(Some((row, col)), streamed, &c, g, &mut scratch);

        // THE ECHO, on the row the user is typing into.
        typed.push(ch);
        let echo = at + Duration::from_millis(40);
        glow.observe_row_with_trust(row, col + 1, &typed, echo, ProbeTrust::ContentOnly);
        glow.tick(Some((row, col + 1)), echo, &c, g, &mut scratch);
        segments.push(glow.ribbon_segments());

        // A COLD caret walk from the streamer, once the press's own
        // freshness window has closed: it must be declined, and it must
        // leave the ribbon exactly where the keystrokes left it — the
        // RESIDENT band (`Status::cells`), which is what "retired"
        // means. (The lit-boundary count is not the measure here: the
        // band's ATTACH under the caret cell follows the caret MIRROR,
        // which follows a declined row change at the end
        // of the tick that saw it, so the frame that brings the caret
        // back plans one frame behind and the attach returns on the
        // next; a resident cell is never touched by any of it.)
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

/// THE LICENSE, DENIAL SIDE — `docs/design/EFFECTS-LICENSE-REDESIGN.md`.
///
/// An UNLICENSED move is a cursor delta with no fresh key hint behind it:
/// a program moved the caret and nobody's fingers asked for it. Two
/// clauses, and BOTH are the law:
///
/// 1. IT MINTS NOTHING — no quad, no spark, no thermal, no momentum, no
///    glide sample, no sound cue, no crown — in every shape a cold program
///    can present and in every selectable style. This is deliberately
///    STRONGER than v0.43.0, whose tick spawned on each presented delta
///    and let the one-cell advance in the first row of `shapes` earn heat:
///    the cold token streamer that paint row 9 measures.
/// 2. IT RETIRES NOTHING — the second half, and the NEW behaviour. The old
///    denial path reached `clear_denied_move_visuals` and wiped the ribbon
///    the user's own typing had just earned because an unrelated program
///    moved its caret. Earned light leaves by decay, `note_scroll`
///    translation, or reset. Nothing else.
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
    // streamer walks (row 9's shape), a coalesced hop, a line wrap, a
    // one-column retreat (a spinner rewinding), a row change, and a
    // full-screen warp.
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

    // CLAUSE 2 — earned light survives someone else's output. One licensed
    // typed move lays real light; the same cold shapes then present, and
    // every one of them must leave that light exactly where it was.
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
            // The cold delta rides the SAME instant, so nothing may change
            // by decay: any difference below is the denial path destroying
            // light it did not earn.
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

/// THE PHANTOM-POOF FENCE HOLDS: a ContentOnly probe (plain alt screen)
/// never licenses the kill/poof detector. Ctrl-U in `less`/`vi` is a page
/// scroll — the region scroll shrinks the probed row without erasing
/// anything — and the OLD protection was to withhold the probe entirely.
/// Now the probe flows for the confirm proof, and the poof branches must
/// refuse it: same kill hint, same shrink, zero vapor. The Full-trust
/// twin (a real shell kill) still poofs, so this pin cannot pass
/// vacuously.
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

/// MOMENTUM DECOUPLES FROM ADMISSION (v0.43.0 restoration): the spine
/// advances on the PRESS-HINT half of a typing-shaped echo, so a burst of
/// real keystrokes whose candidates ALL retire (each `note_typed`
/// supersedes the previous cohort and arms no exact candidate — the
/// streaming-collision cadence a busy TUI produces) still climbs the
/// canonical metric, and the starfield density consumer sees `disp` rise.
/// Amplitude, never provenance: every denied spawn is verified to mint no
/// light, and program output without presses stays pinned at zero
/// (`program_output_alone_builds_no_momentum`).
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
