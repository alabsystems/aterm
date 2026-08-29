// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `cat_glyph` — render one traced cat-glyph asset TOML to a PNG for visual review.
//!
//! The cat-art v4 pipeline traces the reference sheets to SVG, then hand-labels each
//! glyph into a semantic layered asset (`crates/aterm-effects/art/glyphs/<id>.toml`).
//! This tool rasterizes an asset with the exact [`aterm_scene::vector`] filler the
//! bake path uses, composites it on a light or dark ground, and writes a PNG — the
//! human/agent QA loop against the reference crop.
//!
//! ```text
//! cargo run -q -p aterm-effects --example cat_glyph -- <asset.toml> <out.png> [--px 200] [--bg dark|light]
//! ```
//!
//! Asset format (see `art/glyphs/README` / the labeling brief): paths are SVG `d`
//! strings in the glyph's own `viewbox = [w, h]` pixel frame; each `[[layer]]` names a
//! `role`, a `ref_fill` (the colour it had in the reference), a `recolor` tag (what the
//! genome may swap at bake time — this tool always paints `ref_fill`), and its `paths`.
//! Painter order = file order. White/background paths are dropped at labeling time.

use std::path::Path;
use std::process::ExitCode;

use aterm_scene::{PathCmd, PathTransform, Tile, fill_path, parse_path};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let positional: Vec<&String> = args[1..].iter().filter(|a| !a.starts_with("--")).collect();
    if positional.len() < 2 {
        eprintln!("usage: cat_glyph <asset.toml> <out.png> [--px N] [--bg dark|light]");
        return ExitCode::FAILURE;
    }
    let asset = positional[0];
    let out = positional[1];
    let px: u32 = flag(&args, "--px")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let dark = matches!(flag(&args, "--bg").as_deref(), Some("dark"));

    let toml_text = match std::fs::read_to_string(asset) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read {asset}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let doc: aterm_toml::Value = match toml_text.parse() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("parse {asset}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Viewbox: the pixel frame the path d-strings live in.
    let vb = doc.get("viewbox").and_then(|v| v.as_array());
    let (vw, vh) = match vb {
        Some(a) if a.len() == 2 => (num(&a[0]), num(&a[1])),
        _ => {
            eprintln!("{asset}: missing `viewbox = [w, h]`");
            return ExitCode::FAILURE;
        }
    };
    if vw <= 0.0 || vh <= 0.0 {
        eprintln!("{asset}: degenerate viewbox");
        return ExitCode::FAILURE;
    }

    // Preserve aspect: fit the viewbox height to `px`, width follows.
    let scale = px as f32 / vh;
    let tw = (vw * scale).round().max(1.0) as u32;
    let th = px;
    let mut tile = Tile::new(tw, th);
    let xform = PathTransform {
        scale_x: scale,
        scale_y: scale,
        dx: 0.0,
        dy: 0.0,
    };

    let layers = doc
        .get("layer")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut painted = 0usize;
    for layer in &layers {
        let fill = layer
            .get("ref_fill")
            .and_then(|v| v.as_str())
            .and_then(hex_rgb)
            .unwrap_or((0.5, 0.5, 0.5));
        let alpha = layer.get("alpha").map(num).unwrap_or(1.0).clamp(0.0, 1.0);
        let Some(paths) = layer.get("paths").and_then(|v| v.as_array()) else {
            continue;
        };
        let parsed: Vec<Vec<PathCmd>> = paths
            .iter()
            .filter_map(|p| p.as_str())
            .filter_map(parse_path)
            .collect();
        let refs: Vec<&[PathCmd]> = parsed.iter().map(Vec::as_slice).collect();
        if refs.is_empty() {
            continue;
        }
        fill_path(&mut tile, &refs, fill, alpha, xform);
        painted += 1;
    }

    // Composite over the requested ground.
    let bg: [u8; 3] = if dark {
        [0x1A, 0x1B, 0x26]
    } else {
        [0xFA, 0xFA, 0xF4]
    };
    let src = tile.pixels();
    let mut rgb = vec![0u8; (tw * th * 3) as usize];
    for i in 0..(tw * th) as usize {
        let (r, g, b, a) = (
            src[i * 4] as f32,
            src[i * 4 + 1] as f32,
            src[i * 4 + 2] as f32,
            src[i * 4 + 3] as f32 / 255.0,
        );
        for (c, (fg, bgc)) in [(r, bg[0]), (g, bg[1]), (b, bg[2])].into_iter().enumerate() {
            rgb[i * 3 + c] = (fg * a + bgc as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        }
    }

    if let Err(e) = write_png(Path::new(out), tw, th, &rgb) {
        eprintln!("write {out}: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "wrote {out} ({tw}x{th}, {painted}/{} layers, bg={})",
        layers.len(),
        if dark { "dark" } else { "light" }
    );
    ExitCode::SUCCESS
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn num(v: &aterm_toml::Value) -> f32 {
    v.as_float()
        .map(|f| f as f32)
        .or_else(|| v.as_integer().map(|i| i as f32))
        .unwrap_or(0.0)
}

fn hex_rgb(s: &str) -> Option<(f32, f32, f32)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some((r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0))
}

fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = aterm_png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header()?
        .write_image_data(rgb)
        .map_err(std::io::Error::other)
}
