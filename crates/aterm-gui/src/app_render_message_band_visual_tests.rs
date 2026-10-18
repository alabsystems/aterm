// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use aterm_messages::{Look, Pace};

use crate::{App, WindowId, term_lock};

/// The capture font's size in px, used by the CPU renderer and by a
/// [`glass_window`]'s cell layout so the band's pixel mapping agrees.
const CAPTURE_PX: f32 = 20.0;

/// A capture window that is not a whole number of cells wide: `pad` px of
/// gutter on every side and `extra` px (under one cell) of remainder band
/// split around it. Band cells sit at `pad + extra/2` in a window
/// `cols·cw + 2·pad + extra` px wide, so 0 % is an empty track, 50 % the
/// window's middle and 100 % edge to edge.
fn glass_window(app: &mut App, session: u64, cols: u16, pad: usize, extra: usize) -> WindowId {
    let wid = app.insert_logical_window(crate::stub_session(session), 20, cols);
    let ws = app.windows.get_mut(&wid).unwrap();
    ws.metrics.font_px = CAPTURE_PX;
    ws.metrics.pad = pad;
    ws.metrics.pad_top = pad;
    let (cw, ch) = app.win_cell_size(wid);
    // Less than one cell, or the presenters would count another column.
    let extra = extra.min(cw.saturating_sub(1));
    let width = usize::from(cols) * cw + 2 * pad + extra;
    let height = 21 * ch + 2 * pad;
    app.windows.get_mut(&wid).unwrap().win_px = Some(winit::dpi::PhysicalSize::new(
        u32::try_from(width).unwrap_or(u32::MAX),
        u32::try_from(height).unwrap_or(u32::MAX),
    ));
    wid
}

// -----------------------------------------------------------------------
// The web band's golden: this host's band, frame by frame, for the web
// module to be held to.
// -----------------------------------------------------------------------

/// The golden the web module's `the_web_band_is_the_native_golden` reads.
const WEB_GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-wasm/tests/fixtures/band_native.tsv"
);

/// The face both hosts draw the golden with: the bundled JetBrains Mono the
/// web page ships, at the capture size.
const WEB_FONT: &[u8] =
    include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");

/// The frame's side and top padding, px (the web page's `set_chrome(6, 0)`).
const WEB_PAD: usize = 6;

/// One golden scene: its name, its `notice` lines (ms, line), and its shots.
type WebScene = (&'static str, &'static [(u64, &'static str)], &'static [u64]);

/// Each golden scene: its name, the `notice` lines at their offsets (ms from
/// the center's birth), and the instants it is shot at. Lines go through
/// the engine's wire path on both hosts, and every shot is also a settle
/// tick (a progress row reaches the glass at its grace's end only if a tick
/// runs then, so finishes are shot at 2.5 s first).
const WEB_SCENES: &[WebScene] = &[
    ("record", &[(0, "post ci Build finished")], &[100]),
    (
        "warn",
        &[(0, "post ci sev=warn Disk nearly full -- 2 GB left on /")],
        &[100],
    ),
    (
        "error",
        &[(0, "post ci sev=error Deploy failed -- exit 1")],
        &[100],
    ),
    (
        "bar0",
        &[(0, "progress p pct=0 Downloading assets")],
        &[2500],
    ),
    (
        "bar25",
        &[(0, "progress p pct=25 Downloading assets")],
        &[2500],
    ),
    (
        "bar50",
        &[(0, "progress p pct=50 Downloading assets")],
        &[2500],
    ),
    (
        "bar999",
        &[(0, "progress p pct=99.9 Downloading assets")],
        &[2500],
    ),
    (
        "bar100",
        &[(0, "progress p pct=100 Downloading assets")],
        &[2500],
    ),
    (
        "amount",
        &[
            (0, "progress f done=3/10 Copying files"),
            (1000, "progress f done=5/10 Copying files"),
            (2000, "progress f done=7/10 Copying files"),
        ],
        &[2600, 3000],
    ),
    (
        "busy",
        &[(0, "progress b busy Indexing the tree")],
        &[2600, 3400, 4700],
    ),
    (
        "stall",
        &[(0, "progress s pct=40 Uploading logs")],
        &[2500, 13_000],
    ),
    (
        "complete",
        &[(0, "progress p pct=50 Building"), (3000, "done p ok Built")],
        &[2500, 3100, 3400],
    ),
    (
        "fault",
        &[
            (0, "progress p pct=50 Uploading"),
            (3000, "done p warn Upload failed"),
        ],
        &[2500, 3100, 3300],
    ),
    (
        "vanish",
        &[(0, "progress p pct=50 Syncing"), (3000, "done p withdraw")],
        &[2500, 3050, 3200],
    ),
    (
        "stack",
        &[
            (0, "post ci sev=warn Disk nearly full"),
            (10, "post ci sev=error Deploy failed"),
            (20, "progress p pct=60 Downloading assets"),
        ],
        &[2600],
    ),
    (
        "shrink",
        &[
            (0, "post ci sev=warn Disk nearly full"),
            (10, "post ci sev=error Deploy failed"),
            (1000, "dismiss 2"),
        ],
        &[900, 1200, 2700],
    ),
];

/// FNV-1a-64's offset basis.
const WEB_FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a-64 — the golden's digest (the web reader computes the same).
fn web_fnv(bytes: &[u8]) -> u64 {
    web_fnv_more(WEB_FNV_BASIS, bytes)
}

/// [`web_fnv`]'s state `h` carried on over `bytes`.
fn web_fnv_more(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// [`web_fnv`] of CPU-frame pixels (`0xAARRGGBB`, `AA` the inverted alpha)
/// read as the RGBA bytes the golden digests: `R, G, B, 0xff - AA` each,
/// hashed straight from the frame with no RGBA copy.
fn web_fnv_pixels(pixels: &[u32]) -> u64 {
    pixels.iter().fold(WEB_FNV_BASIS, |h, &p| {
        web_fnv_more(
            h,
            &[
                (p >> 16) as u8,
                (p >> 8) as u8,
                p as u8,
                0xff - (p >> 24) as u8,
            ],
        )
    })
}

/// One golden frame line: the band's rows (cells, raster, pixels each), the
/// bleed, and the pixels above the grid, from the composed frame `input`,
/// its CPU-frame `pixels` (`w` px wide), `grid_top` and `cell_h`.
#[allow(
    clippy::too_many_arguments,
    reason = "a golden line is a pure function of exactly these inputs"
)]
fn web_golden_line(
    key: &str,
    n: usize,
    input: &aterm_core::render::RenderInput,
    bleed: Option<&aterm_render::ChromeBleed>,
    pixels: &[u32],
    w: usize,
    grid_top: usize,
    cell_h: usize,
) -> String {
    use std::fmt::Write as _;
    let px = |y0: usize, y1: usize| web_fnv_pixels(&pixels[y0 * w..y1 * w]);
    let mut line = format!(
        "frame\t{key}\tn={n}\tw={w}\ttop={:016x}\tbleed={:016x}",
        px(0, grid_top),
        web_fnv(format!("{bleed:?}").as_bytes())
    );
    let mut words = Vec::new();
    for r in 0..n {
        let cells = web_fnv(format!("{:?}", input.cells[r]).as_bytes());
        let rasters: Vec<_> = input
            .chrome_rasters
            .iter()
            .filter(|m| usize::from(m.row) == r)
            .collect();
        let raster = web_fnv(format!("{rasters:?}").as_bytes());
        let y0 = grid_top + r * cell_h;
        let _ = write!(
            line,
            "\tr{r}={cells:016x}:{raster:016x}:{:016x}",
            px(y0, y0 + cell_h)
        );
        words.push(
            input.cells[r]
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_string(),
        );
    }
    let _ = write!(line, "\t|{}|", words.join("|"));
    line
}

/// The renderer both hosts draw the golden with: the bundled face on the
/// portable rasterizer.
fn web_portable(theme: aterm_render::Theme) -> aterm_render::Renderer {
    let mut r =
        aterm_render::Renderer::from_bytes(WEB_FONT, CAPTURE_PX, theme).expect("bundled font");
    // The web module has one rasterizer, the portable one; this host's
    // default (CoreText on macOS, hinted on Linux and Windows) is
    // pinned to it, and neither side discovers system faces.
    r.debug_force_fontdue();
    r.set_runtime_font_discovery(false);
    r
}

/// This host's golden, as text: the themes, the scenes and every frame.
///
/// Each theme's frames are drawn on a thread and App of their own
/// ([`web_golden_theme_frames`]) and joined in the golden's theme order, so
/// the text is a single sequential pass's, line for line.
fn web_golden_text() -> String {
    use std::fmt::Write as _;
    let scheme = |name: &str| aterm_types::scheme::builtin(name).expect("builtin scheme");
    let default_palette = aterm_types::ColorPalette::new();
    let rgb = |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
    let mut themes: Vec<(&str, aterm_render::Theme, aterm_types::ColorPalette)> = vec![(
        "dark",
        aterm_render::Theme::default(),
        default_palette.clone(),
    )];
    for name in ["GitHub Light", "Catppuccin Latte", "Solarized Light"] {
        let s = scheme(name);
        themes.push((name, aterm_render::Theme::from(&s), s.to_color_palette()));
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# band_native.tsv — THIS HOST's message band, frame by frame (design ruling 333 of\n\
         # docs/DESIGN-unified-messages-2026-09-21.md). Written by aterm-gui's\n\
         # `message_band_visual_tests::the_web_band_golden_is_this_hosts_band` (which also\n\
         # asserts it; rewrite with ATERM_BAND_GOLDEN=write) and read by aterm-wasm's\n\
         # `messages_api::tests::the_web_band_is_the_native_golden`, which replays the scenes\n\
         # through the web module and must land on every digest. Digests are FNV-1a-64 of each\n\
         # band row's RenderCells (Debug), its ChromeRasters (Debug) and its RGBA pixel rows,\n\
         # the ChromeBleed (Debug) and the pixels above the grid. Both hosts draw with the\n\
         # bundled JetBrains Mono at 20 px on the portable rasterizer, pad 6, links withheld.\n\
         # The colour arithmetic runs on THIS host's libm (macOS libSystem here): on another\n\
         # libm both tests would flag the same last-place difference together (ruling 326 (g)),\n\
         # which is a platform fact, not a web defect."
    );
    let _ = writeln!(
        out,
        "font\tJetBrainsMono-Regular.ttf\t{CAPTURE_PX}\tpad\t{WEB_PAD}"
    );
    for (name, t, pal) in &themes {
        let _ = writeln!(
            out,
            "theme\t{name}\t{:06x}\t{:06x}\t{:06x}\t{:06x}\t{:06x}\t{:06x}\t{:06x}\t{:06x}",
            t.fg,
            t.bg,
            t.cursor,
            t.selection,
            rgb(pal.get(4)),
            rgb(pal.get(6)),
            rgb(pal.get(1)),
            rgb(pal.get(3))
        );
    }
    for (name, lines, shots) in WEB_SCENES {
        for (at, line) in *lines {
            let _ = writeln!(out, "line\t{name}\t{at}\t{line}");
        }
        for at in *shots {
            let _ = writeln!(out, "shot\t{name}\t{at}");
        }
    }
    let blocks: Vec<String> = std::thread::scope(|scope| {
        let jobs: Vec<_> = themes
            .iter()
            .map(|(tname, theme, palette)| {
                scope.spawn(move || web_golden_theme_frames(tname, *theme, palette))
            })
            .collect();
        jobs.into_iter()
            .map(|job| {
                job.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    for block in blocks {
        out.push_str(&block);
    }
    out
}

/// One theme's frames of the golden, in the golden's order — every width,
/// look, scene and shot — drawn on a headless App of their own that lays
/// the band as the web does (links withheld).
#[allow(clippy::too_many_lines, reason = "one pass over the golden's matrix")]
fn web_golden_theme_frames(
    tname: &str,
    theme: aterm_render::Theme,
    palette: &aterm_types::ColorPalette,
) -> String {
    use aterm_messages::wire::{self, NoticeRequest, Paint, WireGate};
    use aterm_messages::{MessageCenter, MessageLog, WallStamp};
    use std::fmt::Write as _;
    // The web band's ground is the theme blend; Linux's own band sits on its
    // CSD headerbar grey (`chrome_band::PLATFORM_BAR_BASE`), so the golden is
    // drawn on the web's base here, as on every other platform.
    crate::chrome_band::lay_bands_on_the_theme_blend_for_test();
    let mut app = App::headless_for_test();
    app.backend = crate::BackendSlot::Ready(crate::Backend::Cpu(web_portable(
        aterm_render::Theme::default(),
    )));
    app.band_lay = |cols| aterm_messages::drive::Lay::web(cols, false);
    let mut out = String::new();
    let stamp = WallStamp {
        unix_ms: 1_790_000_000_000,
    };
    let looks = [
        ("moving", Look::MOVING),
        (
            "still",
            Look {
                pace: Pace::Still,
                graded: true,
            },
        ),
    ];
    let windows: Vec<(u16, WindowId)> = [60u16, 80, 120]
        .into_iter()
        .zip(1u64..)
        .map(|(cols, session)| (cols, glass_window(&mut app, session, cols, WEB_PAD, 0)))
        .collect();
    let mut tc = app
        .session_factory
        .terminal_config
        .clone()
        .unwrap_or_default();
    tc.custom_palette = (tname != "dark").then(|| palette.clone());
    app.session_factory.terminal_config = Some(tc);
    for &(cols, wid) in &windows {
        app.frontmost_window = Some(wid);
        let mut cpu = web_portable(theme);
        cpu.set_pad(WEB_PAD);
        assert_eq!(
            app.win_cell_size(wid),
            cpu.cell_size(),
            "both hosts lay the band out in the same cells"
        );
        let terminal = app.front_terminal(wid).expect("front").term.clone();
        {
            let mut term = term_lock(&terminal);
            let rgb = |c: u32| aterm_core::terminal::Rgb {
                r: (c >> 16) as u8,
                g: (c >> 8) as u8,
                b: c as u8,
            };
            term.set_default_foreground(rgb(theme.fg));
            term.set_default_background(rgb(theme.bg));
            term.set_default_cursor_color(Some(rgb(theme.cursor)));
            term.set_default_selection_background(Some(rgb(theme.selection)));
        }
        for (lname, look) in looks {
            for (scene, lines, shots) in WEB_SCENES {
                let t0 = Instant::now();
                app.messages = MessageCenter::new(MessageLog::default(), t0);
                app.wire_gate = WireGate::default();
                let _ = app.settle_messages(t0);
                let mut events: Vec<(u64, Option<&str>)> = lines
                    .iter()
                    .map(|&(at, l)| (at, Some(l)))
                    .chain(shots.iter().map(|&at| (at, None)))
                    .collect();
                events.sort_by_key(|&(at, l)| (at, l.is_none()));
                for (at, line) in events {
                    let now = t0 + Duration::from_millis(at);
                    if let Some(line) = line {
                        // `App::take_notice` at an injected instant.
                        let req = NoticeRequest::parse(line).expect("a golden line parses");
                        let applied =
                            wire::apply(&mut app.messages, &mut app.wire_gate, req, stamp, now);
                        if applied.paint == Paint::Now {
                            let _ = app.settle_messages(now);
                        }
                        continue;
                    }
                    let (rows, cols_n) = {
                        let ws = &app.windows[&wid];
                        (ws.rows as usize, ws.cols as usize)
                    };
                    {
                        let ws = app.windows.get_mut(&wid).expect("window");
                        term_lock(&terminal).cell_frame_into(&mut ws.input_scratch, rows, cols_n);
                        ws.band.invalidate();
                    }
                    let _ = app.settle_messages(now);
                    app.prepare_band_motion_with(wid, now, look);
                    app.splice_message_band(wid, theme);
                    let n = usize::from(app.message_band_rows);
                    let inks = app.chrome_palette_on(theme).colors();
                    let ws = &app.windows[&wid];
                    let bleed = (n > 0).then(|| {
                        aterm_render::band::band_bleed(
                            &inks,
                            0,
                            n,
                            aterm_render::band::band_row_edges(0, n, &ws.cached_band_edges),
                        )
                    });
                    cpu.set_chrome_bleed(bleed);
                    let frame = cpu.render_input(&ws.input_scratch);
                    let key = format!("{scene}\t{tname}\t{cols}\t{lname}\t{at}");
                    let _ = writeln!(
                        out,
                        "{}",
                        web_golden_line(
                            &key,
                            n,
                            &ws.input_scratch,
                            bleed.as_ref(),
                            &frame.pixels,
                            frame.width,
                            cpu.grid_top(),
                            cpu.cell_size().1,
                        )
                    );
                }
            }
        }
    }
    out
}

/// The web band's golden digests are this host's band: every scene, theme,
/// width and look, which the web module's parity test must match. Rewrite
/// with `ATERM_BAND_GOLDEN=write` after a deliberate band change.
#[test]
fn the_web_band_golden_is_this_hosts_band() {
    let text = web_golden_text();
    let path = std::path::Path::new(WEB_GOLDEN);
    if std::env::var("ATERM_BAND_GOLDEN").is_ok_and(|v| v == "write") {
        std::fs::create_dir_all(path.parent().expect("fixtures dir")).expect("fixtures dir");
        std::fs::write(path, &text).expect("write the golden");
        return;
    }
    let want = std::fs::read_to_string(path).expect("the golden (ATERM_BAND_GOLDEN=write)");
    let frames = text.lines().filter(|l| l.starts_with("frame\t")).count();
    assert!(
        frames > 500,
        "the golden covers the matrix: {frames} frames"
    );
    for (i, (got, want)) in text.lines().zip(want.lines()).enumerate() {
        assert_eq!(got, want, "golden line {}", i + 1);
    }
    assert_eq!(text.lines().count(), want.lines().count(), "golden length");
}
