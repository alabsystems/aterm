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

    // A navigation press licenses its own scrub and cues the sound its shape
    // earns; it never heats (`navigation_earns_no_heat_and_no_meteor`).
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

/// The newest erase key owns the poof: with an unanswered kill still fresh, a
/// newer ordinary Backspace's one-cell shrink is a Backspace (its own bell),
/// not a kill with the clause-scale swoosh.
///
/// The negative control has identical geometry but follows the host's
/// word-Backspace order (generic Backspace bookkeeping, then the kill arm), so
/// the kill keeps the swoosh. Both class changes retire older [`TypedStamps`]
/// rather than collapsing or spending them.
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

    // An unanswered kill, then a newer ordinary Backspace.
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

    // Negative control: identical geometry, but a word-Backspace ends with the
    // kill arm, as app_input dispatches it.
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

/// Resuming after a pause leaves no black gap. A mid-line pause (1.7-5 s:
/// inside the chain window, past spark lifetimes) expires the sparks; the
/// resume key pushes its head spark and the four-letter guarantee re-mints the
/// three expired tail cells. The re-mints must keep cell order (the
/// rasterizer derives `sp`/`asp` from ordinal), or an interior cell gets the
/// far feather's zero and prints as background. Two assertions:
/// 1. the Vec walks the mark tail→head, and
/// 2. the ribbon's lit columns are contiguous (no interior cell dark between
///    lit neighbours).
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
    // The pause: longer than every spark's life, shorter than the chain window
    // (5 s), so the resume key re-mints the expired tail.
    t += Duration::from_millis(4500);
    // Resume at a quicker cadence so the resume cell (col 10) is still resident
    // when its ordinal could reach the far feather's zero.
    for k in 10..=18u16 {
        t += Duration::from_millis(150);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, k)), t, &c, g, &mut out);
    }
    // 1. Cell order is Vec order.
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
    // 2. No interior hole: the resume cell is resident (re-armed through col
    // 13), so it must not be culled while both neighbours draw.
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

/// Flood typing leaves no black gap: K presses inside one frame gap bank K
/// [`TypedStamps`], and each observed sweep spends what it covers, so an echo
/// arriving as several sweeps lights every cell. Once the sweeps have spent the
/// stamps, the next program move finds no licence.
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
    // Licensed sweeps never exceed banked presses: the coalesced `+2` spent two
    // presses and the `+1` the third. The remaining stamp licenses nothing
    // without an unpaid press, so the next observed move is declined.
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

/// A typed stamp is a licence only while a press is unpaid. A three-key burst
/// echoing as one `+3` spends all three presses but pops one stamp; under
/// Rainbow Kitty the two surviving stamps license nothing, so a keyless `+1`
/// lays no cell and a keyless wide hop lights no landing. The classic trail's
/// lockstep contract (`move_licensed`) is unchanged.
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

/// A keyless hop the in-flight pool cannot pay for lights no landing: the
/// dangling-credit half of
/// [`a_coalesced_echos_surviving_stamps_license_nothing_once_its_presses_are_paid`].
/// Presses whose echoes were never judged (a hidden-caret TUI's first keys on a
/// row the anchored lane had not seen) stay on the ring for the patience and
/// open the in-flight licence for a same-row forward hop. A twelve-cell program
/// flood is refused `no-credits` and the pool forgotten, and the re-anchor's
/// landing sweep must not spend a dangling credit on its last cell: the landing
/// is for a hop a key licensed inside the stamp window (vim's `w`). Control:
/// the same hop with a key pressed inside the window lights its landing.
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

    // Control: the same twelve-cell hop with a key pressed inside the stamp
    // window (vim's `w`, the box-growth re-anchor) lights exactly its landing.
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

/// The key-time click: the click is born at the physical keypress, and the echo
/// a round trip later must not click again.
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

    // The NEXT advance has no credit behind it, so it clicks at the echo.
    let out_echo = t0 + Duration::from_millis(60);
    glow.note_synthetic_typed(out_echo, 1);
    glow.tick(Some((2, 2)), out_echo, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
}

/// `take_key_cue` hands the click to the host so it reaches the synth on the key
/// thread rather than the next render tick. Taking it changes only who carries
/// the cue: the credit ledger still mutes the echo (one character, one click),
/// and earlier cues stay queued for the frame drain.
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

/// A printable key whose echo lands as a jump (vim motions, keys in
/// less/htop/fzf, multi-cell IME commits) speaks exactly once and spends its
/// credit: no `Typed` at the key plus `Jump` at the echo (Jump bypasses
/// MIN_GAP), and no leftover credit to swallow a later advance.
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

    // Its echo is a multi-cell jump (`w` in vim). The host still arms the
    // printable-key witness; the move must find the credit and stay silent.
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

/// Every admitted spawn arm handles an outstanding key-time credit without a
/// duplicate printable-key click.
///
/// A printable key clicks at the press and arms a credit, then its echo is
/// swallowed or folds into the next key's echo, which may take the `deletion`
/// or `navigation` arm. An arm that leaves the credit standing leaks it, and a
/// typing burst keeps it fresh until it mutes a genuine click. Retiring must
/// not silence those arms (a Backspace or arrow never clicked at press time),
/// so each row asserts both halves plus the follow-up click.
#[test]
fn admitted_spawn_arms_handle_the_key_time_ledger() {
    use crate::trail_sound::SoundKind;
    // (arm name, hint armed before the echo, the echo's landing cell, the
    // gesture that echo must still speak; `None` for arms a printable key's
    // own echo lands on, which suppress themselves.)
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
        // credit; an exact deletion is a different key and leaves it standing.
        let settles = matches!(name, "typing" | "jump");
        assert_eq!(
            glow.keyed_clicks,
            u8::from(!settles),
            "{name}: wrong ledger disposition for this arm"
        );

        // The accepted cost: the exact deletion leaves the printable credit to
        // mute one later typing echo, while typing and jump already spent it.
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

/// The key-time click's timbre belongs to its own keystroke. `heat` scales the
/// synth's note level (`0.55 + 0.45·heat`) and ember layer (`0.1 + 0.28·heat`);
/// the key-time cue carries the charge its keystroke is about to add, tracking
/// the echo-time engine within a few percent (the residue is round-trip
/// cooling, and the key-time value is the heat when the click is heard).
#[test]
fn the_key_time_click_carries_its_own_keystrokes_heat() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // Two engines, one script: 6 keys at 100 ms with a 5 ms local echo. `keyed`
    // cues at the key; `echo_only` cues at the echo.
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

        // The first key of a burst earns no cadence credit (unbounded gap), as
        // its echo would, so cold-start clicks stay cold.
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

/// The A/B twin of the test above on the same transition: an explicit synthetic
/// preview has causal provenance and speaks; a bare gesture timestamp (a Tab
/// whose completion never echoed, an undelivered paste) stays dark and silent
/// (a delivered insert has its own class, `note_insert_delivered`).
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

/// A paste strums under the music box and is silent under the other styles
/// (§28): rainbow kitty records one `Strum` cue and banks one echo credit
/// (the paste's echo spends it silently); other styles record and bank
/// nothing, so their echo Jump cues as usual.
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

/// A batched echo run spends one credit (one spawn, one cue), and the unspent
/// remainder cannot mute the stream forever: past the freshness window the
/// ledger zeroes and output-driven advances click again.
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

    // One coalesced 3-cell echo is one spawn: it spends one credit and stays
    // silent (the other two are owed by echoes folded into this frame).
    let echo = t0 + Duration::from_millis(20);
    glow.note_synthetic_typed(echo, 3);
    glow.tick(Some((2, 3)), echo, &c, g, &mut out);
    assert_eq!(glow.drain_sound_cues().count(), 0);

    // Past the window the owed credits are abandoned, so the next echo clicks.
    let late = echo + Duration::from_secs_f32(CursorGlow::KEYED_CLICK_FRESH + 0.1);
    glow.note_typed(late);
    glow.tick(Some((2, 4)), late, &c, g, &mut out);
    let cues: Vec<_> = glow.drain_sound_cues().collect();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].kind, SoundKind::Typed);
    assert_eq!(glow.keyed_clicks, 0, "stale credits are dropped wholesale");
}

/// Dark means silent for the key seam, for the gates about sound. A cue born at
/// the key is gated on the last tick's verdict: master off, a dark tick the host
/// marked inaudible (unfocus), or no tick yet all close it. A dark tick the host
/// marked audible (load-shed frame, Reduce Motion, empty grid) does not; see
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

    // Zero amplitude with the host's inaudible verdict (an unfocused window)
    // closes it and drops the credit banked while open.
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

/// One seam law on every return past the master switch, the classic wake and a
/// degenerate grid included: all take `settle_key_seam`, heard when the tick
/// drew or the host marked it audible, silent otherwise.
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

    // An empty grid in the key's window: dark and heard, the banked credit kept
    // (its echo must stay silent), and the dark latch not set, so a later
    // master-off still wipes.
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

/// The preview feed of rapid synthetic new lines keeps its cue morphology (a
/// Return timestamp alone is pinned dark elsewhere). Two shapes:
/// - Enter after a typed command (one row down, column snapped left,
///   chebyshev above 1, no key hint) is the Jump gesture; Jumps bypass the synth's min-gap
///   thinning, so a rapid run stacks overlapping flourishes;
/// - a bare held Enter at an empty prompt (one row down, same column, chebyshev
///   1) is the Typed gesture, plucks filling the gaps.
///
/// The synth half is `trail_sound::tests::brrrring_of_rapid_line_feeds_is_pinned`;
/// this half keeps the cue path from reclassifying them.
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
