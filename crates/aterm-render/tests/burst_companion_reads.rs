// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests glyph-pixel contrast of typing bursts rendered with the companion sprite.

use std::time::{Duration, Instant};

use crate::wcag::contrast;
use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::companion::{CompanionOwner, ContrastFallback, GlowOwnership, PetFacts};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{
    CaptureMode, FrameGeom, HostFrameInput, SingFacts, TerminalFacts, Visibility,
};
use aterm_effects::kitty_pet::{PetAction, PetInputKind};
use aterm_effects::pet_world::{PetPane, PetReserve, PetWorldFacts};
use aterm_effects::word_decorations::WordDecorations;
use aterm_render::sprites::FreeZ;
use aterm_render::{Renderer, Theme};

const ROWS: u16 = 14;
const COLS: u16 = 72;
const FRAME_MS: u64 = 16;
const KEY_MS: u64 = 50;
const LINE: &str = "echo the quick brown fox jumps over the lazy dog as the kitty flies";
const BAR: f64 = 4.5;
const SOLID_SHARE: f64 = 0.85;

fn median(px: &mut [u32]) -> u32 {
    let mid = px.len() / 2;
    let mut out = 0;
    for sh in [16, 8, 0] {
        let mut ch: Vec<u32> = px.iter().map(|p| (p >> sh) & 0xff).collect();
        ch.sort_unstable();
        out |= ch[mid] << sh;
    }
    out
}

fn worst_ink(
    clean: &[u32],
    frame: &[u32],
    width: usize,
    (ink, page): (u32, u32),
    letter: impl Fn(usize, usize) -> bool,
) -> (f64, usize, usize) {
    let height = clean.len() / width;
    let solid = SOLID_SHARE * contrast(ink, page);
    let mut worst = (f64::INFINITY, 0, 0);
    let mut ring = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            if contrast(clean[i], page) < solid || !letter(x, y) {
                continue;
            }
            for d in 1..=2i64 {
                ring.clear();
                for dy in -d..=d {
                    for dx in -d..=d {
                        if dx.abs().max(dy.abs()) != d {
                            continue;
                        }
                        let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                        if xx < 0 || yy < 0 || xx >= width as i64 || yy >= height as i64 {
                            continue;
                        }
                        let j = yy as usize * width + xx as usize;
                        if clean[j] == page {
                            ring.push(frame[j]);
                        }
                    }
                }
                if !ring.is_empty() {
                    break;
                }
            }
            if ring.is_empty() {
                continue;
            }
            let c = contrast(frame[i], median(&mut ring));
            if c < worst.0 {
                worst = (c, x, y);
            }
        }
    }
    worst
}

struct Replay {
    term: Terminal,
    owner: CompanionOwner,
    decos: WordDecorations,
    rend: Renderer,
    cell: (u16, u16),
    ink: u32,
    page: u32,
    t0: Instant,
    now: Instant,
    clean: Option<(u64, Vec<u32>)>,
}

struct Judged {
    ms: u64,
    action: PetAction,
    under: bool,
    worst: (f64, usize, usize),
}

impl Replay {
    fn new(ink: u32, page: u32) -> Self {
        let rend = Renderer::from_system(22.0, Theme::default())
            .expect("no font: build with aterm-render's default embedded-font feature");
        let (cw, ch) = rend.cell_size();
        let mut term = Terminal::new(ROWS, COLS);
        term.process(format!("\x1b]11;#{page:06x}\x07\x1b]10;#{ink:06x}\x07").as_bytes());
        let mut owner = CompanionOwner::default();
        owner.set_enabled_seed(true, 0x5EED);
        let now = Instant::now();
        Self {
            term,
            owner,
            decos: WordDecorations::default(),
            rend,
            cell: (cw as u16, ch as u16),
            ink,
            page,
            t0: now,
            now,
            clean: None,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn out(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.now,
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
    }

    fn present(&mut self) -> Option<Judged> {
        let (cw, ch) = self.cell;
        let mut input: RenderInput = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let world = PetWorldFacts::read(&self.term, 7);
        let reserve = PetReserve {
            protected: &[],
            overlays: &[],
            ink: &[],
        };
        self.owner
            .observe_console_reserved(&input, &world, PetPane::full(&input), reserve);
        let facts = TerminalFacts::read(&self.term, 7, false);
        let host = HostFrameInput {
            now: self.now,
            visibility: Visibility::Focused,
            reduced_motion: false,
            serious: false,
            shed_envelope: 1.0,
            shed_active: false,
            pointer_px: None,
            capture: CaptureMode::Present,
            geometry: FrameGeom {
                rows: ROWS,
                cols: COLS,
                cell_w: cw,
                cell_h: ch,
                origin_px: (0, 0),
            },
        };
        let tick = self.owner.prepare(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: GlowOwnership {
                    enabled: true,
                    style: GlowStyle::RainbowKitty,
                    style_raw_names_pet: true,
                },
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut self.decos,
        );
        let t = self.owner.tick(tick);
        let mut free = Vec::new();
        let fallback = ContrastFallback {
            bg: self.page,
            cursor: 0x00FF_FFFF,
            accent: 0x00FF_66CC,
        };
        let _ = self
            .owner
            .emit(&t, &input.cells, fallback, &mut self.decos, &mut free);
        let near_letters = free.iter().any(|s| {
            let (x0, x1) = (s.x - i32::from(cw), s.x + i32::from(s.w) + i32::from(cw));
            let (y0, y1) = (s.y - i32::from(ch), s.y + i32::from(s.h) + i32::from(ch));
            input.cells.iter().enumerate().any(|(row, line)| {
                let (ry0, ry1) = (row as i32 * i32::from(ch), (row as i32 + 1) * i32::from(ch));
                ry1 > y0
                    && ry0 < y1
                    && line.iter().enumerate().any(|(col, c)| {
                        let cx = col as i32 * i32::from(cw);
                        c.ch.is_alphanumeric() && cx + i32::from(cw) > x0 && cx < x1
                    })
            })
        });
        if !near_letters {
            return None;
        }
        let seq = self.term.screen_mutations();
        if self.clean.as_ref().is_none_or(|(at, _)| *at != seq) {
            let clean = self.rend.render_input(&input).pixels.clone();
            self.clean = Some((seq, clean));
        }
        let under = free.iter().any(|s| s.z == FreeZ::UnderText);
        crate::sprites::put(&mut input, &free);
        let frame = self.rend.render_input(&input).pixels.clone();
        let width = usize::from(COLS) * usize::from(cw);
        let clean = &self.clean.as_ref().expect("rendered above").1;
        // Letters only: the caret's own cell is the caret law's (`cursor_ink`), not this one's.
        let caret = (input.cursor_row, input.cursor_col);
        let cells = &input.cells;
        let letter = |x: usize, y: usize| {
            let (row, col) = (y / usize::from(ch), x / usize::from(cw));
            (row, col) != caret
                && cells
                    .get(row)
                    .and_then(|l| l.get(col))
                    .is_some_and(|c| c.ch.is_alphanumeric())
        };
        Some(Judged {
            ms: self.ms(),
            action: t.pet.action,
            under,
            worst: worst_ink(clean, &frame, width, (self.ink, self.page), letter),
        })
    }

    fn burst(&mut self, tail: u64) -> (Vec<Judged>, u64) {
        let mut events: Vec<(u64, Option<u8>, Vec<u8>)> =
            vec![(300, None, b"\x1b[2J\x1b[H\r\n\r\n\r\n\r\n$ ".to_vec())];
        let mut t = 1_000;
        for c in LINE.bytes() {
            events.push((t, Some(c), Vec::new()));
            events.push((t + 2, None, vec![c]));
            t += KEY_MS;
        }
        let last = t - KEY_MS;
        let end = last + tail;
        let mut judged = Vec::new();
        let mut vsync = self.t0 + Duration::from_millis(FRAME_MS);
        let mut next = 0;
        while self.ms() < end {
            self.now = vsync;
            while next < events.len() && events[next].0 <= self.ms() {
                let (_, key, bytes) = &events[next];
                if key.is_some() {
                    self.owner.note_console_input(self.now, PetInputKind::Text);
                } else {
                    let bytes = bytes.clone();
                    self.out(&bytes);
                }
                next += 1;
            }
            if let Some(j) = self.present() {
                judged.push(j);
            }
            vsync += Duration::from_millis(FRAME_MS);
        }
        (judged, last)
    }
}

#[test]
fn every_letter_the_burst_s_companion_lies_on_reads_at_the_bar() {
    for (name, ink, page) in [
        ("dark", 0x00D0_D0D0u32, 0x0011_1318u32),
        ("light", 0x001F_2328, 0x00FF_FFFF),
    ] {
        let mut r = Replay::new(ink, page);
        let (judged, last) = r.burst(4_600);
        let under: Vec<&Judged> = judged.iter().filter(|j| j.under).collect();
        assert!(
            under.len() > 50,
            "{name}: the companion must lie under the letters for a good part of the replay \
             ({} frames), or the replay judges nothing",
            under.len()
        );
        let rest = judged
            .iter()
            .rfind(|j| j.ms > last + 3_000)
            .expect("judged frames after the burst");
        assert!(
            rest.action.settled() && !rest.under,
            "{name}: the pet rests with its body off the line ({:?}, under {})",
            rest.action,
            rest.under
        );
        for j in &judged {
            assert!(
                j.worst.0 >= BAR,
                "{name}: {} ms after the last key ({:?}, under the ink: {}) a letter pixel at \
                 ({}, {}) reads {:.2}:1 against the ground next to it",
                j.ms as i64 - last as i64,
                j.action,
                j.under,
                j.worst.1,
                j.worst.2,
                j.worst.0
            );
        }

        let mut off = Replay::new(ink, page);
        off.rend.set_knock_out(false);
        let (judged, _) = off.burst(4_600);
        let worst = judged
            .iter()
            .filter(|j| j.under)
            .map(|j| j.worst.0)
            .fold(f64::INFINITY, f64::min);
        assert!(
            worst < BAR,
            "negative control, {name}: with the knock-out off the body under the letters must \
             take one under {BAR}:1 (worst {worst:.2})"
        );
    }
}
