// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Every letter a cursor trail lies on reads at the bar: the trail gallery's scenes replayed through
// the shipping effects pipeline (the page host's `EffectsPipeline::apply`, which runs the same
// cursor family as a window) and the real renderer, every 16 ms frame, each lettered cell judged
// as `aterm-lab trail-contrast` judges a gallery frame: the lower of the whole cell's median ink
// against its median ground and the worst pixel row's lower median of each solid ink pixel against
// its nearest ring of ground (`local_contrast`), on the default dark theme, GitHub Light and
// Default Light.

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Rgb, Terminal};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::kitty_pet::PetInputKind;
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::{Renderer, Theme};

pub const ROWS: u16 = 14;
pub const COLS: u16 = 72;
const FRAME_MS: u64 = 16;
pub const BAR: f64 = 4.5;
const SOLID_SHARE: f64 = 0.85;
const CORE_SHARE: f64 = 0.70;
const BLANK_BELOW: f64 = 1.5;
const RING_REACH: i64 = 2;

/// A theme as the gallery photographs it: text, page and caret.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub name: &'static str,
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
}

pub const DARK: Look = Look {
    name: "dark",
    fg: 0x00D0_D0D0,
    bg: 0x0011_1318,
    cursor: 0x0050_FA7B,
};
pub const GITHUB_LIGHT: Look = Look {
    name: "light",
    fg: 0x001F_2328,
    bg: 0x00FF_FFFF,
    cursor: 0x0009_69DA,
};
pub const DEFAULT_LIGHT: Look = Look {
    name: "default-light",
    fg: 0x002B_2D33,
    bg: 0x00F6_F7F9,
    cursor: 0x001A_7F37,
};
/// Text that clears the bar with little room, whose solid ink reads under
/// it on the bare page: a 5.0:1 grey on the dark page, Solarized Dark's
/// text (4.75:1) and a 4.95:1 grey on white.
pub const DIM_GREY: Look = Look {
    name: "dim-grey",
    fg: 0x0085_8585,
    bg: 0x0011_1318,
    cursor: 0x0050_FA7B,
};
pub const SOLARIZED_DARK: Look = Look {
    name: "solarized-dark",
    fg: 0x0083_9496,
    bg: 0x0000_2B36,
    cursor: 0x0093_A1A1,
};
pub const PAPER_GREY: Look = Look {
    name: "paper-grey",
    fg: 0x0070_7070,
    bg: 0x00FF_FFFF,
    cursor: 0x0009_69DA,
};

fn lin(c: u32) -> f64 {
    let s = f64::from(c & 0xff) / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn luma(p: u32) -> f64 {
    0.2126 * lin(p >> 16) + 0.7152 * lin(p >> 8) + 0.0722 * lin(p)
}

pub fn contrast(a: u32, b: u32) -> f64 {
    let (la, lb) = (luma(a), luma(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Each channel's median (the upper one of an even count).
fn median(px: &[u32]) -> u32 {
    let mid = px.len() / 2;
    let mut out = 0;
    let mut ch = Vec::with_capacity(px.len());
    for sh in [16, 8, 0] {
        ch.clear();
        ch.extend(px.iter().map(|p| (p >> sh) & 0xff));
        ch.sort_unstable();
        out |= ch[mid] << sh;
    }
    out
}

fn mode(px: &[u32]) -> u32 {
    let mut v: Vec<u32> = px.iter().map(|p| p & 0x00FF_FFFF).collect();
    v.sort_unstable();
    let (mut best, mut best_n, mut i) = (v[0], 0, 0);
    while i < v.len() {
        let mut j = i;
        while j < v.len() && v[j] == v[i] {
            j += 1;
        }
        if j - i > best_n {
            best_n = j - i;
            best = v[i];
        }
        i = j;
    }
    best
}

/// One judged letter.
#[derive(Clone, Copy, Debug)]
pub struct Letter {
    pub row: usize,
    pub col: usize,
    pub ch: char,
    pub cell: f64,
    pub local: f64,
}

impl Letter {
    pub fn judged(&self) -> f64 {
        self.cell.min(self.local)
    }
}

/// A frame and its clean twin, `w` px wide, cells `cw`x`ch` from the frame's origin.
pub struct Pair<'a> {
    pub frame: &'a [u32],
    pub clean: &'a [u32],
    pub w: usize,
    pub cw: usize,
    pub ch: usize,
}

impl Pair<'_> {
    fn at(px: &[u32], w: usize, x: usize, y: usize) -> u32 {
        px[y * w + x] & 0x00FF_FFFF
    }

    /// The cell `(row, col)` as `aterm-lab trail-contrast` measures it; `None` for a blank cell or
    /// one the effects left as its twin.
    pub fn letter(&self, row: usize, col: usize, span: usize, ch: char) -> Option<Letter> {
        let h = self.clean.len() / self.w;
        let (x0, y0) = (col * self.cw, row * self.ch);
        let (x1, y1) = ((x0 + span * self.cw).min(self.w), (y0 + self.ch).min(h));
        let mut same = true;
        let mut clean_px = Vec::with_capacity((x1 - x0) * (y1 - y0));
        let mut frame_px = Vec::with_capacity(clean_px.capacity());
        for y in y0..y1 {
            for x in x0..x1 {
                let (c, f) = (
                    Self::at(self.clean, self.w, x, y),
                    Self::at(self.frame, self.w, x, y),
                );
                same &= c == f;
                clean_px.push(c);
                frame_px.push(f);
            }
        }
        if same || clean_px.is_empty() {
            return None;
        }
        let ground = mode(&clean_px);
        let extreme = clean_px
            .iter()
            .copied()
            .max_by(|a, b| contrast(*a, ground).total_cmp(&contrast(*b, ground)))?;
        if contrast(extreme, ground) < BLANK_BELOW {
            return None;
        }
        let gl = luma(ground);
        let reach = CORE_SHARE * (luma(extreme) - gl).abs();
        let (mut ink, mut bed) = (Vec::new(), Vec::new());
        for (&c, &f) in clean_px.iter().zip(&frame_px) {
            if c == ground {
                bed.push(f);
            } else if (luma(c) - gl).abs() >= reach {
                ink.push(f);
            }
        }
        let cell = if ink.is_empty() || bed.is_empty() {
            f64::INFINITY
        } else {
            contrast(median(&ink), median(&bed))
        };
        let solid = SOLID_SHARE * contrast(extreme, ground);
        let mut worst_row = f64::INFINITY;
        let mut ratios = Vec::new();
        let mut ring = Vec::new();
        for y in y0..y1 {
            ratios.clear();
            for x in x0..x1 {
                if contrast(Self::at(self.clean, self.w, x, y), ground) < solid {
                    continue;
                }
                let mut g = None;
                for d in 1..=RING_REACH {
                    ring.clear();
                    for dy in -d..=d {
                        for dx in -d..=d {
                            if dx.abs().max(dy.abs()) != d {
                                continue;
                            }
                            let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                            if xx < 0 || yy < 0 || xx >= self.w as i64 || yy >= h as i64 {
                                continue;
                            }
                            let (xx, yy) = (xx as usize, yy as usize);
                            if Self::at(self.clean, self.w, xx, yy) == ground {
                                ring.push(Self::at(self.frame, self.w, xx, yy));
                            }
                        }
                    }
                    if !ring.is_empty() {
                        g = Some(median(&ring));
                        break;
                    }
                }
                if let Some(g) = g {
                    ratios.push(contrast(Self::at(self.frame, self.w, x, y), g));
                }
            }
            if !ratios.is_empty() {
                ratios.sort_by(f64::total_cmp);
                worst_row = worst_row.min(ratios[(ratios.len() - 1) / 2]);
            }
        }
        Some(Letter {
            row,
            col,
            ch,
            cell,
            local: worst_row,
        })
    }
}

/// What a replay does at an instant.
#[derive(Clone, Debug)]
pub enum Ev {
    /// A key: the host notes it, and its echo lands 2 ms later.
    Key(char, Vec<u8>),
    /// Output lands.
    Out(Vec<u8>),
}

/// One judged frame.
#[derive(Clone, Debug)]
pub struct Judged {
    pub ms: u64,
    pub letters: Vec<Letter>,
}

impl Judged {
    pub fn worst(&self) -> Option<Letter> {
        self.letters
            .iter()
            .copied()
            .min_by(|a, b| a.judged().total_cmp(&b.judged()))
    }
}

pub struct Replay {
    pub term: Terminal,
    pub fx: EffectsPipeline,
    pub rend: Renderer,
    pub cell: (usize, usize),
    pub look: Look,
    pub ms: u64,
    clean: Option<(u64, (usize, usize), Vec<u32>)>,
    /// Frames to keep as PNGs: `(ms, path)`.
    pub dump: Vec<(u64, String)>,
}

impl Replay {
    pub fn new(style: &str, look: Look, trail_color: TrailColor) -> Self {
        let rend = Renderer::from_system(22.0, Theme::default())
            .expect("no font: build with aterm-render's default embedded-font feature");
        let (cw, ch) = rend.cell_size();
        let mut term = Terminal::new(ROWS, COLS);
        let rgb = |c: u32| Rgb {
            r: (c >> 16) as u8,
            g: (c >> 8) as u8,
            b: c as u8,
        };
        term.set_default_foreground(rgb(look.fg));
        term.set_default_background(rgb(look.bg));
        term.set_default_cursor_color(Some(rgb(look.cursor)));
        let mut fx = EffectsPipeline::new();
        fx.set_focused(true);
        let classic = style.trim().eq_ignore_ascii_case("classic");
        fx.set_cursor_glow(
            true,
            style,
            trail_color,
            None,
            260,
            24,
            if classic { 0.7 } else { 1.0 },
            0.6,
            true,
            look.cursor,
        );
        let pet = aterm_effects::cursor_glow::GlowStyle::style_names_any_pet(style);
        fx.set_cursor_pet(pet, 0x5EED);
        Self {
            term,
            fx,
            rend,
            cell: (cw, ch),
            look,
            ms: 0,
            clean: None,
            dump: Vec::new(),
        }
    }

    fn to(&mut self, ms: u64) {
        if ms > self.ms {
            self.fx.advance((ms - self.ms) as f64);
            self.ms = ms;
        }
    }

    fn out(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.fx.now(),
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
    }

    /// Present one frame and judge its letters.
    pub fn present(&mut self) -> Judged {
        let (cw, ch) = self.cell;
        let (rows, cols) = (usize::from(ROWS), usize::from(COLS));
        let mut input: RenderInput = self.term.cell_frame(rows, cols);
        let caret = (input.cursor_row, input.cursor_col);
        let key = (self.term.screen_mutations(), caret);
        if self
            .clean
            .as_ref()
            .is_none_or(|(at, c, _)| (*at, *c) != key)
        {
            let clean = self.rend.render_input(&input).pixels;
            self.clean = Some((key.0, key.1, clean));
        }
        self.fx.apply(&mut self.term, &mut input, cw, ch);
        let frame = self.rend.render_input(&input);
        if let Some(i) = self.dump.iter().position(|(at, _)| *at == self.ms) {
            let (_, path) = self.dump.remove(i);
            std::fs::write(&path, frame.to_png()).expect("dump");
        }
        let w = frame.width;
        let clean = &self.clean.as_ref().expect("rendered above").2;
        let pair = Pair {
            frame: &frame.pixels,
            clean,
            w,
            cw,
            ch,
        };
        let mut letters = Vec::new();
        for (row, line) in input.cells.iter().enumerate() {
            for (col, c) in line.iter().enumerate().take(cols) {
                if (row, col) == caret || c.ch == ' ' || c.ch == '\0' || c.ch.is_control() {
                    continue;
                }
                if !c.ch.is_alphanumeric() {
                    continue;
                }
                let span = if c.wide { 2 } else { 1 };
                if let Some(l) = pair.letter(row, col, span, c.ch) {
                    letters.push(l);
                }
            }
        }
        Judged {
            ms: self.ms,
            letters,
        }
    }

    /// Run `events` (`(ms, event)`, sorted) and `tail` ms after the last, presenting a frame every
    /// [`FRAME_MS`]; every frame judged.
    pub fn run(&mut self, events: &[(u64, Ev)], tail: u64) -> Vec<Judged> {
        let mut timeline: Vec<(u64, u8, usize)> = Vec::new();
        for (i, (at, ev)) in events.iter().enumerate() {
            timeline.push((*at, 0, i));
            if matches!(ev, Ev::Key(..)) {
                timeline.push((*at + 2, 1, i));
            }
        }
        let end = events.last().map_or(0, |e| e.0) + tail;
        let mut f = FRAME_MS;
        while f <= end {
            timeline.push((f, 2, 0));
            f += FRAME_MS;
        }
        timeline.sort_unstable();
        let mut judged = Vec::new();
        for (at, kind, i) in timeline {
            self.to(at);
            match (kind, &events[i].1) {
                (0, Ev::Key('\r', _)) => {
                    // A window's Enter: the console's submit (the trail's
                    // return flare and the companion's) and the keystroke.
                    self.fx.note_console_input(PetInputKind::Submit);
                    self.fx.note_keystroke();
                }
                (0, Ev::Key(c, _)) => {
                    let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
                    let _ = self.fx.note_committed_char(&mut self.term, &input, *c);
                }
                (0, Ev::Out(bytes)) | (1, Ev::Key(_, bytes)) => {
                    let bytes = bytes.clone();
                    self.out(&bytes);
                }
                (2, _) => judged.push(self.present()),
                _ => {}
            }
        }
        judged
    }
}

/// `text` typed from `start` ms, a key every `every` ms.
pub fn typed(text: &str, start: u64, every: u64) -> Vec<(u64, Ev)> {
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let mut b = [0u8; 4];
            (
                start + i as u64 * every,
                Ev::Key(c, c.encode_utf8(&mut b).as_bytes().to_vec()),
            )
        })
        .collect()
}

pub const TYPING_TEXT: &str = "echo the quick brown fox";
pub const BURST_TEXT: &str = "echo the quick brown fox jumps over the lazy dog as the kitty flies";

/// The gallery's scenes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    Typing,
    EnterBottom,
    Burst,
    Cjk,
}

impl Scene {
    pub fn events(self) -> (Vec<(u64, Ev)>, u64) {
        let prompt = |at: u64| (at, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()));
        match self {
            Scene::Typing => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed(TYPING_TEXT, 1_000, 60));
                (ev, 600)
            }
            Scene::Cjk => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed("echo 你好世界", 1_000, 60));
                (ev, 600)
            }
            Scene::Burst => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed(BURST_TEXT, 1_000, 50));
                (ev, 4_600)
            }
            Scene::EnterBottom => {
                let mut seq = String::from("\x1b[2J\x1b[H$ seq 1 40\r\n");
                for n in 1..=40 {
                    seq.push_str(&format!("{n}\r\n"));
                }
                seq.push_str("$ ");
                let mut ev = vec![(200, Ev::Out(seq.into_bytes()))];
                ev.extend(typed(TYPING_TEXT, 1_000, 60));
                let enter = 1_000 + 60 * TYPING_TEXT.len() as u64 + 200;
                ev.push((enter, Ev::Key('\r', b"\r\n".to_vec())));
                ev.push((enter + 6, Ev::Out(b"the quick brown fox\r\n$ ".to_vec())));
                (ev, 600)
            }
        }
    }
}

/// The worst letter of a replay and its frame.
pub fn worst(judged: &[Judged]) -> Option<(u64, Letter)> {
    judged
        .iter()
        .filter_map(|j| j.worst().map(|l| (j.ms, l)))
        .min_by(|a, b| a.1.judged().total_cmp(&b.1.judged()))
}

pub const STYLES: [&str; 14] = [
    "rainbow kitty pet",
    "rainbow kitty underline",
    "rainbow dog",
    "flat rainbow",
    "nyan rainbow",
    "phaser",
    "fire",
    "water",
    "laser",
    "comet",
    "sparkle",
    "lumen",
    "beam",
    "classic",
];

/// One replay's verdict: how many judged letters fell under the bar, and the worst.
#[derive(Clone, Debug)]
pub struct Verdict {
    pub style: &'static str,
    pub look: &'static str,
    pub scene: Scene,
    pub under: usize,
    pub worst: Option<(u64, Letter)>,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:24} {:13} {:12?} under={:5} worst=",
            self.style, self.look, self.scene, self.under
        )?;
        match self.worst {
            None => write!(f, "-"),
            Some((ms, l)) => write!(
                f,
                "{:.2} (cell {:.2} local {:.2}) at {ms} ms r{} c{} '{}'",
                l.judged(),
                l.cell,
                l.local,
                l.row,
                l.col,
                l.ch
            ),
        }
    }
}

/// The bar a letter on `look` is judged at: [`BAR`], or four fifths of the theme's own
/// contrast where that leaves its clean letters under the bar (the judge reads a clean
/// letter's solid ink at [`SOLID_SHARE`] of it).
pub fn look_bar(look: Look) -> f64 {
    BAR.min(0.8 * contrast(look.fg, look.bg))
}

/// Replay `style` on `look` through `scene`, the letter guard on or off, every frame judged
/// at [`look_bar`]; `dump` names frames to keep as PNGs.
pub fn replay(
    style: &'static str,
    look: Look,
    scene: Scene,
    guard: bool,
    dump: Vec<(u64, String)>,
) -> Verdict {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.fx.set_letter_guard(guard);
    r.dump = dump;
    let (ev, tail) = scene.events();
    let judged = r.run(&ev, tail);
    let bar = look_bar(look);
    Verdict {
        style,
        look: r.look.name,
        scene,
        under: judged
            .iter()
            .map(|j| j.letters.iter().filter(|l| l.judged() < bar).count())
            .sum(),
        worst: worst(&judged),
    }
}

/// [`replay`] every job, spread over the machine's cores.
pub fn replay_all(
    jobs: &[(&'static str, Look, Scene)],
    guard: bool,
    dump: impl Fn(&str, Look, Scene) -> Vec<(u64, String)> + Sync,
) -> Vec<Verdict> {
    let lanes = std::thread::available_parallelism().map_or(4, usize::from);
    let dump = &dump;
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(lanes).max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(style, look, scene)| {
                            replay(style, look, scene, guard, dump(style, look, scene))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a replay lane"))
            .collect()
    })
}

const SCENES: [Scene; 4] = [Scene::Typing, Scene::EnterBottom, Scene::Burst, Scene::Cjk];
const LOOKS: [Look; 3] = [DARK, GITHUB_LIGHT, DEFAULT_LIGHT];
const DIM_LOOKS: [Look; 3] = [DIM_GREY, SOLARIZED_DARK, PAPER_GREY];

/// THE LEGIBILITY LAW, judged frame by frame: every style the picker offers but classic (main's
/// byte-identical v0.28 restoration, ruled), on dark, GitHub Light and Default Light, through
/// typing, a burst, an Enter at the bottom row (the echo and the printed line every frame after
/// it) and wide letters, keeps every letter it lies on at 4.5:1 or better.
#[test]
fn every_letter_a_trail_lies_on_reads_at_the_bar() {
    let mut jobs = Vec::new();
    for style in STYLES.into_iter().filter(|s| *s != "classic") {
        for look in LOOKS {
            for scene in SCENES {
                jobs.push((style, look, scene));
            }
        }
    }
    let verdicts = replay_all(&jobs, true, |_, _, _| Vec::new());
    assert_eq!(verdicts.len(), jobs.len());
    let failed: Vec<String> = verdicts
        .iter()
        .filter(|v| v.under > 0)
        .map(ToString::to_string)
        .collect();
    assert!(
        failed.is_empty(),
        "letters under {BAR}:1:\n{}",
        failed.join("\n")
    );
    // Not vacuous: the scenes put the trails on letters and the judge read them.
    assert!(verdicts.iter().all(|v| v.worst.is_some()));
}

/// The negative control: the same replays with the letter guard off bury letters, each one a
/// defect the trail gallery photographed: the ribbon's Enter flare across the
/// printed line and the echo, Laser's Enter bolt across the printed line, the caret bloom of
/// Beam, Phaser, Comet, Lumen and Sparkle on the newest letters, Water's wave through the letters
/// on dark and in the burst on paper, the ribbon head's bloom in the burst, and Fire's spark on a
/// wide letter on both papers.
#[test]
fn without_the_letter_guard_the_trails_bury_letters() {
    let jobs = [
        ("rainbow kitty pet", DARK, Scene::EnterBottom),
        ("flat rainbow", DEFAULT_LIGHT, Scene::EnterBottom),
        ("laser", DARK, Scene::EnterBottom),
        ("beam", DARK, Scene::Typing),
        ("phaser", DARK, Scene::Burst),
        ("comet", DARK, Scene::Typing),
        ("lumen", DARK, Scene::Typing),
        ("sparkle", DARK, Scene::Typing),
        ("water", DARK, Scene::Typing),
        ("water", DEFAULT_LIGHT, Scene::Burst),
        ("nyan rainbow", DARK, Scene::Burst),
        ("fire", GITHUB_LIGHT, Scene::Cjk),
        ("fire", DEFAULT_LIGHT, Scene::Cjk),
    ];
    let off = replay_all(&jobs, false, |_, _, _| Vec::new());
    let held: Vec<String> = off
        .iter()
        .filter(|v| v.under == 0)
        .map(ToString::to_string)
        .collect();
    assert!(
        held.is_empty(),
        "the guard is off, yet these read at the bar (the control no longer controls):\n{}",
        held.join("\n")
    );
}

const RIBBONS: [&str; 5] = [
    "rainbow kitty pet",
    "rainbow kitty underline",
    "rainbow dog",
    "flat rainbow",
    "nyan rainbow",
];

/// THE LAW ON DIM TEXT: text that clears the bar with little room, a 5.0:1 grey on the dark
/// page and Solarized Dark's 4.75:1, reads its solid ink under the guard's floor on the bare
/// page, so a guard that gives up on it leaves every light whole (a 5.0:1 grey under Sparkle's
/// caret bloom read 1.01:1, Solarized under Laser's Enter bolt 1.34:1). Every style but
/// classic, through every scene, keeps each letter it lies on at four fifths of the theme's
/// own contrast ([`look_bar`]); the ribbons through the Enter (their flare, which the guard
/// holds), their bodies under the typed line being the ribbon's own hold's (0.7 of the theme's
/// contrast on these themes, not the guard's). Negative control: the guard off buries a letter
/// under that bar in each of the four control replays.
#[test]
fn every_letter_on_a_dim_theme_keeps_its_own_reading() {
    let mut jobs = Vec::new();
    for look in [DIM_GREY, SOLARIZED_DARK] {
        for style in STYLES.into_iter().filter(|s| *s != "classic") {
            for scene in SCENES {
                if RIBBONS.contains(&style) && scene != Scene::EnterBottom {
                    continue;
                }
                jobs.push((style, look, scene));
            }
        }
    }
    let verdicts = replay_all(&jobs, true, |_, _, _| Vec::new());
    assert_eq!(verdicts.len(), jobs.len());
    let failed: Vec<String> = verdicts
        .iter()
        .filter(|v| v.under > 0)
        .map(ToString::to_string)
        .collect();
    assert!(
        failed.is_empty(),
        "letters under four fifths of their theme's contrast:\n{}",
        failed.join("\n")
    );
    assert!(verdicts.iter().all(|v| v.worst.is_some()));
    let controls = [
        ("sparkle", DIM_GREY, Scene::Typing),
        ("rainbow kitty pet", DIM_GREY, Scene::EnterBottom),
        ("laser", SOLARIZED_DARK, Scene::EnterBottom),
        ("water", SOLARIZED_DARK, Scene::Typing),
    ];
    let off = replay_all(&controls, false, |_, _, _| Vec::new());
    let held: Vec<String> = off
        .iter()
        .filter(|v| v.under == 0)
        .map(ToString::to_string)
        .collect();
    assert!(
        held.is_empty(),
        "the guard is off, yet these keep their reading (the control no longer controls):\n{}",
        held.join("\n")
    );
}

#[test]
#[ignore = "survey: prints every style's worst letter (TRAIL_SURVEY_STYLES, TRAIL_LOOKS=dark|light|dim, \
            TRAIL_GUARD=0, TRAIL_DUMP=<dir>, TRAIL_DUMP_MS=<ms,...>)"]
fn survey_every_style_s_worst_letter() {
    let want: Option<Vec<String>> = std::env::var("TRAIL_SURVEY_STYLES")
        .ok()
        .map(|s| s.split(',').map(str::to_string).collect());
    let looks: Vec<Look> = match std::env::var("TRAIL_LOOKS").ok().as_deref() {
        Some("dark") => vec![DARK],
        Some("light") => vec![GITHUB_LIGHT, DEFAULT_LIGHT],
        Some("dim") => DIM_LOOKS.to_vec(),
        _ => LOOKS.to_vec(),
    };
    let mut jobs = Vec::new();
    for style in STYLES {
        if want.as_ref().is_some_and(|w| !w.iter().any(|x| x == style)) {
            continue;
        }
        for &look in &looks {
            for scene in SCENES {
                jobs.push((style, look, scene));
            }
        }
    }
    let dir = std::env::var("TRAIL_DUMP").ok();
    let dump_ms: Option<Vec<u64>> = std::env::var("TRAIL_DUMP_MS")
        .ok()
        .map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect());
    let guard = std::env::var("TRAIL_GUARD").map_or(true, |v| v != "0");
    let verdicts = replay_all(&jobs, guard, |style, look, scene| {
        let Some(dir) = &dir else {
            return Vec::new();
        };
        let all: Vec<u64> = (0..10_000).step_by(16).collect();
        dump_ms
            .as_ref()
            .unwrap_or(&all)
            .iter()
            .map(|&f| {
                (
                    f,
                    format!(
                        "{dir}/{}-{}-{scene:?}-{f:05}.png",
                        style.replace(' ', "-"),
                        look.name
                    ),
                )
            })
            .collect()
    });
    for v in verdicts {
        println!("{v}");
    }
}
