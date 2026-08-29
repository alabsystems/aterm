// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// WHERE THE WINDOWS APPLICATION HEAP GOES — measured, per allocation site and
// per line item.
//
// Run one at a time (see `heapprof`'s "one report per process" note):
//   targo test --profile profiling -p aterm-render --test win_heap_attribution \
//     -- --ignored --nocapture --test-threads=1 <name>

#![cfg(windows)]

#[path = "common/heapprof.rs"]
mod heapprof;

#[global_allocator]
static PROF: heapprof::Prof = heapprof::Prof;

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

fn ascii_frame(r: &mut Renderer) {
    let mut term = Terminal::new(24, 80);
    term.process(b"$ cargo build --release\r\n   Compiling aterm v0.63.0\r\n");
    let input = term.cell_frame(24, 80);
    let _ = r.render_input(&input);
}

/// The candidate font files aterm's Windows cascade reads at seal, with the
/// sizes this host reports — the arithmetic the seal's cost is checked against.
fn windows_font_bill() {
    let files = [
        (
            "chain  msyh.ttc (Microsoft YaHei)",
            r"C:\Windows\Fonts\msyh.ttc",
        ),
        (
            "chain  malgun.ttf (Malgun Gothic)",
            r"C:\Windows\Fonts\malgun.ttf",
        ),
        (
            "chain  Nirmala.ttc (Nirmala UI)",
            r"C:\Windows\Fonts\Nirmala.ttc",
        ),
        ("chain  LeelawUI.ttf", r"C:\Windows\Fonts\LeelawUI.ttf"),
        ("chain  sylfaen.ttf", r"C:\Windows\Fonts\sylfaen.ttf"),
        ("chain  ebrima.ttf", r"C:\Windows\Fonts\ebrima.ttf"),
        ("chain  gadugi.ttf", r"C:\Windows\Fonts\gadugi.ttf"),
        ("chain  mvboli.ttf", r"C:\Windows\Fonts\mvboli.ttf"),
        (
            "chain  arial.ttf (broad backstop)",
            r"C:\Windows\Fonts\arial.ttf",
        ),
        ("symbol seguisym.ttf", r"C:\Windows\Fonts\seguisym.ttf"),
        ("colour seguiemj.ttf", r"C:\Windows\Fonts\seguiemj.ttf"),
        (
            "primary CascadiaMono.ttf",
            r"C:\Windows\Fonts\CascadiaMono.ttf",
        ),
        ("chrome SegUIVar.ttf", r"C:\Windows\Fonts\SegUIVar.ttf"),
        ("chrome seguisb.ttf", r"C:\Windows\Fonts\seguisb.ttf"),
    ];
    let mut sealed = 0_u64;
    eprintln!("\n--- font files on this host ---");
    for (what, path) in files {
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        eprintln!("  {what:<38} {:>9} B  {:>7} kB", len, len / 1024);
        if what.starts_with("chain") || what.starts_with("symbol") || what.starts_with("colour") {
            sealed += len;
        }
    }
    eprintln!(
        "  {:<38} {:>9} B  {:>7} kB   <= what the seal must read\n",
        "SUM(chain + symbol + colour)",
        sealed,
        sealed / 1024
    );
}

// ---------------------------------------------------------------------------
// THE GATE. Everything else in this file is a measurement (`#[ignore]`d); this
// one runs in the normal suite, and it is the standing guard on the two things
// the Windows font seal is allowed to do.
// ---------------------------------------------------------------------------

/// What the seal may LEAVE BEHIND. Measured on this box: **55,086 kB**, which is
/// the eleven font files of §2 to within 0.01% and nothing else — the chain's
/// fontdue parses are deferred (`LazyFontdue`, W8). The cap is set where an
/// EAGER parse of any chain face would blow straight through it: `msyh.ttc` is
/// 19.7 MB of file, and fontdue materialises every outline at parse (the Linux
/// note measured a 4.0 MB file parsing to 113.8 MB).
const MAX_SEAL_LIVE_KB: i64 = 96 * 1024;

/// What the seal may allocate TRANSIENTLY over what it keeps. Measured: **2 kB**
/// — the read buffers go straight into the discovery store. Before
/// `intern_discovered_font_bytes` took the reader's handle it was **5,063 kB**,
/// so this bound is what catches a return to a copying intern; the headroom over
/// the real figure is three orders of magnitude.
const MAX_SEAL_TRANSIENT_KB: i64 = 4 * 1024;

/// The Windows font seal reads its candidate files ONCE each and keeps ONE copy.
///
/// Two independent regressions this catches, both of them invisible to every
/// other test in the tree because nothing else asks the process how many bytes it
/// is holding:
///
/// 1. an EAGER fontdue parse creeping back into the fallback chain, symbol slot
///    or colour face — the `live` bound;
/// 2. a COPYING intern on the discovery read path — the `transient` bound. That
///    is the defect this gate was written with: `FallbackFace::from_path_bytes`
///    took a slice and copied it, so all nine Windows chain files were resident
///    twice while `build_fallback_chain`'s scoped workers ran.
///
/// Gated: no system font -> the test no-ops.
#[test]
fn the_font_seal_reads_each_file_once_and_keeps_one_copy() {
    let Some(mut r) = Renderer::from_system(14.0, Theme::default()) else {
        eprintln!("SKIP: no system font");
        return;
    };
    let ((), live_kb, peak_kb) = heapprof::measure(|| {
        r.seal_admitted_font_sources();
    });
    eprintln!("seal: live +{live_kb} kB, own transient peak +{peak_kb} kB");
    // Non-vacuity: the seal really did admit faces, so a passing run cannot be
    // a renderer that read nothing.
    assert!(
        live_kb > 1024,
        "the seal kept {live_kb} kB — it admitted no font files, so the bounds \
         below would pass vacuously"
    );
    assert!(
        live_kb <= MAX_SEAL_LIVE_KB,
        "the font seal kept {live_kb} kB (cap {MAX_SEAL_LIVE_KB} kB). Either a \
         chain/symbol/colour face is being fontdue-PARSED eagerly again, or a \
         face is resident more than once. See docs/measured/win-heap-2026-08-29.md"
    );
    assert!(
        peak_kb - live_kb <= MAX_SEAL_TRANSIENT_KB,
        "the font seal allocated {} kB it did not keep (cap {MAX_SEAL_TRANSIENT_KB} kB). \
         The discovery read is copying its buffer instead of handing it to \
         `intern_discovered_font_bytes` — on Windows that is the whole nine-file \
         fallback chain resident twice. See docs/measured/win-heap-2026-08-29.md §5",
        peak_kb - live_kb
    );
}

/// The terminal font engine alone: what a `Renderer` holds in an ASCII-only
/// session, step by step.
#[test]
#[ignore = "measurement, not a gate: run alone with --nocapture"]
fn renderer_startup_heap() {
    windows_font_bill();
    heapprof::mark("process start");
    heapprof::start();

    let Some(mut r) = heapprof::bill("Renderer::from_system", || {
        Renderer::from_system(14.0, Theme::default())
    }) else {
        eprintln!("SKIP: no system font");
        return;
    };
    heapprof::mark("from_system (primary parsed)");

    heapprof::bill("seal_admitted_font_sources", || {
        r.seal_admitted_font_sources()
    });
    heapprof::mark("seal_admitted_font_sources");

    heapprof::bill("one ASCII frame", || ascii_frame(&mut r));
    heapprof::mark("one ASCII frame");

    heapprof::report("renderer, ASCII-only, settled", 30);
}

/// The WHOLE font stack a windowed aterm builds, in the order it builds it:
/// the terminal engine, then the GUI chrome's faces (`tray_raster`), then the
/// two background warms (`warm_font_coverage_index`, the font catalogue).
///
/// Every step below is the real shipped call. The three that live in
/// `aterm-gui` are reproduced here EXACTLY as that crate writes them (the call
/// is quoted in the comment) because `tray_raster` is `pub(crate)`; each one
/// goes through the same public `aterm-render` entry point the GUI uses, so the
/// bytes are the same bytes.
#[test]
#[ignore = "measurement, not a gate: run alone with --nocapture"]
fn full_font_stack_heap() {
    windows_font_bill();
    heapprof::mark("process start");
    heapprof::start();

    let Some(mut r) = heapprof::bill("Renderer::from_system", || {
        Renderer::from_system(14.0, Theme::default())
    }) else {
        eprintln!("SKIP: no system font");
        return;
    };
    heapprof::bill("seal_admitted_font_sources", || {
        r.seal_admitted_font_sources()
    });
    heapprof::bill("one ASCII frame", || ascii_frame(&mut r));

    // aterm-gui `tray_raster::default_chrome_fonts()`:
    //   fallback: ChromeFace::from_bytes(aterm_render::embedded_font(), 0)
    let embedded = heapprof::bill("chrome fallback: embedded DejaVu parse", || {
        aterm_render::shared_parsed_face(aterm_render::embedded_font(), 0)
    });

    // aterm-gui `App::sync_chrome_fonts()` -> `tray_raster::set_chrome_fonts`:
    //   ChromeFace::from_bytes(renderer.chrome_primary_face())
    //   ChromeFace::from_bytes(renderer.chrome_bold_face())
    let cp = r.chrome_primary_face();
    eprintln!(
        "   chrome_primary_face -> {}",
        cp.as_ref().map_or("None".to_string(), |(b, i)| format!(
            "{} B at index {i}",
            b.len()
        ))
    );
    let chrome_primary = heapprof::bill("chrome primary face parse", || {
        cp.and_then(|(b, i)| aterm_render::shared_parsed_face(&b, i).ok())
    });
    let cb = r.chrome_bold_face();
    eprintln!(
        "   chrome_bold_face    -> {}",
        cb.as_ref().map_or("None".to_string(), |(b, i)| format!(
            "{} B at index {i}",
            b.len()
        ))
    );
    let chrome_bold = heapprof::bill("chrome bold face parse", || {
        cb.and_then(|(b, i)| aterm_render::shared_parsed_face(&b, i).ok())
    });

    // aterm-gui `tray_raster::resolve_ui_font_assets()` -> `parse_ui_font` x2.
    // Verbatim: `std::fs::read` then `fontdue::Font::from_bytes`, and the
    // REGULAR's file bytes are retained by `variable_semibold_of` on Windows.
    let ui = heapprof::bill("chrome UI pair (SegUIVar + seguisb)", ui_font_pair);

    // aterm-gui `App::sync_chrome_fonts()` third value.
    let semantic = heapprof::bill("semantic renderer fork", || {
        r.fork_semantic_surface(14.0, Theme::default())
    });

    // aterm-gui `app_render.rs` first-present hook.
    heapprof::bill(
        "warm_font_coverage_index",
        aterm_render::warm_font_coverage_index,
    );

    // aterm-gui config font resolution (`native_font_catalog`).
    let batch = heapprof::bill("font_catalog::resolve_and_admit", || {
        aterm_render::font_catalog::resolve_and_admit(&["Cascadia Mono".to_string()])
    });

    // --- the ENGINE side of one tab, for scale against the font terms ---
    // A ~1080p grid on this suite's standard face, the geometry the GPU
    // memory-floor work measured at (`aterm-gpu/tests/gpu_memory_floor.rs`).
    let mut big = Terminal::new(51, 174);
    heapprof::bill("Terminal::new(51x174) (one empty tab)", || {
        big.process(b"$ cargo build --release\r\n")
    });
    heapprof::bill("one 51x174 CPU frame (raster + atlas + cache)", || {
        let input = big.cell_frame(51, 174);
        let _ = r.render_input(&input);
    });
    heapprof::bill("30,000-line scrollback fill", || {
        for i in 1..=30_000u32 {
            big.process(format!("{i}\r\n").as_bytes());
        }
        let input = big.cell_frame(51, 174);
        let _ = r.render_input(&input);
    });

    heapprof::mark("settled");
    heapprof::report("full font stack, ASCII-only, settled", 34);

    // Keep every handle alive across the report.
    std::hint::black_box((
        &embedded,
        &chrome_primary,
        &chrome_bold,
        &ui,
        &semantic,
        &batch,
    ));
    drop(r);
}

/// `tray_raster::resolve_ui_font_assets`'s Windows arm, verbatim.
fn ui_font_pair() -> Option<(std::sync::Arc<fontdue::Font>, std::sync::Arc<[u8]>)> {
    let root = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let parse = |p: std::path::PathBuf| -> Option<(fontdue::Font, Vec<u8>)> {
        let bytes = std::fs::read(p).ok()?;
        let font = fontdue::Font::from_bytes(&bytes[..], fontdue::FontSettings::default()).ok()?;
        Some((font, bytes))
    };
    let (regular, bytes) = parse(root.join("SegUIVar.ttf"))?;
    let (_semibold, _) = parse(root.join("seguisb.ttf"))?;
    // `variable_semibold_of` keeps the variable regular's whole file.
    Some((std::sync::Arc::new(regular), bytes.into()))
}
