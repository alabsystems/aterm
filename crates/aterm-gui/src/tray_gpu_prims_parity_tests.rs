// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU tray prims against the CPU raster on this Mac's GPU
//! (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §3.2, S3): random geometry of every kind at
//! fractional places over clear, dark, light and translucent grounds at every alpha step, and the
//! owner scenes' live elements. The parity: no byte more than 2 levels off, and none off inside an
//! opaque prim's interior. A host without a GPU, or one whose GPU has no framebuffer fetch (an
//! Intel Mac, which keeps the CPU raster), ends COULD NOT RUN.

use aterm_gpu::tray_prims::{TrayPrim, TrayPrimLaw};

use super::{DeviceRect, gpu_prims, tray_prim_law};
use crate::tray_raster::{RasterRect, rasterize_tray_pixels, rasterize_tray_region};
use crate::widget::DrawPrim;

/// A renderer whose GPU draws tray prims, or COULD NOT RUN with why it cannot.
fn gpu() -> aterm_gpu::GpuRenderer {
    let theme = aterm_render::Theme::default();
    let cpu = aterm_render::Renderer::from_bytes(aterm_render::embedded_font(), 18.0, theme)
        .expect("the bundled font");
    let mut gpu = match aterm_gpu::GpuContext::new()
        .and_then(|ctx| aterm_gpu::GpuRenderer::from_parts(ctx, cpu, None, theme))
    {
        Ok(gpu) => gpu,
        Err(error) => {
            aterm_spec::host::could_not_run(format_args!("no GPU renderer on this host: {error}"))
        }
    };
    if let Err(why) = gpu.tray_prims_over(&[], tray_prim_law(), &[], (1, 1), (0, 0), &[0; 4]) {
        aterm_spec::host::could_not_run(format_args!("this GPU draws no tray prims: {why}"));
    }
    gpu
}

/// SplitMix64: the same geometry on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 56) as u8
    }
}

/// The kinds the random sweep draws, each a [`DrawPrim`] the GPU draws.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Panel,
    PanelSubpixel,
    Ring,
    Arc,
    Line,
    Dot,
    Capsule,
    Stroke,
    StrokeSubpixel,
    Dashed,
}

const KINDS: [Kind; 10] = [
    Kind::Panel,
    Kind::PanelSubpixel,
    Kind::Ring,
    Kind::Arc,
    Kind::Line,
    Kind::Dot,
    Kind::Capsule,
    Kind::Stroke,
    Kind::StrokeSubpixel,
    Kind::Dashed,
];

/// The grounds a card is drawn over: clear (a deck card's own), dark, light, and a translucent one.
const GROUNDS: [(&str, [u8; 4]); 4] = [
    ("clear", [0, 0, 0, 0]),
    ("dark", [24, 24, 28, 255]),
    ("light", [242, 240, 236, 255]),
    ("translucent", [40, 90, 160, 128]),
];

/// A tile's side (pt), and how many tiles a sweep's canvas holds a side.
const TILE: f32 = 40.0;
const TILES: u32 = 32;
/// Sweeps per kind and ground, two prims per tile: 5 × 32² × 2 = 10,240 geometries each.
const SWEEPS: u32 = 5;
/// The scales the sweeps cycle through.
const SCALES: [f32; 3] = [1.0, 1.5, 2.0];

/// One random prim of `kind` inside the tile at `(tx, ty)` (pt) in `color`.
fn random_prim(rng: &mut Rng, kind: Kind, (tx, ty): (f32, f32), color: [u8; 4]) -> Vec<DrawPrim> {
    let at = |rng: &mut Rng, lo: f32, hi: f32| rng.range(lo, hi);
    let prim = match kind {
        Kind::Panel | Kind::PanelSubpixel => DrawPrim::Panel {
            x: tx + at(rng, 1.0, 20.0),
            y: ty + at(rng, 1.0, 20.0),
            w: at(rng, 0.5, 18.0),
            h: at(rng, 0.5, 18.0),
            radius: at(rng, 0.0, 10.0),
            fill: color,
        },
        Kind::Ring => {
            let r_outer = at(rng, 1.0, 16.0);
            let frac = match rng.next() % 4 {
                0 => 1.0,
                1 => 0.0,
                _ => at(rng, 0.0, 1.0),
            };
            // An arc turned in place (`start`), forward and back, as well as from 12 o'clock.
            let start = match rng.next() % 3 {
                0 => 0.0,
                _ => at(rng, -1.0, 1.0),
            };
            DrawPrim::Ring {
                cx: tx + 20.0 + at(rng, -3.0, 3.0),
                cy: ty + 20.0 + at(rng, -3.0, 3.0),
                r_outer,
                thickness: at(rng, 0.3, r_outer),
                track: [color[0] / 2, color[1], color[2] / 3, color[3] / 2],
                start,
                frac,
                fill: color,
            }
        }
        Kind::Arc => {
            let from = at(rng, -1.0, 1.0);
            DrawPrim::Arc {
                cx: tx + 20.0 + at(rng, -3.0, 3.0),
                cy: ty + 20.0 + at(rng, -3.0, 3.0),
                r: at(rng, 0.5, 14.0),
                width: at(rng, 0.2, 6.0),
                from,
                to: from + at(rng, -0.2, 1.3),
                color,
            }
        }
        Kind::Line => {
            let (x1, y1) = (tx + at(rng, 4.0, 36.0), ty + at(rng, 4.0, 36.0));
            let (x2, y2) = if rng.next().is_multiple_of(16) {
                (x1, y1)
            } else {
                (tx + at(rng, 4.0, 36.0), ty + at(rng, 4.0, 36.0))
            };
            DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width: at(rng, 0.0, 6.0),
                color,
            }
        }
        Kind::Dot => DrawPrim::Dot {
            cx: tx + 20.0 + at(rng, -4.0, 4.0),
            cy: ty + 20.0 + at(rng, -4.0, 4.0),
            r: at(rng, 0.1, 15.0),
            color,
        },
        Kind::Capsule => DrawPrim::Capsule {
            x: tx + at(rng, 1.0, 8.0),
            y: ty + at(rng, 1.0, 26.0),
            w: at(rng, 1.0, 30.0),
            h: at(rng, 1.0, 12.0),
            frac: if rng.next().is_multiple_of(5) {
                0.0
            } else {
                at(rng, 0.0, 1.0)
            },
            fill: color,
            track: [color[2], color[0], color[1], color[3] / 2],
        },
        Kind::Stroke | Kind::StrokeSubpixel => DrawPrim::Stroke {
            x: tx + at(rng, 3.0, 10.0),
            y: ty + at(rng, 3.0, 10.0),
            w: at(rng, 0.5, 26.0),
            h: at(rng, 0.5, 26.0),
            radius: at(rng, 0.0, 10.0),
            width: at(rng, 0.2, 5.0),
            color,
        },
        Kind::Dashed => DrawPrim::Dashed {
            x: tx + at(rng, 1.0, 8.0),
            y: ty + at(rng, 1.0, 8.0),
            w: at(rng, 0.0, 30.0),
            h: at(rng, 0.0, 30.0),
            width: at(rng, 0.5, 4.0),
            on: at(rng, 0.5, 7.0),
            off: at(rng, -1.0, 5.0),
            color,
        },
    };
    if matches!(kind, Kind::PanelSubpixel | Kind::StrokeSubpixel) {
        vec![DrawPrim::SubpixelPush, prim, DrawPrim::SubpixelPop]
    } else {
        vec![prim]
    }
}

/// One sweep's list: every tile clipped to itself, two prims of `kind` in it, the first at the
/// tile's step of the alpha cycle and the second opaque on even tiles; with the opaque colours
/// the sweep draws.
fn sweep(rng: &mut Rng, kind: Kind, sweep: u32) -> (Vec<DrawPrim>, Vec<[u8; 4]>) {
    let mut prims = Vec::new();
    let mut opaque = Vec::new();
    for t in 0..TILES * TILES {
        let (tx, ty) = ((t % TILES) as f32 * TILE, (t / TILES) as f32 * TILE);
        prims.push(DrawPrim::ClipPush {
            x: tx,
            y: ty,
            w: TILE,
            h: TILE,
        });
        for j in 0..2u32 {
            let step = (sweep * TILES * TILES + t) * 2 + j;
            let alpha = if j == 1 && t % 2 == 0 {
                255
            } else {
                (step % 255 + 1) as u8
            };
            let color = [rng.byte(), rng.byte(), rng.byte(), alpha];
            if alpha == 255 {
                opaque.push(color);
            }
            prims.extend(random_prim(rng, kind, (tx, ty), color));
        }
        prims.push(DrawPrim::ClipPop);
    }
    (prims, opaque)
}

/// How far one card's GPU bytes are from its CPU bytes.
#[derive(Default, Debug)]
struct Delta {
    bytes: u64,
    off_by: [u64; 4],
    max: u8,
    interior_misses: u64,
}

impl Delta {
    /// Count `gpu` against `cpu` (`w` px wide): every byte's distance, and every pixel off whose
    /// CPU byte is an opaque colour in `opaque` with its eight neighbours the same (an interior).
    /// A row whose bytes all match passes whole (a slice compare is one `memcmp`, even
    /// unoptimized): its bytes are off by none and it holds no miss. A test build's walk of
    /// every byte and pixel was most of the sweep's time.
    fn add(&mut self, cpu: &[u8], gpu: &[u8], w: usize, opaque: &[[u8; 4]]) {
        assert_eq!(cpu.len(), gpu.len(), "the GPU card is the CPU card's size");
        let row = w.max(1) * 4;
        for (c, g) in cpu.chunks(row).zip(gpu.chunks(row)) {
            self.bytes += c.len() as u64;
            if c == g {
                self.off_by[0] += c.len() as u64;
                continue;
            }
            for (c, g) in c.iter().zip(g) {
                let d = c.abs_diff(*g);
                self.off_by[usize::from(d.min(3))] += 1;
                self.max = self.max.max(d);
            }
        }
        let h = cpu.len() / row;
        let px = |buf: &[u8], x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        for y in 1..h.saturating_sub(1) {
            if cpu[y * row..(y + 1) * row] == gpu[y * row..(y + 1) * row] {
                continue;
            }
            for x in 1..w.saturating_sub(1) {
                let c = px(cpu, x, y);
                if c[3] != 255 || px(gpu, x, y) == c || !opaque.contains(&c) {
                    continue;
                }
                let interior =
                    (y - 1..=y + 1).all(|ny| (x - 1..=x + 1).all(|nx| px(cpu, nx, ny) == c));
                if interior {
                    self.interior_misses += 1;
                }
            }
        }
    }

    fn line(&self) -> String {
        format!(
            "max delta {}, bytes off by 1: {}, by 2: {}, by more: {}, of {}; interior misses {}",
            self.max,
            self.off_by[1],
            self.off_by[2],
            self.off_by[3],
            self.bytes,
            self.interior_misses
        )
    }
}

/// The GPU's bytes for `prims` at `scale` over a `w × h` card of `ground` at device origin 0.
fn gpu_card(
    gpu: &mut aterm_gpu::GpuRenderer,
    law: &TrayPrimLaw,
    prims: &[DrawPrim],
    scale: f32,
    (w, h): (u32, u32),
    ground: [u8; 4],
) -> Vec<u8> {
    let bounds: DeviceRect = (0, 0, w as i32, h as i32);
    let mapped: Vec<TrayPrim> = gpu_prims(prims, scale, bounds).expect("vector prims map");
    let under = ground.repeat((w * h) as usize);
    gpu.tray_prims_over(&mapped, law, &[], (w, h), (0, 0), &under)
        .expect("the GPU draws the card")
}

/// Every kind, 10,240 random geometries over each ground at every alpha step, at fractional
/// places and at 1×, 1.5× and 2×: within 2 levels of the CPU raster everywhere and exact inside
/// opaque interiors.
#[test]
fn random_prims_of_every_kind_land_the_cpu_bytes() {
    let mut gpu = gpu();
    let law = tray_prim_law();
    let mut worst = 0u8;
    let mut misses = 0u64;
    let mut table = String::new();
    for kind in KINDS {
        for (name, ground) in GROUNDS {
            let mut rng = Rng(0x7A11_C0DE ^ (kind as u64) << 32 ^ u64::from(ground[0]));
            let mut delta = Delta::default();
            for n in 0..SWEEPS {
                let scale = SCALES[n as usize % SCALES.len()];
                let (prims, opaque) = sweep(&mut rng, kind, n);
                let side = ((TILES as f32 * TILE) * scale).round() as u32;
                let cpu = rasterize_tray_pixels(&prims, side, side, scale, ground);
                let got = gpu_card(&mut gpu, law, &prims, scale, (side, side), ground);
                delta.add(&cpu, &got, side as usize, &opaque);
            }
            worst = worst.max(delta.max);
            misses += delta.interior_misses;
            table.push_str(&format!("{kind:?} over {name}: {}\n", delta.line()));
        }
    }
    eprint!(
        "tray prims parity, {} geometries per kind and ground:\n{table}",
        SWEEPS * TILES * TILES * 2
    );
    assert!(
        worst <= 2 && misses == 0,
        "the GPU tray prims are within 2 levels of the CPU raster and exact inside opaque \
         interiors:\n{table}"
    );
}

/// NEGATIVE CONTROL: the same sweep through a gamma-space law (the bytes blended as they are
/// stored, as a fixed-function blend on a Unorm card does) misses by more than the parity allows,
/// so the parity above is the law's, not a slack tolerance.
#[test]
fn a_gamma_space_law_fails_the_parity() {
    let mut gpu = gpu();
    let exact = tray_prim_law();
    let gamma = TrayPrimLaw {
        to_linear: exact.unit,
        unit: exact.unit,
        thresholds: std::array::from_fn(|n| {
            if n < 255 {
                (n as f32 + 0.5) / 255.0
            } else {
                f32::INFINITY
            }
        }),
    };
    let mut rng = Rng(0x6A33A);
    let (prims, opaque) = sweep(&mut rng, Kind::Dot, 0);
    let side = (TILES as f32 * TILE) as u32;
    let mut lines = String::new();
    for (name, ground) in GROUNDS {
        let cpu = rasterize_tray_pixels(&prims, side, side, 1.0, ground);
        let mut controls = [Delta::default(), Delta::default()];
        for (law, delta) in [exact, &gamma].into_iter().zip(&mut controls) {
            let got = gpu_card(&mut gpu, law, &prims, 1.0, (side, side), ground);
            delta.add(&cpu, &got, side as usize, &opaque);
        }
        lines.push_str(&format!(
            "over {name}: exact law {}; gamma law {}\n",
            controls[0].line(),
            controls[1].line()
        ));
        assert!(
            controls[0].max <= 2,
            "CONTROL: the exact law holds:\n{lines}"
        );
        if name != "clear" {
            assert!(
                controls[1].max > 2,
                "a gamma-space blend misses by more than 2:\n{lines}"
            );
        }
    }
    eprint!("tray prims negative control:\n{lines}");
}

/// The owner scenes' live elements (`stats_deck_owner_scenes.rs`), Full and Folded, dark and
/// light, at the owner's 1039 pt window at 2×: each live rect of the deck's vector prims (its text
/// and images left out of both sides) on Metal against `rasterize_tray_region`.
#[test]
fn the_owner_scenes_live_elements_land_the_cpu_bytes() {
    use crate::stats_deck::paint_tests::light_inks;
    use crate::stats_deck::paint_tests::owner_scenes::{
        OWNER_W, Owner, owner_geom_at, owner_inks, owner_input, owner_scene,
    };
    use crate::stats_plan::DeckForm;

    let mut gpu = gpu();
    let law = tray_prim_law();
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut table = String::new();
    let mut worst = 0u8;
    let mut misses = 0u64;
    let mut rects = 0usize;
    for o in Owner::ALL {
        let snap = owner_scene(o);
        for form in [DeckForm::Full, DeckForm::Folded] {
            for (theme, inks) in [("dark", owner_inks()), ("light", light_inks())] {
                let input = crate::stats_deck::DeckInput {
                    inks,
                    form,
                    ..owner_input(&snap)
                };
                let g = owner_geom_at(form, OWNER_W, 2.0);
                let (prims, _, live) = crate::stats_deck::deck_paint_live(&g, &input);
                let vector: Vec<DrawPrim> = prims
                    .into_iter()
                    .filter(|p| {
                        !matches!(
                            p,
                            DrawPrim::Text { .. }
                                | DrawPrim::Image { .. }
                                | DrawPrim::BandIcon { .. }
                                | DrawPrim::HsvDisk { .. }
                                | DrawPrim::TerminalSpecimen { .. }
                        )
                    })
                    .collect();
                let opaque: Vec<[u8; 4]> = vector.iter().filter_map(opaque_ink).collect();
                assert!(
                    !live.rects.is_empty(),
                    "{} {form:?} {theme} lays live rects",
                    o.name()
                );
                let mut delta = Delta::default();
                for (layer, (x, y, w, h)) in &live.rects {
                    let x0 = x.floor().max(0.0) as u32;
                    let y0 = y.floor().max(0.0) as u32;
                    let x1 = ((x + w).ceil() as u32).min(g.w as u32);
                    let y1 = ((y + h).ceil() as u32).min(g.h as u32);
                    if x1 <= x0 || y1 <= y0 {
                        continue;
                    }
                    let region = RasterRect {
                        x: x0,
                        y: y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    };
                    let cpu = rasterize_tray_region(&vector, 1.0, [0, 0, 0, 0], region);
                    let bounds = (x0 as i32, y0 as i32, x1 as i32, y1 as i32);
                    let mapped = gpu_prims(&vector, 1.0, bounds)
                        .unwrap_or_else(|| panic!("{layer:?}: the deck's vector prims map"));
                    let under = vec![0u8; (region.width * region.height * 4) as usize];
                    let got = gpu
                        .tray_prims_over(
                            &mapped,
                            law,
                            &[],
                            (region.width, region.height),
                            (x0 as i32, y0 as i32),
                            &under,
                        )
                        .expect("the GPU draws the live rect");
                    delta.add(&cpu, &got, region.width as usize, &opaque);
                    rects += 1;
                }
                worst = worst.max(delta.max);
                misses += delta.interior_misses;
                table.push_str(&format!(
                    "{} {form:?} {theme}: {} live rects, {}\n",
                    o.name(),
                    live.rects.len(),
                    delta.line()
                ));
            }
        }
    }
    eprint!("tray prims parity, owner scenes ({rects} live rects):\n{table}");
    assert!(
        worst <= 2 && misses == 0,
        "the owner scenes' live elements are within 2 levels of the CPU raster:\n{table}"
    );
}

/// A prim's opaque ink, for the interior check.
fn opaque_ink(p: &DrawPrim) -> Option<[u8; 4]> {
    let c = match p {
        DrawPrim::Panel { fill, .. } | DrawPrim::Capsule { fill, .. } => *fill,
        DrawPrim::Ring { fill, .. } => *fill,
        DrawPrim::Dot { color, .. }
        | DrawPrim::Stroke { color, .. }
        | DrawPrim::Line { color, .. }
        | DrawPrim::Arc { color, .. }
        | DrawPrim::Dashed { color, .. } => *color,
        _ => return None,
    };
    (c[3] == 255).then_some(c)
}
