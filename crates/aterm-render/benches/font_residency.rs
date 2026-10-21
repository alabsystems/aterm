// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HOW MUCH OF EACH MAPPED FONT FILE THE FONT STACK TOUCHES ON WINDOWS — the
//! "pages fault in only when touched" half of the mapped-font case, which
//! `docs/measured/win-heap-2026-08-29.md` §6.1 says main has not measured. A
//! measurement, not a gate:
//!
//! ```sh
//! targo --unverified bench -p aterm-render --bench font_residency
//! ```
//!
//! For each file of the Windows font seal (§2 of that record) that
//! `font_file::admit_font_file` maps (`MAPPABLE_FONT_ROOTS`), every arm takes a
//! fresh mapping of its own, so a page one arm faulted in is never counted by
//! the next, and asks the kernel (`QueryWorkingSetEx`) how many of the view's
//! pages are valid in this process's working set, and how many of those are
//! shared (file-backed, not private commit), after:
//!
//! - `map`: the admission alone (open, size bound, map), nothing read;
//! - `admit`: the admission's verdict, `aterm_font::Face::parse` and a non-zero
//!   `units_per_em`, what `face_admissible` asks before a face enters the chain;
//! - `cmap`: that, then the render thread's coverage question for one code point
//!   of every script the chain carries (`Face::glyph_index`);
//! - `parse`: the face materialised (`Font::from_shared`, what a chain face's
//!   first draw runs through `LazyFontdue::get`);
//! - `draw`: that, then each covered sample rasterized at 16 px.
//!
//! NON-VACUITY: a file that was not mapped, or that the admission refused, is
//! named and fails the run, since its counts would measure a copy or a failed
//! parse; a file that is absent is named as not measured.
//!
//! Re-derived on 2026-10-19 from win/font-mmap ede86746e2's `font_mmap::residency`
//! probe and its `#[ignore]`d `how_much_of_each_font_file_the_font_stack_actually_touches`
//! (2026-08-29, tag `archive/win-font-mmap`), whose arms measured that branch's
//! fontdue admission; these measure main's. Public API only, as
//! `first_frame_per_script`. Windows only: elsewhere it says so and exits.
//! Type-checked for Windows from macOS (`verify cells --cell win --cell win-arm`);
//! its first run is owed to a Windows host (`docs/WINDOWS_PARAGON_DESIGN.md` §8).

/// The files the Windows seal reads (`docs/measured/win-heap-2026-08-29.md` §2):
/// the fallback chain in `FALLBACK_CANDIDATES`' order, then the symbol and
/// colour faces.
#[cfg(windows)]
const SEAL_FILES: &[&str] = &[
    "msyh.ttc",
    "malgun.ttf",
    "Nirmala.ttc",
    "LeelawUI.ttf",
    "sylfaen.ttf",
    "ebrima.ttf",
    "gadugi.ttf",
    "mvboli.ttf",
    "arial.ttf",
    "seguisym.ttf",
    "seguiemj.ttf",
];

/// One code point per script the chain names a face for
/// (`windows_fallback_chain_covers_the_scripts_it_claims`), then the symbol
/// slot's and the colour slot's.
#[cfg(windows)]
const SCRIPT_SAMPLES: &[char] = &[
    '\u{4E2D}',  // Han
    '\u{30C6}',  // Kana
    '\u{D55C}',  // Hangul
    '\u{1100}',  // Hangul jamo
    '\u{0915}',  // Devanagari
    '\u{0B95}',  // Tamil
    '\u{0985}',  // Bengali
    '\u{0C05}',  // Telugu
    '\u{0D05}',  // Malayalam
    '\u{0D85}',  // Sinhala
    '\u{0E01}',  // Thai
    '\u{1780}',  // Khmer
    '\u{0E81}',  // Lao
    '\u{10D0}',  // Georgian
    '\u{1200}',  // Ethiopic
    '\u{2D30}',  // Tifinagh
    '\u{A500}',  // Vai
    '\u{13A0}',  // Cherokee
    '\u{1401}',  // Canadian Aboriginal
    '\u{0780}',  // Thaana
    '\u{05D0}',  // Hebrew
    '\u{0627}',  // Arabic
    '\u{0531}',  // Armenian
    '\u{23F8}',  // the symbol slot
    '\u{1F600}', // the colour slot
];

/// The kernel's answer for one view: which of its pages are in this process's
/// working set.
#[cfg(windows)]
mod residency {
    use std::ffi::c_void;

    /// `PSAPI_WORKING_SET_EX_INFORMATION`: an address and its
    /// `PSAPI_WORKING_SET_EX_BLOCK` (bit 0 `Valid`, bit 15 `Shared`).
    #[repr(C)]
    struct WorkingSetExInformation {
        virtual_address: *mut c_void,
        attributes: usize,
    }

    // SAFETY: standard kernel32 declarations, signatures per the Win32 docs
    // (`K32QueryWorkingSetEx` is psapi's `QueryWorkingSetEx` as kernel32
    // exports it).
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32QueryWorkingSetEx(process: *mut c_void, buffer: *mut c_void, size: u32) -> i32;
    }

    /// The small page on both Windows triples aterm builds (x86_64, aarch64).
    pub(crate) const PAGE: usize = 4096;

    /// How many of `view`'s pages are VALID in this process's working set, and
    /// how many of those the kernel reports SHARED (a section other processes
    /// can map too, not private commit).
    pub(crate) fn resident_pages(view: &[u8]) -> (usize, usize) {
        let base = view.as_ptr();
        let mut info: Vec<WorkingSetExInformation> = (0..view.len().div_ceil(PAGE))
            .map(|page| WorkingSetExInformation {
                // Inside the view: `page * PAGE < len` for every page counted.
                virtual_address: base.wrapping_add(page * PAGE).cast_mut().cast::<c_void>(),
                attributes: 0,
            })
            .collect();
        let bytes = u32::try_from(std::mem::size_of_val(info.as_slice()))
            .expect("a font view's page list fits a u32 byte count");
        // SAFETY: `info` is a live array of correctly shaped entries and `bytes`
        // is its exact size; the kernel writes only their `attributes`.
        let ok = unsafe {
            K32QueryWorkingSetEx(
                GetCurrentProcess(),
                info.as_mut_ptr().cast::<c_void>(),
                bytes,
            )
        };
        assert!(
            ok != 0,
            "QueryWorkingSetEx failed: {}",
            std::io::Error::last_os_error()
        );
        for (page, entry) in info.iter().enumerate() {
            assert_eq!(
                entry.virtual_address.cast_const().cast::<u8>(),
                base.wrapping_add(page * PAGE),
                "QueryWorkingSetEx answers each entry in place"
            );
        }
        let valid = |entry: &&WorkingSetExInformation| entry.attributes & 1 == 1;
        let resident = info.iter().filter(valid).count();
        let shared = info
            .iter()
            .filter(valid)
            .filter(|entry| (entry.attributes >> 15) & 1 == 1)
            .count();
        (resident, shared)
    }
}

/// One arm's pages: `work` runs on a fresh mapping of `path`, and the pages
/// valid after it are counted, with the shared ones among them. `Err` names a
/// file that was not mapped.
#[cfg(windows)]
fn pages_after(
    path: &std::path::Path,
    work: &dyn Fn(&aterm_render::font::FaceBytes),
) -> Result<(usize, usize), String> {
    let bytes = aterm_render::font_file::admit_font_file(path)
        .map_err(|error| format!("{}: not admitted: {error}", path.display()))?;
    if !bytes.is_mapped() {
        return Err(format!(
            "{}: admitted as a copy, not a mapping",
            path.display()
        ));
    }
    work(&bytes);
    Ok(residency::resident_pages(&bytes))
}

/// The admission's verdict: what `face_admissible` asks of a face.
#[cfg(windows)]
fn admissible(bytes: &[u8]) -> bool {
    aterm_font::Face::parse(bytes, 0).is_ok_and(|face| face.units_per_em() > 0)
}

/// How many of [`SCRIPT_SAMPLES`] face 0 of `bytes` maps to a real glyph.
#[cfg(windows)]
fn covered(bytes: &[u8]) -> usize {
    aterm_font::Face::parse(bytes, 0).map_or(0, |face| {
        SCRIPT_SAMPLES
            .iter()
            .filter(|&&ch| face.glyph_index(ch).is_some_and(|glyph| glyph.0 != 0))
            .count()
    })
}

/// One file's five arms, each on its own mapping, in the module note's order,
/// with the admission's verdict and the samples face 0 covers.
#[cfg(windows)]
struct Arms {
    pages: [(usize, usize); 5],
    admitted: bool,
    covers: usize,
}

#[cfg(windows)]
fn arms_of(path: &std::path::Path) -> Result<Arms, String> {
    use aterm_render::font::{Font, FontSettings};

    let admitted = std::cell::Cell::new(false);
    let covers = std::cell::Cell::new(0_usize);
    let pages = [
        pages_after(path, &|_| {})?,
        pages_after(path, &|bytes| admitted.set(admissible(bytes)))?,
        pages_after(path, &|bytes| {
            std::hint::black_box(admissible(bytes));
            covers.set(covered(bytes));
        })?,
        pages_after(path, &|bytes| {
            drop(std::hint::black_box(Font::from_shared(
                bytes.clone(),
                FontSettings::default(),
            )));
        })?,
        pages_after(path, &|bytes| {
            if let Ok(font) = Font::from_shared(bytes.clone(), FontSettings::default()) {
                for &ch in SCRIPT_SAMPLES {
                    if font.lookup_glyph_index(ch) != 0 {
                        std::hint::black_box(font.rasterize(ch, 16.0));
                    }
                }
            }
        })?,
    ];
    Ok(Arms {
        pages,
        admitted: admitted.get(),
        covers: covers.get(),
    })
}

#[cfg(windows)]
fn main() {
    use residency::PAGE;

    let root = std::path::Path::new(aterm_render::font_file::MAPPABLE_FONT_ROOTS[0]);
    eprintln!(
        "\n{:<14} {:>9} {:>6} | {:>5} {:>6} {:>6} {:>6} {:>6} | {:>7}",
        "file", "bytes", "pages", "map", "admit", "cmap", "parse", "draw", "shared"
    );
    let mut totals = [0_usize; 6];
    let mut refused = Vec::new();
    let mut absent = Vec::new();
    for name in SEAL_FILES {
        let path = root.join(name);
        if !path.is_file() {
            absent.push(*name);
            continue;
        }
        let arms = match arms_of(&path) {
            Ok(arms) => arms,
            Err(why) => {
                refused.push(why);
                continue;
            }
        };
        if !arms.admitted {
            refused.push(format!(
                "{name}: the admission refused it, so its pages measure a failed parse"
            ));
            continue;
        }
        let len = std::fs::metadata(&path).map_or(0, |meta| meta.len());
        let pages = usize::try_from(len).unwrap_or(usize::MAX).div_ceil(PAGE);
        let [map, admit, cmap, parse, draw] = arms.pages.map(|(resident, _)| resident);
        let shared = arms.pages[4].1;
        eprintln!(
            "{name:<14} {len:>9} {pages:>6} | {map:>5} {admit:>6} {cmap:>6} {parse:>6} {draw:>6} | \
             {shared:>7}   (covers {}/{} samples; parse {:.1}%, draw {:.1}%)",
            arms.covers,
            SCRIPT_SAMPLES.len(),
            100.0 * parse as f64 / pages.max(1) as f64,
            100.0 * draw as f64 / pages.max(1) as f64,
        );
        for (total, value) in totals
            .iter_mut()
            .zip([pages, map, admit, cmap, parse, draw])
        {
            *total += value;
        }
    }
    let [pages, map, admit, cmap, parse, draw] = totals;
    let kb = |p: usize| p * PAGE / 1024;
    eprintln!(
        "{:<14} {:>9} {pages:>6} | {map:>5} {admit:>6} {cmap:>6} {parse:>6} {draw:>6}",
        "TOTAL",
        pages * PAGE
    );
    eprintln!(
        "  kB: whole {} | map {} | admit {} | cmap {} | parse {} | draw {}",
        kb(pages),
        kb(map),
        kb(admit),
        kb(cmap),
        kb(parse),
        kb(draw)
    );
    if !absent.is_empty() {
        eprintln!("not installed here, so not measured: {}", absent.join(", "));
    }
    assert!(
        refused.is_empty(),
        "these files measure no mapped face: {}",
        refused.join("; ")
    );
}

#[cfg(not(windows))]
fn main() {
    println!(
        "font_residency: how much of each mapped Windows font file the font stack touches; nothing to measure here"
    );
}
