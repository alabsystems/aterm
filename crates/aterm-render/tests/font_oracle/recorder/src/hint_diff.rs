// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hinter differential comparing `skrifa` and `aterm_font::autohint` outline command streams.

use crate::hint::{Mode, PathPen, Skrifa};
use crate::hint_raster::Hinter;
use crate::probe::Fnv;

/// aterm-font's sink into the oracle's pen.
struct Pen<'a>(&'a mut PathPen);

impl aterm_font::OutlineSink for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.curve_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

fn target(mode: Mode) -> aterm_font::autohint::Target {
    match mode {
        Mode::Full => aterm_font::autohint::Target::Normal,
        Mode::Light => aterm_font::autohint::Target::Light,
        Mode::Lcd => aterm_font::autohint::Target::Lcd,
    }
}

/// One drawn glyph: `None` when the hinter declined it, else the digest of
/// its command stream.
fn draw_skrifa(
    hint: &skrifa::outline::HintingInstance,
    bytes: &[u8],
    index: u32,
    gid: u16,
) -> Option<String> {
    let mut pen = PathPen::default();
    Skrifa.draw(hint, bytes, index, gid, &mut pen)?;
    let mut h = Fnv::new();
    pen.digest(&mut h);
    Some(h.hex())
}

fn draw_carve(
    hint: &aterm_font::autohint::HintingInstance,
    bytes: &[u8],
    index: u32,
    gid: u16,
) -> Option<String> {
    let mut pen = PathPen::default();
    hint.draw(bytes, index, gid, &mut Pen(&mut pen))?;
    let mut h = Fnv::new();
    pen.digest(&mut h);
    Some(h.hex())
}

/// Returns every font file under the host's system font directories, `dir`, and `extra`, sorted.
pub(crate) fn face_files(dir: &str, extra: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let roots = [
        "/System/Library/Fonts".to_string(),
        "/Library/Fonts".to_string(),
        "/usr/share/fonts".to_string(),
        "C:\\Windows\\Fonts".to_string(),
        format!("{dir}/assets"),
        format!("{dir}/tests/fixtures"),
    ];
    let mut stack: Vec<std::path::PathBuf> = roots.iter().chain(extra).map(Into::into).collect();
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| {
                ["ttf", "otf", "ttc", "otc"]
                    .iter()
                    .any(|ext| x.eq_ignore_ascii_case(ext))
            }) {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[derive(Default)]
struct Tally {
    faces: usize,
    instances: usize,
    draws: usize,
    identical: usize,
    both_declined: usize,
    differ: usize,
    skrifa_only: usize,
    carve_only: usize,
    instance_mismatch: usize,
}

pub(crate) fn run(dir: &str, extra: &[String]) {
    let mut t = Tally::default();
    let mut report: Vec<String> = Vec::new();
    for path in face_files(dir, extra) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let count = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
        for index in 0..count {
            let Ok(face) = ttf_parser::Face::parse(&bytes, index) else {
                continue;
            };
            t.faces += 1;
            let n = face.number_of_glyphs();
            let stride = (n / 3000).max(1);
            let gids: Vec<u16> = (0..n).step_by(usize::from(stride)).collect();
            let mut instances: Vec<Vec<(u32, f32)>> = vec![Vec::new()];
            for axis in face.variation_axes() {
                let tag = axis.tag.0;
                instances.push(vec![(tag, axis.min_value)]);
                instances.push(vec![(tag, axis.max_value)]);
                if &axis.tag.to_bytes() == b"wght" && axis.max_value >= 700.0 {
                    instances.push(vec![(tag, 700.0)]);
                }
            }
            let mut face_differ = 0usize;
            for coords in &instances {
                for px in [11.0f32, 16.0, 23.0] {
                    for mode in [Mode::Full, Mode::Light, Mode::Lcd] {
                        if mode == Mode::Lcd && !coords.is_empty() {
                            continue;
                        }
                        t.instances += 1;
                        let sk = Skrifa.instance(&bytes, index, px, mode, coords);
                        let ca = aterm_font::autohint::HintingInstance::new(
                            &bytes,
                            index,
                            px,
                            coords,
                            target(mode),
                        );
                        let (sk, ca) = match (sk, ca) {
                            (Some(sk), Some(ca)) => (sk, ca),
                            (None, None) => continue,
                            (sk, ca) => {
                                t.instance_mismatch += 1;
                                report.push(format!(
                                    "INSTANCE {path}#{index} {coords:?} {px} {mode:?}: skrifa {} carve {}",
                                    sk.is_some(),
                                    ca.is_some()
                                ));
                                continue;
                            }
                        };
                        for &gid in &gids {
                            t.draws += 1;
                            match (
                                draw_skrifa(&sk, &bytes, index, gid),
                                draw_carve(&ca, &bytes, index, gid),
                            ) {
                                (Some(a), Some(b)) if a == b => t.identical += 1,
                                (None, None) => t.both_declined += 1,
                                (Some(_), Some(_)) => {
                                    t.differ += 1;
                                    face_differ += 1;
                                    if face_differ <= 3 {
                                        report.push(format!(
                                            "DIFFER {path}#{index} {coords:?} {px} {mode:?} gid {gid}"
                                        ));
                                    }
                                }
                                (Some(_), None) => {
                                    t.skrifa_only += 1;
                                    face_differ += 1;
                                    if face_differ <= 3 {
                                        report.push(format!(
                                            "SKRIFA-ONLY {path}#{index} {coords:?} {px} {mode:?} gid {gid}"
                                        ));
                                    }
                                }
                                (None, Some(_)) => {
                                    t.carve_only += 1;
                                    face_differ += 1;
                                    if face_differ <= 3 {
                                        report.push(format!(
                                            "CARVE-ONLY {path}#{index} {coords:?} {px} {mode:?} gid {gid}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if face_differ > 0 {
                report.push(format!(
                    "FACE {path}#{index}: {face_differ} glyph draws differ"
                ));
            }
        }
    }
    for line in &report {
        println!("{line}");
    }
    println!(
        "hint-differential: {} faces, {} face-instances, {} glyph draws: {} identical, {} declined by both, {} differ, {} skrifa only, {} carve only; {} instances built by one hinter only",
        t.faces,
        t.instances,
        t.draws,
        t.identical,
        t.both_declined,
        t.differ,
        t.skrifa_only,
        t.carve_only,
        t.instance_mismatch
    );
}
