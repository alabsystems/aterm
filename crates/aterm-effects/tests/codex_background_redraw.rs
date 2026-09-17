// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real-terminal probes for background composer redraws around genuine input.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

struct Host {
    term: Terminal,
    glow: CursorGlow,
    now: Instant,
    blink: u64,
    buf: Vec<char>,
    out: Vec<GlowQuad>,
}

impl Host {
    fn new() -> Self {
        let mut h = Self {
            term: Terminal::new(24, 80),
            glow: CursorGlow::default(),
            now: Instant::now(),
            blink: 0,
            buf: Vec::new(),
            out: Vec::new(),
        };
        h.term.process(b"\x1b[21;3H");
        h.frame();
        h
    }

    fn frame(&mut self) {
        let c = self.term.cursor();
        let cursor = self.term.cursor_visible().then_some((c.row, c.col));
        if self.term.repaint_blink_epoch() != self.blink {
            self.blink = self.term.repaint_blink_epoch();
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.glow.observe_print_anchor(self.term.print_anchor());
        self.term.row_cols_into(usize::from(c.row), &mut self.buf);
        self.glow.observe_row(c.row, c.col, &self.buf, self.now);
        self.glow.observe_ribbon_row(c.row, &self.buf);
        let mut rows = [0; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for row in &rows[..n] {
            self.term.row_cols_into(usize::from(*row), &mut self.buf);
            self.glow.observe_ribbon_row(*row, &self.buf);
        }
        let cfg = GlowConfig {
            classic_mono: false,
            ribbon_tall: true,
            ribbon_flat: false,
            enabled: true,
            dark_theme: true,
            theme_fg: 0x00c8_d3f5,
            theme_bg: 0x001a_1b26,
            style: GlowStyle::RainbowKitty,
            color: 0x0050_fa7b,
            accent: 0x007a_a2f7,
            duration: Duration::from_millis(240),
            length: 18,
            intensity: 0.7,
            radius: 0.6,
            ring: true,
            beam: false,
            head_dx: 0.5,
            pack: None,
        };
        let geom = Geom {
            cw: 8,
            ch: 16,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 640,
            win_h: 384,
            head: 0,
        };
        self.glow.tick(cursor, self.now, &cfg, geom, &mut self.out);
    }

    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    fn key(&mut self, byte: u8) {
        self.now += Duration::from_millis(90);
        self.glow.note_typed_cells(self.now, 1);
        self.term.process(&[byte]);
        self.frame();
    }

    fn idle(&mut self, millis: u64) {
        let end = self.now + Duration::from_millis(millis);
        while self.now < end {
            self.program(b"");
        }
    }

    fn clock(&self, row: u16) -> Instant {
        self.glow
            .v2_ribbon()
            .unwrap()
            .cohorts()
            .iter()
            .find(|c| c.row == row)
            .unwrap()
            .alive_at
    }

    fn abandoned(&self, row: u16) -> bool {
        self.glow
            .v2_ribbon()
            .unwrap()
            .cohorts()
            .iter()
            .filter(|c| c.row == row)
            .all(|c| c.abandoned)
    }

    fn live(&self, row: u16) -> Vec<u16> {
        let mut cols: Vec<_> = self
            .glow
            .v2_ribbon()
            .unwrap()
            .cells()
            .iter()
            .filter(|cell| cell.row == row && !cell.leaving())
            .map(|cell| cell.col)
            .collect();
        cols.sort_unstable();
        cols.dedup();
        cols
    }
}

#[test]
fn an_unchanged_composer_redrawn_in_one_batch_keeps_its_typed_band() {
    let mut h = Host::new();
    for b in b"abcdefgh" {
        h.key(*b);
    }
    h.program(b"\x1b[21;1H\x1b[2K  abcdefgh");
    for b in b"ijklmnop" {
        h.key(*b);
    }
    assert_eq!(h.live(20), (2..18).collect::<Vec<_>>());
}

#[test]
fn an_unchanged_composer_redrawn_across_frames_keeps_its_typed_band() {
    let mut h = Host::new();
    for b in b"abcdefgh" {
        h.key(*b);
    }
    h.program(b"\x1b[?25l\x1b[21;1H\x1b[2K");
    h.program(b"  abcdefgh\x1b[?25h");
    for b in b"ijklmnop" {
        h.key(*b);
    }
    assert_eq!(h.live(20), (2..18).collect::<Vec<_>>());
}

#[test]
fn a_background_footer_park_cannot_spend_the_composer_key() {
    let mut h = Host::new();
    h.program(b"\x1b[22;1Hstatus footer\x1b[21;3H");
    for b in b"abcdefgh" {
        h.key(*b);
    }
    h.now += Duration::from_millis(90);
    h.glow.note_typed_cells(h.now, 1);
    h.program(b"\x1b[22;30H.");
    h.program(b"\x1b[21;11Hi");
    h.program(b"");
    assert!(h.live(21).is_empty(), "footer stole key: {:?}", h.live(21));
    assert!(
        h.live(20).contains(&10),
        "actual echo went dark: {:?}",
        h.live(20)
    );
}

#[test]
fn background_particles_beside_typing_preserve_the_composer_and_leave_the_footer_dark() {
    let mut h = Host::new();
    h.program("\x1b[20;25H⠂\x1b[21;40H⡀\x1b[22;15H⠐\x1b[24;1Hstatus footer\x1b[21;3H".as_bytes());
    for (i, b) in b"abcdefghijklmnop".iter().enumerate() {
        h.key(*b);
        let glyph = if i % 2 == 0 { "⠁" } else { "⡀" };
        h.program(format!("\x1b[?2026h\x1b[20;25H{glyph}\x1b[21;40H{glyph}\x1b[22;15H{glyph}\x1b[21;{}H\x1b[?2026l", 4+i).as_bytes());
    }
    assert_eq!(h.live(20), (2..18).collect::<Vec<_>>());
    assert!(h.live(19).is_empty());
    assert!(h.live(21).is_empty());
    assert!(h.live(23).is_empty());
}

#[test]
fn a_footer_park_that_never_returns_expires_without_painting_the_footer() {
    let mut h = Host::new();
    for b in b"abcdefgh" {
        h.key(*b);
    }
    h.now += Duration::from_millis(90);
    h.glow.note_typed_cells(h.now, 1);
    h.program(b"\x1b[22;30H.");
    for i in 0..700 {
        if i % 5 == 0 {
            h.program(if i % 10 == 0 {
                b"\x1b[22;30H."
            } else {
                b"\x1b[22;32H."
            });
        } else {
            h.program(b"");
        }
        assert!(
            h.live(21).is_empty(),
            "unreturned park painted at frame {i}"
        );
    }
    assert!(
        h.glow.in_flight_tally().park_flushed > 0,
        "bounded custody expires"
    );
}

#[test]
fn a_delayed_original_echo_survives_multiple_foreign_row_repaints() {
    let mut h = Host::new();
    for b in b"abcdefgh" {
        h.key(*b);
    }
    h.now += Duration::from_millis(90);
    h.glow.note_typed_cells(h.now, 1);
    h.program(b"\x1b[22;30H.");
    for i in 0..40 {
        h.program(if i % 2 == 0 {
            b"\x1b[22;32H."
        } else {
            b"\x1b[22;30H."
        });
        assert!(h.live(21).is_empty());
    }
    h.program(b"\x1b[?25l");
    h.program(b"\x1b[21;11Hi\x1b[?25h");
    assert!(h.live(21).is_empty());
    assert!(
        h.live(20).contains(&10),
        "the actual delayed echo must be paid"
    );
    assert!(h.glow.in_flight_tally().park_returns > 0);
}

#[test]
fn a_real_return_and_natural_wrap_keep_their_immediate_row_move() {
    let mut h = Host::new();
    for b in b"abcdefgh" {
        h.key(*b);
    }
    h.now += Duration::from_millis(90);
    h.glow.note_return(h.now);
    h.program(b"\r\n");
    h.key(b'x');
    assert!(h.live(21).contains(&0));
    assert_eq!(h.glow.in_flight_tally().park_returns, 0);

    let mut h = Host::new();
    h.program(b"\x1b[21;79H");
    h.key(b'a');
    h.key(b'b');
    h.key(b'c');
    assert!(
        h.live(21).contains(&0),
        "the pending-margin wrap stays immediate"
    );
    assert_eq!(h.glow.in_flight_tally().park_returns, 0);
}

#[test]
fn a_park_and_its_real_echo_move_together_when_output_scrolls_the_grid() {
    let mut h = Host::new();
    for b in b"abcdefgh" {
        h.key(*b);
    }
    h.now += Duration::from_millis(90);
    h.glow.note_typed_cells(h.now, 1);
    h.program(b"\x1b[22;30H.");
    h.glow.note_scroll(1);
    h.program(b"\x1b[S\x1b[21;31H");
    h.program(b"\x1b[20;11Hi");
    assert!(h.live(20).is_empty());
    assert!(h.live(19).contains(&10));
    assert_eq!(h.glow.in_flight_tally().park_returns, 1);
}

#[test]
fn a_partial_restore_cannot_recover_the_clock_but_the_complete_original_can() {
    let mut h = Host::new();
    for b in b"ab" {
        h.key(*b);
    }
    let clock = h.clock(20);
    let identities: Vec<_> = h
        .glow
        .v2_ribbon()
        .unwrap()
        .cells()
        .iter()
        .map(|c| (c.row, c.col, c.born))
        .collect();
    h.program(b"\x1b[?25l\x1b[21;1H\x1b[2K");
    assert!(h.abandoned(20));
    h.program(b"  a");
    assert!(
        h.abandoned(20),
        "a partial redraw is not the complete original"
    );
    h.program(b"b\x1b[?25h");
    assert!(!h.abandoned(20));
    assert_eq!(
        h.clock(20),
        clock,
        "the original clock is recovered, never renewed"
    );
    assert_eq!(
        h.glow
            .v2_ribbon()
            .unwrap()
            .cells()
            .iter()
            .map(|c| (c.row, c.col, c.born))
            .collect::<Vec<_>>(),
        identities
    );
    let counted = h.glow.v2_status().unwrap().retired;
    h.program(b"\x1b[21;1H\x1b[2K");
    assert_eq!(
        h.glow.v2_status().unwrap().retired,
        counted,
        "identity counts survive restoration"
    );
}

#[test]
fn replaced_text_and_an_expired_original_do_not_restore() {
    let mut h = Host::new();
    for b in b"ab" {
        h.key(*b);
    }
    h.program(b"\x1b[?25l\x1b[21;1H\x1b[2K");
    h.program(b"  ax\x1b[?25h");
    h.idle(160);
    assert!(
        h.live(20).len() < 2,
        "a different glyph must melt the replaced band rather than restore it"
    );
    h.program(b"\x1b[21;3Hab");
    assert!(
        h.live(20).len() < 2,
        "replacement cannot rearm the old identity"
    );

    let mut h = Host::new();
    for b in b"ab" {
        h.key(*b);
    }
    h.program(b"\x1b[?25l\x1b[21;1H\x1b[2K");
    h.idle(800);
    h.program(b"  ab\x1b[?25h");
    assert!(h.live(20).is_empty(), "a redraw cannot mint expired cells");
}
