// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cursor_glow::{Geom, Light, Live, SoundCue};

/// THE PAPER TRAIN'S GOLD READS GOLD (looks9 Q5): the gallery's
/// `enter-bottom` Return on paper laid its gold as an ochre (`#a27b13`
/// to `#b37113`, the caret's 4.5:1 darkening it to a brown) and began
/// its green at a darkened lime (`#329213`). Over every hue of the
/// spectrum, on white and on a grey paper, each warm ink the paper train
/// lays (bent hue 25 to 55 degrees) keeps three fifths of its value or
/// more and a graphic's 3:1 against the page, and it lays no hue between
/// 50 and 112 degrees (no olive: looks8 K10b's `#8b8013`): gold, then a
/// clean green. Negative control: the same bend held to the caret's bar
/// darkens its golds to ochre.
#[test]
fn the_paper_train_lays_gold_then_a_clean_green() {
    let lay = |caret_bar: bool| -> (usize, usize, f32) {
        let (mut ochre, mut lime, mut least) = (0usize, 0usize, f32::MAX);
        for page in [0x00FF_FFFFu32, 0x00F2_F2F2] {
            for k in 0..1440u32 {
                let rgb = hsv2rgb(k as f32 * 0.25, 1.0, 1.0);
                let ink = if caret_bar {
                    paper_vivid_ink_bent(rgb, page, PAPER_CARET_CONTRAST, &PAPER_TRAIN_HUE_BEND)
                } else {
                    paper_train_ink(rgb, page)
                };
                let (h, s, v) = rgb2hsv(ink);
                if s <= 0.3 {
                    continue;
                }
                if (PAPER_TRAIN_GOLD_HUES.0..=PAPER_TRAIN_GOLD_HUES.1).contains(&h) {
                    ochre += usize::from(v < 0.66);
                    let (yi, yp) = (relative_luminance(ink), relative_luminance(page));
                    least = least.min((yi.max(yp) + 0.05) / (yi.min(yp) + 0.05));
                }
                lime += usize::from((50.0..112.0).contains(&h));
            }
        }
        (ochre, lime, least)
    };
    let (ochre, lime, least) = lay(false);
    assert_eq!(
        ochre, 0,
        "{ochre} of the paper train's warm inks read ochre"
    );
    assert_eq!(lime, 0, "the paper train lays {lime} olive or lime inks");
    assert!(
        least >= PAPER_TRAIN_GOLD_CONTRAST - 0.05,
        "a warm ink of the paper train holds only {least:.2}:1"
    );
    let (dark, ..) = lay(true);
    assert!(
        dark >= 8,
        "control: at the caret's bar only {dark} golds read ochre"
    );
}

#[test]
fn the_dark_bed_keeps_its_stops_chroma() {
    // Under `#d0d0d0` text the dark bed is held to some 0.07 of
    // luminance. Every named stop keeps a vivid colour there (OKLab
    // chroma), the walk's warm half (red to green) draws little of its
    // length in the olive and brown basin, and the letters' budget
    // holds. (The green-to-blue crossing's teal is the same physics but
    // reads as no mud, so the bend leaves hues past 90° as they are.)
    let budget = bed_luma_budget(0x00D0_D0D0);
    let chroma = |rgb: u32| {
        let [_, a, b] = crate::color_math::oklab(rgb);
        a.hypot(b)
    };
    let warm = (0..BED_INK_LUT_LEN)
        .take_while(|&i| {
            let (h, ..) = rgb2hsv(walk_arc_at(i));
            h <= 120.0
        })
        .count();
    let muddy = |ink: fn(u32, f32) -> u32| {
        (0..warm)
            .filter(|&i| chroma(ink(walk_arc_at(i), budget)) < 0.105)
            .count()
    };
    for (i, &stop) in crate::spectrum::SPECTRUM_ANCHORS.iter().enumerate() {
        let ink = bed_ink(stop, budget);
        assert!(
            chroma(ink) >= 0.11,
            "stop {i} holds to #{ink:06x}, chroma {:.3}",
            chroma(ink)
        );
        assert!(
            relative_luminance(ink) <= budget + 0.002,
            "stop {i} #{ink:06x} passes the letters' budget"
        );
    }
    let (bent, unbent) = (muddy(bed_ink), muddy(bed_ink_unbent));
    assert!(warm > 40, "fixture: the warm half spans {warm} stations");
    assert!(
        5 * bent <= warm,
        "{bent} of the warm half's {warm} stations hold to mud"
    );
    assert!(
        3 * unbent > warm,
        "control: unbent, {unbent} of {warm} warm stations hold to mud"
    );
    let yellow = bed_ink_unbent(0x00FF_FF00, budget);
    assert!(
        chroma(yellow) < 0.1,
        "control: unbent, yellow holds to #{yellow:06x}, chroma {:.3}",
        chroma(yellow)
    );
}
use aterm_render::{GlowQuad, RainHalo, over_premul, premul_rgb};
use std::time::Duration;

use super::super::{CaretSeam, Dir, TypedClass};

const DEFAULT_BG: u32 = 0x001A_1B26;
const DEFAULT_FG: u32 = 0x00C8_D3F5;
const NORD_BG: u32 = 0x002E_3440;
const NORD_FG: u32 = 0x00D8_DEE9;

const SHIPPED_BG: u32 = 0x0011_1318;
const SHIPPED_FG: u32 = 0x00D0_D0D0;

const fn geom() -> Geom {
    Geom {
        cw: 9,
        ch: 18,
        rows: 40,
        cols: 120,
        origin_x: 0,
        origin_y: 0,
        win_w: 1080,
        win_h: 720,
        head: 0,
    }
}

const fn geom2x() -> Geom {
    Geom {
        cw: 18,
        ch: 36,
        rows: 40,
        cols: 120,
        origin_x: 0,
        origin_y: 0,
        win_w: 2160,
        win_h: 1440,
        head: 0,
    }
}

const fn cfg(dark: bool, tall: bool) -> Config {
    Config {
        dark_theme: dark,
        intensity: 1.0,
        duration: Duration::from_millis(400),
        ribbon_tall: tall,
        ribbon_flat: false,
        theme_fg: if dark { DEFAULT_FG } else { 0x0016_161C },
        theme_bg: if dark { DEFAULT_BG } else { 0x00FF_FFFF },
        reduced_motion: false,
        light: crate::cursor_glow::Light::OFF,
        head_dx: 0.5,
    }
}

const fn flat(dark: bool, tall: bool) -> Config {
    let mut c = cfg(dark, tall);
    c.ribbon_flat = true;
    c
}

const fn ctx_in<'a>(
    now: Instant,
    cfg: &'a Config,
    caret: (u16, u16),
    disp: f32,
    g: Geom,
) -> Ctx<'a> {
    Ctx {
        now,
        geom: g,
        cfg,
        disp,
        birth_disp: disp,
        phase: 0.0,
        caret,
        caret_t: 0.0,
        caret_walk: None,
        mend: None,
        surge: 0.0,
        flow: crate::rainbow_kitty::spine::Flow::EMPTY,
    }
}

const fn ctx<'a>(now: Instant, cfg: &'a Config, caret: (u16, u16), disp: f32) -> Ctx<'a> {
    ctx_in(now, cfg, caret, disp, geom())
}

#[derive(Default)]
struct Sink {
    under: Vec<GlowQuad>,
    out: Vec<GlowQuad>,
    under_ramps: aterm_render::GlowRamps,
    out_ramps: aterm_render::GlowRamps,
    halos: Vec<RainHalo>,
    beams: Vec<BeamVertex>,
    cues: Vec<SoundCue>,
}

impl Sink {
    fn frame(&mut self) -> Frame<'_> {
        self.under.clear();
        self.out.clear();
        self.halos.clear();
        self.cues.clear();
        Frame {
            under: &mut self.under,
            out: &mut self.out,
            under_ramps: &mut self.under_ramps,
            out_ramps: &mut self.out_ramps,
            halos: &mut self.halos,
            beams: &mut self.beams,
            cues: &mut self.cues,
            caret: CaretSeam::default(),
            companion: None,
            fp: 0,
            rows_cost: Some(aterm_render::RampCost::NEVER),
        }
    }
}

fn at(t0: Instant, ms: u64) -> Instant {
    t0.checked_add(Duration::from_millis(ms)).expect("clock")
}

const fn typed() -> Event {
    Event::Typed {
        cells: 1,
        shifted: false,
        class: TypedClass::Glyph,
    }
}

const fn nav(from: (u16, u16), to: (u16, u16)) -> Event {
    Event::Move {
        from,
        to,
        licence: Licence::Nav,
        dir: Dir::Right,
    }
}

const fn pty(from: (u16, u16), to: (u16, u16)) -> Event {
    Event::Move {
        from,
        to,
        licence: Licence::Pty,
        dir: Dir::Right,
    }
}

#[derive(Clone, Copy)]
struct Keys {
    g: Geom,
    row: u16,
    col0: u16,
    n: u16,
    period_ms: u64,
    disp: f32,
}

fn type_keys(rib: &mut Ribbon, t0: Instant, keys: Keys, c: &Config) {
    for i in 0..keys.n {
        let now = at(t0, u64::from(i) * keys.period_ms);
        let caret = (keys.row, keys.col0 + i + 1);
        let cx = ctx_in(now, c, caret, keys.disp, keys.g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
    }
}

fn type_run(rib: &mut Ribbon, t0: Instant, col0: u16, n: u16, c: &Config, disp: f32) {
    let keys = Keys {
        g: geom(),
        row: 2,
        col0,
        n,
        period_ms: 60,
        disp,
    };
    type_keys(rib, t0, keys, c);
}

fn erase_at(rib: &mut Ribbon, now: Instant, caret: (u16, u16), c: &Config) {
    let cx = ctx(now, c, caret, 0.7);
    rib.on_event(&Event::Erase, now, &cx);
    rib.plan(&cx);
}

fn cell_lit(under: &[GlowQuad], g: Geom, row: u16, col: u16) -> bool {
    let x0 = f32::from(g.origin_x) + f32::from(col) * g.cw as f32;
    let x1 = x0 + g.cw as f32;
    let y0 = f32::from(g.origin_y) + f32::from(row) * g.ch as f32;
    let y1 = y0 + g.ch as f32 * 0.5;
    under.iter().any(|q| {
        let qx0 = f32::from(q.x);
        let qx1 = qx0 + f32::from(q.w);
        let qy = f32::from(q.y);
        qx0 < x1 && qx1 > x0 && (y0..y1).contains(&qy)
    })
}

fn column_census(
    quads: &[GlowQuad],
    xs: std::ops::Range<i32>,
    ys: std::ops::Range<i32>,
    floor: u8,
) -> Vec<Option<(i32, i32, u8)>> {
    let level = |q: &GlowQuad, x: i32| -> u8 {
        let i = (x - i32::from(q.x)) as u32;
        let w = u32::from(q.w);
        if q.alpha > 0 {
            aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, w) as u8
        } else {
            let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, w);
            ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff) as u8
        }
    };
    xs.map(|x| {
        let mut top = i32::MAX;
        let mut bot = i32::MIN;
        let mut peak = 0u8;
        for q in quads {
            let qx0 = i32::from(q.x);
            if q.w == 0 || x < qx0 || x >= qx0 + i32::from(q.w) {
                continue;
            }
            let lv = level(q, x);
            if lv < floor {
                continue;
            }
            let y = i32::from(q.y);
            if !ys.contains(&y) {
                continue;
            }
            top = top.min(y);
            bot = bot.max(y + i32::from(q.h) - 1);
            peak = peak.max(lv);
        }
        (top != i32::MAX).then_some((top, bot, peak))
    })
    .collect()
}

fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn peak_channel(quads: &[GlowQuad]) -> u32 {
    quads
        .iter()
        .map(|q| {
            ((q.color >> 16) & 0xff)
                .max((q.color >> 8) & 0xff)
                .max(q.color & 0xff)
        })
        .max()
        .unwrap_or(0)
}

fn plan_peak(rib: &Ribbon) -> u8 {
    rib.plan_segments().iter().map(|s| s.cov).max().unwrap_or(0)
}

fn plan_sum(rib: &Ribbon) -> u32 {
    rib.plan_segments().iter().map(|s| u32::from(s.cov)).sum()
}

fn band_sum(rib: &Ribbon, col: u16) -> u32 {
    let x = f32::from(col) * geom().cw as f32;
    rib.plan_segments()
        .iter()
        .filter(|s| s.x <= x + 0.5)
        .map(|s| u32::from(s.cov))
        .sum()
}

#[test]
fn letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor() {
    let mut fgs: Vec<u32> = (0x90u32..=0xFF)
        .step_by(3)
        .map(|g| (g << 16) | (g << 8) | g)
        .collect();
    for r in [0xA0u32, 0xD0, 0xFF] {
        for g in [0xA0u32, 0xD0, 0xFF] {
            for b in [0xA0u32, 0xD0, 0xFF] {
                fgs.push((r << 16) | (g << 8) | b);
            }
        }
    }
    fgs.extend([
        DEFAULT_FG,
        0x00E8_E8F0,
        0x00F8_F8F2,
        0x00AB_B2BF,
        0x00CD_D6F4,
    ]);
    fgs.push(NORD_FG);
    let grounds = [0x0000_0000u32, DEFAULT_BG, 0x0028_2C34, NORD_BG];
    let mut worst = f32::INFINITY;
    let mut at = String::new();
    let mut themes = 0;
    for fg in fgs {
        let budget = bed_luma_budget(fg);
        if budget <= BED_LUMA_MIN {
            continue;
        }
        themes += 1;
        for bg in grounds {
            if relative_luminance(bg) >= budget {
                continue;
            }
            for i in 0..=128u32 {
                let ink = bed_ink(spectrum(i as f32 / 128.0), budget);
                for cov in 1..=(BODY_FRAME_TOP as u32 + 1) {
                    let cov = cov as u8;
                    let lit = over_premul(bg, premul_rgb(ink, cov), cov);
                    let c = contrast(fg, lit);
                    if c < worst {
                        worst = c;
                        at = format!(
                            "fg #{fg:06X} over #{bg:06X}, t {:.3}, cov {cov}, #{lit:06X}",
                            i as f32 / 128.0
                        );
                    }
                }
            }
        }
    }
    assert!(
        themes >= 60,
        "the sweep must cover the themes it claims ({themes})"
    );
    assert!(
        worst >= BODY_CONTRAST_BAR,
        "the bed composited to {worst:.4}:1 at {at}; the bar is {BODY_CONTRAST_BAR}:1 on every dark theme above the floor"
    );
}

/// The Default dark bed is as bright as the letters' bar lets it be: its
/// worst stop, at any coverage the body lays, composites within a tenth
/// of a ratio point over the bar (5.35:1 on 5.25 at Prism Glass P0.11).
/// The sweep above holds the floor on every dark theme; this holds the
/// bed off a dimmer one (replaces the pruned
/// `the_bed_sits_at_the_bar_not_under_it_on_the_default_dark_theme`).
#[test]
fn the_dark_bed_sits_at_the_bar_on_the_default_theme() {
    let c = cfg(true, true);
    let mut lut = BedInkLut::default();
    lut.sync(&c);
    let mut worst = f32::INFINITY;
    for &ink in &lut.lut {
        for cov in 1..=(BODY_FRAME_TOP as u32) {
            let cov = cov as u8;
            let lit = over_premul(c.theme_bg, premul_rgb(ink, cov), cov);
            worst = worst.min(contrast(c.theme_fg, lit));
        }
    }
    assert!(
        worst >= BODY_CONTRAST_BAR,
        "the bed composited to {worst:.4}:1; the bar is {BODY_CONTRAST_BAR}:1"
    );
    assert!(
        worst - BODY_CONTRAST_BAR <= 0.10,
        "the bed's worst stop composites at {worst:.4}:1 against a {BODY_CONTRAST_BAR}:1 bar: \
         light left under the bar is diminished light"
    );
}

/// On a dark page a hot ribbon's body reaches its ceiling and the
/// baseline strip's accent takes the spine past it, never past the
/// ledger's frame top (replaces the pruned
/// `a_dark_body_rides_its_ceiling_and_the_strip_shows_above_it`; it
/// peaked at the frame top, 251, at Prism Glass P0.11).
#[test]
fn a_dark_body_rides_its_ceiling_and_the_strip_shows_above_it() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 12, &c, 1.0);
    let now = at(t0, 11 * 60);
    let cx = ctx(now, &c, (2, 22), 1.0);
    rib.plan(&cx);
    let mut sink = Sink::default();
    rib.emit(&cx, &mut sink.frame());
    let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
    assert!(
        peak > (UNDER_COV_CAP * 0.9) as u8,
        "a hot ribbon must reach its ceiling (peak {peak}); one that cannot is the \
         'dim and muddy' defect"
    );
    assert!(
        peak > UNDER_COV_CAP as u8,
        "the baseline strip must show above the body's ceiling (peak {peak}, cap \
         {UNDER_COV_CAP})"
    );
    assert!(
        f32::from(peak) <= BODY_FRAME_TOP,
        "the strip's lift took the spine to {peak}, past the ledger's frame top"
    );
}

/// On a dark page the companion's head colour is the AUTHORED stop of the
/// caret's own cell, not the stop the bed dims it to (replaces the dark
/// arm of the pruned
/// `head_rgb_is_the_authored_stop_on_dark_and_the_rail_ink_on_light`).
#[test]
fn head_rgb_is_the_authored_stop_on_dark() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    assert!(rib.head_rgb(&c).is_none(), "no cursor observed, no colour");
    type_run(&mut rib, t0, 10, 6, &c, 0.8);
    let cx = ctx(at(t0, 5 * 60), &c, (2, 16), 0.8);
    rib.plan(&cx);
    let t = rib.field_at_caret();
    assert!(
        (t - walk_t(5.0)).abs() < 1e-5,
        "the caret's own cell is the field"
    );
    let arc = spectrum(walk_arc(t));
    assert_eq!(rib.head_rgb(&c), Some(arc), "the head is the authored stop");
    assert_ne!(
        arc,
        bed_ink(arc, bed_luma_budget(c.theme_fg)),
        "fixture: this stop is one the bed dims, so the pin is not vacuous"
    );
}

const PAPERS: [(u32, u32); 6] = [
    (0x001F_2328, 0x00FF_FFFF),
    (0x0020_2733, 0x00F8_FAFD),
    (0x002B_2D33, 0x00F6_F7F9),
    (0x003C_3836, 0x00FB_F1C7),
    (0x004C_4F69, 0x00EF_F1F5),
    (0x0058_6E75, 0x00FD_F6E3),
];

#[test]
fn the_paper_bed_keeps_every_stop_its_hue_and_every_letter_its_bar() {
    let cap = InkRole::OverText.alpha_cap() as u32;
    for (fg, bg) in PAPERS {
        let bar = (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD)
            .min(PAPER_BAR_THEME_SHARE * contrast(fg, bg))
            - BODY_CONTRAST_GUARD;
        for i in 0..=128u32 {
            let stop = spectrum(i as f32 / 128.0);
            let ink = paper_ink(stop, fg, bg, InkRole::OverText);
            for cov in 1..=cap {
                let cov = cov as u8;
                let lit = over_premul(bg, premul_rgb(ink, cov), cov);
                let c = contrast(fg, lit);
                assert!(
                    c >= bar,
                    "#{fg:06X} on #{bg:06X}: stop #{stop:06X} at cov {cov} composites #{lit:06X}, {c:.3}:1 under {bar:.3}:1"
                );
            }
            let lit = over_premul(bg, premul_rgb(ink, cap as u8), cap as u8);
            assert_ne!(lit, bg, "#{bg:06X}: stop #{stop:06X} went out");
            if bg == 0x00FF_FFFF {
                let (h, ..) = rgb2hsv(lit);
                let (h0, ..) = rgb2hsv(stop);
                let off = (h - h0).abs().min(360.0 - (h - h0).abs());
                assert!(
                    off <= 4.0,
                    "on white stop #{stop:06X} (hue {h0:.1}) composites #{lit:06X} (hue {h:.1})"
                );
            }
        }
    }
    let yellow = paper_ink(0x00FF_FF00, PAPERS[0].0, PAPERS[0].1, InkRole::OverText);
    let lit = over_premul(0x00FF_FFFF, premul_rgb(yellow, cap as u8), cap as u8);
    assert!(
        lit & 0x00FF_FF00 == 0x00FF_FF00,
        "yellow on white composites #{lit:06X}, not a yellow"
    );
}

/// Whether two bed tables are main's same table: the same key, inks and floors.
fn same_mains(a: &BedInkLut, b: &BedInkLut) -> bool {
    a.key() == b.key()
        && a.lut() == b.lut()
        && a.seen()
            .iter()
            .zip(b.seen())
            .all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Every light a frame can carry: none, then a latched scheme at each live level.
const LIGHTS: [Light; 6] = [
    Light::OFF,
    Light::at(Live::Off),
    Light::at(Live::Glass),
    Light::at(Live::Lip),
    Light::at(Live::Marks),
    Light::at(Live::Vanish),
];

/// The glass twin of [`the_paper_bed_keeps_every_stop_its_hue_and_every_letter_its_bar`]
/// (Prism Glass §15.2 1.5): on every paper page whose shipped palette has headroom, the bed a
/// glass frame syncs draws its palette's glass129 at every entry and main's sampler between
/// them, and every entry composited over the page at every alpha keeps the theme fg's glyph,
/// as it is seen (`prism_glass::colour::seen`), at the glass bar; what of each the page shows
/// is the glass's own, and main's table beside it is
/// main's. A lip-forward page (Gruvbox Light, Solarized Light, Catppuccin Latte) draws main's
/// paper bed.
#[test]
fn the_glass_bed_keeps_every_entry_its_glass_and_every_letter_its_bar() {
    use crate::rainbow_kitty::prism_glass::{Palette, colour, glass_bar};
    let linear = &crate::color_math::relative_luminance_table().linear;
    let (mut glass_pages, mut forward) = (0, 0);
    for (fg, bg) in PAPERS {
        let paper = Config {
            theme_fg: fg,
            theme_bg: bg,
            ..cfg(false, true)
        };
        let main = BedInkLut::synced(&paper);
        let bed = BedInkLut::synced(&Config {
            light: Light::at(Live::Glass),
            ..paper
        });
        assert!(
            same_mains(&bed, &main),
            "#{bg:06X}: main's table beside the glass"
        );
        let palette = Palette::shipped(fg, bg).expect("every paper page ships");
        if !palette.headroom() {
            forward += 1;
            assert!(!bed.glass_lit(), "#{bg:06X} is lip-forward");
            for k in 0..=1024u32 {
                let t = k as f32 / 1024.0;
                assert_eq!(bed.at(t), main.at(t), "#{bg:06X} at {t}");
            }
            continue;
        }
        glass_pages += 1;
        assert_eq!(bed.glass(), Some(&palette.glass), "#{bg:06X}: glass129");
        let bar = glass_bar(fg, bg);
        let bg_lin = bg_linear_rgb(linear, bg);
        for (i, &glass) in palette.glass.iter().enumerate() {
            let t = i as f32 / (BED_INK_LUT_LEN - 1) as f32;
            assert_eq!(bed.at(t), glass, "#{bg:06X}: entry {i}");
            let seen = seen_floor_with_bg_lin(linear, glass, bg, bg_lin);
            assert_eq!(
                bed.seen_floor_at(t).to_bits(),
                seen.max(seen_floor_with_bg_lin(
                    linear,
                    palette.glass[(i + 1).min(BED_INK_LUT_LEN - 1)],
                    bg,
                    bg_lin
                ))
                .to_bits(),
                "#{bg:06X}: entry {i}'s floor is its glass's"
            );
            for cov in 1..=255u8 {
                let lit = over_premul(bg, premul_rgb(glass, cov), cov);
                let c = colour::seen(fg, bg, lit);
                assert!(
                    c >= bar,
                    "#{fg:06X} on #{bg:06X}: glass #{glass:06X} at cov {cov} composites #{lit:06X}, {c:.3}:1 under {bar:.3}:1"
                );
            }
            assert_ne!(
                over_premul(bg, premul_rgb(glass, 255), 255),
                bg,
                "#{bg:06X}: entry {i} went out"
            );
        }
        for k in 0..=1024u32 {
            let t = k as f32 / 1024.0;
            assert_eq!(
                bed.at(t),
                BedInkLut::sample(&palette.glass, t),
                "#{bg:06X} at {t}: main's sampler over the glass"
            );
        }
        assert_ne!(
            (0..BED_INK_LUT_LEN)
                .filter(|&i| bed.at(i as f32 / 128.0) != main.at(i as f32 / 128.0))
                .count(),
            0,
            "control: #{bg:06X}'s glass is not its paper bed"
        );
    }
    assert_eq!((glass_pages, forward), (3, 3), "the pages swept");
}

/// Prism Glass's bed pin (§15.2 1.5, §15.0.3): on every dark ground (each built-in and pack
/// scheme `aterm_render::theme_is_dark` reads dark, and the tests' own), on both spellings,
/// whatever light the frame carries, the bed syncs main's table byte for byte and draws by
/// it, a ribbon's frame lays main's body and plans and lays no glass (§15.2 1.7), and no
/// palette solve unit runs. Negative control: the same lights on Default Light's page light
/// the glass from the glass level up.
#[test]
fn the_bed_ink_lut_is_mains_on_every_dark_ground() {
    use crate::rainbow_kitty::prism_glass::take_solves;
    let pack = |c: aterm_types::Rgb| aterm_render::rgb_to_u32([c.r, c.g, c.b]);
    let builtins = aterm_types::scheme::builtin_names()
        .into_iter()
        .filter_map(aterm_types::scheme::builtin);
    let packs = aterm_theme_pack::themes()
        .iter()
        .filter_map(|theme| aterm_types::scheme::parse_scheme_str(theme.text).ok());
    let mut grounds: Vec<(u32, u32)> = builtins
        .chain(packs)
        .map(|scheme| (pack(scheme.foreground), pack(scheme.background)))
        .filter(|&(_, bg)| aterm_render::theme_is_dark(bg))
        .collect();
    let schemes = grounds.len();
    assert!(schemes >= 37, "{schemes} dark schemes swept");
    grounds.extend([(DEFAULT_FG, DEFAULT_BG), (SHIPPED_FG, SHIPPED_BG)]);
    let t0 = Instant::now();
    let _ = take_solves();
    let body = |c: &Config| {
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 12, c, 0.9);
        let now = at(t0, 11 * 60 + 16);
        let cx = ctx(now, c, (2, 22), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        (rib, sink.under, now)
    };
    for &(fg, bg) in &grounds {
        for tall in [true, false] {
            let mains = Config {
                theme_fg: fg,
                theme_bg: bg,
                ..cfg(true, tall)
            };
            let main = BedInkLut::synced(&mains);
            for light in LIGHTS {
                let lit = Config { light, ..mains };
                let bed = BedInkLut::synced(&lit);
                assert!(
                    same_mains(&bed, &main) && !bed.glass_lit() && bed.glass().is_none(),
                    "#{fg:06X} on #{bg:06X}, tall {tall}, {light:?}: the bed is main's"
                );
                for k in 0..=256u32 {
                    let t = k as f32 / 256.0;
                    assert_eq!(bed.at(t), main.at(t));
                    assert_eq!(
                        bed.seen_floor_at(t).to_bits(),
                        main.seen_floor_at(t).to_bits()
                    );
                }
            }
        }
        let mains = Config {
            theme_fg: fg,
            theme_bg: bg,
            ..cfg(true, true)
        };
        let (main_rib, main_under, now) = body(&mains);
        let (rib, under, _) = body(&Config {
            light: Light::at(Live::Vanish),
            ..mains
        });
        assert!(!main_under.is_empty(), "fixture: the run laid a body");
        assert_eq!(
            under, main_under,
            "#{fg:06X} on #{bg:06X}: a lit frame lays main's body"
        );
        assert!(same_mains(&rib.ink, &main_rib.ink) && !rib.ink.glass_lit());
        assert!(!rib.glass_drew_at(now) && rib.palettes.is_empty());
        assert!(
            !rib.glass_planned() && aterm_render::test_counters::take_glass_slabs() == 0,
            "#{fg:06X} on #{bg:06X}: a lit frame plans and lays no glass"
        );
    }
    assert_eq!(
        take_solves(),
        0,
        "no palette solve unit ran on a dark ground"
    );

    let (fg, bg) = (0x002B_2D33, 0x00F6_F7F9);
    let paper = Config {
        theme_fg: fg,
        theme_bg: bg,
        ..cfg(false, true)
    };
    let lit: Vec<bool> = LIGHTS
        .iter()
        .map(|&light| BedInkLut::synced(&Config { light, ..paper }).glass_lit())
        .collect();
    assert_eq!(
        lit,
        [false, false, true, true, true, true],
        "control: Default Light's page lights the glass from the glass level up"
    );
    let forced = Config {
        light: Light::at(Live::Vanish),
        dark_theme: true,
        ..paper
    };
    assert!(
        !BedInkLut::synced(&forced).glass_lit(),
        "a dark frame never lights the glass, whatever its pair"
    );
    assert_eq!(take_solves(), 0, "a shipped page runs no unit");
}

/// The glass needs the tall comet spelling and a page with headroom (§15.0.4): the underline
/// and flat spellings of a glass page (composed O11(a)), and a lip-forward page, draw main's
/// paper bed under any light; the tall comet spelling of a glass page lights the glass.
#[test]
fn the_glass_lights_only_the_tall_spelling_of_a_page_with_headroom() {
    let (fg, bg) = (0x002B_2D33, 0x00F6_F7F9);
    for (what, c) in [
        ("the underline spelling", cfg(false, false)),
        ("the flat spelling", flat(false, true)),
        ("Solarized Light", cfg(false, true)),
        ("Catppuccin Latte", cfg(false, true)),
    ] {
        let (fg, bg) = match what {
            "Solarized Light" => (0x0058_6E75, 0x00FD_F6E3),
            "Catppuccin Latte" => (0x004C_4F69, 0x00EF_F1F5),
            _ => (fg, bg),
        };
        let paper = Config {
            theme_fg: fg,
            theme_bg: bg,
            ..c
        };
        let main = BedInkLut::synced(&paper);
        let bed = BedInkLut::synced(&Config {
            light: Light::at(Live::Vanish),
            ..paper
        });
        assert!(!bed.glass_lit(), "{what} lights no glass");
        for k in 0..=256u32 {
            let t = k as f32 / 256.0;
            assert_eq!(bed.at(t), main.at(t), "{what} at {t}");
        }
    }
    let lit = |c: Config| Config {
        theme_fg: fg,
        theme_bg: bg,
        light: Light::at(Live::Vanish),
        ..c
    };
    assert!(
        BedInkLut::synced(&lit(cfg(false, true))).glass_lit(),
        "the tall spelling lights the glass"
    );
    FLAT_GLASS.with(|on| on.set(true));
    let flat_lit = BedInkLut::synced(&lit(flat(false, true))).glass_lit();
    FLAT_GLASS.with(|on| on.set(false));
    assert!(
        flat_lit,
        "control: with its seam set back the flat spelling lights the glass"
    );
}

/// A light page whose palette does not ship draws main's paper bed while its solve runs, at
/// most [`prism_glass::SOLVE_UNITS_PER_FRAME`] units a frame, then the solved glass129: the
/// glass needs a ready palette (§15.2 1.4, 1.5).
#[test]
fn a_pending_palette_draws_mains_paper_bed_until_its_glass_is_solved() {
    use crate::rainbow_kitty::prism_glass::{
        MAX_SOLVE_UNITS, Palette, SOLVE_UNITS_PER_FRAME, solve, take_solves,
    };
    let (fg, bg) = (0x0033_3333, 0x00FA_FAF7);
    assert!(Palette::shipped(fg, bg).is_none(), "fixture: not shipped");
    let paper = Config {
        theme_fg: fg,
        theme_bg: bg,
        ..cfg(false, true)
    };
    let main = BedInkLut::synced(&paper);
    let c = Config {
        light: Light::at(Live::Glass),
        ..paper
    };
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 12, &c, 0.9);
    let now = at(t0, 11 * 60 + 16);
    let cx = ctx(now, &c, (2, 22), 0.9);
    let _ = take_solves();
    let mut frames = 0;
    while !rib.ink.glass_lit() {
        assert!(
            same_mains(&rib.ink, &main),
            "frame {frames}: main's paper bed while the palette solves"
        );
        for k in 0..=128u32 {
            let t = k as f32 / 128.0;
            assert_eq!(rib.ink.at(t), main.at(t), "frame {frames} at {t}");
        }
        rib.plan(&cx);
        let ran = take_solves();
        assert!(
            ran <= SOLVE_UNITS_PER_FRAME,
            "{ran} units in frame {frames}"
        );
        frames += 1;
        assert!(
            frames <= MAX_SOLVE_UNITS.div_ceil(SOLVE_UNITS_PER_FRAME),
            "the solve outran its unit count"
        );
    }
    assert!(frames > 1, "the solve took {frames} frames");
    let solved = solve(fg, bg);
    assert!(solved.headroom(), "fixture: the pair takes glass");
    assert_eq!(rib.ink.glass(), Some(&solved.glass));
    assert!(same_mains(&rib.ink, &main), "main's table beside the glass");
    let _ = take_solves();
    rib.plan(&cx);
    assert_eq!(take_solves(), 0, "a solved pair runs no unit");
    rib.plan(&ctx(now, &paper, (2, 22), 0.9));
    assert!(!rib.ink.glass_lit(), "main's light puts the glass out");
}

/// Default Light's page at the glass level, the glass's fixture (§15.2 1.7): a frame the bed
/// draws by its glass.
const fn glass_cfg(c: Config) -> Config {
    Config {
        theme_fg: 0x002B_2D33,
        theme_bg: 0x00F6_F7F9,
        light: Light::at(Live::Glass),
        ..c
    }
}

/// Default Light's page on main's light: the glass fixture's paper twin.
const fn paper_cfg(c: Config) -> Config {
    Config {
        light: Light::OFF,
        ..glass_cfg(c)
    }
}

/// The bottom of row 2 on [`geom`], where the glass stands.
const ROW2_BOTTOM: i32 = 3 * 18;

/// [`type_run`]'s 12 keys from column 10 of row 2 on `c`, and its frame 16 ms after the
/// last: the ribbon and what it laid under the text.
fn typed_frame(c: &Config, t0: Instant) -> (Ribbon, Vec<GlowQuad>, Instant) {
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 12, c, 0.9);
    let now = at(t0, 11 * 60 + 16);
    let cx = ctx(now, c, (2, 22), 0.9);
    rib.plan(&cx);
    let mut sink = Sink::default();
    rib.emit(&cx, &mut sink.frame());
    (rib, sink.under, now)
}

/// ON A GLASS PAGE THE FLAT RIBBON READS APART FROM THE PET'S (composed O11(a), identity r3's
/// must_fix): under the glass the flat spelling laid the comet's body at the comet's height
/// and in its colours, its taper the only difference, and on GitHub Light 969 px of the
/// ribbon's box told the pet's and the flat ribbon apart where looks10's paper told 7684. It
/// keeps main's paper bed there, its soft even band (looks9 Q6), as the underline spelling
/// does. Typing one run on Default Light at the glass level, each laid on the page: the
/// pixels whose deepest channel lies more than 24 levels apart between the pet's ribbon and
/// the flat one are at least half of the pet's lit pixels (those more than 24 levels off the
/// page) in the run's box, from two rows above its row's bottom to half a row below it.
/// Read here: 2087 px apart against the pet's 1677 lit. Negative control (`FLAT_GLASS`: the
/// flat spelling lights the glass, as before): under three tenths (read: 330).
#[test]
fn on_a_glass_page_the_flat_ribbon_reads_apart_from_the_pet_s() {
    use aterm_render::{add_sat, glow_lerp, glow_lerp_rgb};
    let (g, t0) = (geom(), Instant::now());
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let page = glass_cfg(cfg(false, true)).theme_bg;
    let laid = |c: Config| {
        let (rib, under, _) = typed_frame(&glass_cfg(c), t0);
        (rib.ink.glass_lit(), under)
    };
    // `under` laid on the page at `(x, y)`, each quad's colour and alpha lerped across it.
    let on_page = |under: &[GlowQuad], x: i32, y: i32| -> u32 {
        under
            .iter()
            .filter(|q| {
                let (qx, qy) = (i32::from(q.x), i32::from(q.y));
                q.w > 0
                    && (qx..qx + i32::from(q.w)).contains(&x)
                    && (qy..qy + i32::from(q.h).max(1)).contains(&y)
            })
            .fold(page, |acc, q| {
                let (i, w) = ((x - i32::from(q.x)) as u32, u32::from(q.w));
                let color = glow_lerp_rgb(q.color, q.color2, i, w);
                if q.alpha == 0 && q.alpha2 == 0 {
                    add_sat(acc, color)
                } else {
                    let alpha = glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, w);
                    over_premul(acc, color, alpha.min(255) as u8)
                }
            })
    };
    let deepest = |a: u32, b: u32| {
        [16u32, 8, 0]
            .map(|sh| ((a >> sh) & 0xff).abs_diff((b >> sh) & 0xff))
            .into_iter()
            .max()
            .unwrap_or(0)
    };
    // The pet's lit pixels in the run's box, and those the flat ribbon lays apart from it.
    let apart = |pet: &[GlowQuad], flat: &[GlowQuad]| {
        let (mut lit, mut apart) = (0usize, 0usize);
        for y in ROW2_BOTTOM - 2 * ch..ROW2_BOTTOM + ch / 2 {
            for x in i32::from(g.origin_x) + 10 * cw..i32::from(g.origin_x) + 23 * cw {
                let (p, f) = (on_page(pet, x, y), on_page(flat, x, y));
                lit += usize::from(deepest(p, page) > 24);
                apart += usize::from(deepest(p, f) > 24);
            }
        }
        (apart, lit)
    };
    let (pet_glass, pet) = laid(cfg(false, true));
    let (flat_glass, band) = laid(flat(false, true));
    assert!(
        pet_glass && !flat_glass,
        "fixture: the pet lights the glass and the flat spelling keeps main's paper"
    );
    let (read_apart, lit) = apart(&pet, &band);
    assert!(lit > 0, "fixture: the pet's glass lights the run");
    assert!(
        2 * read_apart >= lit,
        "the flat ribbon lays {read_apart} px apart from the pet's {lit} lit px"
    );
    FLAT_GLASS.with(|on| on.set(true));
    let (old_glass, glassy) = laid(flat(false, true));
    FLAT_GLASS.with(|on| on.set(false));
    assert!(
        old_glass,
        "control fixture: the seam lights the flat spelling's glass"
    );
    let (alike, _) = apart(&pet, &glassy);
    eprintln!("flat apart {read_apart} of the pet's {lit} lit px; under the glass {alike}");
    assert!(
        10 * alike < 3 * lit,
        "negative control: under the glass the flat ribbon lays {alike} px apart from the \
         pet's {lit} lit px"
    );
}

/// A glass frame (§15.2 1.5, 1.7) lays the glass in the glass's ink, standing on its row's
/// bottom with nothing below it where main's paper body reaches below the row, and says so
/// ([`Ribbon::glass_drew_at`], Prism Glass's `glass` arm). Its plan keeps main's geometry, so
/// every cell's band is main's. The same frame on main's light lays main's paper body, plans
/// no glass and draws no arm, and a lit frame with nothing to plan draws none either.
#[test]
fn a_glass_frame_lays_the_glass_in_the_glass_ink() {
    use crate::rainbow_kitty::prism_glass::{colour, glass_bar};
    let paper = paper_cfg(cfg(false, true));
    let lit = glass_cfg(cfg(false, true));
    let t0 = Instant::now();
    let (main_rib, main_under, now) = typed_frame(&paper, t0);
    let (rib, under, _) = typed_frame(&lit, t0);
    assert!(
        !main_under.is_empty() && !under.is_empty(),
        "fixture: the run laid a body"
    );
    assert!(rib.ink.glass_lit() && !main_rib.ink.glass_lit());
    assert!(
        main_under.iter().any(|q| i32::from(q.y) >= ROW2_BOTTOM),
        "control: main's paper body reaches below its row"
    );
    assert!(
        under.iter().all(|q| i32::from(q.y) < ROW2_BOTTOM),
        "the glass stands on its row's bottom"
    );
    let bar = glass_bar(lit.theme_fg, lit.theme_bg);
    let opaque = rib
        .cells()
        .iter()
        .map(|c| (GLASS_ALPHA * c.cov0) as u8)
        .min()
        .expect("fixture: cells");
    let whole: Vec<u32> = under
        .iter()
        .filter(|q| q.alpha >= opaque && q.alpha2 >= opaque)
        .flat_map(|q| [q.color, q.color2])
        .collect();
    assert!(
        !whole.is_empty(),
        "the glass lays its whole opacity, {opaque} or more"
    );
    for ink in whole {
        let c = colour::seen(lit.theme_fg, lit.theme_bg, ink);
        assert!(
            c >= bar,
            "the glass's #{ink:06X} keeps the letters, as seen, {c:.3}:1, under {bar:.3}:1"
        );
    }
    assert!(rib.glass_planned() && !main_rib.glass_planned());
    for col in 0..40 {
        assert_eq!(rib.band(2, col), main_rib.band(2, col), "column {col}");
    }
    assert!(rib.glass_drew_at(now) && !main_rib.glass_drew_at(now));
    assert!(
        !rib.glass_drew_at(at(t0, 11 * 60 + 32)),
        "the arm is the frame's"
    );

    let mut idle = Ribbon::new();
    let cx = ctx(now, &lit, (2, 22), 0.9);
    idle.plan(&cx);
    let mut sink = Sink::default();
    idle.emit(&cx, &mut sink.frame());
    assert!(idle.ink.glass_lit() && sink.under.is_empty() && idle.plan.is_empty());
    assert!(
        !idle.glass_drew_at(now) && idle.glass_span().is_none(),
        "a frame with no plan draws no arm"
    );
}

/// v4's glass on a single-row composer (§2.2, §15.2 1.7): typed along one row, the glass
/// stands on the row's bottom and lays nothing on or below it; it is 0.96 ch tall at the hand
/// and 0.86 ch a taper's length behind it, its top a 2.5 px melt over whole opacity
/// (source-over, 255 hot and [`BODY_COLD_SHARE`] of it cold), laid by the glass's wedge walk; it draws no rail and no hot edge.
/// The negative control is main's paper arm on the same keys: it reaches below the row and
/// lays no glass slab, under its role's cap.
#[test]
fn a_single_row_composer_draws_v4s_glass() {
    use aterm_render::test_counters::take_glass_slabs;
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as f32);
    let keys = Keys {
        g,
        row: 2,
        col0: 2,
        n: 30,
        period_ms: 60,
        disp: 0.9,
    };
    let lay = |c: &Config| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(&mut rib, t0, keys, c);
        let cx = ctx(at(t0, 29 * 60 + 16), c, (2, 32), 0.9);
        let _ = take_glass_slabs();
        rib.plan(&cx);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        (rib, sink.under, sink.out, take_glass_slabs())
    };
    let (rib, under, out, slabs) = lay(&glass_cfg(cfg(false, true)));
    assert!(rib.ink.glass_lit(), "fixture: the bed draws by its glass");
    assert!(slabs > 0, "the glass's wedge walk laid it");
    assert!(out.is_empty(), "no hot edge on light");
    assert!(rib.rail_spans().is_empty(), "no rail under glass");
    assert!(
        under.iter().all(|q| q.alpha > 0 || q.alpha2 > 0),
        "the glass is source-over"
    );
    assert!(
        under.iter().all(|q| i32::from(q.y) < ROW2_BOTTOM),
        "nothing on or below the row's bottom"
    );
    let census = |col: i32, floor: u8| {
        column_census(&under, col * cw..(col + 1) * cw, 0..ROW2_BOTTOM + 18, floor)
            .into_iter()
            .flatten()
            .fold((i32::MAX, 0u8), |(top, peak), (t, _, p)| {
                (top.min(t), peak.max(p))
            })
    };
    let (hand_top, hand_peak) = census(31, 128);
    let (tail_top, _) = census(31 - 16, 128);
    let (hand_lit, _) = census(31, 1);
    let reach = |up_ch: f32| ROW2_BOTTOM as f32 - up_ch * ch;
    assert!(
        (reach(GLASS_UP_CH).floor() as i32..=(reach(GLASS_UP_CH) + GLASS_EDGE_PX).ceil() as i32)
            .contains(&hand_top),
        "the hand's glass is half opaque from y {hand_top}, not {:.2} ch tall",
        GLASS_UP_CH
    );
    assert!(
        (reach(GLASS_TAIL_UP_CH).floor() as i32 - 1
            ..=(reach(GLASS_TAIL_UP_CH) + GLASS_EDGE_PX).ceil() as i32 + 1)
            .contains(&tail_top),
        "the tail's glass is half opaque from y {tail_top}, not {:.2} ch tall",
        GLASS_TAIL_UP_CH
    );
    assert!(tail_top > hand_top, "the glass tapers from the hand");
    let whole = rib
        .cells()
        .iter()
        .find(|c| c.col == 30)
        .map_or(0, |c| (GLASS_ALPHA * c.cov0) as u8);
    assert!(
        whole >= 250 && hand_peak == whole,
        "the hand's glass is whole: {hand_peak} of {whole}"
    );
    let whole_from = column_census(&under, 31 * cw..32 * cw, 0..ROW2_BOTTOM, 250)
        .into_iter()
        .flatten()
        .map(|(t, _, _)| t)
        .min()
        .expect("whole rows at the hand");
    assert!(
        whole_from - hand_lit <= GLASS_EDGE_PX.ceil() as i32 + 1,
        "the top melts over {} rows, not {GLASS_EDGE_PX} px",
        whole_from - hand_lit
    );

    let (main, main_under, _, main_slabs) = lay(&paper_cfg(cfg(false, true)));
    assert!(!main.ink.glass_lit() && main_slabs == 0);
    assert!(
        main_under.iter().any(|q| i32::from(q.y) >= ROW2_BOTTOM),
        "control: main's paper arm reaches below the row"
    );
    let cap = light_role(&paper_cfg(cfg(false, true))).alpha_cap();
    assert!(
        main_under
            .iter()
            .filter(|q| i32::from(q.y) < ROW2_BOTTOM)
            .all(|q| f32::from(q.alpha.max(q.alpha2)) <= cap),
        "control: main's paper body keeps its role's cap"
    );
}

/// §2.2's seams (§15.2 1.7): on a glass frame a cohort seam and an in-run walk jump (cells
/// whose walk differs by more than [`GLASS_HARD_STEP_T`]) are hard steps: the seam's run
/// opens at its own walk, and the jump's boundary is two vertices at one x, each in its own
/// side's ink. A cell boundary lerps: one vertex at the mean of its cells' walk. Main's frame
/// lerps across both (the negative control): its seam opens at its neighbour's walk and its
/// jump is one vertex at the mean.
#[test]
fn a_seam_or_walk_jump_is_a_hard_step_on_light_and_a_cell_boundary_is_not() {
    let cw = geom().cw as f32;
    // The jump sits in the run left of the seam, the run the ribbon paints last, whose
    // vertices it keeps.
    let (jump_col, seam_col) = (13u16, 16u16);
    let lay = |c: &Config| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 12, c, 0.9);
        let coh = rib.cells[0].cohort;
        let ci = rib
            .cohorts
            .iter()
            .position(|k| k.id == coh)
            .expect("the run's cohort");
        let mut copy = rib.cohorts[ci];
        copy.id = rib.next_cohort;
        rib.next_cohort += 1;
        copy.col0 = seam_col;
        // A wake cohort: a seam heals only between two hot ones.
        copy.wake = true;
        rib.cohorts[ci].col1 = seam_col;
        rib.cohorts.push(copy);
        for cell in &mut rib.cells {
            if cell.col >= jump_col {
                cell.t += 0.25;
            }
            if cell.col >= seam_col {
                cell.cohort = copy.id;
                cell.t += 0.25;
            }
        }
        let cx = ctx(at(t0, 11 * 60 + 16), c, (2, 22), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        assert!(!sink.under.is_empty(), "fixture: the runs laid a body");
        rib
    };
    let t_of = |rib: &Ribbon, col: u16| {
        rib.cells
            .iter()
            .find(|k| k.col == col)
            .map(|k| k.t)
            .expect("a cell")
    };
    for (glass, c) in [
        (true, glass_cfg(cfg(false, true))),
        (false, paper_cfg(cfg(false, true))),
    ] {
        let rib = lay(&c);
        assert_eq!(rib.ink.glass_lit(), glass);
        let right = rib
            .runs
            .iter()
            .find(|r| r.col0 == seam_col)
            .copied()
            .expect("the seam's right run");
        assert!(
            rib.runs.iter().any(|r| r.col1 + 1 == seam_col),
            "fixture: a run abuts the seam"
        );
        let first = rib.plan[right.lo];
        let (left_t, own_t) = (t_of(&rib, seam_col - 1), t_of(&rib, seam_col));
        assert!(
            own_t - left_t > GLASS_HARD_STEP_T,
            "fixture: the seam jumps"
        );
        if glass {
            assert_eq!(first.t, own_t, "glass: the seam opens at its own walk");
        } else {
            assert_eq!(
                first.t, left_t,
                "control: main's seam opens at its neighbour's"
            );
        }
        let at_x = |x: f32| {
            rib.verts
                .iter()
                .filter(|v| (v.x - x).abs() < 1e-3)
                .map(|v| v.color)
                .collect::<Vec<_>>()
        };
        let plan_at = |x: f32| {
            rib.plan
                .iter()
                .find(|s| (s.x - x).abs() < 1e-3)
                .copied()
                .expect("a boundary")
        };
        let (x_cell, x_jump) = (f32::from(jump_col - 1) * cw, f32::from(jump_col) * cw);
        let (t11, t12, t13) = (
            t_of(&rib, jump_col - 2),
            t_of(&rib, jump_col - 1),
            t_of(&rib, jump_col),
        );
        assert!(t13 - t12 > GLASS_HARD_STEP_T, "fixture: the run jumps");
        let cell = plan_at(x_cell);
        assert_eq!(
            cell.glass_jump, 0.0,
            "glass {glass}: a cell boundary is no step"
        );
        assert_eq!(
            cell.t,
            (t11 + t12) * 0.5,
            "glass {glass}: a cell boundary is the mean of its cells' walk"
        );
        // Main's last laid vertices are its rail's, in the rail's ink.
        let ink = |t: f32| {
            if glass {
                rib.ink.at(t)
            } else {
                rib.ink.rail_at(t)
            }
        };
        assert_eq!(
            at_x(x_cell),
            [ink(cell.t)],
            "glass {glass}: a cell boundary is one vertex"
        );
        let jump = plan_at(x_jump);
        if glass {
            assert_eq!((jump.t, jump.glass_jump), (t12, t13 - t12));
            assert_eq!(
                at_x(x_jump),
                [rib.ink.at(t12 + (t13 - t12)), rib.ink.at(t12)],
                "glass: the jump is a hard step, its right side first as the run is laid"
            );
        } else {
            assert_eq!((jump.t, jump.glass_jump), ((t12 + t13) * 0.5, 0.0));
            assert_eq!(
                at_x(x_jump),
                [ink(jump.t)],
                "control: main's jump is one vertex at the mean"
            );
        }
    }
}

/// The glass's intensity is its height (§15.2 1.7): at half intensity the glass is half as
/// tall and as whole at the hand; at a tenth, under [`GLASS_MIN_PX`], it lays nothing but is
/// still the glass arm's frame, and leaves no span for the guard or the yield. The negative
/// control is main's paper arm, whose intensity is its opacity.
#[test]
fn glass_at_half_intensity_is_half_height_at_full_alpha() {
    let cw = geom().cw as i32;
    let hand = |c: Config| {
        let t0 = Instant::now();
        let (rib, under, now) = typed_frame(&c, t0);
        let (mut top, mut lit, mut peak) = (i32::MAX, i32::MAX, 0u8);
        for (floor, at) in [(128u8, &mut top), (1, &mut lit)] {
            for (t, _, p) in column_census(&under, 20 * cw..21 * cw, 0..ROW2_BOTTOM + 18, floor)
                .into_iter()
                .flatten()
            {
                *at = (*at).min(t);
                peak = peak.max(p);
            }
        }
        let whole = rib
            .cells()
            .iter()
            .find(|c| c.col == 20)
            .map_or(0, |c| (GLASS_ALPHA * c.cov0) as u8);
        (
            ROW2_BOTTOM - top,
            ROW2_BOTTOM - lit,
            (peak, whole),
            (rib.glass_drew_at(now), rib.glass_span().is_some()),
            under.len(),
        )
    };
    let at_intensity = |c: Config, intensity: f32| Config { intensity, ..c };
    let glass = glass_cfg(cfg(false, true));
    let (h1, _, peak1, drew1, _) = hand(glass);
    let (h2, _, peak2, drew2, _) = hand(at_intensity(glass, 0.5));
    assert!(drew1 == (true, true) && drew2 == (true, true));
    assert!(
        (h2 as f32 - h1 as f32 * 0.5).abs() <= 1.5,
        "half intensity stands {h2} px against {h1} px"
    );
    assert!(
        peak1.1 >= 250 && peak1 == (peak1.1, peak1.1) && peak2 == (peak2.1, peak2.1),
        "whole at the hand at both intensities: {peak1:?}, {peak2:?}"
    );
    let (_, _, _, drew, quads) = hand(at_intensity(glass, 0.1));
    assert!(
        0.1 * GLASS_UP_CH * (geom().ch as f32) < GLASS_MIN_PX,
        "fixture: a tenth of the glass is under its least"
    );
    assert_eq!(quads, 0, "a glass under {GLASS_MIN_PX} px draws nothing");
    assert_eq!(
        drew,
        (true, false),
        "its frame is the glass arm's, with no span laid"
    );

    let paper = paper_cfg(cfg(false, true));
    let (_, lit1, main1, _, _) = hand(paper);
    let (_, lit2, main2, _, _) = hand(at_intensity(paper, 0.5));
    assert!(
        f32::from(main2.0) <= 0.75 * f32::from(main1.0),
        "control: main's half intensity lays {} against {}",
        main2.0,
        main1.0
    );
    assert!(
        (lit1 - lit2).abs() <= 2,
        "control: main's height holds ({lit1} and {lit2} px)"
    );
}

/// A see-through page (`host::PageFacts`: a translucent window, a wallpaper or an image) draws
/// main's paper arm (§15.2 1.7): its sheer light at the glass level syncs main's paper bed,
/// plans no glass, lays main's paper frame byte for byte and draws no arm. The negative
/// control is the same light on a clear page, which draws the glass.
#[test]
fn a_translucent_page_draws_mains_paper_arm() {
    use aterm_render::test_counters::take_glass_slabs;
    let paper = paper_cfg(cfg(false, true));
    let t0 = Instant::now();
    let _ = take_glass_slabs();
    let (main, main_under, now) = typed_frame(&paper, t0);
    for light in [Light::at(Live::Glass), Light::at(Live::Vanish)] {
        let sheer = Config {
            light: light.on_page(false),
            ..paper
        };
        assert!(sheer.light.on() && !sheer.light.glass());
        let (rib, under, _) = typed_frame(&sheer, t0);
        assert!(!rib.ink.glass_lit() && same_mains(&rib.ink, &main.ink));
        assert!(!rib.glass_planned() && !rib.glass_drew_at(now));
        assert_eq!(under, main_under, "{light:?}: main's paper frame");
        assert_eq!(
            rib.rail_spans(),
            main.rail_spans(),
            "{light:?}: main's rail"
        );
    }
    assert_eq!(take_glass_slabs(), 0, "a sheer light lays no glass");
    let clear = Config {
        light: Light::at(Live::Glass).on_page(true),
        ..paper
    };
    let (rib, under, _) = typed_frame(&clear, t0);
    assert!(rib.ink.glass_lit() && rib.glass_drew_at(now));
    assert_ne!(under, main_under, "control: a clear page draws the glass");
}

/// No rail under glass (§15.2 1.7, the declared interim until the lip): a glass frame lays
/// no rail span and nothing on or below its row's bottom, on the comet spelling (the flat
/// one keeps main's paper bed, composed O11(a)), with the row below free and with the row
/// held. The negative control is main's paper comet, whose rail lies below the row.
#[test]
fn no_rail_quad_under_glass() {
    let t0 = Instant::now();
    for (what, c) in [("the comet", cfg(false, true))] {
        for held in [false, true] {
            let lit = glass_cfg(c);
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 12, &lit, 0.9);
            let now = at(t0, 11 * 60 + 16);
            let cx = ctx(now, &lit, (2, 22), 0.9);
            rib.plan(&cx);
            if held {
                for run in &mut rib.runs {
                    run.hold = 1.0;
                }
            }
            let mut sink = Sink::default();
            rib.emit(&cx, &mut sink.frame());
            assert!(!sink.under.is_empty(), "{what}, held {held}: fixture");
            assert!(
                rib.rail_spans().is_empty(),
                "{what}, held {held}: a rail span"
            );
            assert!(
                sink.under.iter().all(|q| i32::from(q.y) < ROW2_BOTTOM),
                "{what}, held {held}: light on or below the row's bottom"
            );
        }
    }
    let (main, under, _) = typed_frame(&paper_cfg(cfg(false, true)), t0);
    assert!(Ribbon::rail_lit(&paper_cfg(cfg(false, true))) && !main.ink.glass_lit());
    assert!(
        under.iter().any(|q| i32::from(q.y) > ROW2_BOTTOM),
        "control: main's paper comet lays its rail below the row"
    );
}

#[test]
fn the_paper_caret_is_its_stop_deepened_and_never_olive() {
    for (_, bg) in PAPERS {
        for i in 0..=128u32 {
            let stop = spectrum(i as f32 / 128.0);
            let caret = paper_caret_ink(stop, bg);
            let c = contrast(caret, bg);
            assert!(
                c >= PAPER_CARET_CONTRAST - 0.01,
                "on #{bg:06X} stop #{stop:06X} carets #{caret:06X} at {c:.3}:1"
            );
            let (_, s, _) = rgb2hsv(caret);
            assert!(
                s >= 0.99,
                "stop #{stop:06X} carets #{caret:06X}, off its hue ray"
            );
        }
    }
    // Blue stands 8.6:1 on white and stays pure; red, at 4.0:1, is
    // deepened along its own ray to the caret law's 4.5:1.
    assert_eq!(paper_caret_ink(0x0000_00FF, 0x00FF_FFFF), 0x0000_00FF);
    let red = paper_caret_ink(0x00FF_0000, 0x00FF_FFFF);
    assert!(
        red & 0x0000_FFFF == 0 && contrast(red, 0x00FF_FFFF) >= 4.5,
        "red carets #{red:06X}"
    );
    // Control: held to a graphic's 3:1, a stop's caret on white falls
    // under the caret law.
    let low = (0..=128u32)
        .map(|i| {
            let stop = spectrum(i as f32 / 128.0);
            contrast(paper_vivid_ink(stop, 0x00FF_FFFF, 3.0), 0x00FF_FFFF)
        })
        .fold(f32::INFINITY, f32::min);
    assert!(low < 4.5, "control: at 3:1 every caret stands {low:.2}:1");
    let (h, ..) = rgb2hsv(paper_caret_ink(0x00FF_FF00, 0x00FF_FFFF));
    assert!(
        (35.0..=50.0).contains(&h),
        "yellow's caret sits at hue {h:.1}, not amber"
    );
    // looks10 L7: no stop's caret is an olive or a khaki, a hue between
    // the amber and the green deepened to the caret law. Control: bent
    // as a mark is, the yellow-greens keep their hue and read olive.
    let olive = |c: u32| {
        let (h, s, v) = rgb2hsv(c);
        (46.0..100.0).contains(&h) && s > 0.3 && v < 0.85
    };
    for (_, bg) in PAPERS {
        for i in 0..=128u32 {
            let stop = spectrum(i as f32 / 128.0);
            let caret = paper_caret_ink(stop, bg);
            assert!(
                !olive(caret),
                "on #{bg:06X} stop #{stop:06X} carets #{caret:06X}, an olive"
            );
        }
    }
    let olives = (0..=128u32)
        .filter(|&i| {
            olive(paper_vivid_ink(
                spectrum(i as f32 / 128.0),
                0x00FF_FFFF,
                4.5,
            ))
        })
        .count();
    assert!(
        olives > 0,
        "control: bent as a mark is, no caret reads olive"
    );
    // looks10 round 2's L7: the burst's lime head (88 degrees) carets a
    // clean green, as the head reads, not round 1's bronze. Control:
    // stepped to green at 89 degrees, as round 1 bent it, it is bronze.
    const ROUND_1_BEND: [(f32, f32); 5] = [
        (30.0, 30.0),
        (72.0, 40.0),
        (89.0, 43.0),
        (89.0, 118.0),
        (122.0, 122.0),
    ];
    let lime = crate::color_math::hsv2rgb(88.0, 1.0, 1.0);
    for (_, bg) in PAPERS {
        let caret = paper_caret_ink(lime, bg);
        let (h, ..) = rgb2hsv(caret);
        assert!(
            (110.0..=130.0).contains(&h),
            "on #{bg:06X} the lime head carets #{caret:06X} at hue {h:.1}, not a green"
        );
        let bronze = paper_vivid_ink_bent(lime, bg, PAPER_CARET_CONTRAST, &ROUND_1_BEND);
        let (h1, ..) = rgb2hsv(bronze);
        assert!(
            h1 < 50.0,
            "control: on #{bg:06X} round 1's bend carets the lime #{bronze:06X} at {h1:.1}"
        );
    }
}

#[test]
fn the_field_index_answers_what_a_newest_first_scan_would() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let scan_matches = |rib: &Ribbon, ms: u64| {
        for row in 0..6u16 {
            for col in 0..30u16 {
                let scan = rib
                    .cells()
                    .iter()
                    .rev()
                    .find(|l| l.row == row && l.col == col)
                    .map(|l| l.t);
                let idx = rib.field_at(row, col);
                match (scan, idx) {
                    (Some(a), Some(b)) => assert!(
                        (a - b).abs() < 1e-6,
                        "+{ms} ms: index says {b} at ({row},{col}); a newest-first scan says {a}"
                    ),
                    (None, None) => {}
                    (a, b) => {
                        panic!("+{ms} ms: index and scan disagree at ({row},{col}): {a:?} vs {b:?}")
                    }
                }
            }
        }
    };
    let mut script: Vec<(u64, (u16, u16), Event)> = Vec::new();
    for i in 0..8u16 {
        script.push((u64::from(i) * 60, (2, 11 + i), typed()));
    }
    script.push((500, (2, 40), nav((2, 18), (2, 40))));
    script.push((500, (2, 14), nav((2, 40), (2, 14))));
    script.push((500, (2, 14), typed()));
    script.push((560, (2, 15), typed()));
    script.push((900, (3, 10), nav((2, 15), (3, 10))));
    for i in 0..6u16 {
        script.push((1000 + u64::from(i) * 60, (3, 11 + i), typed()));
    }
    script.push((1600, (2, 21), nav((3, 16), (2, 21))));
    for i in 0..3u16 {
        script.push((1600 + u64::from(i) * 60, (2, 21 + i), typed()));
    }
    let mut caret = (2u16, 10u16);
    let (mut saw_shadow, mut saw_dark_row_2, mut saw_relit_row_2) = (false, false, false);
    for ms in (0..=2400u64).step_by(20) {
        let now = at(t0, ms);
        for (_, c_at, ev) in script.iter().filter(|(t, _, _)| *t == ms) {
            caret = *c_at;
            let cx = ctx(now, &c, caret, 0.6);
            rib.on_event(ev, now, &cx);
        }
        let cx = ctx(now, &c, caret, 0.6);
        rib.plan(&cx);
        scan_matches(&rib, ms);
        let cells = rib.cells();
        saw_shadow |= cells.iter().filter(|l| (l.row, l.col) == (2, 13)).count() >= 2;
        let row_2_dark = !cells.is_empty() && cells.iter().all(|l| l.row != 2);
        saw_dark_row_2 |= row_2_dark;
        saw_relit_row_2 |= saw_dark_row_2 && cells.iter().any(|l| l.row == 2);
    }
    assert!(
        saw_shadow,
        "the script must put a genuinely shadowed cell in the pool"
    );
    assert!(saw_dark_row_2, "the script must take row 2 dark");
    assert!(saw_relit_row_2, "…and relight it");
}

fn lab_de(a: u32, b: u32) -> f32 {
    let lab = |c: u32| {
        let ch = |sh: u32| ((c >> sh) & 0xff) as f32 / 255.0;
        let lin = |v: f32| {
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, bl) = (lin(ch(16)), lin(ch(8)), lin(ch(0)));
        let (x, y, z) = (
            0.4124 * r + 0.3576 * g + 0.1805 * bl,
            0.2126 * r + 0.7152 * g + 0.0722 * bl,
            0.0193 * r + 0.1192 * g + 0.9505 * bl,
        );
        let f = |t: f32| {
            if t > 0.008_856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
        (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
    };
    let (l1, a1, b1) = lab(a);
    let (l2, a2, b2) = lab(b);
    ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
}

fn arc_step(a: f32, b: f32, budget: f32) -> (f32, f32) {
    let bed = |t: f32| bed_ink(spectrum(t), budget);
    (
        lab_de(bed(a), bed(b)),
        lab_de(rail_ink(spectrum(a)), rail_ink(spectrum(b))),
    )
}

#[test]
fn the_walk_s_pace_is_the_arc_s_own_perceptual_step() {
    let budget = bed_luma_budget(NORD_FG);
    const N: usize = 4096;
    let (w0, w1) = (CROSS_ARC0, CROSS_ARC1);
    let mut cum = [0.0f32; N + 1];
    for i in 1..=N {
        let a = w0 + (w1 - w0) * (i - 1) as f32 / N as f32;
        let b = w0 + (w1 - w0) * i as f32 / N as f32;
        let (db, dr) = arc_step(a, b, budget);
        cum[i] = cum[i - 1] + (db + dr).max(0.005);
    }
    let total = cum[N];
    let mut j = 0usize;
    let mut worst = 0.0f32;
    for (k, &baked) in CROSS_PACE.iter().enumerate() {
        let want = k as f32 / (CROSS_PACE_KNOTS - 1) as f32 * total;
        while j < N && cum[j + 1] < want {
            j += 1;
        }
        let span = (cum[j + 1] - cum[j]).max(1e-9);
        let f = ((want - cum[j]) / span).clamp(0.0, 1.0);
        let a = w0 + (w1 - w0) * (j as f32 + f) / N as f32;
        let got = f32::from(baked) / 65535.0;
        worst = worst.max((a - got).abs());
    }
    assert!(
        worst <= 2.0 * (CROSS_ARC1 - CROSS_ARC0) / (CROSS_PACE_KNOTS - 1) as f32,
        "the baked pace is {worst:.5} of the arc off the ink's own step — re-generate it"
    );
}

#[test]
fn the_walk_is_a_function_of_position_so_a_retyped_cell_takes_back_its_stop() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 40, &c, 0.7);
    let mut ms = 40 * 60;
    for caret in [41u16, 40, 39] {
        erase_at(&mut rib, at(t0, ms), (2, caret), &c);
        ms += 60;
    }
    for cell in rib.cells() {
        assert_eq!(
            cell.retract_at.is_some(),
            cell.col >= 39,
            "col {} is on the wrong side of the backspace",
            cell.col
        );
    }
    assert!(
        (2..39u16).all(|col| rib.field_at(2, col).is_some()),
        "the lit run must stay contiguous from the caret backward"
    );
    for caret in [40u16, 41, 42, 21] {
        let now = at(t0, ms);
        let cx = ctx(now, &c, (2, caret), 0.7);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        ms += 60;
    }
    for cell in rib.cells().iter().filter(|l| l.retract_at.is_none()) {
        let want = walk_t(f32::from(cell.col) - 2.0);
        assert!(
            (cell.t - want).abs() < 1e-5,
            "col {} carries t = {} but the walk at its column is {want}",
            cell.col,
            cell.t
        );
    }
    for col in [20u16, 39, 40, 41] {
        let owners: Vec<&Cell> = rib.cells().iter().filter(|l| l.col == col).collect();
        assert_eq!(
            owners.len(),
            1,
            "col {col} must have exactly one owner after a retype"
        );
        assert!(owners[0].retract_at.is_none(), "…and it is live again");
    }
}

#[test]
fn the_reach_stops_at_the_hand_s_own_first_column() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let keys = Keys {
        g: geom(),
        row: 2,
        col0: 28,
        n: 2,
        period_ms: 60,
        disp: 0.8,
    };
    type_keys(&mut rib, t0, keys, &c);
    let now = at(t0, 60 + (PHRASE_REST_MIN_S * 1000.0) as u64 + 100);
    let cx = ctx(now, &c, (2, 30), 0.0);
    rib.plan(&cx);
    assert!(
        rib.field_at(2, 28).is_some(),
        "the hand's own first cell must still be lit, or this pins nothing"
    );
    for col in 0..28u16 {
        assert!(
            rib.field_at(2, col).is_none(),
            "the reach lit column {col}, left of the first column the hand \
             ever held on the row — at a shell prompt that is the prompt"
        );
    }
}

#[test]
fn a_jump_s_landing_column_is_stamped_on_its_own_row_alone() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g: geom(),
            row: 2,
            col0: 28,
            n: 32,
            period_ms: 12,
            disp: 0.8,
        },
        &c,
    );
    type_keys(
        &mut rib,
        at(t0, 400),
        Keys {
            g: geom(),
            row: 3,
            col0: 0,
            n: 16,
            period_ms: 12,
            disp: 0.8,
        },
        &c,
    );
    assert!(
        rib.cohorts().iter().any(|coh| coh.row == 2),
        "row 2 must still carry a cohort, or this pins nothing"
    );
    let now = at(t0, 700);
    let cx = ctx(now, &c, (3, 40), 0.0);
    rib.on_event(
        &Event::Move {
            from: (3, 16),
            to: (3, 40),
            licence: Licence::Nav,
            dir: Dir::Right,
        },
        now,
        &cx,
    );
    for coh in rib.cohorts().iter().filter(|coh| coh.row != 3) {
        assert!(
            coh.retract_col != Some(40),
            "a cohort on row {} was handed column 40, which is a place on \
             row 3 — it must reach the per-row law instead",
            coh.row
        );
    }
}

#[test]
fn the_wave_breathes_a_tenth_of_a_cell_at_full_momentum_inside_the_body_s_own_rows() {
    for (c, dn_ch) in [
        (cfg(true, true), COMET_DN_HAND_CH),
        (flat(true, true), DN_TOP_CH),
    ] {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 24, &c, 1.0);
        let g = geom();
        let ch = g.ch as f32;
        let rest = f32::from(g.origin_y) + 3.0 * ch;
        let mut deepest = 0.0f32;
        for k in 0..16u32 {
            let mut cx = ctx(at(t0, 23 * 60 + 8), &c, (2, 34), 1.0);
            cx.phase = k as f32 / 16.0;
            rib.plan(&cx);
            for col in 10..34u16 {
                let band = rib.band(2, col).expect("a laid cell has a band");
                deepest = deepest.max((band.spine - rest).abs());
                assert!(
                    band.top >= rest - TALL_UP_CH * ch - 0.5,
                    "the wave lifted the body's top past its own reach at col {col}, phase {k}/16 ({})",
                    band.top
                );
                assert!(
                    band.bottom <= rest + dn_ch * ch + 0.5,
                    "the wave dropped the body's bottom past its own reach at col {col}, phase {k}/16 ({}; flat {})",
                    band.bottom,
                    c.ribbon_flat
                );
            }
        }
        assert!(
            deepest >= 0.10 * ch,
            "the wave's deepest swing at full momentum is {deepest:.2} px ({:.3} ch); the owner asked for a ribbon that visibly breathes with speed — `WAVE_AMP_CELLS` 0.110",
            deepest / ch
        );
    }
}

#[test]
fn the_hot_edge_lives_only_at_the_head_on_a_dark_ground() {
    let dark = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &dark, 0.9);
    let g = geom();
    let caret_top = f32::from(g.origin_y) + 2.0 * g.ch as f32;
    let caret_bot = caret_top + g.ch as f32;
    let reach_y = HOT_EDGE_CARET_REACH_CH * g.ch as f32;
    let check = |out: &[GlowQuad], head_col: u16, label: &str| {
        assert!(!out.is_empty(), "{label}: a hot hand must leave a hot edge");
        let head_x = f32::from(g.origin_x) + f32::from(head_col) * g.cw as f32;
        for q in out {
            assert!(
                u32::from(q.alpha) == 0,
                "{label}: the hot edge is ADDITIVE light; it may not carry a source-over alpha"
            );
            let x = f32::from(q.x);
            assert!(
                x <= head_x + 1.0,
                "{label}: the hot edge ran PAST the head cell (x = {x}, head = {head_x})"
            );
            assert!(
                head_x - x <= HOT_EDGE_CELLS_MAX * g.cw as f32 + f32::from(q.w) + 1.0,
                "{label}: the hot edge reached further than {HOT_EDGE_CELLS_MAX} cells behind the head"
            );
            let y = f32::from(q.y);
            assert!(
                (caret_top - reach_y..=caret_bot + reach_y).contains(&y),
                "{label}: the hot edge left the caret's own row band (y = {y})"
            );
        }
        assert!(
            peak_channel(out) as f32 <= HOT_EDGE_COV_MAX + 1.0,
            "{label}: hot-edge coverage is over the {HOT_EDGE_COV_MAX} request ceiling"
        );
    };
    let mut sink = Sink::default();
    let cx = ctx(at(t0, 8 * 60), &dark, (2, 18), 0.9);
    rib.plan(&cx);
    {
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }
    check(&sink.out, 18, "typing");
    let mut ms = 8 * 60;
    for caret in [17u16, 16, 15] {
        erase_at(&mut rib, at(t0, ms), (2, caret), &dark);
        ms += 60;
    }
    let cx = ctx(at(t0, ms), &dark, (2, 15), 0.9);
    rib.plan(&cx);
    {
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }
    assert!(
        rib.cells().iter().any(|l| l.col >= 15),
        "the erased cells must still be draining for this frame to test anything"
    );
    check(&sink.out, 15, "backspace");
    for caret in [14u16, 13, 12, 11, 10] {
        erase_at(&mut rib, at(t0, ms), (2, caret), &dark);
        ms += 60;
    }
    let cx = ctx(at(t0, ms), &dark, (2, 10), 0.9);
    rib.plan(&cx);
    {
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }
    assert!(
        !rib.cells().is_empty() && !sink.under.is_empty(),
        "the draining cells must still be on glass for this frame to test anything"
    );
    assert!(
        sink.out.is_empty(),
        "a word that is entirely draining has no wet head; the hot edge sat on erased cells"
    );
}

#[test]
fn the_body_never_brightens_after_the_last_keystroke() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 10, &c, 0.9);
    let last = at(t0, 9 * 60);
    let mut sink = Sink::default();
    let mut peak_prev = 255u8;
    let mut edge_first: Option<u32> = None;
    for ms in (20..1400).step_by(8) {
        let now = at(last, ms);
        let disp = (0.4 + 0.6 * (ms as f32 / 700.0)).min(1.0);
        let cx = ctx(now, &c, (2, 20), disp);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let peak = plan_peak(&rib);
        assert!(
            peak <= peak_prev,
            "the body brightened at +{ms} ms with no keystroke behind it ({peak_prev} → {peak})"
        );
        peak_prev = peak;
        let lit = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
        assert!(
            f32::from(lit) <= BODY_FRAME_TOP,
            "the composited body passed the ledger's frame top at +{ms} ms ({lit})"
        );
        let edge = peak_channel(&sink.out);
        let first = *edge_first.get_or_insert(edge);
        assert!(
            edge <= first + 4,
            "the hot edge brightened at +{ms} ms with no keystroke behind it ({first} → {edge})"
        );
    }
    assert!(
        edge_first.is_some_and(|e| e > 0),
        "the hot edge must have been lit for its law to have been tested"
    );
}

/// THE BODY KEEPS UNDER THE BOX'S TOP: typed on the grid's top row with no head band
/// over it (the Settings card's grid, a pane under a chrome strip), the body and its hot
/// edge, laying and through the exit swoosh, are laid inside the effects box with their
/// soft tops whole, under [`crate::cursor_glow::RISE_CLEAR_PX`], on dark and on paper.
/// The negative control (`rise_cut`): cut by the box's clip, the body stood flat on the
/// box's top row (settings round 2's four-row card, 40 to 211 px a frame).
#[test]
fn a_body_on_the_grids_top_row_keeps_under_the_boxs_top() {
    for dark in [true, false] {
        let c = cfg(dark, true);
        let on_top = |cut: bool| {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.rise_cut = cut;
            let keys = Keys {
                g: geom(),
                row: 0,
                col0: 10,
                n: 16,
                period_ms: 60,
                disp: 1.0,
            };
            type_keys(&mut rib, t0, keys, &c);
            let mut sink = Sink::default();
            let (mut drawn, mut top) = (0, 0);
            let ceiling = geom().rise_ceiling();
            for ms in (15 * 60..15 * 60 + 2_400).step_by(40) {
                let cx = ctx_in(at(t0, ms), &c, (0, 26), 1.0, geom());
                rib.plan(&cx);
                rib.emit(&cx, &mut sink.frame());
                for q in sink.under.iter().chain(&sink.out) {
                    // Additive quads (opacity 0) light by their colour alone.
                    if q.color | q.color2 == 0 && q.alpha == 0 && q.alpha2 == 0 {
                        continue;
                    }
                    drawn += 1;
                    if f32::from(q.y) < ceiling {
                        top += 1;
                    }
                }
            }
            (drawn, top)
        };
        let side = if dark { "dark" } else { "paper" };
        let (drawn, top) = on_top(false);
        assert!(drawn > 0, "{side}: the body draws");
        assert_eq!(
            top, 0,
            "{side}: {top} quads of the body on the box's top row"
        );
        let (_, cut) = on_top(true);
        assert!(
            cut > 0,
            "negative control ({side}): the box's clip cuts the body's top"
        );
    }
}

#[test]
fn the_exit_swoosh_retracts_toward_the_caret_and_reaches_exactly_zero() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.8);
    let last = at(t0, 7 * 60);
    let caret = (2u16, 18u16);
    let mut sink = Sink::default();
    let mut left_prev = f32::NEG_INFINITY;
    let mut edge_prev = u32::MAX;
    let mut edge_first: Option<u32> = None;
    let block_right = 19.0 * geom().cw as f32;
    let mut reach_prev = f32::INFINITY;
    let retract_start = RETRACT_START_S * 1000.0;
    let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
    for ms in ((retract_start as u64)..total + 20).step_by(10) {
        let now = at(last, ms);
        let cx = ctx(now, &c, caret, 0.0);
        rib.plan(&cx);
        let reach = rib
            .plan_segments()
            .iter()
            .map(|s| s.x)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            reach <= block_right + 0.5 && reach <= reach_prev + 0.5,
            "+{ms} ms: a slab at x {reach} (was {reach_prev}) is past the caret block's right \
             edge {block_right} or grew"
        );
        reach_prev = reach;
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let edge = peak_channel(&sink.out);
        edge_first.get_or_insert(edge);
        assert!(
            edge <= edge_prev.saturating_add(2),
            "the hot edge brightened inside the swoosh at +{ms} ms ({edge_prev} → {edge})"
        );
        edge_prev = edge;
        if ms == total - 90 {
            let first = edge_first.unwrap_or(0);
            assert!(
                !sink.under.is_empty() && edge * 4 < first,
                "mid-fade the hot edge must be well under way with its body ({first} → {edge})"
            );
        }
        if ms == total - 10 {
            assert!(
                !rib.cells().is_empty(),
                "the cells must still be in the pool for the pin to mean anything"
            );
            assert_eq!(
                edge, 0,
                "10 ms before the pool clears the hot edge must already be at exactly zero"
            );
        }
        let Some(left) = sink.under.iter().map(|q| f32::from(q.x)).reduce(f32::min) else {
            continue;
        };
        assert!(
            left + 1.0 >= left_prev,
            "the retract moved AWAY from the caret at +{ms} ms ({left_prev} → {left})"
        );
        left_prev = left;
    }
    assert!(
        edge_first.is_some_and(|e| e > 0),
        "the hot edge must have been lit at the retract's start for its law to have been tested"
    );
    let now = at(last, (SWOOSH_TOTAL_S * 1000.0) as u64 + 40);
    let cx = ctx(now, &c, caret, 0.0);
    rib.plan(&cx);
    let mut f = sink.frame();
    rib.emit(&cx, &mut f);
    assert!(
        sink.under.is_empty() && sink.out.is_empty(),
        "the swoosh must reach EXACTLY zero, not a residue"
    );
    assert!(rib.at_rest(), "and the pools must be empty with it");
}

/// THE DARK BAND, BYTE FOR BYTE, ACROSS ITS EXIT: every boundary bit and
/// every emitted quad of a busy dark band through typing, rest, Backspace
/// and the curtain hash to the values recorded at Prism Glass P0.11, as
/// re-recorded for §10.2 #3 (the rail's hairline keeps the judge's ring on
/// this 18 px row). A dark golden held through the light-only Prism Glass
/// phases (docs/design/PRISM-GLASS.md §10.3 #4): only a §10.2 change may
/// record new values (replaces the pruned
/// `a_busy_band_keeps_its_boundary_and_pixel_signature_across_its_exit`).
#[test]
fn a_dark_busy_band_keeps_its_signature_across_its_exit() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 30, &c, 0.9);
    let last = at(t0, 29 * 60);
    let mut signatures = Vec::new();
    let mut sink = Sink::default();
    for (offset, erase, curtain) in [
        (8u64, false, false),
        (100, false, false),
        (120, true, false),
        (180, false, true),
    ] {
        let now = at(last, offset);
        let caret = if erase || curtain { (2, 39) } else { (2, 40) };
        if erase {
            erase_at(&mut rib, now, caret, &c);
        }
        if curtain {
            rib.curtain(at(last, 140));
        }
        let cx = ctx(now, &c, caret, 0.9);
        rib.plan(&cx);
        if curtain {
            assert!(rib.brisk(now), "at +{offset} ms");
            assert_eq!(
                rib.next_change_deadline(now),
                Some(now + super::super::FRAME_CADENCE),
                "a moving band keeps its full frame cadence"
            );
        } else if offset == 100 {
            assert!(!rib.brisk(now));
            assert!(
                rib.next_change_deadline(now)
                    .is_some_and(|due| due >= now + super::super::TAIL_FLOOR),
                "the settled band uses the paced tail"
            );
        }
        let mut hash = aterm_hash::FNV64_OFFSET;
        let mut feed = |part: u64| hash = aterm_hash::fnv_mix_u64(hash, part);
        for seg in rib.plan_segments() {
            for part in [seg.x, seg.spine, seg.up, seg.dn, seg.t] {
                feed(u64::from(part.to_bits()));
            }
            feed(u64::from(seg.cov));
        }
        rib.emit(&cx, &mut sink.frame());
        for q in &sink.under {
            for part in [
                u64::from(q.row),
                u64::from(q.x),
                u64::from(q.y),
                u64::from(q.w),
                u64::from(q.h),
                u64::from(q.color),
                u64::from(q.alpha),
                u64::from(q.color2),
                u64::from(q.alpha2),
            ] {
                feed(part);
            }
        }
        signatures.push((rib.plan_segments().len(), sink.under.len(), hash));
    }
    assert_eq!(
        signatures,
        [
            (94, 2520, 2_871_221_491_103_572_636),
            (94, 2521, 1_139_705_768_875_531_907),
            (91, 2457, 6_051_614_497_633_733_274),
            (91, 2469, 2_216_295_461_641_169_251),
        ]
    );
}

#[test]
fn a_cohort_still_being_typed_keeps_its_boundaries_while_its_row_mate_retracts() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.8);
    let now = at(t0, 1000);
    let cx = ctx(now, &c, (2, 40), 0.8);
    rib.on_event(&pty((2, 18), (2, 40)), now, &cx);
    rib.plan(&cx);
    let keys = Keys {
        g: geom(),
        row: 2,
        col0: 40,
        n: 6,
        period_ms: 60,
        disp: 0.8,
    };
    type_keys(&mut rib, at(t0, 1000), keys, &c);
    let now = at(t0, 1320);
    let cx = ctx(now, &c, (2, 46), 0.8);
    rib.plan(&cx);
    let cw = geom().cw as f32;
    let (a, b): (Vec<&Segment>, Vec<&Segment>) =
        rib.plan_segments().iter().partition(|s| s.x < 30.0 * cw);
    assert!(
        !a.is_empty() && !b.is_empty(),
        "both cohorts must be planned"
    );
    let a_left = a.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
    assert!(
        a_left > 10.0 * cw + 0.5,
        "the abandoned cohort must be moving toward the caret (left edge {a_left})"
    );
    let b_left = b.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
    let b_right = b.iter().map(|s| s.x).fold(f32::NEG_INFINITY, f32::max);
    assert!(
        (b_left - 40.0 * cw).abs() < 0.5 && (b_right - 47.0 * cw).abs() < 0.5,
        "the cohort under the hand was squashed with its row-mate: [{b_left}, {b_right}]"
    );
}

#[test]
fn a_rewrite_retracts_the_suffix_from_the_given_column_not_the_mirror() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 20, &c, 0.8);
    let now = at(t0, 1300);
    let cx = ctx(now, &c, (2, 30), 0.8);
    rib.on_event(
        &Event::Rewrite {
            row: 2,
            col: 18,
            cells: 12,
        },
        now,
        &cx,
    );
    let retract_at = |col: u16| {
        rib.cells()
            .iter()
            .find(|cell| cell.row == 2 && cell.col == col)
            .unwrap_or_else(|| panic!("no cell at (2, {col})"))
            .retract_at
    };
    assert!(
        (18..30u16).all(|col| retract_at(col).is_some()),
        "every cell at or right of the rewrite's caret retracts"
    );
    assert!(
        (10..18u16).all(|col| retract_at(col).is_none()),
        "the cells left of it are untouched"
    );
    let span = kill_span_s(12);
    let far = retract_at(29).unwrap();
    let near = retract_at(18).unwrap();
    assert!(far < near, "farthest from the caret goes first");
    let near_s = near.saturating_duration_since(now).as_secs_f32();
    assert!(
        (near_s - span * 11.0 / 12.0).abs() < 1e-3,
        "the near cell goes last, at the kill's 12·n + 240 stagger: {near_s}"
    );
    assert_eq!(
        rib.caret(),
        Some((2, 18)),
        "the ribbon's caret moves to the rewrite"
    );
}

#[test]
fn under_reduced_motion_the_exit_swoosh_moves_nothing_and_only_fades() {
    let mut c = cfg(true, true);
    c.reduced_motion = true;
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.8);
    let last = at(t0, 7 * 60);
    let cw = geom().cw as f32;
    let mut sink = Sink::default();
    let mut frames = 0u32;
    let mut peak_prev = u8::MAX;
    let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
    for ms in ((PHRASE_REST_MIN_S * 1000.0) as u64..total + 20).step_by(10) {
        let now = at(last, ms);
        let cx = ctx(now, &c, (2, 18), 0.0);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        if sink.under.is_empty() {
            continue;
        }
        frames += 1;
        let left = sink
            .under
            .iter()
            .map(|q| f32::from(q.x))
            .fold(f32::INFINITY, f32::min);
        let right = sink
            .under
            .iter()
            .map(|q| f32::from(q.x) + f32::from(q.w))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (left - 10.0 * cw).abs() < 0.5 && (right - 19.0 * cw).abs() < 0.5,
            "a static mark moved at +{ms} ms: [{left}, {right}]"
        );
        let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
        assert!(
            peak <= peak_prev,
            "the one linear fade may only fall (+{ms} ms: {peak_prev} → {peak})"
        );
        peak_prev = peak;
    }
    assert!(
        frames > 40,
        "the mark must be visible through most of the swoosh window ({frames})"
    );
    assert!(rib.at_rest(), "…and be gone at its end");
}

#[test]
fn a_jump_takes_the_band_out_from_the_light_it_has_never_by_a_step() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 40, &c, 0.9);
    let last = at(t0, 39 * 60);
    let cx = ctx(at(last, 8), &c, (2, 42), 0.9);
    rib.plan(&cx);
    let before = band_sum(&rib, 42);
    let segs = rib.plan_segments().len() as u32;
    assert!(
        plan_peak(&rib) > 200,
        "the mark must be hot for a step to be measurable"
    );
    let now = at(last, 16);
    let cx = ctx(now, &c, (2, 80), 0.9);
    rib.on_event(&pty((2, 42), (2, 80)), now, &cx);
    rib.plan(&cx);
    let after = band_sum(&rib, 42);
    assert!(
        after + 2 * segs >= before,
        "the jump stepped the band down ({before} → {after} over {segs} boundaries) instead of retracting it from where it was"
    );
    let cx = ctx(at(last, 16 + 340), &c, (2, 80), 0.9);
    rib.plan(&cx);
    let mid = plan_sum(&rib);
    assert!(
        mid * 10 < before * 8,
        "the abandoned band must be well into its drain by mid-retract ({before} → {mid})"
    );
    let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
    rib.plan(&cx);
    assert!(
        rib.at_rest(),
        "the abandoned band must be out {RETRACT_DUR_S} + {RETRACT_FADE_S} s after the jump"
    );
}

#[test]
fn a_run_the_hand_leaves_is_feathered_at_its_head_end() {
    let level = |q: &GlowQuad| {
        if q.alpha > 0 {
            f32::from(q.alpha)
        } else {
            [16u32, 8, 0]
                .iter()
                .map(|&sh| ((q.color >> sh) & 0xff) as f32)
                .fold(0.0, f32::max)
        }
    };
    let peak = |qs: &[GlowQuad], x0: f32, x1: f32| {
        qs.iter()
            .filter(|q| f32::from(q.x) < x1 && f32::from(q.x + q.w) > x0)
            .map(level)
            .fold(0.0f32, f32::max)
    };
    let cw = geom().cw as f32;
    let head_x = 22.0 * cw;
    for (name, c) in [
        ("tall dark", cfg(true, true)),
        ("underline dark", cfg(true, false)),
        ("flat dark", flat(true, true)),
        ("tall light", cfg(false, true)),
    ] {
        let jump = |square: bool| {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.head_square = square;
            type_run(&mut rib, t0, 2, 20, &c, 0.9);
            let now = at(t0, 19 * 60 + 16);
            let cx = ctx(now, &c, (10, 2), 0.9);
            rib.on_event(&pty((2, 22), (10, 2)), now, &cx);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let end = (head_x - 0.25 * cw, head_x);
            (
                peak(&sink.under, end.0, end.1),
                peak(&sink.under, head_x - 4.0 * cw, head_x - 3.0 * cw),
                peak(&sink.out, end.0, end.1),
            )
        };
        let (end, body, edge) = jump(false);
        let (square_end, square_body, square_edge) = jump(true);
        assert!(
            body > 0.0 && square_body > 0.0,
            "{name}: fixture: the band is on glass on the jump frame"
        );
        assert!(
            end <= 0.4 * body,
            "{name}: the end the caret left stands at {end} against the body's {body}"
        );
        assert!(
            square_end >= 0.75 * square_body,
            "{name}: control: square-cut, the end stands at {square_end} against {square_body}"
        );
        if square_edge > 0.0 {
            assert!(
                edge <= 0.4 * square_edge,
                "{name}: the hot edge stands at {edge} at the end the caret left \
                 (square-cut {square_edge})"
            );
        }
    }
}

/// The trail gallery's glass: 72x14 cells of 14x26 px from (12, 2).
const fn gallery_geom() -> Geom {
    Geom {
        cw: 14,
        ch: 26,
        rows: 14,
        cols: 72,
        origin_x: 12,
        origin_y: 2,
        win_w: 1032,
        win_h: 378,
        head: 0,
    }
}

/// Under a filled cell on the row above the caret's (the gallery's yellow
/// `main +2` powerline segment), the hot edge held half its thickness
/// inside the row lights nothing above the row's top; centred on the top
/// (the negative control) it does.
#[test]
fn the_hot_edge_keeps_off_a_filled_cell_above() {
    let g = gallery_geom();
    let c = cfg(true, true);
    let (ox, oy, cw, ch) = (
        i32::from(g.origin_x),
        i32::from(g.origin_y),
        g.cw as i32,
        g.ch as i32,
    );
    let row_top = oy + 4 * ch;
    let bar = (ox + 13 * cw, ox + 22 * cw, row_top - ch, row_top);
    let above = |on_bar: bool| -> (usize, u32) {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.hot_edge_on_bar = on_bar;
        rib.keep_out.set([bar]);
        let keys = Keys {
            g,
            row: 4,
            col0: 2,
            n: 19,
            period_ms: 85,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let cx = ctx_in(at(t0, 18 * 85 + 20), &c, (4, 21), 0.9, g);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let lit = |q: &&GlowQuad| q.color & 0x00FF_FFFF != 0 || q.color2 & 0x00FF_FFFF != 0;
        let n = sink.out.iter().filter(lit).count();
        let over = sink
            .out
            .iter()
            .filter(lit)
            .filter(|q| {
                let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
                i32::from(q.y) < row_top && x0 < bar.1 && bar.0 < x1
            })
            .map(|q| peak_channel(std::slice::from_ref(q)))
            .max()
            .unwrap_or(0);
        (n, over)
    };
    let (n, over) = above(false);
    assert!(n > 0, "fixture: the hand leaves a hot edge");
    assert_eq!(
        over, 0,
        "the hot edge lights the segment's foot above the row by {over}"
    );
    let (n, over) = above(true);
    assert!(n > 0, "control fixture: the hand leaves a hot edge");
    assert!(
        over >= 40,
        "control: centred on the row's top the edge lights the segment's foot by only {over}"
    );
}

/// A run's tail on the gallery's 14 px cells under `c`, laid finely or, under `stair`, at the
/// stride: the largest step its light takes from one lit column to the next across its tail
/// cell, and the body's light four cells in.
fn tail_profile(c: &Config, stair: bool) -> (f32, f32) {
    let g = gallery_geom();
    let col0 = 3u16;
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    rib.tail_stair = stair;
    let keys = Keys {
        g,
        row: 4,
        col0,
        n: 16,
        period_ms: 60,
        disp: 0.6,
    };
    type_keys(&mut rib, t0, keys, c);
    let now = at(t0, 15 * 60 + 16);
    let cx = ctx_in(now, c, (4, col0 + 16), 0.6, g);
    rib.plan(&cx);
    let mut sink = Sink::default();
    {
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }
    let mut col = vec![0.0f32; usize::from(g.win_w)];
    for q in sink.under.iter().filter(|q| q.alpha > 0 || q.alpha2 > 0) {
        let w = f32::from(q.w.max(1));
        for i in 0..q.w {
            let t = if q.w > 1 {
                f32::from(i) / (w - 1.0)
            } else {
                0.0
            };
            let a = f32::from(q.alpha) + (f32::from(q.alpha2) - f32::from(q.alpha)) * t;
            let x = usize::from(q.x + i);
            col[x] = col[x].max(a);
        }
    }
    let ox = usize::from(g.origin_x);
    let tail = ox + usize::from(col0) * g.cw;
    let body = col[tail + 4 * g.cw];
    // From the run's first lit column (its outer boundary keeps a
    // tenth of the light, a soft start) across the tail cell.
    let first = (tail.saturating_sub(2)..tail + g.cw)
        .find(|&x| col[x] > 0.0)
        .unwrap_or(tail);
    let step = col[first..first + g.cw + 2]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    (step, body)
}

/// On the gallery's 14 px cells a run's tail light steps by no more than
/// an eighth of the body's from one lit column to the next, on dark and
/// on paper; laid at the stride (the negative control) it steps by a fifth
/// or more.
#[test]
fn the_run_s_tail_eases_in_without_a_stair() {
    for dark in [true, false] {
        let c = cfg(dark, true);
        let (step, body) = tail_profile(&c, false);
        assert!(
            body > 40.0,
            "fixture (dark {dark}): the body is lit ({body})"
        );
        assert!(
            step <= body / 8.0,
            "dark {dark}: the tail steps by {step} in a column against the body's {body}"
        );
        let (stair, body) = tail_profile(&c, true);
        assert!(
            stair >= body / 5.0,
            "control (dark {dark}): at the stride the tail steps only {stair} against {body}"
        );
    }
}

/// The glass's oldest end (`docs/design/PRISM-GLASS.md` §2.2 The oldest end, §15.2 1.8): on a
/// glass frame the run's height eases in from its oldest end by smooth01(d /
/// [`GLASS_TAIL_CELLS`]), from a pixel or two at its first lit column to the body's over the
/// ease, half way at half of it, with no stair (no column a step of more than two pixels
/// above its neighbour, none lower than its neighbour by more than one, the wave's swing),
/// and at whole opacity wherever it stands taller than its melt: the tail is the glass's
/// height. The negative control is main's paper on the same keys, whose tail is its
/// opacity's ([`RUN_TAIL_EASE`]): its first lit column holds under half its body's light.
#[test]
fn the_glass_run_s_tail_eases_in_by_height_without_a_stair() {
    let g = gallery_geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let col0 = 3u16;
    let lay = |c: &Config| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 4,
            col0,
            n: 16,
            period_ms: 60,
            disp: 0.6,
        };
        type_keys(&mut rib, t0, keys, c);
        let now = at(t0, 15 * 60 + 16);
        let cx = ctx_in(now, c, (4, col0 + 16), 0.6, g);
        rib.plan(&cx);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        sink.under
    };
    let row_bottom = i32::from(g.origin_y) + 5 * ch;
    let x0 = i32::from(g.origin_x) + i32::from(col0) * cw;
    let span = x0 - 2..x0 + 4 * cw;
    let census = |under: &[GlowQuad], floor: u8| {
        column_census(
            under,
            span.clone(),
            row_bottom - 2 * ch..row_bottom + ch,
            floor,
        )
    };
    let lit = glass_cfg(cfg(false, true));
    let under = lay(&lit);
    let heights: Vec<i32> = census(&under, 128)
        .iter()
        .map(|c| c.map_or(0, |(top, _, _)| row_bottom - top))
        .collect();
    let peaks: Vec<u8> = census(&under, 1)
        .iter()
        .map(|c| c.map_or(0, |(_, _, peak)| peak))
        .collect();
    let at_cells = |d: f32| (2.0 + d * cw as f32).round() as usize;
    let body = heights[at_cells(3.5)];
    let body_peak = peaks[at_cells(3.5)];
    assert!(
        (body as f32 - GLASS_TAIL_UP_CH * ch as f32).abs() <= 2.0 && body_peak >= 224,
        "fixture: the glass's body stands {body} px at {body_peak}"
    );
    let first = heights
        .iter()
        .position(|&h| h > 0)
        .expect("the tail is laid");
    assert!(
        heights[first] <= 3,
        "the tail starts {} px tall",
        heights[first]
    );
    let half = heights[at_cells(GLASS_TAIL_CELLS * 0.5)];
    assert!(
        (half - body / 2).abs() <= 3,
        "half way along the ease the glass stands {half} px of the body's {body}"
    );
    let eased = heights[at_cells(GLASS_TAIL_CELLS) + 2];
    assert!(
        eased >= body - 1,
        "past the ease the glass stands {eased} px of the body's {body}"
    );
    for (i, w) in heights[first..at_cells(3.5)].windows(2).enumerate() {
        assert!(
            w[1] - w[0] <= 2 && w[0] - w[1] <= 1,
            "column {}: the tail steps from {} to {} px",
            first + i,
            w[0],
            w[1]
        );
    }
    for (i, (&h, &peak)) in heights.iter().zip(&peaks).enumerate() {
        if h > GLASS_EDGE_PX.ceil() as i32 + 1 {
            assert!(
                peak.abs_diff(body_peak) <= 1,
                "column {i}: the tail's glass stands {h} px at {peak}, not whole ({body_peak})"
            );
        }
    }

    let paper = lay(&paper_cfg(cfg(false, true)));
    let paper_peaks: Vec<u8> = census(&paper, 1)
        .iter()
        .map(|c| c.map_or(0, |(_, _, peak)| peak))
        .collect();
    let paper_first = paper_peaks
        .iter()
        .position(|&p| p > 0)
        .expect("control: main's tail is laid");
    let paper_body = paper_peaks[at_cells(3.5)];
    assert!(
        u32::from(paper_peaks[paper_first]) * 2 < u32::from(paper_body),
        "control: main's tail starts at {} of its body's {paper_body}",
        paper_peaks[paper_first]
    );
}

/// One frame's plan read for an exit: its highest coverage and its tallest glass, px.
fn plan_peaks(rib: &Ribbon) -> (u8, f32) {
    let segs = rib.plan_segments();
    (
        segs.iter().map(|s| s.cov).max().unwrap_or(0),
        segs.iter().map(|s| s.glass_up).fold(0.0, f32::max),
    )
}

/// The exits [`the_glass_leaves_by_height`] drives, each from the settled band of
/// [`type_run`]'s 12 keys on row 2: its name, the event that starts it (none for the swoosh
/// and melt the band leaves by when the hand rests), the caret it plans under, and how long
/// its frames run, ms.
const EXITS: [(&str, u64); 4] = [
    ("swoosh and melt", 30_000),
    ("retract", 1_000),
    ("ember", 400),
    ("curtain", (CURTAIN_S * 1000.0) as u64 + 48),
];

/// Exit `which` of [`EXITS`] on `c`: the settled plan's peaks and every 16 ms frame's from
/// the exit's start ([`plan_peaks`]), until the ribbon rests or the exit's time is out.
fn exit_run(c: &Config, which: usize) -> ((u8, f32), Vec<(u8, f32)>) {
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 12, c, 0.9);
    let start = at(t0, 11 * 60 + 100);
    let cx = ctx(start, c, (2, 22), 0.9);
    rib.plan(&cx);
    let settled = plan_peaks(&rib);
    let caret = match which {
        1 => {
            rib.on_event(&pty((2, 22), (2, 80)), start, &cx);
            (2, 80)
        }
        2 => {
            rib.on_event(&Event::Focus(false), start, &cx);
            (2, 22)
        }
        3 => {
            rib.curtain(start);
            (3, 0)
        }
        _ => (2, 22),
    };
    let mut frames = Vec::new();
    for k in 1..=EXITS[which].1 / 16 {
        let cx = ctx(at(start, k * 16), c, caret, 0.9);
        rib.plan(&cx);
        frames.push(plan_peaks(&rib));
        if rib.at_rest() {
            break;
        }
    }
    (settled, frames)
}

/// EXITS BY HEIGHT (`docs/design/PRISM-GLASS.md` §2.2 Exits by height, §15.2 1.8): on a glass
/// frame the swoosh and the drawn melt the band leaves by when the hand rests, a jump's
/// retract, the focus ember and the curtain each take the glass down by its height: through
/// every frame of each the glass's opacity is the settled band's whole while any of it
/// stands, and its height never rises, falls under half of the band's, and ends at nothing
/// (the ember burning at its floor, a glass still drawn, [`GLASS_MIN_PX`] or more, until the
/// band goes out whole). Under reduced motion it is the other way round: the exits are the
/// opacity's fade (§2.2's fourth place) and the height stands, but for the focus ember, which
/// takes the height to its floor at whole opacity there too, so an unfocused window never
/// holds a translucent pastel glass. The negative control is main's paper on the same exits,
/// whose coverage falls.
#[test]
fn the_glass_leaves_by_height() {
    let lit = glass_cfg(cfg(false, true));
    let paper = paper_cfg(cfg(false, true));
    for (which, &(name, _)) in EXITS.iter().enumerate() {
        let ((cov, up), frames) = exit_run(&lit, which);
        assert!(
            cov >= 224 && up > 0.8 * 18.0,
            "{name}: fixture: the glass stands {up} px at {cov}"
        );
        let mut prev = up;
        let mut least = up;
        for (k, &(c, u)) in frames.iter().enumerate() {
            if u > GLASS_EDGE_PX {
                assert_eq!(
                    c, cov,
                    "{name}: frame {k}: the glass stands {u} px at {c}, not its whole {cov}"
                );
            }
            assert!(
                u <= prev + 0.01,
                "{name}: frame {k}: the glass rises from {prev} to {u} px"
            );
            prev = u;
            least = least.min(u);
        }
        assert!(
            least < 0.5 * up,
            "{name}: the glass falls only to {least} of its {up} px"
        );
        if name == "ember" {
            let burning: Vec<f32> = frames.iter().filter(|f| f.0 > 0).map(|f| f.1).collect();
            assert!(
                !burning.is_empty() && burning.iter().all(|&u| u >= GLASS_MIN_PX),
                "the ember takes the glass under its least: {burning:?}"
            );
        }
        assert!(prev < GLASS_MIN_PX, "{name}: the glass ends {prev} px tall");

        let ((paper_cov, _), paper_frames) = exit_run(&paper, which);
        let paper_least = paper_frames.iter().map(|f| f.0).min().unwrap_or(0);
        assert!(
            u32::from(paper_least) * 5 < u32::from(paper_cov) * 3,
            "control: {name}: main's paper keeps {paper_least} of its {paper_cov}"
        );
    }

    let reduced = Config {
        reduced_motion: true,
        ..lit
    };
    for which in [1usize, 3] {
        let name = EXITS[which].0;
        let ((cov, up), frames) = exit_run(&reduced, which);
        let laid: Vec<_> = frames.iter().filter(|f| f.0 > 0).collect();
        assert!(
            !laid.is_empty() && laid.iter().all(|f| (f.1 - up).abs() < 0.01),
            "reduced {name}: the glass's height moves ({up} px)"
        );
        let least = frames.iter().map(|f| f.0).min().unwrap_or(0);
        assert!(
            u32::from(least) * 2 < u32::from(cov),
            "reduced {name}: the glass's opacity keeps {least} of {cov}"
        );
    }
    let ((cov, up), frames) = exit_run(&reduced, 2);
    let laid: Vec<_> = frames.iter().filter(|f| f.0 > 0).collect();
    assert!(
        !laid.is_empty() && laid.iter().all(|f| f.0 == cov),
        "reduced ember: the glass's opacity moves from {cov}: {frames:?}"
    );
    let floor = laid.iter().map(|f| f.1).fold(up, f32::min);
    assert!(
        floor < 0.5 * up && laid.iter().all(|f| f.1 >= GLASS_MIN_PX),
        "reduced ember: the glass stands {floor} of its {up} px: {frames:?}"
    );
}

/// One frame of a twin run: the ribbon's state words and its bed's, and every live cell's
/// envelope as main folds it (`env_of`), by its bits.
type TwinFrame = (Vec<crate::state_words::Word>, Vec<u32>);

/// Exit `which` of [`EXITS`] on `c` as [`exit_run`] runs it, its typing included, each frame
/// planned and emitted: every frame's [`TwinFrame`]. With `leak`, main's ember floor reads
/// the glass's colour floor (`Ribbon::ember_reads_glass`).
fn twin_run(c: &Config, which: usize, leak: bool) -> Vec<TwinFrame> {
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    rib.ember_reads_glass = leak;
    let mut frames = Vec::new();
    let mut lay = |rib: &mut Ribbon, cx: &Ctx<'_>| {
        rib.plan(cx);
        let mut sink = Sink::default();
        rib.emit(cx, &mut sink.frame());
        let mut words = crate::state_words::StateWords::new(t0);
        rib.state_words(&mut words);
        rib.bed_ink_lut().state_words(&mut words);
        let envs = rib
            .cells
            .iter()
            .map(|cell| rib.debug_cell_envelope(cx, cell).to_bits())
            .collect();
        frames.push((words.into_words(), envs));
    };
    for i in 0..12u16 {
        let now = at(t0, u64::from(i) * 60);
        let cx = ctx(now, c, (2, 10 + i + 1), 0.9);
        rib.on_event(&typed(), now, &cx);
        lay(&mut rib, &cx);
    }
    let start = at(t0, 11 * 60 + 100);
    let cx = ctx(start, c, (2, 22), 0.9);
    lay(&mut rib, &cx);
    let caret = match which {
        1 => {
            rib.on_event(&pty((2, 22), (2, 80)), start, &cx);
            (2, 80)
        }
        2 => {
            rib.on_event(&Event::Focus(false), start, &cx);
            (2, 22)
        }
        3 => {
            rib.curtain(start);
            (3, 0)
        }
        _ => (2, 22),
    };
    for k in 1..=EXITS[which].1 / 16 {
        let cx = ctx(at(start, k * 16), c, caret, 0.9);
        lay(&mut rib, &cx);
        if rib.at_rest() {
            break;
        }
    }
    frames
}

/// The first frame two twin runs part at, and what parts them: `None` when every frame is
/// the same.
fn twin_parts(a: &[TwinFrame], b: &[TwinFrame]) -> Option<String> {
    if a.len() != b.len() {
        return Some(format!("{} frames beside {}", a.len(), b.len()));
    }
    a.iter().zip(b).enumerate().find_map(|(k, (x, y))| {
        if let Some(n) = (0..x.0.len().max(y.0.len())).find(|&n| x.0.get(n) != y.0.get(n)) {
            return Some(format!(
                "frame {k}: word {n}: {:?} beside {:?}",
                x.0.get(n),
                y.0.get(n)
            ));
        }
        (x.1 != y.1).then(|| format!("frame {k}: envelopes {:?} beside {:?}", x.1, y.1))
    })
}

/// The glass draws main's ribbon (`docs/design/PRISM-GLASS.md` §15.2 1.8, §15.0.4): typing,
/// then each exit of [`EXITS`] (the swoosh and melt, a jump's retract, the focus ember, the
/// curtain), under full and reduced motion, a glass frame leaves every state word, the bed's
/// main table and every cell's envelope as main folds it (`env_of`) exactly as its paper twin
/// does (main's light on the same page). The negative control: main's ember floor read off
/// the glass's colour floor, the 1.5 leak `main_seen_floor_at` closed, parts the ember's
/// frames from its twin's, and leaves main's paper, which has no glass, as it was.
#[test]
fn a_glass_ribbon_folds_its_paper_twins_state_through_every_exit() {
    let lit = glass_cfg(cfg(false, true));
    let paper = paper_cfg(cfg(false, true));
    let reduced = |c: Config| Config {
        reduced_motion: true,
        ..c
    };
    for (motion, lit, paper) in [
        ("full", lit, paper),
        ("reduced", reduced(lit), reduced(paper)),
    ] {
        for (which, &(name, _)) in EXITS.iter().enumerate() {
            let glass = twin_run(&lit, which, false);
            assert!(glass.len() > 13, "{motion} {name}: fixture: the exit ran");
            assert_eq!(
                twin_parts(&glass, &twin_run(&paper, which, false)),
                None,
                "{motion} {name}: the glass frame is its paper twin's"
            );
        }
    }
    let ember = 2;
    assert!(
        twin_parts(
            &twin_run(&lit, ember, true),
            &twin_run(&paper, ember, false)
        )
        .is_some(),
        "control: an ember floor read off the glass parts the twins"
    );
    assert_eq!(
        twin_parts(
            &twin_run(&paper, ember, true),
            &twin_run(&paper, ember, false)
        ),
        None,
        "control: on main's paper the glass's floor is main's"
    );
}

/// A Retina geometry at §2's 2x (`docs/design/PRISM-GLASS.md` §2: 28 px cells 15 wide), 240
/// columns: room for a line past the glass's one-slab density.
const fn geom_retina() -> Geom {
    Geom {
        cw: 15,
        ch: 28,
        rows: 40,
        cols: 240,
        origin_x: 0,
        origin_y: 0,
        win_w: 3600,
        win_h: 1120,
        head: 0,
    }
}

/// Whether `under` lays light anywhere over cell `(row, col)` of `g`: a glass's oldest end
/// stands lower than [`cell_lit`]'s top half.
fn row_lit(under: &[GlowQuad], g: Geom, row: u16, col: u16) -> bool {
    let x0 = i32::from(g.origin_x) + i32::from(col) * g.cw as i32;
    let y0 = i32::from(g.origin_y) + i32::from(row) * g.ch as i32;
    column_census(under, x0..x0 + g.cw as i32, y0..y0 + g.ch as i32, 1)
        .iter()
        .any(Option::is_some)
}

/// The lights a frame laid under the text: its quads and its ramps' lit rows.
fn lights(sink: &Sink) -> usize {
    sink.under_ramps.laid(sink.under.len())
}

/// THE GLASS'S PRICE (`docs/design/PRISM-GLASS.md` §2.2 Quad budget, §15.2 1.8): at §2's 28 px
/// a glass slab prices some 36 rows ([`glass_slab_rows`]: its reach and the lip's reserve),
/// and a glass line takes the densest slabs its price allows to some 91 cells for three and
/// some 137 for two ([`Ribbon::glass_slabs_for`]), a whole number to each of the plan's
/// vertices, which stay main's ([`Ribbon::glass_slabs_per_cell`]): three while main's plan
/// lays three, two to 136 cells, one past 141. Every line's frame lays no more lights than
/// its price less the lip's reserve, so its wedges ([`glass_tail_wedge_lights`]) are paid
/// for and the lip a later phase lays fits; the price stays within [`RIBBON_QUAD_BUDGET`]
/// wherever the density is more than one, and no frame lays past the budget. A paragraph's
/// frames keep within it too, the hand's row whole. The negative control is main's paper on
/// the same lines, which prices its paper rows (its body, its reach below the row and the
/// rail) and so gives up its density first.
#[test]
fn a_glass_frame_stays_within_the_ribbon_quad_budget() {
    let g = geom_retina();
    let ch = g.ch as f32;
    assert_eq!(
        glass_slab_rows(ch),
        36.0,
        "§2.2: 36 rows a glass slab at 28 px"
    );
    let lit = glass_cfg(cfg(false, true));
    let paper = paper_cfg(cfg(false, true));
    let line = |c: &Config, n: u16| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 2,
            n,
            period_ms: 5,
            disp: 1.0,
        };
        type_keys(&mut rib, t0, keys, c);
        let cx = ctx_in(at(t0, u64::from(n) * 5 + 16), c, (2, 2 + n), 1.0, g);
        rib.plan(&cx);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        (rib, sink)
    };
    let lip = (GLASS_LIP_RESERVE_CH * ch).ceil();
    for n in [
        1u16, 2, 4, 8, 16, 30, 50, 70, 85, 90, 95, 110, 130, 136, 142, 160, 200, 230,
    ] {
        let (rib, sink) = line(&lit, n);
        assert!(rib.ink.glass_lit(), "fixture: a glass frame");
        let slabs = rib.glass_slabs_per_cell();
        let plan = rib.slabs_per_cell();
        let priced = Ribbon::glass_slabs_for(
            &ctx_in(Instant::now(), &lit, (2, 2), 1.0, g),
            usize::from(n),
            1,
        );
        assert_eq!(
            slabs,
            plan * (priced / plan).max(1),
            "{n} cells: the glass lays {slabs} a cell over main's {plan} vertices, priced {priced}"
        );
        let laid = lights(&sink);
        let price = glass_frame_price(ch, usize::from(n), 1, slabs);
        let reserve = f32::from(n + 1) * slabs as f32 * lip;
        assert!(
            laid as f32 + reserve <= price,
            "{n} cells at {slabs} a cell: {laid} lights and the lip's {reserve} pass the price {price}"
        );
        assert!(laid <= RIBBON_QUAD_BUDGET, "{n} cells lay {laid} lights");
        assert!(
            slabs == 1 || price <= RIBBON_QUAD_BUDGET as f32,
            "{n} cells at {slabs} a cell price {price}"
        );
        let want = match n {
            0..=50 => 3,
            51..=136 => 2,
            _ => 1,
        };
        assert_eq!(slabs, want, "{n} cells take {slabs} slabs a cell");
        for col in 2..2 + n {
            assert!(
                row_lit(&sink.under, g, 2, col),
                "{n} cells: col {col} is dark"
            );
        }
        if n == 110 {
            let (main, _) = line(&paper, n);
            assert!(
                main.slabs_per_cell() < slabs && main.glass_slabs_per_cell() == 0,
                "control: main's paper takes {} slabs a cell over {n} cells",
                main.slabs_per_cell()
            );
        }
    }

    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let mut sink = Sink::default();
    let mut ms = 0u64;
    for row in 2..6u16 {
        let keys = Keys {
            g,
            row,
            col0: 2,
            n: 76,
            period_ms: 5,
            disp: 1.0,
        };
        type_keys(&mut rib, at(t0, ms), keys, &lit);
        ms += 76 * 5;
        let cx = ctx_in(at(t0, ms), &lit, (row, 78), 1.0, g);
        rib.plan(&cx);
        rib.emit(&cx, &mut sink.frame());
        let laid = lights(&sink);
        assert!(
            laid <= RIBBON_QUAD_BUDGET,
            "{} rows lay {laid} lights",
            row - 1
        );
        for col in 2..78u16 {
            assert!(
                row_lit(&sink.under, g, row, col),
                "the hand's row {row} is dark at col {col}"
            );
        }
    }
}

/// A SETTLED GLASS IDLES TO EXACTLY ZERO (`docs/design/PRISM-GLASS.md` §15.2 1.8): woken only
/// at its own deadlines ([`Ribbon::next_change_deadline`]), the settled band of
/// [`type_run`]'s 12 keys, its phrase held past its life so its drawn melt alone takes it,
/// wakes not once until the melt moves it a pixel; it then steps down by its height (at
/// most two wakes a pixel of its reach, its cells stepping at their own instants), its
/// opacity whole and its height never rising, until it lays nothing at all, books no
/// deadline and rests: no faint glass and no wake is left. The negative control is the same
/// glass stepped by main's coverage levels, which wakes before it moves a pixel and half
/// again as often.
#[test]
fn a_settled_glass_idles_to_exactly_zero() {
    let lit = glass_cfg(cfg(false, true));
    let idle = |level_steps: bool| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.glass_level_steps = level_steps;
        type_run(&mut rib, t0, 10, 12, &lit, 0.9);
        for coh in &mut rib.cohorts {
            coh.grace_s = 1000.0;
        }
        for cell in &mut rib.cells {
            cell.life_s = 8.0;
        }
        let settled = at(t0, 11 * 60 + 100);
        let mut now = settled;
        let mut sink = Sink::default();
        let mut wakes = Vec::new();
        let mut first = None;
        loop {
            let cx = ctx(now, &lit, (2, 22), 0.9);
            rib.plan(&cx);
            rib.emit(&cx, &mut sink.frame());
            let (cov, up) = plan_peaks(&rib);
            if let Some(&(_, before)) = wakes.last() {
                assert!(
                    up <= before + 0.01,
                    "wake {}: the glass rises from {before} to {up} px",
                    wakes.len()
                );
            }
            assert!(
                cov == 0 || cov >= 250,
                "wake {}: the glass at {cov}",
                wakes.len()
            );
            if now > settled && first.is_none() {
                first = Some(up);
            }
            wakes.push((now, up));
            let Some(next) = rib.next_change_deadline(now) else {
                break;
            };
            assert!(wakes.len() < 100_000, "the glass never settles");
            now = next.max(now + Duration::from_millis(1));
        }
        (rib, sink, wakes, first.unwrap_or(0.0))
    };
    let (rib, sink, wakes, first) = idle(false);
    let reach = wakes[0].1;
    let now = wakes[wakes.len() - 1].0;
    assert!(reach > 0.8 * 18.0, "fixture: the glass stands {reach} px");
    assert!(
        reach - first >= 0.5,
        "the glass's first wake moves it {} px",
        reach - first
    );
    assert!(
        sink.under.is_empty() && !rib.glass_drew_at(now),
        "the settled glass still lays {} quads",
        sink.under.len()
    );
    assert!(rib.at_rest(), "the settled glass holds cells");
    let steps = wakes.len() - 1;
    assert!(
        steps > 0 && steps as f32 <= 2.0 * reach.ceil(),
        "the glass wakes {steps} times to leave {reach} px"
    );
    let (_, _, level_wakes, level_first) = idle(true);
    assert!(
        reach - level_first < 0.5 && 2 * (level_wakes.len() - 1) >= 3 * steps,
        "control: main's level steps wake the glass {} times, first by {} px, against {steps}",
        level_wakes.len() - 1,
        reach - level_first
    );
}

/// [`type_run`]'s 12 keys along row 2 from column 10, then the hand wrapped to row 3 and 4
/// more keys there from column 0, 60 ms apart, on `c`: §2.4's wrapped composer. The ribbon,
/// and the instant the hand left row 2 (its first key on row 3).
fn wrapped(c: &Config, t0: Instant) -> (Ribbon, Instant) {
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 12, c, 0.9);
    let wrap = at(t0, 12 * 60);
    let keys = Keys {
        g: geom(),
        row: 3,
        col0: 0,
        n: 4,
        period_ms: 60,
        disp: 0.9,
    };
    type_keys(&mut rib, wrap, keys, c);
    (rib, wrap)
}

/// The caret after [`wrapped`]'s last key.
const WRAPPED_CARET: (u16, u16) = (3, 4);

/// A frame of `rib` at `now` with the hand at `caret` on `c`: what it laid under the text.
fn glass_frame(rib: &mut Ribbon, c: &Config, now: Instant, caret: (u16, u16)) -> Vec<GlowQuad> {
    let cx = ctx(now, c, caret, 0.9);
    rib.plan(&cx);
    let mut sink = Sink::default();
    rib.emit(&cx, &mut sink.frame());
    sink.under
}

/// The last plan's boundaries on `row`: each one's x, glass reach and coverage.
fn row_glass(rib: &Ribbon, row: u16) -> Vec<(f32, f32, u8)> {
    rib.runs
        .iter()
        .filter(|r| r.row == row)
        .flat_map(|r| {
            rib.plan[r.lo..r.hi]
                .iter()
                .map(|s| (s.x, s.glass_up, s.cov))
        })
        .collect()
}

/// THE WRAP'S SINK (§2.4, §15.2 1.10): on a glass frame of a wrapped composer, the row the hand
/// has left is recorded left ([`GlassWrap`]) on the frame its first key lands below, and its
/// glass sinks by height, up × lerp(1, min(1, [`WRAP_SINK_CH`] / up), smooth01((now −
/// left_at) / [`WRAP_SINK_S`])), half way at 0.2 s and to 0.34 ch from 0.4 s on, its
/// opacity untouched and its top a pixel row of the low glass; the hand's row is not sunk.
/// Negative control: the same frames with no sink keep the left row's glass whole.
#[test]
fn a_wrapped_row_the_hand_left_sinks_to_its_low_glass_by_height() {
    let c = glass_cfg(cfg(false, true));
    let t0 = Instant::now();
    let (mut rib, wrap) = wrapped(&c, t0);
    let (mut whole, _) = wrapped(&c, t0);
    whole.wrap_unsunk = true;
    assert!(rib.ink.glass_lit(), "fixture: the bed draws by its glass");
    assert_eq!(
        rib.structural_rows(3),
        (2, 3),
        "fixture: one phrase over two rows"
    );
    let left = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 2)
        .expect("row 2's cohort");
    assert_eq!(
        rib.glass_wraps(),
        [GlassWrap {
            cohort: left.id,
            row: 2,
            left_at: wrap,
        }],
        "the row is left when the hand's first key lands below it"
    );
    let chf = geom().ch as f32;
    let low = WRAP_SINK_CH * chf;
    // Three cells in from the run's oldest end, past its tail's ease.
    let body = (10.0 + 3.0) * geom().cw as f32;
    for ms in [200u64, 300, 400, 600] {
        let now = at(wrap, ms);
        let under = glass_frame(&mut rib, &c, now, WRAPPED_CARET);
        let _ = glass_frame(&mut whole, &c, now, WRAPPED_CARET);
        let sunk = smoothstep01(ms as f32 / 1000.0 / WRAP_SINK_S);
        let (laid, full) = (row_glass(&rib, 2), row_glass(&whole, 2));
        assert!(
            !laid.is_empty() && laid.len() == full.len(),
            "+{ms} ms: fixture: row 2 planned alike"
        );
        for (&(x, up, cov), &(_, reach, reach_cov)) in laid.iter().zip(&full) {
            assert_eq!(
                cov, reach_cov,
                "+{ms} ms, x {x}: the sink leaves the opacity"
            );
            if x < body {
                continue;
            }
            let want = reach * (1.0 + ((low / reach).min(1.0) - 1.0) * sunk);
            assert!(
                (up - want).abs() < 1e-3,
                "+{ms} ms, x {x}: the left row's glass {up} px, not {want} of {reach}"
            );
        }
        assert!(
            full.iter()
                .any(|&(x, reach, _)| x >= body && reach > 0.8 * chf),
            "control: unsunk, the left row keeps its glass"
        );
        assert_eq!(
            row_glass(&rib, 3),
            row_glass(&whole, 3),
            "+{ms} ms: the hand's row is not sunk"
        );
        if ms >= 400 {
            let top = |floor: u8| {
                column_census(
                    &under,
                    body as i32..(21 * geom().cw) as i32,
                    0..ROW2_BOTTOM,
                    floor,
                )
                .into_iter()
                .flatten()
                .map(|(top, _, _)| top)
                .min()
                .expect("the low glass lays")
            };
            let reach = ROW2_BOTTOM as f32 - low;
            let (half, lit) = (top(128), top(1));
            assert!(
                (reach.floor() as i32..=(reach + GLASS_EDGE_PX).ceil() as i32).contains(&half),
                "+{ms} ms: the low glass is half opaque from y {half}, not {WRAP_SINK_CH} ch tall"
            );
            assert!(
                lit >= reach.floor() as i32,
                "+{ms} ms: the low glass lays from y {lit}, above its {WRAP_SINK_CH} ch"
            );
        }
    }
}

/// THE LEFT ROW'S STREAK (the after-land pass's R3): once a wrapped row the hand has left has
/// sunk ([`a_wrapped_row_the_hand_left_sinks_to_its_low_glass_by_height`]), its exits fade
/// the glass's opacity at the sunk height, so the row retires as a streak along its foot, as
/// the dark ribbon's retiring row does: on every frame it stands, its body reaches nine
/// tenths of [`WRAP_SINK_CH`] or more, and its opacity falls under half of its whole before
/// it goes.
/// Negative control: [`with_left_rows_shrinking`] melts it by height, as 1.12 did, and its
/// body falls under half the sunk height while its opacity stays whole: the hairline.
#[test]
fn a_wrapped_row_the_hand_left_retires_as_a_streak_along_its_foot() {
    let c = glass_cfg(cfg(false, true));
    let t0 = Instant::now();
    let chf = geom().ch as f32;
    let low = WRAP_SINK_CH * chf;
    let body = (10.0 + 3.0) * geom().cw as f32;
    // Each frame's left-row body: its most reach (its oldest end tapers, and eats in as it
    // retires) and its most coverage, while it lays.
    let frames = || {
        let (mut rib, wrap) = wrapped(&c, t0);
        let mut out = Vec::new();
        for ms in (400u64..6000).step_by(40) {
            let _ = glass_frame(&mut rib, &c, at(wrap, ms), WRAPPED_CARET);
            let laid: Vec<(f32, f32, u8)> = row_glass(&rib, 2)
                .into_iter()
                .filter(|&(x, up, cov)| x >= body && up > 0.0 && cov > 0)
                .collect();
            if laid.is_empty() {
                break;
            }
            let up = laid.iter().map(|l| l.1).fold(0.0, f32::max);
            let cov = laid.iter().map(|l| l.2).max().unwrap_or(0);
            out.push((ms, up, cov));
        }
        out
    };
    let streak = frames();
    assert!(
        streak.len() > 3,
        "fixture: the left row stands for several frames: {streak:?}"
    );
    let whole = streak[0].2;
    assert!(
        streak.iter().all(|&(_, up, _)| up >= 0.9 * low),
        "the left row keeps its sunk height through its exit: {streak:?}"
    );
    assert!(
        streak
            .iter()
            .any(|&(_, _, cov)| u32::from(cov) * 2 < u32::from(whole)),
        "the left row's opacity fades out: {streak:?}"
    );
    let hairline = with_left_rows_shrinking(frames);
    assert!(
        hairline.iter().any(|&(_, up, _)| up < 0.5 * low)
            && hairline.iter().all(|&(_, _, cov)| cov == hairline[0].2),
        "control: melted by height, the left row shrinks at whole opacity: {hairline:?}"
    );
}

/// THE HAND-ROW INSET (§2.4, §15.2 1.10): on a multi-row structural set the hand's row keeps
/// [`WRAP_HAND_TOP_PX`] of page above its glass: its reach is at most a cell less that, and
/// nothing is laid in its top 3 px. Negative control: a single-row composer's hand reaches
/// into its row's top 3 px.
#[test]
fn the_hand_row_of_a_multi_row_band_keeps_3_px_of_page_above_its_glass() {
    let c = glass_cfg(cfg(false, true));
    let t0 = Instant::now();
    let chf = geom().ch as f32;
    let (mut rib, wrap) = wrapped(&c, t0);
    let under = glass_frame(&mut rib, &c, at(wrap, 3 * 60 + 16), WRAPPED_CARET);
    assert_eq!(rib.structural_rows(3), (2, 3), "fixture: a multi-row set");
    let reach = row_glass(&rib, 3)
        .iter()
        .map(|&(_, up, _)| up)
        .fold(0.0, f32::max);
    assert!(
        (reach - (chf - WRAP_HAND_TOP_PX)).abs() < 1e-3,
        "the hand's glass reaches {reach} px of a {chf} px cell"
    );
    let row3_top = 3 * geom().ch as u16;
    let hand: Vec<&GlowQuad> = under.iter().filter(|q| q.row == 3).collect();
    assert!(!hand.is_empty(), "fixture: the hand's row lays glass");
    assert!(
        hand.iter()
            .all(|q| q.y >= row3_top + WRAP_HAND_TOP_PX as u16),
        "glass in the hand's row's top {WRAP_HAND_TOP_PX} px"
    );

    let mut single = Ribbon::new();
    type_run(&mut single, t0, 10, 12, &c, 0.9);
    let under = glass_frame(&mut single, &c, at(t0, 11 * 60 + 16), (2, 22));
    assert_eq!(single.structural_rows(2), (2, 2), "control: one row");
    let row2_top = 2 * geom().ch as u16;
    assert!(
        under
            .iter()
            .any(|q| q.row == 2 && q.y < row2_top + WRAP_HAND_TOP_PX as u16),
        "control: a single row's hand reaches into its top {WRAP_HAND_TOP_PX} px"
    );
}

/// A scroll or a band move carries each sink with its row ([`GlassWrap`], §15.2 1.10,
/// ribbon-translate): the record rides to the row the text went to, keeping its instant, so
/// the next frame lays that row's glass as sunk as it was; a row moved off the screen or out of
/// its band drops its record.
#[test]
fn a_scroll_keeps_each_sink_on_its_row() {
    let c = glass_cfg(cfg(false, true));
    let t0 = Instant::now();
    let (mut rib, wrap) = wrapped(&c, t0);
    let now = at(wrap, 250);
    let _ = glass_frame(&mut rib, &c, now, WRAPPED_CARET);
    let [record] = *rib.glass_wraps() else {
        panic!("fixture: one record, {:?}", rib.glass_wraps());
    };
    let sunk = row_glass(&rib, 2);
    assert!(!sunk.is_empty(), "fixture: the left row lays glass");

    let mut scrolled = rib.clone();
    scrolled.translate_scroll(1);
    assert_eq!(
        scrolled.glass_wraps(),
        [GlassWrap { row: 1, ..record }],
        "the scroll carried the sink a row up"
    );
    let _ = glass_frame(&mut scrolled, &c, now, (2, 4));
    assert_eq!(
        row_glass(&scrolled, 1),
        sunk,
        "the row's glass is as sunk as it was"
    );
    scrolled.translate_scroll(2);
    assert!(
        scrolled.glass_wraps().is_empty(),
        "a row scrolled off the screen drops its sink"
    );

    let mut banded = rib.clone();
    banded.translate_band(1, 12, 3);
    assert_eq!(
        banded.glass_wraps(),
        [GlassWrap { row: 5, ..record }],
        "the band move carried the sink with its row"
    );
    let _ = glass_frame(&mut banded, &c, now, (6, 4));
    assert_eq!(
        row_glass(&banded, 5),
        sunk,
        "the row's glass is as sunk as it was"
    );
    banded.translate_band(3, 12, -3);
    assert!(
        banded.glass_wraps().is_empty(),
        "a row moved out of its band drops its sink"
    );
}

/// A carry moves each sink with its row ([`Ribbon::translate_run`], [`GlassWrap`], §15.2 1.10,
/// ribbon-translate): a run carried whole takes its record to its new row; a run carried in
/// part leaves its record on the part left behind and gives the carried part its own, with
/// the same instant; a carry off the pane drops it; the hand's lift maps a record on the
/// hand's row with the hand.
#[test]
fn a_carry_moves_each_sink_with_its_row() {
    use super::super::witness::FollowRun;
    let c = glass_cfg(cfg(false, true));
    let t0 = Instant::now();
    let (mut rib, wrap) = wrapped(&c, t0);
    let now = at(wrap, 250);
    let _ = glass_frame(&mut rib, &c, now, WRAPPED_CARET);
    let [record] = *rib.glass_wraps() else {
        panic!("fixture: one record, {:?}", rib.glass_wraps());
    };
    let sunk = row_glass(&rib, 2);
    let run = |lo: u16, hi: u16, dr: i16| FollowRun {
        row: 2,
        cohort: record.cohort,
        lo,
        hi,
        dr,
    };
    let mut moved = Vec::new();

    let mut whole = rib.clone();
    assert_eq!(
        whole.translate_run(run(10, 21, -1), now, false, &mut moved),
        12
    );
    assert_eq!(
        whole.glass_wraps(),
        [GlassWrap { row: 1, ..record }],
        "a run carried whole takes its sink"
    );
    let _ = glass_frame(&mut whole, &c, now, WRAPPED_CARET);
    let carried = row_glass(&whole, 1);
    assert_eq!(carried.len(), sunk.len(), "fixture: the run laid alike");
    for (&(x, up, _), &(_, was, _)) in carried.iter().zip(&sunk) {
        assert!(
            up <= was + 1e-3,
            "x {x}: the carried row's glass rose from {was} to {up} px"
        );
    }

    let mut split = rib.clone();
    assert_eq!(
        split.translate_run(run(16, 21, 4), now, false, &mut moved),
        6
    );
    let copy = split
        .cohorts()
        .iter()
        .find(|k| k.row == 6)
        .expect("the carried part's cohort")
        .id;
    assert_ne!(copy, record.cohort, "fixture: the carry split the run");
    assert_eq!(
        split.glass_wraps(),
        [
            record,
            GlassWrap {
                cohort: copy,
                row: 6,
                ..record
            }
        ],
        "the part left behind keeps its sink and the carried part takes one"
    );

    let mut off = rib.clone();
    off.set_pane_rows(Some((0, 4)));
    let _ = off.translate_run(run(10, 21, 4), now, false, &mut moved);
    assert!(
        off.glass_wraps().is_empty(),
        "a run carried off the pane drops its sink"
    );

    let mut lifted = rib.clone();
    lifted.lift_hand(2, 1);
    assert_eq!(
        lifted.glass_wraps(),
        [GlassWrap { row: 1, ..record }],
        "the hand's lift maps a record on its row"
    );
}

/// The glass's falling edge (§2.4 Light-only state, §15.2 1.10): while the glass lights, a
/// sinking row books its own one-pixel wakes (the negative control: with no sink the deadline
/// differs); the first frame on main's light, on the paper page or on a dark ground, clears
/// every record, leaves no light words, and books exactly the deadline the same keys book on
/// main's light throughout, so a dark frame never schedules a sink wake; when the glass
/// lights again, the row the hand left is left at that moment.
#[test]
fn a_light_to_dark_flip_clears_every_left_at_and_schedules_no_sink_wake() {
    let lit = glass_cfg(cfg(false, true));
    let paper = paper_cfg(cfg(false, true));
    let t0 = Instant::now();
    let (mut rib, wrap) = wrapped(&lit, t0);
    let (mut main, _) = wrapped(&paper, t0);
    let mid = at(wrap, 250);
    let _ = glass_frame(&mut rib, &lit, mid, WRAPPED_CARET);
    let _ = glass_frame(&mut main, &paper, mid, WRAPPED_CARET);
    assert!(!rib.glass_wraps().is_empty(), "fixture: the left row sinks");
    let mut unsunk = rib.clone();
    unsunk.wrap_unsunk = true;
    let booked = rib.next_change_deadline(mid);
    assert!(
        booked.is_some() && booked != unsunk.next_change_deadline(mid),
        "control: the sink books its own wakes"
    );

    // The same moment flipped to a dark ground instead, beside main's ribbon flipped there.
    let dark = Config {
        dark_theme: true,
        theme_fg: DEFAULT_FG,
        theme_bg: DEFAULT_BG,
        light: Light::OFF,
        ..lit
    };
    let (mut to_dark, mut main_dark) = (rib.clone(), main.clone());

    let flip = at(wrap, 266);
    let _ = glass_frame(&mut rib, &paper, flip, WRAPPED_CARET);
    let _ = glass_frame(&mut main, &paper, flip, WRAPPED_CARET);
    let _ = glass_frame(&mut to_dark, &dark, flip, WRAPPED_CARET);
    let _ = glass_frame(&mut main_dark, &dark, flip, WRAPPED_CARET);
    for (ground, rib, main) in [("paper", &rib, &main), ("dark", &to_dark, &main_dark)] {
        assert!(
            rib.glass_wraps().is_empty(),
            "{ground}: the falling edge clears every record"
        );
        let mut words = crate::state_words::StateWords::new(t0);
        rib.light_words(&mut words);
        assert!(
            words.words().is_empty(),
            "{ground}: no light words on main's light"
        );
        for ms in [0u64, 16, 100, 400] {
            let now = at(flip, ms);
            assert_eq!(
                rib.next_change_deadline(now),
                main.next_change_deadline(now),
                "{ground} +{ms} ms: the flipped ribbon books main's deadline"
            );
        }
    }

    let back = at(flip, 16);
    let _ = glass_frame(&mut rib, &lit, back, WRAPPED_CARET);
    assert!(
        matches!(rib.glass_wraps(), [w] if w.row == 2 && w.left_at == back),
        "lit again, the row the hand left is left at that moment: {:?}",
        rib.glass_wraps()
    );
}

/// The hand's structural set (§14 amendment 4, §15.2 1.10): the contiguous rows beside the
/// hand's that each hold a live cohort of its open phrase, neither abandoned nor a wake, the
/// [`STRUCTURAL_ROWS_MAX`] nearest the hand at most; a row of another phrase, an abandoned or
/// wake cohort or a blank row ends it, and with no phrase open it is the hand's row alone.
#[test]
fn the_structural_set_is_the_hand_s_open_phrase_rows_capped_at_eight() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    for row in 2..=13u16 {
        let keys = Keys {
            g: geom(),
            row,
            col0: 0,
            n: 3,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(t0, u64::from(row - 2) * 180), keys, &c);
        // Every row stays live while the next is typed.
        for coh in &mut rib.cohorts {
            coh.grace_s = 1000.0;
        }
        for cell in &mut rib.cells {
            cell.life_s = 8.0;
        }
    }
    let open = rib.phrase_open().expect("an open phrase");
    assert!(
        (2..=13).all(|row| rib.holds_phrase_on(open, row)),
        "fixture: one phrase over twelve rows: {:?}",
        rib.cohorts()
            .iter()
            .map(|k| (k.row, k.phrase, k.abandoned))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        rib.structural_rows(13),
        (6, 13),
        "the eight rows up from the hand"
    );
    assert_eq!(
        rib.structural_rows(7),
        (3, 10),
        "the eight rows nearest the hand"
    );
    assert_eq!(
        rib.structural_rows(15),
        (15, 15),
        "a blank row beside the hand ends it"
    );
    let cut = |rib: &mut Ribbon, row: u16, how: fn(&mut Cohort)| {
        let k = rib
            .cohorts
            .iter_mut()
            .find(|k| k.row == row)
            .expect("the row's cohort");
        how(k);
    };
    for (how, name) in [
        (
            (|k: &mut Cohort| k.abandoned = true) as fn(&mut Cohort),
            "an abandoned cohort",
        ),
        (|k: &mut Cohort| k.wake = true, "a wake"),
        (
            |k: &mut Cohort| k.phrase = k.phrase.wrapping_add(1),
            "another phrase",
        ),
        (|k: &mut Cohort| k.phase = Phase::Fading, "a dead cohort"),
    ] {
        let mut cut_rib = rib.clone();
        cut(&mut cut_rib, 10, how);
        assert_eq!(cut_rib.structural_rows(13), (11, 13), "{name} ends the set");
    }
    rib.phrase_open = None;
    assert_eq!(rib.structural_rows(13), (13, 13), "no phrase open, no set");
}

/// One height owner (§15.2 1.10 Hold): main's row hold runs unchanged on glass (a row over
/// ink below is held as main holds it), but the glass's height is the same held or free, so
/// the hold moves only main's body. Negative control: on main's light the held row's body
/// differs from the free one's.
#[test]
fn main_s_row_hold_runs_and_leaves_the_glass_s_height_alone() {
    let mut inked = GlyphProbe::new();
    inked.probe_row(3, &[true; 40]);
    let mut blank = GlyphProbe::new();
    blank.probe_row(3, &[false; 40]);
    let lay = |c: &Config, probe: &GlyphProbe| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 12, c, 0.9);
        rib.plan_over(&ctx(at(t0, 11 * 60 + 16), c, (2, 22), 0.9), Some(probe));
        let judged = at(t0, 11 * 60 + (HOLD_EASE_S * 1000.0) as u64 + 32);
        let cx = ctx(judged, c, (2, 22), 0.9);
        rib.plan_over(&cx, Some(probe));
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        (rib.holds.clone(), row_glass(&rib, 2), sink.under)
    };
    let lit = glass_cfg(cfg(false, true));
    let (held, held_glass, held_under) = lay(&lit, &inked);
    let (free, free_glass, free_under) = lay(&lit, &blank);
    let level = |holds: &[(u16, f32, Instant)]| {
        holds
            .iter()
            .find(|h| h.0 == 2)
            .map(|h| h.1)
            .expect("row 2's hold")
    };
    assert_eq!(
        (level(&held), level(&free)),
        (1.0, 0.0),
        "main's hold runs on glass"
    );
    assert_eq!(
        held_glass, free_glass,
        "the glass's plan is the same held or free"
    );
    assert_eq!(held_under, free_under, "and so is the glass it lays");

    let paper = paper_cfg(cfg(false, true));
    let (_, _, main_held) = lay(&paper, &inked);
    let (_, _, main_free) = lay(&paper, &blank);
    assert_ne!(main_held, main_free, "control: the hold moves main's body");
}

#[test]
fn the_left_run_s_feather_is_smooth_and_sheds_the_hot_edge() {
    let c = cfg(true, true);
    let g = geom();
    let cw = g.cw as f32;
    let head_x = 22.0 * cw;
    let jump = |coarse: bool, left_lit: bool| -> (f32, f32, usize) {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.feather_coarse = coarse;
        rib.hot_edge_left_lit = left_lit;
        type_run(&mut rib, t0, 2, 20, &c, 0.9);
        let now = at(t0, 19 * 60 + 16);
        let cx = ctx(now, &c, (10, 2), 0.9);
        rib.on_event(&pty((2, 22), (10, 2)), now, &cx);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let mut col = vec![0.0f32; usize::from(g.win_w)];
        for q in sink.under.iter().filter(|q| q.row == 2 && q.alpha > 0) {
            for x in q.x..q.x + q.w {
                let a = f32::from(q.alpha.max(q.alpha2));
                col[usize::from(x)] = col[usize::from(x)].max(a);
            }
        }
        let body = col[(head_x - 4.0 * cw) as usize];
        let lo = (head_x - 1.5 * cw) as usize;
        let hi = (head_x + 0.5 * cw) as usize;
        let step = col[lo..hi]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        let edge = sink.out.iter().filter(|q| q.row <= 2).count();
        (step, body, edge)
    };
    let (step, body, edge) = jump(false, false);
    assert!(
        body > 0.0,
        "fixture: the band is on glass on the jump frame"
    );
    assert!(
        step <= 0.2 * body,
        "the feather steps by {step} in a column against the body's {body}"
    );
    assert_eq!(
        edge, 0,
        "{edge} hot-edge quads stand over the run the hand left"
    );
    let (coarse, coarse_body, _) = jump(true, false);
    assert!(
        coarse > 0.2 * coarse_body,
        "control: laid at the stride the feather steps by only {coarse} against {coarse_body}"
    );
    let (_, _, kept) = jump(false, true);
    assert!(
        kept > 0,
        "control: kept on the left run, the hairline lays nothing"
    );
}

fn row_cells(rib: &Ribbon, row: u16) -> Vec<(u16, bool, f32, Instant)> {
    let mut v: Vec<_> = rib
        .cells()
        .iter()
        .filter(|c| c.row == row)
        .map(|c| (c.col, c.typing, c.life_s, c.born))
        .collect();
    v.sort_by_key(|c| c.0);
    v
}

#[test]
fn a_jump_lays_its_wake_over_the_live_band_and_leaves_the_band_s_cells_untouched() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g: geom(),
            row: 2,
            col0: 12,
            n: 40,
            period_ms: 60,
            disp: 0.9,
        },
        &c,
    );
    let last = at(t0, 39 * 60);
    let cx = ctx(at(last, 8), &c, (2, 52), 0.9);
    rib.plan(&cx);
    let band_light = |rib: &Ribbon| band_sum(rib, 51);
    let before = band_light(&rib);
    let before_cells: Vec<Cell> = rib.cells().to_vec();
    let segs = rib.plan_segments().len() as u32;
    let stops: Vec<Option<f32>> = (12..52u16).map(|col| rib.field_at(2, col)).collect();
    assert!(
        stops.iter().all(Option::is_some),
        "the band is laid under cols 12..52"
    );
    let now = at(last, 16);
    let cx = ctx(now, &c, (2, 2), 0.9);
    let envs_before: Vec<(u16, f32)> = before_cells
        .iter()
        .map(|l| (l.col, rib.env_of(&cx, l)))
        .collect();
    rib.on_event(&nav((2, 52), (2, 2)), now, &cx);
    rib.plan(&cx);
    for (col, want) in (12..52u16).zip(&stops) {
        let got = rib.field_at(2, col);
        assert!(
            matches!((got, want), (Some(g), Some(w)) if (g - w).abs() < 1e-5),
            "col {col}: the wake repainted a stop the eye had read ({want:?} → {got:?})"
        );
    }
    let wake = rib
        .cohorts()
        .iter()
        .map(|c| c.id)
        .max()
        .expect("the wake cohort");
    let band = rib.cohorts()[0];
    assert_ne!(band.id, wake);
    assert!(band.abandoned, "the band is abandoned beneath the corridor");
    assert_eq!(
        (band.retract_col, band.drain_right_first),
        (Some(2), true),
        "…toward the landing, draining from the origin end first (the drain follows the meteor)"
    );
    let flight = flight_ms(50.0) / 1000.0;
    for col in 2..12u16 {
        assert!(
            !rib.cells()
                .iter()
                .any(|c| c.row == 2 && c.col == col && c.layer == Layer::Base),
            "col {col}: dark ground carries no band cell"
        );
        let over: Vec<&Cell> = rib
            .cells()
            .iter()
            .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Over)
            .collect();
        assert_eq!(over.len(), 1, "col {col}: the corridor lit the dark ground");
        assert!(over[0].rearm.is_none(), "col {col}: nothing to re-arm from");
    }
    for col in 12..52u16 {
        let base: Vec<&Cell> = rib
            .cells()
            .iter()
            .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Base)
            .collect();
        let over: Vec<&Cell> = rib
            .cells()
            .iter()
            .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Over)
            .collect();
        assert_eq!(
            base.len(),
            1,
            "col {col}: the band's own cell is still there"
        );
        assert_eq!(over.len(), 1, "col {col}: one wake cell over it");
        let (b, o) = (base[0], over[0]);
        let was = before_cells
            .iter()
            .find(|c| c.col == col)
            .expect("was laid");
        assert!(
            b.cohort == was.cohort
                && b.born == was.born
                && b.cov0 == was.cov0
                && b.life_s == was.life_s,
            "col {col}: the wake touched the band's cell (K1)"
        );
        assert_eq!(o.cohort, wake, "col {col}: …in the wake cohort");
        assert!(!o.typing, "col {col}: a wake cell is never typing");
        assert!(
            o.cov0 >= b.cov0 - 1e-6,
            "col {col}: the corridor is never dimmer than the band it covers"
        );
        let (re_at, level) = o.rearm.expect("born at the band's level");
        assert_eq!(re_at, o.born);
        let env_before = envs_before
            .iter()
            .find(|&&(c, _)| c == col)
            .map_or(0.0, |&(_, e)| e);
        assert!(
            env_before > 0.9 && (level * o.cov0 - b.cov0 * env_before).abs() < 1e-5,
            "col {col}: the level is the band's own coverage share (level {level}, env {env_before})"
        );
        let j = f32::from(52 - col);
        let want = now + Duration::from_secs_f32(flight * j / 50.0);
        assert!(
            o.born.saturating_duration_since(want) <= Duration::from_millis(1)
                && want.saturating_duration_since(o.born) <= Duration::from_millis(1),
            "col {col}: born on the flight clock"
        );
    }
    assert!(rib.field_at(2, 52).is_none());
    assert_eq!(
        rib.cells().len(),
        90,
        "the band's 40 cells and the corridor's 50 over them"
    );
    let after = band_light(&rib);
    assert!(
        after + 2 * segs >= before,
        "the jump frame stepped the band down ({before} → {after} over {segs} boundaries)"
    );
    let cx = ctx(at(now, (flight * 1000.0) as u64 + 20), &c, (2, 2), 0.9);
    rib.plan(&cx);
    let landed = band_light(&rib);
    assert!(
        landed + 2 * segs >= before,
        "the landed corridor is dimmer than the band it covers ({before} → {landed})"
    );
    let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
    rib.plan(&cx);
    let held = band_light(&rib);
    assert!(
        held * 2 > before,
        "0.66 s after the jump the wake must still hold the corridor ({before} → {held})"
    );
    let cx = ctx(at(last, 16 + 1250), &c, (2, 80), 0.9);
    rib.plan(&cx);
    assert!(rib.at_rest(), "the wake is out by +1.25 s");
}

#[test]
fn a_ping_pong_at_speed_does_not_accumulate_and_empties_one_wake_life_after_its_last_key() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let mut caret = (2u16, 2u16);
    let mut peak_by_cycle = Vec::new();
    let mut peak = 0usize;
    let landed = 200 + flight_ms(58.0) as u64 + (EDGE_IN_S * 1000.0) as u64 + 8;
    let mut interval_frames = 0u32;
    for ms in (0..=3000u64).step_by(8) {
        let now = at(t0, ms);
        if ms % 200 == 0 {
            let to = if caret.1 == 2 { (2, 60) } else { (2, 2) };
            let cx = ctx(now, &c, to, 0.0);
            rib.on_event(&nav(caret, to), now, &cx);
            caret = to;
            if ms > 0 {
                peak_by_cycle.push(peak);
                peak = 0;
            }
        }
        let cx = ctx(now, &c, caret, 0.0);
        rib.plan(&cx);
        peak = peak.max(rib.cells().len());
        if ms % 200 == 0 {
            let wakes = rib
                .cohorts()
                .iter()
                .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
                .count();
            assert_eq!(wakes, 1, "+{ms} ms: one live wake cohort on the row (K4)");
        }
        if ms >= landed {
            let lit: Vec<u16> = (2..=60u16)
                .filter(|&col| {
                    rib.cells().iter().any(|l| {
                        l.row == 2
                            && l.col == col
                            && UNDER_COV_CAP * l.cov0 * rib.env_of(&cx, l)
                                >= f32::from(STATUS_LIT_COV)
                    })
                })
                .collect();
            assert!(
                lit.first() == Some(&2) && lit.last() == Some(&60),
                "+{ms} ms: the lit set holds both endpoints ({lit:?})"
            );
            assert!(
                lit.windows(2).all(|w| w[1] - w[0] <= 1),
                "+{ms} ms: the lit set is one interval, no hole (K2): {lit:?}"
            );
            interval_frames += 1;
        }
    }
    assert!(
        interval_frames > 300,
        "the interval law was checked on {interval_frames} frames"
    );
    let bound = usize::from(WAKE_MAX_CELLS + 1) * (WAKE_LIFE_S / 0.2).ceil() as usize
        + 59 * (RETRACT_START_S / 0.2).ceil() as usize;
    let worst = *peak_by_cycle.iter().max().expect("cycles");
    assert!(
        worst <= bound,
        "the pool grew past its bound: {worst} > {bound} ({peak_by_cycle:?})"
    );
    let (early, late) = peak_by_cycle.split_at(peak_by_cycle.len() / 2);
    assert!(
        late.iter().max() <= early.iter().max(),
        "the second half of the ping-pong holds more than the first: {peak_by_cycle:?}"
    );
    let cx = ctx(at(t0, 3000 + 1250), &c, caret, 0.0);
    rib.plan(&cx);
    assert!(
        rib.at_rest(),
        "everything is out one wake life after the last jump"
    );
}

#[test]
fn a_partial_reversal_keeps_the_whole_corridor_on_one_clock() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 43, &c, 0.9);
    let cold = at(t0, 6000);
    rib.plan(&ctx(cold, &c, (2, 43), 0.0));
    assert!(
        rib.at_rest(),
        "the typed band is gone before the first jump"
    );
    let a = at(cold, 100);
    rib.on_event(&nav((2, 43), (2, 0)), a, &ctx(a, &c, (2, 0), 0.0));
    rib.plan(&ctx(at(a, 130), &c, (2, 0), 0.0));
    let first = rib
        .cohorts()
        .iter()
        .find(|k| k.wake)
        .map(|k| k.id)
        .expect("the first corridor");
    let b = at(a, 300);
    rib.on_event(&nav((2, 0), (2, 20)), b, &ctx(b, &c, (2, 20), 0.0));
    rib.plan(&ctx(at(b, 130), &c, (2, 20), 0.0));
    let live: Vec<(u32, bool)> = rib
        .cohorts()
        .iter()
        .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
        .map(|k| (k.id, k.abandoned))
        .collect();
    assert_eq!(
        live,
        vec![(first, false)],
        "the reversal re-owns the corridor it overlaps: one live wake cohort, \
         not abandoned ({live:?})"
    );
    let cx = ctx(at(b, 130), &c, (2, 20), 0.0);
    let clocks: Vec<Instant> = rib
        .cells()
        .iter()
        .filter(|l| l.layer == Layer::Over)
        .map(|l| rib.live_since(l))
        .collect();
    assert!(clocks.len() > 30, "the corridor is laid ({})", clocks.len());
    assert_eq!(
        clocks.iter().min(),
        clocks.iter().max(),
        "every corridor cell reads one clock, covered or not"
    );
    let mut frames = 0u32;
    for ms in (0..1000u64).step_by(8) {
        let now = at(b, 130 + ms);
        let cx = ctx(now, &c, (2, 20), 0.0);
        rib.plan(&cx);
        let live: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.layer == Layer::Over && rib.env_of(&cx, l) > 0.0)
            .map(|l| l.col)
            .collect();
        if live.is_empty() {
            break;
        }
        let (lo, hi) = (
            *live.iter().min().expect("min"),
            *live.iter().max().expect("max"),
        );
        assert_eq!(
            (lo..=hi).filter(|col| live.contains(col)).count(),
            usize::from(hi - lo + 1),
            "+{ms} ms: the corridor is one interval ({lo}..={hi})"
        );
        frames += 1;
    }
    let _ = cx;
    assert!(
        frames > 60,
        "the interval law was checked on {frames} frames"
    );
}

#[test]
fn a_row_change_a_pty_jump_and_a_typed_move_lay_no_wake() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let cx = ctx(t0, &c, (3, 5), 0.0);
    rib.on_event(&nav((2, 5), (3, 5)), t0, &cx);
    assert!(rib.at_rest(), "an Up-arrow recall paints no line");
    let cx = ctx(t0, &c, (2, 2), 0.0);
    rib.on_event(&pty((2, 60), (2, 2)), t0, &cx);
    assert!(rib.at_rest(), "a PTY cascade earns no wake");
    rib.on_event(
        &Event::Move {
            from: (2, 60),
            to: (2, 2),
            licence: Licence::Typed,
            dir: Dir::Right,
        },
        t0,
        &cx,
    );
    assert!(rib.at_rest(), "a typed echo's own motion lays nothing");
    rib.on_event(
        &Event::Move {
            from: (2, 60),
            to: (2, 2),
            licence: Licence::Return,
            dir: Dir::Right,
        },
        t0,
        &cx,
    );
    assert!(rib.at_rest(), "a return-licensed move lays nothing");
}

#[test]
fn an_arrow_hop_lays_a_short_wake_behind_the_caret_and_leaves_live_cells_alone() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.8);
    let word = rib.cohorts()[0];
    assert!(!word.wake);
    let now = at(t0, 600);
    let cx = ctx(now, &c, (2, 19), 0.8);
    let t17 = rib.field_at(2, 17);
    rib.on_event(&nav((2, 18), (2, 19)), now, &cx);
    rib.plan(&cx);
    assert_eq!(
        rib.field_at(2, 17),
        t17,
        "the live cell beside the hop is untouched"
    );
    let laid: Vec<&Cell> = rib
        .cells()
        .iter()
        .filter(|c| c.row == 2 && c.col == 18)
        .collect();
    assert_eq!(laid.len(), 1);
    assert_ne!(
        laid[0].cohort, word.id,
        "the hop's cell is in a cohort of its own, not the word's"
    );
    let hop = rib
        .cohorts()
        .iter()
        .find(|k| k.id == laid[0].cohort)
        .expect("the hop's cohort");
    assert!(hop.wake, "…a wake cohort");
    assert!(
        (laid[0].t - word.t_at(18)).abs() < 1e-6,
        "…on the word's own walk ({} vs {})",
        laid[0].t,
        word.t_at(18)
    );
    assert!(!laid[0].typing);
    assert!((laid[0].life_s - WAKE_LIFE_S * WAKE_HOP_LIFE_SHARE).abs() < 1e-6);
    assert!(
        rib.cells()
            .iter()
            .filter(|c| c.row == 2 && c.col == 17)
            .count()
            == 1
    );
    let word_now = rib
        .cohorts()
        .iter()
        .find(|k| k.id == word.id)
        .expect("the word");
    assert_eq!(
        (word_now.alive_at, word_now.col0, word_now.col1),
        (word.alive_at, word.col0, word.col1),
        "the hop touched the word's clock or bounds"
    );
    assert_eq!(
        word_now.phase,
        Phase::Grace,
        "180 ms after its last key the word is in its grace — a hop that refreshed it would read Laying"
    );
    let cx = ctx(
        at(t0, 420 + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
        &c,
        (2, 19),
        0.0,
    );
    rib.plan(&cx);
    assert!(
        rib.cells().iter().all(|c| c.cohort != word.id),
        "the word must be out {SWOOSH_TOTAL_S} s after its own last key; the arrow lifted its finger"
    );
    let mut rib = Ribbon::new();
    for (k, col) in (40u16..46).enumerate() {
        let now = at(t0, 40 * k as u64);
        let cx = ctx(now, &c, (2, col + 1), 0.0);
        rib.on_event(&nav((2, col), (2, col + 1)), now, &cx);
        rib.plan(&cx);
    }
    let cols: Vec<u16> = row_cells(&rib, 2).iter().map(|c| c.0).collect();
    assert_eq!(
        cols,
        (40..=46).collect::<Vec<_>>(),
        "the arrow painted behind itself"
    );
    assert_eq!(rib.cohorts().len(), 1, "one walk under the held arrow");
    let cx = ctx(at(t0, 200 + 100 + 560), &c, (2, 46), 0.0);
    rib.plan(&cx);
    assert!(
        rib.at_rest(),
        "the arrow's trail is out half a wake life after its last step"
    );
}

fn over_cols(rib: &Ribbon, row: u16) -> Vec<u16> {
    let mut v: Vec<u16> = rib
        .cells()
        .iter()
        .filter(|l| l.row == row && l.layer == Layer::Over)
        .map(|l| l.col)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn lit_cols(rib: &Ribbon, cx: &Ctx<'_>, row: u16) -> Vec<u16> {
    let mut v: Vec<u16> = rib
        .cells()
        .iter()
        .filter(|l| {
            l.row == row && UNDER_COV_CAP * l.cov0 * rib.env_of(cx, l) >= f32::from(STATUS_LIT_COV)
        })
        .map(|l| l.col)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn a_jump_s_cap_binds_at_the_corridor_s_far_edge_never_inside_it() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 100, &c, 0.9);
    let last = at(t0, 99 * 60);
    let idle_ms = ((RETRACT_START_S + RETRACT_FADE_S + 0.2) * 1000.0).round() as u64;
    let now = at(last, idle_ms);
    let cx = ctx(now, &c, (2, 102), 0.9);
    rib.plan(&cx);
    let lit_before = lit_cols(&rib, &cx, 2);
    assert!(
        lit_before.first().is_some_and(|&l| l > 2 + WAKE_MAX_CELLS)
            && lit_before.last() == Some(&101),
        "the setup: the drain frontier lies past the cap, the remnant by the origin \
         ({lit_before:?})"
    );
    let cx = ctx(now, &c, (2, 2), 0.9);
    rib.on_event(&nav((2, 102), (2, 2)), now, &cx);
    assert_eq!(
        over_cols(&rib, 2),
        (2..=101).collect::<Vec<_>>(),
        "the corridor runs from the landing out to the farthest light it keeps"
    );
    let landed = at(now, flight_ms(100.0) as u64 + 30);
    let cx = ctx(landed, &c, (2, 2), 0.9);
    rib.plan(&cx);
    let lit = lit_cols(&rib, &cx, 2);
    assert!(
        lit.first() == Some(&2)
            && lit.last() == Some(&101)
            && lit.windows(2).all(|w| w[1] - w[0] <= 1),
        "(a) the lit set is one interval holding both ends: {lit:?}"
    );
    let mut rib = Ribbon::new();
    let a = at(t0, 0);
    rib.on_event(&nav((2, 110), (2, 2)), a, &ctx(a, &c, (2, 2), 0.0));
    rib.plan(&ctx(at(a, 130), &c, (2, 2), 0.0));
    assert_eq!(
        over_cols(&rib, 2),
        (2..2 + WAKE_MAX_CELLS).collect::<Vec<_>>(),
        "the setup: the cold Ctrl-A lays the cap"
    );
    let e = at(a, 300);
    rib.on_event(&nav((2, 2), (2, 110)), e, &ctx(e, &c, (2, 110), 0.0));
    assert_eq!(
        over_cols(&rib, 2),
        (2..=110).collect::<Vec<_>>(),
        "the End's corridor reaches the island it re-owns"
    );
    let wakes = rib
        .cohorts()
        .iter()
        .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
        .count();
    assert_eq!(wakes, 1, "one wake cohort on the row (K4)");
    for ms in (160..900u64).step_by(8) {
        let cx = ctx(at(e, ms), &c, (2, 110), 0.0);
        rib.plan(&cx);
        let lit = lit_cols(&rib, &cx, 2);
        assert!(
            lit.windows(2).all(|w| w[1] - w[0] <= 1),
            "(b) +{ms} ms: the lit set is one interval (K2): {lit:?}"
        );
        if ms < 500 {
            assert!(
                lit.first() == Some(&2) && lit.last() == Some(&110),
                "(b) +{ms} ms: the lit set holds both ends: {lit:?}"
            );
        }
    }
}

#[test]
fn a_hop_covers_a_leaving_band_from_its_level_and_leaves_a_live_one_as_it_is() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * 0.9;
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 9, &c, 0.9);
    let last = at(t0, 8 * 60);
    let band = rib.cohorts()[0];
    let now = at(last, (RETRACT_START_S * 1000.0).round() as u64 + 100);
    let cx = ctx(now, &c, (2, 11), 0.9);
    rib.plan(&cx);
    assert!(
        rib.cohorts()[0].leaving_at(now),
        "the setup: the band is inside its swoosh"
    );
    let before: Vec<Cell> = rib.cells().to_vec();
    let level: Vec<(u16, f32)> = before
        .iter()
        .map(|l| (l.col, l.cov0 * rib.env_of(&cx, l)))
        .collect();
    assert!(
        level
            .iter()
            .filter(|&&(col, _)| col >= 6)
            .all(|&(_, v)| v > 0.1),
        "the setup: the band is still lit under the hop ({level:?})"
    );
    let cx = ctx(now, &c, (2, 6), 0.9);
    rib.on_event(&nav((2, 11), (2, 6)), now, &cx);
    assert_eq!(
        over_cols(&rib, 2),
        (6..=11).collect::<Vec<_>>(),
        "the hop lays its whole corridor over the leaving band"
    );
    for w in rib.cells().iter().filter(|l| l.layer == Layer::Over) {
        match level.iter().find(|&&(col, _)| col == w.col) {
            Some(&(col, start)) => {
                let (re_at, lv) = w
                    .rearm
                    .unwrap_or_else(|| panic!("col {col}: over band light is re-armed"));
                assert_eq!(re_at, w.born, "col {col}: re-armed at its birth");
                assert!(
                    (w.cov0 * lv - start).abs() < 1e-5,
                    "col {col}: born at the band's own coverage {start} (cov0 {} level {lv})",
                    w.cov0
                );
            }
            None => assert!(
                w.rearm.is_none() && (w.cov0 - cov0_new).abs() < 1e-6,
                "col {}: over nothing, a cold-priced cell from dark",
                w.col
            ),
        }
    }
    for was in &before {
        let is = rib
            .cells()
            .iter()
            .find(|l| l.layer == Layer::Base && l.col == was.col)
            .expect("the band's cells stay");
        assert!(
            is.cohort == was.cohort
                && is.born == was.born
                && is.cov0 == was.cov0
                && is.retract_at == was.retract_at,
            "the hop touched the band's cell at col {} (K1)",
            was.col
        );
    }
    assert_eq!(
        rib.cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .map(|k| k.alive_at),
        Some(band.alive_at),
        "the leaving band keeps its clock"
    );
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 9, &c, 0.9);
    let now = at(last, 300);
    let cx = ctx(now, &c, (2, 11), 0.9);
    rib.plan(&cx);
    assert!(
        !rib.cohorts()[0].leaving_at(now),
        "the setup: the band is live"
    );
    let before: Vec<Cell> = rib.cells().to_vec();
    let cx = ctx(now, &c, (2, 8), 0.9);
    rib.on_event(&nav((2, 14), (2, 8)), now, &cx);
    assert_eq!(
        over_cols(&rib, 2),
        (11..=14).collect::<Vec<_>>(),
        "the hop lays only the dark ground; the live band's cells are left as they are"
    );
    for was in &before {
        assert!(
            rib.cells().iter().any(|l| l.layer == Layer::Base
                && l.col == was.col
                && l.born == was.born
                && l.cohort == was.cohort),
            "the live band's cell at col {} is untouched",
            was.col
        );
    }
}

type CensusColumn = (u16, Option<(u32, Instant, bool, u32)>, u8);

fn census(rib: &Ribbon, row: u16, col0: u16, col1: u16) -> Vec<CensusColumn> {
    let cw = geom().cw as f32;
    (col0..col1)
        .map(|col| {
            let owner = rib
                .cells()
                .iter()
                .rposition(|c| c.row == row && c.col == col && !c.leaving())
                .map(|i| {
                    let c = rib.cells()[i];
                    (c.cohort, c.born, c.typing, c.cov0.to_bits())
                });
            let cov = rib
                .plan_segments()
                .iter()
                .filter(|s| (s.x / cw).floor() as i64 == i64::from(col))
                .map(|s| s.cov)
                .max()
                .unwrap_or(0);
            (col, owner, cov)
        })
        .collect()
}

fn gaps(before: &[CensusColumn], now: &[CensusColumn]) -> Vec<(u16, u8)> {
    before
        .iter()
        .zip(now)
        .filter(|(b, n)| b != n)
        .map(|(_, n)| (n.0, n.2))
        .collect()
}

fn type_the_owner_s_line(rib: &mut Ribbon, t0: Instant, c: &Config) -> Instant {
    let keys = Keys {
        g: geom(),
        row: 2,
        col0: 0,
        n: 44,
        period_ms: 83,
        disp: 0.8,
    };
    type_keys(rib, t0, keys, c);
    at(t0, 43 * 83)
}

#[test]
fn a_scrub_inside_the_band_keeps_every_cell_s_light_and_holds_the_row() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let last = type_the_owner_s_line(&mut rib, t0, &c);
    let band = rib.cohorts()[0];
    let cx = ctx(at(last, 150), &c, (2, 44), 0.8);
    rib.plan(&cx);
    let before = census(&rib, 2, 0, 44);
    assert!(
        before
            .iter()
            .all(|(_, owner, cov)| owner.is_some() && *cov > 100),
        "the band is whole and hot under the line before the scrub"
    );
    let stops = [40u16, 35, 31, 26, 31, 35, 40, 44, 40, 35];
    let mut caret = (2u16, 44u16);
    let mut last_hop = last;
    for (k, &col) in stops.iter().enumerate() {
        let now = at(last, 150 * (k as u64 + 1));
        let to = (2, col);
        let cx = ctx(now, &c, to, 0.8);
        rib.on_event(&nav(caret, to), now, &cx);
        rib.plan(&cx);
        caret = to;
        last_hop = now;
        let now_census = census(&rib, 2, 0, 44);
        let g = gaps(&before, &now_census);
        assert!(
            g.is_empty(),
            "hop {} ({}→{}, +{} ms): the scrub damaged the band at {} of 44 cells: {:?}",
            k + 1,
            if k == 0 { 44 } else { stops[k - 1] },
            col,
            150 * (k + 1),
            g.len(),
            g
        );
        assert!(
            rib.cohorts().iter().all(|k| !k.abandoned),
            "hop {}: a scrub abandons nothing",
            k + 1
        );
        assert!(
            rib.cells().iter().all(|c| !c.leaving()),
            "hop {}: a scrub sends no cell on its way out",
            k + 1
        );
    }
    let held = rib
        .cohorts()
        .iter()
        .find(|k| k.id == band.id)
        .expect("the typed band is still the band");
    assert_eq!(
        held.alive_at, last_hop,
        "the scrubbing hand holds the row — the band's clock is the hop's"
    );
    assert_eq!(held.phase, Phase::Laying);
    let cx = ctx(at(last_hop, 700), &c, caret, 0.8);
    rib.plan(&cx);
    assert!(
        gaps(&before, &census(&rib, 2, 0, 44)).is_empty(),
        "0.7 s after the last hop the band is still whole: the hops renewed its clock"
    );
    let cx = ctx(at(last_hop, 1400), &c, caret, 0.8);
    rib.plan(&cx);
    assert!(
        !rib.at_rest() && plan_sum(&rib) > 0,
        "1.4 s after the last hop the band is leaving on the hop's clock"
    );
    let cx = ctx(
        at(last_hop, (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
        &c,
        caret,
        0.8,
    );
    rib.plan(&cx);
    assert!(
        rib.at_rest(),
        "the band leaves through its swoosh {SWOOSH_TOTAL_S} s after the last hop"
    );
}

#[test]
fn a_scrub_into_a_leftward_corridor_does_not_move_its_soft_end() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 0, 11, &c, 0.9);
    let last = at(t0, 10 * 60);
    for (from, to, ms) in [((2u16, 11u16), (2, 31), 200u64), ((2, 31), (2, 0), 550)] {
        let now = at(last, ms);
        let cx = ctx(now, &c, to, 0.9);
        rib.on_event(&nav(from, to), now, &cx);
        rib.plan(&cx);
    }
    let cx = ctx(at(last, 734), &c, (2, 0), 0.9);
    rib.plan(&cx);
    let corridor = |r: &Ribbon| {
        r.runs
            .iter()
            .find(|u| u.row == 2 && u.col0 == 0)
            .map(|u| (u.col0, u.col1, u.stream_dir))
            .expect("the corridor's own run")
    };
    let (c0, c1, dir_before) = corridor(&rib);
    assert!(
        c1 >= c0 + 4,
        "the setup: the corridor must be long enough to have a far end away from its landing (cols {c0}..={c1})"
    );
    let before = census(&rib, 2, c0, c1 + 2);
    let cohorts_before = rib.cohorts().len();
    let scrub_at = at(last, 750);
    let cx = ctx(scrub_at, &c, (2, 2), 0.9);
    rib.on_event(&nav((2, 0), (2, 2)), scrub_at, &cx);
    rib.plan(&cx);
    let after = census(&rib, 2, c0, c1 + 2);
    let (a0, a1, dir_after) = corridor(&rib);
    let show = |cen: &[CensusColumn]| cen.iter().map(|k| (k.0, k.2)).collect::<Vec<_>>();
    assert_eq!(
        cohorts_before,
        rib.cohorts().len(),
        "the setup: the two Rights are a scrub over lit cells and mint nothing"
    );
    assert_eq!(
        (a0, a1),
        (c0, c1),
        "the scrub moved the corridor's own bounds, so the two censuses read different columns"
    );
    assert_ne!(
        dir_before, dir_after,
        "the setup: the caret move still flips the corridor run's stream, which is what the soft end must no longer follow"
    );
    for (name, cen) in [("before the scrub", &before), ("after it", &after)] {
        let whole = cen[1].2;
        assert!(
            cen[0].2 < whole && whole > 100 && cen[1..].iter().all(|k| k.2 == whole),
            "{name}: the corridor opens soft at its left and is whole to its right edge: {:?}",
            show(cen)
        );
    }
    for (b, a) in before.iter().zip(after.iter()) {
        assert!(
            i32::from(a.2) - i32::from(b.2) <= 2,
            "col {}: the scrub raised the corridor's light {} -> {} in one frame ({:?} -> {:?})",
            b.0,
            b.2,
            a.2,
            show(&before),
            show(&after)
        );
    }
}

#[test]
fn a_new_phrase_holds_none_of_a_closed_one_s_cohorts_until_the_hand_resumes_it() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let keys = |col0: u16, n: u16| Keys {
        g: geom(),
        row: 2,
        col0,
        n,
        period_ms: 100,
        disp: 0.9,
    };
    type_keys(&mut rib, t0, keys(2, 3), &c);
    let first = rib.phrase_open().expect("open");
    let abc = rib.cohorts()[0];
    assert_eq!(abc.phrase, first);
    type_keys(&mut rib, at(t0, 1400), keys(20, 3), &c);
    let second = rib.phrase_open().expect("open");
    assert_ne!(second, first, "a rest closed the first phrase");
    let abc_now = rib
        .cohorts()
        .iter()
        .find(|k| k.id == abc.id)
        .expect("abc is still resident inside its swoosh");
    assert_eq!(
        abc_now.phrase, first,
        "the closed phrase's cohort is not re-assigned"
    );
    assert_eq!(
        abc_now.alive_at,
        at(t0, 200),
        "…and not renewed by the new phrase's keys"
    );
    assert_ne!(abc_now.phase, Phase::Laying, "it is on its way out");
    let now = at(t0, 1700);
    let cx = ctx(now, &c, (2, 6), 0.9);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    let abc_now = rib
        .cohorts()
        .iter()
        .find(|k| k.id == abc.id)
        .expect("the resumed cohort");
    assert_eq!(
        abc_now.phrase, second,
        "resuming a line re-joins the open phrase"
    );
    assert_eq!(abc_now.alive_at, now);
    assert_eq!(abc_now.phase, Phase::Laying);
}

#[test]
fn a_backspace_run_holds_the_phrase_so_the_left_end_stays_lit_through_it() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 20, &c, 0.9);
    let last = at(t0, 19 * 60);
    let coh = rib.cohorts()[0];
    let mut caret = 22u16;
    for i in 0..8u64 {
        caret -= 1;
        let now = at(last, 100 + i * 120);
        erase_at(&mut rib, now, (2, caret), &c);
        let k = rib
            .cohorts()
            .iter()
            .find(|k| k.id == coh.id)
            .expect("the band is held");
        assert_eq!(
            k.alive_at, now,
            "erase {i}: the phrase's clock follows the hand"
        );
        assert_eq!(k.phase, Phase::Laying);
        let cx = ctx(now, &c, (2, caret), 0.9);
        let left = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 2)
            .expect("the left end is in the pool");
        assert!(
            rib.env_of(&cx, left) > 0.99,
            "erase {i} (+{} ms after the last key): the left end is at full ({})",
            100 + i * 120,
            rib.env_of(&cx, left)
        );
    }
    let last_erase = at(last, 100 + 7 * 120);
    let cx = ctx(at(last_erase, 800), &c, (2, 14), 0.0);
    rib.plan(&cx);
    let k = rib.cohorts().iter().find(|k| k.id == coh.id).expect("held");
    assert_eq!(k.phase, Phase::Grace);
    let cx = ctx(at(last_erase, 950), &c, (2, 14), 0.0);
    rib.plan(&cx);
    let k = rib.cohorts().iter().find(|k| k.id == coh.id).expect("held");
    assert_eq!(
        k.phase,
        Phase::Reaching,
        "the rest is measured from the last erase"
    );
}

#[test]
fn a_backspace_run_begun_after_the_rest_snaps_the_band_back_and_runs_the_tail_from_it() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 20,
            period_ms: 100,
            disp: 0.9,
        },
        &c,
    );
    let id = rib.cohorts()[0].id;
    type_keys(
        &mut rib,
        at(t0, 2000),
        Keys {
            g: geom(),
            row: 5,
            col0: 2,
            n: 1,
            period_ms: 100,
            disp: 0.9,
        },
        &c,
    );
    let last = at(t0, 2000);
    let other = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 5)
        .expect("row 5")
        .alive_at;
    let glow = at(last, 1400);
    let cx = ctx(glow, &c, (2, 22), 0.0);
    rib.plan(&cx);
    let leaving = rib
        .cohorts()
        .iter()
        .find(|k| k.id == id)
        .expect("row 2 is still resident");
    assert!(
        leaving.phase != Phase::Laying && leaving.phase != Phase::Grace,
        "the fixture must erase into a band that is already leaving ({:?})",
        leaving.phase
    );
    let dim = lit_row(&rib, &cx, 2, 1.0).len();
    let full_env = rib
        .cells()
        .iter()
        .filter(|l| l.row == 2 && rib.env_of(&cx, l) > 0.99)
        .count();
    let bs0 = at(glow, 0);
    erase_at(&mut rib, bs0, (2, 21), &c);
    let cx = ctx(bs0, &c, (2, 21), 0.0);
    let k = rib
        .cohorts()
        .iter()
        .find(|k| k.id == id)
        .expect("the band is taken back");
    assert_eq!(k.alive_at, bs0, "the clock follows the hand that came back");
    assert_eq!(k.phase, Phase::Laying);
    let back = lit_row(&rib, &cx, 2, 1.0).len();
    let back_env = rib
        .cells()
        .iter()
        .filter(|l| l.row == 2 && rib.env_of(&cx, l) > 0.99)
        .count();
    assert!(
        back > dim && back_env > full_env,
        "the erase must SNAP the band back ({dim} -> {back} lit, {full_env} -> {back_env} at full)"
    );
    let left = rib
        .cells()
        .iter()
        .find(|l| l.row == 2 && l.col == 2)
        .expect("the left end is in the pool");
    assert!(
        rib.env_of(&cx, left) > 0.99,
        "…and the left end is back at full ({})",
        rib.env_of(&cx, left)
    );
    assert_eq!(
        rib.cohorts()
            .iter()
            .find(|k| k.row == 5)
            .expect("row 5 is resident")
            .alive_at,
        other,
        "an erase on row 2 must not renew row 5"
    );
    let mut caret = 21u16;
    for i in 1..8u64 {
        caret -= 1;
        erase_at(&mut rib, at(glow, i * 120), (2, caret), &c);
    }
    let last_erase = at(glow, 7 * 120);
    let mut tail_ms = 0u64;
    for ms in (0..=2400).step_by(20) {
        let cx = ctx(at(last_erase, ms), &c, (2, caret), 0.0);
        rib.plan(&cx);
        if rib.cells().iter().any(|l| l.row == 2) {
            tail_ms = ms;
        } else {
            break;
        }
    }
    assert!(
        tail_ms >= 1176,
        "the band must outlive the last erase by at least the 1175.8 ms v0.85.0 measured ({tail_ms} ms)"
    );
    let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
    assert!(
        tail_ms <= total + 60,
        "…and by no more than its own swoosh ({tail_ms} ms against {total} ms)"
    );
}

#[test]
fn a_row_the_caret_is_not_on_retracts_into_its_own_nearest_end_in_logical_order() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let below = |to: (u16, u16)| {
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 4, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let now = at(last, 100);
        let cx = ctx(now, &c, to, 0.9);
        rib.on_event(&nav((2, 24), to), now, &cx);
        rib.plan(&cx);
        let k = rib.cohorts()[0];
        let env = |col: u16, ms: u64| {
            let cx = ctx(at(now, ms), &c, to, 0.0);
            let cell = rib
                .cells()
                .iter()
                .find(|l| l.row == 2 && l.col == col)
                .expect("cell");
            rib.env_of(&cx, cell)
        };
        (k, env(4, 200), env(23, 200))
    };
    let (k, left, right) = below((1, 24));
    assert!(k.abandoned && k.phase.is_retracting());
    assert_eq!(
        k.retract_col,
        Some(4),
        "a row below the caret pulls into its left end"
    );
    assert!(k.drain_right_first, "…and drains from its right end");
    assert!(
        right < left,
        "200 ms into the retract the right end has spent more than the left ({right} vs {left})"
    );
    let (k, left, right) = below((3, 0));
    assert_eq!(
        k.retract_col,
        Some(23),
        "a row above the caret pulls into its right end — the fold — not toward col 0 on another row"
    );
    assert!(!k.drain_right_first);
    assert!(
        left < right,
        "…and drains from its left end ({left} vs {right})"
    );
}

fn cell_cov(rib: &Ribbon, g: Geom, col: u16) -> u8 {
    let x0 = f32::from(g.origin_x) + f32::from(col) * g.cw as f32;
    let x1 = x0 + g.cw as f32;
    rib.plan_segments()
        .iter()
        .filter(|s| s.x > x0 + 0.5 && s.x < x1 - 0.5)
        .map(|s| s.cov)
        .max()
        .unwrap_or(0)
}

#[test]
fn a_return_dims_the_line_it_leaves() {
    let c = cfg(false, true);
    let t0 = Instant::now();
    let lit_after = |licence: Licence| -> (u8, bool) {
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let leave = at(t0, 8 * 60 + 40);
        let cx = ctx(leave, &c, (3, 0), 0.9);
        rib.on_event(&row_change((2, 10), (3, 0), licence), leave, &cx);
        rib.plan(&cx);
        let late = at(leave, 100);
        let cx = ctx(late, &c, (3, 0), 0.9);
        rib.plan(&cx);
        let cov = rib.plan_segments().iter().map(|s| s.cov).max().unwrap_or(0);
        (cov, rib.cells().iter().any(|c| c.row == 2 && c.leaving()))
    };
    let (ret, ret_leaving) = lit_after(Licence::Return);
    let (nav, _) = lit_after(Licence::Nav);
    assert!(nav > 0, "control: an arrow down leaves the band lit");
    assert!(
        f32::from(ret) <= RETURN_PRINTED_SHARE * f32::from(nav) + 2.0,
        "the line a Return left still stands at {ret} of the arrow's {nav} at 100 ms"
    );
    assert!(
        ret > 0 && !ret_leaving,
        "…but dimmed, not cut: it goes out through its own swoosh"
    );
}

#[test]
fn the_paper_ribbon_keeps_its_yellow_clean() {
    for (fg, bg) in [(0x001F_2328, 0x00FF_FFFF), (0x002B_2D33, 0x00F6_F7F9)] {
        let mut c = cfg(false, true);
        (c.theme_fg, c.theme_bg) = (fg, bg);
        let mut lut = BedInkLut::default();
        lut.sync(&c);
        let a = (cov_cap(&c) + 16.0).min(255.0) as u8;
        let lay = |ink: u32| over_premul(bg, premul_rgb(ink, a), a) & 0x00FF_FFFF;
        let yellow = (0..BED_INK_LUT_LEN)
            .map(|i| i as f32 / (BED_INK_LUT_LEN - 1) as f32)
            .min_by(|x, y| {
                let d = |t: f32| {
                    let (h, ..) = rgb2hsv(spectrum(walk_pace(t)));
                    (h - 60.0).abs()
                };
                d(*x).total_cmp(&d(*y))
            })
            .expect("a walk");
        let (h, s, v) = rgb2hsv(lay(lut.at(yellow)));
        assert!(
            (48.0..=68.0).contains(&h) && v > 0.85 && s > 0.2,
            "on #{bg:06X} the yellow lays ({h:.0}°, {s:.2}, {v:.2})"
        );
        let law = crate::cursor_glow::light_body_ink(spectrum(walk_pace(yellow)), fg, bg, a);
        let (_, _, lv) = rgb2hsv(lay(law));
        assert!(
            lv < 0.8,
            "control: the body law's yellow is not olive ({lv:.2})"
        );
    }
    let (h, ..) = rgb2hsv(paper_caret_ink(
        crate::color_math::hsv2rgb(68.0, 1.0, 1.0),
        0x00FF_FFFF,
    ));
    assert!(
        h <= 45.0,
        "a greenish yellow's caret sits at hue {h:.1}, not amber"
    );
}

#[test]
fn a_retired_cell_melts_to_exactly_nothing_inside_150_ms_and_never_brightens() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.9);
    let last = at(t0, 7 * 60);
    let fired = at(last, 40);
    let cx = ctx(fired, &c, (2, 18), 0.9);
    rib.plan(&cx);
    assert_eq!(
        rib.slabs_per_cell(),
        SLABS_PER_CELL,
        "interior slabs exist to read"
    );
    let before = (cell_cov(&rib, g, 16), cell_cov(&rib, g, 17));
    assert!(
        before.0 > 0 && before.1 > 0,
        "the head cells are lit: {before:?}"
    );
    let mut control = rib.clone();
    let born_of = |rib: &Ribbon, col: u16| {
        rib.cells()
            .iter()
            .find(|l| l.row == 2 && l.col == col)
            .map(|l| l.born)
            .expect("laid")
    };
    let (b16, b17) = (born_of(&rib, 16), born_of(&rib, 17));
    assert_eq!(
        rib.retire_cells(&[(2, 15, fired)], fired),
        0,
        "a cell at a laid position with ANOTHER birth is not it (identity, not position)"
    );
    assert_eq!(
        rib.retire_cells(
            &[(2, 16, b16), (2, 17, b17), (2, 99, fired), (5, 16, b16)],
            fired
        ),
        2,
        "two live cells stamped; a cell that is not laid is not counted"
    );
    assert_eq!(
        rib.retire_cells(&[(2, 16, b16)], at(fired, 8)),
        0,
        "a retired cell is not restamped"
    );
    let mut prev = before;
    let mut gone_by: Option<u64> = None;
    for k in 1..=20u64 {
        let ms = 8 * k;
        let now = at(fired, ms);
        let cx = ctx(now, &c, (2, 18), 0.9);
        rib.plan(&cx);
        control.plan(&cx);
        let here = (cell_cov(&rib, g, 16), cell_cov(&rib, g, 17));
        assert!(
            here.0 <= prev.0 && here.1 <= prev.1,
            "+{ms} ms: the melt is monotone, got {here:?} after {prev:?}"
        );
        assert!(
            here.0 <= before.0 && here.1 <= before.1,
            "+{ms} ms: never brighter than the cell was ({before:?}), got {here:?}"
        );
        prev = here;
        let resident = rib
            .cells()
            .iter()
            .any(|l| l.row == 2 && (l.col == 16 || l.col == 17));
        if here == (0, 0) && !resident && gone_by.is_none() {
            gone_by = Some(ms);
        }
        for col in [10u16, 11, 12, 13] {
            assert_eq!(
                cell_cov(&rib, g, col),
                cell_cov(&control, g, col),
                "+{ms} ms: cell {col} is untouched by the retirement"
            );
        }
    }
    assert!(
        gone_by.is_some_and(|ms| ms <= 150),
        "off the glass and out of the pool inside 150 ms, got {gone_by:?}"
    );
    assert!(
        rib.cells().iter().any(|l| l.row == 2 && l.col == 15),
        "the un-retired band is still resident"
    );
}

#[test]
fn a_released_run_is_abandoned_into_its_retract_and_a_cell_already_leaving_keeps_its_clock() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 6, &c, 0.9);
    let last = at(t0, 5 * 60);
    erase_at(&mut rib, at(last, 50), (2, 15), &c);
    let stamp_15 = rib
        .cells()
        .iter()
        .find(|l| l.row == 2 && l.col == 15)
        .and_then(|l| l.retract_at);
    assert!(stamp_15.is_some(), "the erased cell is retracting");
    let named: Vec<(u16, u16, Instant)> = rib
        .cells()
        .iter()
        .filter(|l| l.row == 2)
        .map(|l| (l.row, l.col, l.born))
        .collect();
    let released = rib.release_cells(&named, at(last, 60));
    assert_eq!(
        released, 5,
        "the five live cells; the retracting one keeps its clock"
    );
    assert!(
        rib.cells()
            .iter()
            .all(|l| l.row != 2 || l.retire_at.is_none()),
        "not one cell is stamped on the melt"
    );
    assert_eq!(
        rib.cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 15)
            .and_then(|l| l.retract_at),
        stamp_15,
        "the retracting cell keeps its own clock"
    );
    let coh = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 2)
        .copied()
        .expect("the row's cohort");
    assert!(coh.abandoned, "the cohort is abandoned");
    assert!(!coh.rejoinable, "…and not rejoinable: the text is gone");
    let cx = ctx(at(last, 76), &c, (2, 15), 0.9);
    rib.plan(&cx);
    assert!(
        rib.cohorts()
            .iter()
            .find(|k| k.row == 2)
            .is_some_and(|k| k.phase.is_retracting()),
        "on the next plan the cohort is in its retract"
    );
    assert!(
        rib.cells().iter().filter(|l| l.row == 2).count() >= 5,
        "…and the five cells are still resident, drawn in, not cut"
    );
    let alive_at = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 2)
        .map(|k| k.alive_at);
    rib.release_cells(&named, at(last, 90));
    assert_eq!(
        rib.cohorts()
            .iter()
            .find(|k| k.row == 2)
            .map(|k| k.alive_at),
        alive_at,
        "a repeat release does not rewind the clock again"
    );
    let cx = ctx(
        at(
            last,
            60 + ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64 + 20,
        ),
        &c,
        (2, 15),
        0.9,
    );
    rib.plan(&cx);
    assert!(
        !rib.cells().iter().any(|l| l.row == 2),
        "the released run is out of the pool the retract's own span later, \
         not {RETIRE_MELT_S} s later"
    );
}

#[test]
fn partial_release_counts_follow_splits_and_reunions_without_weakening() {
    use super::super::witness::FollowRun;

    let c = cfg(true, true);
    let t0 = Instant::now();
    for hi in [12u16, 13] {
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        assert_eq!(rib.cohorts.len(), 1);
        let original = rib.cohorts[0];
        let named: Vec<_> = rib
            .cells
            .iter()
            .filter(|cell| (12..=13).contains(&cell.col))
            .map(|cell| (cell.row, cell.col, cell.born))
            .collect();
        let stamp = at(t0, 360);
        assert_eq!(rib.release_cells(&named, stamp), 2);
        let mut moved = Vec::new();
        assert_eq!(
            rib.translate_run(
                FollowRun {
                    row: 2,
                    cohort: original.id,
                    lo: 10,
                    hi,
                    dr: 1,
                },
                at(stamp, 10),
                false,
                &mut moved
            ),
            usize::from(hi - 9)
        );
        let split = rib
            .cohorts
            .iter()
            .find(|cohort| cohort.row == 3)
            .expect("split")
            .id;
        assert_eq!(rib.cohorts.len(), 2);
        assert!(
            rib.cohorts
                .iter()
                .all(|cohort| cohort.alive_at == original.alive_at)
        );
        assert!(
            rib.release_parts
                .iter()
                .all(|part| part.cells == 2 && part.stamp == stamp)
        );
        let mut accepted = Vec::new();
        if hi == 13 {
            assert_eq!(
                rib.release_parts.len(),
                1,
                "only the new identity has stamped cells"
            );
            assert_eq!(rib.release_parts[0].cohort, split);
            rib.restore_releases(&[(split, Some(stamp))], &mut accepted);
            assert_eq!(
                accepted,
                vec![split],
                "a whole release part carries its saved count"
            );
        } else {
            assert_eq!(rib.release_parts.len(), 2);
            rib.prune_release_parts();
            rib.restore_releases(
                &[(original.id, Some(stamp)), (split, Some(stamp))],
                &mut accepted,
            );
            assert!(
                accepted.is_empty(),
                "neither one-cell fragment satisfies its saved count of two"
            );
            assert_eq!(
                rib.release_parts.len(),
                2,
                "resident fragments retain their custody"
            );
            assert_eq!(
                rib.translate_run(
                    FollowRun {
                        row: 3,
                        cohort: split,
                        lo: 10,
                        hi,
                        dr: -1,
                    },
                    at(stamp, 20),
                    false,
                    &mut moved
                ),
                3
            );
            rib.heal_seams();
            assert_eq!(
                rib.cohorts.len(),
                1,
                "the original adjacent fragments reunite"
            );
            assert_eq!(rib.release_parts.len(), 1);
            assert_eq!(
                rib.release_parts[0].cells, 2,
                "reunion never sums cloned counts"
            );
            let reunited = rib.cohorts[0].id;
            rib.restore_releases(&[(reunited, Some(stamp))], &mut accepted);
            assert_eq!(accepted, vec![reunited]);
        }
        assert!(rib.release_parts.is_empty());
        for &(_, col, born) in &named {
            let restored = rib
                .cells
                .iter()
                .find(|cell| cell.col == col)
                .expect("restored cell");
            assert_eq!(restored.born, born);
            assert!(restored.released_at.is_none() && restored.retract_at.is_none());
        }
    }
}

#[test]
fn independent_same_stamp_releases_keep_the_first_count_guard_when_merged() {
    use super::super::witness::FollowRun;

    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 6, &c, 0.9);
    let original = rib.cohorts[0].id;
    let stamp = at(t0, 360);
    let mut moved = Vec::new();
    rib.translate_run(
        FollowRun {
            row: 2,
            cohort: original,
            lo: 10,
            hi: 12,
            dr: 1,
        },
        stamp,
        false,
        &mut moved,
    );
    let split = rib
        .cohorts
        .iter()
        .find(|cohort| cohort.row == 3)
        .expect("split")
        .id;
    let named: Vec<_> = rib
        .cells
        .iter()
        .filter(|cell| cell.col == 12 || cell.col == 15)
        .map(|cell| (cell.row, cell.col, cell.born))
        .collect();
    assert_eq!(rib.release_cells(&named, stamp), 2);
    assert_eq!(rib.release_parts.len(), 2);
    assert!(rib.release_parts.iter().all(|part| part.cells == 1));
    rib.translate_run(
        FollowRun {
            row: 3,
            cohort: split,
            lo: 10,
            hi: 12,
            dr: -1,
        },
        at(stamp, 10),
        false,
        &mut moved,
    );
    let mut revoked = rib.clone();
    revoked
        .cohorts
        .iter_mut()
        .find(|cohort| cohort.id == original)
        .expect("merge source")
        .revoke_releases(at(stamp, 1));
    revoked.heal_seams();
    assert_eq!(revoked.cohorts.len(), 1);
    revoked.settle_revoked_parts();
    assert!(
        revoked.release_parts.is_empty(),
        "a merged source's pending cutoff follows its custody"
    );
    rib.heal_seams();
    assert_eq!(rib.cohorts.len(), 1);
    assert_eq!(rib.release_parts.len(), 1);
    assert_eq!(
        rib.release_parts[0].cells, 1,
        "the first saved count is not widened by a collision"
    );
    assert_eq!(
        rib.cells
            .iter()
            .filter(|cell| cell.released_at == Some(stamp))
            .count(),
        2
    );
    let held = rib
        .cells
        .iter()
        .find(|cell| cell.released_at == Some(stamp))
        .expect("a released identity remains resident");
    assert!(
        !rib.partial_custody_valid(held, at(stamp, 20)),
        "a saved-count collision cannot suppress a genuine revive"
    );
    let mut accepted = Vec::new();
    rib.restore_releases(&[(rib.cohorts[0].id, Some(stamp))], &mut accepted);
    assert!(
        accepted.is_empty(),
        "independent batches with the same stamp still fail the saved-count guard"
    );
    let mut replacement = *rib
        .cells
        .iter()
        .find(|cell| cell.col == 12)
        .expect("released cell");
    replacement.born = at(stamp, 20);
    replacement.attack_at = replacement.born;
    replacement.released_at = None;
    replacement.retract_at = None;
    rib.place(0, replacement, replacement.born, 2);
    assert_eq!(rib.release_parts.len(), 1);
    assert_eq!(
        rib.release_parts[0].cells, 0,
        "the first count can reach zero while another fragment remains"
    );
    let held = rib
        .cells
        .iter()
        .find(|cell| cell.released_at == Some(stamp))
        .expect("the zero-count guard still has a resident fragment");
    assert!(
        !rib.partial_custody_valid(held, at(stamp, 20)),
        "a zero saved count never makes resident custody valid"
    );
    rib.restore_releases(&[(rib.cohorts[0].id, Some(stamp))], &mut accepted);
    assert!(
        accepted.is_empty(),
        "zero is retained as a refusal guard, never recomputed from residents"
    );
}

#[test]
fn partial_release_pool_keeps_counts_and_cleans_up_with_its_cells() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 6, &c, 0.9);
    let cohort = rib.cohorts[0].id;
    let stamp = at(t0, 360);
    let named: Vec<_> = rib
        .cells
        .iter()
        .filter(|cell| cell.col >= 14)
        .map(|cell| (cell.row, cell.col, cell.born))
        .collect();
    assert_eq!(rib.release_cells(&named, stamp), 2);
    rib.plan(&ctx(at(stamp, 300), &c, (2, 16), 0.9));
    assert_eq!(
        rib.release_parts.len(),
        1,
        "dark custody outlives its visual retract"
    );
    assert_eq!(rib.release_parts[0].cells, 2);
    assert!(rib.field_at(2, 14).is_none() && rib.field_at(2, 15).is_none());
    let now = at(stamp, 300);
    let held = |rib: &Ribbon| {
        rib.cells
            .iter()
            .find(|cell| cell.col == 14)
            .copied()
            .expect("the original cell remains resident")
    };
    assert!(
        rib.partial_custody_valid(&held(&rib), now),
        "a complete dark release part remains eligible to return"
    );
    assert!(
        !rib.partial_custody_valid(&rib.cells[0], now),
        "standing unstamped ink is not partial custody"
    );

    let mut short_lived = rib.clone();
    let index = short_lived
        .cells
        .iter()
        .position(|cell| cell.col == 14)
        .unwrap();
    let since = short_lived.cells[index]
        .born
        .max(short_lived.cohorts[0].alive_at);
    short_lived.cells[index].life_s = now.saturating_duration_since(since).as_secs_f32();
    assert!(
        short_lived.partial_custody_valid(
            &held(&short_lived),
            now - std::time::Duration::from_millis(1)
        ),
        "the cell is eligible immediately before its own expiry"
    );
    assert!(
        !short_lived.partial_custody_valid(&held(&short_lived), now),
        "the cell's expiry is exclusive even while its cohort stands"
    );

    let mut retired = rib.clone();
    retired.cells[index].retire_at = Some(now);
    assert!(
        !retired.partial_custody_valid(&held(&retired), now),
        "a content-retired cell cannot borrow partial custody"
    );

    let mut whole = rib.clone();
    let standing: Vec<_> = whole
        .cells
        .iter()
        .filter(|cell| !cell.leaving())
        .map(|cell| (cell.row, cell.col, cell.born))
        .collect();
    assert_eq!(whole.release_cells(&standing, now), 4);
    assert!(whole.cohorts[0].abandoned && whole.cohorts[0].release_clock.is_some());
    assert!(
        !whole.partial_custody_valid(&held(&whole), now),
        "a whole clear abandons the earlier partial custody too"
    );

    let mut replaced = rib.clone();
    let mut replacement = *replaced
        .cells
        .iter()
        .find(|cell| cell.col == 14)
        .expect("old cell");
    replacement.born = at(stamp, 310);
    replacement.attack_at = replacement.born;
    replacement.released_at = None;
    replacement.retract_at = None;
    replaced.place(0, replacement, replacement.born, 2);
    assert_eq!(
        replaced.release_parts[0].cells, 1,
        "an explicit replacement decrements the saved count"
    );
    let mut accepted = Vec::new();
    replaced.restore_releases(&[(cohort, Some(stamp))], &mut accepted);
    assert_eq!(accepted, vec![cohort]);
    assert!(replaced.release_parts.is_empty());

    let mut revoked = rib.clone();
    revoked.cohorts[0].revoke_releases(stamp);
    assert!(
        revoked.partial_custody_valid(&held(&revoked), now),
        "an equal pending cutoff preserves its own stamp"
    );
    revoked.settle_revoked_parts();
    assert_eq!(
        revoked.release_parts.len(),
        1,
        "the revocation cutoff is strictly earlier stamps"
    );
    assert!(revoked.partial_custody_valid(&held(&revoked), now));
    revoked.cohorts[0].revoke_releases(at(stamp, 1));
    assert!(
        !revoked.partial_custody_valid(&held(&revoked), now),
        "a newer pending cutoff already revokes custody before settlement"
    );
    revoked.settle_revoked_parts();
    assert!(!revoked.partial_custody_valid(&held(&revoked), now));
    revoked.restore_releases(&[(cohort, Some(stamp))], &mut accepted);
    assert!(accepted.is_empty() && revoked.release_parts.is_empty());

    let mut expired = rib.clone();
    expired.cells[index].life_s = 100.0;
    assert!(expired.cohorts[0].leaving_at(at(stamp, 30_000)));
    assert!(
        !expired.partial_custody_valid(&held(&expired), at(stamp, 30_000)),
        "an expired cohort cannot retain even a longer-lived cell's custody"
    );
    expired.plan(&ctx(at(stamp, 30_000), &c, (2, 16), 0.0));
    assert!(expired.cells.is_empty() && expired.release_parts.is_empty());
    let mut reset = rib.clone();
    reset.reset();
    assert!(reset.release_parts.is_empty());
    let mut scrolled = rib.clone();
    scrolled.translate_scroll(1);
    assert_eq!(
        scrolled.release_parts.len(),
        1,
        "a surviving row keeps its count"
    );
    scrolled.translate_scroll(2);
    assert!(scrolled.cells.is_empty() && scrolled.release_parts.is_empty());
    let mut band = rib;
    band.translate_band(2, 2, -1);
    assert!(band.cells.is_empty() && band.release_parts.is_empty());
}

#[test]
fn a_partial_release_is_restored_by_its_stamped_cells_when_a_key_joined_the_cohort() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 6, &c, 0.9);
    let last = at(t0, 5 * 60);
    let id = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 2)
        .map(|k| k.id)
        .expect("the row's cohort");
    let born_15 = rib
        .cells()
        .iter()
        .find(|l| l.row == 2 && l.col == 15)
        .map(|l| l.born)
        .expect("cell 15");
    let cell_15 = |rib: &Ribbon| {
        rib.cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 15 && l.born == born_15)
            .copied()
            .expect("cell 15 is resident")
    };
    let stamp = at(last, 60);
    assert_eq!(
        rib.release_cells(&[(2, 15, born_15)], stamp),
        1,
        "one cell stamped"
    );
    assert_eq!(cell_15(&rib).released_at, Some(stamp));
    assert!(cell_15(&rib).leaving(), "…and leaving");
    for reduced_motion in [false, true] {
        let mut c = cfg(true, true);
        c.reduced_motion = reduced_motion;
        let mut held = rib.clone();
        let mut absent = rib.clone();
        absent.cells.retain(|cell| cell.released_at != Some(stamp));
        for ms in [300, 400, 500] {
            let now = at(stamp, ms);
            let cx = ctx(now, &c, (2, 16), 0.9);
            held.plan(&cx);
            absent.plan(&cx);
            assert_eq!(cell_15(&held).released_at, Some(stamp));
            assert!(held.field_at(2, 15).is_none(), "custody is dark");
            assert_eq!(held.cells.len(), absent.cells.len() + 1);
            let expected = absent.next_change_deadline(now).expect("standing prefix");
            assert!(
                expected > now + super::super::TAIL_FLOOR,
                "reduced={reduced_motion}, +{ms}ms: the prefix has no immediate fade"
            );
            assert_eq!(
                held.next_change_deadline(now),
                Some(expected),
                "reduced={reduced_motion}, +{ms}ms: dark custody adds no visual wake"
            );
        }
    }
    let key = at(last, 80);
    let cx = ctx(key, &c, (2, 17), 0.9);
    rib.on_event(&typed(), key, &cx);
    rib.plan(&cx);
    let on_row: Vec<(u16, u32)> = rib
        .cells()
        .iter()
        .filter(|l| l.row == 2)
        .map(|l| (l.col, l.cohort))
        .collect();
    assert_eq!(
        rib.cohorts().iter().filter(|k| k.row == 2).count(),
        1,
        "one cohort on the row: {on_row:?}"
    );
    let count = rib.cells().iter().filter(|l| l.cohort == id).count();
    assert_eq!(count, 7, "the cohort grew by one: {on_row:?}");
    let alive_at = rib
        .cohorts()
        .iter()
        .find(|k| k.id == id)
        .map(|k| k.alive_at);
    let mut accepted = Vec::new();
    rib.restore_releases(&[(id, Some(stamp))], &mut accepted);
    assert_eq!(
        accepted,
        vec![id],
        "the restore is accepted by the stamp's own cells, the cohort's count notwithstanding"
    );
    assert!(!cell_15(&rib).leaving(), "the stamp is lifted");
    assert_eq!(cell_15(&rib).released_at, None);
    let coh = rib
        .cohorts()
        .iter()
        .find(|k| k.id == id)
        .copied()
        .expect("the cohort");
    assert!(coh.release_clock.is_none(), "the clock is handed back");
    assert_eq!(Some(coh.alive_at), alive_at, "…and was never touched");
    assert!(!coh.abandoned);
    rib.restore_releases(&[(id, Some(stamp))], &mut accepted);
    assert!(accepted.is_empty(), "nothing is released any more");
}

#[test]
fn a_relay_over_a_content_retired_cell_has_a_fresh_witness_identity() {
    use super::super::witness::{RowSample, Witness};

    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 2, &c, 0.9);
    let born = |rib: &Ribbon, col| {
        rib.cells()
            .iter()
            .find(|cell| cell.row == 2 && cell.col == col)
            .expect("typed cell")
            .born
    };
    let old = born(&rib, 10);
    let neighbor = born(&rib, 11);
    let mut witness = Witness::new();
    let mut retired = Vec::new();
    let mut released = Vec::new();
    let original: Vec<char> = "          ab".chars().collect();
    witness.walk(
        rib.cells(),
        &[RowSample {
            row: 2,
            cols: &original,
        }],
        &[],
        Instant::now(),
        &mut retired,
        &mut released,
    );
    assert!(retired.is_empty());
    let fired = at(t0, 100);
    assert_eq!(rib.retire_cells(&[(2, 10, old)], fired), 1);
    let fresh = at(t0, 116);
    rib.on_event(&typed(), fresh, &ctx(fresh, &c, (2, 11), 0.9));
    assert_eq!(born(&rib, 10), fresh, "a retired identity cannot be re-wet");
    assert_eq!(born(&rib, 11), neighbor, "the neighboring key is untouched");
    assert_eq!(rib.retire_cells(&[(2, 10, old)], fresh), 0);

    let replacement: Vec<char> = "          Xb".chars().collect();
    witness.walk(
        rib.cells(),
        &[RowSample {
            row: 2,
            cols: &replacement,
        }],
        &[],
        Instant::now(),
        &mut retired,
        &mut released,
    );
    assert!(retired.is_empty(), "the fresh key arms its own record");
    let changed: Vec<char> = "          Yb".chars().collect();
    witness.walk(
        rib.cells(),
        &[RowSample {
            row: 2,
            cols: &changed,
        }],
        &[],
        Instant::now(),
        &mut retired,
        &mut released,
    );
    assert_eq!(
        retired,
        vec![(2, 10, fresh)],
        "later changes retire the new identity"
    );
}

#[test]
fn a_relocation_pays_a_content_retired_destination_and_preserves_live_neighbors() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 3, &c, 0.9);
    let before: Vec<_> = rib
        .cells()
        .iter()
        .map(|cell| (cell.col, cell.born))
        .collect();
    let old = before
        .iter()
        .find(|&&(col, _)| col == 11)
        .expect("middle")
        .1;
    let fired = at(t0, 160);
    assert_eq!(rib.retire_cells(&[(2, 11, old)], fired), 1);
    let fresh = at(t0, 176);
    rib.relay_word(2, 10, 13, fresh, None);
    for col in 10..13 {
        let cell = rib
            .cells()
            .iter()
            .find(|cell| cell.row == 2 && cell.col == col)
            .expect("relocated word");
        assert!(
            !cell.leaving(),
            "column {col} belongs to the relocated word"
        );
        let expected = if col == 11 {
            fresh
        } else {
            before.iter().find(|&&(c, _)| c == col).expect("neighbor").1
        };
        assert_eq!(
            cell.born, expected,
            "only the retired destination is paid again"
        );
    }
    assert_eq!(
        rib.retire_cells(&[(2, 11, old)], fresh),
        0,
        "stale identity has no authority over the relocation"
    );
}

#[test]
fn a_live_retype_with_a_changed_glyph_does_not_inherit_the_old_witness() {
    use super::super::witness::{RowSample, Witness};

    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 1, &c, 0.9);
    let mut witness = Witness::new();
    let mut retired = Vec::new();
    let mut released = Vec::new();
    let original: Vec<char> = "          a".chars().collect();
    witness.walk(
        rib.cells(),
        &[RowSample {
            row: 2,
            cols: &original,
        }],
        &[],
        Instant::now(),
        &mut retired,
        &mut released,
    );
    assert!(retired.is_empty());
    let before_ctx = ctx(at(t0, 99), &c, (2, 11), 0.9);
    rib.plan(&before_ctx);
    let before = cell_cov(&rib, geom(), 10);
    assert!(before > 0, "the old glyph is visibly lit");
    let old_attack = rib.cells()[0].attack_at;
    let fresh = at(t0, 100);
    rib.on_event(&typed(), fresh, &ctx(fresh, &c, (2, 11), 0.9));
    let after_ctx = ctx(at(t0, 101), &c, (2, 11), 0.9);
    rib.plan(&after_ctx);
    assert_eq!(rib.cells()[0].born, fresh, "fresh key, fresh identity");
    assert_eq!(rib.cells()[0].attack_at, old_attack);
    assert!(
        cell_cov(&rib, geom(), 10) >= before,
        "re-wetting cannot dim the old light"
    );
    assert!(
        !rib.brisk(after_ctx.now),
        "a settled attack is not restarted for scheduling either"
    );
    let replacement: Vec<char> = "          X".chars().collect();
    witness.walk(
        rib.cells(),
        &[RowSample {
            row: 2,
            cols: &replacement,
        }],
        &[],
        Instant::now(),
        &mut retired,
        &mut released,
    );
    assert!(
        retired.is_empty(),
        "a genuine retyped glyph must not be mistaken for an unlicensed overwrite: {retired:?}"
    );
}

#[test]
fn focus_regained_inside_the_ember_comes_back_up_through_edge_in_not_a_snap() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 6, &c, 0.8);
    let last = at(t0, 5 * 60);
    let cx = ctx(at(last, 100), &c, (2, 16), 0.8);
    rib.plan(&cx);
    let full = plan_peak(&rib);
    let full_sum = plan_sum(&rib);
    rib.on_event(&Event::Focus(false), at(last, 100), &cx);
    let cx = ctx(at(last, 250), &c, (2, 16), 0.8);
    rib.plan(&cx);
    let dimmed = plan_peak(&rib);
    let dim_sum = plan_sum(&rib);
    let mut twin = Ribbon::new();
    type_run(&mut twin, t0, 10, 6, &c, 0.8);
    let blur_cx = ctx(at(last, 100), &c, (2, 16), 0.8);
    twin.on_event(&Event::Focus(false), at(last, 100), &blur_cx);
    let end_cx = ctx(at(last, 399), &c, (2, 16), 0.8);
    twin.plan(&end_cx);
    let floor_sum = plan_sum(&twin);
    let range = full_sum.saturating_sub(floor_sum);
    let burned = full_sum.saturating_sub(dim_sum);
    assert!(
        dimmed < full,
        "the ember must be burning ({full} → {dimmed})"
    );
    assert!(
        range > 0 && floor_sum > 0,
        "the fixture must leave the band SOME range to spend and some light at the floor \
         ({full_sum} → {floor_sum})"
    );
    assert!(
        burned as f32 >= range as f32 * (1.0 - spend(0.5) - 0.05),
        "the ember must be well under way: {burned} of {range} spendable \
         ({full_sum} → {dim_sum}, floor {floor_sum})"
    );
    rib.on_event(&Event::Focus(true), at(last, 250), &cx);
    let cx = ctx(at(last, 251), &c, (2, 16), 0.8);
    rib.plan(&cx);
    let back = plan_peak(&rib);
    assert!(
        back <= dimmed + 3,
        "focus regain snapped the ribbon from {dimmed} to {back} in one frame"
    );
    let cx = ctx(at(last, 290), &c, (2, 16), 0.8);
    rib.plan(&cx);
    let rearmed = plan_peak(&rib);
    assert!(
        rearmed + 3 >= full,
        "after the edge-in the ribbon must be back at its level ({full} vs {rearmed})"
    );
}

const fn census_paint(px: u32, light: bool) -> bool {
    let r = ((px >> 16) & 0xff) as u8 as f32;
    let g = ((px >> 8) & 0xff) as u8 as f32;
    let b = (px & 0xff) as u8 as f32;
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let level = if light {
        mn <= 255.0 - 60.0
    } else {
        mx >= 60.0
    };
    level && mx - mn >= 40.0
}

fn lit_row(rib: &Ribbon, cx: &Ctx<'_>, row: u16, scale: f32) -> Vec<u16> {
    let bg = cx.cfg.theme_bg;
    let light = !cx.cfg.dark_theme;
    let mut out: Vec<u16> = Vec::new();
    for cell in rib.cells().iter().filter(|c| c.row == row) {
        let coh = rib.cohorts.iter().find(|c| c.id == cell.cohort);
        let a = (rib.cov_of(cx, cell, coh) * scale)
            .round()
            .clamp(0.0, 255.0) as u8;
        if a > 0 && census_paint(over_premul(bg, premul_rgb(rib.ink.at(cell.t), a), a), light) {
            out.push(cell.col);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn dark_run(lit: &[u16]) -> u16 {
    let (mut worst, mut run) = (0u16, 0u16);
    let (Some(&first), Some(&last)) = (lit.first(), lit.last()) else {
        return 0;
    };
    for col in first..=last {
        if lit.contains(&col) {
            run = 0;
        } else {
            run += 1;
            worst = worst.max(run);
        }
    }
    worst
}

#[test]
fn the_focus_ember_never_opens_a_dark_run_inside_the_band() {
    for (theme, fg, bg) in [
        ("default #111318", SHIPPED_FG, SHIPPED_BG),
        ("Nord", NORD_FG, NORD_BG),
    ] {
        let mut c = cfg(true, true);
        c.theme_fg = fg;
        c.theme_bg = bg;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 2,
                n: 60,
                period_ms: 40,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 59 * 40);
        let caret = (2u16, 62u16);
        let cx = ctx(at(last, 60), &c, caret, 0.9);
        rib.plan(&cx);
        let full = lit_row(&rib, &cx, 2, 1.0);
        assert!(
            full.len() >= 40,
            "{theme}: the fixture's band is lit before the blur ({} cells)",
            full.len()
        );
        assert_eq!(dark_run(&full), 0, "{theme}: the fixture starts whole");
        let blur = at(last, 60);
        rib.on_event(&Event::Focus(false), blur, &cx);
        let (mut worst_new, mut worst_bare, mut bare_split_frames) = (0u16, 0u16, 0u32);
        let mut shrunk_while_lit = Vec::new();
        for ms in (0..=300).step_by(4) {
            let now = at(blur, ms);
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
            let burned = ms as f32 / 1000.0 / FOCUS_EMBER_S;
            let now_lit = lit_row(&rib, &cx, 2, 1.0);
            let burning = rib.ember_at.take();
            let bare_lit = lit_row(&rib, &cx, 2, spend(clamp01(burned)));
            rib.ember_at = burning;
            worst_new = worst_new.max(dark_run(&now_lit));
            let bd = dark_run(&bare_lit);
            worst_bare = worst_bare.max(bd);
            if bd >= 2 {
                bare_split_frames += 1;
            }
            if !now_lit.is_empty() && now_lit != full {
                shrunk_while_lit.push((ms, now_lit.len()));
            }
        }
        assert_eq!(
            worst_new, 0,
            "{theme}: the ember opened a dark run of {worst_new} cells inside the band"
        );
        assert!(
            shrunk_while_lit.is_empty(),
            "{theme}: cells left the band before the end of the ember: {shrunk_while_lit:?}"
        );
        let gone = ctx(at(blur, 301), &c, (2, 62), 0.0);
        rib.plan(&gone);
        assert!(
            lit_row(&rib, &gone, 2, 1.0).is_empty(),
            "{theme}: the ember's end takes every cell"
        );
        assert!(
            worst_bare >= 2 && bare_split_frames >= 2,
            "{theme}: the fixture cannot show the defect it pins \
             (worst bare-spend dark run {worst_bare} cells over {bare_split_frames} frames)"
        );
    }
}

/// On every dark built-in and pack theme, every stop the bed's bar
/// leaves as paint at full coverage ([`census_paint`]) is still paint at
/// the coverage the ember leaves it at: the ember may dim a stop, never
/// take it off the glass. Negative control: at half the floor the census
/// loses stops on every ground (replaces the dark half of the pruned
/// `the_ember_never_takes_a_stop_under_the_census_on_any_shipped_ground`).
#[test]
fn the_ember_never_takes_a_stop_off_the_glass_on_any_dark_theme() {
    use aterm_types::scheme::{builtin, builtin_names, parse_theme_str};
    let rgb = |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
    let grounds: Vec<(String, u32, u32)> = builtin_names()
        .into_iter()
        .filter_map(|n| {
            let s = builtin(n)?;
            Some((n.to_owned(), rgb(s.foreground), rgb(s.background)))
        })
        .chain(aterm_theme_pack::themes().iter().map(|t| {
            let parsed = parse_theme_str(t.text).expect("a pack theme parses");
            let s = parsed.scheme;
            (t.id.to_owned(), rgb(s.foreground), rgb(s.background))
        }))
        .filter(|&(_, _, bg)| aterm_render::theme_is_dark(bg))
        .collect();
    assert!(
        grounds.len() >= 30,
        "the dark theme table went missing ({} grounds)",
        grounds.len()
    );
    for (name, fg, bg) in &grounds {
        let mut c = cfg(true, true);
        c.theme_fg = *fg;
        c.theme_bg = *bg;
        let mut lut = BedInkLut::default();
        lut.sync(&c);
        let cap = cov_cap(&c);
        let lay = |ink: u32, cov: f32| {
            let a = cov.clamp(0.0, cap) as u8;
            over_premul(*bg, premul_rgb(ink, a), a)
        };
        let (mut lit, mut lost, mut lost_halved) = (0u32, Vec::new(), 0u32);
        for i in 0..BED_INK_LUT_LEN {
            let t = i as f32 / (BED_INK_LUT_LEN - 1) as f32;
            let ink = lut.at(t);
            if !census_paint(lay(ink, cap), false) {
                // The bar already leaves this stop under the instrument
                // (Solarized Dark's foreground does it to half the arc):
                // not the ember's to fix.
                continue;
            }
            lit += 1;
            let floor = lut.seen_floor_at(t) * EMBER_FLOOR_MARGIN;
            let px = lay(ink, floor);
            if !census_paint(px, false) {
                lost.push(format!("t={t:.2} #{px:06X}"));
            }
            lost_halved += u32::from(!census_paint(lay(ink, floor * 0.5), false));
        }
        assert!(lit > 0, "{name}: fixture: the bar leaves no stop lit");
        assert!(
            lost.is_empty(),
            "{name} #{bg:06X}: the ember took {} stops the bar left lit: {:?}",
            lost.len(),
            &lost[..lost.len().min(6)]
        );
        assert!(
            lost_halved > 0,
            "control ({name}): at half the floor the census loses no stop"
        );
    }
}

#[test]
fn the_hot_edge_embers_out_with_its_body_on_focus_loss() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.9);
    let lost = at(t0, 7 * 60 + 100);
    let mut sink = Sink::default();
    let frame = |rib: &mut Ribbon, sink: &mut Sink, now: Instant| {
        let cx = ctx(now, &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    };
    frame(&mut rib, &mut sink, lost);
    let full = peak_channel(&sink.out);
    assert!(
        full > 0,
        "a hot hand must leave a hot edge for the law to have been tested"
    );
    rib.on_event(&Event::Focus(false), lost, &ctx(lost, &c, (2, 18), 0.9));
    frame(&mut rib, &mut sink, at(lost, 150));
    let mid = peak_channel(&sink.out);
    assert!(
        mid < full,
        "halfway through the ember the hot edge must already be spending ({full} → {mid})"
    );
    frame(&mut rib, &mut sink, at(lost, 250));
    assert!(
        !sink.under.is_empty(),
        "the body must still be on glass for the pin to mean anything"
    );
    let low = peak_channel(&sink.out);
    let cx = ctx(at(lost, 250), &c, (2, 18), 0.9);
    let head = rib
        .cells()
        .iter()
        .max_by_key(|l| l.col)
        .copied()
        .expect("the head is in the pool");
    let env = rib.env_of(&cx, &head);
    let share = low as f32 / full as f32;
    assert!(
        (share - env).abs() < 0.06,
        "+250 ms into the ember the hot edge is at {low} of {full} (share {share:.3}); \
         it must ride the head cell's own envelope ({env:.3}), not a clock of its own"
    );
    let end_cx = ctx(at(lost, 299), &c, (2, 18), 0.9);
    let env_end = rib.env_of(&end_cx, &head);
    let law = env_end + (1.0 - env_end) * spend(250.0 / 300.0);
    assert!(
        low <= mid && (env - law).abs() < 0.03 && env_end < 1.0,
        "…and it must be going out with its body on the ember's own clock, not holding \
         until the pool clears ({full} → {mid} → {low}, envelope {env:.3}, the ember's \
         law at this instant {law:.3}, its floor {env_end:.3})"
    );
    frame(&mut rib, &mut sink, at(lost, 300));
    assert!(
        sink.out.is_empty() && rib.at_rest() && rib.next_change_deadline(at(lost, 300)).is_none(),
        "…and at the ember's end there is nothing, hairline included, and no wake asked for"
    );
}

#[test]
fn a_typed_key_under_a_burning_ember_clears_it_and_relights_from_the_level() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.9);
    let lost = at(t0, 7 * 60 + 100);
    rib.plan(&ctx(lost, &c, (2, 18), 0.9));
    let pre = plan_peak(&rib);
    rib.on_event(&Event::Focus(false), lost, &ctx(lost, &c, (2, 18), 0.9));
    let cx = ctx(at(lost, 150), &c, (2, 18), 0.9);
    rib.plan(&cx);
    let mid = plan_peak(&rib);
    let full = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
    assert!(
        mid > 0 && mid < pre,
        "halfway through the ember the body is spending ({pre} → {mid}, cold ceiling {full})"
    );
    let key = at(lost, 150);
    let cx = ctx(key, &c, (2, 19), 0.9);
    rib.on_event(&typed(), key, &cx);
    rib.plan(&cx);
    assert!(
        plan_peak(&rib) >= mid,
        "the key re-arms the run from the ember's level, never below it ({} < {mid})",
        plan_peak(&rib)
    );
    let cx = ctx(at(key, 30), &c, (2, 19), 0.9);
    rib.plan(&cx);
    assert!(
        plan_peak(&rib) >= full - 2,
        "one edge-in after the key the run is back at its ceiling ({})",
        plan_peak(&rib)
    );
    let cx = ctx(at(lost, 320), &c, (2, 19), 0.9);
    rib.plan(&cx);
    assert!(
        !rib.at_rest() && plan_peak(&rib) >= full - 2,
        "past the ember's end the ribbon is still on the glass: the key cleared the ember"
    );
}

#[test]
fn a_curtain_gathers_the_band_into_its_hand_over_a_quarter_second_with_no_cliff_and_lays_nothing() {
    let c = cfg(true, true);
    let g = geom();
    let cw = g.cw as f32;
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 20, &c, 0.9);
    let last = at(t0, 19 * 60);
    let caret = (3u16, 0u16);
    let mut sink = Sink::default();
    #[derive(Clone, Copy)]
    struct Sample {
        peak: u8,
        sum: u32,
        left: f32,
        cells: usize,
        segs: usize,
        ink: usize,
    }
    let frame = |rib: &mut Ribbon, sink: &mut Sink, now: Instant| -> Sample {
        let cx = ctx(now, &c, caret, 0.9);
        rib.plan(&cx);
        let segs = rib.lit_segments().count();
        let ink = rib.ink_segments().count();
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        Sample {
            peak: sink.under.iter().map(|q| q.alpha).max().unwrap_or(0),
            sum: sink.under.iter().map(|q| u32::from(q.alpha)).sum(),
            left: sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min),
            cells: (0..40u16)
                .filter(|&col| cell_lit(&sink.under, g, 2, col))
                .count(),
            segs,
            ink,
        }
    };
    let before = frame(&mut rib, &mut sink, at(last, 8));
    assert!(
        before.peak > 200 && before.cells == 20 && before.segs >= 20,
        "the band is whole before the seam (peak {}, {} cells, {} segments)",
        before.peak,
        before.cells,
        before.segs
    );
    let fell = at(last, 16);
    rib.curtain(fell);
    assert!(rib.curtained());
    assert!(rib.brisk(fell), "a falling curtain asks for every frame");
    assert!(
        rib.next_change_deadline(fell)
            .is_some_and(|d| d <= fell + Duration::from_secs_f32(CURTAIN_S)),
        "…and books its end"
    );
    let cells = rib.cells().len();
    let cx = ctx(at(fell, 40), &c, (3, 1), 0.9);
    rib.on_event(&typed(), at(fell, 40), &cx);
    rib.on_event(
        &Event::Sweep {
            row: 3,
            col0: 0,
            col1: 1,
        },
        at(fell, 40),
        &cx,
    );
    rib.on_event(&nav((3, 1), (3, 40)), at(fell, 40), &cx);
    assert_eq!(rib.cells().len(), cells, "a curtain lays nothing");
    let mut prev = before;
    let mut worst_step = 0usize;
    let mut worst_ink_step = 0usize;
    let mut ink_frames = 0usize;
    let mut dark_at = None;
    for ms in [33u64, 66, 99, 132, 165, 198, 231, 264, 297, 330] {
        let now = frame(&mut rib, &mut sink, at(fell, ms));
        assert!(
            now.peak <= prev.peak,
            "+{ms} ms: the curtain never brightens ({} → {})",
            prev.peak,
            now.peak
        );
        assert!(
            now.sum <= prev.sum + prev.sum / 500,
            "+{ms} ms: the band's total light rose ({} → {})",
            prev.sum,
            now.sum
        );
        if now.sum > 0 {
            assert!(
                now.left >= prev.left - 0.5,
                "+{ms} ms: the band is drawn toward its head, never away ({} → {})",
                prev.left,
                now.left
            );
        }
        let step = prev.segs.saturating_sub(now.segs);
        let ink_step = prev.ink.saturating_sub(now.ink);
        worst_step = worst_step.max(step);
        worst_ink_step = worst_ink_step.max(ink_step);
        let bar = CURTAIN_MAX_STEP_FRAC * before.segs.max(before.ink) as f32;
        assert!(
            step as f32 <= bar && ink_step as f32 <= bar,
            "+{ms} ms: the curtain stepped {step} claimed / {ink_step} inked segments in one \
             frame ({} → {} / {} → {}); the bound is {bar:.1} \
             ({CURTAIN_MAX_STEP_FRAC} of the {} it fell on)",
            prev.segs,
            now.segs,
            prev.ink,
            now.ink,
            before.segs.max(before.ink)
        );
        if now.ink > 0 {
            ink_frames += 1;
        }
        if now.ink == 0 && dark_at.is_none() {
            dark_at = Some(ms);
        }
        prev = now;
    }
    assert!(
        dark_at.is_some_and(|ms| ms >= 132),
        "the band went dark at {dark_at:?} ms — a curtain lasts CURTAIN_S"
    );
    assert!(
        worst_ink_step > 0,
        "the sweep never saw the band leave: it was already dark"
    );
    assert!(
        prev.sum * 8 < before.sum,
        "the fall spent nothing: {} → {} over the whole sweep",
        before.sum,
        prev.sum
    );
    assert!(
        ink_frames >= CURTAIN_MIN_FRAMES,
        "the curtain drew ink on {ink_frames} of the swept frames; the law is \
         {CURTAIN_MIN_FRAMES} (a cut draws one, a one-clock curtain two)"
    );
    let end = frame(&mut rib, &mut sink, at(fell, 250));
    assert_eq!((end.peak, end.segs, end.ink), (0, 0, 0));
    assert!(rib.at_rest() && !rib.curtained());
    let after = at(fell, 300);
    let cx = ctx(after, &c, (4, 6), 0.9);
    rib.on_event(&typed(), after, &cx);
    rib.plan(&cx);
    assert_eq!(
        rib.cells().len(),
        1,
        "after the curtain a key lays its cell"
    );
    for seam_caret in [(3u16, 0u16), (2, 0)] {
        let mut rib2 = Ribbon::new();
        type_run(&mut rib2, t0, 10, 20, &c, 0.9);
        let cx = ctx(at(last, 8), &c, seam_caret, 0.9);
        rib2.plan(&cx);
        rib2.curtain(fell);
        let near_end = at(fell, 216);
        let cx = ctx(near_end, &c, seam_caret, 0.9);
        rib2.plan(&cx);
        {
            let mut f2 = sink.frame();
            rib2.emit(&cx, &mut f2);
        }
        let left_x = sink
            .under
            .iter()
            .map(|q| f32::from(q.x))
            .fold(f32::INFINITY, f32::min);
        let head_x = f32::from(g.origin_x) + 30.0 * cw;
        assert!(
            !sink.under.is_empty(),
            "seam caret {seam_caret:?}: the band was gone before the gather could be read"
        );
        assert!(
            (left_x - head_x).abs() <= 3.0 * cw,
            "seam caret {seam_caret:?}: the band gathered to x {left_x:.1}, not to its own \
             head at {head_x:.1} (column 0 is x {:.1}, {:.0} cells away)",
            f32::from(g.origin_x),
            (head_x - f32::from(g.origin_x)) / cw
        );
    }
}

#[test]
fn tall_and_underline_price_light_alike_and_differ_only_in_reach_above_the_spine() {
    let t0 = Instant::now();
    let mut peaks = Vec::new();
    let mut tops = Vec::new();
    for c in [cfg(true, true), cfg(true, false)] {
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.7);
        let cx = ctx(at(t0, 5 * 60), &c, (2, 16), 0.7);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        peaks.push(sink.under.iter().map(|q| q.alpha).max().unwrap_or(0));
        tops.push(sink.under.iter().map(|q| q.y).min().unwrap_or(0));
    }
    assert_eq!(
        peaks[0], peaks[1],
        "the tall and underline spellings must price light identically"
    );
    assert!(
        tops[0] < tops[1],
        "the tall body's top edge must sit above the underline's ({tops:?})"
    );
}

/// THE UNDERLINE KEEPS TO A LAST ROW'S FOOT (looks10 L3): typed along the
/// pane's last row, where the tall ribbon is lifted off the floor by its
/// reach below, the underline keeps to the row's foot band, no higher
/// than its reach above the row's bottom, and lays nothing past the
/// floor; on a row with room below it keeps its reach below. Negative
/// control: lifted as the tall ribbon is, as it was, the underline's top
/// edge rises into the lower half of the row's letters.
#[test]
fn the_underline_keeps_to_a_last_rows_foot() {
    let g = geom();
    let chf = g.ch as f32;
    let t0 = Instant::now();
    let band = |row: u16, lifted: bool| {
        let c = cfg(true, false);
        let mut rib = Ribbon::new();
        rib.underline_lifted = lifted;
        let keys = Keys {
            g,
            row,
            col0: 10,
            n: 6,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let cx = ctx_in(at(t0, 5 * 60 + 30), &c, (row, 16), 0.9, g);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let lit: Vec<_> = sink
            .under
            .iter()
            .filter(|q| q.color & 0x00FF_FFFF != 0)
            .collect();
        assert!(!lit.is_empty(), "row {row}: the underline laid nothing");
        let top = lit
            .iter()
            .map(|q| f32::from(q.y))
            .fold(f32::INFINITY, f32::min);
        let bottom = lit
            .iter()
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(0.0, f32::max);
        (top, bottom, (f32::from(row) + 1.0) * chf)
    };
    let last = u16::try_from(g.rows - 1).expect("a row");
    let (top, bottom, row_bot) = band(last, false);
    assert!(
        top >= row_bot - UNDERLINE_UP_CH * chf - 1.0,
        "on the last row the underline rose to y {top:.1}, over its foot band from {:.1}",
        row_bot - UNDERLINE_UP_CH * chf
    );
    assert!(
        bottom <= row_bot + 0.5,
        "on the last row the underline laid to y {bottom:.1}, past the floor at {row_bot:.1}"
    );
    let (_, mid_bottom, mid_bot) = band(2, false);
    assert!(
        mid_bottom > mid_bot + 0.5 * DN_FLOOR_CH * chf,
        "with room below the underline keeps its reach below: to y {mid_bottom:.1} \
         from the row's bottom at {mid_bot:.1}"
    );
    let (old_top, _, _) = band(last, true);
    assert!(
        old_top < row_bot - 0.4 * chf,
        "control: lifted as the tall ribbon is, the underline still keeps to the foot \
         (top y {old_top:.1}, row bottom {row_bot:.1})"
    );
}

/// THE UNDERLINE SETTLES AT ONCE OFF THE LETTERS BELOW (looks10 round 3):
/// a lit underline whose next row inks under it lays nothing of its band
/// past its row's bottom from the first frame the letters land, where the
/// tall ribbon eases into its row over [`HOLD_EASE_S`]
/// (`a_line_whose_next_row_inks_settles_into_its_row_over_the_ease`). The
/// rail, laid clear of the row's ring and fading with the hold, is the
/// letter guard's to judge and is not read here. Negative control: eased
/// with the hold, as it was, the first frame's band still reaches most of
/// its free reach below the row.
#[test]
fn the_underline_settles_at_once_off_the_letters_below() {
    let g = geom();
    let row_bottom = f32::from(g.origin_y) + 3.0 * g.ch as f32;
    let mut inked = GlyphProbe::new();
    inked.probe_row(3, &[[true; 30], [false; 30]].concat());
    let mut blank = GlyphProbe::new();
    blank.probe_row(3, &[false; 120]);
    let c = cfg(true, false);
    let deepest = |rib: &mut Ribbon, now: Instant, probe: &GlyphProbe| {
        let cx = ctx(now, &c, (2, 18), 0.9);
        rib.plan_over(&cx, Some(probe));
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let rails = rib.rail_spans();
        let in_rail = |i: usize| {
            rails
                .iter()
                .any(|(from, to, _)| (from.0..to.0).contains(&i))
        };
        sink.under
            .iter()
            .enumerate()
            .filter(|&(i, q)| q.color != 0 && !in_rail(i))
            .map(|(_, q)| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max)
            - row_bottom
    };
    for eases in [false, true] {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.underline_eases_off_letters = eases;
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let free = deepest(&mut rib, at(t0, 8 * 60), &blank);
        assert!(
            free > 2.0,
            "premise: a free underline reaches {free:.1} px below its row"
        );
        let first = deepest(&mut rib, at(t0, 8 * 60 + 16), &inked);
        assert!(
            first.is_finite(),
            "eases {eases}: the underline is lit the frame the letters land"
        );
        if eases {
            assert!(
                first > 0.5 * free,
                "negative control: eased with the hold, the first frame's band reaches \
                 {first:.1} of its {free:.1} px below the row"
            );
        } else {
            assert!(
                first <= 0.5,
                "the frame the letters land below, the underline's band still reaches \
                 {first:.1} px past its row's bottom (free {free:.1})"
            );
        }
    }
}

#[test]
fn the_light_bed_keeps_theme_ink_over_it_at_the_bar() {
    fn contrast(a: u32, b: u32) -> f32 {
        let (x, y) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }
    let blend_at = |c: &Config, ink: u32| {
        let a = (cov_cap(c) + 16.0).min(255.0) as u8;
        over_premul(c.theme_bg, premul_rgb(ink, a), a) & 0x00FF_FFFF
    };
    let mut themes = vec![(0x001F_2328, 0x00FF_FFFF)];
    for bg in [0x00FF_FFFF, 0x00FD_F6E3] {
        for grey in (0..=0x33u32).step_by(0x11) {
            themes.push(((grey << 16) | (grey << 8) | grey, bg));
        }
    }
    for (fg, bg) in themes {
        let mut c = cfg(false, true);
        c.theme_fg = fg;
        c.theme_bg = bg;
        let mut lut = BedInkLut::default();
        lut.sync(&c);
        for &ink in &lut.lut {
            let blend = blend_at(&c, ink);
            assert!(
                contrast(fg, blend) >= BODY_CONTRAST_BAR,
                "{ink:06x} under {fg:06x} on {bg:06x}: {:.2}:1",
                contrast(fg, blend)
            );
        }
    }
    let mut sol = cfg(false, true);
    sol.theme_fg = 0x0065_7B83;
    sol.theme_bg = 0x00FD_F6E3;
    let mut lut = BedInkLut::default();
    lut.sync(&sol);
    for &ink in &lut.lut {
        assert!(seen(blend_at(&sol, ink), sol.theme_bg), "{ink:06x} reads");
    }
    let old = blend_at(&cfg(false, true), InkRole::OverText.ink(0x0000_00FF));
    assert!(contrast(0x001F_2328, old) < 3.0, "the old table's blue");
}

/// On a light page every quad composites source-over and nothing is additive (L6; the hot
/// edge is dark-only). Main's paper ribbon keeps the leading role's cap; the glass (§2.2,
/// §15.2 1.7) is whole, over that cap, and still source-over: its wedges' runs (§15.2 1.8: its
/// oldest end eases in by height) may start or end at no opacity, but none is both, which the
/// raster draws additively.
#[test]
fn a_light_theme_ribbon_is_source_over_ink_and_never_additive() {
    let cap = InkRole::Leading.alpha_cap() as u8;
    let t0 = Instant::now();
    for (glass, c) in [
        (false, cfg(false, false)),
        (true, glass_cfg(cfg(false, true))),
    ] {
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let now = at(t0, 5 * 60);
        let cx = ctx(now, &c, (2, 16), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert_eq!(rib.ink.glass_lit(), glass, "fixture");
        assert!(
            !sink.under.is_empty(),
            "glass {glass}: the light ribbon still draws"
        );
        assert!(
            sink.out.is_empty(),
            "glass {glass}: no additive light on white — L6, and the hot edge is dark-only"
        );
        for q in &sink.under {
            assert!(
                q.alpha > 0 || q.alpha2 > 0,
                "glass {glass}: every light-theme quad composites SOURCE-OVER"
            );
            assert!(
                glass || q.alpha > 0,
                "each of main's paper quads starts lit"
            );
            if !glass {
                assert!(q.alpha <= cap, "light alpha {} over the {cap} cap", q.alpha);
            }
        }
        let peak = sink.under.iter().map(|q| q.alpha.max(q.alpha2)).max();
        if glass {
            assert!(
                peak.is_some_and(|p| p > cap),
                "the glass is whole, over the {cap} cap: {peak:?}"
            );
        }
    }
}

fn composite_at(under: &[GlowQuad], x: f32, y: f32) -> u32 {
    use aterm_render::add_sat;
    let (px, py) = (x.floor(), y.floor());
    under
        .iter()
        .filter(|q| {
            let qx = f32::from(q.x);
            (qx..qx + f32::from(q.w)).contains(&px) && f32::from(q.y) == py
        })
        .fold(DEFAULT_BG, |acc, q| {
            if q.alpha == 0 {
                add_sat(acc, q.color)
            } else {
                over_premul(acc, q.color, q.alpha)
            }
        })
}

#[test]
fn the_glyph_box_stays_at_the_bar_over_the_emitted_rail() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.9);
    let cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
    rib.plan(&cx);
    let mut sink = Sink::default();
    {
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }
    let g = geom();
    let (cw, ch) = (g.cw as f32, g.ch as f32);
    let row_top = f32::from(g.origin_y) + 2.0 * ch;
    let row_bottom = row_top + ch;
    let mut worst = f32::INFINITY;
    let mut worst_at = (0.0, 0.0);
    let mut x = f32::from(g.origin_x) + 10.0 * cw;
    while x < f32::from(g.origin_x) + 18.0 * cw {
        let mut y = row_top - 0.2 * ch;
        while y < row_bottom {
            let lit = composite_at(&sink.under, x, y);
            let k = contrast(DEFAULT_FG, lit);
            if k < worst {
                worst = k;
                worst_at = (x, y - row_bottom);
            }
            y += 1.0;
        }
        x += 1.0;
    }
    assert!(
        worst >= BODY_CONTRAST_BAR,
        "the text composites at {worst:.3}:1 at (x {}, {:.0} px above the row bottom); the bar is {BODY_CONTRAST_BAR}:1 over every row a letter can touch",
        worst_at.0,
        -worst_at.1
    );
}

/// The vivid rail stands a hairline below the row ([`RAIL_GAP_CH`]):
/// the first pixels under the row's bottom, where a descender's foot
/// meets the ground next to it, keep the text at the body's bar at every
/// phase of the wave, and the rail is still lit below them. Negative
/// control: the rail laid at the row's bottom (`rail_flush`) takes those
/// pixels under the bar.
#[test]
fn the_rail_leaves_the_descenders_a_hairline_of_body() {
    let c = cfg(true, true);
    let g = geom();
    let ch = g.ch as f32;
    let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
    let gap = rail_gap_px(ch);
    assert!(gap >= 1.0, "premise: a hairline of at least a pixel");
    for flush in [false, true] {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.rail_flush = flush;
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let (mut worst, mut rail_lit) = (f64::INFINITY, false);
        for ph in 0..8u16 {
            let mut cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
            cx.phase = f32::from(ph) / 8.0;
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let mut x = f32::from(g.origin_x) + 10.0 * g.cw as f32;
            while x < f32::from(g.origin_x) + 18.0 * g.cw as f32 {
                for d in 0..gap as i32 {
                    let lit = composite_at(&sink.under, x, row_bottom + d as f32);
                    worst = worst.min(f64::from(contrast(DEFAULT_FG, lit)));
                }
                for d in gap as i32..(gap as i32 + 4) {
                    let lit = composite_at(&sink.under, x, row_bottom + d as f32);
                    rail_lit |= contrast(DEFAULT_FG, lit) < 3.0;
                }
                x += 1.0;
            }
        }
        if flush {
            assert!(
                worst < f64::from(BODY_CONTRAST_BAR),
                "negative control: the flush rail meets the row's bottom ({worst:.2}:1)"
            );
        } else {
            assert!(
                worst >= f64::from(BODY_CONTRAST_BAR),
                "the hairline under the row reads the text at {worst:.2}:1, under the \
                 body's {BODY_CONTRAST_BAR}:1"
            );
            assert!(rail_lit, "the rail is still lit below the hairline");
        }
    }
}

/// looks8 K9, THE RAIL KEEPS OFF A FILLED ROW: over a filled, lettered
/// row below (a selection) the rail lies whole in its own row's foot
/// band ([`ROW_RAIL_CH`]), parted round a descender's cell, and lays
/// none on the filled row; the row's hold over the letters below had
/// withheld the rail, and on Solarized Dark the ribbons drew a bed and
/// no rail there. Negative controls: kept below its row, and on a row
/// whose glyphs nobody named (its feet unknown), the held row lays no
/// rail in its own row nor on the filled one.
#[test]
fn the_rail_keeps_off_a_filled_row_below() {
    let c = cfg(true, true);
    let g = geom();
    let ch = g.ch as f32;
    let (ox, cw) = (i32::from(g.origin_x), g.cw as i32);
    let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
    let band = (ROW_RAIL_CH * ch).floor().max(1.0);
    let filled = (
        ox,
        ox + 40 * cw,
        row_bottom as i32,
        (row_bottom + ch) as i32,
    );
    let row = (row_bottom - ch) as i32;
    let sole = (ox, ox + 40 * cw, row, row_bottom as i32);
    let foot = (ox + 14 * cw, ox + 15 * cw, row, row_bottom as i32);
    // The rail's light (alpha 32 or more in the rail's own inks) in its
    // row's foot band and on the filled row, and the foot rail's within
    // the judge's ring of the foot's cell.
    let mut inked = GlyphProbe::new();
    inked.probe_row(3, &[true; 40]);
    let ring = crate::cursor_glow::LETTER_GUARD_RING;
    let rail_light = |off: bool, named: bool| -> (usize, usize, usize) {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.row_rail_off = off;
        rib.keep_out.set_filled([filled]);
        if named {
            rib.keep_out.set_feet([foot], [sole]);
        }
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        rib.plan_over(&ctx(at(t0, 8 * 60), &c, (2, 18), 0.9), Some(&inked));
        let judged_at = at(t0, 8 * 60 + (HOLD_EASE_S * 1000.0) as u64 + 16);
        let cx = ctx(judged_at, &c, (2, 18), 0.9);
        rib.plan_over(&cx, Some(&inked));
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let inks: Vec<u32> = (0..BED_INK_LUT_LEN)
            .map(|i| rail_ink(walk_arc_at(i)))
            .collect();
        let rail = |q: &&GlowQuad| {
            q.alpha >= 32 && {
                let straight =
                    |sh: u32| (((q.color >> sh) & 0xFF) * 255 / u32::from(q.alpha)).min(255);
                let rgb = (straight(16) << 16) | (straight(8) << 8) | straight(0);
                inks.iter().any(|&ink| {
                    [16u32, 8, 0]
                        .iter()
                        .all(|&sh| ((ink >> sh) & 0xFF).abs_diff((rgb >> sh) & 0xFF) <= 6)
                })
            }
        };
        let in_band = |q: &&GlowQuad| {
            let (y0, y1) = (f32::from(q.y), f32::from(q.y) + f32::from(q.h));
            y0 >= row_bottom - band && y1 <= row_bottom
        };
        let in_row = sink.under.iter().filter(rail).filter(in_band).count();
        let at_foot = rib
            .rail_spans()
            .iter()
            .filter(|span| span.2)
            .flat_map(|&(from, to, _)| &sink.under[from.0..to.0])
            .filter(|q| q.alpha | q.alpha2 > 0)
            .filter(|q| {
                let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
                x0 < foot.1 + ring && x1 > foot.0 - ring
            })
            .count();
        let on_fill = sink
            .under
            .iter()
            .filter(rail)
            .filter(|q| f32::from(q.y) + f32::from(q.h) > row_bottom)
            .count();
        (in_row, on_fill, at_foot)
    };
    let (kept, kept_fill, at_foot) = rail_light(false, true);
    assert!(
        kept >= 20,
        "the rail lays {kept} quads in its row's foot band"
    );
    assert_eq!(
        kept_fill, 0,
        "the rail lays {kept_fill} quads on the filled row"
    );
    assert_eq!(
        at_foot, 0,
        "the rail lays {at_foot} quads within the judge's ring of the foot's cell"
    );
    for (off, named) in [(true, true), (false, false)] {
        let (bare, bare_fill, _) = rail_light(off, named);
        assert!(
            bare == 0 && bare_fill == 0,
            "control (kept below {off}, glyphs named {named}): the held row lays {bare} \
             rail quads in its row and {bare_fill} on the filled one"
        );
    }
}

/// looks7 round 2, A DIM PAGE'S BED: on a dim dark theme every stop of
/// the walk lays its bed at the page's own luminance (never over it,
/// never more than a rounding under it), in its own colour (the walk's
/// reds and blues stand well off the page in OKLab), and laid at half
/// opacity over the page (a soft edge's middle) keeps three quarters of
/// the page's luminance; a theme past the dim bar keeps its bed byte for
/// byte. Negative control: the bed held toward black (its forerunner,
/// [`dim_edge_held`]) lays a shade under half the page's luminance on
/// Solarized Dark (the gallery's maroon-black `#150301`).
#[test]
fn a_dim_theme_s_bed_lays_its_hue_at_the_page_s_luminance() {
    const SOLARIZED_PAGE: u32 = 0x0000_2B36;
    let stops = |fg: u32, held: &dyn Fn(u32, u32, u32) -> u32, page: u32| -> Vec<u32> {
        (0..64)
            .map(|i| {
                let arc = walk_arc_at(i * BED_INK_LUT_LEN / 64);
                held(bed_ink(arc, bed_luma_budget(fg)), fg, page)
            })
            .collect()
    };
    for (fg, page) in [(0x0083_9496, SOLARIZED_PAGE), (0x0085_8585, 0x0011_1318)] {
        assert!(
            dim_theme(fg, page),
            "fixture: {fg:06x} on {page:06x} is dim"
        );
        let lp = relative_luminance(page);
        let beds = stops(fg, &dim_page_bed, page);
        let mut far = 0.0f32;
        for (i, &bed) in beds.iter().enumerate() {
            let l = relative_luminance(bed);
            assert!(
                l <= lp && l >= 0.95 * lp,
                "{fg:06x} on {page:06x}: stop {i} bed {bed:06x} at {l:.4}, the page at {lp:.4}"
            );
            let half = over_premul(page, premul_rgb(bed, 128), 128) & 0x00FF_FFFF;
            let lh = relative_luminance(half);
            assert!(
                lh >= 0.75 * lp,
                "{fg:06x} on {page:06x}: stop {i} bed {bed:06x} at half opacity reads {lh:.4}, \
                 under three quarters of the page's {lp:.4}"
            );
            far = far.max(crate::color_math::oklab_distance(bed, page));
        }
        assert!(
            far >= 0.08,
            "{fg:06x} on {page:06x}: the walk lies on the page ({far:.3} OKLab at most)"
        );
    }
    let fg = 0x00D0_D0D0;
    for i in 0..64 {
        let arc = walk_arc_at(i * BED_INK_LUT_LEN / 64);
        let bed = bed_ink(arc, bed_luma_budget(fg));
        assert_eq!(
            dim_page_bed(bed, fg, 0x001E_1E1E),
            bed,
            "a bright theme's bed is its own"
        );
    }
    let fg = 0x0083_9496;
    let lp = relative_luminance(SOLARIZED_PAGE);
    let darkest = stops(fg, &dim_edge_held, SOLARIZED_PAGE)
        .into_iter()
        .map(relative_luminance)
        .fold(f32::INFINITY, f32::min);
    assert!(
        darkest < 0.5 * lp,
        "control: the bed held toward black lays no shade ({darkest:.4} on the page's {lp:.4})"
    );
}

#[test]
fn on_paper_the_pet_s_ribbon_and_the_flat_one_read_apart() {
    // On paper the pet's ribbon keeps its under-edge (ink below the row's
    // bottom past the hairline) and the flat one has none and a lower
    // reach; drawn alike (the negative control) the two columns match.
    let g = geom();
    let ch = g.ch as f32;
    let row_top = f32::from(g.origin_y) + 2.0 * ch;
    let row_bottom = row_top + ch;
    let gap = rail_gap_px(ch);
    let profile = |c: &Config, alike: bool| -> (u32, f32) {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.paper_alike = alike;
        type_run(&mut rib, t0, 10, 8, c, 0.9);
        let cx = ctx(at(t0, 8 * 60), c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        // The ink laid on the white page at `(x, y)`: its deepest
        // channel's fall from the page.
        let ink = |x: f32, y: f32| {
            let (px, py) = (x.floor(), y.floor());
            let p = sink
                .under
                .iter()
                .filter(|q| {
                    let qx = f32::from(q.x);
                    (qx..qx + f32::from(q.w)).contains(&px) && f32::from(q.y) == py
                })
                .fold(0x00FF_FFFF, |acc, q| over_premul(acc, q.color, q.alpha));
            [16u32, 8, 0]
                .iter()
                .map(|&sh| 255 - ((p >> sh) & 0xff))
                .max()
                .unwrap_or(0)
        };
        let (mut under, mut top) = (0u32, row_bottom);
        let mut x = f32::from(g.origin_x) + 12.0 * g.cw as f32;
        while x < f32::from(g.origin_x) + 17.0 * g.cw as f32 {
            for d in gap as i32..(gap as i32 + 4) {
                under += ink(x, row_bottom + d as f32);
            }
            if let Some(y) = (0..(2.0 * ch) as i32)
                .map(|k| row_bottom - 2.0 * ch + k as f32)
                .find(|&y| ink(x, y) > 24)
            {
                top = top.min(y);
            }
            x += 1.0;
        }
        (under, top)
    };
    let (pet_under, pet_top) = profile(&cfg(false, true), false);
    let (flat_under, flat_top) = profile(&flat(false, true), false);
    assert!(
        5 * pet_under >= 8 * flat_under.max(1),
        "the pet's under-edge on paper ({pet_under}) against the flat one's ({flat_under})"
    );
    assert!(
        flat_top >= pet_top + 3.0,
        "the flat ribbon reaches as high as the pet's ({flat_top} vs {pet_top})"
    );
    let (alike_pet, alike_pet_top) = profile(&cfg(false, true), true);
    let (alike_flat, alike_flat_top) = profile(&flat(false, true), true);
    assert!(
        5 * alike_pet < 8 * alike_flat.max(1) && (alike_flat_top - alike_pet_top).abs() < 3.0,
        "control: drawn alike, the two still read apart ({alike_pet} vs {alike_flat}, \
         tops {alike_pet_top} vs {alike_flat_top})"
    );
}

/// looks9 Q6, THE FLAT RIBBON ON PAPER IS A SOFT EVEN BAND: on paper
/// the flat ribbon and the pet's were near twins at 1x, the flat lacking
/// only the pet's rail (`flat-rainbow-light.t0` against
/// `rainbow-kitty-pet-light.t0`). As its rail is on a dim page (looks8
/// K3), its body is now a soft even band: at most two thirds of the
/// pet's light, with no lit plateau, and as light under the run's middle
/// as at its brightest. Negative control: laid as the pet's glow, it
/// holds a plateau at the pet's light.
#[test]
fn on_paper_the_flat_ribbon_is_a_soft_even_band() {
    let g = geom();
    let ch = g.ch as f32;
    let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
    // Each body pixel column's alpha profile above the row's bottom.
    let profile = |c: &Config, glow: bool| -> std::collections::BTreeMap<u16, Vec<u8>> {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.flat_paper_glow = glow;
        type_run(&mut rib, t0, 10, 8, c, 0.9);
        let cx = ctx(at(t0, 8 * 60), c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let mut cols: std::collections::BTreeMap<u16, Vec<u8>> = Default::default();
        for q in sink
            .under
            .iter()
            .filter(|q| f32::from(q.y) + f32::from(q.h) <= row_bottom && q.alpha > 0)
        {
            for x in q.x..q.x + q.w {
                for _ in q.y..q.y + q.h {
                    cols.entry(x).or_default().push(q.alpha);
                }
            }
        }
        cols
    };
    let peak = |p: &std::collections::BTreeMap<u16, Vec<u8>>| {
        p.values().flatten().copied().max().unwrap_or(0)
    };
    let plateau = |p: &std::collections::BTreeMap<u16, Vec<u8>>| {
        p.values()
            .map(|col| {
                let top = col.iter().copied().max().unwrap_or(0);
                col.iter()
                    .filter(|&&a| u32::from(a) * 20 >= u32::from(top) * 19)
                    .count()
            })
            .max()
            .unwrap_or(0)
    };
    // The least column top under the run's middle (cells 12 to 16) as a
    // share of the brightest.
    let evenness = |p: &std::collections::BTreeMap<u16, Vec<u8>>| {
        let x0 = g.origin_x + 12 * g.cw as u16;
        let x1 = g.origin_x + 17 * g.cw as u16;
        let tops: Vec<u8> = p
            .range(x0..x1)
            .map(|(_, col)| col.iter().copied().max().unwrap_or(0))
            .collect();
        let (lo, hi) = (
            tops.iter().copied().min().unwrap_or(0),
            tops.iter().copied().max().unwrap_or(0),
        );
        f32::from(lo) / f32::from(hi.max(1))
    };
    let pet = profile(&cfg(false, true), false);
    let band = profile(&flat(false, true), false);
    let glow = profile(&flat(false, true), true);
    let (pp, bp, gp) = (peak(&pet), peak(&band), peak(&glow));
    assert!(
        bp > 0 && pp > 0,
        "fixture: both bodies are laid ({bp}, {pp})"
    );
    assert!(
        u32::from(bp) * 3 <= u32::from(pp) * 2,
        "the flat band peaks at {bp}, more than two thirds of the pet's {pp}"
    );
    let (band_plateau, glow_plateau) = (plateau(&band), plateau(&glow));
    assert!(
        band_plateau <= 3,
        "the flat band holds a plateau of {band_plateau} px"
    );
    assert!(
        glow_plateau >= band_plateau + 3,
        "control: laid as the glow the flat body holds only {glow_plateau} px at its peak"
    );
    let even = evenness(&band);
    assert!(
        even >= 0.9,
        "the flat band falls to {even:.2} of its light under the run"
    );
    assert!(
        u32::from(gp) * 3 > u32::from(pp) * 2,
        "control: laid as the glow the flat body peaks at {gp}, two thirds of the pet's {pp}"
    );
}

#[test]
fn the_flat_spelling_draws_no_rail() {
    let c = flat(true, true);
    assert!(!Ribbon::comet(&c) && !Ribbon::rail_lit(&c));
    assert!(Ribbon::comet(&cfg(true, true)) && Ribbon::rail_lit(&cfg(true, true)));
    // The pet's rail is lit on paper too, so L6's bed alone below the
    // baseline holds for the flat spelling only.
    assert!(
        Ribbon::rail_lit(&cfg(false, true)) && !Ribbon::rail_lit(&flat(false, true)),
        "on paper the pet keeps its under-edge and the flat spelling none"
    );
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 8, &c, 0.9);
    let cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
    rib.plan(&cx);
    let mut sink = Sink::default();
    {
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }
    let g = geom();
    let ch = g.ch as f32;
    let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
    for q in &sink.under {
        assert!(
            f32::from(q.y) < row_bottom + DN_TOP_CH * ch + 1.0,
            "the flat body reaches no deeper than {DN_TOP_CH} ch (y {} vs row bottom {row_bottom})",
            q.y
        );
        let lit = over_premul(DEFAULT_BG, q.color, q.alpha);
        assert!(
            contrast(DEFAULT_FG, lit) >= BODY_CONTRAST_BAR,
            "the flat body is the bed everywhere: #{lit:06X} at y {}",
            q.y
        );
    }
}

/// looks7 round 3, THE FLAT RIBBON ON A DIM PAGE: on Solarized Dark the
/// flat body lays only its hue at the page's luminance, and its warm half
/// is the page's own teal or olive (no rainbow past `jumps`); there it
/// lays a soft rail of the rainbow's light in the air below its letters,
/// a hairline under the row, in the rail's own inks. The pet's rail is
/// unchanged, and on the reference dark page and on paper the flat
/// spelling draws none. Negative control: the same run on the reference
/// dark page lays none of the rail's inks under the row.
#[test]
fn the_flat_spelling_lays_a_soft_rail_on_a_dim_page() {
    let dim = |mut c: Config| {
        (c.theme_fg, c.theme_bg) = (0x0083_9496, 0x0000_2B36);
        c
    };
    let sol_flat = dim(flat(true, true));
    assert!(Ribbon::flat_dim(&sol_flat) && Ribbon::rail_drawn(&sol_flat));
    assert!(
        !Ribbon::flat_dim(&dim(cfg(true, true))),
        "the pet keeps its own rail"
    );
    assert!(!Ribbon::flat_dim(&flat(true, true)) && !Ribbon::rail_drawn(&flat(true, true)));
    assert!(
        !Ribbon::rail_drawn(&flat(false, true)),
        "paper draws the flat spelling none"
    );
    let g = geom();
    let ch = g.ch as f32;
    let gap = rail_gap_px(ch);
    let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
    let rail_quads = |c: &Config| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, c, 0.9);
        let cx = ctx(at(t0, 8 * 60), c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let inks: Vec<u32> = (0..BED_INK_LUT_LEN)
            .map(|i| rail_ink(walk_arc_at(i)))
            .collect();
        sink.under
            .iter()
            .filter(|q| f32::from(q.y) >= row_bottom + gap && q.alpha >= 32)
            .filter(|q| {
                let straight =
                    |sh: u32| (((q.color >> sh) & 0xFF) * 255 / u32::from(q.alpha)).min(255);
                let rgb = (straight(16) << 16) | (straight(8) << 8) | straight(0);
                inks.iter().any(|&ink| {
                    [16u32, 8, 0]
                        .iter()
                        .all(|&sh| ((ink >> sh) & 0xFF).abs_diff((rgb >> sh) & 0xFF) <= 6)
                })
            })
            .count()
    };
    let lit = rail_quads(&sol_flat);
    assert!(
        lit >= 40,
        "the flat ribbon lays {lit} rail quads under its row on a dim page"
    );
    let reference = rail_quads(&flat(true, true));
    assert_eq!(
        reference, 0,
        "control: on the reference dark page the flat ribbon lays {reference} rail quads"
    );
}

/// looks8 K3, THE FLAT RAIL KEEPS ITS OWN SIGNATURE ON A DIM PAGE: on
/// Solarized Dark the flat ribbon's rail is an even-height band at about
/// half the pet's peak with no full-strength plateau, where the pet's rail
/// keeps its lit core: the two no longer read alike (lift 74.6 against
/// 74.8, looks7 final). Negative control: the pet's own rail on the same
/// page holds a plateau at its peak.
#[test]
fn the_flat_rail_on_a_dim_page_is_a_soft_even_band_unlike_the_pets() {
    let dim = |mut c: Config| {
        (c.theme_fg, c.theme_bg) = (0x0083_9496, 0x0000_2B36);
        c
    };
    let g = geom();
    let ch = g.ch as f32;
    let floor = f32::from(g.origin_y) + 3.0 * ch + rail_gap_px(ch);
    // Each rail pixel column's alpha profile under the row, by y.
    let profile = |c: &Config| -> std::collections::BTreeMap<u16, Vec<(u16, u8)>> {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, c, 0.9);
        let cx = ctx(at(t0, 8 * 60), c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        // The rail's own inks: the body's and the bed's marks are not it.
        let inks: Vec<u32> = (0..BED_INK_LUT_LEN)
            .map(|i| rail_ink(walk_arc_at(i)))
            .collect();
        let rail_ink_of = |q: &&GlowQuad| {
            let straight = |sh: u32| (((q.color >> sh) & 0xFF) * 255 / u32::from(q.alpha)).min(255);
            let rgb = (straight(16) << 16) | (straight(8) << 8) | straight(0);
            inks.iter().any(|&ink| {
                [16u32, 8, 0]
                    .iter()
                    .all(|&sh| ((ink >> sh) & 0xFF).abs_diff((rgb >> sh) & 0xFF) <= 6)
            })
        };
        let mut cols: std::collections::BTreeMap<u16, Vec<(u16, u8)>> = Default::default();
        for q in sink
            .under
            .iter()
            .filter(|q| f32::from(q.y) >= floor && q.alpha > 0)
            .filter(rail_ink_of)
        {
            for x in q.x..q.x + q.w {
                for y in q.y..q.y + q.h {
                    cols.entry(x).or_default().push((y, q.alpha));
                }
            }
        }
        cols
    };
    let peak = |p: &std::collections::BTreeMap<u16, Vec<(u16, u8)>>| {
        p.values().flatten().map(|&(_, a)| a).max().unwrap_or(0)
    };
    let plateau = |p: &std::collections::BTreeMap<u16, Vec<(u16, u8)>>| {
        p.values()
            .map(|col| {
                let top = col.iter().map(|&(_, a)| a).max().unwrap_or(0);
                col.iter()
                    .filter(|&&(_, a)| u32::from(a) * 20 >= u32::from(top) * 19)
                    .count()
            })
            .max()
            .unwrap_or(0)
    };
    // The depth each column of the band's body is visible to (alpha 8
    // and up; its feathered ends, under three quarters of its peak,
    // taper).
    let depths = |p: &std::collections::BTreeMap<u16, Vec<(u16, u8)>>| {
        let top = peak(p);
        let d: Vec<u16> = p
            .values()
            .filter(|col| {
                col.len() > 2
                    && col.iter().map(|&(_, a)| u32::from(a)).max().unwrap_or(0) * 4
                        >= u32::from(top) * 3
            })
            .map(|col| {
                col.iter()
                    .filter(|&&(_, a)| a >= 8)
                    .map(|&(y, _)| y)
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        (
            d.iter().copied().min().unwrap_or(0),
            d.iter().copied().max().unwrap_or(0),
        )
    };
    let flat = profile(&dim(flat(true, true)));
    let pet = profile(&dim(cfg(true, true)));
    let (fp, pp) = (peak(&flat), peak(&pet));
    assert!(
        fp > 0 && pp > 0,
        "fixture: both rails are drawn ({fp}, {pp})"
    );
    assert!(
        u32::from(fp) * 10 <= u32::from(pp) * 6 && u32::from(fp) * 10 >= u32::from(pp) * 3,
        "the flat rail peaks at {fp}, about half the pet's {pp}"
    );
    let (shallow, deep) = depths(&flat);
    assert!(
        deep - shallow <= 1,
        "the flat rail is an even band: its depth runs {shallow}..{deep}"
    );
    let (flat_plateau, pet_plateau) = (plateau(&flat), plateau(&pet));
    assert!(
        pet_plateau >= 2,
        "control: the pet's rail holds a plateau ({pet_plateau} px at its peak)"
    );
    assert!(
        flat_plateau < pet_plateau,
        "the flat rail has no core: {flat_plateau} px at its peak against the pet's {pet_plateau}"
    );
}

/// A row of light whose next line holds ink stays inside its own row: no
/// body, core, wave crest or rail lies on the next line's ascenders, at any
/// phase of the wave, dark or light, tall, short or flat. The band is still
/// drawn in its row. Negative controls: the same typing over a row probed
/// blank, and over no probe at all, reaches below the row's bottom.
#[test]
fn a_row_with_ink_below_keeps_its_light_inside_the_row() {
    let g = geom();
    let ch = g.ch as f32;
    let row_top = f32::from(g.origin_y) + 2.0 * ch;
    let row_bottom = row_top + ch;
    let mut inked = GlyphProbe::new();
    inked.probe_row(3, &[[true; 30], [false; 30]].concat());
    let mut blank = GlyphProbe::new();
    blank.probe_row(3, &[false; 120]);
    // Ink only far from the run (cols 40..) holds nothing either.
    let mut aside = GlyphProbe::new();
    aside.probe_row(3, &[[false; 40], [true; 40]].concat());
    for (c, name) in [
        (cfg(true, true), "tall dark"),
        (cfg(true, false), "short dark"),
        (cfg(false, true), "tall light"),
        (flat(true, true), "flat dark"),
    ] {
        for (probe, held, what) in [
            (Some(&inked), true, "ink below"),
            (Some(&blank), false, "blank below"),
            (Some(&aside), false, "ink below, away from the run"),
            (None, false, "no probe"),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 8, &c, 0.9);
            // The line was typed with no probe: told of the row below, it
            // settles over the ease, judged once that is spent.
            rib.plan_over(&ctx(at(t0, 8 * 60), &c, (2, 18), 0.9), probe);
            let judged_at = at(t0, 8 * 60 + (HOLD_EASE_S * 1000.0) as u64 + 16);
            let mut deepest = f32::NEG_INFINITY;
            let mut in_row = 0usize;
            for step in 0..8u16 {
                let mut cx = ctx(judged_at, &c, (2, 18), 0.9);
                cx.phase = f32::from(step) / 8.0;
                rib.plan_over(&cx, probe);
                let mut sink = Sink::default();
                {
                    let mut f = sink.frame();
                    rib.emit(&cx, &mut f);
                }
                for q in sink.under.iter().filter(|q| q.color != 0) {
                    let bottom = f32::from(q.y) + f32::from(q.h);
                    deepest = deepest.max(bottom);
                    in_row += usize::from(f32::from(q.y) >= row_top && bottom <= row_bottom);
                }
                // Lifted into its row, the body keeps the typed letters at the bar.
                if held && c.dark_theme {
                    let cw = g.cw as f32;
                    let mut x = f32::from(g.origin_x) + 10.0 * cw;
                    while x < f32::from(g.origin_x) + 18.0 * cw {
                        let mut y = row_top;
                        while y < row_bottom {
                            let lit = composite_at(&sink.under, x, y);
                            let k = contrast(DEFAULT_FG, lit);
                            assert!(
                                k >= BODY_CONTRAST_BAR,
                                "{name}, {what}: the held body takes the typed letters to \
                                 {k:.2}:1 at x {x}, {:.0} px above the row bottom",
                                row_bottom - y
                            );
                            y += 1.0;
                        }
                        x += 1.0;
                    }
                }
            }
            assert!(in_row > 0, "{name}, {what}: the band must still be drawn");
            if held {
                assert!(
                    deepest <= row_bottom,
                    "{name}, {what}: light reaches {:.1} px past the row's bottom onto the \
                     next line's ascenders",
                    deepest - row_bottom
                );
            } else {
                assert!(
                    deepest > row_bottom + 1.0,
                    "negative control, {name}, {what}: the body's lower reach must lie \
                     below the row (deepest {:.1} px past it)",
                    deepest - row_bottom
                );
            }
        }
    }
}

/// A lit line whose next row inks under it settles into its row over
/// [`HOLD_EASE_S`], never in one frame: the frame after the ink lands it
/// still reaches below the row by most of its reach, and once the ease is
/// spent nothing does. Negative control: the hold stepped to its target
/// (`hold_steps`) is inside the row in the first frame.
#[test]
fn a_line_whose_next_row_inks_settles_into_its_row_over_the_ease() {
    let g = geom();
    let row_bottom = f32::from(g.origin_y) + 3.0 * g.ch as f32;
    let mut inked = GlyphProbe::new();
    inked.probe_row(3, &[[true; 30], [false; 30]].concat());
    let mut blank = GlyphProbe::new();
    blank.probe_row(3, &[false; 120]);
    let c = cfg(true, true);
    let deepest = |rib: &mut Ribbon, now: Instant, probe: &GlyphProbe| {
        let cx = ctx(now, &c, (2, 18), 0.9);
        rib.plan_over(&cx, Some(probe));
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        sink.under
            .iter()
            .filter(|q| q.color != 0)
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max)
            - row_bottom
    };
    for steps in [false, true] {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.hold_steps = steps;
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let lit = at(t0, 8 * 60);
        let free = deepest(&mut rib, lit, &blank);
        assert!(
            free > 2.0,
            "premise: a free line reaches {free:.1} px below"
        );
        let first = deepest(&mut rib, at(t0, 8 * 60 + 16), &inked);
        let settled = deepest(
            &mut rib,
            at(t0, 8 * 60 + 16 + (HOLD_EASE_S * 1000.0) as u64 + 16),
            &inked,
        );
        assert!(
            settled.is_finite(),
            "steps {steps}: the line is still lit once the ease is spent"
        );
        assert!(
            settled <= 0.0,
            "steps {steps}: once the ease is spent nothing reaches below the row \
             ({settled:.1} px)"
        );
        if steps {
            assert!(
                first <= 0.0,
                "negative control: a stepped hold is in its row in one frame ({first:.1} px)"
            );
        } else {
            assert!(
                first > 0.5 * free,
                "the frame after the ink lands the line still reaches {first:.1} of its \
                 {free:.1} px below: it eases in, not in one frame"
            );
        }
    }
    // A full reset ends the old row's custody. A new line at that row
    // starts free, whether the row below is blank or not yet known.
    for probe in [Some(&blank), None] {
        let t0 = Instant::now();
        let mut reset = Ribbon::new();
        type_run(&mut reset, t0, 10, 8, &c, 0.9);
        let held_at = at(t0, 8 * 60 + 16);
        deepest(&mut reset, held_at, &inked);
        let settled_at = at(held_at, (HOLD_EASE_S * 1000.0) as u64 + 16);
        assert!(deepest(&mut reset, settled_at, &inked) <= 0.0);
        reset.reset();

        let next = at(settled_at, 16);
        let mut fresh = Ribbon::new();
        for rib in [&mut reset, &mut fresh] {
            type_run(rib, next, 10, 8, &c, 0.9);
        }
        let cx = ctx(at(next, 8 * 60), &c, (2, 18), 0.9);
        let draw = |rib: &mut Ribbon| {
            rib.plan_over(&cx, probe);
            let mut sink = Sink::default();
            rib.emit(&cx, &mut sink.frame());
            sink.under
        };
        let (got, want) = (draw(&mut reset), draw(&mut fresh));
        assert!(
            want.iter()
                .any(|q| f32::from(q.y) + f32::from(q.h) > row_bottom + 2.0),
            "the fresh line still reaches below its row"
        );
        assert_eq!(
            got, want,
            "a reset must not lend the new line an old row's hold"
        );
    }
}

/// Typed on the pane's last row and carried up two rows by a scroll, a
/// line lays no light past its row's bottom, on the scroll's frame (the
/// probe cleared, knowing nothing below) and on every frame after (the
/// probe knowing the printed line). Negative control: the hold left
/// behind by the scroll (`holds_left_behind`) lays the body's lower reach
/// on the printed line in the scroll's frame.
#[test]
fn a_line_a_scroll_carries_up_keeps_its_rows_shape() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 18,
        ..geom()
    };
    let ch = g.ch as f32;
    let c = cfg(true, true);
    let bottom = 13u16;
    let echo = bottom - 2;
    let echo_bottom = f32::from(g.origin_y) + f32::from(echo + 1) * ch;
    let mut printed = GlyphProbe::new();
    printed.probe_row(i32::from(echo) + 1, &[[true; 30], [false; 30]].concat());
    let deepest = |rib: &mut Ribbon, now: Instant, probe: Option<&GlyphProbe>| {
        let cx = ctx_in(now, &c, (bottom, 2), 0.9, g);
        rib.plan_over(&cx, probe);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        sink.under
            .iter()
            .filter(|q| q.color != 0)
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max)
            - echo_bottom
    };
    for left_behind in [false, true] {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.holds_left_behind = left_behind;
        let keys = Keys {
            g,
            row: bottom,
            col0: 2,
            n: 12,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        rib.translate_scroll(2);
        let scrolled = deepest(&mut rib, at(t0, 12 * 60), None);
        assert!(
            scrolled.is_finite(),
            "left behind {left_behind}: the carried line is still lit"
        );
        if left_behind {
            assert!(
                scrolled > 2.0,
                "negative control: with the hold left behind the scroll's frame reaches \
                 only {scrolled:.1} px onto the printed line"
            );
            continue;
        }
        assert!(
            scrolled <= 0.0,
            "the scroll's frame lays light {scrolled:.1} px onto the printed line"
        );
        for ms in (16..=320).step_by(16) {
            let later = deepest(&mut rib, at(t0, 12 * 60 + ms), Some(&printed));
            assert!(
                later <= 0.0,
                "+{ms} ms: light reaches {later:.1} px onto the printed line"
            );
        }
    }
}

/// A content-proven repaint carries the line's held shape just as a VT scroll does.
/// The destination is outside the caret probe, so unknown glyphs must retain its
/// previous hold rather than make the carried line free in the repaint's first frame.
#[test]
fn a_proven_repaint_carries_a_held_rows_shape_through_unknown_glyphs() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 18,
        ..geom()
    };
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 2,
            n: 12,
            period_ms: 60,
            disp: 0.9,
        },
        &c,
    );
    let cohort = rib
        .cohorts()
        .iter()
        .find(|coh| coh.row == 13)
        .expect("the typed line has a cohort")
        .id;
    let carry = at(t0, 12 * 60);
    let mut moved = Vec::new();
    assert_eq!(
        rib.translate_run(
            super::super::witness::FollowRun {
                row: 13,
                cohort,
                lo: 2,
                hi: 13,
                dr: -2,
            },
            carry,
            true,
            &mut moved,
        ),
        12,
        "the production witness carry moved the complete typed line"
    );
    let row_bottom = f32::from(g.origin_y) + 12.0 * g.ch as f32;
    let cx = ctx_in(carry, &c, (13, 2), 0.9, g);
    rib.plan_over(&cx, None);
    let mut sink = Sink::default();
    rib.emit(&cx, &mut sink.frame());
    let deepest = sink
        .under
        .iter()
        .filter(|q| q.color != 0)
        .map(|q| f32::from(q.y) + f32::from(q.h))
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(deepest.is_finite(), "the carried line is still lit");
    assert!(
        deepest <= row_bottom,
        "the proven repaint loses the held shape: light jumps {:.1} px below its row",
        deepest - row_bottom
    );
}

/// A partial repaint leaves both halves held, and a free run joining the held
/// destination does not lower that row's floor. The geometry at each run's own
/// columns distinguishes both errors without reading the hold bookkeeping.
#[test]
fn a_partial_repaint_keeps_both_halves_and_a_stronger_destination_held() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 18,
        ..geom()
    };
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 11,
            col0: 2,
            n: 12,
            period_ms: 60,
            disp: 0.9,
        },
        &c,
    );
    let mut inked = GlyphProbe::new();
    inked.probe_row(12, &[true; 120]);
    let start = at(t0, 12 * 60);
    rib.plan_over(&ctx_in(start, &c, (11, 14), 0.9, g), Some(&inked));
    let carry = at(start, (HOLD_EASE_S * 1000.0) as u64 + 16);
    rib.plan_over(&ctx_in(carry, &c, (11, 14), 0.9, g), Some(&inked));
    let depth = |rib: &mut Ribbon, now: Instant, row: u16, col: u16| {
        let cx = ctx_in(now, &c, (13, 2), 0.9, g);
        rib.plan_over(&cx, None);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        let x = f32::from(g.origin_x) + (f32::from(col) + 0.5) * g.cw as f32;
        sink.under
            .iter()
            .filter(|q| q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w))
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max)
            - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
    };
    let cohort = rib
        .cohorts()
        .iter()
        .find(|coh| coh.row == 11)
        .expect("the held line")
        .id;
    let mut moved = Vec::new();
    assert_eq!(
        rib.translate_run(
            super::super::witness::FollowRun {
                row: 11,
                cohort,
                lo: 8,
                hi: 13,
                dr: -2,
            },
            carry,
            true,
            &mut moved,
        ),
        6
    );
    for (row, col) in [(11, 4), (9, 10)] {
        let below = depth(&mut rib, carry, row, col);
        assert!(below.is_finite(), "the partial run on row {row} stays lit");
        assert!(
            below <= 0.0,
            "row {row}'s partial run lost its held shape: {below:.1} px"
        );
    }

    rib.on_event(
        &nav((11, 14), (7, 20)),
        carry,
        &ctx_in(carry, &c, (7, 20), 0.9, g),
    );
    type_keys(
        &mut rib,
        carry,
        Keys {
            g,
            row: 7,
            col0: 20,
            n: 4,
            period_ms: 60,
            disp: 0.9,
        },
        &c,
    );
    let join = at(carry, 4 * 60);
    let free_depth = depth(&mut rib, join, 7, 22);
    assert!(
        free_depth > 2.0,
        "control: the joining run is free ({free_depth:.1} px)"
    );
    let cohort = rib
        .cohorts()
        .iter()
        .find(|coh| coh.row == 7)
        .expect("the free joining run")
        .id;
    assert_eq!(
        rib.translate_run(
            super::super::witness::FollowRun {
                row: 7,
                cohort,
                lo: 20,
                hi: 23,
                dr: 2,
            },
            join,
            true,
            &mut moved,
        ),
        4
    );
    let below = depth(&mut rib, join, 9, 22);
    assert!(below.is_finite(), "the joining run stays lit");
    assert!(
        below <= 0.0,
        "the joining run weakens the destination's hold: {below:.1} px"
    );
}

/// A repaint batch may move a held line onto a free one's old row before
/// moving that free line away. Each run carries its own previous shape,
/// including another hop of the first run in the same batch.
#[test]
fn a_repaint_batch_keeps_each_runs_previous_shape_across_multiple_hops() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 18,
        ..geom()
    };
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 7,
            col0: 2,
            n: 8,
            period_ms: 20,
            disp: 0.9,
        },
        &c,
    );
    let mut inked = GlyphProbe::new();
    inked.probe_row(8, &[true; 120]);
    let start = at(t0, 8 * 20);
    rib.plan_over(&ctx_in(start, &c, (7, 10), 0.9, g), Some(&inked));
    let next = at(start, (HOLD_EASE_S * 1000.0) as u64 + 16);
    rib.plan_over(&ctx_in(next, &c, (7, 10), 0.9, g), Some(&inked));
    // A pasted line leaves the first flowing; a navigation jump would
    // abandon it before the second line finished typing.
    rib.on_event(
        &Event::Move {
            from: (7, 10),
            to: (8, 20),
            licence: Licence::Insert,
            dir: Dir::Right,
        },
        next,
        &ctx_in(next, &c, (8, 20), 0.9, g),
    );
    type_keys(
        &mut rib,
        next,
        Keys {
            g,
            row: 8,
            col0: 20,
            n: 8,
            period_ms: 20,
            disp: 0.9,
        },
        &c,
    );
    let carry = at(next, 8 * 20);
    let depth = |rib: &mut Ribbon, row: u16, col: u16| {
        let cx = ctx_in(carry, &c, (13, 2), 0.9, g);
        rib.plan_over(&cx, None);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        let x = f32::from(g.origin_x) + (f32::from(col) + 0.5) * g.cw as f32;
        sink.under
            .iter()
            .filter(|q| q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w))
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max)
            - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
    };
    let held_depth = depth(&mut rib, 7, 5);
    let free_depth = depth(&mut rib, 8, 24);
    assert!(
        held_depth.is_finite() && held_depth <= 0.0,
        "the first run is held ({held_depth:.1} px)"
    );
    assert!(
        free_depth > 2.0,
        "the other run is free ({free_depth:.1} px)"
    );
    let held = rib
        .cohorts()
        .iter()
        .find(|coh| coh.row == 7)
        .expect("held run")
        .id;
    let free = rib
        .cohorts()
        .iter()
        .find(|coh| coh.row == 8)
        .expect("free run")
        .id;
    let mut moved = Vec::new();
    for (row, cohort, lo, hi, dr) in [(7, held, 2, 9, 1), (8, held, 2, 9, 2), (8, free, 20, 27, 3)]
    {
        assert_eq!(
            rib.translate_run(
                super::super::witness::FollowRun {
                    row,
                    cohort,
                    lo,
                    hi,
                    dr
                },
                carry,
                true,
                &mut moved
            ),
            8
        );
    }
    let held_depth = depth(&mut rib, 10, 5);
    let free_depth = depth(&mut rib, 11, 24);
    assert!(
        held_depth.is_finite() && held_depth <= 0.0,
        "a second hop lost the held run's shape ({held_depth:.1} px)"
    );
    assert!(
        free_depth > 2.0,
        "the later run inherited another run's hold ({free_depth:.1} px)"
    );
}

fn repaint_column_depth(
    rib: &mut Ribbon,
    c: &Config,
    g: Geom,
    now: Instant,
    row: u16,
    col: u16,
) -> f32 {
    let cx = ctx_in(now, c, (13, 2), 0.9, g);
    rib.plan_over(&cx, None);
    let mut sink = Sink::default();
    rib.emit(&cx, &mut sink.frame());
    let x = f32::from(g.origin_x) + (f32::from(col) + 0.5) * g.cw as f32;
    sink.under
        .iter()
        .filter(|q| q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w))
        .map(|q| f32::from(q.y) + f32::from(q.h))
        .fold(f32::NEG_INFINITY, f32::max)
        - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
}

/// Adjacent, content-proven runs can heal into the older cohort before
/// painting. The removed younger identity's held shape must survive.
#[test]
fn a_repaint_join_keeps_the_held_shape_when_the_older_free_cohort_survives() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 18,
        ..geom()
    };
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 7,
            col0: 2,
            n: 8,
            period_ms: 20,
            disp: 0.9,
        },
        &c,
    );
    let next = at(t0, 8 * 20);
    rib.on_event(
        &Event::Move {
            from: (7, 10),
            to: (8, 10),
            licence: Licence::Insert,
            dir: Dir::Right,
        },
        next,
        &ctx_in(next, &c, (8, 10), 0.9, g),
    );
    type_keys(
        &mut rib,
        next,
        Keys {
            g,
            row: 8,
            col0: 10,
            n: 8,
            period_ms: 20,
            disp: 0.9,
        },
        &c,
    );
    let mut inked = GlyphProbe::new();
    inked.probe_row(9, &[true; 120]);
    let start = at(next, 8 * 20);
    rib.plan_over(&ctx_in(start, &c, (8, 18), 0.9, g), Some(&inked));
    let carry = at(start, (HOLD_EASE_S * 1000.0) as u64 + 16);
    rib.plan_over(&ctx_in(carry, &c, (8, 18), 0.9, g), Some(&inked));
    let free_depth = repaint_column_depth(&mut rib, &c, g, carry, 7, 4);
    let held_depth = repaint_column_depth(&mut rib, &c, g, carry, 8, 14);
    assert!(
        free_depth > 2.0,
        "the older run is free ({free_depth:.1} px)"
    );
    assert!(
        held_depth.is_finite() && held_depth <= 0.0,
        "the newer run is held ({held_depth:.1} px)"
    );
    let free = rib
        .cohorts()
        .iter()
        .find(|coh| coh.row == 7)
        .expect("free run")
        .id;
    let held = rib
        .cohorts()
        .iter()
        .find(|coh| coh.row == 8)
        .expect("held run")
        .id;
    let mut moved = Vec::new();
    for (row, cohort, lo, hi, dr) in [(7, free, 2, 9, 2), (8, held, 10, 17, 1)] {
        assert_eq!(
            rib.translate_run(
                super::super::witness::FollowRun {
                    row,
                    cohort,
                    lo,
                    hi,
                    dr
                },
                carry,
                false,
                &mut moved
            ),
            8
        );
    }
    let depth = repaint_column_depth(&mut rib, &c, g, carry, 9, 14);
    assert_eq!(
        rib.cohorts().iter().filter(|coh| coh.row == 9).count(),
        1,
        "the real seam healer merged both runs"
    );
    assert!(
        depth.is_finite() && depth <= 0.0,
        "the seam merge discarded the held shape ({depth:.1} px)"
    );
}

/// An empty natural-retirement frame ends the previous row's custody.
/// A later cold run over unknown glyphs is born free, as a fresh ribbon is.
#[test]
fn a_repaint_rows_shape_does_not_survive_its_natural_empty_frame() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 18,
        ..geom()
    };
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let keys = Keys {
        g,
        row: 11,
        col0: 2,
        n: 12,
        period_ms: 60,
        disp: 0.9,
    };
    type_keys(&mut rib, t0, keys, &c);
    let mut inked = GlyphProbe::new();
    inked.probe_row(12, &[true; 120]);
    let start = at(t0, 12 * 60);
    rib.plan_over(&ctx_in(start, &c, (11, 14), 0.9, g), Some(&inked));
    let held = at(start, (HOLD_EASE_S * 1000.0) as u64 + 16);
    rib.plan_over(&ctx_in(held, &c, (11, 14), 0.9, g), Some(&inked));
    let before = repaint_column_depth(&mut rib, &c, g, held, 11, 8);
    assert!(
        before.is_finite() && before <= 0.0,
        "the previous line is held ({before:.1} px)"
    );
    let empty = at(t0, 20_000);
    rib.plan_over(&ctx_in(empty, &c, (11, 14), 0.9, g), None);
    assert!(rib.cells().is_empty(), "the old line naturally retired");
    let cold = at(empty, 16);
    type_keys(&mut rib, cold, keys, &c);
    let now = at(cold, 12 * 60);
    let depth = repaint_column_depth(&mut rib, &c, g, now, 11, 8);
    let mut fresh = Ribbon::new();
    type_keys(&mut fresh, cold, keys, &c);
    let control = repaint_column_depth(&mut fresh, &c, g, now, 11, 8);
    assert!(control > 2.0, "the fresh twin is free ({control:.1} px)");
    assert!(
        (depth - control).abs() < 0.5,
        "the cold run inherited retired row state ({depth:.1} vs {control:.1} px)"
    );
}

/// A navigation wake can still be visible after abandonment. If its text is
/// repainted and another navigation takes over its cells before planning,
/// preserving their light also preserves the shape carried with that light.
#[test]
fn a_repainted_wake_keeps_its_shape_when_navigation_remints_its_live_cells() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 18,
        ..geom()
    };
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let first = ctx_in(t0, &c, (9, 4), 0.9, g);
    let old = rib
        .wake(9, 20, 4, t0, &first, WAKE_LIFE_S, true)
        .expect("navigation lays a wake");
    let carry = at(t0, 100);
    let mut inked = GlyphProbe::new();
    inked.probe_row(10, &[true; 120]);
    let source = ctx_in(carry, &c, (9, 4), 0.9, g);
    rib.plan_over(&source, Some(&inked));
    let depth = |rib: &mut Ribbon, cx: &Ctx<'_>, row: u16| {
        let mut sink = Sink::default();
        rib.emit(cx, &mut sink.frame());
        let x = f32::from(g.origin_x) + 12.5 * g.cw as f32;
        sink.under
            .iter()
            .filter(|q| q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w))
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max)
            - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
    };
    let held = depth(&mut rib, &source, 9);
    assert!(
        held.is_finite() && held <= 0.0,
        "the source wake is held ({held:.1} px)"
    );
    let old_cells: Vec<_> = rib
        .cells()
        .iter()
        .filter(|cell| cell.cohort == old)
        .map(|cell| (cell.col, cell.born))
        .collect();
    assert_eq!(old_cells.len(), 17);

    rib.on_event(
        &nav((9, 4), (5, 0)),
        carry,
        &ctx_in(carry, &c, (5, 0), 0.9, g),
    );
    assert!(
        rib.cohorts()
            .iter()
            .any(|coh| coh.id == old && coh.abandoned)
    );
    assert!(
        rib.cells()
            .iter()
            .filter(|cell| cell.cohort == old)
            .all(|cell| !cell.leaving())
    );
    let mut moved = Vec::new();
    assert_eq!(
        rib.translate_run(
            super::super::witness::FollowRun {
                row: 9,
                cohort: old,
                lo: 4,
                hi: 20,
                dr: -2,
            },
            carry,
            true,
            &mut moved,
        ),
        17
    );

    let target = ctx_in(carry, &c, (7, 4), 0.9, g);
    rib.on_event(&nav((7, 20), (7, 4)), carry, &target);
    let adopted: Vec<_> = rib.cells().iter().filter(|cell| cell.row == 7).collect();
    assert_eq!(adopted.len(), 17);
    assert!(
        adopted
            .iter()
            .all(|cell| { cell.cohort != old && old_cells.contains(&(cell.col, cell.born)) }),
        "the fresh wake must actually adopt the old visual cells"
    );
    rib.plan_over(&target, None);
    let after = depth(&mut rib, &target, 7);
    assert!(after.is_finite(), "the adopted wake is still visible");
    assert!(
        after <= 0.0,
        "navigation remint lost the repainted wake's shape ({after:.1} px)"
    );
}

#[test]
fn a_reminted_free_wake_does_not_borrow_the_held_cohort_that_moved_out() {
    let g = Geom {
        rows: 14,
        win_h: 14 * 18,
        ..geom()
    };
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let first = ctx_in(t0, &c, (7, 4), 0.9, g);
    let free = rib
        .wake(7, 20, 4, t0, &first, WAKE_LIFE_S, true)
        .expect("the free source wake");
    let lay = at(t0, 100);
    rib.plan_over(&ctx_in(lay, &c, (7, 4), 0.9, g), None);
    rib.relay_word(9, 40, 46, lay, None);
    let held = rib
        .cohorts()
        .iter()
        .find(|coh| coh.row == 9 && !coh.wake)
        .expect("the held destination's original cohort")
        .id;
    let carry = at(t0, 160);
    let mut inked = GlyphProbe::new();
    inked.probe_row(10, &[true; 120]);
    let source = ctx_in(carry, &c, (9, 46), 0.9, g);
    rib.plan_over(&source, Some(&inked));
    let depth = |rib: &mut Ribbon, cx: &Ctx<'_>, row: u16, col: u16| {
        let mut sink = Sink::default();
        rib.emit(cx, &mut sink.frame());
        let x = f32::from(g.origin_x) + (f32::from(col) + 0.5) * g.cw as f32;
        sink.under
            .iter()
            .filter(|q| q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w))
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max)
            - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
    };
    let free_before = depth(&mut rib, &source, 7, 12);
    let held_before = depth(&mut rib, &source, 9, 42);
    assert!(
        free_before > 2.0,
        "the source wake is free ({free_before:.1} px)"
    );
    assert!(
        held_before.is_finite() && held_before <= 0.0,
        "the destination's original cohort is held ({held_before:.1} px)"
    );
    let old_cells: Vec<_> = rib
        .cells()
        .iter()
        .filter(|cell| cell.cohort == free)
        .map(|cell| (cell.col, cell.born))
        .collect();
    assert_eq!(old_cells.len(), 17);
    rib.on_event(
        &nav((7, 4), (5, 0)),
        carry,
        &ctx_in(carry, &c, (5, 0), 0.9, g),
    );
    assert!(
        rib.cohorts()
            .iter()
            .any(|coh| coh.id == free && coh.abandoned)
    );
    let mut moved = Vec::new();
    for (row, cohort, lo, hi, dr, expected) in [(9, held, 40, 45, 2, 6), (7, free, 4, 20, 2, 17)] {
        assert_eq!(
            rib.translate_run(
                super::super::witness::FollowRun {
                    row,
                    cohort,
                    lo,
                    hi,
                    dr,
                },
                carry,
                true,
                &mut moved,
            ),
            expected
        );
    }
    assert!(
        rib.cells()
            .iter()
            .all(|cell| cell.cohort != held || cell.row == 11)
    );
    let target = ctx_in(carry, &c, (9, 4), 0.9, g);
    rib.on_event(&nav((9, 20), (9, 4)), carry, &target);
    let adopted: Vec<_> = rib.cells().iter().filter(|cell| cell.row == 9).collect();
    assert_eq!(adopted.len(), 17);
    assert!(
        adopted
            .iter()
            .all(|cell| { cell.cohort != free && old_cells.contains(&(cell.col, cell.born)) }),
        "the fresh wake actually adopts the free source's visual cells"
    );
    rib.plan_over(&target, None);
    let free_after = depth(&mut rib, &target, 9, 12);
    let held_after = depth(&mut rib, &target, 11, 42);
    assert!(
        held_after.is_finite() && held_after <= 0.0,
        "the moved-out original remains held ({held_after:.1} px)"
    );
    assert!(
        free_after > 2.0,
        "the reminted free wake borrowed the departed cohort's hold ({free_after:.1} px)"
    );
}

#[test]
fn the_band_stops_at_the_focused_pane_s_floor_not_the_grid_s() {
    let c = cfg(true, true);
    let g = geom();
    let ch = g.ch as f32;
    let pane_last_row = 3u16;
    let deepest = |pane_rows: Option<(u16, u16)>| -> f32 {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.set_pane_rows(pane_rows);
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: pane_last_row,
                col0: 10,
                n: 8,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let cx = ctx(at(t0, 8 * 60), &c, (pane_last_row, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        sink.under
            .iter()
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(0.0f32, f32::max)
    };
    let pane_floor = f32::from(g.origin_y) + f32::from(pane_last_row.saturating_add(1)) * ch;
    let bounded = deepest(Some((0, pane_last_row + 1)));
    let unbounded = deepest(None);
    assert!(
        unbounded > pane_floor + 0.5,
        "fixture: with no pane the band reaches past {pane_floor} \
         (deepest {unbounded}) — otherwise this pins nothing"
    );
    assert!(
        bounded <= pane_floor + 0.5,
        "the band reached {bounded}, past the focused pane's floor \
         {pane_floor} — that is the divider and the pane below"
    );
}

#[test]
fn a_hot_paragraph_at_retina_keeps_its_head_row_whole_and_sheds_only_the_oldest_tail() {
    let c = cfg(true, true);
    let g = geom2x();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let mut sink = Sink::default();
    let mut ms = 0u64;
    for row in 2..5u16 {
        let keys = Keys {
            g,
            row,
            col0: 2,
            n: 76,
            period_ms: 5,
            disp: 1.0,
        };
        type_keys(&mut rib, at(t0, ms), keys, &c);
        ms += 76 * 5;
        let cx = ctx_in(at(t0, ms), &c, (row, 78), 1.0, g);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let live_rows = row - 1;
        assert!(
            sink.under.len() <= RIBBON_QUAD_BUDGET,
            "{live_rows} retina rows spent {} quads against a {RIBBON_QUAD_BUDGET} budget",
            sink.under.len()
        );
        for col in 2..78u16 {
            assert!(
                cell_lit(&sink.under, g, row, col),
                "row {row} col {col} is dark on its own frame with {live_rows} rows live"
            );
        }
        match live_rows {
            1 => assert!(
                rib.slabs_per_cell() < SLABS_PER_CELL,
                "a 76-cell retina line does not fit at three slabs per cell; the density must give"
            ),
            2 => {
                for col in 2..78u16 {
                    assert!(
                        cell_lit(&sink.under, g, 2, col),
                        "two retina rows fit the budget; row 2 col {col} was shed anyway"
                    );
                }
            }
            _ => {
                for col in 2..78u16 {
                    assert!(
                        cell_lit(&sink.under, g, 3, col),
                        "row 3 col {col} was shed before row 2"
                    );
                }
                let lit: Vec<bool> = (2..78u16)
                    .map(|col| cell_lit(&sink.under, g, 2, col))
                    .collect();
                assert!(
                    lit.iter().any(|&l| !l) || rib.slabs_per_cell() == 1,
                    "three retina rows exceed the budget: the density must give or the oldest must lose its tail"
                );
                let first = lit.iter().position(|&l| l).unwrap_or(lit.len());
                assert!(
                    lit[first..].iter().all(|&l| l),
                    "the shed must come off the oldest row's tail, not out of its middle"
                );
            }
        }
    }
}

/// A geometry of `ch` px cells half as wide, 240 columns: room for [`slab_walk`]'s line.
const fn geom_ch(ch: usize) -> Geom {
    Geom {
        cw: ch / 2,
        ch,
        rows: 40,
        cols: 240,
        origin_x: 0,
        origin_y: 0,
        win_w: (240 * (ch / 2)) as u16,
        win_h: (40 * ch) as u16,
        head: 0,
    }
}

/// The frame's vertex density ([`Ribbon::slabs_per_cell`]) and the glass's
/// ([`Ribbon::glass_slabs_per_cell`]) after each of 160 keys typed 2 ms apart along row 2 of
/// `g` under `c`: the plan's choice for every count of cells from one to the line's.
fn slab_walk(c: &Config, g: Geom) -> Vec<(usize, usize)> {
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    (0..160u16)
        .map(|i| {
            let now = at(t0, u64::from(i) * 2);
            let cx = ctx_in(now, c, (2, 3 + i), 0.9, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            (rib.slabs_per_cell(), rib.glass_slabs_per_cell())
        })
        .collect()
}

/// FNV-1a over `words`.
fn fold_words(words: impl IntoIterator<Item = u64>) -> u64 {
    words
        .into_iter()
        .fold(aterm_hash::FNV64_OFFSET, aterm_hash::fnv1a_64_u64_le)
}

/// The spellings a density is chosen for: the tall comet, the tall flat ribbon, the
/// underline comet and the underline flat ribbon, on a page `dark` or not.
const fn spellings(dark: bool) -> [Config; 4] {
    [
        cfg(dark, true),
        flat(dark, true),
        cfg(dark, false),
        flat(dark, false),
    ]
}

/// The lights a dark ground is drawn under in [`dark_slabs_for_is_mains`]: none, each live
/// level, and the glass level on a see-through page.
const DARK_LIGHTS: [Light; 6] = [
    Light::OFF,
    Light::at(Live::Glass),
    Light::at(Live::Lip),
    Light::at(Live::Marks),
    Light::at(Live::Vanish),
    Light::at(Live::Glass).on_page(false),
];

/// The cell heights [`dark_slabs_for_is_mains`] walks: the dump's 1×, the tests' 18, a
/// Retina's 28 and 36.
const SLAB_CHS: [usize; 4] = [16, 18, 28, 36];

/// [`slab_walk`] on two dark grounds (Default's, and Solarized Dark's, a dim page whose flat
/// ribbon lays its rail) under [`DARK_LIGHTS`], every spelling and [`SLAB_CHS`], folded, and
/// the densities the walks chose (bit `n` for `n` slabs per cell) and the most glass slabs
/// any frame chose.
fn dark_slab_fold() -> (u64, u32, usize) {
    let mut words: Vec<u64> = Vec::new();
    let mut glass = 0;
    for (fg, bg) in [(DEFAULT_FG, DEFAULT_BG), (0x0083_9496, 0x0000_2B36)] {
        for light in DARK_LIGHTS {
            for c in spellings(true) {
                for ch in SLAB_CHS {
                    let c = Config {
                        theme_fg: fg,
                        theme_bg: bg,
                        light,
                        ..c
                    };
                    for (n, g) in slab_walk(&c, geom_ch(ch)) {
                        words.push(n as u64);
                        glass = glass.max(g);
                    }
                }
            }
        }
    }
    let chose = words.iter().fold(0u32, |m, &n| m | 1 << n);
    (fold_words(words), chose, glass)
}

/// [`dark_slab_fold`] of main's densities on every dark ground, with the underline
/// spelling at looks10 L3's reach ([`UNDERLINE_UP_CH`], [`UNDERLINE_DN_CH`]). Captured at
/// this pin's parent (origin/main `b11eed7965`, before Prism Glass 1.8), with the underline
/// at its old reach, the walks folded to `0x0ee8_6c14_beca_e565`. trail/looks10's merge of
/// 1.8 folds to that value again with L3's reach put back (`1.0 - DN_TOP_CH` up,
/// [`COMET_DN_HAND_CH`] or [`DN_TOP_CH`] down, the tall walk). The thin underline alone moves
/// the fold; the glass moves none of it.
const MAIN_DARK_SLABS: u64 = 0xfe94_9fbb_f997_eca5;

/// DARK SLABS ARE MAIN'S (`docs/design/PRISM-GLASS.md` §2.2 Quad budget, §15.2 1.8): the glass
/// prices its own rows only on a page it lights, so on every dark ground, under every light
/// level (and on a see-through page), every spelling chooses main's vertex density for every
/// count of cells at every cell height: the walks fold to the hash main's walks folded to at
/// this pin's parent, and they choose one, two and three slabs a cell; no frame prices a
/// glass slab. The glass's own density on a light page, denser than main's paper, is
/// [`a_glass_frame_stays_within_the_ribbon_quad_budget`]'s control.
#[test]
fn dark_slabs_for_is_mains() {
    let (fold, chose, glass) = dark_slab_fold();
    assert_eq!(
        chose, 0b1110,
        "fixture: the walks choose 1, 2 and 3 slabs a cell"
    );
    assert_eq!(fold, MAIN_DARK_SLABS, "{fold:#x}");
    assert_eq!(glass, 0, "a dark frame chose {glass} glass slabs a cell");
}

const fn geom_codex() -> Geom {
    Geom {
        cw: 9,
        ch: 18,
        rows: 57,
        cols: 151,
        origin_x: 0,
        origin_y: 0,
        win_w: 1359,
        win_h: 1026,
        head: 0,
    }
}

fn type_row(rib: &mut Ribbon, t0: Instant, row: u16, col0: u16, n: u16, c: &Config) {
    type_keys(
        rib,
        t0,
        Keys {
            g: geom_codex(),
            row,
            col0,
            n,
            period_ms: 60,
            disp: 0.5,
        },
        c,
    );
}

#[test]
fn a_band_move_carries_the_composer_band_down_and_leaves_the_transcript_alone() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_row(&mut rib, t0, 5, 10, 3, &c);
    type_row(&mut rib, at(t0, 500), 20, 10, 6, &c);
    let before: Vec<Cell> = rib.cells().to_vec();
    assert_eq!(before.iter().filter(|l| l.row == 20).count(), 6);
    assert_eq!(before.iter().filter(|l| l.row == 5).count(), 3);
    let composer: Vec<Option<f32>> = (10..16).map(|col| rib.field_at(20, col)).collect();
    assert!(
        composer.iter().all(Option::is_some),
        "fixture: the composer is lit"
    );
    let transcript: Vec<Option<f32>> = (10..13).map(|col| rib.field_at(5, col)).collect();
    assert!(
        transcript.iter().all(Option::is_some),
        "fixture: the transcript is lit"
    );
    assert_eq!(rib.caret(), Some((20, 16)));

    rib.translate_band(19, 56, 1);

    let after = rib.cells();
    assert_eq!(
        after.len(),
        before.len(),
        "a band move loses no cell inside its band"
    );
    for b in &before {
        let want = if b.row == 20 { 21 } else { b.row };
        assert!(
            after.iter().any(|a| a.row == want
                && a.col == b.col
                && a.t == b.t
                && a.born == b.born
                && a.cohort == b.cohort
                && a.retract_at == b.retract_at),
            "cell ({}, {}) did not arrive on row {want} with its clocks and prices",
            b.row,
            b.col
        );
    }
    assert!(
        after.iter().all(|a| a.row != 20),
        "the vacated row keeps no light"
    );
    for (i, col) in (10..16).enumerate() {
        assert_eq!(
            rib.field_at(21, col),
            composer[i],
            "the field moved with its cell"
        );
        assert_eq!(
            rib.field_at(20, col),
            None,
            "the vacated cell answers nothing"
        );
    }
    for (i, col) in (10..13).enumerate() {
        assert_eq!(
            rib.field_at(5, col),
            transcript[i],
            "the transcript stood still"
        );
    }
    assert_eq!(rib.caret(), Some((21, 16)), "the caret rides its band");
}

#[test]
fn a_top_anchored_band_moves_the_transcript_up_and_pins_the_footer() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_row(&mut rib, t0, 0, 4, 2, &c);
    type_row(&mut rib, at(t0, 300), 3, 10, 4, &c);
    type_row(&mut rib, at(t0, 900), 54, 13, 5, &c);
    let footer: Vec<Option<f32>> = (13..18).map(|col| rib.field_at(54, col)).collect();
    let t3 = rib.field_at(3, 11).expect("fixture: row 3 is lit");
    assert_eq!(rib.caret(), Some((54, 18)));

    rib.translate_band(0, 51, -1);

    assert!(rib.cells().iter().all(|l| l.row != 0 && l.row != 3));
    assert_eq!(
        rib.cells().iter().filter(|l| l.row == 2).count(),
        4,
        "row 3 rode up to row 2"
    );
    assert_eq!(rib.field_at(2, 11), Some(t3));
    assert!(
        rib.cells().iter().all(|l| l.row != u16::MAX),
        "nothing wrapped around the top"
    );
    assert_eq!(
        rib.cells().iter().filter(|l| l.row == 54).count(),
        5,
        "the pinned composer is outside the band and untouched"
    );
    for (i, col) in (13..18).enumerate() {
        assert_eq!(rib.field_at(54, col), footer[i]);
    }
    assert_eq!(
        rib.caret(),
        Some((54, 18)),
        "a caret outside the band stays put"
    );
    assert_eq!(
        rib.cells().len(),
        9,
        "row 0's two cells left through the top; nothing else was lost"
    );
}

#[test]
fn a_band_move_drops_what_leaves_the_band_and_retires_its_cohort() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_row(&mut rib, t0, 56, 10, 6, &c);
    assert!(rib.live_cells() == 6 && !rib.cohorts.is_empty());
    assert_eq!(rib.index.live_rows(), 1);

    rib.translate_band(50, 56, 1);

    assert!(
        rib.at_rest(),
        "light carried off the band's edge is not kept"
    );
    assert!(
        rib.cohorts.is_empty(),
        "a cohort with no cells retires with them"
    );
    for col in 10..16 {
        assert_eq!(rib.index.at(56, col), None);
        assert_eq!(rib.index.at(57, col), None);
    }
    assert_eq!(
        rib.index.live_rows(),
        0,
        "the lane retired into the spare pool"
    );
    assert_eq!(
        rib.index.spare.len(),
        1,
        "…and is there for the next row that lights"
    );
    assert_eq!(
        rib.caret(),
        Some((56, 16)),
        "the caret saturates at the edge"
    );
}

#[test]
fn a_scroll_carries_the_ribbon_with_its_text() {
    let c = cfg(true, true);
    let g = geom2x();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 4, &c, 0.5);
    let now = at(t0, 4 * 60);
    erase_at(&mut rib, now, (2, 13), &c);
    let t13 = rib
        .field_at(2, 13)
        .expect("the retracting cell keeps its stop");
    let t12 = rib.field_at(2, 12).expect("col 12 is lit");
    assert_ne!(
        t13, t12,
        "the fixture needs a caret stop the newest key does not share"
    );
    assert_eq!(rib.caret(), Some((2, 13)));
    assert_eq!(rib.field_at_caret(), t13);
    assert!(
        rib.cells().iter().any(|l| l.retract_at.is_some()),
        "the fixture needs a cell on its retract clock"
    );
    let cx = ctx_in(now, &c, (2, 13), 0.7, g);
    rib.plan(&cx);
    let mut before = Sink::default();
    {
        let mut f = before.frame();
        rib.emit(&cx, &mut f);
    }
    assert!(
        !before.under.is_empty(),
        "the frame must draw for the law to bite"
    );
    let cells_before: Vec<Cell> = rib.cells().to_vec();

    rib.translate_scroll(1);

    assert_eq!(rib.caret(), Some((1, 13)), "the caret rides the scroll");
    assert_eq!(
        rib.field_at(1, 13),
        Some(t13),
        "the field moved with its cell"
    );
    assert_eq!(rib.field_at(2, 13), None, "nothing is left on the old row");
    assert_eq!(
        rib.field_at_caret(),
        t13,
        "the caret's stop is its own cell's, not the newest key's"
    );
    let cells_after = rib.cells();
    assert_eq!(
        cells_after.len(),
        cells_before.len(),
        "a scroll drops nothing on the grid"
    );
    for (a, b) in cells_before.iter().zip(cells_after) {
        assert_eq!(b.row + 1, a.row);
        assert_eq!(
            (b.col, b.cohort, b.born, b.retract_at, b.typing),
            (a.col, a.cohort, a.born, a.retract_at, a.typing)
        );
        assert_eq!(
            (b.t, b.life_s, b.cov0, b.birth_disp),
            (a.t, a.life_s, a.cov0, a.birth_disp)
        );
    }
    let cx1 = ctx_in(now, &c, (1, 13), 0.7, g);
    rib.plan(&cx1);
    let mut after = Sink::default();
    {
        let mut f = after.frame();
        rib.emit(&cx1, &mut f);
    }
    let ch = u16::try_from(g.ch).expect("ch");
    assert_eq!(
        after.under.len(),
        before.under.len(),
        "the body kept every quad"
    );
    for (a, b) in before.under.iter().zip(&after.under) {
        assert_eq!(
            (b.x, b.w, b.h, b.color, b.alpha),
            (a.x, a.w, a.h, a.color, a.alpha)
        );
        assert_eq!(
            b.y + ch,
            a.y,
            "a body quad moved by {} px, not one cell height ({ch})",
            i32::from(a.y) - i32::from(b.y)
        );
        assert_eq!(b.row + 1, a.row, "the damage hint rides with the quad");
    }
    rib.translate_scroll(2);
    assert!(rib.at_rest());
    assert_eq!(rib.field_at(0, 13), None);
}

#[test]
fn a_fades_next_level_is_solved_not_polled() {
    let poll = |level_now: f32, curve: &dyn Fn(f32) -> f32, u: f32, span: f32| -> f32 {
        let mut step = 0u64;
        loop {
            step += 1;
            let t = (step as f64 * 1e-5) as f32;
            if (curve(u + t / span)).round() < level_now {
                return t;
            }
            assert!(t < 5.0, "the polled level never dropped");
        }
    };
    let mut checked = 0usize;
    for peak in [UNDER_COV_CAP * 0.6, 30.0, 4.0] {
        for u in [0.0, 0.3, 0.7, 0.9, 0.97] {
            let span = RETRACT_FADE_S;
            let level = (peak * spend(u)).round();
            match level_step_spend(peak, u, span) {
                None => assert!(
                    level < 1.0,
                    "spend peak {peak} u {u}: None while at {level}"
                ),
                Some(dt) => {
                    let got = poll(level, &|v| peak * spend(v), u, span);
                    assert!(
                        (dt - got).abs() < 2e-4,
                        "spend peak {peak} u {u}: solved {dt} s, polled {got} s"
                    );
                    checked += 1;
                }
            }
            let life = 1.7;
            let level = (peak * expiry_melt(u)).round();
            match level_step_melt(peak, u, life) {
                None => assert!(level < 1.0, "melt peak {peak} u {u}: None while at {level}"),
                Some(dt) => {
                    let got = poll(level, &|v| peak * expiry_melt(v), u, life);
                    assert!(
                        (dt - got).abs() < 2e-4,
                        "melt peak {peak} u {u}: solved {dt} s, polled {got} s"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 20, "only {checked} steps checked");
}

#[test]
fn a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let row = 4u16;
    let col0 = 2u16;
    let words: [(&str, u64, u64, f32); 4] = [
        ("rainbow ", 110, 0, 0.9),
        ("theme", 250, 700, 0.45),
        (" truly ", 125, 0, 0.9),
        ("magical", 125, 0, 0.9),
    ];
    let mut key_ms = 0u64;
    let mut caret = col0;
    let mut now_ms = 0u64;
    let mut dark: Vec<String> = Vec::new();
    let mut keys = 0;
    for (word, per_key, pause, disp) in words {
        key_ms += pause;
        for _ in word.chars() {
            key_ms += per_key;
            while now_ms + 16 <= key_ms {
                now_ms += 16;
                let cx = ctx_in(at(t0, now_ms), &c, (row, caret), disp, g);
                rib.plan(&cx);
                let missing: Vec<u16> = (col0..caret)
                    .filter(|&col| rib.field_at(row, col).is_none())
                    .collect();
                if !missing.is_empty() && dark.len() < 6 {
                    dark.push(format!(
                        "+{:.3}s keys={keys} caret={caret} dark={missing:?}",
                        now_ms as f32 / 1000.0
                    ));
                }
            }
            caret += 1;
            keys += 1;
            let cx = ctx_in(at(t0, key_ms), &c, (row, caret), disp, g);
            rib.on_event(&typed(), at(t0, key_ms), &cx);
            rib.plan(&cx);
        }
    }
    assert_eq!(keys, 27);
    assert!(
        dark.is_empty(),
        "a cell went dark under a hand still typing:\n{}",
        dark.join("\n")
    );
    let last = key_ms;
    let grace_ms = (PHRASE_REST_MIN_S * 1000.0) as u64;
    let cx = ctx_in(at(t0, last + grace_ms), &c, (row, caret), 0.9, g);
    rib.plan(&cx);
    assert_eq!(
        (col0..caret)
            .filter(|&col| rib.field_at(row, col).is_some())
            .count(),
        usize::from(caret - col0),
        "every cell stands through the {PHRASE_REST_MIN_S} s grace"
    );
    let gone = last + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20;
    let cx = ctx_in(at(t0, gone), &c, (row, caret), 0.9, g);
    rib.plan(&cx);
    let mut sink = Sink::default();
    rib.emit(&cx, &mut sink.frame());
    assert!(
        rib.at_rest() && sink.under.is_empty() && sink.out.is_empty(),
        "off the glass {SWOOSH_TOTAL_S} s after the last key"
    );
}

fn landing_lit(rib: &Ribbon, row: u16) -> Vec<u16> {
    let mut v: Vec<u16> = rib
        .cells()
        .iter()
        .filter(|l| l.row == row && !l.leaving())
        .map(|l| l.col)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn landing_any(rib: &Ribbon, row: u16) -> Vec<u16> {
    let mut v: Vec<u16> = rib
        .cells()
        .iter()
        .filter(|l| l.row == row)
        .map(|l| l.col)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn a_parking_pass_after_the_row_was_backspaced_empty_still_finds_the_hand() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 2,
            n: 16,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    let mut ms = 16 * 60;
    for i in 0..16u16 {
        ms += 40;
        let now = at(t0, ms);
        let cx = ctx_in(now, &c, (13, 17 - i), 0.7, g);
        rib.on_event(&Event::Erase, now, &cx);
        rib.plan(&cx);
    }
    assert_eq!(rib.caret(), Some((13, 2)));
    assert!(
        landing_lit(&rib, 13).is_empty() && !landing_any(&rib, 13).is_empty(),
        "fixture: the held Backspace left row 13 entirely in retract, \
         live {:?} / all {:?}",
        landing_lit(&rib, 13),
        landing_any(&rib, 13)
    );
    ms += 40;
    let now = at(t0, ms);
    let cx = ctx_in(now, &c, (12, 0), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert_eq!(
        landing_lit(&rib, 13),
        vec![2],
        "the key after the Backspace run did not land on the hand's own row"
    );
    assert!(
        landing_any(&rib, 11).is_empty(),
        "a stray was laid on the row above the box home: {:?}",
        landing_any(&rib, 11)
    );
}

#[test]
fn a_parking_pass_is_caught_at_every_column_it_can_be_sampled_at() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 2,
            n: 16,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    assert_eq!(rib.caret(), Some((13, 18)));
    let lit = |rib: &Ribbon, row: u16| {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == row && !l.leaving())
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    assert_eq!(lit(&rib, 13), (2..=17).collect::<Vec<_>>());
    for park in [0u16, 1, 5, 9, 17] {
        let mut rib = rib.clone();
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (12, park), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            lit(&rib, 13),
            (2..=18).collect::<Vec<_>>(),
            "a parking pass sampled at column {park} took the key off the hand's row"
        );
        assert!(
            lit(&rib, 12).is_empty() && lit(&rib, 11).is_empty(),
            "a stray was laid off the hand's row for a pass sampled at column {park}: \
             row 12 {:?}, row 11 {:?}",
            lit(&rib, 12),
            lit(&rib, 11)
        );
    }
}

#[test]
fn a_repaints_home_pass_poisons_the_mirror_and_the_pen_still_holds_the_hand() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 2,
            n: 16,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    assert_eq!(rib.caret(), Some((13, 18)));
    let now = at(t0, 16 * 60);
    let cx = ctx_in(now, &c, (12, 0), 0.7, g);
    rib.on_event(&typed_move((13, 18), (12, 0)), now, &cx);
    rib.plan(&cx);
    assert_eq!(
        rib.caret(),
        Some((12, 0)),
        "the fixture's whole point: the MIRROR took the park"
    );
    assert!(
        rib.cohorts().iter().all(|k| !k.abandoned),
        "fixture: and the sibling law kept the hand's row"
    );
    let now = at(t0, 17 * 60);
    let cx = ctx_in(now, &c, (12, 0), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert_eq!(
        landing_lit(&rib, 13),
        (2..=18).collect::<Vec<_>>(),
        "the key did not land on the hand's own row"
    );
    assert!(
        landing_any(&rib, 12).is_empty() && landing_any(&rib, 11).is_empty(),
        "a stray was laid off the hand's row: 11={:?} 12={:?}",
        landing_any(&rib, 11),
        landing_any(&rib, 12)
    );
}

#[test]
fn a_nav_moves_the_pen_so_a_fold_after_it_still_lays_where_it_folded() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 2,
            n: 16,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    let last = u16::try_from(g.cols).expect("cols") - 1;
    let now = at(t0, 16 * 60);
    let cx = ctx_in(now, &c, (20, last), 0.7, g);
    rib.on_event(
        &Event::Move {
            from: (13, 18),
            to: (20, last),
            licence: Licence::Nav,
            dir: Dir::Right,
        },
        now,
        &cx,
    );
    rib.plan(&cx);
    let now = at(t0, 17 * 60);
    let cx = ctx_in(now, &c, (21, 0), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert_eq!(
        landing_any(&rib, 20),
        vec![last],
        "the hand's own fold after a nav did not lay on the row it folded off"
    );
    assert!(
        !landing_any(&rib, 13).contains(&18),
        "the wrap key was dragged back onto the row the nav left: {:?}",
        landing_any(&rib, 13)
    );
}

#[test]
fn a_downward_landing_is_questioned_only_where_lay_would_write_above_it() {
    let c = cfg(true, true);
    let g = geom();
    let seat = |t0: Instant| {
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 18)));
        rib
    };
    let t0 = Instant::now();
    let mut rib = seat(t0);
    let now = at(t0, 16 * 60);
    let cx = ctx_in(now, &c, (15, 0), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert_eq!(landing_lit(&rib, 13), (2..=18).collect::<Vec<_>>());
    assert!(landing_any(&rib, 14).is_empty() && landing_any(&rib, 15).is_empty());
    let t0 = Instant::now();
    let mut rib = seat(t0);
    let now = at(t0, 16 * 60);
    let cx = ctx_in(now, &c, (15, 3), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert_eq!(
        landing_lit(&rib, 15),
        vec![2],
        "a downward caret that does not fold is the hand's own, however far"
    );
    assert_eq!(landing_lit(&rib, 13), (2..=17).collect::<Vec<_>>());
}

#[test]
fn a_delivered_burst_that_wraps_keeps_the_cells_it_left_on_the_row_above() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 112,
            n: 5,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    assert_eq!(
        rib.caret(),
        Some((13, 117)),
        "fixture: the hand is five from the margin"
    );
    let now = at(t0, 5 * 60);
    let cx = ctx_in(now, &c, (14, 2), 0.7, g);
    rib.on_event(
        &Event::Typed {
            cells: 5,
            shifted: false,
            class: TypedClass::Glyph,
        },
        now,
        &cx,
    );
    rib.plan(&cx);
    assert_eq!(
        landing_lit(&rib, 13),
        (112..=119).collect::<Vec<_>>(),
        "the burst's first three cells finished the hand's own row"
    );
    assert_eq!(
        landing_lit(&rib, 14),
        vec![0, 1],
        "…and the other two went on over the wrap"
    );
}

#[test]
fn a_cold_caret_mirror_is_not_a_hand_a_key_can_be_brought_back_to() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let cold = ctx_in(t0, &c, (0, 0), 0.7, g);
    rib.plan(&cold);
    assert_eq!(rib.caret(), Some((0, 0)));
    assert!(
        rib.cells().is_empty(),
        "a cold ribbon holds no cell — there is no hand's row yet"
    );
    let now = at(t0, 80);
    let cx = ctx_in(now, &c, (2, 5), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    let lit = |rib: &Ribbon, row: u16| {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == row)
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    assert_eq!(
        lit(&rib, 2),
        vec![4],
        "the first key of the session did not land on the row the host reported"
    );
    assert!(
        lit(&rib, 0).is_empty(),
        "the cold mirror was read as a hand and took the key to row 0: {:?}",
        lit(&rib, 0)
    );
}

#[test]
fn a_key_sampled_at_readlines_cr_park_stays_on_the_hands_own_row() {
    let c = cfg(true, true);
    let g = geom();

    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 2,
            n: 16,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    assert_eq!(rib.caret(), Some((13, 18)), "fixture: the hand's caret");
    let now = at(t0, 16 * 60);
    let cx = ctx_in(now, &c, (13, 0), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert!(
        landing_any(&rib, 12).is_empty(),
        "the key folded off the pane and laid on the row above — the \
         previous command's output: {:?}",
        landing_any(&rib, 12)
    );
    assert_eq!(
        landing_lit(&rib, 13),
        (2..=18).collect::<Vec<_>>(),
        "the key did not land on the hand's own row, next to its last cell"
    );

    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    rib.set_pane(Some((40, 40)));
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 42,
            n: 16,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    assert_eq!(rib.caret(), Some((13, 58)), "fixture: the hand's caret");
    let now = at(t0, 16 * 60);
    let cx = ctx_in(now, &c, (13, 40), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert!(
        landing_any(&rib, 12).is_empty(),
        "the key folded off the PANE's first column onto the row above: {:?}",
        landing_any(&rib, 12)
    );
    assert_eq!(
        landing_lit(&rib, 13),
        (42..=58).collect::<Vec<_>>(),
        "the key did not land on the hand's own row inside the pane"
    );

    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 20,
            n: 16,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    assert_eq!(rib.caret(), Some((13, 36)), "fixture: the hand's caret");
    let now = at(t0, 16 * 60);
    let cx = ctx_in(now, &c, (13, 5), 0.7, g);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert!(
        landing_lit(&rib, 13).contains(&4),
        "a same-row caret that folds nothing was re-homed to the pen: {:?}",
        landing_lit(&rib, 13)
    );
    assert!(
        landing_any(&rib, 12).is_empty(),
        "a caret that folds nothing reached the row above: {:?}",
        landing_any(&rib, 12)
    );
}

#[test]
fn a_repaints_home_pass_does_not_abandon_the_row_the_hand_is_typing_on() {
    let c = cfg(true, true);
    let g = geom();
    for (excursion, what) in [
        ((12u16, 0u16), "the row above, column 0"),
        ((11, 0), "two rows above, column 0"),
    ] {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 20,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        let ms = 19 * 60 + 60;
        let now = at(t0, ms);
        let cx = ctx_in(now, &c, excursion, 0.7, g);
        rib.on_event(&typed_move((13, 22), excursion), now, &cx);
        rib.plan(&cx);
        assert!(
            rib.cohorts().iter().all(|k| !k.abandoned),
            "{what}: the repaint's home abandoned the band"
        );
        assert_eq!(
            rib.cells()
                .iter()
                .filter(|l| l.row == 13 && !l.leaving())
                .count(),
            20,
            "{what}: the band lost cells to a caret the hand never put there"
        );
        let now = at(t0, ms + 60);
        let cx = ctx_in(now, &c, (12, 79), 0.7, g);
        rib.on_event(&typed_move((13, 2), (12, 79)), now, &cx);
        rib.plan(&cx);
        assert!(
            rib.cohorts().iter().any(|k| k.row == 13 && k.abandoned),
            "{what}: a backspace back through the fold stopped being a row change"
        );
    }
}

#[test]
fn a_re_anchor_inside_the_scroll_that_moved_the_band_drains_nothing() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: 13,
            col0: 2,
            n: 20,
            period_ms: 60,
            disp: 0.7,
        },
        &c,
    );
    rib.translate_scroll(1);
    assert_eq!(
        rib.cells().iter().filter(|l| l.row == 12).count(),
        20,
        "the band followed its text up"
    );
    let now = at(t0, 19 * 60 + 60);
    let cx = ctx_in(now, &c, (13, 3), 0.7, g);
    rib.on_event(&typed_move((12, 22), (12, 2)), now, &cx);
    assert!(
        rib.cells().iter().all(|l| l.retract_at.is_none()),
        "the re-anchor drained the row the scroll had just carried: {:?}",
        rib.cells()
            .iter()
            .filter(|l| l.retract_at.is_some())
            .map(|l| l.col)
            .collect::<Vec<_>>()
    );
    rib.plan(&cx);
    let now = at(t0, 19 * 60 + 120);
    let cx = ctx_in(now, &c, (12, 2), 0.7, g);
    rib.on_event(&typed_move((12, 22), (12, 2)), now, &cx);
    assert!(
        rib.cells().iter().any(|l| l.retract_at.is_some()),
        "the scroll guard outlived its own tick"
    );
}

#[test]
fn a_backward_typed_echo_under_the_re_anchor_floor_stamps_nothing() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let census = |rib: &Ribbon| {
        let mut v: Vec<(u16, u16, Instant, bool)> = rib
            .cells()
            .iter()
            .map(|l| (l.row, l.col, l.born, l.typing))
            .collect();
        v.sort_unstable();
        v
    };
    let stamped = |rib: &Ribbon| {
        rib.cells()
            .iter()
            .filter(|l| l.leaving())
            .map(|l| l.col)
            .collect::<Vec<_>>()
    };
    for back in 1..RE_ANCHOR_MIN_CELLS {
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.8);
        let before = census(&rib);
        let now = at(t0, 6 * 60 + 200);
        let landing = (2, 16 - back);
        let cx = ctx(now, &c, landing, 0.8);
        rib.on_event(&typed_move((2, 16), landing), now, &cx);
        rib.plan(&cx);
        assert!(
            stamped(&rib).is_empty(),
            "a {back}-cell backward typed echo drained the row: stamped={:?}",
            stamped(&rib)
        );
        assert_eq!(census(&rib), before, "{back} back: the census is unchanged");
        assert_eq!(rib.caret(), Some(landing), "{back} back: the mirror moved");
    }
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 10, 6, &c, 0.8);
    let now = at(t0, 6 * 60 + 200);
    let landing = (2, 16 - RE_ANCHOR_MIN_CELLS);
    let cx = ctx(now, &c, landing, 0.8);
    rib.on_event(&typed_move((2, 16), landing), now, &cx);
    assert_eq!(
        stamped(&rib),
        vec![13, 14, 15],
        "at the floor the re-anchor drains the cells at and past the landing"
    );
}

#[test]
fn a_shell_home_relays_no_word_onto_the_prompt() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let mut now = t0;
    for (i, ch) in "hello there now".chars().enumerate() {
        now = at(t0, i as u64 * 60);
        let col0 = 28 + i as u16;
        let cx = ctx_in(now, &c, (5, col0 + 1), 0.9, g);
        let ev = if ch == ' ' { typed_space() } else { typed() };
        rib.on_event(&ev, now, &cx);
        rib.plan(&cx);
    }
    let home = at(now, 60);
    let cx = ctx_in(home, &c, (5, 28), 0.9, g);
    rib.on_event(&typed_move((5, 43), (5, 28)), home, &cx);
    rib.plan(&cx);
    let inside_prompt: Vec<u16> = rib
        .cells()
        .iter()
        .filter(|l| l.row == 5 && l.col < 28)
        .map(|l| l.col)
        .collect();
    assert!(
        inside_prompt.is_empty(),
        "the home relaid a word onto the prompt: columns {inside_prompt:?} are lit left of \
         the 28-column PS1"
    );
    let k = at(home, 60);
    let cx = ctx_in(k, &c, (5, 29), 0.9, g);
    rib.on_event(&typed(), k, &cx);
    rib.on_event(&typed_move((5, 28), (5, 29)), k, &cx);
    rib.plan(&cx);
    let inside_prompt: Vec<u16> = rib
        .cells()
        .iter()
        .filter(|l| l.row == 5 && l.col < 28)
        .map(|l| l.col)
        .collect();
    assert!(
        inside_prompt.is_empty(),
        "a pending relocation settled onto the prompt: columns {inside_prompt:?}"
    );
}

const fn typed_move(from: (u16, u16), to: (u16, u16)) -> Event {
    Event::Move {
        from,
        to,
        licence: Licence::Typed,
        dir: Dir::of(to.1 as i32 - from.1 as i32, to.0 as i32 - from.0 as i32),
    }
}

const fn typed_space() -> Event {
    Event::Typed {
        cells: 1,
        shifted: false,
        class: TypedClass::Space,
    }
}

fn interior_covs(rib: &Ribbon, col: u16) -> Vec<u8> {
    let cw = geom().cw as f32;
    let (x0, x1) = (f32::from(col) * cw, f32::from(col + 1) * cw);
    rib.plan_segments()
        .iter()
        .filter(|s| s.x > x0 + 0.5 && s.x < x1 - 0.5)
        .map(|s| s.cov)
        .collect()
}

fn boundary_cov(rib: &Ribbon, col: u16) -> Option<u8> {
    let x = f32::from(col) * geom().cw as f32;
    rib.plan_segments()
        .iter()
        .find(|s| (s.x - x).abs() < 0.5)
        .map(|s| s.cov)
}

#[test]
fn a_coalesced_echo_leaves_one_cohort_and_no_feather_inside_the_row() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 6, &c, 0.9);
    let k1 = at(t0, 360);
    let k2 = at(t0, 370);
    let now = at(t0, 380);
    let cx = ctx(now, &c, (2, 10), 0.9);
    rib.on_event(&typed(), k1, &cx);
    rib.on_event(&typed(), k2, &cx);
    rib.on_event(
        &Event::Sweep {
            row: 2,
            col0: 8,
            col1: 10,
        },
        k1,
        &cx,
    );
    rib.on_event(&typed_move((2, 8), (2, 10)), now, &cx);
    rib.plan(&cx);
    assert_eq!(
        rib.cohorts().iter().filter(|k| k.row == 2).count(),
        1,
        "the batch's last cell joined the row's one cohort: {:?}",
        rib.cohorts()
    );
    for col in 2..10u16 {
        assert!(
            rib.cells().iter().any(|l| l.row == 2 && l.col == col),
            "col {col} is laid"
        );
    }
    let cx = ctx(at(t0, 500), &c, (2, 10), 0.9);
    rib.plan(&cx);
    let body = boundary_cov(&rib, 6).expect("a boundary inside the band");
    for col in 3..10u16 {
        let b = boundary_cov(&rib, col).expect("boundary {col}");
        assert!(
            u32::from(b) * 10 >= u32::from(body) * 9,
            "boundary {col} is feathered: {b} against the body's {body}"
        );
    }
}

#[test]
fn a_boundary_takes_the_brighter_cell_s_coverage() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 4, &c, 0.9);
    let cx = ctx(at(t0, 400), &c, (2, 6), 0.9);
    rib.plan(&cx);
    let settled = interior_covs(&rib, 5);
    let edge = boundary_cov(&rib, 6).expect("the head's right edge");
    let k = at(t0, 420);
    let cx = ctx(at(t0, 422), &c, (2, 7), 0.9);
    rib.on_event(&typed(), k, &cx);
    rib.plan(&cx);
    assert_eq!(
        interior_covs(&rib, 5),
        settled,
        "cell 5 dipped under cell 6's attack"
    );
    assert!(
        boundary_cov(&rib, 6).expect("the shared boundary") >= edge - 1,
        "the shared boundary sagged toward the attacking cell"
    );
    let laid: Vec<u8> = interior_covs(&rib, 6);
    assert!(
        laid.iter().all(|&v| v + 1 >= edge),
        "the cell laid where the attach stood keeps the attach's level: {laid:?} under {edge}"
    );
    let young: Vec<u8> = interior_covs(&rib, 7);
    assert!(
        !young.is_empty() && young.iter().all(|&v| v < edge),
        "the attack lives in the attach under the caret cell: {young:?} under {edge}"
    );
    let cx = ctx(at(t0, 700), &c, (2, 7), 0.9);
    rib.plan(&cx);
    let hold = interior_covs(&rib, 5);
    let hold_edge = boundary_cov(&rib, 6).expect("edge");
    erase_at(&mut rib, at(t0, 720), (2, 6), &c);
    for ms in [730u64, 800, 900, 940, 970, 1000] {
        let cx = ctx(at(t0, ms), &c, (2, 6), 0.9);
        rib.plan(&cx);
        assert_eq!(
            interior_covs(&rib, 5),
            hold,
            "+{ms} ms: the survivor sagged"
        );
        assert!(
            boundary_cov(&rib, 6).expect("edge") + 1 >= hold_edge,
            "+{ms} ms: the survivor's edge sagged"
        );
    }
    assert!(
        !rib.cells().iter().any(|l| l.col == 6),
        "the erased cell retired inside {RETRACT_FADE_S} s"
    );
}

#[test]
fn an_erase_whose_echo_lands_a_tick_late_still_retracts_the_erased_cell() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 8, &c, 0.9);
    let last = at(t0, 7 * 60);
    let k = at(last, 300);
    let cx = ctx(k, &c, (2, 10), 0.9);
    rib.on_event(&Event::Erase, k, &cx);
    rib.plan(&cx);
    assert!(
        rib.cells().iter().all(|l| l.retract_at.is_none()),
        "nothing right of the pre-retreat caret to retract"
    );
    let now = at(last, 316);
    let cx = ctx(now, &c, (2, 9), 0.9);
    rib.on_event(&typed_move((2, 10), (2, 9)), now, &cx);
    rib.plan(&cx);
    let erased = rib
        .cells()
        .iter()
        .find(|l| l.row == 2 && l.col == 9)
        .expect("the erased glyph's cell is still spending");
    assert!(
        erased.retract_at.is_some(),
        "the erased cell stayed lit under the blank"
    );
    assert!(
        rib.cells()
            .iter()
            .filter(|l| l.col < 9)
            .all(|l| l.retract_at.is_none()),
        "only the erased cell retracts"
    );
    let now = at(last, 400);
    let cx = ctx(now, &c, (2, 10), 0.9);
    rib.on_event(&typed(), now, &cx);
    rib.plan(&cx);
    assert_eq!(rib.pending_erase, 0);
}

#[test]
fn a_typed_row_change_lets_the_old_row_leave_on_its_own_clock() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let keys = Keys {
        g: geom(),
        row: 2,
        col0: 100,
        n: 20,
        period_ms: 60,
        disp: 0.9,
    };
    type_keys(&mut rib, t0, keys, &c);
    let last = at(t0, 19 * 60);
    let now = at(last, 60);
    let cx = ctx(now, &c, (3, 0), 0.9);
    rib.on_event(&typed_move((2, 120), (3, 0)), now, &cx);
    rib.plan(&cx);
    let old = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 2)
        .expect("the old row");
    let last_key_on_the_old_row = old.alive_at;
    assert!(
        !old.abandoned,
        "a fold abandoned the row the wrap came off: the band is killed at every wrap"
    );
    assert_eq!(
        old.retract_col, None,
        "nothing froze the old row's retract target: it has not entered one"
    );
    assert!(
        last_key_on_the_old_row >= at(t0, 19 * 60),
        "the old row keeps the life its last key gave it"
    );
    let keys = Keys {
        g: geom(),
        row: 3,
        col0: 0,
        n: 12,
        period_ms: 60,
        disp: 0.9,
    };
    type_keys(&mut rib, at(now, 60), keys, &c);
    let mut sink = Sink::default();
    let cx = ctx(at(now, 500), &c, (3, 12), 0.9);
    rib.plan(&cx);
    let old = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 2)
        .expect("the old row at +0.5 s");
    assert!(old.flow.is_some(), "the fold set the old row flowing");
    assert_eq!(old.phase, Phase::Retracting, "…and it is moving at +0.5 s");
    assert_eq!(
        old.retract_col,
        Some(119),
        "toward the fold point: the row's own right end"
    );
    assert_eq!(
        rib.cells().iter().filter(|l| l.row == 2).count(),
        20,
        "the row the wrap came off went dark under the hand: the band does not span the fold"
    );
    let mut f = sink.frame();
    rib.emit(&cx, &mut f);
    let (cov, _, left) = row_light(&sink, geom(), 2).expect("row 2 is lit at +0.5 s");
    assert!(cov > 0.0);
    assert!(
        left > 100.0 * geom().cw as f32 + 0.5,
        "the far (left) end drained first: the left edge is at {left} px"
    );
    let gone_ms = (FLOW_TOTAL_S * 1000.0) as u64 + 60;
    let cx = ctx(at(now, gone_ms), &c, (3, 12), 0.9);
    rib.plan(&cx);
    assert!(
        !rib.cells().iter().any(|l| l.row == 2),
        "the old row is still lit {FLOW_TOTAL_S} s + 60 ms after the fold"
    );
    assert!(rib.cells().iter().any(|l| l.row == 3), "the new row is lit");
}

fn a_line_of_thirty(rib: &mut Ribbon, t0: Instant, c: &Config) -> f32 {
    type_run(rib, t0, 2, 30, c, 0.9);
    let band = rib.cohorts()[0];
    let t_next = band.t_at(32);
    assert_eq!(
        t_next.to_bits(),
        (band.t0 + walk_t(f32::from(32 - band.anchor_col))).to_bits(),
        "the premise: a cold walk, read as it was before the odometer"
    );
    t_next
}

const fn row_change(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
    Event::Move {
        from,
        to,
        licence,
        dir: Dir::of(to.1 as i32 - from.1 as i32, to.0 as i32 - from.0 as i32),
    }
}

#[test]
fn a_composer_gate_waits_for_its_home_and_only_its_first_real_echo_spends_it() {
    let model = aterm_models_effects::rainbow_composer_newline_gate_model();
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let t_next = a_line_of_thirty(&mut rib, t0, &c);
    let chord = at(t0, 29 * 60 + 100);
    let old = ctx(chord, &c, (2, 32), 0.9);

    rib.on_event(&Event::ComposerNewline, chord, &old);
    let armed = model.successors("ComposerKey", &model.init_state())[0].clone();
    assert_eq!(armed.get("moved"), Some(&0));
    assert_eq!(
        rib.fresh_line,
        Some(FreshLine {
            since: chord,
            chord_at: Some(chord),
            awaiting_move: true,
            left_row: None,
        })
    );

    rib.on_event(&typed(), chord, &old);
    let stamped = model.successors("ChordStamp", &armed)[0].clone();
    assert_eq!(stamped.get("spent"), Some(&0));
    assert!(rib.fresh_line.is_some_and(|gate| gate.awaiting_move));

    let key = at(chord, 10);
    rib.on_event(&typed(), key, &ctx(key, &c, (2, 32), 0.9));
    let raced = model.successors("TypeAheadBeforeHome", &stamped)[0].clone();
    assert_eq!(raced.get("spent"), Some(&0));
    assert!(rib.fresh_line.is_some_and(|gate| gate.awaiting_move));

    let seen = at(chord, 30);
    rib.on_event(
        &row_change((2, 32), (2, 2), Licence::Return),
        seen,
        &ctx(seen, &c, (2, 2), 0.9),
    );
    let homed = model.successors("ObserveHome", &raced)[0].clone();
    assert_eq!(homed.get("moved"), Some(&1));
    assert_eq!(
        rib.fresh_line.map(|gate| (gate.since, gate.awaiting_move)),
        Some((chord, false))
    );

    let echo = at(seen, 20);
    rib.on_event(
        &Event::Sweep {
            row: 2,
            col0: 2,
            col1: 3,
        },
        key,
        &ctx(echo, &c, (2, 3), 0.9),
    );
    let echoed = model.successors("FirstEcho", &homed)[0].clone();
    assert_eq!(echoed.get("spent"), Some(&1));
    assert!(rib.fresh_line.is_none());
    let line = rib
        .cohorts()
        .iter()
        .find(|q| q.row == 2 && q.anchor_col == 2 && q.born > chord)
        .expect("the first echo made the new line's cohort");
    assert_eq!((line.t0.to_bits(), line.d0), (t_next.to_bits(), 0.0));

    let mut plain = Ribbon::new();
    a_line_of_thirty(&mut plain, t0, &c);
    let plain_state = model.successors("PlainModifiedKey", &model.init_state())[0].clone();
    plain.on_event(&typed(), chord, &old);
    plain.on_event(
        &row_change((2, 32), (2, 2), Licence::Typed),
        seen,
        &ctx(seen, &c, (2, 2), 0.9),
    );
    assert!(!model.action_enabled("ObserveHome", &plain_state));
    assert!(
        plain.fresh_line.is_none(),
        "a non-composer park arms no line gate"
    );

    let mut interrupted = Ribbon::new();
    a_line_of_thirty(&mut interrupted, t0, &c);
    interrupted.on_event(&Event::ComposerNewline, chord, &old);
    assert!(
        interrupted
            .fresh_line
            .is_some_and(|gate| gate.awaiting_move)
    );
    interrupted.curtain(at(chord, 1));
    assert!(
        interrupted.fresh_line.is_some_and(|gate| {
            !gate.awaiting_move && gate.chord_at.is_none() && gate.left_row.is_none()
        }),
        "a new coordinate space cannot wait for the old composer's home"
    );

    let pending = model.successors("ComposerKey", &model.init_state())[0].clone();
    let cancelled = model.successors("CancelBeforeHome", &pending)[0].clone();
    assert_eq!(cancelled.get("composer"), Some(&0));
    for superseding in [
        Event::CancelComposerNewline,
        Event::Erase,
        Event::Kill {
            cells: 1,
            scope: super::super::KillScope::Word,
        },
        row_change((2, 32), (2, 31), Licence::Nav),
    ] {
        let mut interrupted = Ribbon::new();
        a_line_of_thirty(&mut interrupted, t0, &c);
        interrupted.on_event(&Event::ComposerNewline, chord, &old);
        interrupted.on_event(&superseding, at(chord, 1), &old);
        assert!(
            interrupted.fresh_line.is_none(),
            "{superseding:?} left the cancelled composer gate pending"
        );
    }
}

fn row_light(sink: &Sink, g: Geom, row: u16) -> Option<(f32, f32, f32)> {
    let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
    let (mut cov, mut num, mut left) = (0.0f32, 0.0f32, f32::INFINITY);
    for q in &sink.under {
        let y0 = f32::from(q.y);
        let y1 = y0 + f32::from(q.h);
        if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
            continue;
        }
        let w = f32::from(q.w) * f32::from(q.alpha) / 255.0;
        cov += w;
        num += (f32::from(q.x) + f32::from(q.w) * 0.5) * w;
        left = left.min(f32::from(q.x));
    }
    (cov > 0.0).then(|| (cov / (g.cols * g.cw) as f32, num / cov, left))
}

fn drift_census(sink: &Sink, g: Geom, row: u16) -> Option<DriftCensus> {
    let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
    let mut cov = 0.0f32;
    let (mut left, mut right) = (f32::INFINITY, f32::NEG_INFINITY);
    for q in &sink.under {
        let y0 = f32::from(q.y);
        let y1 = y0 + f32::from(q.h);
        if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
            continue;
        }
        cov += f32::from(q.w) * f32::from(q.alpha) / 255.0;
        left = left.min(f32::from(q.x));
        right = right.max(f32::from(q.x) + f32::from(q.w));
    }
    (cov > 0.0).then(|| DriftCensus {
        cov: cov / (g.cols * g.cw) as f32,
        left,
        right,
    })
}

struct DriftCensus {
    cov: f32,
    left: f32,
    right: f32,
}

fn seam_far_tail(sink: &Sink, g: Geom, row: u16, head_col: u16) -> (f32, f32) {
    let cw = g.cw as f32;
    let head_x = f32::from(g.origin_x) + f32::from(head_col) * cw;
    let (y_lo, y_hi) = (
        f32::from(g.origin_y) + (f32::from(row) - 0.5) * g.ch as f32,
        f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32,
    );
    let (mut far, mut left) = (0.0f32, f32::INFINITY);
    for q in &sink.out {
        let (x, y) = (f32::from(q.x), f32::from(q.y));
        if q.w == 0 || y < y_lo || y >= y_hi || x >= head_x - (HOT_EDGE_CELLS + 1.0) * cw {
            continue;
        }
        let lum = (q.color >> 16 & 0xFF) + (q.color >> 8 & 0xFF) + (q.color & 0xFF);
        far += f32::from(q.w) * lum as f32;
        if lum > 0 {
            left = left.min(x);
        }
    }
    (far, left)
}

fn light_between(sink: &Sink, g: Geom, row: u16, x0: f32, x1: f32) -> f32 {
    let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
    let mut cov = 0.0f32;
    for q in &sink.under {
        let y0 = f32::from(q.y);
        let y1 = y0 + f32::from(q.h);
        if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
            continue;
        }
        let (qx0, qx1) = (f32::from(q.x), f32::from(q.x) + f32::from(q.w));
        let w = (qx1.min(x1) - qx0.max(x0)).max(0.0);
        cov += w * f32::from(q.alpha) / 255.0;
    }
    cov / (g.cols * g.cw) as f32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BsEcho {
    SameTick,
    Late(u64),
}

type BsFrame = (u64, Option<(f32, f32, f32)>, bool, Vec<Phase>);

fn bs_back_through_a_fold(folded: bool, echo: BsEcho, bs_ms: &[u64]) -> (Vec<BsFrame>, f32) {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let keys = Keys {
        g,
        row: 2,
        col0: 100,
        n: 20,
        period_ms: 60,
        disp: 0.9,
    };
    type_keys(&mut rib, t0, keys, &c);
    let rest = rib.rest_s();
    let fold = at(t0, 20 * 60);
    let mut caret = (2u16, 120u16);
    if folded {
        caret = (3, 0);
        let cx = ctx(fold, &c, caret, 0.9);
        rib.on_event(&typed_move((2, 120), caret), fold, &cx);
        rib.plan(&cx);
    }
    let mut sink = Sink::default();
    let mut out = Vec::new();
    let mut k = 0usize;
    let mut due: Option<(u64, (u16, u16))> = None;
    let mut ms = 16u64;
    while ms <= 1520 {
        if k < bs_ms.len() && ms >= bs_ms[k] && due.is_none() {
            let key = at(fold, bs_ms[k]);
            let to = if caret.0 == 3 {
                (2, 119)
            } else {
                (2, caret.1 - 1)
            };
            match echo {
                BsEcho::SameTick => {
                    let cx = ctx(key, &c, to, 0.9);
                    rib.on_event(&Event::Erase, key, &cx);
                    rib.on_event(&typed_move(caret, to), key, &cx);
                    caret = to;
                }
                BsEcho::Late(lag) => {
                    let cx = ctx(key, &c, caret, 0.9);
                    rib.on_event(&Event::Erase, key, &cx);
                    due = Some((bs_ms[k] + lag, to));
                }
            }
            k += 1;
        }
        if let Some((due_ms, to)) = due
            && ms >= due_ms
        {
            let echo_at = at(fold, due_ms);
            let cx = ctx(echo_at, &c, to, 0.9);
            rib.on_event(&typed_move(caret, to), echo_at, &cx);
            caret = to;
            due = None;
        }
        let now = at(fold, ms);
        let cx = ctx(now, &c, caret, 0.9);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let row2: Vec<Cohort> = rib
            .cohorts()
            .iter()
            .filter(|q| q.row == 2)
            .copied()
            .collect();
        out.push((
            ms,
            row_light(&sink, g, 2),
            row2.iter().any(|q| q.flow.is_some()),
            row2.iter().map(|q| q.phase).collect::<Vec<_>>(),
        ));
        ms += 16;
    }
    (out, rest)
}

fn assert_the_row_holds_under_the_hand(
    what: &str,
    folded: &[BsFrame],
    control: &[BsFrame],
    from_ms: u64,
    until_ms: u64,
) {
    let settled = folded
        .iter()
        .find(|s| s.0 >= from_ms)
        .and_then(|s| s.1)
        .unwrap_or_else(|| panic!("{what}: row 2 is lit at +{from_ms} ms"))
        .2;
    let mut read = 0u32;
    for (f, k) in folded.iter().zip(control) {
        let (ms, light, flowing, phases) = f;
        if *ms < from_ms || *ms >= until_ms {
            continue;
        }
        read += 1;
        assert!(
            !flowing,
            "{what}: +{ms} ms: row 2 still flows under the hand that came back to it"
        );
        assert!(
            phases
                .iter()
                .all(|p| matches!(p, Phase::Laying | Phase::Grace)),
            "{what}: +{ms} ms: the row left its hold under the hand: {phases:?}"
        );
        let (cov, _, left) =
            light.unwrap_or_else(|| panic!("{what}: +{ms} ms: row 2 is lit under the hand"));
        assert!(
            (left - settled).abs() <= 0.5,
            "{what}: +{ms} ms: the row's left edge moved from {settled} px to {left} px: the \
             drift restarted"
        );
        let (want, _, _) = k.1.expect("the control is lit");
        assert!(
            cov >= 0.85 * want,
            "{what}: +{ms} ms: row 2 drained under the hand: {cov} against the never-folded \
             control's {want}"
        );
    }
    assert!(
        read >= 10,
        "{what}: the window +{from_ms}..+{until_ms} ms read {read} frames"
    );
}

#[test]
fn the_hand_arriving_back_through_a_shell_fold_takes_the_row_back_after_the_rest_and_alone() {
    let runs: [(&str, &[u64]); 3] = [
        ("a run inside the rest", &[416, 480, 540, 600, 660, 720]),
        ("a run after the rest", &[900, 960, 1020, 1080, 1140]),
        ("a lone Backspace", &[416]),
    ];
    for (name, bs_ms) in runs {
        for echo in [BsEcho::SameTick, BsEcho::Late(0), BsEcho::Late(16)] {
            let what = format!("{name}, {echo:?}");
            let (control, rest) = bs_back_through_a_fold(false, echo, bs_ms);
            let (folded, _) = bs_back_through_a_fold(true, echo, bs_ms);
            let lag = match echo {
                BsEcho::SameTick => 0,
                BsEcho::Late(lag) => lag,
            };
            let rest_ms = (rest * 1000.0) as u64;
            if name == "a run after the rest" {
                assert!(
                    60 + bs_ms[0] >= rest_ms,
                    "{what}: the run starts {} ms after the last key, inside the {rest_ms} \
                     ms rest",
                    60 + bs_ms[0]
                );
            }
            let from = bs_ms[0] + lag;
            let until = bs_ms[bs_ms.len() - 1] + lag + rest_ms;
            assert_the_row_holds_under_the_hand(&what, &folded, &control, from, until);
        }
    }
}

#[test]
fn a_composer_re_wrap_s_moved_word_fades_in_place_while_the_row_it_left_drifts() {
    let c = cfg(true, true);
    let g = geom();
    let cw = g.cw as f32;
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let keys = Keys {
        g,
        row: 2,
        col0: 40,
        n: 30,
        period_ms: 60,
        disp: 0.9,
    };
    type_keys(&mut rib, t0, keys, &c);
    for i in 0..9u16 {
        let now = at(t0, u64::from(30 + i) * 60);
        let ev = if i == 0 { typed_space() } else { typed() };
        let cx = ctx(now, &c, (2, 71 + i), 0.9);
        rib.on_event(&ev, now, &cx);
        rib.plan(&cx);
    }
    let wrap = at(t0, 39 * 60);
    let cx = ctx(wrap, &c, (3, 11), 0.9);
    rib.on_event(&typed(), wrap, &cx);
    rib.on_event(&typed_move((2, 79), (3, 11)), wrap, &cx);
    rib.plan(&cx);
    let (word_x0, word_x1) = (70.0 * cw, 79.0 * cw);
    let kept_end = 70.0 * cw;
    let mut sink = Sink::default();
    let mut f = sink.frame();
    rib.emit(&cx, &mut f);
    let first = drift_census(&sink, g, 2).expect("row 2 is lit at the wrap");
    let word0 = light_between(&sink, g, 2, word_x0, word_x1);
    assert!(
        word0 > 0.0 && (first.right - word_x1).abs() <= 0.5,
        "the fixture's point: the moved word is lit on row 2 at the wrap ({word0}, right \
         edge {} px)",
        first.right
    );
    let mut prev = first.cov;
    let mut worst = 0.0f32;
    let mut clipped = 0u32;
    let mut key_col = 11u16;
    let mut ms = 16u64;
    while ms <= 1600 {
        let now = at(wrap, ms);
        if ms / 60 != (ms - 16) / 60 {
            key_col += 1;
            let kx = ctx(now, &c, (3, key_col), 0.9);
            rib.on_event(&typed(), now, &kx);
        }
        let cx = ctx(now, &c, (3, key_col), 0.9);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let Some(r) = drift_census(&sink, g, 2) else {
            worst = worst.max(prev / first.cov);
            prev = 0.0;
            ms += 16;
            continue;
        };
        if ms == 16 {
            let word = light_between(&sink, g, 2, word_x0, word_x1);
            assert!(
                word >= 0.8 * word0,
                "the moved word was cut on the flow's first frame: {word} of its {word0}"
            );
        }
        worst = worst.max((prev - r.cov) / first.cov);
        assert!(
            r.right <= word_x1 + 0.5,
            "light at {} px, past the moved word's old end at {word_x1} px (+{ms} ms)",
            r.right
        );
        if !rib.cells().iter().any(|l| l.row == 2 && l.col >= 70) {
            clipped += 1;
            assert!(
                r.right <= kept_end + 0.5,
                "CLIP: light at {} px, past the row's kept end at {kept_end} px (+{ms} ms)",
                r.right
            );
        }
        prev = r.cov;
        ms += 16;
    }
    assert!(
        worst <= 0.04,
        "NO CLIFF: a frame took {:.1} % of the wrap frame's light",
        100.0 * worst
    );
    assert!(
        clipped >= 20,
        "CLIP was read on the drifting band after the word had gone: {clipped} frames"
    );
}

#[test]
fn a_band_carried_down_off_the_caret_s_row_drifts_into_its_kept_start_and_never_past_it() {
    let c = cfg(true, true);
    let g = geom();
    let cw = g.cw as f32;
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let keys = Keys {
        g,
        row: 5,
        col0: 20,
        n: 30,
        period_ms: 60,
        disp: 0.9,
    };
    type_keys(&mut rib, t0, keys, &c);
    let carry = at(t0, 30 * 60);
    let id = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 5)
        .map(|k| k.id)
        .expect("the band");
    let run = super::super::witness::FollowRun {
        row: 5,
        cohort: id,
        lo: 20,
        hi: 49,
        dr: 1,
    };
    let mut moved = Vec::new();
    assert_eq!(rib.translate_run(run, carry, true, &mut moved), 30);
    let cx = ctx(carry, &c, (5, 50), 0.9);
    rib.plan(&cx);
    let mut sink = Sink::default();
    let mut f = sink.frame();
    rib.emit(&cx, &mut f);
    let first = drift_census(&sink, g, 6).expect("row 6 is lit at the carry");
    let start = f32::from(g.origin_x) + 20.0 * cw;
    assert!(
        (first.left - start).abs() <= 0.5,
        "the fixture's point: the band starts on the clip line: {} px",
        first.left
    );
    let mut drifted = false;
    let mut ms = 16u64;
    while ms <= 1600 {
        let now = at(carry, ms);
        let cx = ctx(now, &c, (5, 50), 0.9);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        if ms == 16 {
            let coh = rib
                .cohorts()
                .iter()
                .find(|k| k.id == id)
                .copied()
                .expect("the carried band");
            assert!(coh.flow.is_some(), "the carry set the band flowing");
            assert!(
                coh.drain_right_first && coh.retract_col == Some(20),
                "a row below the caret flows into its own left end: {:?}, right first {}",
                coh.retract_col,
                coh.drain_right_first
            );
        }
        if let Some(r) = drift_census(&sink, g, 6) {
            assert!(
                r.left >= start - 0.5,
                "CLIP: light at {} px, past the band's kept start at {start} px (+{ms} ms)",
                r.left
            );
            drifted |= r.right < first.right - cw;
        }
        ms += 16;
    }
    assert!(drifted, "the band drifted toward its start");

    for below in [false, true] {
        for edge_disp in [0.0, 1.0] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 0, 1, &c, 0.0);
            rib.plan(&ctx(at(t0, 5000), &c, (2, 1), 0.0));
            assert!(rib.at_rest());
            let born = at(t0, 6000);
            type_run(&mut rib, born, 10, 2, &c, 0.0);
            let edge_at = at(born, 120);
            let cx = ctx(edge_at, &c, (2, 13), edge_disp);
            rib.on_event(&typed(), edge_at, &cx);
            rib.plan(&cx);
            let edge = *rib
                .cells()
                .iter()
                .find(|cell| cell.col == 12)
                .expect("edge");
            let coh = *rib
                .cohorts()
                .iter()
                .find(|coh| coh.id == edge.cohort)
                .expect("band");
            let reach_at = edge_at + Duration::from_secs_f32(coh.grace_s + 0.001);
            assert_eq!(rib.retire_cells(&[(2, 12, edge.born)], reach_at), 1);
            rib.plan(&ctx(reach_at, &c, (2, 13), 0.0));
            let reach = *rib
                .cells()
                .iter()
                .find(|cell| cell.col == 9)
                .expect("reach");
            assert!(!reach.leaving() && reach.born < edge.born);
            let mut moved = Vec::new();
            assert_eq!(
                rib.shift_run(
                    super::super::witness::ShiftRun {
                        row: 2,
                        cohort: coh.id,
                        lo: 9,
                        hi: 9,
                        dc: 3,
                    },
                    &mut moved
                ),
                1
            );
            assert_eq!(
                rib.shift_run(
                    super::super::witness::ShiftRun {
                        row: 2,
                        cohort: coh.id,
                        lo: 10,
                        hi: 11,
                        dc: -8,
                    },
                    &mut moved
                ),
                2
            );
            let (edge_col, kept) = if below {
                assert_eq!(
                    rib.shift_run(
                        super::super::witness::ShiftRun {
                            row: 2,
                            cohort: coh.id,
                            lo: 2,
                            hi: 3,
                            dc: 18,
                        },
                        &mut moved
                    ),
                    2
                );
                (12, 10.0)
            } else {
                (12, 11.0)
            };
            let residents: Vec<_> = rib
                .cells()
                .iter()
                .filter(|cell| cell.col == edge_col)
                .collect();
            assert_eq!(residents.len(), 2);
            assert!(residents[0].leaving() && residents[0].born == edge.born);
            assert!(!residents[1].leaving() && residents[1].born == reach.born);
            assert_eq!(residents[0].birth_disp, edge_disp);
            assert_eq!(residents[1].birth_disp, 0.0);
            assert_eq!(
                rib.translate_run(
                    super::super::witness::FollowRun {
                        row: 2,
                        cohort: coh.id,
                        lo: 0,
                        hi: u16::MAX,
                        dr: 1,
                    },
                    reach_at,
                    true,
                    &mut moved
                ),
                4
            );
            let caret = (if below { 1 } else { 4 }, 13);
            rib.plan(&ctx(at(reach_at, 1), &c, caret, 0.0));
            let flowed = rib
                .cohorts()
                .iter()
                .find(|coh| coh.id == edge.cohort)
                .expect("flow");
            assert_eq!(flowed.retract_col, Some(edge_col));
            assert_eq!(flowed.drain_right_first, below);
            let priced = if edge_disp == 0.0 {
                FLOW_DRIFT_MIN_CELLS
            } else {
                FLOW_DRIFT_MAX_CELLS
            };
            assert_eq!(flowed.flow_drift, priced.min(FLOW_DRIFT_MAX_SHARE * kept));
        }
    }
}

#[test]
fn a_band_move_under_the_hand_carries_the_seam_and_its_floor_together() {
    let c = cfg(true, true);
    let g = geom();
    let read = |moved: bool| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 100,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let fold = at(t0, 20 * 60);
        let cx = ctx(fold, &c, (3, 0), 0.9);
        rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
        rib.plan(&cx);
        let keys = Keys {
            g,
            row: 3,
            col0: 0,
            n: 12,
            period_ms: 20,
            disp: 0.9,
        };
        type_keys(&mut rib, at(fold, 20), keys, &c);
        let row = if moved {
            rib.translate_band(0, 39, 1);
            4
        } else {
            3
        };
        let now = at(fold, 300);
        let cx = ctx(now, &c, (row, 12), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        (seam_far_tail(&sink, g, row, 12).0, rib.hand_floor_cell())
    };
    let (still, floor_still) = read(false);
    let (moved, floor_moved) = read(true);
    assert!(still > 0.0, "the seam holds the new line's far tail");
    assert_eq!(floor_still, Some((3, 0)));
    assert_eq!(
        floor_moved,
        Some((4, 0)),
        "the hand's floor rides the band move with its text"
    );
    assert!(
        (moved - still).abs() <= 0.01 * still,
        "the seam went with its row: {moved} on row 4 after the move, {still} on row 3 \
         without it"
    );
}

#[test]
fn under_reduced_motion_a_flowing_band_asks_for_no_wake_before_its_fade() {
    let mut c = cfg(true, true);
    c.reduced_motion = true;
    let g = geom();
    let opens_ms = (FLOW_TOTAL_S * 1000.0 - REDUCED_MOTION_FADE_MS).round() as u64;
    for pause_ms in [60u64, 960] {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 100,
            n: 20,
            period_ms: 60,
            disp: 0.2,
        };
        type_keys(&mut rib, t0, keys, &c);
        let fold = at(t0, 19 * 60 + pause_ms);
        let cx = ctx(fold, &c, (3, 0), 0.2);
        rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
        rib.plan(&cx);
        assert!(
            rib.cohorts().iter().any(|k| k.row == 2 && k.flow.is_some()),
            "pause {pause_ms} ms: the fold set the row flowing"
        );
        let earliest = at(fold, opens_ms) - super::super::TAIL_FLOOR;
        let mut ms = 100u64;
        while ms <= 1300 {
            let now = at(fold, ms);
            let cx = ctx(now, &c, (3, 0), 0.2);
            rib.plan(&cx);
            let d = rib.next_change_deadline(now).expect("the band is live");
            assert!(
                d >= earliest,
                "pause {pause_ms} ms: the read at +{ms} ms asks for a wake at +{} ms, before \
                 the fade opens at +{opens_ms} ms — nothing on the glass changes",
                d.saturating_duration_since(fold).as_millis()
            );
            ms += 25;
        }
    }
}

#[test]
fn a_shift_of_a_flowing_band_carries_its_frozen_fold_edge() {
    let c = cfg(true, true);
    let g = geom();
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let keys = Keys {
        g,
        row: 2,
        col0: 60,
        n: 40,
        period_ms: 60,
        disp: 0.9,
    };
    type_keys(&mut rib, t0, keys, &c);
    let fold = at(t0, 40 * 60);
    let cx = ctx(fold, &c, (3, 0), 0.9);
    rib.on_event(&typed_move((2, 100), (3, 0)), fold, &cx);
    rib.plan(&cx);
    let cx = ctx(at(fold, 16), &c, (3, 0), 0.9);
    rib.plan(&cx);
    let flow = rib
        .cohorts()
        .iter()
        .find(|k| k.row == 2 && k.flow.is_some())
        .copied()
        .expect("the folded row flows");
    assert_eq!(flow.retract_col, Some(99), "frozen at the row's kept end");
    let back = at(fold, 64);
    let cx = ctx(back, &c, (2, 80), 0.9);
    rib.on_event(&nav((3, 0), (2, 80)), back, &cx);
    rib.plan(&cx);
    let key = at(fold, 80);
    let cx = ctx(key, &c, (2, 81), 0.9);
    rib.on_event(&typed(), key, &cx);
    let shift = super::super::witness::ShiftRun {
        row: 2,
        cohort: flow.id,
        lo: 80,
        hi: 99,
        dc: 1,
    };
    let mut moved = Vec::new();
    assert_eq!(rib.shift_run(shift, &mut moved), 20, "the tail moved");
    rib.plan(&cx);
    let after = rib
        .cohorts()
        .iter()
        .find(|k| k.id == flow.id)
        .copied()
        .expect("the band");
    assert!(after.flow.is_some(), "the row is still flowing");
    assert_eq!(
        after.retract_col,
        Some(100),
        "the frozen fold edge rides the shift with the glyph it bounds"
    );
    let mut sink = Sink::default();
    for ms in [96u64, 160, 320, 640] {
        let now = at(fold, ms);
        let cx = ctx(now, &c, (2, 81), 0.9);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        assert!(
            cell_lit(&sink.under, g, 2, 100),
            "+{ms} ms: the pushed glyph's column 100 is dark"
        );
    }
}

#[test]
fn a_slow_hand_s_folded_row_flows_whole_whatever_it_paused_before_the_wrap() {
    let c = cfg(true, true);
    let g = geom();
    let flow = |pause_ms: u64| {
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 90,
            n: 30,
            period_ms: 150,
            disp: 0.2,
        };
        type_keys(&mut rib, t0, keys, &c);
        let fold = at(t0, 29 * 150 + pause_ms);
        let cx = ctx(fold, &c, (3, 0), 0.2);
        rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
        rib.plan(&cx);
        assert!(
            rib.cohorts().iter().any(|k| k.row == 2 && k.flow.is_some()),
            "pause {pause_ms} ms: the fold set the row flowing"
        );
        let mut sink = Sink::default();
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let first = row_light(&sink, g, 2).expect("lit at the fold").0;
        let (mut prev, mut worst, mut share, mut dark) = (first, 0.0f32, None, None);
        let mut key_col = 0u16;
        let mut ms = 16u64;
        while ms <= 1700 {
            let now = at(fold, ms);
            if ms / 150 != (ms - 16) / 150 {
                key_col += 1;
                let kx = ctx(now, &c, (3, key_col), 0.2);
                rib.on_event(&typed(), now, &kx);
            }
            let cx = ctx(now, &c, (3, key_col), 0.2);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let cov = row_light(&sink, g, 2).map_or(0.0, |l| l.0);
            assert!(
                dark.is_none() || cov == 0.0,
                "pause {pause_ms} ms: lit again at +{ms} ms after going dark"
            );
            if cov == 0.0 && dark.is_none() {
                dark = Some(ms as f32 / 1000.0);
            }
            worst = worst.max((prev - cov) / first);
            if ms == 1008 {
                share = Some(cov / first);
            }
            prev = cov;
            ms += 16;
        }
        (
            dark.expect("the folded row goes dark"),
            share.expect("read at +1.0 s"),
            worst,
        )
    };
    let base = flow(60);
    let (dark0, share0, _) = base;
    for (pause_ms, (dark, share, worst)) in [(60u64, base), (600, flow(600)), (900, flow(900))] {
        assert!(
            (0.8 * FLOW_TOTAL_S..=FLOW_TOTAL_S + 0.017).contains(&dark),
            "pause {pause_ms} ms: LIFE: dark at +{dark} s"
        );
        assert!(
            worst <= 0.04,
            "pause {pause_ms} ms: NO CLIFF: a frame took {:.1} % of the fold frame's light",
            100.0 * worst
        );
        assert!(
            (dark - dark0).abs() <= 0.017 && (share - share0).abs() <= 0.01,
            "pause {pause_ms} ms: the flow's timeline depends on the pause: dark at +{dark} s \
             and {share} of the light at +1.0 s, where the 60 ms pause reads +{dark0} s and \
             {share0}"
        );
    }
}

#[test]
fn a_program_round_trip_does_not_strand_the_band_under_a_typing_hand() {
    let c = cfg(true, true);
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_run(&mut rib, t0, 2, 8, &c, 0.9);
    let last = at(t0, 7 * 60);
    let cx = ctx(at(last, 100), &c, (3, 0), 0.9);
    rib.on_event(&pty((2, 10), (3, 0)), at(last, 100), &cx);
    rib.plan(&cx);
    let coh = rib.cohorts().iter().find(|k| k.row == 2).expect("the band");
    assert!(coh.abandoned && coh.rejoinable);
    assert_eq!(
        coh.retract_col,
        Some(10),
        "retracts toward the hand, not the parked cursor"
    );
    let cx = ctx(at(last, 116), &c, (2, 10), 0.9);
    rib.on_event(&pty((3, 0), (2, 10)), at(last, 116), &cx);
    rib.plan(&cx);
    let k = at(last, 200);
    let cx = ctx(k, &c, (2, 11), 0.9);
    rib.on_event(&typed(), k, &cx);
    rib.plan(&cx);
    let cohorts: Vec<_> = rib.cohorts().iter().filter(|k| k.row == 2).collect();
    assert_eq!(cohorts.len(), 1, "one band, taken back: {cohorts:?}");
    assert!(!cohorts[0].abandoned);
    assert_eq!((cohorts[0].col0, cohorts[0].col1), (2, 11));
    assert!(
        rib.cells()
            .iter()
            .filter(|l| l.row == 2)
            .all(|l| l.retract_at.is_none()),
        "no cell is left draining under the hand"
    );
}

const fn geom_owner() -> Geom {
    Geom {
        cw: 30,
        ch: 56,
        rows: 60,
        cols: 144,
        origin_x: 0,
        origin_y: 0,
        win_w: 4320,
        win_h: 3360,
        head: 0,
    }
}

const fn cfg_owner() -> Config {
    let mut c = cfg(true, true);
    c.intensity = 0.70;
    c
}

const OWNER_ROW: u16 = 56;

fn type_owner_line(
    rib: &mut Ribbon,
    t0: Instant,
    c: &Config,
    disp: f32,
    period_ms: u64,
    pause_ms: u64,
) -> (u64, u16) {
    let g = geom_owner();
    let mut ms = 0u64;
    let mut col = 2u16;
    let mut last = 0u64;
    for (i, _) in "and I also seee an ugly gap".chars().enumerate() {
        let now = at(t0, ms);
        let cx = ctx_in(now, c, (OWNER_ROW, col + 1), disp, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        col += 1;
        last = ms;
        ms += period_ms;
        if i == 10 {
            ms += pause_ms;
        }
    }
    (last, col)
}

fn owner_frame(rib: &mut Ribbon, sink: &mut Sink, c: &Config, now: Instant, caret: u16, disp: f32) {
    let cx = ctx_in(now, c, (OWNER_ROW, caret), disp, geom_owner());
    rib.plan(&cx);
    let mut f = sink.frame();
    rib.emit(&cx, &mut f);
}

fn owner_rows() -> std::ops::Range<i32> {
    let g = geom_owner();
    let top = i32::from(OWNER_ROW) * g.ch as i32;
    top - g.ch as i32 / 2..top + 2 * g.ch as i32
}

#[test]
fn the_caret_block_stands_on_the_band_under_a_cold_hand_and_a_hot_one() {
    for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let (last, caret) = type_owner_line(&mut rib, t0, &c, disp, 83, 700);
        let mut sink = Sink::default();
        owner_frame(&mut rib, &mut sink, &c, at(t0, last + 100), caret, disp);
        assert_eq!(
            rib.slabs_per_cell(),
            3,
            "{hand}: the composer line alone plans at three slabs"
        );
        let head_x0 = i32::from(caret - 1) * cw;
        let block_x1 = i32::from(caret + 1) * cw;
        let x0 = head_x0 - 3 * cw;
        let cens = column_census(&sink.under, x0..block_x1, owner_rows(), 8);
        let plateau = cens[..3 * cw as usize]
            .iter()
            .filter_map(|e| e.map(|(t, _, _)| t))
            .min()
            .expect("the cells before the head are lit");
        let head = &cens[3 * cw as usize..4 * cw as usize];
        let head_bot = head
            .iter()
            .filter_map(|e| e.map(|(_, b, _)| b))
            .max()
            .unwrap_or(0);
        let head_peak = head
            .iter()
            .filter_map(|e| e.map(|(_, _, p)| p))
            .max()
            .unwrap_or(0);
        let mut dark = Vec::new();
        for (k, e) in cens.iter().enumerate().skip(3 * cw as usize) {
            let x = x0 + k as i32;
            let Some((top, bot, peak)) = *e else {
                dark.push(x);
                continue;
            };
            assert!(
                top <= plateau + 1,
                "{hand}: column {x} tops out at {top}, under the plateau's {plateau}"
            );
            if x >= head_x0 + cw {
                assert!(
                    bot >= head_bot - 1,
                    "{hand}: the foot under the block at column {x} ends at {bot}, the head's rail at {head_bot}"
                );
                assert!(
                    peak + 2 >= head_peak,
                    "{hand}: the band under the block at column {x} peaks at {peak}, the head at {head_peak}"
                );
            }
        }
        assert!(
            dark.is_empty(),
            "{hand}: ground-dark columns between the head cell and the block's right edge: {} of them, from x {}",
            dark.len(),
            dark[0]
        );
        let hot = column_census(&sink.out, head_x0..head_x0 + cw, owner_rows(), 1);
        let unlit = hot.iter().filter(|e| e.is_none()).count();
        assert_eq!(
            unlit, 0,
            "{hand}: {unlit} of the head cell's columns carry no hot edge"
        );
    }
}

type FrameCensus = (u64, u16, Vec<Option<(i32, i32, u8)>>);

fn owner_frames(
    rib: &mut Ribbon,
    t0: Instant,
    c: &Config,
    disp: f32,
    ms: std::ops::RangeInclusive<u64>,
    xs: std::ops::Range<i32>,
    mut step: impl FnMut(&mut Ribbon, u64) -> u16,
) -> Vec<FrameCensus> {
    let mut sink = Sink::default();
    let mut frames = Vec::new();
    let (mut ms, to_ms) = (*ms.start(), *ms.end());
    while ms <= to_ms {
        let caret = step(rib, ms);
        owner_frame(rib, &mut sink, c, at(t0, ms), caret, disp);
        frames.push((
            ms,
            caret,
            column_census(&sink.under, xs.clone(), owner_rows(), 8),
        ));
        ms += 16;
    }
    frames
}

const FADE_STEP_MAX: i32 = 36;

#[test]
fn the_block_stands_on_whole_light_after_a_jump_and_the_keys_that_follow_it() {
    let g = geom_owner();
    let cw = g.cw as i32;
    let row = OWNER_ROW;
    for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
        let c = cfg_owner();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row,
            col0: 2,
            n: 10,
            period_ms: 83,
            disp,
        };
        type_keys(&mut rib, t0, keys, &c);
        let mut ms = 9 * 83 + 120;
        for (from, to) in [((row, 12u16), (row, 0u16)), ((row, 0), (row, 12))] {
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, to, disp, g);
            rib.on_event(&nav(from, to), now, &cx);
            rib.plan(&cx);
            ms += 120;
        }
        let mut caret = 12u16;
        for _ in 0..3 {
            ms += 83;
            caret += 1;
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (row, caret), disp, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
        }
        ms += 300;
        let mut sink = Sink::default();
        owner_frame(&mut rib, &mut sink, &c, at(t0, ms), caret, disp);
        let x0 = i32::from(caret - 5) * cw;
        let x1 = i32::from(caret + 1) * cw;
        let cens = column_census(&sink.under, x0..x1, owner_rows(), 8);
        let plateau = cens[..4 * cw as usize]
            .iter()
            .filter_map(|e| e.map(|(_, _, p)| p))
            .max()
            .expect("the band before the head is lit");
        let show = |lo: usize| {
            (0..3)
                .map(|j| cens[lo + j * (cw as usize / 3)].map_or(0, |(_, _, p)| i32::from(p)))
                .collect::<Vec<_>>()
        };
        for (k, e) in cens.iter().enumerate().skip(4 * cw as usize) {
            let x = x0 + k as i32;
            let peak = e.map_or(0, |(_, _, p)| p);
            assert!(
                i32::from(plateau) - i32::from(peak) <= 2,
                "{hand}: device column {x} reads {peak} against the band's {plateau} — the head cell is {:?} and the block's own {:?}",
                show(4 * cw as usize),
                show(5 * cw as usize)
            );
        }
    }
}

fn assert_block_on_band(frames: &[FrameCensus], x0: i32, cw: i32, hold_x1: i32, leg: &str) {
    for (i, (ms, caret, cens)) in frames.iter().enumerate() {
        let cx0 = i32::from(*caret) * cw;
        let dark: Vec<i32> = (cx0..cx0 + cw)
            .filter(|x| cens[(x - x0) as usize].is_none())
            .collect();
        assert!(
            dark.is_empty(),
            "{leg}, +{ms} ms, caret {caret}: {} ground-dark columns under the block, from x {}",
            dark.len(),
            dark[0]
        );
        if i == 0 {
            continue;
        }
        let prev = &frames[i - 1].2;
        let level = |e: &Option<(i32, i32, u8)>| e.map_or(0, |(_, _, p)| i32::from(p));
        for (k, (now, was)) in cens.iter().zip(prev.iter()).enumerate() {
            let x = x0 + k as i32;
            let (now, was) = (level(now), level(was));
            assert!(
                now - was <= 2,
                "{leg}, +{ms} ms: column {x} rose {was} -> {now} in one frame"
            );
            if (cx0..hold_x1).contains(&x) {
                assert!(
                    was - now <= FADE_STEP_MAX,
                    "{leg}, +{ms} ms: column {x} fell {was} -> {now} in one frame, faster than the fade"
                );
            }
        }
    }
}

#[test]
fn the_block_stays_on_the_band_while_the_cell_under_it_leaves_and_when_it_is_gone() {
    let g = geom_owner();
    let cw = g.cw as i32;
    let row = OWNER_ROW;
    let fade_ms = (RETRACT_FADE_S * 1000.0) as u64;
    let melt_ms = (RETIRE_MELT_S * 1000.0) as u64;
    for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
        for (period, erases, leg) in [
            (0u64, 1u16, "one Backspace"),
            (33, 4, "Backspace held at 30/s"),
        ] {
            let leg = format!("{hand} hand, {leg}");
            let c = cfg_owner();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 8,
                period_ms: 83,
                disp,
            };
            type_keys(&mut rib, t0, keys, &c);
            let ms0 = 7 * 83 + 300;
            let last_erase = ms0 + u64::from(erases - 1) * period;
            let mut done = 0u16;
            let mut caret = 10u16;
            let x0 = i32::from(10 - erases) * cw;
            let frames = owner_frames(
                &mut rib,
                t0,
                &c,
                disp,
                ms0 - 16..=last_erase + fade_ms + 200,
                x0..11 * cw,
                |rib, ms| {
                    while done < erases && ms >= ms0 + u64::from(done) * period {
                        caret -= 1;
                        let now = at(t0, ms);
                        let cx = ctx_in(now, &c, (row, caret), disp, g);
                        rib.on_event(&Event::Erase, now, &cx);
                        done += 1;
                    }
                    caret
                },
            );
            assert_block_on_band(&frames, x0, cw, 10 * cw, &leg);
            assert!(
                !rib.cells().iter().any(|l| l.row == row && l.col >= caret),
                "{leg}: the erased cells retired inside the window"
            );
            assert_eq!(caret, 10 - erases, "{leg}: every erase landed");
        }
        {
            let leg = format!("{hand} hand, own cells retired under the caret");
            let c = cfg_owner();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 8,
                period_ms: 83,
                disp,
            };
            type_keys(&mut rib, t0, keys, &c);
            let mut ms = 7 * 83 + 300;
            let cx = ctx_in(at(t0, ms), &c, (row, 7), disp, g);
            rib.on_event(&nav((row, 10), (row, 7)), at(t0, ms), &cx);
            rib.plan(&cx);
            ms += 300;
            let cx = ctx_in(at(t0, ms), &c, (row, 8), disp, g);
            rib.on_event(&typed(), at(t0, ms), &cx);
            rib.plan(&cx);
            let ms0 = ms + 60;
            let ids: Vec<(u16, u16, Instant)> = rib
                .cells()
                .iter()
                .filter(|l| l.row == row && l.col >= 8)
                .map(|l| (l.row, l.col, l.born))
                .collect();
            let mut retired = false;
            let x0 = 7 * cw;
            let frames = owner_frames(
                &mut rib,
                t0,
                &c,
                disp,
                ms0 - 16..=ms0 + melt_ms + 200,
                x0..11 * cw,
                |rib, ms| {
                    if !retired && ms >= ms0 {
                        assert_eq!(
                            rib.retire_cells(&ids, at(t0, ms)),
                            2,
                            "{leg}: the shifted cells retire"
                        );
                        retired = true;
                    }
                    8
                },
            );
            assert_block_on_band(&frames, x0, cw, 9 * cw, &leg);
            assert!(
                !rib.cells().iter().any(|l| l.row == row && l.col >= 8),
                "{leg}: the retired cells left the pool inside the window"
            );
        }
        {
            let leg = format!("{hand} hand, a wake's cell retired under the caret");
            let c = cfg_owner();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 3,
                period_ms: 83,
                disp,
            };
            type_keys(&mut rib, t0, keys, &c);
            let mut ms = 2 * 83 + 200;
            let cx = ctx_in(at(t0, ms), &c, (row, 9), disp, g);
            rib.on_event(&nav((row, 5), (row, 9)), at(t0, ms), &cx);
            rib.plan(&cx);
            ms += 120;
            let cx = ctx_in(at(t0, ms), &c, (row, 5), disp, g);
            rib.on_event(&nav((row, 9), (row, 5)), at(t0, ms), &cx);
            rib.plan(&cx);
            let typed_cohort = rib
                .cells()
                .iter()
                .find(|l| l.row == row && l.col == 4)
                .expect("the word's last cell")
                .cohort;
            let ids: Vec<(u16, u16, Instant)> = rib
                .cells()
                .iter()
                .filter(|l| l.row == row && l.col >= 5)
                .map(|l| (l.row, l.col, l.born))
                .collect();
            assert!(
                rib.cells()
                    .iter()
                    .any(|l| l.row == row && l.col == 5 && l.cohort != typed_cohort),
                "{leg}: the wake is a cohort of its own"
            );
            let ms0 = ms + 80;
            let mut retired = false;
            let x0 = 4 * cw;
            let frames = owner_frames(
                &mut rib,
                t0,
                &c,
                disp,
                ms0 - 16..=ms0 + melt_ms + 200,
                x0..7 * cw,
                |rib, ms| {
                    if !retired && ms >= ms0 {
                        assert!(
                            rib.retire_cells(&ids, at(t0, ms)) >= 5,
                            "{leg}: the wake retires"
                        );
                        retired = true;
                    }
                    5
                },
            );
            assert_block_on_band(&frames, x0, cw, 6 * cw, &leg);
            assert!(
                !rib.cells().iter().any(|l| l.row == row && l.col >= 5),
                "{leg}: the retired wake left the pool inside the window"
            );
        }
    }
}

fn column_peaks(under: &[GlowQuad], xs: std::ops::Range<i32>) -> Vec<i32> {
    column_census(under, xs, owner_rows(), 1)
        .iter()
        .map(|e| e.map_or(0, |(_, _, p)| i32::from(p)))
        .collect()
}

fn sliver_columns(peaks: &[i32], cw: usize, skip: usize, tol: i32) -> Vec<usize> {
    (skip..peaks.len())
        .filter(|&i| {
            let left = peaks[i.saturating_sub(cw)..i]
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
            let right = peaks[i + 1..(i + 1 + cw).min(peaks.len())]
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
            peaks[i] + tol < left && peaks[i] + tol < right
        })
        .collect()
}

#[test]
fn a_boundary_two_live_runs_share_carries_the_older_side_s_light() {
    let c = cfg_owner();
    let g = geom_owner();
    let cw = g.cw as i32;
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    type_keys(
        &mut rib,
        t0,
        Keys {
            g,
            row: OWNER_ROW,
            col0: 2,
            n: 6,
            period_ms: 83,
            disp: 0.7,
        },
        &c,
    );
    let hop = at(t0, 5 * 83 + 120);
    let cx = ctx_in(hop, &c, (OWNER_ROW, 11), 0.7, g);
    rib.on_event(&nav((OWNER_ROW, 8), (OWNER_ROW, 11)), hop, &cx);
    rib.plan(&cx);
    let mut sink = Sink::default();
    owner_frame(&mut rib, &mut sink, &c, at(hop, 300), 11, 0.7);
    assert_eq!(
        rib.cohorts().iter().filter(|k| k.row == OWNER_ROW).count(),
        2,
        "the word and its wake are two cohorts: {:?}",
        rib.cohorts()
    );
    assert_eq!(rib.runs.len(), 2, "…and two runs abutting at column 8");
    let x0 = 2 * cw;
    let x1 = 12 * cw;
    let peaks = column_peaks(&sink.under, x0..x1);
    let dark: Vec<i32> = peaks
        .iter()
        .enumerate()
        .filter(|(_, p)| **p == 0)
        .map(|(i, _)| x0 + i as i32)
        .collect();
    assert!(dark.is_empty(), "dark columns inside the band: {dark:?}");
    let slivers: Vec<i32> = sliver_columns(&peaks, cw as usize, cw as usize, 2)
        .into_iter()
        .map(|i| x0 + i as i32)
        .collect();
    assert!(
        slivers.is_empty(),
        "columns darker than both sides by more than a dither step: {slivers:?} (peaks {:?})",
        &peaks[5 * cw as usize..7 * cw as usize]
    );
    let tops = column_census(&sink.under, x0 + cw..x1, owner_rows(), 8);
    for w in tops.windows(2) {
        let (Some(a), Some(b)) = (w[0], w[1]) else {
            continue;
        };
        assert!(
            (a.0 - b.0).abs() <= 1,
            "the top edge steps {} rows between adjacent columns ({} → {})",
            (a.0 - b.0).abs(),
            a.0,
            b.0
        );
    }
}

#[test]
fn the_feather_grows_in_as_a_drained_neighbour_goes_never_in_one_frame() {
    let c = cfg_owner();
    let g = geom_owner();
    let cw = g.cw as i32;
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let first = "and I also ";
    let mut ms = 0u64;
    let mut col = 2u16;
    for _ in first.chars() {
        let now = at(t0, ms);
        let cx = ctx_in(now, &c, (OWNER_ROW, col + 1), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        col += 1;
        ms += 83;
    }
    let park = at(t0, ms + 20);
    let cx = ctx_in(park, &c, (OWNER_ROW, col), 0.7, g);
    rib.on_event(&typed_move((OWNER_ROW, col), (OWNER_ROW, 2)), park, &cx);
    rib.on_event(&typed_move((OWNER_ROW, 2), (OWNER_ROW, col)), park, &cx);
    rib.plan(&cx);
    let mut keys: Vec<(u64, u16)> = Vec::new();
    let mut k = ms + 70;
    for _ in "seee an ugly gap".chars() {
        keys.push((k, col + 1));
        col += 1;
        k += 83;
    }
    let last_key = keys.last().map_or(k, |e| e.0);
    let mut sink = Sink::default();
    let x0 = 2 * cw;
    let x1 = 30 * cw;
    let mut prev: Option<Vec<i32>> = None;
    let mut next_key = 0usize;
    let mut caret = col - 16;
    let mut worst_drop = (0i32, 0i32, 0u64);
    let mut slivers_seen = Vec::new();
    let mut t = ms + 20;
    let mut last_key_at = 0u64;
    let until = last_key + (PHRASE_REST_MIN_S * 1000.0) as u64;
    while t <= until {
        while next_key < keys.len() && keys[next_key].0 <= t {
            let (kt, kc) = keys[next_key];
            let now = at(t0, kt);
            let cx = ctx_in(now, &c, (OWNER_ROW, kc), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            caret = kc;
            last_key_at = kt;
            next_key += 1;
        }
        owner_frame(&mut rib, &mut sink, &c, at(t0, t), caret, 0.7);
        let peaks = column_peaks(&sink.under, x0..x1);
        if let Some(p) = &prev {
            for (i, (&a, &b)) in p.iter().zip(&peaks).enumerate() {
                if a - b > worst_drop.0 {
                    worst_drop = (a - b, x0 + i as i32, t);
                }
            }
        }
        let settled = t.saturating_sub(last_key_at) >= 60;
        if settled {
            let live_col = rib
                .cells()
                .iter()
                .filter(|l| !l.leaving())
                .map(|l| l.col)
                .min()
                .unwrap_or(2);
            let first = (usize::from(live_col) - 2) * cw as usize;
            let live = &peaks[first.min(peaks.len())..];
            for i in sliver_columns(live, cw as usize, cw as usize, 2) {
                slivers_seen.push((t, x0 + (first + i) as i32, live[i]));
            }
        }
        prev = Some(peaks);
        t += 16;
    }
    assert!(
        rib.cells().iter().all(|l| l.col >= 13),
        "by the end of the grace the drained cells have retired"
    );
    assert!(
        worst_drop.0 <= 36,
        "a column's light fell {} levels in one 16 ms frame (x {}, +{} ms): the feather popped in",
        worst_drop.0,
        worst_drop.1,
        worst_drop.2
    );
    assert!(
        slivers_seen.is_empty(),
        "columns darker than both sides inside the live band: {:?}",
        &slivers_seen[..slivers_seen.len().min(8)]
    );
}

#[test]
fn the_attach_and_the_seam_law_hold_at_one_slab_per_cell() {
    let c = cfg_owner();
    let g = geom_owner();
    let cw = g.cw as i32;
    let t0 = Instant::now();
    let mut rib = Ribbon::new();
    let mut ms = 0u64;
    for row in 50..53u16 {
        type_keys(
            &mut rib,
            at(t0, ms),
            Keys {
                g,
                row,
                col0: 1,
                n: 100,
                period_ms: 1,
                disp: 0.7,
            },
            &c,
        );
        ms += 100;
    }
    let mut col = 2u16;
    for _ in "and I also seee an ugly gap".chars() {
        let now = at(t0, ms);
        let cx = ctx_in(now, &c, (OWNER_ROW, col + 1), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        col += 1;
        ms += 20;
    }
    let mut sink = Sink::default();
    owner_frame(&mut rib, &mut sink, &c, at(t0, ms - 20 + 100), col, 0.7);
    assert_eq!(rib.slabs_per_cell(), 1, "327 retina cells plan at one slab");
    assert!(
        sink.under.len() + 2 > RIBBON_QUAD_BUDGET - 400,
        "the frame is at the budget's edge: {} quads",
        sink.under.len()
    );
    let head_x0 = i32::from(col - 1) * cw;
    let block_x1 = i32::from(col + 1) * cw;
    let cens = column_census(&sink.under, head_x0 - 3 * cw..block_x1, owner_rows(), 8);
    let plateau = cens[..3 * cw as usize]
        .iter()
        .filter_map(|e| e.map(|(t, _, _)| t))
        .min()
        .expect("lit");
    for (k, e) in cens.iter().enumerate().skip(3 * cw as usize) {
        let x = head_x0 - 3 * cw + k as i32;
        let (top, _, _) = e.unwrap_or_else(|| panic!("column {x} is ground-dark at one slab"));
        assert!(
            top <= plateau + 1,
            "column {x} tops out at {top} under the plateau {plateau}"
        );
    }
    let x0 = 2 * cw;
    let peaks = column_peaks(&sink.under, x0..block_x1);
    let slivers = sliver_columns(&peaks, cw as usize, cw as usize, 2);
    assert!(slivers.is_empty(), "slivers at one slab: {slivers:?}");
    let tops = column_census(&sink.under, x0 + cw..block_x1, owner_rows(), 8);
    let worst = tops
        .windows(2)
        .filter_map(|w| Some((w[0]?.0 - w[1]?.0).abs()))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= 3,
        "the top edge steps {worst} rows at one slab per cell"
    );
}

fn capped_wide_run(head_col: u16, head: usize, stream_dir: i8) -> (Vec<GlowQuad>, u16) {
    let c = cfg_owner();
    let mut g = geom_owner();
    g.cols = 180;
    g.win_w = 5400;
    let now = Instant::now();
    let cx = ctx_in(now, &c, (OWNER_ROW, head_col), 0.7, g);
    let mut rib = Ribbon::new();
    rib.plan(&cx); // Sync the real theme ink table.
    rib.slabs = 1;
    rib.plan = (0..=179)
        .map(|col| Segment {
            x: col as f32 * g.cw as f32,
            spine: (f32::from(OWNER_ROW) + 1.0) * g.ch as f32,
            up: g.ch as f32,
            dn: 0.30 * g.ch as f32,
            t: 0.4,
            cov: 220,
            glass_up: 0.0,
            glass_jump: 0.0,
        })
        .collect();
    rib.runs.push(Run {
        row: OWNER_ROW,
        col0: 0,
        col1: 178,
        lo: 0,
        hi: rib.plan.len(),
        head_col,
        head,
        wet: true,
        at_caret: true,
        born: now,
        stream_dir,
        hold: 0.0,
    });

    let mut sink = Sink::default();
    rib.emit(&cx, &mut sink.frame());
    assert!(
        sink.under.len() + 2 * g.ch >= RIBBON_QUAD_BUDGET,
        "precondition: the real quad ceiling was reached ({})",
        sink.under.len()
    );
    (sink.under, g.cw as u16)
}

/// A frame out of budget ends the run it cuts in a feather. The beam's own stop, handed the
/// whole run, printed a step of the whole band's light in one column.
#[test]
fn a_cut_near_the_head_leaves_the_head_whole() {
    let vertex = |x: f32| RibbonVertex {
        x,
        spine: 0.0,
        up: 1.0,
        dn: 1.0,
        core_up: 0.0,
        core_dn: 0.0,
        color: 0,
        cov: 200.0,
        lift: 100.0,
        lift_span: 1.0,
    };
    let near = [100.0, 110.0, 120.0];
    let mut verts = near.map(vertex);
    Ribbon::thin_toward_cut(&mut verts, 60.0);
    assert_eq!(
        verts[0].cov, 200.0,
        "the head is shared with the other half"
    );
    assert_eq!(
        verts[0].lift, 100.0,
        "the head is shared with the other half"
    );
    assert!(verts[0].cov > verts[1].cov && verts[1].cov > verts[2].cov);
    assert!((verts[2].cov - 200.0 * RUN_TAIL_EASE).abs() < 1e-3);
    let far = [100.0, 150.0, 200.0];
    let mut verts = far.map(vertex);
    Ribbon::thin_toward_cut(&mut verts, 60.0);
    assert_eq!(verts[0].cov, 200.0, "beyond the feather nothing thins");
    assert!(verts[1].cov < 200.0 && verts[1].cov > verts[2].cov);
}

#[test]
fn a_capped_runs_cut_end_is_a_feather_not_a_cliff() {
    for (what, head_col, head, stream_dir) in [
        ("a leftward wake", 0u16, 0usize, 1i8),
        ("a midline edit", 30, 31, -1),
    ] {
        let (quads, cw) = capped_wide_run(head_col, head, stream_dir);
        let cw = usize::from(cw);
        let mut light = vec![0u8; 180 * cw];
        for quad in &quads {
            for x in usize::from(quad.x)..usize::from(quad.x) + usize::from(quad.w) {
                if let Some(at) = light.get_mut(x) {
                    *at = (*at).max(quad.alpha.max(quad.alpha2));
                }
            }
        }
        let last = light
            .iter()
            .rposition(|&level| level > 0)
            .expect("the frame laid light");
        assert!(last < 178 * cw, "{what}: the distant tail paid the budget");
        let tail = &light[last + 1 - (BUDGET_FEATHER_CELLS + 2) * cw..=last];
        assert!(
            tail.windows(2).all(|pair| pair[0].abs_diff(pair[1]) <= 60),
            "{what}: a step of over 60 levels in {tail:?}"
        );
        assert!(
            light[last] <= 60,
            "{what}: the cut lands on {}",
            light[last]
        );
        assert!(
            tail[..cw].iter().all(|&level| level >= 150),
            "{what}: a feather, whole light inward of it: {tail:?}"
        );
    }
}

#[test]
fn a_capped_rail_does_not_cut_the_body_of_an_adjacent_run() {
    let c = cfg_owner();
    let g = geom_owner();
    let now = Instant::now();
    let cx = ctx_in(now, &c, (OWNER_ROW, 130), 0.7, g);
    let mut rib = Ribbon::new();
    rib.plan(&cx); // Sync the real theme ink table.
    rib.slabs = 1;
    rib.plan = (0..=130)
        .map(|col| Segment {
            x: col as f32 * g.cw as f32,
            spine: (f32::from(OWNER_ROW) + 1.0) * g.ch as f32,
            up: g.ch as f32,
            dn: 0.30 * g.ch as f32,
            t: 0.4,
            cov: 220,
            glass_up: 0.0,
            glass_jump: 0.0,
        })
        .collect();
    rib.runs.extend([
        Run {
            row: OWNER_ROW,
            col0: 20,
            col1: 129,
            lo: 20,
            hi: 131,
            head_col: 129,
            head: 130,
            wet: true,
            at_caret: true,
            born: now,
            stream_dir: -1,
            hold: 0.0,
        },
        Run {
            row: OWNER_ROW,
            col0: 0,
            col1: 19,
            lo: 0,
            hi: 21,
            head_col: 19,
            head: 20,
            wet: true,
            at_caret: false,
            born: now,
            stream_dir: -1,
            hold: 0.0,
        },
    ]);
    let mut sink = Sink::default();
    rib.emit(&cx, &mut sink.frame());
    assert!(
        sink.under.len() + 2 * g.ch >= RIBBON_QUAD_BUDGET,
        "precondition: body plus rail must fill the budget ({})",
        sink.under.len()
    );
    assert!(sink.under.len() <= RIBBON_QUAD_BUDGET);
    assert!(cell_lit(&sink.under, g, OWNER_ROW, 129));
    assert!(
        (0..130).all(|col| cell_lit(&sink.under, g, OWNER_ROW, col)),
        "the vivid rail consumed body quads and opened a dark gap inside the typed row"
    );

    let mut uncapped = rib.clone();
    uncapped.plan.truncate(61);
    uncapped.runs[0].col1 = 59;
    uncapped.runs[0].hi = 61;
    uncapped.runs[0].head_col = 59;
    uncapped.runs[0].head = 60;
    let short_cx = ctx_in(now, &c, (OWNER_ROW, 60), 0.7, g);
    let mut joint = Sink::default();
    uncapped.emit(&short_cx, &mut joint.frame());
    let mut current = uncapped.clone();
    current.runs.truncate(1);
    let mut current_sink = Sink::default();
    current.emit(&short_cx, &mut current_sink.frame());
    let mut older = uncapped;
    older.runs.remove(0);
    let mut older_sink = Sink::default();
    older.emit(&short_cx, &mut older_sink.frame());
    let seam_x = (20 * g.cw) as u16;
    assert!(current_sink.under.iter().all(|q| q.x >= seam_x));
    assert!(older_sink.under.iter().all(|q| q.x + q.w <= seam_x));
    let mut per_run = current_sink.under;
    per_run.extend(older_sink.under);
    assert_eq!(joint.under, per_run, "uncapped emission changed");
}

#[test]
fn a_capped_run_keeps_the_light_at_its_head_and_sheds_the_distant_tail() {
    for (what, head_col, head, stream_dir) in [
        ("a leftward wake", 0u16, 0usize, 1i8),
        ("a midline edit", 30, 31, -1),
    ] {
        let (quads, cw) = capped_wide_run(head_col, head, stream_dir);
        assert!(
            quads
                .iter()
                .any(|q| q.x >= head_col * cw && q.x < (head_col + 1) * cw),
            "{what}: the cell at the caret went dark under the cap"
        );
        assert!(
            quads.iter().any(|q| q.x < cw),
            "{what}: the run's left side must connect to the head"
        );
        assert!(
            !quads.iter().any(|q| q.x >= 178 * cw),
            "{what}: the distant right tail should pay the budget, not the head"
        );
    }
}

#[test]
fn attach_cell_follows_scroll_band_and_hand_lift() {
    let now = Instant::now();
    let c = cfg(true, true);
    let g = geom();
    let mut rib = Ribbon::new();

    let cx5 = ctx_in(now, &c, (5, 5), 0.7, g);
    rib.on_event(&typed(), now, &cx5);
    rib.plan(&cx5);
    assert_eq!(rib.attach, Some((5, 5)));

    rib.translate_scroll(2);
    assert_eq!(rib.attach, Some((3, 5)));
    assert!(rib.attach_floor.is_none());

    rib.translate_band(2, 6, 1);
    assert_eq!(rib.attach, Some((4, 5)));

    rib.lift_hand(4, 3);
    assert_eq!(rib.attach, Some((3, 5)));
    rib.carry = Some((
        SoftWrap {
            origin: (3, 79),
            landing: (4, 2),
            word_col0: 74,
        },
        now,
    ));
    rib.lift_hand(4, 5);
    assert_eq!(
        rib.carry.map(|(sw, _)| (sw.origin, sw.landing)),
        Some(((3, 79), (5, 2)))
    );

    let t_next = now + Duration::from_millis(20);
    let cx3 = ctx_in(t_next, &c, (3, 6), 0.7, g);
    rib.on_event(&typed(), t_next, &cx3);
    let laid = rib
        .cells()
        .iter()
        .find(|cell| cell.row == 3 && cell.col == 5)
        .expect("cell laid at lifted caret");
    assert!(
        laid.attack_at < laid.born,
        "typed cell at translated attach must inherit spent attack"
    );
}

#[test]
fn chained_word_kills_retract_right_to_left_without_a_gap() {
    let t0 = Instant::now();
    let c = cfg(true, true);
    let g = geom();
    let mut rib = Ribbon::new();
    let row = 5u16;
    let text = "so that when you fix ";
    let mut col = 2u16;
    let mut t = t0;
    for ch in text.chars() {
        t += Duration::from_millis(40);
        col += 1;
        let cx = ctx_in(t, &c, (row, col), 0.7, g);
        let ev = if ch == ' ' { typed_space() } else { typed() };
        rib.on_event(&ev, t, &cx);
        rib.plan(&cx);
    }
    let kills = [4u16, 4, 5, 5, 3];
    for cells in kills {
        for step_ms in (16..=160).step_by(16) {
            let frame_t = t + Duration::from_millis(step_ms);
            let cx = ctx_in(frame_t, &c, (row, col), 0.7, g);
            rib.plan(&cx);
            let mut lit_cols: Vec<u16> = rib
                .lit_segments()
                .map(|s| (s.x / g.cw as f32).floor() as u16)
                .collect();
            lit_cols.sort_unstable();
            lit_cols.dedup();
            if let (Some(&lo), Some(&hi)) = (lit_cols.first(), lit_cols.last()) {
                for check_col in lo..=hi {
                    assert!(
                        lit_cols.binary_search(&check_col).is_ok(),
                        "chained word-kill left an unlit gap at col {check_col} inside {lo}..={hi} ({lit_cols:?})"
                    );
                }
            }
        }
        t += Duration::from_millis(165);
        let from = (row, col);
        col -= cells;
        let to = (row, col);
        let cx_before = ctx_in(t, &c, from, 0.7, g);
        rib.on_event(
            &Event::Kill {
                cells,
                scope: super::super::KillScope::Word,
            },
            t,
            &cx_before,
        );
        let cx_after = ctx_in(t, &c, to, 0.7, g);
        rib.on_event(
            &Event::Move {
                from,
                to,
                licence: Licence::Typed,
                dir: Dir::of(-i32::from(cells), 0),
            },
            t,
            &cx_after,
        );
    }
}
