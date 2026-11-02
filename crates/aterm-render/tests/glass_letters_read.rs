// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The theme fg's letters over Prism Glass's light glass as the trail census reads them
// (`docs/design/PRISM-GLASS.md` §15.8 amendment 49): every printable ASCII glyph of the window's
// face at the window's size (`Renderer::from_system`'s: SF Mono on macOS, the bundled JetBrains
// Mono elsewhere, at 24 px, a 12 pt font at scale 2, its cells 28 px tall for SF Mono), set in each
// shipped glass page's theme fg with each of its glass129 entries as the ground and with the page
// as its clean twin, judged as `aterm-lab trail-census` judges a frame's glyph minimum: each solid
// ink pixel of the twin (its contrast at least `SOLID_SHARE` of its glyph's extreme's against the
// page) read in the frame against the median of its nearest ring of ground. The glass is solved to
// the glyph as it is seen (`prism_glass::colour::seen`: the fg's dimmest solid ink against the
// glass, the window's thinnest glyph's darkest pixel `colour::THIN_INK_SHARE` of the way to the
// page), so every entry keeps the census's bar, and the solver's reading of an entry is never more
// than a rounding over what the census measures: the margin the rim takes off the bare ratio, per
// entry, prints with `--nocapture`. Negative control: a ground that keeps the bare fg at the old
// law's floor (the bar plus the solve's guard, 5.45 on a 5.25 bar) reads under the bar as the
// census reads it, on every page.

use crate::wcag::contrast;
use aterm_core::terminal::{Rgb, Terminal};
use aterm_effects::rainbow_kitty::prism_glass::colour::{BLANK_BELOW, RING_REACH};
use aterm_effects::rainbow_kitty::prism_glass::{
    GLASS_BAR, Mode, SOLVE_GUARD, colour, glass_bar, tables,
};
use aterm_render::{Renderer, Theme};

mod live;

/// The window's font size in device pixels: the default 12 pt at scale 2.
pub const WINDOW_PX: f32 = 24.0;
/// The glyphs judged: printable ASCII.
pub const GLYPHS: &str = concat!(
    "!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    "[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~"
);
/// What the solver's reading of an entry may sit over the census's measure of it: the 8-bit
/// rounding of a rim pixel.
pub const ROUNDING: f64 = 0.02;

/// Each channel's median, the upper of an even count.
fn median(px: &[u32]) -> u32 {
    let mut out = 0;
    for sh in [16, 8, 0] {
        let mut ch: Vec<u32> = px.iter().map(|p| (p >> sh) & 0xff).collect();
        ch.sort_unstable();
        out |= ch[ch.len() / 2] << sh;
    }
    out
}

/// The commonest colour, the lowest of a tie.
fn mode(px: &[u32]) -> u32 {
    let mut seen: Vec<(u32, usize)> = Vec::new();
    for &p in px {
        match seen.iter_mut().find(|(c, _)| *c == p) {
            Some((_, n)) => *n += 1,
            None => seen.push((p, 1)),
        }
    }
    seen.into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map_or(0, |(c, _)| c)
}

/// `0x00RRGGBB` as the terminal's colour.
const fn rgb(c: u32) -> Rgb {
    Rgb {
        r: (c >> 16) as u8,
        g: (c >> 8) as u8,
        b: c as u8,
    }
}

/// A sheet of grounds: for each, a blank row, a row of [`GLYPHS`] with a blank cell each side,
/// and a blank row, all on that ground, so every ring of a glyph's pixel lies on its own ground;
/// and its clean twin, the same text on the page.
pub struct Sheet {
    pub frame: Vec<u32>,
    pub clean: Vec<u32>,
    pub w: usize,
    pub cw: usize,
    pub ch: usize,
}

/// The text row of ground `k`.
pub const fn text_row(k: usize) -> usize {
    3 * k + 1
}

impl Sheet {
    /// `fg` set on page `bg` over each of `grounds`, at `px` in the window's face
    /// ([`Renderer::from_system`]: SF Mono on macOS, the bundled JetBrains Mono elsewhere).
    pub fn new(px: f32, fg: u32, bg: u32, grounds: &[u32]) -> Self {
        let mut rend = Renderer::from_system(px, Theme::default())
            .expect("no font: build with aterm-render's default embedded-font feature");
        Self::drawn_by(&mut rend, fg, bg, grounds)
    }

    /// [`Self::new`] in `rend`'s face, at its size and line height.
    pub fn drawn_by(rend: &mut Renderer, fg: u32, bg: u32, grounds: &[u32]) -> Self {
        let (cw, ch) = rend.cell_size();
        let rows = 3 * grounds.len();
        let cols = GLYPHS.len() + 2;
        let mut paint = |lit: bool| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.set_default_foreground(rgb(fg));
            term.set_default_background(rgb(bg));
            let mut bytes = Vec::new();
            for (k, &g) in grounds.iter().enumerate() {
                for r in 0..3 {
                    bytes.extend(format!("\x1b[{};1H", 3 * k + r + 1).bytes());
                    if lit {
                        bytes.extend(
                            format!("\x1b[48;2;{};{};{}m", g >> 16, (g >> 8) & 0xff, g & 0xff)
                                .bytes(),
                        );
                    }
                    let line = if r == 1 {
                        format!(" {GLYPHS} ")
                    } else {
                        " ".repeat(cols)
                    };
                    bytes.extend(line.bytes());
                    bytes.extend(b"\x1b[0m");
                }
            }
            term.process(&bytes);
            let frame = rend.render_input(&term.cell_frame(rows, cols));
            assert_eq!(frame.width, cols * cw, "the frame is the grid, no pad");
            frame
                .pixels
                .iter()
                .map(|p| p & 0x00FF_FFFF)
                .collect::<Vec<u32>>()
        };
        let clean = paint(false);
        let frame = paint(true);
        Self {
            frame,
            clean,
            w: cols * cw,
            cw,
            ch,
        }
    }

    fn at(px: &[u32], w: usize, x: usize, y: usize) -> u32 {
        px[y * w + x]
    }

    /// The glyphs of ground `k`'s row that hold ink: each one's character, its cell's bounds and
    /// its twin's ground (the commonest colour) and extreme (the colour farthest from it).
    fn glyphs_for(&self, k: usize, text: &str) -> Vec<Glyph> {
        let row = text_row(k);
        let mut out = Vec::new();
        for (i, ch) in text.chars().enumerate() {
            let col = i + 1;
            let (x0, y0) = (col * self.cw, row * self.ch);
            let (x1, y1) = (x0 + self.cw, y0 + self.ch);
            let clean_px: Vec<u32> = (y0..y1)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .map(|(x, y)| Self::at(&self.clean, self.w, x, y))
                .collect();
            let ground = mode(&clean_px);
            let Some(extreme) = clean_px
                .iter()
                .copied()
                .max_by(|a, b| contrast(*a, ground).total_cmp(&contrast(*b, ground)))
            else {
                continue;
            };
            if contrast(extreme, ground) >= BLANK_BELOW {
                out.push(Glyph {
                    ch,
                    x: x0..x1,
                    y: y0..y1,
                    ground,
                    extreme,
                });
            }
        }
        out
    }

    /// The median of `px`'s nearest ring, 1 then [`RING_REACH`] px out, of pixels that are
    /// `ground` in the twin; `None` when no ring that near holds any.
    fn ring_ground(&self, px: &[u32], (x, y): (usize, usize), ground: u32) -> Option<u32> {
        let h = self.clean.len() / self.w;
        let mut ring = Vec::new();
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
                    if Self::at(&self.clean, self.w, xx, yy) == ground {
                        ring.push(Self::at(px, self.w, xx, yy));
                    }
                }
            }
            if !ring.is_empty() {
                return Some(median(&ring));
            }
        }
        None
    }

    /// The census's glyph minimum over the glyphs of ground `k`'s row: each solid ink pixel of
    /// the twin against the median of the frame's nearest ring of twin ground, and the glyph it
    /// lies in. `None` when no glyph holds solid ink with ground that near.
    pub fn census_min(&self, k: usize) -> Option<(f64, char)> {
        let mut worst: Option<(f64, char)> = None;
        for (glyph, reading, _) in self.glyph_readings(k) {
            if worst.is_none_or(|w| reading < w.0) {
                worst = Some((reading, glyph));
            }
        }
        worst
    }

    /// The same census reduction paired with each glyph's own clean-page reading.
    fn glyph_readings(&self, k: usize) -> Vec<(char, f64, f64)> {
        self.glyph_readings_for(k, GLYPHS)
            .into_iter()
            .map(|(_, ch, reading, own)| (ch, reading, own))
            .collect()
    }

    /// The same unchanged census reducer with the actual text's column attached.
    fn glyph_readings_for(&self, k: usize, text: &str) -> Vec<(usize, char, f64, f64)> {
        let mut readings = Vec::new();
        for g in self.glyphs_for(k, text) {
            let solid = colour::SOLID_SHARE * contrast(g.extreme, g.ground);
            let mut worst: Option<f64> = None;
            for y in g.y.clone() {
                for x in g.x.clone() {
                    if contrast(Self::at(&self.clean, self.w, x, y), g.ground) < solid {
                        continue;
                    }
                    if let Some(ring) = self.ring_ground(&self.frame, (x, y), g.ground) {
                        let r = contrast(Self::at(&self.frame, self.w, x, y), ring);
                        if worst.is_none_or(|w| r < w) {
                            worst = Some(r);
                        }
                    }
                }
            }
            if let Some(reading) = worst {
                readings.push((
                    g.x.start / self.cw,
                    g.ch,
                    reading,
                    contrast(g.extreme, g.ground),
                ));
            }
        }
        readings
    }
}

/// One glyph of a sheet row that holds ink.
struct Glyph {
    ch: char,
    x: core::ops::Range<usize>,
    y: core::ops::Range<usize>,
    ground: u32,
    extreme: u32,
}

/// `c` darkened in linear light until the bare `fg` keeps `target` on it, no further: a ground
/// on the old law's floor.
fn bare_floor(fg: u32, c: u32, target: f64) -> u32 {
    let lin = |v: u32| {
        let s = f64::from(v) / 255.0;
        if s <= 0.040_45 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    let enc = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0 + 0.5) as u32
    };
    let at = |k: f64| {
        let ch = |sh: u32| enc(lin((c >> sh) & 0xff) * k);
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    };
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if colour::contrast(fg, at(mid)) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    at(hi)
}

/// The shipped glass pages.
pub fn glass_pages()
-> impl Iterator<Item = &'static aterm_effects::rainbow_kitty::prism_glass::Palette> {
    tables::SHIPPED.iter().filter(|p| p.mode == Mode::Glass)
}

/// **THE GLASS KEEPS EVERY LETTER ITS BAR AS THE CENSUS READS IT**: on every shipped glass page,
/// over every glass129 entry, every printable ASCII glyph of the window's face at the window's
/// cell keeps [`GLASS_BAR`] by the census's glyph minimum, and the solver's seen reading of the
/// entry lies at most [`ROUNDING`] over it. The per-entry figures (the bare fg's ratio, the
/// solver's seen reading and the census's minimum, whose difference from the bare ratio is the
/// rim's margin) print with `--nocapture`.
#[test]
fn the_glass_keeps_every_letter_its_bar_as_the_census_reads_it() {
    let mut pages = 0;
    for p in glass_pages() {
        pages += 1;
        let sheet = Sheet::new(WINDOW_PX, p.fg, p.bg, &p.glass);
        let (mut least, mut margin) = ((f64::INFINITY, 0usize, ' '), (f64::INFINITY, 0.0_f64));
        for (k, &g) in p.glass.iter().enumerate() {
            let (measured, glyph) = sheet
                .census_min(k)
                .unwrap_or_else(|| panic!("#{:06x}: entry {k} judged no glyph", p.bg));
            let (bare, seen) = (colour::contrast(p.fg, g), colour::seen(p.fg, p.bg, g));
            println!(
                "#{:06x} on #{:06x} entry {k:3} #{g:06x}: bare {bare:.4} seen {seen:.4} census \
                 {measured:.4} ('{glyph}') rim margin {:.4}",
                p.fg,
                p.bg,
                bare - measured
            );
            assert!(
                measured >= GLASS_BAR,
                "#{:06x} on #{:06x}: entry {k} #{g:06x} holds '{glyph}' at {measured:.4} by the \
                 census, under {GLASS_BAR}",
                p.fg,
                p.bg
            );
            assert!(
                seen <= measured + ROUNDING,
                "#{:06x}: entry {k} #{g:06x} reads {seen:.4} to the solver, {measured:.4} to the \
                 census",
                p.bg
            );
            if measured < least.0 {
                least = (measured, k, glyph);
            }
            margin.0 = margin.0.min(bare - measured);
            margin.1 = margin.1.max(bare - measured);
        }
        println!(
            "#{:06x} on #{:06x}: census min {:.4} at entry {} ('{}'), rim margin {:.4}..{:.4}",
            p.fg, p.bg, least.0, least.1, least.2, margin.0, margin.1
        );
    }
    assert_eq!(pages, 5, "the five shipped glass pages");
}

/// The negative control: on every shipped glass page, its darkest glass entry darkened to the old
/// law's floor (the bare fg at the bar plus [`SOLVE_GUARD`]) holds a glyph under [`GLASS_BAR`]
/// by the census: a glass solved to the bare fg does not keep the letters as they are seen.
#[test]
fn a_glass_solved_to_the_bare_fg_holds_a_letter_under_its_bar() {
    for p in glass_pages() {
        let darkest = p
            .glass
            .iter()
            .copied()
            .min_by(|a, b| colour::luminance(*a).total_cmp(&colour::luminance(*b)))
            .expect("a glass table");
        let floor = bare_floor(p.fg, darkest, glass_bar(p.fg, p.bg) + SOLVE_GUARD);
        let sheet = Sheet::new(WINDOW_PX, p.fg, p.bg, &[floor]);
        let (measured, glyph) = sheet.census_min(0).expect("the row judges its glyphs");
        assert!(
            measured < GLASS_BAR,
            "#{:06x} on #{:06x}: #{floor:06x} ({:.4} bare) holds '{glyph}' at {measured:.4}",
            p.fg,
            p.bg,
            colour::contrast(p.fg, floor)
        );
    }
}

/// The window's face, loaded by the path [`Renderer::from_system`] takes it from: SF Mono on
/// macOS, the bundled JetBrains Mono elsewhere.
fn window_face() -> (&'static str, Vec<u8>) {
    #[cfg(target_os = "macos")]
    let path = "/System/Library/Fonts/SFNSMono.ttf";
    #[cfg(not(target_os = "macos"))]
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/bundled/JetBrainsMono-Regular.ttf"
    );
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    (path, bytes)
}

/// How many of a page's glass entries the size sweep judges: those whose seen reading
/// ([`colour::seen`]) is the page's least, where the census's glyph minimum lies nearest the bar.
const SWEPT_ENTRIES: usize = 8;

/// One face at one size and line height on each shipped glass page: the face's thin share in the
/// page's fg on its page ([`aterm_render::GlyphInk::thin_share`]), the census's glyph minimum over
/// the page's [`SWEPT_ENTRIES`] least-seen glass entries, and its glyph.
fn swept(bytes: &[u8], px: f32, line_height: f32) -> Vec<(f32, f64, char)> {
    let pages: Vec<_> = glass_pages().collect();
    std::thread::scope(|s| {
        let runs: Vec<_> = pages
            .iter()
            .map(|p| {
                s.spawn(move || {
                    let mut rend = Renderer::from_bytes(bytes, px, Theme::default())
                        .expect("the window's face");
                    rend.set_line_height(line_height);
                    let share = rend.glyph_ink().thin_share(
                        aterm_render::linear_luminance(p.fg),
                        aterm_render::linear_luminance(p.bg),
                    );
                    let mut least: Vec<u32> = p.glass.to_vec();
                    least.sort_by(|a, b| {
                        colour::seen(p.fg, p.bg, *a).total_cmp(&colour::seen(p.fg, p.bg, *b))
                    });
                    least.truncate(SWEPT_ENTRIES);
                    let sheet = Sheet::drawn_by(&mut rend, p.fg, p.bg, &least);
                    let (min, glyph) = (0..least.len())
                        .filter_map(|k| sheet.census_min(k))
                        .fold((f64::INFINITY, ' '), |w, m| if m.0 < w.0 { m } else { w });
                    (share, min, glyph)
                })
            })
            .collect();
        runs.into_iter()
            .map(|run| run.join().expect("a page's sheet"))
            .collect()
    })
}

/// **THE GLASS DRAWS WHERE THE WINDOW'S FACE LAYS ITS LETTERS' FULL INK** (§15.8 amendment 53,
/// `host::PageFacts::glass`): the window's face loaded by path ([`window_face`]) at every whole
/// px from 10 to 48 and at line heights 1.0, 1.1 and 1.2, on every shipped glass page. Where its
/// thin share in the page's fg on its page is at most [`colour::THIN_INK_SHARE`], the page's glass
/// draws, and the census's glyph minimum over its [`SWEPT_ENTRIES`] least-seen entries keeps
/// [`GLASS_BAR`]; a 12 pt window at scale 1 (12 px) and the Settings card at scale 1 (14 px) are
/// refused, and a 12 pt window at scale 2 (24 px) admitted on every page. The face loaded by path
/// is the one `Renderer::from_system` draws (their glyph ink is one). Negative control: a size the
/// rule refuses holds a glyph under [`GLASS_BAR`] on a page, though its cells are at least 24 px
/// tall, which a rule keyed on the cells' height admitted (amendment 50's; SF Mono at 26 px,
/// whose `_` lays no row near the full ink). Each size's shares and minima print with
/// `--nocapture`.
#[test]
fn the_glass_draws_where_the_windows_face_lays_its_letters_full() {
    let (path, bytes) = window_face();
    let system = Renderer::from_system(WINDOW_PX, Theme::default())
        .expect("the window's face")
        .glyph_ink()
        .fingerprint();
    let by_path = Renderer::from_bytes(&bytes, WINDOW_PX, Theme::default())
        .expect("the window's face")
        .glyph_ink()
        .fingerprint();
    assert_eq!(by_path, system, "{path} is the face from_system draws");
    let share = colour::THIN_INK_SHARE as f32;
    let mut refused_under = Vec::new();
    for px in 10..=48_u8 {
        for line_height in [1.0_f32, 1.1, 1.2] {
            let pages = swept(&bytes, f32::from(px), line_height);
            let mut rend = Renderer::from_bytes(&bytes, f32::from(px), Theme::default())
                .expect("the window's face");
            rend.set_line_height(line_height);
            let cells = rend.cell_size().1;
            for (k, &(thin, min, glyph)) in pages.iter().enumerate() {
                let admitted = thin <= share;
                println!(
                    "{path} {px} px x{line_height} ({cells} px cells) page {k}: thin share \
                     {thin:.4} {} census {min:.4} ('{glyph}')",
                    if admitted { "glass" } else { "paper" }
                );
                if admitted {
                    assert!(
                        min >= GLASS_BAR,
                        "{px} px x{line_height}: page {k} admitted at {thin:.4} holds '{glyph}' \
                         at {min:.4}"
                    );
                } else if min < GLASS_BAR && cells >= 24 {
                    refused_under.push((px, line_height, k, thin, min, glyph));
                }
                if line_height == 1.0 {
                    match px {
                        12 | 14 => assert!(!admitted, "{px} px: page {k} at {thin:.4}"),
                        24 => assert!(admitted, "24 px: page {k} at {thin:.4}"),
                        _ => {}
                    }
                }
            }
        }
    }
    println!("refused, under the bar, on cells of 24 px or more: {refused_under:?}");
    assert!(
        !refused_under.is_empty(),
        "negative control: no size the rule refuses on cells of 24 px or more loses a letter"
    );
}
