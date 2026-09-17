// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ATLAS MUST FOLLOW THE CPU'S RASTERS.
//!
//! `GpuRenderer` wraps a CPU `Renderer` and keeps its own copy of that
//! renderer's glyph pixels, uploaded into a texture atlas. So the two hold the
//! same pixels twice, and every setter that changes them has to drop BOTH. When
//! the CPU setter drops its rasterized glyphs and the GPU wrapper does not
//! invalidate its atlas, the CPU re-rasterizes and the GPU keeps serving the old
//! upload: the config edit reaches the face and never reaches the glass.
//!
//! That is not hypothetical. `set_adjust_baseline` forwarded to the CPU face —
//! which drops its glyphs, because the baseline moves the pixels — and never
//! invalidated, so a live `adjust_baseline` edit did nothing visible.
//! `set_runtime_font_discovery` had the same gap on its chain-widening path.
//!
//! **This is a SOURCE scan, deliberately, and for the same reason
//! `ty_drivers_are_armed.rs` is one**: the failure mode is a setter that never
//! calls the shared helper at all, which no runtime hook on the helper can
//! observe. It also needs no GPU, so it gates on every host rather than only the
//! ones with a device.
//!
//! THE PREDICATE IS `clear_glyph_images`, not "clears something". An earlier
//! version of this scan matched `glyphs.clear()` as a substring and so flagged
//! `set_font_subpixel`, whose CPU side clears `subpx_glyphs` — the LCD overlay,
//! a CPU-only path whose bytes never reach this atlas. That was a false alarm,
//! and a rule that cries wolf is one people turn off.

use std::collections::BTreeMap;

/// Bodies of every `    pub fn <name>(` in `path`, keyed by name: the lines from
/// the signature to the first line that is exactly `    }`. Crude on purpose —
/// it depends only on rustfmt's indentation, which `trustfmt` enforces.
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
        // Only the forwarding setters: a purely GPU-side one has no CPU twin to
        // disagree with.
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
        // An explicit, reasoned exemption is allowed — the same escape hatch
        // `activate_px` already documents for the content-addressed case — but it
        // has to be written down, not left as an absence.
        if gpu_body.contains("invalidate_atlas") || gpu_body.contains("Deliberately NO") {
            continue;
        }
        mismatched.push(name.clone());
    }

    // ANTI-VACUITY. If the scan stops matching (a rustfmt change, a refactor that
    // moves these out of the file), every list above goes empty and the
    // assertion below passes while checking NOTHING — which is precisely the
    // class of defect this file exists to catch, turned on itself. The floor is
    // well under the 13 pairs present when this was written, so ordinary
    // additions and removals do not trip it, but a scan that has stopped working
    // cannot masquerade as a clean one.
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
