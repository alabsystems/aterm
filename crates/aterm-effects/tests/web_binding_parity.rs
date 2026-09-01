// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Drift guard for the duplicated web-binding modules.
//!
//! `aterm-wasm` (CPU/self-contained bundle) and `aterm-gpu-web` (WebGPU parity)
//! each carry a copy of FIVE `#[wasm_bindgen]` surface modules — `effects_api`,
//! `notifications_api`, `predict_api`, `scroll_input_api`,
//! `scrollback_tiers_api`. They deliberately are NOT single-sourced (a macro
//! over `#[wasm_bindgen] impl` blocks degrades error locality and IDE
//! navigation), so the ONLY thing keeping the copies honest is this test.
//!
//! COVERAGE IS DERIVED, NOT DECLARED. The subject set is the filename
//! intersection of the two crates' `src/`, so a module duplicated into both is
//! guarded the instant it lands. The predecessor of this file hand-listed its
//! two subjects and named a third that never existed (`scene_surface_parity`
//! had an EMPTY body — a green test asserting nothing); three modules were
//! added after it and all three arrived unguarded. Filename identity is a
//! heuristic, not a law, so [`NOT_DUPLICATED`] carves out the one counterexample
//! (`lib.rs` is each crate's own entry point, 4415 vs 3414 lines). That list is
//! hand-maintained — but it fails LOUD (a new shared name defaults to covered),
//! which is the property the old ceiling lacked.
//!
//! SCOPE: the production binding surface — the JS-visible face, where drift is
//! catastrophic. Everything from `mod tests` down is out of contract, because
//! gpu-web's `render()` is wasm-only and it has no `rgba()`, so its natively
//! testable surface genuinely differs. That carve-out is NOT a clean bill of
//! health: gpu-web's `scrollback_tiers_api` copy is missing five `#[test]`s the
//! wasm copy has, most of which look mirrorable (budget/limit tests over a
//! production surface this guard proves matches line for line). That is real
//! coverage loss, tracked separately — [`TEST_FLOORS`] pins it so it cannot
//! quietly widen.
//!
//! Legitimate divergence is exactly THREE things, all normalized below: (1) the
//! host terminal type ident (`AtermTerminal` vs `AtermGpuTerminal`), (2) the
//! per-crate doc/comment wording, and (3) whatever [`FOLDED`] declares, entry by
//! entry, each with a written reason and each required to still resolve.
//! Anything else that differs is unintended drift and MUST fail.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const WASM_CRATE: &str = "aterm-wasm";
const GPU_WEB_CRATE: &str = "aterm-gpu-web";

/// Same-named files in both crates that are NOT mirrored copies. Filename
/// identity implies duplication for every file here EXCEPT these.
///
/// `lib.rs` is each crate's own entry point, not a shared binding surface: it
/// wires a CPU raster pipeline in one crate and a WebGPU one in the other, and
/// the two are ~4400 vs ~3400 lines. Guarding them against each other would be
/// asserting a falsehood, so the exclusion is the honest call — but note it is a
/// CEILING, the only hand-typed one here, and every other shared name defaults
/// to covered. It is NO LONGER a total blind spot: `lib.rs` stays line-excluded,
/// but its JS binding SURFACE is guarded by
/// [`wasm_lib_facade_surface_is_mirrored_by_gpu_web`] (audit-2 item 14 — the gap
/// that let five wasm-only facade methods go unmirrored). Only line-level parity
/// is waived here, not surface parity.
const NOT_DUPLICATED: &[&str] = &["lib.rs"];

/// A FLOOR on the derived set, never a ceiling: discovery may only add. Without
/// it, deleting `aterm-gpu-web/src/predict_api.rs` would silently shrink
/// coverage back toward the original bug instead of failing. Removing an entry
/// is therefore a deliberate, reviewable act.
const KNOWN_DUPLICATED: &[&str] = &[
    "effects_api.rs",
    "notifications_api.rs",
    "predict_api.rs",
    "scroll_input_api.rs",
    "scrollback_tiers_api.rs",
];

/// Per-copy `#[test]` floors: `(module, wasm count, gpu-web count)`. Test
/// bodies are out of the parity contract, which would otherwise make "reconcile
/// by deleting the tests" a green path. These pin the counts so the existing
/// gpu-web shortfall cannot widen unnoticed.
const TEST_FLOORS: &[(&str, usize, usize)] = &[
    ("effects_api.rs", 0, 0),
    ("notifications_api.rs", 5, 5),
    ("predict_api.rs", 11, 11),
    ("scroll_input_api.rs", 14, 14),
    ("scrollback_tiers_api.rs", 7, 2),
];

/// One declared exception to parity: a source line that may appear in one copy
/// and not the other, plus the reason that is legitimate rather than drift.
struct Folded {
    /// The EXACT trimmed source line. Never a prefix, never a pattern. A regex
    /// over `#[cfg_attr(...)]` — or any general "strip attributes" rule — would
    /// make this guard blind to `#[wasm_bindgen(getter)]` drift, which silently
    /// changes the JS surface. Exactness is the whole safety property.
    line: &'static str,
    /// Why the divergence is structural rather than a missed mirror-edit. Read
    /// by nothing at runtime; read by every reviewer who asks "can I add one?".
    why: &'static str,
}

/// The declared exceptions (fold #3), in the shape `xtask`'s `WITNESS_REGISTRY`
/// and census OB-3 use: every allowlist entry carries a written justification
/// AND must still resolve. [`every_folded_line_still_resolves`] enforces the
/// second half, so an exception cannot outlive the divergence it excuses and sit
/// there waiting to swallow a real one.
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
/// Both staleness assertions live HERE, not in one test, because every caller is
/// a `for module in duplicated_modules()` loop: a derivation that silently
/// yields nothing turns each of them into a vacuous pass, which is the precise
/// failure mode (a green test asserting nothing) this file was rewritten to end.
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

/// Everything above the test module — the production binding surface.
///
/// Splits on `mod tests`, NOT on `#[cfg(test)]`: the real gate is
/// `#[cfg(all(test, not(target_arch = "wasm32")))]`, so a `#[cfg(test)]` matcher
/// silently fails to split and compares whole files while looking correct.
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

fn count_tests(src: &str) -> usize {
    src.lines()
        .filter(|l| {
            let t = l.trim();
            t == "#[test]" || t == "#[tokio::test]"
        })
        .count()
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
fn production_binding_surface_is_identical() {
    for module in duplicated_modules() {
        assert_parity(
            &module,
            &read_module(WASM_CRATE, &module),
            &read_module(GPU_WEB_CRATE, &module),
        );
    }
}

/// Mutual omission is still drift from the shared engine contract. Keep the
/// host-facing PHOSPHOR surface explicit so both bindings cannot "agree" by
/// silently dropping matrix rain again.
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

/// PRISM WAKE's twin of the rain symbol floor, and it exists for exactly the
/// same reason: line parity proves the two copies AGREE, never that they agree
/// on something. A symmetric deletion — dropping the streak from both bindings
/// in one commit — reads as perfectly parallel and would sail through every
/// other test in this file.
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

/// Test bodies are out of the parity contract, so nothing else stops a copy
/// from being reconciled by DELETING its tests.
#[test]
fn tests_module_presence_and_count_hold_their_floor() {
    for module in duplicated_modules() {
        let wasm = read_module(WASM_CRATE, &module);
        let gpu = read_module(GPU_WEB_CRATE, &module);
        let has = |s: &str| s.lines().any(|l| l.trim_start().starts_with("mod tests"));
        assert_eq!(
            has(&wasm),
            has(&gpu),
            "{module}: one copy has a `mod tests` and the other does not"
        );

        let Some(&(_, wasm_floor, gpu_floor)) = TEST_FLOORS.iter().find(|(m, _, _)| *m == module)
        else {
            panic!("{module} is duplicated but has no TEST_FLOORS entry — add one");
        };
        assert!(
            count_tests(&wasm) >= wasm_floor,
            "{module}: aterm-wasm has {} tests, floor is {wasm_floor}",
            count_tests(&wasm)
        );
        assert!(
            count_tests(&gpu) >= gpu_floor,
            "{module}: aterm-gpu-web has {} tests, floor is {gpu_floor}",
            count_tests(&gpu)
        );
    }
}

/// OB-3's second half: an allowlist entry must still RESOLVE.
///
/// A `FOLDED` line whose divergence has been reconciled away is not harmless —
/// it keeps a hole open in the normalizer, so the NEXT copy to grow that exact
/// line diverges silently. An exception must not outlive its reason, and the
/// only way to know is to go look.
#[test]
fn every_folded_line_still_resolves() {
    // Search the production slices, not whole files: a fold that only matches
    // inside `mod tests` is doing no work for the parity contract either.
    let slices: Vec<String> = duplicated_modules()
        .iter()
        .flat_map(|module| {
            [WASM_CRATE, GPU_WEB_CRATE].map(|krate| {
                let src = read_module(krate, module);
                production_slice(&format!("{module} ({krate})"), &src).join("\n")
            })
        })
        .collect();

    for folded in FOLDED {
        assert!(
            slices
                .iter()
                .any(|s| s.lines().any(|l| l.trim() == folded.line)),
            "stale FOLDED entry — `{}` no longer appears in any guarded production slice, so \
             the normalizer is carrying a hole for a divergence that is gone. Delete it.\n\
             Its declared reason was: {}",
            folded.line,
            folded.why
        );
    }
}

// Guard the normalizer and the splitter themselves: a real code difference must
// NOT be masked by any fold (otherwise a green parity test is vacuous).
#[test]
fn normalizer_does_not_mask_code_drift() {
    let a = "let x = self.effects.advance(now);";
    let b = "let x = self.effects.retreat(now);";
    assert_ne!(
        normalize(a),
        normalize(b),
        "normalizer must not hide code drift"
    );

    // ...but it MUST fold the two unconditional divergences — host type ident
    // and per-crate comment wording — to nothing.
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
            "declared FOLDED line must fold to nothing: {}",
            folded.line
        );
    }

    // ...and NOTHING ELSE may. These are the non-vacuity ratchet: an undeclared
    // attribute difference has to fail, or the guard is decoration. The first is
    // an unrelated attribute; the second is a near-miss of the declared line,
    // which is what proves FOLDED is an exact-literal list and not a pattern
    // over `#[cfg_attr(...)]` — a pattern would wave through a real gate change.
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

#[test]
fn production_slice_drops_the_test_module() {
    let src = "impl X {}\n\n#[cfg(all(test, not(target_arch = \"wasm32\")))]\nmod tests {\n    fn t() {}\n}\n";
    assert_eq!(
        production_slice("synthetic", src),
        vec!["impl X {}".to_owned()],
        "the split must keep the impl and drop the gate, blank line, and test module"
    );
}

// ---------------------------------------------------------------------------
// lib.rs binding-SURFACE floor (audit-2 item 14). `lib.rs` stays in
// NOT_DUPLICATED (two different pipelines, WebGPU vs CPU raster, cannot be
// compared line-for-line) — but that made its JS binding surface a blind spot:
// `process_str`, `last_command_output`, `row_range_json`, `search_summary` and
// `search_index_release` were added to aterm-wasm and never mirrored into
// aterm-gpu-web, uncaught. This restores a floor at the SURFACE level.
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
/// every `pub fn` inside a wasm-bindgen-attributed `impl` of that facade type.
/// Non-`wasm_bindgen` impls (native test helpers, private-method impls) are
/// skipped, so an internal `pub fn` never enters the surface. Block extent is
/// the rustfmt column-0 `}` closer — never brace depth, which the JSON string
/// literals in the method bodies would corrupt.
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

/// wasm-facade exports LEGITIMATELY absent from the gpu-web facade — each a
/// CPU-raster export with no WebGPU counterpart. Shaped like the `#[test]`
/// floors: a reason plus a still-resolves check below, so a carve-out cannot
/// outlive its cause.
const CPU_ONLY_FACADE_FNS: &[(&str, &str)] = &[(
    "rgba_ptr",
    "Byte offset of the CPU rasterizer's persistent `self.rgba` framebuffer in \
     wasm linear memory, for a zero-copy `putImageData`. gpu-web renders through \
     WebGPU to an offscreen target and exposes readback via `rgba()`/render_offscreen; \
     it holds no persistent CPU `rgba` buffer to point into, so the zero-copy \
     pointer variant is CPU-pipeline-only by construction.",
)];

/// Every method aterm-wasm exports on its `#[wasm_bindgen]` facade must also be
/// exported by aterm-gpu-web's, save the reasoned CPU-only carve-outs.
/// Deliberately ONE-directional — gpu-web adds GPU-only exports
/// (`render_offscreen`/`adapter_info`/`gpu_ready`) wasm has no counterpart for,
/// and those are not drift.
#[test]
fn wasm_lib_facade_surface_is_mirrored_by_gpu_web() {
    let wasm = wasm_facade_surface(&read_module(WASM_CRATE, "lib.rs"));
    let gpu = wasm_facade_surface(&read_module(GPU_WEB_CRATE, "lib.rs"));

    // Non-vacuity: a scanner that yields nothing must FAIL, not pass silently.
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

    // A carve-out that is no longer a divergence is a hole (mirrors the FOLDED
    // still-resolves discipline): require each to STILL be wasm-only.
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
