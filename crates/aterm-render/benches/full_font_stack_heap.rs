// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHERE THE WINDOWS APPLICATION HEAP GOES.

#[cfg(windows)]
#[expect(
    dead_code,
    reason = "`measure` is the `win_heap_attribution` gate's; this bench bills and reports"
)]
#[path = "../tests/common/heapprof.rs"]
mod heapprof;

#[cfg(windows)]
#[global_allocator]
static PROF: heapprof::Prof = heapprof::Prof;

#[cfg(windows)]
use aterm_core::terminal::Terminal;
#[cfg(windows)]
use aterm_render::{Renderer, Theme};

#[cfg(windows)]
fn main() {
    full_font_stack_heap();
}

#[cfg(not(windows))]
fn main() {
    println!("full_font_stack_heap: the Windows heap and font stack; nothing to measure here");
}

#[cfg(windows)]
fn ascii_frame(r: &mut Renderer) {
    let mut term = Terminal::new(24, 80);
    term.process(b"$ cargo build --release\r\n   Compiling aterm v0.63.0\r\n");
    let input = term.cell_frame(24, 80);
    let _ = r.render_input(&input);
}

/// The candidate font files aterm's Windows cascade reads at seal, with the
/// sizes this host reports — the arithmetic the seal's cost is checked against.
#[cfg(windows)]
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

/// The WHOLE font stack a windowed aterm builds, in the order it builds it:
/// the terminal engine, then the GUI chrome's faces (`tray_raster`), then the
/// background font-catalogue warm.
#[cfg(windows)]
fn full_font_stack_heap() {
    windows_font_bill();
    heapprof::mark("process start");
    heapprof::start();

    let mut r = heapprof::bill("Renderer::from_system", || {
        Renderer::from_system(14.0, Theme::default())
    })
    .expect("no font: build with aterm-render's default embedded-font feature");
    heapprof::bill("seal_admitted_font_sources", || {
        r.seal_admitted_font_sources()
    });
    heapprof::bill("one ASCII frame", || ascii_frame(&mut r));

    let embedded = heapprof::bill("chrome fallback: embedded DejaVu parse", || {
        aterm_render::shared_parsed_face(aterm_render::embedded_font(), 0)
    });

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

    let ui = heapprof::bill("chrome UI pair (SegUIVar + seguisb)", ui_font_pair);

    let semantic = heapprof::bill("semantic renderer fork", || {
        r.fork_semantic_surface(14.0, Theme::default())
    });

    let batch = heapprof::bill("font_catalog::resolve_and_admit", || {
        aterm_render::font_catalog::resolve_and_admit(&[(
            "Cascadia Mono".to_string(),
            aterm_render::FaceTarget::REGULAR,
        )])
    });

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
#[cfg(windows)]
fn ui_font_pair() -> Option<(
    std::sync::Arc<aterm_render::font::Font>,
    std::sync::Arc<[u8]>,
)> {
    let root = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let parse = |p: std::path::PathBuf| -> Option<(aterm_render::font::Font, Vec<u8>)> {
        let bytes = std::fs::read(p).ok()?;
        let font = aterm_render::font::Font::from_bytes(
            &bytes[..],
            aterm_render::font::FontSettings::default(),
        )
        .ok()?;
        Some((font, bytes))
    };
    let (regular, bytes) = parse(root.join("SegUIVar.ttf"))?;
    let (_semibold, _) = parse(root.join("seguisb.ttf"))?;
    Some((std::sync::Arc::new(regular), bytes.into()))
}
