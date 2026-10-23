// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::sync_cursor_effect_scroll;
use crate::{App, WindowId};
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal, TerminalBuilder};
use std::time::{Duration, Instant};

#[test]
fn an_erase_does_not_reset_the_cursor_effects_and_the_ribbon_survives_it() {
    let mut app = App::headless_for_test();
    let mut cfg = app.glow_config();
    cfg.enabled = true;
    cfg.style = crate::cursor_glow::GlowStyle::RainbowKitty;
    let geom = crate::cursor_glow::Geom {
        cw: 8,
        ch: 16,
        rows: 10,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 160,
        head: 0,
    };
    let t0 = Instant::now() - Duration::from_millis(500);

    let mut term = Terminal::new(10, 40);
    term.process(b"the quick brown fox");
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.fx.set_scroll_baseline(Some(term.content_scroll_state()));
    let mut out = Vec::new();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((0, 0)), t0, &cfg, geom, &mut out);
    for i in 1..=9u16 {
        let at = t0 + Duration::from_millis(u64::from(i) * 40);
        ws.fx.cursor_mut().glow.note_synthetic_typed(at, 1);
        ws.fx.cursor_mut().glow.tick(
            Some((0, i)),
            at + Duration::from_millis(4),
            &cfg,
            geom,
            &mut out,
        );
    }
    let earned = ws.fx.cursor_mut().glow.ribbon_segments();
    assert!(earned > 0, "the take must start with a ribbon to lose");

    for spelling in [
        &b"\x1b[J"[..],
        &b"\x1b[1J"[..],
        &b"\x1b[2J"[..],
        &b"\x1b#8"[..],
    ] {
        term.process(spelling);
        let decision =
            ContentScrollState::delta_since(ws.fx.scroll_baseline(), term.content_scroll_state());
        assert_eq!(
            decision,
            ContentScrollDelta::Unchanged,
            "{spelling:?} rewrites cells in place; it is not a coordinate change"
        );
        let change = sync_cursor_effect_scroll(ws, term.content_scroll_state());
        assert!(
            !change.invalidated,
            "{spelling:?} must not retire the cursor effects"
        );
        assert_eq!(
            ws.fx.cursor_mut().glow.ribbon_segments(),
            earned,
            "{spelling:?} wiped the earned ribbon: {} -> {}",
            earned,
            ws.fx.cursor_mut().glow.ribbon_segments()
        );
    }

    let before_band = ws.fx.scroll_baseline().expect("the take has a baseline");
    term.process(b"\x1b[2;3r\x1b[3;1H\n");
    assert_eq!(
        ContentScrollState::delta_since(ws.fx.scroll_baseline(), term.content_scroll_state()),
        ContentScrollDelta::Bands {
            first_seq: before_band.band_seq,
            count: 1,
        },
        "a region scroll is an exact row-band translate"
    );
    let change = sync_cursor_effect_scroll(ws, term.content_scroll_state());
    assert!(!change.invalidated, "an explained band never retires");
    assert_eq!(change.band_moves, 1);
    assert!(change.changed(), "the coordinates under the light did move");
    ws.fx.cursor_mut().glow.tick(
        Some((0, 9)),
        t0 + Duration::from_millis(364),
        &cfg,
        geom,
        &mut out,
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.ribbon_segments(),
        earned,
        "row 0 is outside the band [1..=2]: the typed ribbon is untouched"
    );

    term.process(b"\x1b[r\x1b[10;1H\n\x1b[2;3r\x1b[3;1H\n");
    assert_eq!(
        ContentScrollState::delta_since(ws.fx.scroll_baseline(), term.content_scroll_state()),
        ContentScrollDelta::Invalidate,
        "uniform rows beside an epoch step cannot be laundered into either \
         exact transform"
    );
    let change = sync_cursor_effect_scroll(ws, term.content_scroll_state());
    assert!(change.invalidated);
    assert!(
        ws.fx
            .cursor_mut()
            .glow
            .v2_ribbon()
            .is_some_and(|r| r.curtained() && !r.at_rest()),
        "an invalidation must still act on the earned light"
    );
    let fell = Instant::now();
    let mut lit = Vec::new();
    for ms in [0_u64, 16, 33, 50] {
        ws.fx.cursor_mut().glow.tick(
            Some((0, 9)),
            fell + Duration::from_millis(ms),
            &cfg,
            geom,
            &mut out,
        );
        lit.push(ws.fx.cursor_mut().glow.ribbon_segments());
    }
    assert!(
        lit.iter().all(|&n| n > 0) && lit.windows(2).all(|w| w[1] <= w[0]),
        "the curtain draws on at least the first four frames after the seam              and never brightens: {lit:?}"
    );
    ws.fx.cursor_mut().glow.tick(
        Some((0, 9)),
        fell + Duration::from_millis(260),
        &cfg,
        geom,
        &mut out,
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.ribbon_segments(),
        0,
        "…and it is exactly gone by the curtain's end"
    );
}

#[test]
fn seventeen_band_moves_between_presents_fall_back_to_invalidate() {
    let mut app = App::headless_for_test();
    let mut cfg = app.glow_config();
    cfg.enabled = true;
    cfg.style = crate::cursor_glow::GlowStyle::RainbowKitty;
    let geom = crate::cursor_glow::Geom {
        cw: 8,
        ch: 16,
        rows: 20,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 320,
        head: 0,
    };
    let t0 = Instant::now() - Duration::from_millis(500);

    let mut term = Terminal::new(20, 40);
    term.process(b"the quick brown fox");
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.fx.set_scroll_baseline(Some(term.content_scroll_state()));
    let before = term.content_scroll_state();
    let mut out = Vec::new();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((0, 0)), t0, &cfg, geom, &mut out);
    for i in 1..=9u16 {
        let at = t0 + Duration::from_millis(u64::from(i) * 40);
        ws.fx.cursor_mut().glow.note_synthetic_typed(at, 1);
        ws.fx.cursor_mut().glow.tick(
            Some((0, i)),
            at + Duration::from_millis(4),
            &cfg,
            geom,
            &mut out,
        );
    }
    let earned = ws.fx.cursor_mut().glow.ribbon_segments();
    assert!(earned > 0, "the take must start with a ribbon to lose");

    for _ in 0..16 {
        term.process(b"\x1b[10;19r\x1b[19;1H\n");
    }
    let sixteen = term.content_scroll_state();
    assert_eq!(sixteen.band_seq, before.band_seq + 16);
    assert_eq!(
        ContentScrollState::delta_since(Some(before), sixteen),
        ContentScrollDelta::Bands {
            first_seq: before.band_seq,
            count: 16,
        },
        "a full ring is still exact"
    );

    term.process(b"\x1b[10;19r\x1b[19;1H\n");
    let seventeen = term.content_scroll_state();
    assert_eq!(seventeen.band_seq, before.band_seq + 17);
    assert_eq!(
        seventeen.band_batches,
        before.band_batches + 17,
        "every one of them WAS explained; only the ring is too small"
    );
    let decision = ContentScrollState::delta_since(Some(before), seventeen);
    assert_eq!(decision, ContentScrollDelta::Invalidate);

    let change = sync_cursor_effect_scroll(ws, seventeen);
    assert!(change.invalidated);
    assert_eq!(change.band_moves, 0);
    assert!(
        ws.fx
            .cursor_mut()
            .glow
            .v2_ribbon()
            .is_some_and(|r| r.curtained() && !r.at_rest()),
        "the overflow must still retire the earned light"
    );
    let fell = Instant::now();
    ws.fx.cursor_mut().glow.tick(
        Some((0, 9)),
        fell + Duration::from_millis(260),
        &cfg,
        geom,
        &mut out,
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.ribbon_segments(),
        0,
        "…and it is exactly gone at the curtain's end"
    );
}

#[test]
fn a_codex_shaped_terminal_keeps_its_ribbon_through_both_phases() {
    /// `(milliseconds after record 642, the bytes of that record)`.
    const CODEX_STREAM: &[(u64, &[u8])] = &[
    (0, b"\x1b[?2026h\x1b[25;57r\x1b[25;1H\x1bM\x1b[r\x1b[1;25r\x1b[24;1H\r\n"),
    (0, b"\x1b[39;49m\x1b[K  2\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[27;10H\x1b[39m\x1b[49m\x1b[0m\x1b[26;1H\x1b[0 q\x1b[26;1H \x1b[39m\x1b[49m\x1b[0m\x1b[28;10H\x1b[?25h"),
    (0, b"\x1b[?2026l"),
    (56, b"\x1b]0;\xe2\xa0\xb4 aterm\x07"),
    (56, b"\x1b[?2026h\x1b[28;10H\x1b[39;48;2;45;47;51mo\x1b[39m\x1b[49m\x1b[0m\x1b[26;1H\x1b[0 q\x1b[26;1H \x1b[39m\x1b[49m\x1b[0m\x1b[28;11H\x1b[?25h\x1b[?2026l"),
    (113, b"\x1b]0;\xe2\xa0\xa6 aterm\x07"),
    (113, b"\x1b[?2026h\x1b[26;57r\x1b[26;1H\x1bM\x1b[r\x1b[1;26r\x1b[25;1H\r\n"),
    (113, b"\x1b[39;49m\x1b[K  3\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[28;11H\x1b[39m\x1b[49m\x1b[0m\x1b[27;1H\x1b[0 q\x1b[27;1H \x1b[39m\x1b[49m\x1b[0m\x1b[29;11H\x1b[?25h\x1b[?2026l"),
    (124, b"\x1b[?2026h\x1b[27;57r\x1b[27;1H\x1bM\x1b[r\x1b[1;27r\x1b[26;1H\r\n"),
    (124, b"\x1b[39;49m\x1b[K  4\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[29;11H\x1b[39m\x1b[49m\x1b[0m\x1b[28;1H\x1b[0 q\x1b[28;1H \x1b[39m\x1b[49m\x1b[0m\x1b[30;11H\x1b[?25h\x1b[?2026l"),
    (181, b"\x1b[?2026h\x1b[30;11H\x1b[39;48;2;45;47;51mr\x1b[39m\x1b[49m\x1b[0m\x1b[28;1H\x1b[0 q\x1b[28;1H \x1b[39m\x1b[49m\x1b[0m\x1b[30;12H\x1b[?25h\x1b[?2026l"),
    (216, b"\x1b]0;\xe2\xa0\xa7 aterm\x07"),
    (216, b"\x1b[?2026h\x1b[28;57r\x1b[28;1H\x1bM\x1b[r\x1b[1;28r\x1b[27;1H\r\n"),
    (216, b"\x1b[39;49m\x1b[K  5\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[30;12H\x1b[39m\x1b[49m\x1b[0m\x1b[29;1H\x1b[0 q\x1b[29;1H \x1b[39m\x1b[49m\x1b[0m\x1b[31;12H\x1b[?25h\x1b[?2026l"),
    (238, b"\x1b[?2026h\x1b[29;57r\x1b[29;1H\x1bM\x1b[r\x1b[1;29r\x1b[28;1H\r\n"),
    (238, b"\x1b[39;49m\x1b[K  6\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[31;12H\x1b[39m\x1b[49m\x1b[0m\x1b[30;1H\x1b[0 q\x1b[30;1H \x1b[39m\x1b[49m\x1b[0m\x1b[32;12H\x1b[?25h\x1b[?2026l"),
    (306, b"\x1b]0;\xe2\xa0\x87 aterm\x07"),
    (306, b"\x1b[?2026h\x1b[32;12H\x1b[39;48;2;45;47;51ml\x1b[39m\x1b[49m\x1b[0m\x1b[30;1H\x1b[0 q\x1b[30;1H \x1b[39m\x1b[49m\x1b[0m\x1b[32;13H\x1b[?25h\x1b[?2026l"),
    (324, b"\x1b[?2026h\x1b[30;57r\x1b[30;1H\x1bM\x1b[r\x1b[1;30r\x1b[29;1H\r\n"),
    (324, b"\x1b[39;49m\x1b[K  7\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[32;13H\x1b[39m\x1b[49m\x1b[0m\x1b[31;1H\x1b[0 q\x1b[31;1H \x1b[39m\x1b[49m\x1b[0m\x1b[33;13H\x1b[?25h\x1b[?2026l"),
    (426, b"\x1b]0;\xe2\xa0\x8f aterm\x07"),
    (433, b"\x1b[?2026h\x1b[33;13H\x1b[39;48;2;45;47;51md\x1b[39m\x1b[49m\x1b[0m\x1b[31;1H\x1b[0 q\x1b[31;1H \x1b[39m\x1b[49m\x1b[0m\x1b[33;14H\x1b[?25h\x1b[?2026l"),
    (447, b"\x1b[?2026h\x1b[31;57r\x1b[31;1H\x1bM\x1b[r\x1b[1;31r\x1b[30;1H\r\n"),
    (447, b"\x1b[39;49m\x1b[K  8\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[33;14H\x1b[39m\x1b[49m\x1b[0m\x1b[32;1H\x1b[0 q\x1b[32;1H \x1b[39m\x1b[49m\x1b[0m\x1b[34;14H\x1b[?25h\x1b[?2026l"),
    (527, b"\x1b]0;\xe2\xa0\x8b aterm\x07"),
    (527, b"\x1b[?2026h\x1b[32;57r\x1b[32;1H\x1bM\x1b[r\x1b[1;32r\x1b[31;1H\r\n"),
    (527, b"\x1b[39;49m\x1b[K  9\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[34;14H\x1b[39m\x1b[49m\x1b[0m\x1b[33;1H\x1b[0 q\x1b[33;1H \x1b[39m\x1b[49m\x1b[0m\x1b[35;14H\x1b[?25h"),
    (527, b"\x1b[?2026l"),
    (537, b"\x1b[?2026h\x1b[33;57r\x1b[33;1H\x1bM\x1b[r\x1b[1;33r\x1b[32;1H\r\n"),
    (537, b"\x1b[39;49m\x1b[K  10\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[35;14H\x1b[39m\x1b[49m\x1b[0m\x1b[34;1H"),
    (537, b"\x1b[0 q\x1b[34;1H \x1b[39m\x1b[49m\x1b[0m\x1b[36;14H\x1b[?25h\x1b[?2026l"),
    (599, b"\x1b[?2026h\x1b[34;57r\x1b[34;1H\x1bM\x1b[r\x1b[1;34r\x1b[33;1H\r\n"),
    (599, b"\x1b[39;49m\x1b[K  11\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[36;14H\x1b[39m\x1b[49m\x1b[0m\x1b[35;1H\x1b[0 q\x1b[35;1H \x1b[39m\x1b[49m\x1b[0m\x1b[37;14H\x1b[?25h\x1b[?2026l"),
    (608, b"\x1b]0;\xe2\xa0\x99 aterm\x07"),
    (608, b"\x1b[?2026h\x1b[35;57r\x1b[35;1H\x1bM\x1b[r\x1b[1;35r\x1b[34;1H\r\n"),
    (608, b"\x1b[39;49m\x1b[K  12\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[37;14H\x1b[39m\x1b[49m\x1b[0m\x1b[36;1H\x1b[0 q\x1b[36;1H \x1b[39m\x1b[49m\x1b[0m\x1b[38;14H\x1b[?25h\x1b[?2026l"),
    (710, b"\x1b]0;\xe2\xa0\xb9 aterm\x07"),
    (718, b"\x1b[?2026h\x1b[36;57r\x1b[36;1H\x1bM\x1b[r\x1b[1;36r\x1b[35;1H\r\n"),
    (718, b"\x1b[39;49m\x1b[K  13\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[38;14H\x1b[39m\x1b[49m\x1b[0m\x1b[37;1H\x1b[0 q\x1b[37;1H \x1b[39m\x1b[49m\x1b[0m\x1b[39;14H\x1b[?25h\x1b[?2026l"),
    (729, b"\x1b[?2026h\x1b[37;57r\x1b[37;1H\x1bM\x1b[r\x1b[1;37r\x1b[36;1H\r\n"),
    (729, b"\x1b[39;49m\x1b[K  14\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[39;14H\x1b[39m\x1b[49m\x1b[0m\x1b[38;1H\x1b[0 q\x1b[38;1H \x1b[39m\x1b[49m\x1b[0m\x1b[40;14H\x1b[?25h\x1b[?2026l"),
    (791, b"\x1b[?2026h\x1b[38;57r\x1b[38;1H\x1bM\x1b[r\x1b[1;38r\x1b[37;1H\r\n"),
    (791, b"\x1b[39;49m\x1b[K  15\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[40;14H\x1b[39m\x1b[49m\x1b[0m\x1b[39;1H\x1b[0 q\x1b[39;1H \x1b[39m\x1b[49m\x1b[0m\x1b[41;14H\x1b[?25h\x1b[?2026l"),
    (845, b"\x1b]0;\xe2\xa0\xb8 aterm\x07"),
    (845, b"\x1b[?2026h\x1b[39;57r\x1b[39;1H\x1bM\x1b[r\x1b[1;39r\x1b[38;1H\r\n"),
    (845, b"\x1b[39;49m\x1b[K  16\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[41;14H\x1b[39m\x1b[49m\x1b[0m\x1b[40;1H\x1b[0 q\x1b[40;1H \x1b[39m\x1b[49m\x1b[0m\x1b[42;14H\x1b[?25h\x1b[?2026l"),
    (887, b"\x1b[?2026h\x1b[40;57r\x1b[40;1H\x1bM\x1b[r\x1b[1;40r\x1b[39;1H\r\n"),
    (887, b"\x1b[39;49m\x1b[K  17\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[42;14H\x1b[39m\x1b[49m\x1b[0m\x1b[41;1H\x1b[0 q\x1b[41;1H \x1b[39m\x1b[49m\x1b[0m\x1b[43;14H\x1b[?25h\x1b[?2026l"),
    (989, b"\x1b]0;\xe2\xa0\xbc aterm\x07"),
    (1090, b"\x1b]0;\xe2\xa0\xb4 aterm\x07"),
    (1120, b"\x1b]0;\xe2\xa0\xa6 aterm\x07"),
    (1120, b"\x1b[?2026h\x1b[41;57r\x1b[41;1H\x1bM\x1b[r\x1b[1;41r\x1b[40;1H\r\n"),
    (1120, b"\x1b[39;49m\x1b[K  18\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[43;14H\x1b[39m\x1b[49m\x1b[0m\x1b[42;1H\x1b[0 q\x1b[42;1H \x1b[39m\x1b[49m\x1b[0m\x1b[44;14H\x1b[?25h\x1b[?2026l"),
    (1129, b"\x1b[?2026h\x1b[42;57r\x1b[42;1H\x1bM\x1b[r\x1b[1;42r\x1b[41;1H\r\n"),
    (1129, b"\x1b[39;49m\x1b[K  19\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[44;14H\x1b[39m\x1b[49m\x1b[0m\x1b[43;1H\x1b[0 q\x1b[43;1H \x1b[39m\x1b[49m\x1b[0m\x1b[45;14H\x1b[?25h\x1b[?2026l"),
    (1138, b"\x1b[?2026h\x1b[43;57r\x1b[43;1H\x1bM\x1b[r\x1b[1;43r\x1b[42;1H\r\n"),
    (1138, b"\x1b[39;49m\x1b[K  20\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[45;14H\x1b[39m\x1b[49m\x1b[0m\x1b[44;1H\x1b[0 q\x1b[44;1H \x1b[39m\x1b[49m\x1b[0m\x1b[46;14H\x1b[?25h\x1b[?2026l"),
    (1183, b"\x1b[?2026h\x1b[44;57r\x1b[44;1H\x1bM\x1b[r\x1b[1;44r\x1b[43;1H\r\n"),
    (1183, b"\x1b[39;49m\x1b[K  21\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[46;14H\x1b[39m\x1b[49m\x1b[0m\x1b[45;1H\x1b[0 q\x1b[45;1H \x1b[39m\x1b[49m\x1b[0m\x1b[47;14H\x1b[?25h\x1b[?2026l"),
    (1284, b"\x1b]0;\xe2\xa0\xa7 aterm\x07"),
    (1293, b"\x1b[?2026h\x1b[45;57r\x1b[45;1H\x1bM\x1b[r\x1b[1;45r\x1b[44;1H\r\n"),
    (1293, b"\x1b[39;49m\x1b[K  22\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[47;14H\x1b[39m\x1b[49m\x1b[0m\x1b[46;1H\x1b[0 q\x1b[46;1H \x1b[39m\x1b[49m\x1b[0m\x1b[48;14H\x1b[?25h\x1b[?2026l"),
    (1303, b"\x1b[?2026h\x1b[46;57r\x1b[46;1H\x1bM\x1b[r\x1b[1;46r\x1b[45;1H\r\n"),
    (1303, b"\x1b[39;49m\x1b[K  23\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[48;14H\x1b[39m\x1b[49m\x1b[0m\x1b[47;1H\x1b[0 q\x1b[47;1H \x1b[39m\x1b[49m\x1b[0m\x1b[49;14H\x1b[?25h\x1b[?2026l"),
    (1330, b"\x1b]0;\xe2\xa0\x87 aterm\x07\x1b[?2026h\x1b[47;57r\x1b[47;1H\x1bM\x1b[r\x1b[1;47r\x1b[46;1H\r\n"),
    (1330, b"\x1b[39;49m\x1b[K  24\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[49;14H\x1b[39m\x1b[49m\x1b[0m\x1b[48;1H\x1b[0 q\x1b[48;1H \x1b[39m\x1b[49m\x1b[0m\x1b[50;14H\x1b[?25h\x1b[?2026l"),
    (1417, b"\x1b]0;\xe2\xa0\x8f aterm\x07\x1b[?2026h\x1b[48;57r\x1b[48;1H\x1bM\x1b[r\x1b[1;48r\x1b[47;1H\r\n"),
    (1417, b"\x1b[39;49m\x1b[K  25\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[50;14H\x1b[39m\x1b[49m\x1b[0m\x1b[49;1H\x1b[0 q\x1b[49;1H \x1b[39m\x1b[49m\x1b[0m\x1b[51;14H\x1b[?25h\x1b[?2026l"),
    (1473, b"\x1b[?2026h\x1b[49;57r\x1b[49;1H\x1bM\x1b[r\x1b[1;49r\x1b[48;1H\r\n"),
    (1473, b"\x1b[39;49m\x1b[K  26\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[51;14H\x1b[39m\x1b[49m\x1b[0m\x1b[50;1H\x1b[0 q\x1b[50;1H \x1b[39m\x1b[49m\x1b[0m\x1b[52;14H\x1b[?25h\x1b[?2026l"),
    (1527, b"\x1b]0;\xe2\xa0\x8b aterm\x07\x1b[?2026h\x1b[50;57r\x1b[50;1H\x1bM\x1b[r\x1b[1;50r\x1b[49;1H\r\n"),
    (1527, b"\x1b[39;49m\x1b[K  27\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[52;14H\x1b[39m\x1b[49m\x1b[0m\x1b[51;1H\x1b[0 q\x1b[51;1H \x1b[39m\x1b[49m\x1b[0m\x1b[53;14H\x1b[?25h\x1b[?2026l"),
    (1575, b"\x1b[?2026h\x1b[51;57r\x1b[51;1H\x1bM\x1b[r\x1b[1;51r\x1b[50;1H\r\n"),
    (1575, b"\x1b[39;49m\x1b[K  28\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[53;14H\x1b[39m\x1b[49m\x1b[0m\x1b[52;1H\x1b[0 q\x1b[52;1H \x1b[39m\x1b[49m\x1b[0m\x1b[54;14H\x1b[?25h\x1b[?2026l"),
    (1608, b"\x1b]0;\xe2\xa0\x99 aterm\x07"),
    (1608, b"\x1b[?2026h\x1b[52;57r\x1b[52;1H\x1bM\x1b[r\x1b[1;52r\x1b[51;1H\r\n"),
    (1608, b"\x1b[39;49m\x1b[K  29\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[54;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (1710, b"\x1b]0;\xe2\xa0\xb9 aterm\x07"),
    (1738, b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (1738, b"\x1b[39;49m\x1b[K  30\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (1747, b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (1747, b"\x1b[39;49m\x1b[K  31\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (1812, b"\x1b]0;\xe2\xa0\xb8 aterm\x07\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (1812, b"\x1b[39;49m\x1b[K  32\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (1864, b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (1864, b"\x1b[39;49m\x1b[K  33\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (1939, b"\x1b]0;\xe2\xa0\xbc aterm\x07"),
    (1939, b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (1939, b"\x1b[39;49m\x1b[K  34\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (2014, b"\x1b]0;\xe2\xa0\xb4 aterm\x07"),
    (2014, b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (2014, b"\x1b[39;49m\x1b[K  35\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (2046, b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (2046, b"\x1b[39;49m\x1b[K  36\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (2148, b"\x1b]0;\xe2\xa0\xa6 aterm\x07"),
    (2198, b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (2198, b"\x1b[39;49m\x1b[K  37\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (2212, b"\x1b]0;\xe2\xa0\xa7 aterm\x07\x1b[?2026h\x1b[1;52r\x1b[52;1H"),
    (2212, b"\r\n"),
    (2212, b"\x1b[39;49m\x1b[K  38\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (2245, b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (2245, b"\x1b[39;49m\x1b[K  39\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    (2347, b"\x1b]0;\xe2\xa0\x87 aterm\x07"),
    (2409, b"\x1b]0;\xe2\xa0\x8f aterm\x07\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\n"),
    (2409, b"\x1b[39;49m\x1b[K  40\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[55;14H\x1b[39m\x1b[49m\x1b[0m\x1b[53;1H\x1b[0 q\x1b[53;1H \x1b[39m\x1b[49m\x1b[0m\x1b[55;14H\x1b[?25h\x1b[?2026l"),
    ];

    let mut app = App::headless_for_test();
    let mut cfg = app.glow_config();
    cfg.enabled = true;
    cfg.style = crate::cursor_glow::GlowStyle::RainbowKitty;
    let geom = crate::cursor_glow::Geom {
        cw: 8,
        ch: 16,
        rows: 57,
        cols: 151,
        origin_x: 0,
        origin_y: 0,
        win_w: 1208,
        win_h: 912,
        head: 0,
    };
    let t0 = Instant::now();

    let mut term = TerminalBuilder::new()
        .size(57, 151)
        .ring_buffer_size(1000)
        .build();
    term.process(b"\x1b[27;10H");
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.fx.set_scroll_baseline(Some(term.content_scroll_state()));
    let mut out = Vec::new();
    let mut unmoved = crate::cursor_glow::CursorGlow::default();
    let mut unmoved_out = Vec::new();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((26, 1)), t0, &cfg, geom, &mut out);
    unmoved.tick(Some((26, 1)), t0, &cfg, geom, &mut unmoved_out);
    for i in 2..=9u16 {
        let at = t0 + Duration::from_millis(u64::from(i) * 120);
        ws.fx.cursor_mut().glow.note_synthetic_typed(at, 1);
        unmoved.note_synthetic_typed(at, 1);
        unmoved.tick(
            Some((26, i)),
            at + Duration::from_millis(4),
            &cfg,
            geom,
            &mut unmoved_out,
        );
        ws.fx.cursor_mut().glow.tick(
            Some((26, i)),
            at + Duration::from_millis(4),
            &cfg,
            geom,
            &mut out,
        );
    }
    let earned = ws.fx.cursor_mut().glow.ribbon_segments();
    assert!(earned > 0, "fixture: the composer is lit before the answer");
    let spawns = ws.fx.cursor_mut().glow.spawns();
    let t_stream = t0 + Duration::from_millis(9 * 120 + 40);
    let mut momentum = ws.fx.cursor_mut().glow.typing_momentum(t_stream);

    let mut bands = 0u64;
    let mut caret = term.cursor();
    for (k, (dt_ms, bytes)) in CODEX_STREAM.iter().enumerate() {
        let at = t_stream + Duration::from_millis(*dt_ms);
        term.process(bytes);
        let change = sync_cursor_effect_scroll(ws, term.content_scroll_state());
        assert!(
            !change.invalidated,
            "record {k}: a real Codex write retired the cursor effects"
        );
        assert!(
            change.band_moves <= 1,
            "record {k}: one pty write is at most one band move"
        );
        bands += u64::from(change.band_moves);
        caret = term.cursor();
        ws.fx
            .cursor_mut()
            .glow
            .tick(Some((caret.row, caret.col)), at, &cfg, geom, &mut out);

        assert_eq!(
            ws.fx.cursor_mut().glow.v2_status().map(|s| s.meteors),
            Some(0),
            "record {k}: program output flew a meteor"
        );
        assert_eq!(
            ws.fx.cursor_mut().glow.spawns(),
            spawns,
            "record {k}: program output minted light (T1)"
        );
        unmoved.tick(Some((26, 9)), at, &cfg, geom, &mut unmoved_out);
        let reference_cells = unmoved.v2_status().expect("reference engaged").cells;
        if k == 0 {
            assert!(reference_cells > 0, "the reset control must be observable");
        }
        assert_eq!(
            ws.fx
                .cursor_mut()
                .glow
                .v2_status()
                .expect("moving ribbon engaged")
                .cells,
            reference_cells,
            "record {k}: a band changed the earned cells' lifetime"
        );
        if unmoved.ribbon_segments() > 0 {
            assert!(
                ws.fx.cursor_mut().glow.ribbon_segments() > 0,
                "record {k}: the moved ribbon went dark before its stationary twin"
            );
        }
        let now = ws.fx.cursor_mut().glow.typing_momentum(at);
        assert!(
            now <= momentum + 1e-6,
            "record {k}: momentum restarted ({momentum} -> {now}) instead of decaying"
        );
        momentum = now;
    }

    assert_eq!(
        bands, 39,
        "28 phase-A reverse indexes + 11 phase-B archival line feeds"
    );
    assert_eq!(
        ws.fx.cursor_mut().glow.band_moves(),
        39,
        "the glow was handed every one of them"
    );
    assert_eq!(caret.row, 54, "the composer's row at the end of the answer");
    assert_eq!(
        ws.fx
            .cursor_mut()
            .glow
            .v2_status()
            .expect("moving ribbon engaged")
            .cells,
        unmoved.v2_status().expect("reference engaged").cells,
        "the answer preserves exactly the cells their authored lifetime allows"
    );
}

#[test]
fn invalidation_runs_before_current_alt_context_and_drops_old_blink() {
    let mut app = App::headless_for_test();
    let mut cfg = app.glow_config();
    cfg.enabled = true;
    cfg.style = crate::cursor_glow::GlowStyle::Lumen;
    cfg.beam = true;
    let geom = crate::cursor_glow::Geom {
        cw: 8,
        ch: 16,
        rows: 10,
        cols: 20,
        origin_x: 0,
        origin_y: 0,
        win_w: 160,
        win_h: 160,
        head: 0,
    };
    let now = Instant::now();
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.fx
        .set_scroll_baseline(Some(ContentScrollState::default()));
    ws.fx.cursor_mut().glow.note_repaint_blink(now);
    let change = sync_cursor_effect_scroll(
        ws,
        ContentScrollState {
            uniform_up_rows: 0,
            invalidation_epoch: 1,
            ..Default::default()
        },
    );
    assert!(change.invalidated);
    ws.fx.cursor_mut().glow.note_context(true);

    let mut out = Vec::new();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((1, 1)), now, &cfg, geom, &mut out);
    ws.fx
        .cursor_mut()
        .glow
        .note_synthetic_typed(now + Duration::from_millis(1), 1);
    let fp = ws.fx.cursor_mut().glow.tick(
        Some((1, 8)),
        now + Duration::from_millis(2),
        &cfg,
        geom,
        &mut out,
    );
    assert_ne!(
        fp, 0,
        "alt context was re-fed and the pre-invalidation blink was dropped; \
         the deliberate alt move must not take the main/blinking re-anchor path"
    );
}
