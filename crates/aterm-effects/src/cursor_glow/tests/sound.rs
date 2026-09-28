// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing sound cues and the key-time ledger.

use super::*;

/// Sound cues ride the spawn edge with the spawn's own classification,
/// and the disable path drops them (proven-inert covers sound too).
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

    // A navigation press LICENSES its own scrub (v0.43.0 law): it cues
    // the cursor sound its shape earns. What it must never do is heat —
    // that is the classifier's job, pinned by
    // `navigation_earns_no_heat_and_no_meteor`.
    glow.note_navigation(t0);
    glow.tick(Some((4, 0)), t0, &c, g, &mut out);
    assert_eq!(glow.heat, 0.0, "a scrub earns no heat, however it sounds");
    let _ = glow.drain_sound_cues().count();

    // Disabled ⇒ any pending cues are dropped, none accrue.
    glow.note_synthetic_typed(t0, 1);
    glow.tick(Some((4, 2)), t0, &c, g, &mut out);
    glow.tick(Some((4, 3)), t0, &cfg(GlowStyle::Water, false), g, &mut out);
    assert_eq!(glow.drain_sound_cues().count(), 0);
}

/// THE NEWEST ERASE KEY OWNS THE POOF. A kill whose echo has not arrived
/// may still be fresh when the user presses an ordinary Backspace. Both
/// licenses used to remain armed, and `poof_scan` classified the resulting
/// one-cell shrink as a kill solely because `kill_hint.is_some()` — adding
/// the clause-scale swoosh on top of the Backspace's own bell.
///
/// The negative control has byte-for-byte identical row/cursor geometry,
/// but follows the host's actual word-Backspace order: generic Backspace
/// bookkeeping first, then the stronger kill classification. That final
/// kill must retain the swoosh. Both class changes keep the banked-typed
/// contract: all older [`TypedStamps`] are retired, not collapsed or spent.
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

    // REGRESSION: an unanswered kill, then a newer ordinary Backspace.
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

    // NEGATIVE CONTROL: identical geometry, but an actual word-Backspace
    // ends with the stronger kill arm, exactly as app_input dispatches it.
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

/// THE RESUME-AFTER-PAUSE BLACK GAP (the ordinal-inversion regression
/// gate). A mid-line thinking pause (1.7-5 s — inside the chain window,
/// past the spark lifetimes) expires the laid sparks; the resume key then
/// pushes its head spark and the four-letter guarantee re-mints the three
/// expired tail cells. When those re-mints were PUSHED (after the head),
/// the sparks Vec left cell order, the rasterizer's ordinal-derived
/// spatial coordinate handed `asp = 1.0` — the far feather's exact zero —
/// to the INTERIOR resume cell, and that one cell printed as pure
/// background for the rest of its life (measured on-glass 3/3: one full
/// black cell a few cells behind the caret after every mid-line pause).
/// Two assertions, both red under the push ordering:
/// 1. the Vec walks the mark tail→head (the invariant `emit_rainbow`'s
///    `sp`/`asp` derivation documents), and
/// 2. the ribbon's lit columns are CONTIGUOUS — the far tail may melt,
///    but no interior cell may go dark while its neighbours draw.
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
    // A steady 350 ms hunt-and-peck lays cols 1..=9 (spark life rides the
    // 1.7 s swoosh floor at this cadence).
    for k in 1..=9u16 {
        t += key;
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, k)), t, &c, g, &mut out);
    }
    // The thinking pause: longer than every live spark's life, shorter
    // than the chain window (5 s), so the resume key re-mints the expired
    // tail rather than refreshing live sparks in place.
    t += Duration::from_millis(4500);
    // Resume at a quicker cadence so the resume cell (col 10) is still
    // resident when enough newer keys exist for its ordinal to reach the
    // far feather's zero under the broken ordering.
    for k in 10..=18u16 {
        t += Duration::from_millis(150);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, k)), t, &c, g, &mut out);
    }
    // 1. CELL ORDER IS VEC ORDER.
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
    // 2. NO INTERIOR HOLE. The resume cell is resident (re-armed by the
    // four-letter chain through col 13) — if the ribbon culled it while
    // drawing both neighbours, that is exactly the on-glass black gap.
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

/// THE FLOOD-TYPING BLACK GAP (the banked-license regression gate). K
/// presses inside one frame gap used to collapse into ONE license stamp
/// (1-deep slot + press-path wipe); when the echo then arrived as more
/// sweeps than stamps, the surplus sweeps were declined at the
/// no-fresh-hint gate and their cells stayed background-black forever
/// (measured on-glass: 5-13 declined cells per 100-key flood, dark%
/// 11.5-13.2 — the owner's-screenshot shape). With [`TypedStamps`], K
/// keys bank K stamps and each observed sweep spends what it covers — and
/// the license law's other half still holds: once the sweeps have spent
/// the stamps, the NEXT program move finds no license.
#[test]
fn flood_presses_bank_licenses_for_every_echo_sweep() {
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed
    // Three keys land inside one frame gap (the host's typed-press
    // supersede keeps the earlier stamps; each press banks its own).
    let k1 = t0 + Duration::from_millis(10);
    let k2 = t0 + Duration::from_millis(12);
    let k3 = t0 + Duration::from_millis(14);
    glow.supersede_typed_press();
    glow.note_synthetic_typed(k1, 1);
    glow.supersede_typed_press();
    glow.note_synthetic_typed(k2, 1);
    glow.supersede_typed_press();
    glow.note_synthetic_typed(k3, 1);
    // The echo arrives as TWO sweeps: a 2-cell coalesced advance, then a
    // 1-cell advance one frame later.
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
    // THE BOUND: licensed sweeps can never exceed banked presses. Three
    // keys paid for three cells: the coalesced `+2` spent two presses
    // and the `+1` the third. The surviving stamp (the coalesce popped
    // one of three) licenses nothing on its own — a stamp is a licence
    // only while a press is unpaid (2026-09-13) — so the very next
    // observed move finds the ledger empty and is declined: program
    // output cannot outspend the keyboard.
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

/// A TYPED STAMP IS A LICENCE ONLY WHILE A PRESS IS UNPAID (2026-09-13
/// audit, R1-2). A three-key burst echoing as ONE coalesced `+3` spends
/// all three presses but popped only ONE stamp, so the two surviving
/// stamps stayed fresh for a quarter second with nothing left to pay
/// for: a keyless program `+1` on the row passed the gate, laid a cell
/// with zero credits (the host sweep), and a keyless wide hop lit its
/// landing. Under Rainbow Kitty the stamp bank now licenses light only
/// while the credit ring still owes a cell; the classic trail's
/// lockstep contract (`move_licensed`) is untouched.
///
/// RED before the fix: `("licensed","key",(3,8),(3,9))` with cell 8 lit
/// and `stamps still fresh: true`; the wide hop `no-credits` yet cell
/// 39 lit.
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
        // A keyless program move 100 ms later, inside the stamp window:
        // one cell, or a 32-cell hop whose landing a dangling stamp lit.
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

/// A KEYLESS HOP THE IN-FLIGHT POOL CANNOT PAY FOR LIGHTS NO LANDING
/// (2026-09-13 review of the audit round — the paste harness's case 5,
/// "a program flood of twelve cells with no key must stay dark", lit its
/// last cell). The dangling-STAMP half was closed by
/// [`a_coalesced_echos_surviving_stamps_license_nothing_once_its_presses_are_paid`];
/// this is the dangling-CREDIT half. A press whose echo the seam never
/// judged — the harness's shape is a hidden-caret TUI's first key on a
/// row the anchored lane had never seen, one per case, four by case 5 —
/// stays on the ring for the whole patience, and two of them open the
/// IN-FLIGHT licence for any same-row forward hop (`unpaid_typed_echo`:
/// a batch, judged under the share rule). The share rule refused the
/// flood (`no-credits`, correctly) and the forget edge dropped the pool
/// — and between the two, the re-anchor's landing sweep (2f15705bc: "a
/// typed re-anchor lays exactly ONE cell, the landing", gated on a press
/// being unpaid) spent one of the dangling credits on the flood's last
/// cell. The landing is v1's law for a hop a KEY licensed inside the
/// stamp window (vim's `w`); a hop no stamp licensed and the pool could
/// not describe is program output and lays nothing.
///
/// RED before the fix: `("no-credits","none",(3,4),(3,16))` with cell
/// 15 lit beside the two echoed keys (the harness read col 17 of its
/// input row at chroma 53, `ribbon_segments 5 -> 12`, 4/4 runs, on
/// every tree since 2f15705bc). The control keeps v1's law: the same
/// hop with a key pressed inside the window still lights its landing.
#[test]
fn a_keyless_hop_the_in_flight_pool_cannot_pay_for_lights_no_landing() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    // Two presses whose echoes the seam never judged: the caret is seen
    // where it was, nothing is laid, the credits stay on the ring.
    glow.note_typed(t0 + ms(10));
    glow.note_typed(t0 + ms(20));
    glow.tick(Some((3, 2)), t0 + ms(30), &c, g, &mut out);
    assert_eq!(glow.spawns(), 0);
    // Two ordinary keys, each echoed and spent on its own frame.
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
    // The program prints twelve cells on the row with no key behind them.
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

    // CONTROL — v1's landing law is untouched: the same twelve-cell hop
    // with a key pressed inside the stamp window (vim's `w` after a
    // press, the box-growth re-anchor) lights exactly its landing cell.
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

/// THE KEY-TIME CLICK (touch-to-glass audio): the click is born at the
/// PHYSICAL keypress, and the echo that arrives a round trip later must
/// NOT click again. The latency this removes is the whole key → PTY →
/// shell → PTY → parse → next-frame path; the failure mode this pins is
/// the double-click that naive key-time cueing produces.
#[test]
fn key_time_click_fires_at_the_key_and_mutes_its_own_echo() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    // Seed the engine (a live tick is what opens the key seam).
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    assert_eq!(glow.drain_sound_cues().count(), 0);

    // The key. One click, immediately — no terminal round trip involved.
    let key = t0 + Duration::from_millis(1);
    assert!(glow.cue_keystroke(key), "a live engine clicks at the key");
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
    assert_eq!(
        cues[0].col, 0,
        "the key-time cue pans from the PRE-echo cursor cell"
    );

    // The echo, 40 ms later (a loaded flood frame): light spawns, sound
    // does not — the credit is spent instead.
    let echo = t0 + Duration::from_millis(41);
    glow.note_synthetic_typed(echo, 1);
    glow.tick(Some((2, 1)), echo, &c, g, &mut out);
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "one character must click exactly once"
    );

    // The NEXT advance has no credit behind it — it clicks at the echo,
    // byte-identically to the pre-seam behaviour.
    let out_echo = t0 + Duration::from_millis(60);
    glow.note_synthetic_typed(out_echo, 1);
    glow.tick(Some((2, 2)), out_echo, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
}

/// RESPONSIVENESS: `take_key_cue` hands the click back to the host so it can
/// reach the synth ON the key thread instead of on the next render tick —
/// the last frame of quantization on the audio half of the feedback.
///
/// Taking it must change WHO carries the cue and nothing else: the credit
/// ledger still mutes the echo (one character, one click), and cues recorded
/// earlier stay queued for the frame drain that owns them.
#[test]
fn take_key_cue_hands_back_the_click_without_touching_the_ledger() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    // An ECHO-born cue the render drain already owns, left undrained.
    let echo = t0 + Duration::from_millis(1);
    glow.note_synthetic_typed(echo, 1);
    glow.tick(Some((2, 1)), echo, &c, g, &mut out);
    assert_eq!(
        glow.sound_cues.len(),
        1,
        "precondition: a cue is queued for the frame drain"
    );

    // The key: record, then take the click straight back.
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

    // The ledger is untouched: this character's echo stays silent.
    let key_echo = t0 + Duration::from_millis(45);
    glow.note_synthetic_typed(key_echo, 1);
    glow.tick(Some((2, 2)), key_echo, &c, g, &mut out);
    let drained: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(
        drained.len(),
        1,
        "only the pre-existing echo cue drains — the credit muted this echo"
    );

    // And a host that takes with nothing recorded gets nothing.
    assert!(
        glow.take_key_cue().is_none(),
        "an empty queue hands back no click"
    );
}

/// REGRESSION (adversarial review): a printable key whose echo lands as a
/// JUMP rather than as `typing` must still speak exactly ONCE, and must not
/// leave its credit behind.
///
/// A bare Character arms the key seam regardless of how far its echo moves the
/// cursor — so vim normal-mode motions (`w b e 0 $ G`), any printable key in
/// less/htop/fzf, and multi-cell IME commits all echo as a Jump. Debiting the
/// ledger only on the `typing` arm therefore produced BOTH audible defects at
/// once: `Typed` at the key plus `Jump` at the echo (which the synth cannot thin,
/// because Jump bypasses MIN_GAP), and an unspent credit that the next typing
/// advance — possibly pure output with no key behind it — silently swallowed.
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

    // The key speaks immediately.
    let key = t0 + Duration::from_millis(1);
    assert!(glow.cue_keystroke(key));
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);

    // Its echo is a multi-cell JUMP (e.g. `w` in vim). The host still arms
    // the committed printable-key witness even though the geometry is not
    // typing; that witness admits the move, which must find the credit and
    // stay silent rather than speak a second time for one keypress.
    let echo = t0 + Duration::from_millis(40);
    glow.note_synthetic_typed(key, 1);
    glow.tick(Some((6, 9)), echo, &c, g, &mut out);
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "one keypress must not produce Typed at the key AND Jump at the echo"
    );

    // ...and the credit is GONE, so a later genuine typing advance is not muted.
    let later = t0 + Duration::from_millis(80);
    glow.note_synthetic_typed(later, 1);
    glow.tick(Some((6, 10)), later, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1, "the Jump must not leak the credit forward");
    assert_eq!(cues[0].kind, SoundKind::Typed);
}

/// REGRESSION: every causally admitted spawn arm handles an outstanding
/// key-time credit without producing a duplicate printable-key click.
///
/// The orphan is reachable: a printable key clicks at the press and arms a
/// credit, then its echo is swallowed (a start-of-line Backspace erases
/// nothing, an app eats the glyph) or folds into the NEXT key's echo. That
/// next echo carries a Backspace or navigation hint, so it takes the
/// `deletion` / `navigation` arm — and an arm that leaves the credit
/// standing leaks it. Because the freshness anchor is re-stamped by every
/// LATER key, a live typing burst keeps the orphan fresh indefinitely, and
/// it is finally spent muting a genuine click.
///
/// Retiring must NOT silence those arms: a Backspace / arrow is a different
/// key that never clicked at press time, so its own gesture still speaks.
/// Each row asserts BOTH halves plus the un-muted follow-up click, so the
/// test cannot be satisfied by simply muting the arm.
#[test]
fn admitted_spawn_arms_handle_the_key_time_ledger() {
    use crate::trail_sound::SoundKind;
    // (arm name, hint to arm before the echo, the echo's landing cell, the
    // gesture that echo must STILL speak — `None` for the arms a printable
    // key's own echo lands on, which suppress themselves by design.)
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

        // The printable key: one click now, one credit owed.
        let key = t0 + Duration::from_millis(1);
        assert!(glow.cue_keystroke(key), "{name}: the key must click");
        assert_eq!(glow.drain_sound_cues().count(), 1, "{name}");

        // The observed echo takes this arm.
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
        // Only the two arms a printable key's echo can land on settle the
        // credit. An exact deletion is a different physical key and leaves
        // the outstanding printable credit standing.
        let settles = matches!(name, "typing" | "jump");
        assert_eq!(
            glow.keyed_clicks,
            u8::from(!settles),
            "{name}: wrong ledger disposition for this arm"
        );

        // The accepted cost is pinned too: the exact deletion leaves the
        // printable credit to mute one later admitted typing echo, while the
        // typing and jump arms already spent it.
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

/// The KEY-TIME CLICK'S TIMBRE must be the timbre of the keystroke it
/// belongs to, not of the one before it.
///
/// `heat` scales the synth's note level (`0.55 + 0.45·heat`) and its ember
/// layer (`0.1 + 0.28·heat`). The echo-born cue this seam replaced was
/// recorded AFTER `spawn`'s heat ramp, so it rode the freshly-charged
/// blaze. Sampling `blaze()` at the key instead put the sound one whole
/// keystroke behind the light — the same lateness the seam removes from the
/// TIMING, reintroduced in the TONE. This pins the fix: the key-time cue
/// carries the charge its own keystroke is about to add, tracking the pure
/// echo-time engine within a few percent (the residue is only the round-trip
/// cooling, and the key-time value is the correct one — it is the heat at
/// the instant the ear actually hears the click).
#[test]
fn the_key_time_click_carries_its_own_keystrokes_heat() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // Two engines, one script: 6 keys at 100 ms (a brisk human cadence,
    // inside the full-credit ramp) with a 5 ms local echo. `keyed` cues at
    // the key; `echo_only` is the pre-seam engine and cues at the echo.
    let mut keyed = CursorGlow::default();
    let mut echo_only = CursorGlow::default();
    keyed.tick(Some((2, 0)), t0, &c, g, &mut out);
    echo_only.tick(Some((2, 0)), t0, &c, g, &mut out);

    let mut charged = 0usize;
    for i in 1..=6u64 {
        let key = t0 + Duration::from_millis(i * 100);
        let echo = key + Duration::from_millis(5);

        // The un-charged standing blaze — what a naive `blaze()` sample cues.
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

        // The first key of a burst earns NO cadence credit (its gap is
        // unbounded), exactly like its echo — so the prediction is not a
        // blind constant, and cold-start clicks stay cold.
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

/// The A/B twin of the test above, on the identical cursor transition. An
/// explicit synthetic preview has causal provenance and speaks; a bare
/// gesture timestamp — a Tab whose completion never echoed, an
/// UNDELIVERED paste — does not and stays dark and silent (a delivered
/// insert has its own class, `note_insert_delivered`).
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

    // An UNLICENSED warp of the same shape — a program CUP with no key
    // behind it — speaks and paints nothing at all.
    let mut cold = CursorGlow::default();
    cold.tick(Some((2, 0)), t0, &c, g, &mut out);
    cold.tick(Some((6, 9)), jump, &c, g, &mut out);
    assert_eq!(cold.drain_sound_cues().count(), 0);
    assert!(!frame_has_output(&out, &cold));
}

/// A PASTE STRUMS UNDER THE MUSIC BOX AND IS SILENT UNDER THE NINE (§28,
/// the even hand): the rainbow kitty records exactly one `Strum` cue and
/// banks one echo credit (the paste's own echo spends it and cues
/// nothing); the same paste under another style records nothing and
/// banks nothing, so its echo Jump cues exactly as before. Fails before
/// (`cue_paste` does not exist).
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
    // The paste's echo — a coalesced multi-cell advance — spends the
    // credit and cues nothing: one gesture, one sound.
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

/// A BATCHED echo run spends ONE credit (one spawn, one cue), and the
/// unspent remainder cannot mute the stream forever: past the freshness
/// window the ledger zeroes and output-driven advances click again. This
/// is the "a key an app SWALLOWED banks silence" failure mode.
#[test]
fn key_time_credits_expire_instead_of_banking_silence() {
    use crate::trail_sound::SoundKind;
    let mut glow = CursorGlow::default();
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);

    // Three keys inside one burst — three clicks at the keys.
    for i in 0..3u64 {
        assert!(glow.cue_keystroke(t0 + Duration::from_millis(i)));
    }
    assert_eq!(glow.drain_sound_cues().count(), 3);

    // One coalesced 3-cell echo arrives as ONE spawn: it spends one
    // credit and stays silent. (The other two are still owed against
    // echoes that were folded into this frame.)
    let echo = t0 + Duration::from_millis(20);
    glow.note_synthetic_typed(echo, 3);
    glow.tick(Some((2, 3)), echo, &c, g, &mut out);
    assert_eq!(glow.drain_sound_cues().count(), 0);

    // Past the window, the owed credits are abandoned, so the NEXT echo
    // clicks again instead of being muted forever by a key an app
    // swallowed.
    let late = echo + Duration::from_secs_f32(CursorGlow::KEYED_CLICK_FRESH + 0.1);
    glow.note_typed(late);
    glow.tick(Some((2, 4)), late, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
    assert_eq!(glow.keyed_clicks, 0, "stale credits are dropped wholesale");
}

/// DARK ⇒ SILENT for the key seam, for the gates that are ABOUT SOUND.
/// The spawn edge gets its silence for free (it is unreachable while
/// dark); a cue born at the KEY does not, so the seam is gated on the
/// last tick's verdict — master off, and a dark tick (zero amplitude or a
/// degenerate grid) the host marked INAUDIBLE (unfocus) both close it, as
/// does never having ticked at all. A dark tick the host marked audible —
/// a load-shed frame, a `Reduce Motion` session, an empty grid — does
/// NOT: that case is pinned by
/// `a_shed_frame_keeps_the_key_seam_open` in
/// `tests/shed_frame_is_still_a_heard_key.rs`.
#[test]
fn key_time_click_is_silent_wherever_the_sound_gates_close() {
    let mut glow = CursorGlow::default();
    let g = geom();
    let live = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // Never ticked: a headless host / test harness records nothing.
    assert!(!glow.cue_keystroke(t0));
    assert_eq!(glow.drain_sound_cues().count(), 0);

    // Master OFF.
    glow.tick(
        Some((2, 0)),
        t0,
        &cfg(GlowStyle::RainbowKitty, false),
        g,
        &mut out,
    );
    assert!(!glow.cue_keystroke(t0));
    assert_eq!(glow.drain_sound_cues().count(), 0);

    // Live.
    glow.tick(Some((2, 0)), t0, &live, g, &mut out);
    assert!(glow.cue_keystroke(t0));
    assert_eq!(glow.drain_sound_cues().count(), 1);

    // Zero amplitude WITH the host's inaudible verdict (an unfocused
    // window) closes it again, and drops the credit banked while it was
    // open.
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

/// ONE SEAM LAW ON EVERY RETURN PAST THE MASTER SWITCH, including the two
/// that used to break it. The classic wake returned above every writer
/// of `sound_live`, so a fresh `classic` session never clicked, and one
/// switched from another style kept that style's seam, clicking in an
/// unfocused window. A degenerate grid closed the seam like the master
/// switch, which `aterm ctl tone` could only call `engine-silent`. Both
/// now take `settle_key_seam`: heard when the tick drew or the host
/// marked it audible, silent otherwise.
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

    // A FRESH classic session: its first tick opens the seam and the key
    // clicks, with the timbre snapshot a modern style would have stored.
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 0)), t0, &classic, g, &mut out);
    assert!(glow.sound_seam_open(), "a fresh classic session is heard");
    assert!(glow.cue_keystroke(t0));
    assert!(glow.take_key_cue().is_some());
    assert_eq!(glow.heat_tau_live, CursorGlow::heat_tau(&classic));

    // Classic, dark for a motion reason (audible): still heard.
    let shed = GlowConfig {
        classic_mono: false,
        intensity: 0.0,
        ..classic
    };
    glow.tick(Some((2, 1)), t0, &shed, g, &mut out);
    assert!(glow.sound_seam_open(), "a shed classic frame is heard");

    // SWITCHED from a style that left the seam open, then unfocused: the
    // classic tick closes it and drops the banked credit.
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

    // AN EMPTY GRID in the window the key belongs to: dark and heard, the
    // banked credit kept (its echo must stay silent when it lands), and
    // the dark latch NOT set, so a later master-off still wipes.
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
        // …an unfocused one is silent and wipes the ledger…
        glow.tick(Some((2, 0)), t0, &unfocused(live), empty, &mut out);
        assert!(!glow.sound_seam_open());
        assert_eq!(glow.keyed_clicks, 0);
        // …and the master switch closes it whatever the grid.
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

/// THE BRRRRING'S PREVIEW FEED, pinned — the exact cue morphology of rapid
/// synthetic new lines. A Return timestamp alone is separately pinned dark.
/// Two shapes exist and BOTH are load-bearing:
/// - Enter after a typed command (one row down, column snapped left,
///   chebyshev > 1, no key hint armed) is the JUMP gesture — and Jumps
///   bypass the synth's min-gap thinning, so a rapid run stacks
///   overlapping Jump flourishes. This is the dominant voice of the
///   owner's beloved "brrrring!".
/// - a bare held Enter at an empty prompt (one row down, SAME column —
///   chebyshev exactly 1) is the TYPED gesture: the quick pluck stream
///   filling the gaps between the flourishes.
///
/// The synth half of the pin
/// (`trail_sound::tests::brrrring_of_rapid_line_feeds_is_pinned`) holds
/// the flourishes bit-identical to v0.56; this half guarantees the cue
/// path that feeds them can never silently reclassify.
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
    // Enter after a command: (0,12) → (1,0) — the flourish voice.
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
    // Held Enter at the empty prompt: pure vertical single steps — the
    // pluck stream between flourishes.
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

    // The same line feed with NO key behind it stays silent and dark.
    let mut cold = CursorGlow::default();
    cold.tick(Some((0, 12)), t0, &c, g, &mut out);
    cold.tick(Some((1, 0)), t1, &c, g, &mut out);
    assert_eq!(cold.drain_sound_cues().count(), 0);
    assert!(!frame_has_output(&out, &cold));
}
