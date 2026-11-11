// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

const V056_MIN_RUN_KEYS: u32 = 10;
const _: () = assert!(MIN_RUN_KEYS > V056_MIN_RUN_KEYS);

#[test]
fn authenticated_fold_is_bounded_by_wall_time_and_hidden_cats_ignore_it() {
    use crate::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};

    let t0 = Instant::now();
    let mut hidden = CursorCat::default();
    hidden.on_motion_pulse(CursorCatMotionPulse {
        at: t0,
        kind: CursorCatMotionKind::FoldForward,
    });
    assert!(
        hidden.placement_frame(t0).fold.is_none(),
        "one fold pulse cannot materialize an unearned cat"
    );

    let mut cat = CursorCat {
        state: State::Shown,
        ..CursorCat::default()
    };
    cat.on_motion_pulse(CursorCatMotionPulse {
        at: t0,
        kind: CursorCatMotionKind::FoldForward,
    });
    let launch = cat.placement_frame(t0).fold.expect("live cat folds");
    assert_eq!(launch.direction, CatFoldDirection::Forward);
    assert_eq!(launch.progress, 0.0);

    let middle = cat
        .placement_frame(t0 + CURSOR_FOLD_DURATION / 2)
        .fold
        .expect("the midpoint is still a bounded seam");
    assert!((middle.progress - 0.5).abs() < 1e-6);
    assert!(
        cat.placement_frame(t0 + CURSOR_FOLD_DURATION)
            .fold
            .is_none(),
        "a sparse frame at the duration lands immediately"
    );
}

#[test]
fn reverse_fold_changes_place_without_double_delivering_backspace() {
    use crate::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};

    let t0 = Instant::now();
    let mut cat = CursorCat {
        state: State::Shown,
        ..CursorCat::default()
    };
    cat.momentum.set_value(t0, 0.8);
    let before = cat.momentum(t0);
    cat.on_motion_pulse(CursorCatMotionPulse {
        at: t0,
        kind: CursorCatMotionKind::FoldReverse,
    });
    assert_eq!(
        cat.momentum(t0),
        before,
        "the input seam already delivered the physical Backspace"
    );
    let placement = cat.placement_frame(t0);
    assert!(placement.facing_left);
    assert_eq!(
        placement.fold.expect("reverse seam").direction,
        CatFoldDirection::Reverse
    );

    let _ = cat.static_frame(t0 + Duration::from_millis(1));
    let settled = cat.placement_frame(t0 + Duration::from_millis(1));
    assert!(settled.fold.is_none() && !settled.facing_left);
}

const fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 10,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: (40 * 8) as u16,
        win_h: (10 * 16) as u16,
        head: 0,
    }
}

#[test]
fn unowned_cursor_relocation_retires_only_the_earned_flying_episode() {
    let now = Instant::now();
    let mut earned = CursorCat {
        state: State::Shown,
        flight: Some(now),
        ..CursorCat::default()
    };
    earned.momentum.set_value(now, 1.0);
    assert!(earned.is_active());
    earned.retire_unowned_cursor_motion();
    assert!(!earned.is_active());
    assert_eq!(earned.frame(now).alpha, 0);

    let mut promised = CursorCat::default();
    promised.on_collect(now, KittyLook::default());
    promised.retire_unowned_cursor_motion();
    assert!(
        promised.is_active(),
        "a collection hello is independently owned"
    );
    assert!(
        promised
            .frame(now + Duration::from_secs_f32(FADE_IN * 0.5))
            .alpha
            > 0,
        "the independently owned hello remains visible after its fade-in begins"
    );
}

#[test]
fn casual_word_or_two_never_summons_the_cat() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    for i in 0..15u64 {
        c.on_key(t + Duration::from_millis(i * 60), true);
        assert!(
            !c.is_active(),
            "a casual burst must stay cat-free (key {i})"
        );
    }
    let end = t + Duration::from_millis(14 * 60);
    assert!(
        c.momentum(end) < CAT_BAND,
        "a word or two never reaches the {CAT_BAND} band: {}",
        c.momentum(end)
    );
    assert_eq!(c.frame(end + Duration::from_millis(100)).alpha, 0);
}

#[test]
fn sixteen_high_band_keys_are_required_even_after_full_dwell() {
    let t = Instant::now();
    let mut c = CursorCat::default();
    c.momentum.set_value(t, 1.0);
    c.sustain = CAT_DWELL;
    c.last = Some(t);
    for i in 1..MIN_RUN_KEYS {
        c.on_key(t + Duration::from_millis(u64::from(i) * 40), true);
        assert!(
            !c.is_active(),
            "fewer than {MIN_RUN_KEYS} qualifying keys must stay hidden"
        );
    }
    assert_eq!(c.run_keys, MIN_RUN_KEYS - 1);
    c.on_key(
        t + Duration::from_millis(u64::from(MIN_RUN_KEYS) * 40),
        true,
    );
    assert!(c.is_active(), "the sixteenth qualifying key summons");
}

#[test]
fn cat_requires_the_high_band_held_not_merely_touched() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    let mut band_entry_key = None;
    let mut summoned_at_key = None;
    for i in 0..240u64 {
        let ti = t + Duration::from_millis(i * 40);
        if band_entry_key.is_none() && c.momentum(ti) >= CAT_BAND {
            band_entry_key = Some(i);
        }
        c.on_key(ti, true);
        if c.is_active() {
            summoned_at_key = Some(i);
            break;
        }
    }
    let entered = band_entry_key.expect("flat-out typing must reach the band");
    let summoned = summoned_at_key.expect("a long sustained run must summon");
    let held = (summoned - entered) as f32 * 0.040;
    assert!(
        held >= CAT_DWELL - 0.10,
        "the band must be HELD ~{CAT_DWELL} s before the summon, held only {held} s"
    );
    let elapsed = summoned as f32 * 0.040;
    assert!(
        (2.2..=3.6).contains(&elapsed),
        "band entry + dwell must land in the 2.2-3.6 s target window \
         (owner 2026-07-24: 'a little bit less momentum'), got {elapsed} s"
    );
}

#[test]
fn summon_latency_is_bounded_across_typing_cadences() {
    for (gap_ms, max_keys) in [(25u64, 320usize), (60, 160), (120, 90)] {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let first = (0..max_keys).find_map(|i| {
            c.on_key(t + Duration::from_millis(i as u64 * gap_ms), true);
            c.is_active().then_some(i + 1)
        });
        let first =
            first.unwrap_or_else(|| panic!("{gap_ms}ms cadence must summon by key {max_keys}"));
        let elapsed = (first - 1) as f32 * gap_ms as f32 / 1000.0;
        assert!(
            (2.2..=3.6).contains(&elapsed),
            "{gap_ms}ms cadence summoned after {elapsed} s — outside the 2.2-3.6 s target window"
        );
    }
    for stroll_gap in [700u64, 1000] {
        let mut stroll = CursorCat::default();
        let t = Instant::now();
        for i in 0..80u64 {
            stroll.on_key(t + Duration::from_millis(i * stroll_gap), true);
        }
        assert!(
            !stroll.is_active(),
            "a {stroll_gap}ms cadence never holds the high band, so it never summons"
        );
    }
}

#[test]
fn sustained_typing_summons_then_fades_out_completely() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    let mut on = false;
    for i in 0..300 {
        c.on_key(t + Duration::from_millis(i * 25), true);
        if c.is_active() {
            on = true;
            break;
        }
    }
    assert!(on, "a sustained fast run must always summon the cat");
    let tf = t + Duration::from_secs(6);
    for i in 0..30 {
        c.on_key(tf + Duration::from_millis(i * 25), true);
    }
    let hot = tf + Duration::from_millis(30 * 25 + FADE_IN as u64 * 1000 + 400);
    let f = c.frame(hot);
    assert!(
        f.alpha > 200,
        "fades in to (near) full opacity, got {}",
        f.alpha
    );
    let gone = hot + Duration::from_secs(6);
    let done = c.frame(gone);
    assert_eq!(done.alpha, 0, "a sparse late frame is already hidden");
    assert_eq!(done.fp(), 0, "hidden frames settle to the zero repaint key");
    assert!(!c.is_active());
}

#[test]
fn breaths_keep_the_dwell_but_word_pauses_stay_cat_free() {
    let mut flow = CursorCat::default();
    let mut t = Instant::now();
    for burst in 0..12 {
        for _ in 0..16 {
            t += Duration::from_millis(60);
            flow.on_key(t, true);
        }
        if flow.is_active() {
            break;
        }
        if burst < 11 {
            t += Duration::from_millis(150);
        }
    }
    assert!(
        flow.is_active(),
        "sustained flow with sub-200 ms breaths still earns the cat"
    );
    let mut prose = CursorCat::default();
    let mut t = Instant::now();
    for word in 0..3 {
        for _ in 0..7 {
            t += Duration::from_millis(120);
            prose.on_key(t, true);
        }
        if word < 2 {
            t += Duration::from_millis(650);
        }
    }
    assert!(
        !prose.is_active(),
        "three casual words with real pauses must NOT summon (the raised threshold)"
    );
}

#[test]
fn hello_goodbye_is_not_revived_by_typing() {
    let t = Instant::now();
    let mut cat = CursorCat::default();
    cat.on_collect(t, KittyLook::default());
    let fade_at = t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.05);
    let fading = cat.frame(fade_at);
    assert!(fading.collection_hello, "the goodbye is a hello frame");
    assert!(fading.alpha > 0);
    cat.on_key(fade_at + Duration::from_millis(40), true);
    cat.on_key(fade_at + Duration::from_millis(80), true);
    let done = cat.frame(fade_at + Duration::from_secs_f32(FADE_OUT + 0.2));
    assert_eq!(done.alpha, 0, "the goodbye completes");
    assert!(!done.collection_hello, "hello bookkeeping fully cleared");
    assert!(!cat.is_active(), "the machine returned to Hidden");
}

#[test]
fn fading_cat_revives_on_resumed_typing() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    for i in 0..120 {
        c.on_key(t + Duration::from_millis(i * 60), true);
    }
    assert!(c.is_active(), "the run summons the cat");
    let last = t + Duration::from_millis(119 * 60);
    let low_crossing = last
        + Duration::from_secs_f32(
            crate::typing_momentum::TYPING_MOMENTUM_TAU * (c.momentum(last) / LOW).ln(),
        );
    let quiet = low_crossing + Duration::from_millis(200);
    let fading = c.frame(quiet);
    assert!(
        fading.alpha > 0 && fading.alpha < 255,
        "sampling shortly after the real LOW crossing lands mid-fade"
    );
    let observed = fading;
    c.on_key(quiet, true);
    c.on_key(quiet + Duration::from_millis(80), true);
    let back = c.frame(quiet + Duration::from_millis(400));
    assert!(
        back.alpha > observed.alpha,
        "resumed typing revives the fading cat ({} → {})",
        observed.alpha,
        back.alpha
    );
    assert!(c.is_active());
}

#[test]
fn the_exit_flourish_head_never_shows_at_full_opacity() {
    let mut saw_flourish = 0u32;
    let mut saw_plain_head = 0u32;
    for keys in 120..160u64 {
        let mut c = CursorCat::default();
        // An unmarked head: a marked one keeps its own head through the
        // goodbye ([`a_marked_cat_s_goodbye_keeps_its_markings`]).
        c.set_look(KittyLook {
            variant: CatGlyphId::S100,
            ..KittyLook::default()
        });
        let t = Instant::now();
        for i in 0..keys {
            c.on_key(t + Duration::from_millis(i * 60), true);
        }
        assert!(c.is_active(), "the run summons the cat");
        let last = t + Duration::from_millis((keys - 1) * 60);
        let low_crossing = last
            + Duration::from_secs_f32(
                crate::typing_momentum::TYPING_MOMENTUM_TAU * (c.momentum(last) / LOW).ln(),
            );
        for step in 0..120u32 {
            let now = low_crossing + Duration::from_millis(u64::from(step) * 16);
            let f = c.frame(now);
            let swapped = f.render_look().variant != f.look.variant;
            if matches!(f.exit, CatExit::HeartMeow | CatExit::StarWink) && swapped {
                saw_flourish += 1;
                assert!(
                    f.alpha <= 200,
                    "the flourish head appeared while the cat was still at \
                     full brightness (alpha {}, fade_out {}) — a revive in \
                     the next frame swaps it straight back, which is the \
                     one-frame flash",
                    f.alpha,
                    f.fade_out
                );
            } else if f.alpha > 0 && !swapped {
                saw_plain_head += 1;
            }
        }
    }
    assert!(
        saw_plain_head > 0,
        "the goodbye walks emitted no ordinary frames at all"
    );
    assert!(
        saw_flourish > 0,
        "no trial ever rolled a flourish, so the opacity assertion above \
         never ran — this test would be checking nothing"
    );
}

#[test]
fn reduced_motion_never_draws_a_cat_the_machine_calls_hidden() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    let mut unearned = 0u32;
    for step in 0..60u32 {
        let now = t + Duration::from_millis(u64::from(step) * 16);
        c.set_singing(
            now,
            SingSync {
                drive: 1.0,
                beat: 0.0,
            },
        );
        let f = c.static_frame(now);
        if f.alpha > 0 && !c.is_active() {
            unearned += 1;
        }
    }
    assert_eq!(
        unearned, 0,
        "reduced motion drew {unearned} frame(s) of a cat the machine \
         reports inactive"
    );
}

#[test]
fn sparse_frame_after_collection_deadline_settles_directly_hidden() {
    let t = Instant::now();
    let mut cat = CursorCat::default();
    cat.on_collect(t, KittyLook::default());
    assert!(
        cat.frame(t + Duration::from_millis(16)).alpha > 0,
        "the hello was genuinely visible before the long present gap"
    );

    let late = t + Duration::from_secs_f32(DISCOVERY_HOLD + FADE_OUT + 2.0);
    let frame = cat.frame(late);
    assert_eq!(frame.alpha, 0);
    assert_eq!(frame.fp(), 0);
    assert!(!frame.collection_hello);
    assert!(!cat.is_active());
}

#[test]
fn typed_word_reactions_are_bounded_expression() {
    let t = Instant::now();
    let mut hidden = CursorCat::default();
    assert!(
        !hidden.on_delight(t, 1),
        "delight never summons a hidden companion"
    );
    assert!(
        !hidden.on_curse(t, 1),
        "a wince never summons a hidden companion"
    );
    assert!(!hidden.is_active());

    let look = KittyLook::for_launch(7);
    let mut live = CursorCat::default();
    live.on_collect(t, look);
    assert!(live.on_delight(t + Duration::from_millis(5), 1));
    assert_eq!(
        live.frame(t + Duration::from_millis(5)).look,
        look,
        "delight never re-skins the companion"
    );
    assert!(live.on_curse(t + Duration::from_millis(10), 1));
    let frame = live.frame(t + Duration::from_millis(10));
    assert_eq!(frame.reaction, CatReaction::Wince);
    assert_eq!(frame.look, look, "a wince never re-skins the companion");
}

#[test]
fn delight_is_expression_not_displacement() {
    let mut t = Instant::now();
    let look = KittyLook::for_launch(11);
    let mut cat = CursorCat::default();
    let mut plain = CursorCat::default();
    cat.on_collect(t, look);
    plain.on_collect(t, look);
    for _ in 0..8 {
        t += Duration::from_millis(75);
        cat.on_key(t, true);
        plain.on_key(t, true);
    }
    assert!(cat.on_delight(t, 1));
    let mid = t + Duration::from_secs_f32(DELIGHT_HOLD / 2.0);
    let apex = cat.frame(mid);
    let control = plain.frame(mid);
    assert!(
        control.bob.abs() > 0.0 && apex.bob.abs() <= BOB_AMP + 1e-6,
        "fixture must sample a non-zero stride hover: {}",
        control.bob
    );
    assert!(
        (apex.bob - control.bob).abs() < 1e-6,
        "the companion holds its stride hover through a delight: {} vs {}",
        apex.bob,
        control.bob
    );
    assert_eq!(
        apex.reaction,
        CatReaction::Delight,
        "the reaction is still armed"
    );
    assert_eq!(
        apex.pose.eyes,
        EyesFrame::Happy,
        "hearing its own name is still the happiest the companion gets"
    );
    assert!(
        apex.pose.scale_y > control.pose.scale_y,
        "delight swells the pose vertically ({} vs {})",
        apex.pose.scale_y,
        control.pose.scale_y
    );
}

#[test]
fn collection_hello_uses_exact_look_and_bounded_input_reactions() {
    let t = Instant::now();
    let look = KittyLook {
        variant: CatGlyphId::S103,
        accessory: Some(CatGlyphId::AccBow),
        coat: 11,
        iris: 6,
        ..KittyLook::default()
    };
    let mut cat = CursorCat::default();
    cat.on_enter(t);
    assert!(!cat.is_active(), "Enter never summons a hidden companion");

    assert_eq!(cat.run_keys, 0);
    assert!(cat.sustain < CAT_DWELL);
    cat.on_collect(t, look);
    assert!(
        cat.is_active(),
        "an explicit collection/typed hello bypasses earning"
    );
    let hello = cat.frame(t + Duration::from_millis(400));
    assert_eq!(hello.look, look.normalized());
    assert_eq!(hello.render_look(), look.normalized());
    assert!(hello.discovery && hello.collection_hello);

    cat.on_enter(t + Duration::from_millis(500));
    let celebrate = cat.frame(t + Duration::from_millis(510));
    assert_eq!(celebrate.reaction, CatReaction::Celebrate);
    assert_eq!(celebrate.render_look().variant, CatGlyphId::SpecManeki);
    assert_eq!(celebrate.render_look().accessory, None);

    cat.on_key(t + Duration::from_millis(600), false);
    let startled = cat.frame(t + Duration::from_millis(610));
    assert_eq!(startled.reaction, CatReaction::Startled);
    assert_eq!(startled.render_look().variant, CatGlyphId::S121);
    assert_eq!(startled.render_look().accessory, None);

    let settled = cat.frame(t + Duration::from_millis(1300));
    assert_eq!(settled.reaction, CatReaction::Cruise);
    assert_eq!(settled.render_look(), look.normalized());
}

#[test]
fn reduced_motion_flight_retires_and_releases_the_look_latch() {
    let mut cat = CursorCat::default();
    let t = Instant::now();
    let mut summoned_at = None;
    for i in 0..300u64 {
        let ti = t + Duration::from_millis(i * 25);
        cat.on_key(ti, true);
        if cat.is_active() {
            summoned_at = Some(ti);
            break;
        }
    }
    let summoned_at = summoned_at.expect("sustained typing summons the flight");
    let new_look = KittyLook {
        variant: CatGlyphId::S103,
        coat: 7,
        ..KittyLook::default()
    };
    cat.set_look(new_look);
    assert!(
        cat.pending_look.is_some(),
        "the sync parks while the flight is live"
    );
    let mut retired = None;
    for step in 0..400u64 {
        let ts = summoned_at + Duration::from_millis(step * 25);
        let f = cat.static_frame(ts);
        if !cat.is_active() {
            retired = Some(f);
            break;
        }
    }
    let frame = retired.expect("the reduced-motion flight retires to Hidden by time");
    assert_eq!(frame.alpha, 0, "a retired flight is fully gone");
    assert!(!cat.is_active(), "is_active clears once retired");
    assert_eq!(
        cat.look,
        new_look.normalized(),
        "the parked look applies as the flight retires — the latch released"
    );
    assert!(cat.pending_look.is_none(), "…and the parking slot is empty");
}

#[test]
fn reduced_collection_hello_is_one_bounded_static_pose() {
    let t = Instant::now();
    let look = KittyLook {
        variant: CatGlyphId::S103,
        coat: 9,
        ..KittyLook::default()
    };
    let mut cat = CursorCat::default();
    cat.on_collect(t, look);

    assert_eq!(
        cat.static_deadline(t),
        Some(t + Duration::from_secs_f32(DISCOVERY_HOLD))
    );
    let first = cat.static_frame(t);
    let held = cat.static_frame(t + Duration::from_secs(2));
    for frame in [first, held] {
        assert_eq!(frame.alpha, 255);
        assert_eq!(frame.bob, 0.0);
        assert_eq!(frame.fade_out, 0.0);
        assert_eq!(frame.reaction, CatReaction::Discovery);
        assert_eq!(frame.render_look(), look.normalized());
        assert!(frame.collection_hello);
    }

    assert_eq!(
        cat.static_deadline(t + Duration::from_secs(3)),
        None,
        "an occluded erase redraw must not rearm an already-past wake"
    );
    assert!(
        cat.is_active(),
        "deadline filtering does not advance render-owned lifecycle state"
    );
    let gone = cat.static_frame(t + Duration::from_secs(3));
    assert_eq!(gone.alpha, 0);
    assert_eq!(cat.static_deadline(t + Duration::from_secs(3)), None);
    assert!(!cat.is_active());
}

#[test]
fn hidden_time_does_not_consume_collection_hello() {
    let t = Instant::now();
    let look = KittyLook {
        variant: CatGlyphId::SpecSleeping,
        coat: 12,
        iris: 4,
        ..KittyLook::default()
    };
    let mut cat = CursorCat::default();
    cat.on_collect(t, look);
    cat.set_collection_presentable(t, false);

    let hidden = cat.frame(t + Duration::from_secs(30));
    assert_eq!(hidden.alpha, 0, "the hidden fade-in stays frozen");
    assert!(hidden.discovery && hidden.collection_hello);
    assert_eq!(
        cat.static_deadline(t + Duration::from_secs(30)),
        None,
        "hidden cats arm no wake"
    );

    let resumed_at = t + Duration::from_secs(30);
    cat.set_collection_presentable(resumed_at, true);
    assert_eq!(
        cat.static_deadline(resumed_at),
        Some(resumed_at + Duration::from_secs_f32(DISCOVERY_HOLD)),
        "resume restores the complete promised hold"
    );
    let visible = cat.frame(resumed_at + Duration::from_millis(400));
    assert_eq!(visible.alpha, 255);
    assert!(visible.discovery && visible.collection_hello);
    assert_eq!(visible.render_look(), look.normalized());

    let still_held = cat.frame(resumed_at + Duration::from_secs_f32(DISCOVERY_HOLD - 0.001));
    assert!(still_held.discovery, "only drawable time consumes the hold");
}

/// The goodbye face carries no pattern layers, so it would drop a marked
/// cat's coat (nyan's `burst` at l4200). Every head whose art carries
/// markings keeps its own head, coat and iris through both flourishes;
/// the control: an unmarked head still swaps to the flourish face, so the
/// frames below are the goodbye's.
#[test]
fn a_marked_cat_s_goodbye_keeps_its_markings() {
    use crate::cat_glyphs_gen::{GLYPH_IDS, GLYPHS, GlyphKind, GlyphRole};
    let mut marked_heads = 0;
    let mut swapped_plain = 0;
    for (i, def) in GLYPHS.iter().enumerate() {
        if def.kind != GlyphKind::Head {
            continue;
        }
        let variant = GLYPH_IDS[i];
        let look = KittyLook {
            variant,
            coat: 5,
            iris: 2,
            ..KittyLook::default()
        };
        let has_marks = def
            .layers
            .iter()
            .any(|l| matches!(l.role, GlyphRole::Pattern | GlyphRole::CoatShade));
        for exit in [CatExit::StarWink, CatExit::HeartMeow] {
            let mut f = CursorCat::default().frame(Instant::now());
            f.look = look;
            f.exit = exit;
            f.fade_out = 0.6;
            f.alpha = 90;
            let drawn = f.render_look();
            if has_marks {
                assert_eq!(
                    drawn,
                    look.normalized(),
                    "{} waves goodbye ({exit:?}) without its markings",
                    def.id
                );
            } else if drawn.variant != variant {
                swapped_plain += 1;
            }
        }
        marked_heads += usize::from(has_marks);
    }
    assert!(marked_heads > 0, "the roster has marked heads");
    assert!(
        swapped_plain > 0,
        "an unmarked head still makes the flourish face"
    );
}

#[test]
fn collection_goodbye_keeps_the_discovered_art() {
    let t = Instant::now();
    let look = KittyLook {
        variant: CatGlyphId::SpecSleeping,
        coat: 10,
        iris: 3,
        ..KittyLook::default()
    };
    let mut cat = CursorCat::default();
    cat.on_collect(t, look);

    let fade = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD));
    assert!(fade.collection_hello);
    assert_eq!(fade.exit, CatExit::Plain);
    assert_eq!(fade.render_look(), look.normalized());
}

#[test]
fn global_companion_sync_cannot_morph_an_active_discovery() {
    let t = Instant::now();
    let discovered = KittyLook {
        variant: CatGlyphId::SpecSleeping,
        coat: 10,
        ..KittyLook::default()
    };
    let other_window = KittyLook {
        variant: CatGlyphId::SpecYarn,
        coat: 2,
        ..KittyLook::default()
    };
    let mut cat = CursorCat::default();
    cat.on_collect(t, discovered);
    cat.set_look(other_window);
    assert_eq!(
        cat.static_frame(t + Duration::from_millis(100))
            .render_look(),
        discovered.normalized(),
        "frame setup cannot replace the promised discovery art"
    );

    let _ = cat.static_frame(t + Duration::from_secs(3));
    cat.set_look(other_window);
    assert_eq!(
        cat.static_frame(t + Duration::from_secs(3)).render_look(),
        other_window.normalized(),
        "global companion sync resumes after the hello completes"
    );
}

#[test]
fn mid_flight_look_sync_is_latched_until_the_next_wake() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    for i in 0..120 {
        c.on_key(t + Duration::from_millis(i * 60), true);
    }
    assert!(c.is_active(), "the sustained run summons the cat");
    let last = t + Duration::from_millis(119 * 60);
    let flying = c.frame(last + Duration::from_millis(100));
    assert!(flying.alpha > 0);

    let next_look = KittyLook {
        variant: CatGlyphId::SpecYarn,
        coat: 2,
        iris: 7,
        ..KittyLook::default()
    };
    c.set_look(next_look);
    let held = c.frame(last + Duration::from_millis(200));
    assert_eq!(
        held.look, flying.look,
        "a mid-flight sync must not swap the sprite"
    );
    assert_eq!(
        held.look.coat, flying.look.coat,
        "every component of the current look rides the same latch"
    );

    let reviving = last + Duration::from_secs(4);
    let _ = c.frame(reviving);
    let fading = c.frame(reviving + Duration::from_millis(200));
    assert!(
        fading.fade_out > 0.0 && fading.alpha > 0,
        "fixture must sample mid-FadeOut"
    );
    c.on_key(reviving + Duration::from_millis(200), true);
    c.on_key(reviving + Duration::from_millis(240), true);
    assert!(c.is_active(), "typing mid-FadeOut revives the appearance");
    let revived = c.frame(reviving + Duration::from_millis(280));
    assert_eq!(revived.fade_out, 0.0, "FadeOut revived back into FadeIn");
    assert_eq!(
        revived.look, flying.look,
        "a revived fade is the same appearance — still the latched look"
    );

    let fade_again = reviving + Duration::from_secs(4);
    let _ = c.frame(fade_again);
    let gone = fade_again + Duration::from_secs(2);
    let retired = c.frame(gone);
    assert_eq!(retired.alpha, 0);
    assert_eq!(
        retired.look,
        next_look.normalized(),
        "FadeOut completion releases the parked look onto the hidden frame"
    );
    assert_eq!(c.look, next_look.normalized());
    assert!(c.pending_look.is_none());
    assert!(!c.is_active());
    for i in 0..120 {
        c.on_key(gone + Duration::from_millis(i * 60), true);
    }
    assert!(c.is_active(), "the second run re-earns the flight");
    let rewoken = c.frame(gone + Duration::from_millis(119 * 60 + 100));
    assert_eq!(
        rewoken.look,
        next_look.normalized(),
        "the deferred look lands at the start of the next appearance"
    );
}

#[test]
fn discovery_hello_presents_the_new_look_mid_flight() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    for i in 0..120 {
        c.on_key(t + Duration::from_millis(i * 60), true);
    }
    assert!(c.is_active());
    let last = t + Duration::from_millis(119 * 60);

    let parked = KittyLook {
        variant: CatGlyphId::SpecSleeping,
        coat: 12,
        ..KittyLook::default()
    };
    c.set_look(parked);
    let unlocked = KittyLook {
        variant: CatGlyphId::S103,
        accessory: Some(CatGlyphId::AccBow),
        coat: 5,
        iris: 6,
        ..KittyLook::default()
    };
    let collect_at = last + Duration::from_millis(200);
    c.on_collect(collect_at, unlocked);
    let hello = c.frame(collect_at + Duration::from_millis(100));
    assert_eq!(
        hello.look,
        unlocked.normalized(),
        "the hello presents the new collectible immediately"
    );
    assert!(hello.discovery && hello.collection_hello);

    let over = collect_at + Duration::from_secs_f32(DISCOVERY_HOLD + FADE_OUT + 1.0);
    assert_eq!(c.frame(over).alpha, 0);
    assert!(!c.is_active());
    for i in 0..120 {
        c.on_key(over + Duration::from_millis(i * 60), true);
    }
    assert!(c.is_active());
    assert_eq!(
        c.frame(over + Duration::from_millis(119 * 60 + 100)).look,
        unlocked.normalized(),
        "on_collect cleared the parked sync — the collectible stays"
    );
}

#[test]
fn cursor_palette_is_frozen_for_one_visible_episode() {
    let t = Instant::now();
    let mut cat = CursorCat::default();
    cat.on_collect(t, KittyLook::default());
    let dark = CatColorKey {
        accent: 2,
        background: 0,
    };
    let light = CatColorKey {
        accent: 8,
        background: 3,
    };
    assert_eq!(cat.episode_colors(), None);
    assert_eq!(cat.colors_for_episode(dark), dark);
    assert_eq!(cat.episode_colors(), Some(dark));
    assert_eq!(
        cat.colors_for_episode(light),
        dark,
        "terminal churn cannot change this episode's atlas key"
    );

    let _ = cat.static_frame(t + Duration::from_secs(3));
    cat.on_collect(t + Duration::from_secs(4), KittyLook::default());
    assert_eq!(cat.episode_colors(), None);
    assert_eq!(
        cat.colors_for_episode(light),
        light,
        "a new hello samples a new footprint palette"
    );
}

#[test]
fn coordinate_rebase_invalidates_only_the_promised_cat_palette() {
    let t = Instant::now();
    let seeded = CatColorKey {
        accent: 2,
        background: 7,
    };

    let mut hello = CursorCat::default();
    hello.on_collect(t, KittyLook::default());
    let hello_look = hello.look;
    let hello_until = hello.discovery_until;
    assert_eq!(hello.colors_for_episode(seeded), seeded);
    hello.retire_unowned_cursor_motion();
    assert_eq!(
        hello.episode_colors(),
        Some(seeded),
        "the promised hello survives ordinary-flight retirement intact"
    );
    hello.rebase_placement();
    assert_eq!(
        hello.episode_colors(),
        None,
        "the old footprint key is stale"
    );
    assert!(hello.is_active() && hello.collection_hello);
    assert_eq!(
        hello.look, hello_look,
        "the collected identity is unchanged"
    );
    assert_eq!(
        hello.discovery_until, hello_until,
        "the hello's promised lifetime is unchanged"
    );

    let mut song = CursorCat::default();
    arm_singing_after_travel(&mut song, t + Duration::from_secs(1));
    let song_look = song.look;
    assert_eq!(song.colors_for_episode(seeded), seeded);
    song.retire_unowned_cursor_motion();
    assert_eq!(
        song.episode_colors(),
        Some(seeded),
        "the promised song survives ordinary-flight retirement intact"
    );
    song.rebase_placement();
    assert_eq!(
        song.episode_colors(),
        None,
        "the song also resamples its footprint"
    );
    assert!(song.is_active(), "the promised song lifecycle remains live");
    assert_eq!(song.sing, 1.0, "the rebase cannot cancel the song");
    assert_eq!(song.look, song_look, "the singing identity is unchanged");
    assert_eq!(
        song.colors_for_episode(CatColorKey {
            accent: 9,
            background: 1,
        }),
        CatColorKey {
            accent: 9,
            background: 1,
        },
        "the first frame in the new coordinate space seeds a fresh key"
    );
}

#[test]
fn exit_fx_emits_for_wink_and_heart_only() {
    let g = geom();
    let mut out = Vec::new();
    let mut halos = Vec::new();
    emit_exit_fx(CatExit::Plain, 0.5, 40, 40, true, g, &mut out, &mut halos);
    assert!(out.is_empty(), "plain exit draws nothing");
    assert!(halos.is_empty(), "plain exit draws no veil either");
    emit_exit_fx(
        CatExit::HeartMeow,
        0.5,
        40,
        40,
        true,
        g,
        &mut out,
        &mut halos,
    );
    assert!(
        !out.is_empty(),
        "heart draws additive quads mid-exit on dark"
    );
    assert!(
        halos.is_empty(),
        "dark heart never uses the source-over veil"
    );
    out.clear();
    emit_exit_fx(
        CatExit::StarWink,
        0.5,
        40,
        40,
        true,
        g,
        &mut out,
        &mut halos,
    );
    assert!(
        !out.is_empty(),
        "star draws additive quads mid-exit on dark"
    );
    assert!(
        halos.is_empty(),
        "dark star never uses the source-over veil"
    );
}

#[test]
fn exit_fx_light_theme_is_a_darkening_source_over_veil() {
    let g = geom();
    let luma = |rgb: u32| {
        let r = ((rgb >> 16) & 0xff) as f32;
        let g = ((rgb >> 8) & 0xff) as f32;
        let b = (rgb & 0xff) as f32;
        0.299 * r + 0.587 * g + 0.114 * b
    };
    for exit in [CatExit::HeartMeow, CatExit::StarWink] {
        let mut out = Vec::new();
        let mut halos = Vec::new();
        emit_exit_fx(exit, 0.5, 40, 40, false, g, &mut out, &mut halos);
        assert!(
            out.is_empty(),
            "{exit:?}: light theme emits NO additive quad (never white-on-white)"
        );
        assert!(
            !halos.is_empty(),
            "{exit:?}: light theme emits a visible source-over veil"
        );
        for h in &halos {
            assert!(
                matches!(h.mode, HaloMode::Over),
                "{exit:?}: the light veil composites SOURCE-OVER, not additive"
            );
            let ink = h.color & 0x00FF_FFFF;
            assert!(
                luma(ink) < 160.0,
                "{exit:?}: veil ink must be dark enough to contrast on light, luma {}",
                luma(ink)
            );
            let centre_alpha = (h.color >> 24) & 0xff;
            assert!(
                centre_alpha >= 96,
                "{exit:?}: veil centre opacity must be legible, got {centre_alpha}"
            );
            assert!(
                centre_alpha as f32 <= InkRole::OverText.alpha_cap(),
                "{exit:?}: the veil can land on text, so it stays under the \
                 over-text legibility ceiling ({centre_alpha})"
            );
        }
    }
}

#[test]
fn exit_fx_light_veil_vanishes_at_the_exit_ends() {
    let g = geom();
    for &fade in &[0.0_f32, 0.02, 0.99, 1.0] {
        let mut out = Vec::new();
        let mut halos = Vec::new();
        emit_exit_fx(
            CatExit::HeartMeow,
            fade,
            40,
            40,
            false,
            g,
            &mut out,
            &mut halos,
        );
        assert!(
            halos.is_empty(),
            "the light heart veil is nil at fade_out {fade}"
        );
    }
}

#[test]
fn a_key_steps_the_hover_bob_over_frames_not_in_one() {
    for (frame_us, key_us) in [
        (8_333u64, 111_000u64),
        (8_333, 66_000),
        (16_667, 111_000),
        (16_667, 66_000),
    ] {
        let label = format!("a present every {frame_us} µs, a key every {key_us} µs");
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let bob_of = |stride: f32| (std::f32::consts::TAU * stride).sin() * BOB_AMP;
        let (mut prev, mut prev_pump) = (None::<f32>, None::<f32>);
        let (mut step, mut pump_step) = (0.0f32, 0.0f32);
        let mut drawn = std::collections::BTreeSet::new();
        let mut next_key = 40_000;
        for i in 0..(3_000_000 / frame_us) {
            let at = 40_000 + i * frame_us;
            let ti = t + Duration::from_micros(at);
            if at >= next_key {
                cat.on_key(ti, true);
                next_key += key_us;
            }
            let f = cat.frame(ti);
            let pump = bob_of(cat.stride);
            if let Some(p) = prev {
                step = step.max((f.bob - p).abs());
            }
            if let Some(p) = prev_pump {
                pump_step = pump_step.max((pump - p).abs());
            }
            drawn.insert((f.bob * 4096.0) as i64);
            (prev, prev_pump) = (Some(f.bob), Some(pump));
        }
        assert!(
            step <= 0.06,
            "{label}: a present moved the bob {step} of a row"
        );
        assert!(
            drawn.len() > 20,
            "{label}: the bob still moves with the keys"
        );
        assert!(
            pump_step > 0.1,
            "{label}: control: the pump jumps a key's half cycle ({pump_step})"
        );
    }
}

#[test]
fn stride_pump_animates_from_cadence_not_from_the_pinned_level() {
    let look = KittyLook::default();
    let t = Instant::now();
    let mut cat = CursorCat::default();
    cat.on_collect(t, look);

    let mut poses = Vec::new();
    let mut disps = Vec::new();
    for i in 0..30 {
        let ti = t + Duration::from_millis(40 + i * 16);
        cat.on_key(ti, true);
        let f = cat.frame(ti);
        poses.push((f.pose.scale_y, f.bob));
        disps.push(cat.disp);
    }
    let tail = &poses[15..];
    assert!(
        disps[15..].iter().all(|d| *d > 0.99),
        "precondition: the canonical metric IS pinned during a real run, so a \
         level-driven pose cannot animate here"
    );
    let squash: std::collections::BTreeSet<i64> =
        tail.iter().map(|(sy, _)| (sy * 4096.0) as i64).collect();
    assert!(
        squash.len() > 2,
        "the body must still pump while the level is pinned, got {} distinct \
         values from {} frames",
        squash.len(),
        tail.len()
    );
    let bobs: std::collections::BTreeSet<i64> =
        tail.iter().map(|(_, b)| (b * 4096.0) as i64).collect();
    assert!(bobs.len() > 2, "and the bob steps with it: {bobs:?}");

    let mut fast = CursorCat::default();
    fast.on_collect(t, look);
    let mut slow = CursorCat::default();
    slow.on_collect(t, look);
    for i in 0..20 {
        fast.on_key(t + Duration::from_millis(40 + i * 16), true);
    }
    for i in 0..5 {
        slow.on_key(t + Duration::from_millis(40 + i * 64), true);
    }
    let at = t + Duration::from_millis(400);
    let _ = fast.frame(at);
    let _ = slow.frame(at);
    assert!(
        fast.stride > slow.stride,
        "more keys in the same wall time = more strides ({} vs {})",
        fast.stride,
        slow.stride
    );

    let mut blinky = CursorCat::default();
    blinky.on_collect(t, look);
    let mut saw_blink = false;
    for i in 0..140 {
        let ti = t + Duration::from_millis(40 + i * 16);
        blinky.on_key(ti, true);
        if blinky.frame(ti).pose.eyes == EyesFrame::Blink {
            saw_blink = true;
        }
    }
    assert!(saw_blink, "a cat in sustained flight must still blink");

    let mut rest = CursorCat::default();
    rest.on_collect(t, look);
    let r = rest.frame(t + Duration::from_millis(1600));
    assert!(
        r.pose.scale_x < 1.03 && r.pose.scale_y > 0.97,
        "a resting cat keeps its natural silhouette ({} x {})",
        r.pose.scale_x,
        r.pose.scale_y
    );
}

#[test]
fn banking_leans_and_stretches_with_momentum() {
    let look = KittyLook::default();
    let t = Instant::now();
    let mut fast = CursorCat::default();
    fast.on_collect(t, look);
    let mut ff = fast.frame(t + Duration::from_millis(16));
    for i in 0..26 {
        let ti = t + Duration::from_millis(40 + i * 16);
        fast.on_key(ti, true);
        ff = fast.frame(ti);
    }
    assert!(
        ff.pose.scale_x > 1.08,
        "fast flight stretches forward, got {}",
        ff.pose.scale_x
    );
    assert!(
        ff.pose.lead > 0.08,
        "fast flight leans ahead, got {}",
        ff.pose.lead
    );
    assert!(ff.pose.scale_y < 1.0, "fast flight thins vertically");

    let mut slow = CursorCat::default();
    slow.on_collect(t, look);
    let sf = slow.frame(t + Duration::from_millis(1600));
    assert!(sf.alpha > 0, "still visible during the discovery hold");
    assert!(
        sf.pose.scale_x < 1.03,
        "a resting cat is near natural width, got {}",
        sf.pose.scale_x
    );
    assert!(
        sf.pose.lead < 0.03,
        "a resting cat does not lean, got {}",
        sf.pose.lead
    );
    assert!(
        ff.pose.scale_x > sf.pose.scale_x + 0.05,
        "momentum, not chance, selects the lean"
    );
}

#[test]
fn landing_squash_fires_on_stop_and_on_enter() {
    let look = KittyLook::default();
    let t = Instant::now();

    let mut stomp = CursorCat::default();
    stomp.on_collect(t, look);
    let _ = stomp.frame(t + Duration::from_millis(80));
    stomp.on_enter(t + Duration::from_millis(120));
    let hit = stomp.frame(t + Duration::from_millis(130));
    assert!(
        hit.pose.scale_y < 0.95,
        "the Enter stomp squashes vertically, got {}",
        hit.pose.scale_y
    );
    let settled =
        stomp.frame(t + Duration::from_millis(130) + Duration::from_secs_f32(LAND_DUR + 0.05));
    assert!(
        settled.pose.scale_y > hit.pose.scale_y + 0.05,
        "the bounce releases back toward neutral"
    );

    let mut stop = CursorCat::default();
    stop.on_collect(t, look);
    let _ = stop.frame(t + Duration::from_millis(80));
    let land = stop.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.06));
    assert!(land.alpha > 0, "the touchdown frame is still drawn");
    assert!(
        land.pose.scale_y < 0.95,
        "landing on a stop squashes, got {}",
        land.pose.scale_y
    );
}

#[test]
fn idle_blink_is_deterministic_and_windowed() {
    let build = || {
        let mut cat = CursorCat::default();
        let t = Instant::now();
        cat.on_collect(t, KittyLook::default());
        (cat, t)
    };
    let pump = |cat: &mut CursorCat, t: Instant, upto_ms: u64| {
        let mut f = cat.frame(t + Duration::from_millis(1000));
        let mut ms = 1000;
        while ms < upto_ms {
            ms = (ms + 100).min(upto_ms);
            f = cat.frame(t + Duration::from_millis(ms));
        }
        f
    };
    let (mut a, t) = build();
    let open = pump(&mut a, t, 1000);
    assert!(open.alpha > 0, "lingering during the hold");
    assert_eq!(
        open.pose.eyes,
        EyesFrame::Open,
        "no blink outside the window"
    );
    let (mut b, t) = build();
    let blink = pump(&mut b, t, 1850);
    assert_eq!(
        blink.pose.eyes,
        EyesFrame::Blink,
        "the blink lands in its deterministic window"
    );
    let (mut c, t) = build();
    let again = pump(&mut c, t, 1850);
    assert_eq!(
        blink.pose.eyes, again.pose.eyes,
        "blink is a pure clock function"
    );
}

#[test]
fn sustained_speed_squints_happily() {
    let t = Instant::now();
    let mut cat = CursorCat::default();
    cat.on_collect(t, KittyLook::default());
    let mut f = cat.frame(t + Duration::from_millis(16));
    for i in 0..24 {
        let ti = t + Duration::from_millis(40 + i * 16);
        cat.on_key(ti, true);
        f = cat.frame(ti);
    }
    assert_eq!(
        f.pose.eyes,
        EyesFrame::Happy,
        "a fast sustained cruise squints, eyes were {:?}",
        f.pose.eyes
    );
}

#[test]
fn delete_burst_is_one_sustained_oops_with_one_kick() {
    let t = Instant::now();
    let mut cat = CursorCat::default();
    cat.on_collect(t, KittyLook::default());
    let _ = cat.frame(t + Duration::from_millis(16));
    let t0 = t + Duration::from_millis(200);
    cat.on_key(t0, false);
    let hit = cat.frame(t0);
    assert_eq!(hit.reaction, CatReaction::Startled);
    assert!(
        hit.pose.scale_y < 0.85,
        "the burst's first delete kicks a visible squash, got {}",
        hit.pose.scale_y
    );
    let mut last = t0;
    for i in 1..=6u64 {
        last = t0 + Duration::from_millis(i * 50);
        cat.on_key(last, false);
    }
    let mid = cat.frame(last);
    assert_eq!(
        mid.reaction,
        CatReaction::Startled,
        "the expression sustains across the burst"
    );
    assert_eq!(mid.render_look().variant, CatGlyphId::S121);
    assert!(
        mid.pose.scale_y > 0.85,
        "a repeat key must NOT re-freeze the bounce at impact, got {}",
        mid.pose.scale_y
    );
    let again = last + Duration::from_millis(500);
    cat.on_key(again, false);
    let rekick = cat.frame(again);
    assert!(
        rekick.pose.scale_y < 0.85,
        "a new burst re-arms the squash kick, got {}",
        rekick.pose.scale_y
    );
}

#[test]
fn oops_holds_visibly_then_decays_to_base_look() {
    let t = Instant::now();
    let mut cat = CursorCat::default();
    cat.on_collect(t, KittyLook::default());
    let _ = cat.frame(t + Duration::from_millis(16));
    let del = t + Duration::from_millis(300);
    cat.on_key(del, false);
    for ms in [100u64, 200, 300] {
        let _ = cat.frame(del + Duration::from_millis(ms));
    }
    let held = cat.frame(del + Duration::from_millis(400));
    assert_eq!(held.reaction, CatReaction::Startled);
    assert_eq!(held.render_look().variant, CatGlyphId::S121);
    assert!(
        held.pose.lead < -0.05,
        "mid-hold the body recoils backward, got lead {}",
        held.pose.lead
    );
    let settled = cat.frame(del + Duration::from_millis(700));
    assert_eq!(settled.reaction, CatReaction::Cruise);
    assert_eq!(settled.render_look(), KittyLook::default().normalized());
    assert!(
        settled.pose.lead > -0.02,
        "the backward recoil releases with the expression, got lead {}",
        settled.pose.lead
    );
}

#[test]
fn kill_chords_fire_the_same_oops_and_never_summon() {
    let t = Instant::now();
    let mut hidden = CursorCat::default();
    hidden.on_kill(t);
    assert!(!hidden.is_active(), "a kill can never summon");
    assert_eq!(hidden.frame(t + Duration::from_millis(16)).alpha, 0);

    let mut live = CursorCat::default();
    live.on_collect(t, KittyLook::default());
    let _ = live.frame(t + Duration::from_millis(16));
    live.on_kill(t + Duration::from_millis(200));
    let f = live.frame(t + Duration::from_millis(210));
    assert_eq!(f.reaction, CatReaction::Startled);
    assert_eq!(f.render_look().variant, CatGlyphId::S121);
    assert_eq!(f.render_look().accessory, None);
}

#[test]
fn a_kill_flood_grounds_the_flight_and_the_goodbye_keeps_the_oops() {
    let t = Instant::now();
    let mut c = CursorCat::default();
    for i in 0..120 {
        c.on_key(t + Duration::from_millis(i * 60), true);
    }
    assert!(c.is_active(), "the run summons the cat");
    let last = t + Duration::from_millis(119 * 60);
    let before = c.momentum(last);
    let kill1 = last + Duration::from_millis(100);
    c.on_kill(kill1);
    assert!(
        c.momentum(kill1) < before && c.momentum(kill1) > LOW,
        "one kill dents the metric without grounding: {} -> {}",
        before,
        c.momentum(kill1)
    );
    assert!(
        c.frame(kill1 + Duration::from_millis(50)).fade_out == 0.0,
        "a single kill does not ground an earned flight"
    );
    let mut kill = kill1;
    for _ in 0..8 {
        kill += Duration::from_millis(80);
        c.on_kill(kill);
    }
    assert!(c.momentum(kill) < LOW, "the flood walks the metric low");
    let f = c.frame(kill + Duration::from_millis(100));
    assert!(
        f.fade_out > 0.0,
        "the flood grounds the flight, so the fade begins"
    );
    assert_eq!(
        f.reaction,
        CatReaction::Startled,
        "the goodbye carries the oops"
    );
    assert_eq!(f.render_look().variant, CatGlyphId::S121);
}

#[test]
fn reduced_motion_oops_is_a_static_swap_with_a_decay_wake() {
    let t = Instant::now();
    let look = KittyLook {
        variant: CatGlyphId::S103,
        coat: 7,
        ..KittyLook::default()
    };
    let mut cat = CursorCat::default();
    cat.on_collect(t, look);
    let del = t + Duration::from_millis(500);
    cat.on_key(del, false);
    let f = cat.static_frame(del + Duration::from_millis(100));
    assert_eq!(f.alpha, 255);
    assert_eq!(f.reaction, CatReaction::Startled);
    assert_eq!(f.render_look().variant, CatGlyphId::S121);
    assert_eq!(f.pose, CatPose::STILL, "no pose kick under reduced motion");
    assert_eq!(f.bob, 0.0);
    assert_eq!(
        cat.static_deadline(del + Duration::from_millis(100)),
        Some(del + Duration::from_secs_f32(OOPS_HOLD)),
        "the oops decay arms its own wake (it precedes the erase deadline)"
    );
    let back = cat.static_frame(del + Duration::from_millis(700));
    assert_eq!(back.reaction, CatReaction::Discovery);
    assert_eq!(back.render_look(), look.normalized());
    assert_eq!(back.pose, CatPose::STILL);
    assert_eq!(
        cat.static_deadline(del + Duration::from_millis(700)),
        Some(t + Duration::from_secs_f32(DISCOVERY_HOLD)),
        "after the decay only the single erase deadline is armed"
    );
}

#[test]
fn hidden_settles_to_zero_work() {
    let t = Instant::now();
    let mut cat = CursorCat::default();
    let idle = cat.frame(t);
    assert_eq!(idle.alpha, 0);
    assert_eq!(idle.fp(), 0, "hidden ⇒ fp settles to 0");
    assert_eq!(idle.pose, CatPose::STILL, "hidden ⇒ neutral pose");
    assert!(!cat.is_active(), "hidden ⇒ the host 60 fps timer disarms");

    cat.on_collect(t, KittyLook::default());
    let _ = cat.frame(t + Duration::from_millis(16));
    let _ = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.05));
    let done = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + FADE_OUT + 0.6));
    assert_eq!(done.alpha, 0, "the flight fades out completely");
    assert_eq!(done.fp(), 0, "settled hidden ⇒ fp 0");
    assert_eq!(done.pose, CatPose::STILL, "settled hidden ⇒ neutral pose");
    assert!(!cat.is_active());
    let after = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + FADE_OUT + 2.0));
    assert_eq!(
        after.fp(),
        0,
        "a later hidden frame stays byte-identical off"
    );
    assert_eq!(after.pose, CatPose::STILL);
}

const fn sync(drive: f32, beat: f32) -> SingSync {
    SingSync { drive, beat }
}

fn arm_singing_after_travel(c: &mut CursorCat, t: Instant) -> Instant {
    c.set_singing(t, sync(1.0, 0.0));
    assert!(!c.is_active(), "an armed hold still owes cursor travel");
    let mut armed_at = t;
    for i in 1..=MIN_RUN_KEYS {
        armed_at = t + Duration::from_millis(u64::from(i) * 40);
        c.on_key(armed_at, true);
        c.set_singing(armed_at, sync(1.0, i as f32 * 0.1));
    }
    assert!(
        c.is_active(),
        "the singing bypass may summon after sixteen qualifying events"
    );
    armed_at
}

#[test]
fn singing_pins_momentum_summons_and_dances_on_the_beat() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    let armed_at = arm_singing_after_travel(&mut c, t);
    assert_eq!(c.momentum(armed_at), 1.0, "the documented momentum bypass");
    let t1 = armed_at + Duration::from_secs_f32(FADE_IN + 0.02);
    c.set_singing(t1, sync(1.0, 2.02));
    let on_beat = c.frame(t1);
    assert_eq!(on_beat.alpha, 255);
    assert_eq!(on_beat.sing, 1.0);
    assert_eq!(
        on_beat.render_look().variant,
        CatGlyphId::S115,
        "the roster's open-mouth meow head IS the singing face"
    );
    let t2 = armed_at + Duration::from_secs_f32(FADE_IN + 0.2);
    c.set_singing(t2, sync(1.0, 2.47));
    let mid_beat = c.frame(t2);
    assert!(
        on_beat.pose.scale_y < mid_beat.pose.scale_y,
        "the on-beat squash pulse must relax across the beat ({} vs {})",
        on_beat.pose.scale_y,
        mid_beat.pose.scale_y
    );
    assert_ne!(
        on_beat.bob, mid_beat.bob,
        "the dance bob rides the beat clock"
    );
}

#[test]
fn the_singing_head_never_blinks_at_the_gate_value() {
    for drive in [0.33_f32, 0.3300001, 0.34, 0.5, 1.0] {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        let mut singing_head = 0;
        let mut blinked_singing = 0;
        for i in 0..190 {
            let now = armed_at + Duration::from_secs_f32(FADE_IN + 0.016 * i as f32);
            c.set_singing(now, sync(drive, 0.0));
            let f = c.frame(now);
            if f.alpha != 0 && f.render_look().variant == CatGlyphId::S115 {
                singing_head += 1;
                blinked_singing += u32::from(f.pose.eyes == EyesFrame::Blink);
            }
        }
        assert!(
            singing_head > 0,
            "drive={drive}: the fixture never reached the singing head"
        );
        assert_eq!(
            blinked_singing, 0,
            "drive={drive}: the open-mouth singing head blinked"
        );
    }
    let mut c = CursorCat::default();
    let t = Instant::now();
    let armed_at = arm_singing_after_travel(&mut c, t);
    let mut blinks = 0;
    for i in 0..600 {
        let now = armed_at + Duration::from_secs_f32(FADE_IN + 0.016 * i as f32);
        c.set_singing(now, sync(0.0, 0.0));
        blinks += u32::from(c.frame(now).pose.eyes == EyesFrame::Blink);
    }
    assert!(blinks > 0, "fixture: a cat off the song must still blink");
}

#[test]
fn oops_outranks_the_singing_face() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    let armed_at = arm_singing_after_travel(&mut c, t);
    let t1 = armed_at + Duration::from_secs_f32(FADE_IN + 0.05);
    c.set_singing(t1, sync(1.0, 1.1));
    c.on_key(t1, false);
    let f = c.frame(t1 + Duration::from_millis(16));
    assert_eq!(f.render_look().variant, CatGlyphId::S121, "oops wins");
}

#[test]
fn wind_down_crossfades_the_dance_out() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    let armed_at = arm_singing_after_travel(&mut c, t);
    let t1 = armed_at + Duration::from_secs_f32(FADE_IN + 0.05);
    c.set_singing(t1, sync(0.5, 3.05));
    let half = c.frame(t1);
    assert_eq!(half.sing, 0.5);
    assert_eq!(
        half.render_look().variant,
        CatGlyphId::S115,
        "half drive still sings"
    );
    let t2 = t1 + Duration::from_millis(100);
    c.set_singing(t2, sync(0.0, 0.0));
    let off = c.frame(t2);
    assert_eq!(off.sing, 0.0);
    assert_ne!(
        off.render_look().variant,
        CatGlyphId::S115,
        "at zero drive the singing face hands back to the collected look"
    );
    assert!(
        c.momentum(t2) > 0.7,
        "the bypassed metric hands back to NATURAL decay (still high right after): {}",
        c.momentum(t2)
    );
}

#[test]
fn reduced_motion_celebration_is_a_static_singing_pose() {
    let mut c = CursorCat::default();
    let t = Instant::now();
    let armed_at = arm_singing_after_travel(&mut c, t);
    c.set_singing(armed_at, sync(1.0, 5.3));
    let f = c.static_frame(armed_at);
    assert_eq!(f.alpha, 255, "static celebration presents fully opaque");
    assert_eq!(f.pose, CatPose::STILL, "no dance loop under reduced motion");
    assert_eq!(f.bob, 0.0, "no bob under reduced motion");
    assert_eq!(f.render_look().variant, CatGlyphId::S115);
    let late = armed_at + Duration::from_millis(600);
    c.set_singing(late, sync(0.49, 6.3));
    let retained = c.static_frame(late);
    assert_eq!(retained.alpha, 255, "late wind-down keeps opaque custody");
    assert_eq!(retained.render_look().variant, CatGlyphId::S115);
    let t1 = armed_at + Duration::from_millis(800);
    c.set_singing(t1, sync(0.3, 7.3));
    let off = c.static_frame(t1);
    assert_eq!(off.alpha, 0, "one-step disappearance, no fade animation");
}

#[test]
fn pausing_a_collection_hello_shifts_delight_oops_and_landing_clocks_on_resume() {
    let mut c = CursorCat::default();
    let t0 = Instant::now();
    c.on_collect(t0, KittyLook::default());
    assert!(c.on_delight(t0, 2));
    let t_pause = t0 + Duration::from_millis(40);
    let before_pause = c.frame(t_pause);
    assert_eq!(before_pause.reaction, CatReaction::Delight);
    let u_before = c.delight_arc(t_pause);
    assert!(
        (0.0..1.0).contains(&u_before),
        "summon delight hop is mid-arc before pause: {u_before}"
    );

    c.set_collection_presentable(t_pause, false);
    let t_resume = t_pause + Duration::from_secs(3);
    c.set_collection_presentable(t_resume, true);

    let u_resume = c.delight_arc(t_resume);
    assert!(
        (u_resume - u_before).abs() < 1e-4,
        "resuming after 3s hidden preserves exact delight hop progress ({u_before} vs {u_resume})"
    );
    let after_resume = c.frame(t_resume + Duration::from_millis(16));
    assert_eq!(after_resume.reaction, CatReaction::Delight);
    assert!(
        c.delight_arc(t_resume + Duration::from_millis(16)) < 1.0,
        "delight hop envelope continues smoothly after resume instead of snapping expired"
    );
}

#[test]
fn pausing_a_collection_hello_shifts_fold_and_meteor_and_collect_clears_stale_touchdown() {
    use crate::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
    use crate::rainbow_kitty::Dir;

    let mut c = CursorCat::default();
    let t0 = Instant::now();
    c.on_collect(t0, KittyLook::default());
    c.on_motion_pulse(CursorCatMotionPulse {
        at: t0,
        kind: CursorCatMotionKind::FoldForward,
    });
    let mut frame = c.frame(t0);
    c.on_meteor(
        Flight {
            dir: Dir::Right,
            t0,
            land_at: t0 + Duration::from_millis(120),
            land: (2, 10),
        },
        t0,
        &mut frame,
    );
    let t_pause = t0 + Duration::from_millis(40);
    let fold_before = c
        .placement_frame(t_pause)
        .fold
        .expect("fold active before pause")
        .progress;
    c.set_collection_presentable(t_pause, false);

    let t_mid_pause = t_pause + Duration::from_secs(2);
    let fold_paused = c
        .placement_frame(t_mid_pause)
        .fold
        .expect("fold remains frozen while collection hello is paused")
        .progress;
    assert!((fold_paused - fold_before).abs() < 1e-6);

    let t_resume = t_pause + Duration::from_secs(3);
    c.set_collection_presentable(t_resume, true);
    let fold_resumed = c
        .placement_frame(t_resume)
        .fold
        .expect("fold resumes at pre-pause progress")
        .progress;
    assert!((fold_resumed - fold_before).abs() < 1e-6);
    assert_eq!(
        c.land_at(),
        Some(t0 + Duration::from_millis(120) + Duration::from_secs(3)),
        "meteor landing clock shifts by hidden span"
    );
    assert!(
        c.meteor.is_some_and(|m| m.land_at == c.land_at().unwrap()),
        "meteor flight struct shifts in lockstep with land_at"
    );

    c.on_motion_pulse(CursorCatMotionPulse {
        at: t_resume,
        kind: CursorCatMotionKind::FoldReverse,
    });
    assert!(c.placement_frame(t_resume).facing_left);
    let t_recollect = t_resume + Duration::from_millis(20);
    c.on_collect(t_recollect, KittyLook::default());
    assert_eq!(c.land_at(), None);
    assert!(c.meteor.is_none());
    let recollected_pf = c.placement_frame(t_recollect);
    assert!(
        recollected_pf.fold.is_none() && !recollected_pf.facing_left,
        "on_collect must reset stale fold and reverse facing"
    );

    let mut reduced = CursorCat::default();
    let hello_look = KittyLook::for_app("claude");
    let parked_look = KittyLook::for_app("vim");
    assert_ne!(hello_look, parked_look);
    reduced.on_collect(t0, hello_look);
    reduced.set_look(parked_look);
    assert_eq!(reduced.static_frame(t0).look, hello_look);
    let retired = reduced.static_frame(t0 + Duration::from_secs(5));
    assert_eq!(retired.alpha, 0);
    assert_eq!(
        retired.look, parked_look,
        "static_frame collection-hello retirement must install parked pending_look"
    );
    assert_eq!(reduced.look, parked_look);
}

#[test]
fn delight_chain_pulses_multiple_beats_with_scaled_swell() {
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(120);
    let mut base = CursorCat::default();
    let mut single = CursorCat::default();
    let mut chained = CursorCat::default();
    for cat in [&mut base, &mut single, &mut chained] {
        cat.on_collect(t1, KittyLook::default());
        cat.state = State::Shown;
    }

    single.on_delight(t1, 1);
    chained.on_delight(t0, 1);
    chained.on_delight(t1, 1);
    assert_eq!(single.delight_chain, 1);
    assert_eq!(chained.delight_chain, 2);

    let quarter = Duration::from_secs_f32(DELIGHT_HOLD * 0.25);
    let half = Duration::from_secs_f32(DELIGHT_HOLD * 0.5);
    let three_quarter = Duration::from_secs_f32(DELIGHT_HOLD * 0.75);

    let base_q = base.frame(t1 + quarter);
    let single_q = single.frame(t1 + quarter);
    let chained_q = chained.frame(t1 + quarter);
    assert!(
        chained_q.pose.scale_y > single_q.pose.scale_y
            && single_q.pose.scale_y > base_q.pose.scale_y,
        "chained delight first beat swells higher than a single delight at quarter hold"
    );

    let base_h = base.frame(t1 + half);
    let single_h = single.frame(t1 + half);
    let chained_h = chained.frame(t1 + half);
    assert!(
        (chained_h.pose.scale_y - base_h.pose.scale_y).abs() < 1e-5
            && single_h.pose.scale_y > base_h.pose.scale_y + 0.01,
        "chained delight dips to baseline between beats at mid-hold: chained={} base={}",
        chained_h.pose.scale_y,
        base_h.pose.scale_y
    );

    let base_tq = base.frame(t1 + three_quarter);
    let single_tq = single.frame(t1 + three_quarter);
    let chained_tq = chained.frame(t1 + three_quarter);
    assert!(
        chained_tq.pose.scale_y > single_tq.pose.scale_y
            && single_tq.pose.scale_y > base_tq.pose.scale_y,
        "chained delight rises for its second beat at three-quarter hold"
    );
}

#[test]
fn on_collect_during_fadeout_reseeds_spine_to_open_banked() {
    let mut t = Instant::now();
    let mut from_hidden = CursorCat::default();
    let mut from_fadeout = CursorCat::default();
    let look_a = KittyLook::for_app("claude");
    let look_b = KittyLook::for_app("vim");

    from_fadeout.on_collect(t, look_a);
    let until = t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.10);
    while t < until {
        t += Duration::from_millis(16);
        let _ = from_fadeout.frame(t);
    }
    let fading = from_fadeout.frame(t);
    assert!(matches!(from_fadeout.state, State::FadeOut(_)));
    assert!(fading.alpha > 0);
    assert_eq!(fading.pose.eyes, EyesFrame::Open);

    from_hidden.on_collect(t, look_b);
    from_fadeout.on_collect(t, look_b);
    let f_hidden = from_hidden.frame(t);
    let f_fadeout = from_fadeout.frame(t);
    assert_eq!(f_fadeout.pose.eyes, EyesFrame::Happy);
    assert_eq!(f_fadeout.pose.lead, f_hidden.pose.lead);
    assert_eq!(from_fadeout.disp, 1.0);
}

#[test]
fn hidden_frames_skip_stride_work_and_on_collect_syncs_stride() {
    let t0 = Instant::now();
    let mut cat = CursorCat::default();
    assert_eq!(cat.stride_at, None);

    let f1 = cat.frame(t0);
    let f2 = cat.frame(t0 + Duration::from_millis(16));
    assert_eq!((f1.alpha, f1.bob, f1.pose), (0, 0.0, CatPose::STILL));
    assert_eq!((f2.alpha, f2.bob, f2.pose), (0, 0.0, CatPose::STILL));
    assert_eq!(cat.stride_at, None);
    assert_eq!(cat.stride, 0.0);
    assert_eq!(cat.stride_shown, 0.0);

    cat.on_key(t0 + Duration::from_millis(32), true);
    cat.on_key(t0 + Duration::from_millis(64), true);
    let stride_before = cat.stride;
    let shown_before = cat.stride_shown;
    assert!(stride_before > shown_before);
    let _ = cat.frame(t0 + Duration::from_millis(80));
    assert_eq!(cat.stride, stride_before);
    assert_eq!(cat.stride_shown, shown_before);

    cat.on_collect(t0 + Duration::from_millis(96), KittyLook::for_app("claude"));
    assert_eq!(cat.stride_at, None);
    assert_eq!(cat.stride_shown, cat.stride);
}

#[test]
fn resample_nearest_into_reuses_capacity_and_upscales_nearest() {
    let src: [u8; 16] = [
        10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
    ];
    let mut buf = Vec::with_capacity(256);
    let ptr_before = buf.as_ptr();
    let cap_before = buf.capacity();

    resample_nearest_into(&src, 2, 2, 4, 3, &mut buf);
    let (p00, p01, p10, p11) = (
        [10, 20, 30, 255],
        [40, 50, 60, 255],
        [70, 80, 90, 255],
        [100, 110, 120, 255],
    );
    let want: Vec<u8> = [p00, p00, p01, p01, p00, p00, p01, p01, p10, p10, p11, p11].concat();
    assert_eq!(
        buf, want,
        "2x2 -> 4x3 samples rows [0,0,1] and columns [0,0,1,1]"
    );
    assert_eq!(buf.as_ptr(), ptr_before);
    assert_eq!(buf.capacity(), cap_before);

    resample_nearest_into(&src, 2, 2, 2, 2, &mut buf);
    assert_eq!(&buf[..], &src[..]);
    assert_eq!(buf.as_ptr(), ptr_before);

    resample_nearest_into(&src, 2, 2, 0, 3, &mut buf);
    assert!(buf.is_empty());
    assert_eq!(buf.capacity(), cap_before);
}
