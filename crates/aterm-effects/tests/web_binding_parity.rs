// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Drift guard for the duplicated web-binding modules.
//!
//! `aterm-wasm` (CPU bundle) and `aterm-gpu-web` (WebGPU) each carry a copy of
//! the same `#[wasm_bindgen]` surface modules (`effects_api`, `messages_api`,
//! `notifications_api`, `predict_api`, `scroll_input_api`,
//! `scrollback_tiers_api`). They are not single-sourced (a macro over
//! `#[wasm_bindgen] impl` blocks hurts error locality and IDE navigation), so
//! this test keeps the copies in line.
//!
//! Coverage is derived: the subject set is the filename intersection of the two
//! crates' `src/`, so a module duplicated into both is guarded as soon as it
//! lands. [`NOT_DUPLICATED`] excludes the one same-named file that is not a copy
//! (`lib.rs`); any new shared name defaults to covered.
//!
//! Scope: the production binding surface (the JS-visible face). Everything from
//! `mod tests` down is out of contract, because gpu-web's `render()` is
//! wasm-only and it has no `rgba()`, so its natively testable surface differs.
//!
//! Legitimate divergence is exactly three things, all normalized below: (1) the
//! host terminal type ident (`AtermTerminal` vs `AtermGpuTerminal`), (2) the
//! per-crate doc/comment wording, and (3) each [`FOLDED`] entry, with its reason.
//! Anything else that differs is drift and fails.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const WASM_CRATE: &str = "aterm-wasm";
const GPU_WEB_CRATE: &str = "aterm-gpu-web";

/// Same-named files in both crates that are not mirrored copies.
///
/// `lib.rs` is each crate's own entry point (a CPU raster pipeline in one, a
/// WebGPU one in the other), so it is not compared line for line; its JS
/// binding surface is still guarded by
/// [`wasm_lib_facade_surface_is_mirrored_by_gpu_web`].
const NOT_DUPLICATED: &[&str] = &["lib.rs"];

/// A floor on the derived set: discovery may only add, so deleting one copy
/// (e.g. `aterm-gpu-web/src/predict_api.rs`) fails instead of silently shrinking
/// coverage.
const KNOWN_DUPLICATED: &[&str] = &[
    "effects_api.rs",
    // The message band (docs/DESIGN-unified-messages-2026-09-21.md).
    "messages_api.rs",
    "notifications_api.rs",
    "predict_api.rs",
    "scroll_input_api.rs",
    "scrollback_tiers_api.rs",
];

/// `*_api.rs` binding modules that exist only in `aterm-wasm`, each with the
/// reason the GPU module has no copy. Enforced both ways by
/// [`every_cpu_only_web_binding_module_is_declared`]: a new CPU-only `*_api.rs`
/// must be listed here (or mirrored), and an entry that gains a gpu-web copy
/// must leave.
const CPU_ONLY_MODULES: &[(&str, &str)] = &[(
    "dirty_band_present_api.rs",
    "the RGBA band-scoped present (audit E3) expands the CPU rasterizer's \
         framebuffer into the page's ImageData; the GPU module presents through \
         WebGPU and has no RGBA framebuffer to scope.",
)];

/// One declared exception to parity: a source line that may appear in one copy
/// and not the other, plus the reason that is legitimate rather than drift.
struct Folded {
    /// The exact trimmed source line, never a prefix or pattern: a general
    /// "strip attributes" rule would hide `#[wasm_bindgen(getter)]` drift,
    /// which changes the JS surface.
    line: &'static str,
    /// Why the divergence is structural rather than a missed mirror-edit.
    why: &'static str,
}

/// The declared exceptions (fold #3). Each carries a written justification,
/// printed by the fold test beside the line it excuses.
const FOLDED: &[Folded] = &[Folded {
    line: r#"#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]"#,
    why: "aterm-gpu-web reaches these fns only from a `#[cfg(target_arch = \
          \"wasm32\")]` impl, so they are genuinely dead on a native build there; \
          aterm-wasm's callers are ungated and the same attribute would be a \
          stylistic lie. Mirroring it into aterm-wasm to satisfy a text diff is \
          exactly the stylistic `allow` house style forbids — declaring the \
          asymmetry here, with this reason, is the honest encoding.",
}];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn src_dir(crate_name: &str) -> PathBuf {
    workspace_root().join("crates").join(crate_name).join("src")
}

fn rs_file_names(dir: &Path) -> BTreeSet<String> {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rs"))
        .collect()
}

/// The duplicated-module set as a fact of the filesystem.
///
/// Both staleness assertions live here because every caller loops over this
/// set: a derivation that yields nothing would make each of them pass vacuously.
fn duplicated_modules() -> BTreeSet<String> {
    let wasm = src_dir(WASM_CRATE);
    let gpu = src_dir(GPU_WEB_CRATE);
    for (name, dir) in [(WASM_CRATE, &wasm), (GPU_WEB_CRATE, &gpu)] {
        assert!(
            dir.is_dir(),
            "{name}/src is missing — update WASM_CRATE/GPU_WEB_CRATE if the crate was renamed"
        );
    }
    let found: BTreeSet<String> = rs_file_names(&wasm)
        .intersection(&rs_file_names(&gpu))
        .filter(|n| !NOT_DUPLICATED.contains(&n.as_str()))
        .cloned()
        .collect();
    assert!(
        !found.is_empty(),
        "derived zero duplicated modules — the derivation broke, so every parity loop below \
         would pass by iterating nothing"
    );
    found
}

fn read_module(crate_name: &str, module: &str) -> String {
    let path = src_dir(crate_name).join(module);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Everything above the test module: the production binding surface.
///
/// Splits on `mod tests`, not `#[cfg(test)]`: the real gate is
/// `#[cfg(all(test, not(target_arch = "wasm32")))]`, which a `#[cfg(test)]`
/// matcher would miss and compare whole files.
fn production_slice(label: &str, src: &str) -> Vec<String> {
    let starts: Vec<usize> = src
        .lines()
        .enumerate()
        .filter(|(_, l)| l.trim_start().starts_with("mod tests"))
        .map(|(i, _)| i)
        .collect();
    assert!(
        starts.len() <= 1,
        "{label}: {} `mod tests` lines — the production/test split would be ambiguous",
        starts.len()
    );

    let mut kept: Vec<String> = match starts.first() {
        Some(&at) => src.lines().take(at).map(str::to_owned).collect(),
        None => src.lines().map(str::to_owned).collect(),
    };
    // Drop the gate attribute and blank lines the split left dangling.
    while kept
        .last()
        .is_some_and(|l| l.trim().is_empty() || l.trim_start().starts_with("#["))
    {
        kept.pop();
    }

    // Non-vacuity: the split must never swallow its own subject.
    assert!(!kept.is_empty(), "{label}: production slice is empty");
    assert!(
        kept.iter().any(|l| l.contains("impl ")),
        "{label}: production slice has no `impl ` — the split swallowed the subject"
    );
    kept
}

/// Fold the three legitimate divergences away, returning the comparable lines.
fn normalize(src: &str) -> Vec<String> {
    src.replace("AtermGpuTerminal", "AtermTerminal")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| !FOLDED.iter().any(|f| f.line == line.trim()))
        .map(str::to_owned)
        .collect()
}

fn normalize_slice(lines: &[String]) -> Vec<String> {
    normalize(&lines.join("\n"))
}

fn assert_parity(module: &str, wasm_src: &str, gpu_src: &str) {
    let wasm = normalize_slice(&production_slice(
        &format!("{module} (aterm-wasm)"),
        wasm_src,
    ));
    let gpu = normalize_slice(&production_slice(
        &format!("{module} (aterm-gpu-web)"),
        gpu_src,
    ));
    if wasm == gpu {
        return;
    }
    // Surface the first differing line so the mirror-the-edit fix is obvious.
    let first_diff = wasm
        .iter()
        .zip(gpu.iter())
        .enumerate()
        .find(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| {
            format!("first mismatch at kept-line {i}:\n  wasm:    {a}\n  gpu-web: {b}")
        })
        .unwrap_or_else(|| {
            format!(
                "one copy has {} kept lines, the other {} — a whole block was added/removed",
                wasm.len(),
                gpu.len()
            )
        });
    panic!(
        "web-binding module `{module}` has drifted between aterm-wasm and aterm-gpu-web.\n\
         The two copies' PRODUCTION surfaces must stay identical modulo the host type ident, \
         comments, and the wasm-gating allow(dead_code) — mirror the edit into both crates.\n\
         {first_diff}"
    );
}

#[test]
fn every_duplicated_web_binding_module_is_covered() {
    let found = duplicated_modules();
    let missing: Vec<&str> = KNOWN_DUPLICATED
        .iter()
        .copied()
        .filter(|m| !found.contains(*m))
        .collect();
    assert!(
        missing.is_empty(),
        "{missing:?} are in KNOWN_DUPLICATED but no longer duplicated — coverage would shrink \
         silently; remove them deliberately if the de-duplication was intended"
    );
}

#[test]
fn every_cpu_only_web_binding_module_is_declared() {
    let wasm = rs_file_names(&src_dir(WASM_CRATE));
    let gpu = rs_file_names(&src_dir(GPU_WEB_CRATE));
    for (module, why) in CPU_ONLY_MODULES {
        assert!(
            !why.is_empty(),
            "{module}: a CPU-only module carries its reason"
        );
        assert!(
            wasm.contains(*module),
            "{module} is declared CPU-only but {WASM_CRATE} has no such file — remove the entry"
        );
        assert!(
            !gpu.contains(*module),
            "{module} now exists in {GPU_WEB_CRATE}: the filename intersection guards it line \
             for line, so remove it from CPU_ONLY_MODULES"
        );
    }
    let undeclared: Vec<&String> = wasm
        .iter()
        .filter(|n| n.ends_with("_api.rs") && !gpu.contains(*n))
        .filter(|n| !CPU_ONLY_MODULES.iter().any(|(m, _)| m == n))
        .collect();
    assert!(
        undeclared.is_empty(),
        "{undeclared:?} exist in {WASM_CRATE} only: mirror them into {GPU_WEB_CRATE} or \
         declare them in CPU_ONLY_MODULES with the reason"
    );
}

#[test]
fn production_binding_surface_is_identical() {
    for module in duplicated_modules() {
        assert_parity(
            &module,
            &read_module(WASM_CRATE, &module),
            &read_module(GPU_WEB_CRATE, &module),
        );
    }
}

/// Mutual omission is still drift from the shared engine contract: both
/// bindings must export the PHOSPHOR (matrix rain) surface.
#[test]
fn effects_api_requires_matrix_rain_surface() {
    let wasm = read_module(WASM_CRATE, "effects_api.rs");
    let gpu = read_module(GPU_WEB_CRATE, "effects_api.rs");
    for symbol in [
        "set_matrix_rain_enabled",
        "matrix_rain_enabled",
        "set_matrix_rain(",
        "set_matrix_rain_reduced_motion",
        "set_effects_visibility",
        "note_matrix_rain_bell",
        "note_matrix_rain_alt_scroll",
        "note_matrix_rain_signal",
    ] {
        assert!(wasm.contains(symbol), "aterm-wasm missing {symbol}");
        assert!(gpu.contains(symbol), "aterm-gpu-web missing {symbol}");
    }
}

/// The PRISM WAKE output-streak symbol floor: line parity proves the copies
/// agree, not that they still carry the streak, so a symmetric deletion from
/// both bindings fails here.
#[test]
fn effects_api_requires_output_streak_surface() {
    let wasm = read_module(WASM_CRATE, "effects_api.rs");
    let gpu = read_module(GPU_WEB_CRATE, "effects_api.rs");
    for symbol in [
        "set_output_streak(",
        "set_output_streak_theme",
        "set_output_streak_reduced_motion",
        "note_output_streak_keystroke",
    ] {
        assert!(wasm.contains(symbol), "aterm-wasm missing {symbol}");
        assert!(gpu.contains(symbol), "aterm-gpu-web missing {symbol}");
    }
}

/// The message band's symbol floor: a symmetric deletion of an export from
/// both bindings fails here.
#[test]
fn messages_api_requires_band_surface() {
    let wasm = read_module(WASM_CRATE, "messages_api.rs");
    let gpu = read_module(GPU_WEB_CRATE, "messages_api.rs");
    for symbol in [
        "pub fn notice(",
        "pub fn messages_read(",
        "pub fn messages_entry(",
        "pub fn messages_page(",
        "pub fn messages_tick(",
        "pub fn messages_next_wake_ms(",
        "pub fn set_band_motion(",
        "pub fn set_band_visible(",
        "pub fn set_details_view(",
        "pub fn set_forced_colors(",
        "pub fn band_hover(",
        "pub fn band_press(",
        "pub fn band_press_acts(",
        "pub fn set_frame_period_ms(",
        "pub fn messages_log_since(",
        "pub fn messages_quit(",
        "pub fn messages_restore(",
        "pub fn band_rows(",
        "pub fn band_height_px(",
        "pub fn grid_top_px(",
    ] {
        assert!(wasm.contains(symbol), "aterm-wasm missing {symbol}");
        assert!(gpu.contains(symbol), "aterm-gpu-web missing {symbol}");
    }
}

// Guard the normalizer and the splitter themselves: a real code difference must
// not be masked by any fold.
#[test]
fn normalizer_does_not_mask_code_drift() {
    let a = "let x = self.effects.advance(now);";
    let b = "let x = self.effects.retreat(now);";
    assert_ne!(
        normalize(a),
        normalize(b),
        "normalizer must not hide code drift"
    );

    // ...but it must fold the two unconditional divergences (host type ident
    // and per-crate comment wording) to nothing.
    let wasm_ish = "/// CPU bundle.\nimpl AtermTerminal { fn f(&self) {} }";
    let gpu_ish = "/// WebGPU parity.\nimpl AtermGpuTerminal { fn f(&self) {} }";
    assert_eq!(
        normalize(wasm_ish),
        normalize(gpu_ish),
        "ident + comment normalization must equate the intentional divergences"
    );

    // Every declared fold must actually fold, at any indentation...
    for folded in FOLDED {
        assert_eq!(
            normalize(&format!("    {}\n    fn stamp(&self) {{}}", folded.line)),
            normalize("    fn stamp(&self) {}"),
            "declared FOLDED line must fold to nothing: {} (declared because: {})",
            folded.line,
            folded.why
        );
    }

    // ...and nothing else may. An unrelated attribute difference must fail,
    // and so must a near-miss of the declared line, which proves FOLDED is an
    // exact-literal list and not a pattern over `#[cfg_attr(...)]`.
    for undeclared in [
        "#[inline]",
        "#[wasm_bindgen(getter)]",
        r#"#[cfg_attr(not(target_arch = "wasm64"), allow(dead_code))]"#,
        r#"#[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]"#,
    ] {
        assert!(
            !FOLDED.iter().any(|f| f.line == undeclared),
            "{undeclared} is declared in FOLDED — pick a genuinely undeclared line for this case"
        );
        assert_ne!(
            normalize(&format!("    {undeclared}\n    fn stamp(&self) {{}}")),
            normalize("    fn stamp(&self) {}"),
            "undeclared attribute `{undeclared}` was folded away — the normalizer has become a \
             general attribute strip and can no longer see wasm_bindgen or cfg-gate drift"
        );
    }
}

// ---------------------------------------------------------------------------
// lib.rs binding-surface floor. `lib.rs` is in NOT_DUPLICATED (WebGPU vs CPU
// raster pipelines cannot be compared line for line), so its JS binding
// surface is checked here instead.
// ---------------------------------------------------------------------------

/// Does the contiguous `#[…]` attribute run immediately above `impl_idx` carry
/// a wasm-bindgen export? Admits both styles in use: the `cfg_attr` form and a
/// bare `#[wasm_bindgen]` paired with a separate `#[cfg(target_arch="wasm32")]`.
fn preceding_attrs_export_to_wasm(lines: &[&str], impl_idx: usize) -> bool {
    let mut k = impl_idx;
    while k > 0 {
        let t = lines[k - 1].trim();
        if !t.starts_with("#[") {
            return false;
        }
        if t == r#"#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]"# || t == "#[wasm_bindgen]" {
            return true;
        }
        k -= 1;
    }
    false
}

/// The JS-visible method surface of a crate's `Aterm(Gpu)Terminal` facade:
/// every `pub fn` inside a wasm-bindgen-attributed `impl` of that type. Other
/// impls are skipped. Block extent is the rustfmt column-0 `}` closer, not
/// brace depth, which JSON string literals in method bodies would corrupt.
fn wasm_facade_surface(src: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        let opener = t == "impl AtermTerminal {" || t == "impl AtermGpuTerminal {";
        if opener && preceding_attrs_export_to_wasm(&lines, i) {
            let mut j = i + 1;
            while j < lines.len() && lines[j] != "}" {
                let tt = lines[j].trim_start();
                if let Some(rest) = tt.strip_prefix("pub fn ") {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        out.insert(name);
                    }
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// wasm-facade exports legitimately absent from the gpu-web facade (CPU-raster
/// exports with no WebGPU counterpart), each with a reason and a
/// still-wasm-only check below.
const CPU_ONLY_FACADE_FNS: &[(&str, &str)] = &[(
    "rgba_ptr",
    "Byte offset of the CPU rasterizer's persistent `self.rgba` framebuffer in \
     wasm linear memory, for a zero-copy `putImageData`. gpu-web renders through \
     WebGPU to an offscreen target and exposes readback via `rgba()`/render_offscreen; \
     it holds no persistent CPU `rgba` buffer to point into, so the zero-copy \
     pointer variant is CPU-pipeline-only by construction.",
)];

/// Every method aterm-wasm exports on its `#[wasm_bindgen]` facade must also be
/// exported by aterm-gpu-web's, except the CPU-only carve-outs. One-directional:
/// gpu-web's GPU-only exports (`render_offscreen`/`adapter_info`/`gpu_ready`)
/// are not drift.
#[test]
fn wasm_lib_facade_surface_is_mirrored_by_gpu_web() {
    let wasm = wasm_facade_surface(&read_module(WASM_CRATE, "lib.rs"));
    let gpu = wasm_facade_surface(&read_module(GPU_WEB_CRATE, "lib.rs"));

    // Non-vacuity: a scanner that yields nothing must fail.
    assert!(
        wasm.len() > 50 && gpu.len() > 50,
        "facade-surface enumeration looks broken (wasm={}, gpu-web={}); the impl-opener \
         or column-0 `}}` heuristic stopped matching",
        wasm.len(),
        gpu.len()
    );

    let allow: BTreeSet<&str> = CPU_ONLY_FACADE_FNS.iter().map(|(n, _)| *n).collect();
    let missing: Vec<&String> = wasm
        .iter()
        .filter(|f| !gpu.contains(*f) && !allow.contains(f.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "aterm-wasm exports these `#[wasm_bindgen]` facade methods that aterm-gpu-web does \
         NOT: {missing:?}. Mirror each into gpu-web's facade impl, or — if it is a genuine \
         CPU-only export with no WebGPU counterpart — add it to CPU_ONLY_FACADE_FNS with a reason."
    );

    // A carve-out that is no longer a divergence is a hole: each must still
    // be wasm-only.
    for (name, _why) in CPU_ONLY_FACADE_FNS {
        assert!(
            wasm.contains(*name),
            "CPU_ONLY_FACADE_FNS lists `{name}` but aterm-wasm no longer exports it — \
             stale carve-out, delete it"
        );
        assert!(
            !gpu.contains(*name),
            "CPU_ONLY_FACADE_FNS lists `{name}` as CPU-only but aterm-gpu-web now exports it \
             too — the carve-out is obsolete, delete it"
        );
    }
}
