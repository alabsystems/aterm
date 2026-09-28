// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unknown-width inserts and their orphan keys.

use super::*;

/// A real queued key follows an unknown-width insert on the FIFO wire,
/// while its dispatch stamp precedes the insert's delivery receipt. The
/// first frame shows only the insert's placeholder; cleanup runs after
/// two seconds with the key still unpaid. This is the Tier-1 projection
/// shared by the positive and refusal paths below.
fn unknown_insert_orphan_key_setup(
    t0: Instant,
    keys: usize,
    site: bool,
) -> (CursorGlow, Vec<GlowQuad>, [char; 40]) {
    unknown_insert_orphan_key_setup_with_glyph(t0, keys, site, 'k')
}

fn unknown_insert_orphan_key_setup_with_glyph(
    t0: Instant,
    keys: usize,
    site: bool,
    first_glyph: char,
) -> (CursorGlow, Vec<GlowQuad>, [char; 40]) {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let at = |tick_ms: u64| t0 + ms(tick_ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut glow, t0, &mut out);
    for (time, glyph) in [(800, first_glyph), (810, 'm'), (820, 'q')]
        .into_iter()
        .take(keys)
    {
        let class = if glyph == ' ' {
            rk::TypedClass::Space
        } else {
            rk::TypedClass::Glyph
        };
        glow.note_typed_expected(at(time), 1, false, class, glyph);
        glow.revoke_input_hints_at(at(time)); // queued behind the paste
    }
    glow.note_insert_delivered_from(at(600), at(900), InsertWidth::Unknown);
    for i in 0..keys {
        glow.note_delivered(at(905 + i as u64), DeliveredClass::Typed);
    }
    let mut row = [' '; 40];
    row[2..13].fill('x');
    if site {
        glow.observe_print_anchor(Some((3, 13, 1)));
        glow.observe_row(3, 13, &row, at(910));
    }
    glow.tick(Some((3, 13)), at(910), &c, g, &mut out);
    assert_eq!(glow.spawns(), 4, "the insert lays its own hop");
    if site {
        glow.observe_row(3, 13, &row, at(3_000));
    }
    glow.tick(Some((3, 13)), at(3_000), &c, g, &mut out);
    assert_eq!(glow.typed_credits_within(at(3_000)), 0);
    (glow, out, row)
}

#[test]
fn unknown_insert_orphan_key_exact_echo_matches_derived_model() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let model = aterm_spec::derive::unknown_insert_orphan_key_model();
    let mut state = model.init_state();
    for action in ["QueueOne", "LayWithSite", "Cleanup"] {
        assert!(model.fire(action, &mut state));
    }
    let (mut glow, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 1, true);
    assert_eq!(
        usize::from(state["escrow"] != 0),
        usize::from(glow.insert.orphan_exact.is_some())
    );
    assert_eq!(
        state["generic"],
        glow.typed_credits_within(at(3_000)) as i64
    );
    row[13] = 'k';
    glow.observe_print_anchor(Some((3, 14, 2)));
    glow.observe_row(3, 14, &row, at(3_200));
    glow.tick(Some((3, 14)), at(3_200), &c, g, &mut out);
    assert!(model.fire("ExactNewPrint", &mut state));
    assert_eq!(glow.spawns() - 4, state["lit"] as u64);
    assert!(
        v2_cols(&glow, 3).contains(&13),
        "the queued key's cell is lit"
    );
    assert_eq!(glow.insert.orphan_exact.is_some(), state["escrow"] != 0);
    assert_eq!(glow.typed_credits_within(at(3_200)), 0);

    row[14] = 'z';
    glow.observe_print_anchor(Some((3, 15, 3)));
    glow.observe_row(3, 15, &row, at(3_400));
    glow.tick(Some((3, 15)), at(3_400), &c, g, &mut out);
    assert!(model.fire("LaterProgramPrint", &mut state));
    assert_eq!(glow.spawns() - 4, state["lit"] as u64, "the key spent once");

    for (keys, printed, print_seq, action) in [
        (1, 'z', 2, "AmbientOtherGlyph"),
        (0, 'z', 2, "AmbientOtherGlyph"),
        (1, 'k', 1, "ExactOldPrint"),
    ] {
        let (mut control, mut cout, mut chars) = unknown_insert_orphan_key_setup(t0, keys, true);
        let mut control_state = model.init_state();
        assert!(model.fire(
            if keys == 0 { "QueueNone" } else { "QueueOne" },
            &mut control_state
        ));
        for step in ["LayWithSite", "Cleanup"] {
            assert!(model.fire(step, &mut control_state));
        }
        chars[13] = printed;
        control.observe_print_anchor(Some((3, 14, print_seq)));
        control.observe_row(3, 14, &chars, at(3_200));
        control.tick(Some((3, 14)), at(3_200), &c, g, &mut cout);
        assert!(model.fire(action, &mut control_state));
        assert_eq!(control.spawns() - 4, control_state["lit"] as u64);
        assert!(!v2_cols(&control, 3).contains(&13));
        assert!(control.insert.orphan_exact.is_none());
        assert_eq!(control.typed_credits_within(at(3_200)), 0);
    }
}

#[test]
fn two_unknown_insert_orphan_keys_light_only_on_separate_exact_echoes() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |millis: u64| t0 + ms(millis);
    let model = aterm_spec::derive::unknown_insert_orphan_key_model();
    let mut state = model.init_state();
    for action in ["QueueTwo", "LayWithSite", "Cleanup"] {
        assert!(model.fire(action, &mut state));
    }
    let (mut glow, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 2, true);
    assert_eq!(
        glow.typed_credits_within(at(3_000)),
        state["generic"] as usize
    );
    assert_eq!(
        glow.insert.orphan_exact.map_or(0, |run| run.len - run.next),
        state["escrow"] as usize
    );

    row[13] = 'k';
    glow.observe_print_anchor(Some((3, 14, 2)));
    glow.observe_row(3, 14, &row, at(3_200));
    glow.tick(Some((3, 14)), at(3_200), &c, g, &mut out);
    assert!(model.fire("ExactNewPrint", &mut state));
    assert_eq!(glow.spawns() - 4, state["lit"] as u64);
    assert!(v2_cols(&glow, 3).contains(&13));
    assert!(
        rendered_ribbon_cols(&glow, &out, 3, g).contains(&13),
        "the first key paints"
    );
    assert_eq!(
        glow.insert.orphan_exact.map_or(0, |run| run.len - run.next),
        state["escrow"] as usize,
        "the second key stays out of the generic credit pool"
    );
    assert_eq!(glow.typed_credits_within(at(3_200)), 0);

    row[14] = 'm';
    glow.observe_print_anchor(Some((3, 15, 3)));
    glow.observe_row(3, 15, &row, at(3_400));
    glow.tick(Some((3, 15)), at(3_400), &c, g, &mut out);
    assert!(model.fire("SecondExactNewPrint", &mut state));
    assert_eq!(glow.spawns() - 4, state["lit"] as u64);
    assert!([13, 14].iter().all(|col| v2_cols(&glow, 3).contains(col)));
    assert!(
        [13, 14]
            .iter()
            .all(|col| rendered_ribbon_cols(&glow, &out, 3, g).contains(col)),
        "both typed cells paint as one run"
    );
    assert!(glow.insert.orphan_exact.is_none());
    assert_eq!(glow.typed_credits_within(at(3_400)), 0);

    // A later live key joins the two recovered cells with no dark notch.
    glow.note_typed_expected(at(3_600), 1, false, rk::TypedClass::Glyph, 'q');
    row[15] = 'q';
    glow.observe_print_anchor(Some((3, 16, 4)));
    glow.observe_row(3, 16, &row, at(3_600));
    glow.tick(Some((3, 16)), at(3_600), &c, g, &mut out);
    assert_eq!(glow.spawns(), 7);
    assert!(
        [13, 14, 15]
            .iter()
            .all(|col| v2_cols(&glow, 3).contains(col))
    );
    assert!(
        [13, 14, 15]
            .iter()
            .all(|col| rendered_ribbon_cols(&glow, &out, 3, g).contains(col)),
        "the next live key sees one continuous painted run"
    );

    // A later keyless program print cannot borrow either spent key.
    row[16] = 'z';
    glow.observe_print_anchor(Some((3, 17, 5)));
    glow.observe_row(3, 17, &row, at(3_800));
    glow.tick(Some((3, 17)), at(3_800), &c, g, &mut out);
    assert_eq!(glow.spawns(), 7);
    // The smooth ribbon can blur into the neighbouring pixel column;
    // owned field cells and the admission count are the licence check.
    assert!(!v2_cols(&glow, 3).contains(&16));
}

#[test]
fn two_unknown_insert_orphans_reject_unrelated_or_stale_second_prints() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |millis: u64| t0 + ms(millis);
    let model = aterm_spec::derive::unknown_insert_orphan_key_model();
    for (prior, glyph, print_seq, action) in [
        ('k', 'z', 3, "SecondAmbientOtherGlyph"),
        ('k', 'm', 2, "SecondExactOldPrint"),
        ('z', 'm', 3, "SecondPrefixRewrite"),
    ] {
        let mut state = model.init_state();
        for step in ["QueueTwo", "LayWithSite", "Cleanup", "ExactNewPrint"] {
            assert!(model.fire(step, &mut state));
        }
        let (mut glow, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 2, true);
        row[13] = 'k';
        glow.observe_print_anchor(Some((3, 14, 2)));
        glow.observe_row(3, 14, &row, at(3_200));
        glow.tick(Some((3, 14)), at(3_200), &c, g, &mut out);
        assert_eq!(glow.spawns(), 5);

        row[13] = prior;
        row[14] = glyph;
        glow.observe_print_anchor(Some((3, 15, print_seq)));
        glow.observe_row(3, 15, &row, at(3_400));
        glow.tick(Some((3, 15)), at(3_400), &c, g, &mut out);
        assert!(model.fire(action, &mut state));
        assert_eq!(glow.spawns() - 4, state["lit"] as u64);
        assert!(!v2_cols(&glow, 3).contains(&14));
        assert!(glow.insert.orphan_exact.is_none());
        assert_eq!(glow.typed_credits_within(at(3_400)), 0);

        // Even a later matching glyph cannot reclaim the discarded key.
        row[15] = 'm';
        glow.observe_print_anchor(Some((3, 16, 4)));
        glow.observe_row(3, 16, &row, at(3_600));
        glow.tick(Some((3, 16)), at(3_600), &c, g, &mut out);
        assert_eq!(glow.spawns(), 5);
        assert!(!v2_cols(&glow, 3).contains(&15));
    }

    let (mut no_site, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 2, false);
    assert!(no_site.insert.orphan_exact.is_none());
    row[13] = 'k';
    no_site.observe_print_anchor(Some((3, 14, 2)));
    no_site.observe_row(3, 14, &row, at(3_200));
    no_site.tick(Some((3, 14)), at(3_200), &c, g, &mut out);
    assert_eq!(no_site.spawns(), 4, "no sampled site means no key claim");

    let (mut unrelated, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 2, true);
    let mut unrelated_state = model.init_state();
    for action in ["QueueTwo", "LayWithSite", "Cleanup", "AmbientOtherGlyph"] {
        assert!(model.fire(action, &mut unrelated_state));
    }
    row[13] = 'z';
    unrelated.observe_print_anchor(Some((3, 14, 2)));
    unrelated.observe_row(3, 14, &row, at(3_200));
    unrelated.tick(Some((3, 14)), at(3_200), &c, g, &mut out);
    assert_eq!(
        unrelated.spawns() - 4,
        unrelated_state["lit"] as u64,
        "a different program glyph stays dark"
    );
    assert!(unrelated.insert.orphan_exact.is_none());
    assert_eq!(unrelated_state["escrow"], 0);
    assert_eq!(unrelated.typed_credits_within(at(3_200)), 0);
    assert!(!v2_cols(&unrelated, 3).contains(&13));
}

#[test]
fn two_unknown_insert_orphans_coalesced_exact_cells_light_without_generic_credit() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |millis: u64| t0 + ms(millis);
    let model = aterm_spec::derive::unknown_insert_orphan_key_model();
    let mut state = model.init_state();
    for action in [
        "QueueTwo",
        "LayWithSite",
        "Cleanup",
        "CoalescedTwoExactNewPrint",
    ] {
        assert!(model.fire(action, &mut state));
    }
    let (mut glow, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 2, true);
    row[13] = 'k';
    row[14] = 'm';
    glow.observe_print_anchor(Some((3, 15, 2)));
    glow.observe_row(3, 15, &row, at(3_200));
    glow.tick(Some((3, 15)), at(3_200), &c, g, &mut out);
    assert_eq!(glow.spawns() - 4, 1, "one observed batch, one admission");
    assert_eq!(state["lit"], 2, "both exact cells earned light");
    assert!([13, 14].iter().all(|col| v2_cols(&glow, 3).contains(col)));
    assert!(
        [13, 14]
            .iter()
            .all(|col| rendered_ribbon_cols(&glow, &out, 3, g).contains(col)),
        "both exact typed cells paint in the coalesced frame"
    );
    assert!(glow.insert.orphan_exact.is_none());
    assert_eq!(
        glow.typed_credits_within(at(3_200)),
        state["generic"] as usize
    );

    // A later keyless program print cannot spend either batch credit.
    row[15] = 'z';
    glow.observe_print_anchor(Some((3, 16, 3)));
    glow.observe_row(3, 16, &row, at(3_400));
    glow.tick(Some((3, 16)), at(3_400), &c, g, &mut out);
    assert_eq!(glow.spawns(), 5);
    // Pixel bloom may extend next door; no field at 15 means no keyless
    // program cell was admitted into the typed ribbon.
    assert!(!v2_cols(&glow, 3).contains(&15));
}

#[test]
fn two_unknown_insert_orphans_reject_partial_wrong_stale_or_unprobed_batch() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |millis: u64| t0 + ms(millis);
    let model = aterm_spec::derive::unknown_insert_orphan_key_model();
    for (second, print_seq, probed, action) in [
        ('z', 2, true, "CoalescedTwoWrongOrPartial"),
        (' ', 2, true, "CoalescedTwoWrongOrPartial"),
        ('m', 1, true, "CoalescedTwoOldPrint"),
        ('m', 2, false, "CoalescedTwoMissingProbe"),
    ] {
        let mut state = model.init_state();
        for step in ["QueueTwo", "LayWithSite", "Cleanup", action] {
            assert!(model.fire(step, &mut state));
        }
        let (mut glow, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 2, true);
        row[13] = 'k';
        row[14] = second;
        glow.observe_print_anchor(Some((3, 15, print_seq)));
        if probed {
            glow.observe_row(3, 15, &row, at(3_200));
        }
        glow.tick(Some((3, 15)), at(3_200), &c, g, &mut out);
        assert_eq!(glow.spawns() - 4, 0, "{action} claimed the batch");
        assert_eq!(state["lit"], 0);
        assert!(glow.insert.orphan_exact.is_none());
        assert_eq!(glow.typed_credits_within(at(3_200)), 0);
        assert!(!v2_cols(&glow, 3).contains(&13));
        assert!(!v2_cols(&glow, 3).contains(&14));
    }

    let (mut no_site, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 2, false);
    row[13] = 'k';
    row[14] = 'm';
    no_site.observe_print_anchor(Some((3, 15, 2)));
    no_site.observe_row(3, 15, &row, at(3_200));
    no_site.tick(Some((3, 15)), at(3_200), &c, g, &mut out);
    assert_eq!(no_site.spawns(), 4, "no insert site cannot claim two keys");
    assert!(!v2_cols(&no_site, 3).contains(&13));
    assert!(!v2_cols(&no_site, 3).contains(&14));

    let (mut too_many, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 3, true);
    let mut too_many_state = model.init_state();
    for action in ["QueueThree", "LayWithSite", "Cleanup", "CoalescedTooMany"] {
        assert!(model.fire(action, &mut too_many_state));
    }
    row[13] = 'k';
    row[14] = 'm';
    too_many.observe_print_anchor(Some((3, 15, 2)));
    too_many.observe_row(3, 15, &row, at(3_200));
    too_many.tick(Some((3, 15)), at(3_200), &c, g, &mut out);
    assert_eq!(too_many.spawns() - 4, too_many_state["lit"] as u64);
    assert_eq!(too_many.typed_credits_within(at(3_200)), 0);
    assert!(!v2_cols(&too_many, 3).contains(&13));
    assert!(!v2_cols(&too_many, 3).contains(&14));

    // On the alternate screen, the normal coalesced classifier needs a
    // repaint blink. An exact orphan proof cannot restore credits for a
    // spawn that would then skip the typed sweep and leave both cells dark.
    let (mut alt, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 2, true);
    let mut alt_state = model.init_state();
    for action in [
        "QueueTwo",
        "LayWithSite",
        "Cleanup",
        "CoalescedTwoAltWithoutBlink",
    ] {
        assert!(model.fire(action, &mut alt_state));
    }
    alt.note_context(true);
    row[13] = 'k';
    row[14] = 'm';
    alt.observe_print_anchor(Some((3, 15, 2)));
    alt.observe_row(3, 15, &row, at(3_200));
    alt.tick(Some((3, 15)), at(3_200), &c, g, &mut out);
    assert_eq!(alt.spawns() - 4, alt_state["lit"] as u64);
    assert!(!v2_cols(&alt, 3).contains(&13));
    assert!(!v2_cols(&alt, 3).contains(&14));
    assert!(alt.insert.orphan_exact.is_none());
    assert_eq!(alt.typed_credits_within(at(3_200)), 0);
}

#[test]
fn unknown_insert_orphan_space_cannot_claim_an_unchanged_blank() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let model = aterm_spec::derive::unknown_insert_orphan_key_model();
    let mut state = model.init_state();
    for action in ["QueueBlank", "LayWithSite", "Cleanup"] {
        assert!(model.fire(action, &mut state));
    }
    let (mut glow, mut out, row) = unknown_insert_orphan_key_setup_with_glyph(t0, 1, true, ' ');
    assert_eq!(glow.insert.orphan_exact.is_some(), state["escrow"] != 0);
    assert_eq!(glow.typed_credits_within(at(3_000)), 0);
    // A program print elsewhere advances the PTY print generation, then
    // moves the caret one cell. The candidate cell never changed.
    glow.observe_print_anchor(Some((10, 8, 2)));
    glow.observe_row(3, 14, &row, at(3_200));
    glow.tick(Some((3, 14)), at(3_200), &c, g, &mut out);
    assert!(model.fire("ExactNewPrint", &mut state));
    assert_eq!(glow.spawns() - 4, state["lit"] as u64);
    assert!(!v2_cols(&glow, 3).contains(&13));
}

#[test]
fn unknown_insert_orphan_key_escrow_expires_and_drops_on_space_changes() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let model = aterm_spec::derive::unknown_insert_orphan_key_model();
    for action in ["Expire", "Scroll", "Rewrite", "Reset"] {
        let (mut glow, mut out, mut row) = unknown_insert_orphan_key_setup(t0, 1, true);
        let mut state = model.init_state();
        for step in ["QueueOne", "LayWithSite", "Cleanup"] {
            assert!(model.fire(step, &mut state));
        }
        match action {
            "Expire" => {
                row[13] = 'k';
                glow.observe_print_anchor(Some((3, 14, 2)));
                glow.observe_row(3, 14, &row, at(11_000));
                glow.tick(Some((3, 14)), at(11_000), &c, g, &mut out);
                assert_eq!(glow.spawns(), 4, "an expired key stayed dark");
            }
            "Scroll" => glow.note_scroll(1),
            "Rewrite" => glow.retract_insert(3, 10, 3, at(3_100)),
            "Reset" => glow.reset(),
            _ => unreachable!(),
        }
        assert!(model.fire(action, &mut state));
        assert_eq!(glow.insert.orphan_exact.is_some(), state["escrow"] != 0);
        assert_eq!(glow.typed_credits_within(at(11_000)), 0);
    }
    let (without_site, _, _) = unknown_insert_orphan_key_setup(t0, 1, false);
    let mut no_site = model.init_state();
    for step in ["QueueOne", "LayWithoutSite", "Cleanup"] {
        assert!(model.fire(step, &mut no_site));
    }
    assert_eq!(
        without_site.insert.orphan_exact.is_some(),
        no_site["escrow"] != 0
    );
}

/// The host composes the normal and under-ink streams for one frame;
/// sample both without appending under-quads to `out` across test ticks.
fn rendered_ribbon_cols(
    glow: &CursorGlow,
    out: &[GlowQuad],
    row: u16,
    g: Geom,
) -> std::collections::BTreeSet<u16> {
    let mut cols = ribbon_cols(out, row, g);
    cols.extend(ribbon_cols(glow.under_quads(), row, g));
    cols
}
