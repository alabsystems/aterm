// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE FIRST NON-LATIN FRAME COSTS ON WINDOWS — the other half of the mapped-font question.

#[cfg(windows)]
use aterm_core::terminal::Terminal;
#[cfg(windows)]
use aterm_render::{Renderer, Theme};

/// One line per script the Windows fallback chain names a face for, in the
/// order `FALLBACK_CANDIDATES` lists them, so the first frame of each is the
/// first time its face is asked to draw.
#[cfg(windows)]
const SCRIPT_LINES: &[(&str, &str)] = &[
    ("Han (msyh)", "中文测试 汉字渲染"),
    ("Hangul (malgun)", "한국어 조합 테스트"),
    ("Devanagari (Nirmala)", "देवनागरी परीक्षण"),
    ("Thai (LeelawUI)", "ทดสอบภาษาไทย"),
    ("Armenian (sylfaen)", "հայերեն փորձարկում"),
    ("Georgian (sylfaen)", "ქართული ტესტი"),
    ("Hebrew", "עברית בדיקה"),
    ("Arabic", "اختبار العربية"),
    ("Thaana (mvboli)", "ދިވެހި ޓެސްޓ"),
    ("symbol slot", "\u{23F8}\u{23F9}\u{23FA}"),
    ("colour emoji", "\u{1F600}\u{1F680}\u{1F30D}"),
];

/// Milliseconds `f` took on the wall clock.
#[cfg(windows)]
fn ms(f: impl FnOnce()) -> f64 {
    let t = std::time::Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

/// One 24x80 frame of `text`, rendered whole.
#[cfg(windows)]
fn frame_of(r: &mut Renderer, text: &[u8]) {
    let mut term = Terminal::new(24, 80);
    term.process(text);
    let input = term.cell_frame(24, 80);
    let _ = r.render_input(&input);
}

#[cfg(windows)]
fn main() {
    let mut r = Renderer::from_system(14.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let mut residency = (0_u64, 0_u64);
    let seal_ms = ms(|| {
        residency = r.seal_admitted_font_sources().discovered_residency();
    });
    let ascii_ms = ms(|| frame_of(&mut r, b"$ cargo build --release\r\n"));
    let (mapped, copied) = residency;
    eprintln!(
        "\nseal {seal_ms:.1} ms (discovered faces mapped {} kB, copied {} kB)   \
         first ASCII frame {ascii_ms:.1} ms",
        mapped / 1024,
        copied / 1024
    );
    eprintln!("{:<24} {:>10} {:>10}", "script", "first ms", "second ms");
    let mut worst = 0.0_f64;
    for (label, text) in SCRIPT_LINES {
        let first = ms(|| frame_of(&mut r, text.as_bytes()));
        let second = ms(|| frame_of(&mut r, text.as_bytes()));
        eprintln!("{label:<24} {first:>10.1} {second:>10.1}");
        worst = worst.max(first);
    }
    eprintln!("worst first frame: {worst:.1} ms\n");
}

#[cfg(not(windows))]
fn main() {
    println!(
        "first_frame_per_script: the Windows fallback chain's first frames; nothing to measure here"
    );
}
