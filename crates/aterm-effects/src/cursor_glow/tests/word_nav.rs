// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The word move sounds every time.

use super::*;

/// One row of plain text `fill` cells wide, for the row probe the erase
/// detector reads.
fn wordnav_row(fill: u16) -> [char; 40] {
    let mut row = [' '; 40];
    for (i, cell) in row.iter_mut().enumerate() {
        if (i as u16) < fill {
            *cell = 'x';
        }
    }
    row
}

/// (1) ZSH: the shell integration binds `\e[1;3D`/`\e[1;3C` to
/// backward-/forward-word, and zsh echoes a word move as a caret-only
/// `\x08`×n / `ESC[nC` — the caret never hides. Twenty alternating
/// Option+Left/Right over `… lazy dogs` at the owner's own cadence must
/// sound twenty times. The control that proves the rest of this block is
/// about the SEAM, not about the keys.
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
        // zsh's echo lands within a frame; the caret is visible throughout.
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

/// (2) AN INK-STYLE TUI (Claude Code's shape): every repaint is
/// `ESC[?25l` … rewrite the line … `CUP` … `ESC[?25h`, and on macOS a
/// multi-KiB frame reaches the reader in 1024-byte slices, so a present
/// can land while the caret is HIDDEN. The next visible tick is then a
/// hidden→visible RELOCATION, and the bounded ConPTY hide-bridge refused
/// any hop past 2 cells — silently, with no ring row at all, and wiping
/// the nav hint on its way out. Twenty word keys, twenty cues.
#[test]
fn twenty_word_keys_through_an_ink_repaint_sound_twenty_times() {
    // Both arms: the pane the host names under its frame hold (the reach
    // the nav bridge actually runs under — here the geom's own 40 columns), and
    // no pane at all (the unpaned fallback).
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
        // The repaint's head chunk: `ESC[?25l` is processed and the line is
        // being rewritten, so the caret is not observable this present.
        glow.note_repaint_blink(key + Duration::from_millis(8));
        glow.tick(
            None,
            key + Duration::from_millis(10),
            &c,
            g,
            &mut typed_scratch(),
        );
        // …its tail chunk: `CUP` + `ESC[?25h`, one frame later.
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

/// (3) A PAIR 60 ms APART is two gestures, not one: Option+Left twice
/// inside a third of the nav licence window, each with its own echo.
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

/// (4) THE ANTI-STRAY CONTROL, and the reason the bridge above yields to
/// the nav HINT rather than to the hide window: a program relocating its
/// caret across a hidden repaint with NO key behind it must still mint
/// nothing — and the same relocation under a STALE nav hint must still
/// mint nothing, because a stale stamp is not a licence.
#[test]
fn a_hidden_program_relocation_with_no_fresh_key_still_sounds_nothing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.note_context(true);
    glow.tick(Some((2, 42)), t0, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);

    // No key at all.
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

    // A STALE nav key (older than NAV_HINT_FRESH) is not a witness either.
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

/// …and the SOURCE the nav bridge is allowed to reach back to is bounded
/// too. A viewport parked in history feeds `cur = None` for as long as the
/// user reads, so the engine's last VISIBLE cell can be seconds old and
/// belong to a screen that is no longer there; a word key pressed then must
/// not mint a screen-crossing gesture from it. `nav_bridge_source_ok`: the
/// cell must have been visible within one hide window OF THE KEY.
#[test]
fn the_nav_bridge_never_reaches_back_to_a_source_the_key_did_not_move_from() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 42)), t0, &c, g, &mut typed_scratch());
    let _ = wordnav_drain(&mut glow);

    // Seconds of a scrolled-back viewport: no visible caret at all.
    for step in 1..=8u64 {
        glow.tick(
            None,
            t0 + Duration::from_millis(400 * step),
            &c,
            g,
            &mut typed_scratch(),
        );
    }
    // Now a word key, and the caret reappears far away.
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

    // The same key, with the caret visible one hide window before it, IS
    // bridged — that is the Ink/DECTCEM shape the bridge exists for.
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

/// (5) THE WORD KILL (Ctrl-W, Option+Backspace): one `KillWord` per chord.
/// Its voice rides the erase detector's poof edge, so the caret retreat it
/// also produces must stay inert — and must stay inert
/// exactly ONCE: the kill hint lives 0.35 s and is cleared only by the
/// poof that fires, so a kill whose erase went unwitnessed used to swallow
/// the user's next real word move as a second "retreat".
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

    // THE UNWITNESSED KILL: a chord whose erase the probe never proves
    // leaves the kill hint standing. The word move pressed 200 ms later is
    // the USER's, and must sound.
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

/// (7) A KILL'S RETREAT THAT LANDS WITH A BAND MOVE OR A SCROLL IS STILL
/// THE KILL'S. The host replays every band move and scroll and then
/// drops the row probe, and
/// `drop_row_probe` clears `kill_hint` — the poof's row-content proof.
/// The retreat gate read that hint for its freshness, so a ^U/^W whose
/// caret snap arrived in the same frame as a Codex line (the shape
/// band moves exist for) was classified `Nav`: v2 minted a meteor, the
/// Meteor cue sounded, and the pending retreat was never spent. The
/// pending retreat now carries its own clock (bounded by the kill's
/// window, superseded by a real navigation press), and the probe fence
/// may clear the poof's proof without the retreat changing class.
/// Three shapes — the band move, the scroll, and the band move with an
/// intermediate no-move frame between it and the retreat — against the
/// control with neither.
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
        // What app_render's replay does for a band move or a scroll
        // that lands in the retreat's frame: translate, then drop the
        // probe.
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

/// (6) THE HELD PARK AND THE NAV BRIDGE ARE NEVER TWO VERDICTS ON ONE
/// MOVE (decided at the merge of this fix onto the park).
///
/// Both describe the same shape — "the caret came back across a repaint
/// that hid it" — so the order they run in is a law, not an accident,
/// and this pins it in both directions:
///
/// 1. THE PARK IS JUDGED FIRST. A navigation press flushes a held park
///    (`note_motion` -> `flush_held_park`) BEFORE it stamps `nav_hint`,
///    so Ink's park keeps its own `Move`, from its own origin, and the
///    word key inherits nothing of it.
/// 2. THE BRIDGE THEN ONLY CHOOSES A SOURCE CELL, and `spawn` chooses
///    park-or-spawn after it — where `park_candidate` refuses outright
///    while the nav hint is fresh (and `note_motion` has cleared the
///    typed bank the park stands on besides). So the landing the nav
///    bridge newly admits can never be swallowed as a park: the word
///    hop sounds, and the ring carries its licensed row.
#[test]
fn a_word_key_judges_a_held_park_before_its_own_landing_is_bridged() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();

    // Ink's park: a same-row backward rewrite under a fresh typed stamp,
    // HELD for the return that usually follows (no ring row yet).
    let pre = pre_roll(&mut glow, 11, t0, &c, g);
    let rows_before = ring_rows(&glow).len();
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    let park_at = k + Duration::from_millis(10);
    glow.tick(Some((11, 3)), park_at, &c, g, &mut out);
    assert_eq!(ring_rows(&glow).len(), rows_before, "held: no verdict yet");
    assert_eq!(glow.in_flight_tally().park_flushed, 0);
    let _ = wordnav_drain(&mut glow);

    // …and then the hand presses Option+Left instead of returning.
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

    // The word key's OWN echo arrives through Ink's hidden bracket, nine
    // cells on — a landing the bounded bridge refused before this fix.
    // It is spawned, not held: `park_candidate` refuses under a fresh nav
    // hint, so there is no second park to flush.
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
