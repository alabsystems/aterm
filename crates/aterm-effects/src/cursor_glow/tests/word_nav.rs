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

/// zsh's shell integration binds `\e[1;3D`/`\e[1;3C` to backward-/forward-word, and zsh echoes a
/// word move as a caret-only `\x08`×n / `ESC[nC` (the caret never hides): twenty alternating
/// Option+Left/Right sound twenty times.
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

/// An Ink-style TUI (Claude Code's shape): every repaint is `ESC[?25l` … rewrite … `CUP` …
/// `ESC[?25h`, and a multi-KiB frame arrives in 1024-byte slices, so a present can land while the
/// caret is hidden. The next visible tick is a hidden→visible relocation, which the nav bridge
/// admits: twenty word keys, twenty cues.
#[test]
fn twenty_word_keys_through_an_ink_repaint_sound_twenty_times() {
    // Both arms: the pane the host names under its frame hold (the nav
    // bridge's reach; here the geom's 40 columns), and no pane (the fallback).
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

/// Option+Left twice, 60 ms apart (inside a third of the nav licence window), each with its own
/// echo, is two gestures.
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

/// A program relocating its caret across a hidden repaint with no key behind it mints nothing,
/// and neither does the same relocation under a stale nav hint: the bridge keys on the nav hint,
/// not the hide window.
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

    // A stale nav key (older than `NAV_HINT_FRESH`) is not a witness either.
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

/// The source cell the nav bridge may reach back to is bounded too: a viewport
/// parked in history feeds `cur = None`, so the last visible cell can be
/// seconds old. `nav_bridge_source_ok` requires it visible within one hide
/// window of the key.
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

    // The same key, with the caret visible one hide window before it, is bridged: the
    // Ink/DECTCEM shape the bridge exists for.
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

/// The word kill (Ctrl-W, Option+Backspace) sounds one `KillWord` per chord on the erase
/// detector's poof edge, and its caret retreat stays inert exactly once: an unwitnessed kill must
/// not swallow the next real word move as a second retreat.
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

    // An unwitnessed kill leaves the kill hint standing; the word move pressed
    // 200 ms later is the user's and must sound.
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

/// A kill's retreat that lands with a band move or a scroll is still the kill's. The host
/// replays band moves and scrolls, then drops the row probe, which clears `kill_hint`; the
/// pending retreat carries its own clock (bounded by the kill's window, superseded by a real
/// navigation press), so it keeps its class.
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
        // What app_render's replay does for a band move or scroll landing in
        // the retreat's frame: translate, then drop the probe.
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

/// The held park and the nav bridge never give two verdicts on one move:
///
/// 1. The park is judged first: a navigation press flushes a held park
///    (`note_motion` -> `flush_held_park`) before stamping `nav_hint`, so Ink's park keeps its
///    own `Move` and the word key inherits nothing.
/// 2. The bridge then only chooses a source cell, and `park_candidate` refuses while the nav hint
///    is fresh, so a landing the bridge admits is never swallowed as a park.
#[test]
fn a_word_key_judges_a_held_park_before_its_own_landing_is_bridged() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();

    // Ink's park: a same-row backward rewrite under a fresh typed stamp, held for the return that
    // usually follows (no ring row yet).
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

    // The word key's own echo arrives through Ink's hidden bracket, nine cells
    // on. It is spawned, not held: `park_candidate` refuses under a fresh nav
    // hint.
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

    // Simulate unspent typed credits from earlier typing that did not advance
    // the caret (e.g. a TUI chord or swallowed key), then wait past TYPE_HINT_FRESH.
    for i in 0..4 {
        glow.note_typed(t0 + Duration::from_millis(10 + i * 10));
    }
    let hop = t0 + Duration::from_millis(600);
    // Option+b backward word hop to col 4, then Option+f forward word hop back to col 8.
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
