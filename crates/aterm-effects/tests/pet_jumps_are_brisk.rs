// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that the companion pet follows a jump of the caret (an Enter's new prompt, a line
//! cleared back to its prompt, the prompt under a listing) in one brisk flight to where it rests.

use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::companion::{CompanionOwner, GlowOwnership, PetFacts};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{FrameGeom, HostFrameInput, TerminalFacts};
use aterm_effects::kitty_pet::{OldPath, PetAction, PetFrame, PetInputKind};
use aterm_effects::pet_world::{PetPane, PetReserve, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;

const FRAME_MS: u64 = 16;
const PROMPT: &str = "~/project % ";
/// The longest a flight after a jump of the caret is in the air: the brisk bound's 0.55 s
/// (`kitty_pet::BRISK_DUR_MAX`), and the present it ends on.
const BRISK_AIR_MS: u64 = 550 + FRAME_MS;

/// What a replay does at an instant: a key the host notes, or output that lands.
enum Ev {
    Key(PetInputKind),
    Out(Vec<u8>),
}

/// A pane of `rows` x `cols` 15 x 28 px cells, the pet ticked every [`FRAME_MS`] as a window's
/// host ticks it, and where it was drawn on each present.
struct Host {
    rows: u16,
    cols: u16,
    term: Terminal,
    owner: CompanionOwner,
    decos: WordDecorations,
    lexicon: Lexicon,
    cfg: DecoConfig,
    t0: Instant,
    now: Instant,
    vsync: Instant,
    shots: Vec<(u64, PetFrame)>,
}

impl Host {
    fn new(rows: u16, cols: u16, old: &[OldPath]) -> Self {
        let mut term = Terminal::new(rows, cols);
        term.process(b"\x1b]11;#000000\x07");
        let mut owner = CompanionOwner::default();
        owner.set_enabled_seed(true, 0x5EED);
        for &law in old {
            owner.set_pet_old_path(law, true);
        }
        let now = Instant::now();
        Self {
            rows,
            cols,
            term,
            owner,
            decos: WordDecorations::default(),
            lexicon: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig::default(),
            t0: now,
            now,
            vsync: now + Duration::from_millis(FRAME_MS),
            shots: Vec::new(),
        }
    }

    fn present(&mut self) {
        let (rows, cols) = (self.rows, self.cols);
        let input: RenderInput = self.term.cell_frame(usize::from(rows), usize::from(cols));
        let world = PetWorldFacts::read(&self.term, 7);
        let reserve = PetReserve {
            protected: &[],
            overlays: &[],
            ink: &[],
            blanked: &[],
            painted_over: None,
        };
        self.owner
            .observe_console_reserved(&input, &world, PetPane::full(&input), reserve);
        let facts = TerminalFacts::read(&self.term, 7, false);
        let host = HostFrameInput::new(
            self.now,
            FrameGeom {
                rows,
                cols,
                cell_w: 15,
                cell_h: 28,
                origin_px: (0, 0),
            },
        );
        let tick = self.owner.prepare(
            PetFacts::new(
                &facts,
                &host,
                GlowOwnership {
                    enabled: true,
                    style: GlowStyle::RainbowKitty,
                    style_raw_names_pet: true,
                },
            ),
            &mut self.decos,
        );
        let frame = self.owner.tick(tick).pet;
        self.decos.rescan_from_cells(
            &input.cells,
            &input.line_sizes,
            usize::from(rows),
            usize::from(cols),
            &self.lexicon,
            &self.cfg,
            input.content_seq,
            self.now,
        );
        let ms = self.now.saturating_duration_since(self.t0).as_millis() as u64;
        self.shots.push((ms, frame));
    }

    /// Run `events` (`(ms, event)`, sorted) and `tail` ms after the last.
    fn run(&mut self, events: Vec<(u64, Ev)>, tail: u64) {
        let end = events.last().map_or(0, |e| e.0) + tail;
        for (ms, ev) in events.into_iter().chain([(end, Ev::Out(Vec::new()))]) {
            let t = self.t0 + Duration::from_millis(ms);
            while self.vsync <= t {
                self.now = self.vsync;
                self.present();
                self.vsync += Duration::from_millis(FRAME_MS);
            }
            self.now = self.now.max(t);
            match ev {
                Ev::Key(kind) => self.owner.note_console_input(self.now, kind),
                Ev::Out(bytes) => self.term.process_at(
                    &bytes,
                    ClockReading {
                        monotonic: self.now,
                        wall_ms: None,
                    },
                ),
            }
        }
    }
}

/// One flight: how long it was in the air (ms), the columns it was drawn at in the air, and its
/// landing `(col, row)`.
#[derive(Debug)]
struct Flight {
    aloft: u64,
    cols: Vec<f32>,
    landing: (f32, f32),
}

/// The flights that left from `from` ms on.
fn flights(shots: &[(u64, PetFrame)], from: u64) -> Vec<Flight> {
    let mut out = Vec::new();
    for w in 1..shots.len() {
        let ((_, before), (at, f)) = (&shots[w - 1], &shots[w]);
        if *at < from || f.action != PetAction::Leap || before.action == PetAction::Leap {
            continue;
        }
        let air: Vec<&(u64, PetFrame)> = shots[w..]
            .iter()
            .take_while(|(_, f)| f.action == PetAction::Leap)
            .collect();
        let Some((down, landed)) = shots.get(w + air.len()) else {
            continue;
        };
        out.push(Flight {
            aloft: down - at,
            cols: std::iter::once(before.col)
                .chain(air.iter().map(|(_, f)| f.col))
                .chain([landed.col])
                .collect(),
            landing: (landed.col, landed.row),
        });
    }
    out
}

/// `text` typed from `*t` ms, the first key `first` ms after it and each next 110 ms on, each
/// echoed 2 ms after the host notes it.
fn type_text(ev: &mut Vec<(u64, Ev)>, t: &mut u64, text: &str, first: u64) {
    for (i, c) in text.bytes().enumerate() {
        *t += if i == 0 { first } else { 110 };
        ev.push((*t, Ev::Key(PetInputKind::Text)));
        ev.push((*t + 2, Ev::Out(vec![c])));
    }
}

/// A screen of `above` lines over a prompt.
fn page(above: usize) -> Vec<u8> {
    let mut page = String::from("\x1b[2J\x1b[H");
    for i in 0..above {
        page.push_str(&format!("line {i}\r\n"));
    }
    page.push_str(PROMPT);
    page.into_bytes()
}

/// `command` typed at a prompt under `above` lines and entered, the shell printing `output`
/// and a new prompt 5 ms after the Enter; the Enter's time with the events.
fn entered(above: usize, command: &str, output: &str) -> (Vec<(u64, Ev)>, u64) {
    let mut ev = vec![(300, Ev::Out(page(above)))];
    let mut t = 2_000;
    type_text(&mut ev, &mut t, command, 400);
    t += 350;
    ev.push((t, Ev::Key(PetInputKind::Submit)));
    ev.push((t + 5, Ev::Out(format!("\r\n{output}{PROMPT}").into_bytes())));
    (ev, t)
}

/// A line typed at a prompt under `above` lines, then cleared back to the prompt by zsh's
/// Ctrl+U 1.2 s later; the Ctrl+U's time with the events.
fn cleared(above: usize, cols: u16) -> (Vec<(u64, Ev)>, u64) {
    const LINE: &str =
        "printf the quick brown fox jumps over the lazy dog and keeps on running far past the barn";
    let mut ev = vec![(300, Ev::Out(page(above)))];
    let mut t = 2_000;
    type_text(&mut ev, &mut t, LINE, 400);
    t += 1_200;
    ev.push((t, Ev::Key(PetInputKind::Delete)));
    let up = (PROMPT.len() + LINE.len() - 1) / usize::from(cols);
    let mut out = String::new();
    if up > 0 {
        out.push_str(&format!("\x1b[{up}A"));
    }
    out.push_str(&format!("\r\x1b[{}C\x1b[J", PROMPT.len()));
    ev.push((t + 5, Ev::Out(out.into_bytes())));
    (ev, t)
}

/// `n` lines of an `ls -la` listing and the prompt under them.
fn listing(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        let mut line = format!(
            "drwxr-xr-x@  {i:2} someone  staff  {:5} Oct 15 13:54 ",
            i * 37
        );
        while line.len() < 40 + (i * 7919 % 31) {
            line.push('x');
        }
        out.push_str(&line);
        out.push_str("\r\n");
    }
    out
}

/// r25 K8-3, EVERY BOUND AFTER THE CARET IS BRISK: an Enter's new prompt under a long typed
/// line, a long line cleared back to its prompt by Ctrl+U, and the prompt under an `ls -la`
/// listing (on screen and scrolled), at 80, 102 and 150 columns, each cost one flight, down in
/// the brisk bound's 0.55 s, that never turns back in the air. Paced as a quiet crossing, the
/// flight back to the prompt after a Ctrl+U hung 0.86 to 1.10 s in the air and the one after
/// `ls -la` 0.78 to 0.94 s (windowed zsh, round 24: up to 1.07 s and 0.91 to 0.96 s), and the
/// Enter's, re-aimed late for the seat past the output's end, turned round in the air for it.
#[test]
fn every_flight_after_a_caret_jump_is_one_brisk_bound() {
    let typed = "echo the quick brown fox jumps over the lazy dog";
    for (cols, rows) in [(80u16, 24u16), (102, 29), (150, 48)] {
        let near_foot = usize::from(rows) - 4;
        let scenes = [
            (
                "Enter",
                entered(
                    near_foot,
                    typed,
                    "the quick brown fox jumps over the lazy dog\r\n",
                ),
            ),
            ("Ctrl+U", cleared(near_foot, cols)),
            ("ls -la", entered(0, "ls -la", &listing(20))),
            (
                "ls -la scrolled",
                entered(0, "ls -la", &listing(usize::from(rows) + 6)),
            ),
        ];
        for (name, (events, at)) in scenes {
            let mut host = Host::new(rows, cols, &[]);
            host.run(events, 4_000);
            let flights = flights(&host.shots, at);
            assert_eq!(
                flights.len(),
                1,
                "{cols}x{rows} {name}: one flight, not {flights:?}"
            );
            let f = &flights[0];
            assert!(
                f.aloft <= BRISK_AIR_MS,
                "{cols}x{rows} {name}: {} ms in the air, the brisk bound's {BRISK_AIR_MS} at \
                 most: {f:?}",
                f.aloft
            );
            let way = f.landing.0 - f.cols[0];
            assert!(
                f.cols.windows(2).all(|c| (c[1] - c[0]) * way >= -1e-3),
                "{cols}x{rows} {name}: the flight turned back in the air: {f:?}"
            );
        }
    }
}

/// r25 K8-3, AN ENTER AFTER A SHORT COMMAND COSTS ONE FLIGHT: a short command whose output is
/// one long line (`ls` of a long name, `pwd` deep in a tree, `git status`'s first line) puts the
/// new prompt under letters, and the seat the cat rests on past the line's end. The cat near the
/// prompt waits in its coil for the new prompt to come to rest and goes once, to that seat.
/// Hopped at once to the stand beside the prompt, it came down there and flew on to the seat
/// 0.8 to 1.2 s later (two flights), and after `pwd` printing 70 columns on 80 to a further one
/// (three). Negative control ([`OldPath::BoundLeavesUnrested`]): it leaves at once.
#[test]
fn an_enter_after_a_short_command_s_long_line_costs_one_flight() {
    let line = |n: usize| -> String {
        let mut s: String = "abcdefghij ".chars().cycle().take(n).collect();
        s.push_str("\r\n");
        s
    };
    for (cols, rows) in [(80u16, 24u16), (102, 29)] {
        for (command, out) in [("ls", 60usize), ("pwd", 70), ("git status", 45)] {
            let (events, at) = entered(usize::from(rows) - 4, command, &line(out));
            let mut host = Host::new(rows, cols, &[]);
            host.run(events, 4_000);
            let flights = flights(&host.shots, at);
            assert_eq!(
                flights.len(),
                1,
                "{cols}x{rows} `{command}` printing {out} columns: one flight, not {flights:?}"
            );
        }
    }
    let (events, at) = entered(20, "ls", &line(60));
    let mut old = Host::new(24, 80, &[OldPath::BoundLeavesUnrested]);
    old.run(events, 4_000);
    let flights = flights(&old.shots, at);
    assert!(
        flights.len() >= 2,
        "negative control: leaving at once, the cat still flew once ({flights:?})"
    );
}
