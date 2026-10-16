// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU atlas must follow the CPU's rasters.
//!
//! `GpuRenderer` uploads its CPU `Renderer`'s glyph pixels into its own atlas, so a forwarding
//! setter whose CPU side drops its glyphs must drop the atlas too, or the GPU keeps serving the
//! old upload.
//!
//! A source scan: the failure is a setter that never calls the shared helper, which no runtime
//! hook can observe. It needs no GPU. The predicate is `clear_glyph_images`, not any `.clear()`:
//! the CPU's `subpx_glyphs` (the LCD overlay) never reaches this atlas.

use std::collections::BTreeMap;

/// Bodies of every `    pub fn set_<name>(` in `path`, keyed by name: the lines from the
/// signature to the first line that is exactly `    }`. Crude on purpose: it depends only on
/// rustfmt's indentation, which `trustfmt` enforces.
fn public_setter_bodies(path: &str) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let lines: Vec<&str> = text.lines().collect();
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(rest) = lines[i].strip_prefix("    pub fn set_") else {
            i += 1;
            continue;
        };
        let name = format!(
            "set_{}",
            rest.split(['(', '<', ' ']).next().unwrap_or_default()
        );
        let mut j = i + 1;
        let mut body = String::new();
        while j < lines.len() && lines[j] != "    }" {
            body.push_str(lines[j]);
            body.push('\n');
            j += 1;
        }
        out.entry(name).or_insert(body);
        i = j;
    }
    out
}

#[test]
fn every_gpu_setter_whose_cpu_twin_drops_rasters_invalidates_the_atlas() {
    let gpu = public_setter_bodies(concat!(env!("CARGO_MANIFEST_DIR"), "/src/renderer.rs"));
    let cpu = public_setter_bodies(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aterm-render/src/lib.rs"
    ));

    let mut examined = Vec::new();
    let mut mismatched = Vec::new();
    for (name, gpu_body) in &gpu {
        // Only forwarding setters have a CPU twin to disagree with.
        if !gpu_body.contains("self.cpu.") {
            continue;
        }
        let Some(cpu_body) = cpu.get(name) else {
            continue;
        };
        if !cpu_body.contains("clear_glyph_images") {
            continue;
        }
        examined.push(name.clone());
        // A written-down exemption (a `Deliberately NO` comment, as `activate_px` has) passes.
        if gpu_body.contains("invalidate_atlas") || gpu_body.contains("Deliberately NO") {
            continue;
        }
        mismatched.push(name.clone());
    }

    // Anti-vacuity: a scan that stopped matching would examine nothing and pass. The floor
    // sits well under the current count.
    assert!(
        examined.len() >= 8,
        "the scan examined only {} setter pair(s) — it has stopped finding them, \
         so a green result here would mean nothing. Check that `public_setter_bodies` \
         still matches `    pub fn set_…(` and a closing `    }}` at four spaces.",
        examined.len()
    );

    assert!(
        mismatched.is_empty(),
        "these GpuRenderer setters forward to a CPU setter that DROPS its \
         rasterized glyphs, but keep their own atlas: {mismatched:?}\n\
         The CPU will re-rasterize and the GPU will keep serving the old upload, \
         so the change reaches the face and never reaches the glass. Call \
         `self.invalidate_atlas()` (guard it on the value actually changing, as \
         `set_adjust_baseline` and `set_theme` do), or write down why the atlas \
         is unaffected, starting the comment `Deliberately NO`."
    );
}
