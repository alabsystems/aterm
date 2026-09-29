// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
//
// Web CPU↔GPU parity: the two web renderers must agree on pixels.
//
// `aterm-wasm` rasterizes the grid on the CPU (`aterm_render::Renderer`) and
// `aterm-gpu-web` rasterizes it on the GPU (`aterm_gpu::GpuRenderer`, WebGL2 in
// the browser). Both feed the SAME engine grid and BOTH hand JS an RGBA8 buffer
// for `putImageData` — so a divergence between them is a visible rendering bug in
// one of the two web paths.
//
// The browser GPU path needs a real `<canvas>`/WebGL surface (wasm-only), so it
// can't run here. Instead this test reproduces aterm-gpu-web's EXACT native
// construction — `GpuContext::new()` + a `Renderer::from_bytes` face handed to
// `GpuRenderer::from_parts` (lib.rs `init()`), the same shape as the wasm path —
// and compares it to aterm-wasm's CPU renderer (`Renderer::from_bytes`) at the
// same px/theme. The comparison is on the EXPANDED RGBA8 each web crate emits
// (`render()` packs `0x00RRGGBB` -> `r,g,b,0xff`), not the internal packed frame,
// so it gates the actual bytes that reach the canvas.
//
// Both sides inject the SAME bundled font, exactly as the web crates inject a
// host-fetched font, so the test is deterministic and never skips for a missing
// system face. Gated: no GPU -> the test no-ops (like aterm-gpu's own parity
// tests).

use aterm_core::render::{HostRowPixels, RenderInput};
use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuContext, GpuRenderer, WindowGpu};
use aterm_messages::drive::{self, Commit, Lay, LookIn, View};
use aterm_messages::ink::{AnsiHues, BandInks, BarBase, ThemeInks};
use aterm_messages::paint::Geometry;
use aterm_messages::wire::{self, NoticeRequest, WireGate};
use aterm_messages::{Duration, Instant, Links, MessageCenter, MessageLog, WallStamp};
use aterm_render::band::{self as band_frame, BandFrame};
use aterm_render::{ChromeBleed, Frame, Renderer, Theme};

/// The bundled deterministic monospace face, injected into BOTH renderers the way
/// the web crates inject a font fetched in JS — so parity can't drift on a missing
/// or mismatched system font.
const FONT: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

const PX: f32 = 18.0;

/// A web theme in the `0x00RRGGBB` shape aterm-wasm/aterm-gpu-web seed from JS.
fn web_theme() -> Theme {
    Theme {
        fg: 0x00E0_E0E0,
        bg: 0x001E_1E2E,
        cursor: 0x00FF_FFFF,
        selection: 0x0030_4060,
    }
}

fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}

/// Expand a packed-`0x00RRGGBB` frame to RGBA8 EXACTLY as the web crates' `render`
/// does (`r,g,b,0xff`) — this is the buffer handed to canvas `putImageData`.
fn to_rgba8(f: &Frame) -> Vec<u8> {
    let mut v = Vec::with_capacity(f.pixels.len() * 4);
    for &p in &f.pixels {
        v.push((p >> 16) as u8);
        v.push((p >> 8) as u8);
        v.push(p as u8);
        v.push(0xff);
    }
    v
}

fn max_byte_delta(a: &[u8], b: &[u8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| (x as i32 - y as i32).abs())
        .max()
        .unwrap_or(0)
}

/// ASCII + colour + reverse-video grid (no ligature sequences, no font fallback —
/// both faces use default shaping over the bundled face, so any delta is pure
/// CPU/GPU rounding).
fn demo_term() -> (Terminal, usize, usize) {
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(
        b"\x1b[31mRR\x1b[0m\r\n\
          \x1b[44m  \x1b[0m\r\n\
          \x1b[7mXX\x1b[0m\r\n\
          ab\r\n",
    );
    (term, rows, cols)
}

#[test]
fn web_cpu_gpu_rgba8_parity() {
    let theme = web_theme();

    // GPU side, built EXACTLY as aterm-gpu-web::init does: a GpuContext + a
    // from_bytes CPU face handed to from_parts. Gate: no GPU -> skip cleanly.
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: no GPU: {e}");
            return;
        }
    };
    let gpu_face = Renderer::from_bytes(FONT, PX, theme).expect("bundled font loads (gpu face)");
    let mut gpu = match GpuRenderer::from_parts(ctx, gpu_face, None, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: gpu renderer unavailable: {e}");
            return;
        }
    };

    // CPU side, built as aterm-wasm does: from_bytes at the same px/theme.
    let mut cpu = Renderer::from_bytes(FONT, PX, theme).expect("bundled font loads (cpu face)");

    let mut win = WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    // `None`: no settings-card tray in this CPU/GPU parity test (P3 `render_input`
    // gained the `tray: Option<TrayQuad>` arg; the parity check renders bare frames).
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "web CPU/GPU frame dimensions differ"
    );

    let cpu_rgba = to_rgba8(&cpu_frame);
    let gpu_rgba = to_rgba8(&gpu_frame);
    assert_eq!(
        cpu_rgba.len(),
        gpu_rgba.len(),
        "web CPU/GPU RGBA8 buffer lengths differ"
    );
    assert_eq!(
        cpu_rgba.len(),
        cpu_frame.width * cpu_frame.height * 4,
        "RGBA8 buffer is not width*height*4"
    );

    // The whole frame matches within the established CPU/GPU antialiasing tolerance
    // (only round-vs-floor coverage rounding differs); alpha is 0xff on both.
    let delta = max_byte_delta(&cpu_rgba, &gpu_rgba);
    eprintln!("web CPU/GPU RGBA8 max byte delta = {delta}");
    assert!(
        delta <= 8,
        "web CPU/GPU RGBA8 diverge: max byte delta {delta} > 8"
    );

    // Non-empty sanity: the red 'R' glyph actually rendered on the GPU path, so we
    // didn't just compare two background frames.
    let red_seen = gpu_frame
        .pixels
        .iter()
        .any(|&p| rr(p) > 140 && gg(p) < 90 && bb(p) < 90);
    assert!(
        red_seen,
        "expected red glyph pixels on the GPU frame (non-empty render)"
    );
}

/// The chrome pad the band frame is drawn with (px per edge), so the band's
/// bleed has gutters to reach.
const PAD: usize = 6;

/// A packed `0x00RRGGBB` colour as sRGB bytes.
fn bytes(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// The web modules' band frame (`messages_api`'s sequencing, through the
/// engine's driver): a held warning and a 50 % bar, settled and committed
/// after the bar's grace, laid out with links withheld, painted in the still
/// look over the theme's inks, and composed above `input` with every window
/// stream translated. Returns the frame's bleed.
fn compose_web_band(input: &mut RenderInput, cols: usize, cell: (usize, usize)) -> ChromeBleed {
    let theme = web_theme();
    let t0 = Instant::now();
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let mut gate = WireGate::default();
    let wall = WallStamp {
        unix_ms: 1_790_000_000_000,
    };
    for line in [
        "post ci sev=warn hold=60 Disk nearly full -- 2 GB left",
        "progress p pct=50 Downloading assets",
    ] {
        let req = NoticeRequest::parse(line).expect("a notice line");
        let applied = wire::apply(&mut center, &mut gate, req, wall, t0);
        assert!(applied.reply.starts_with("OK"), "{}", applied.reply);
    }
    let now = t0 + Duration::from_millis(2500);
    let rows = u16::try_from(input.rows).expect("rows");
    let _ = drive::settle_and_commit(
        &mut center,
        Commit {
            now,
            frozen: false,
            afford: drive::afford([rows], 0),
            reserved: 0,
        },
    );
    assert_eq!(center.committed_rows(), 2, "two band rows");
    let palette = aterm_types::ColorPalette::new();
    let hue = |i: u8| {
        let c = palette.get(i);
        [c.r, c.g, c.b]
    };
    let inks = BandInks::derive(
        ThemeInks {
            bg: bytes(theme.bg),
            fg: bytes(theme.fg),
            cursor: bytes(theme.cursor),
        },
        Some(AnsiHues {
            blue: hue(4),
            cyan: hue(6),
        }),
        BarBase::Blend,
    );
    let lay = Lay {
        cols,
        links: Links::Withheld,
        home: || None,
    };
    let look = drive::look(LookIn {
        motion_allowed: false,
        on_screen: true,
        frozen: false,
        forced: false,
    });
    let (cell_w, cell_h) = cell;
    let geom = Geometry {
        win_w: cols * cell_w + 2 * PAD,
        cells_x: PAD,
        cell_w,
    };
    let mut view = View::default();
    let _ = view.prepare(&center, &lay, now, look);
    let resolved = view
        .paint(&center, cols, None, geom, false, &|| inks)
        .expect("the band paints");
    let (painted, edges, rasters) = band_frame::rows(resolved);
    let mut pool = Vec::new();
    let n = band_frame::compose_band(
        input,
        None,
        &painted,
        &rasters,
        2,
        &inks,
        false,
        BandFrame {
            cols,
            cell_w,
            cell_h,
            pad: PAD,
            lo: 0,
            frame_w: cols * cell_w + 2 * PAD,
            grid_top: PAD,
        },
        &mut pool,
        HostRowPixels::Translate,
    );
    assert_eq!(n, 2);
    assert!(
        !input.chrome_rasters.is_empty(),
        "the bar's meter is drawn at pixel resolution"
    );
    input.grid_top_row += n;
    input.grid_bot_row += n;
    band_frame::band_bleed(&inks, 0, n, band_frame::band_row_edges(0, n, &edges))
}

/// THE BAND ON BOTH WEB RENDERERS (design ruling 340): the message band's
/// frame — two rows reserved above the grid, a warning and a metered bar
/// with its pixel-resolution raster, the gutters bled to the frame's edges —
/// drawn by aterm-gpu-web's GPU path (a native `GpuContext`, the bundled
/// face) and by aterm-wasm's CPU rasterizer lands within the same tolerance
/// as the grid alone. The frame is two rows taller than the band-less one,
/// and the band's rows are not the theme's background (the negative
/// control: a frame whose band failed to draw would compare equal to the
/// grid alone there). No GPU: the test says SKIP and passes.
#[test]
fn web_band_frame_cpu_gpu_rgba8_parity() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: no GPU: {e}");
            return;
        }
    };
    let mut gpu_face =
        Renderer::from_bytes(FONT, PX, web_theme()).expect("bundled font loads (gpu face)");
    gpu_face.set_pad(PAD);
    let mut gpu = match GpuRenderer::from_parts(ctx, gpu_face, None, web_theme()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: gpu renderer unavailable: {e}");
            return;
        }
    };
    let mut cpu =
        Renderer::from_bytes(FONT, PX, web_theme()).expect("bundled font loads (cpu face)");
    cpu.set_pad(PAD);

    let mut win = WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let plain = cpu.render_input(&term.cell_frame(rows, cols));
    let mut input = term.cell_frame(rows, cols);
    let bleed = compose_web_band(&mut input, cols, cpu.cell_size());
    cpu.set_chrome_bleed(Some(bleed));
    gpu.set_chrome_bleed(Some(bleed));
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    let (_, ch) = cpu.cell_size();
    assert_eq!(
        (cpu_frame.width, cpu_frame.height),
        (plain.width, plain.height + 2 * ch),
        "the frame grows by the band's two rows"
    );
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "web CPU/GPU band frame dimensions differ"
    );
    let cpu_rgba = to_rgba8(&cpu_frame);
    let gpu_rgba = to_rgba8(&gpu_frame);
    let delta = max_byte_delta(&cpu_rgba, &gpu_rgba);
    eprintln!(
        "web CPU/GPU band frame RGBA8 max byte delta = {delta} ({}x{})",
        cpu_frame.width, cpu_frame.height
    );
    assert!(
        delta <= 8,
        "web CPU/GPU band frame diverges: max byte delta {delta} > 8"
    );
    // The band rows carry ink: pixels there that are not the theme's bg.
    let bg = web_theme().bg & 0x00FF_FFFF;
    let band = &gpu_frame.pixels[PAD * gpu_frame.width..(PAD + 2 * ch) * gpu_frame.width];
    let inked = band.iter().filter(|&&p| p & 0x00FF_FFFF != bg).count();
    assert!(
        inked > band.len() / 20,
        "the GPU drew the band: {inked} inked pixels"
    );
}
