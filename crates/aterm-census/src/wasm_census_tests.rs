// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::scan_set::test_fixtures::synth_tree;

fn body(src: &str) -> Vec<String> {
    src.lines().map(|l| l.to_string()).collect()
}

#[test]
fn wasm_term_hop_flags_the_field_and_rebind_shapes() {
    let hz = wasm_term_hops(&body("        self.term.resize(rows, cols);"));
    assert_eq!(hz.len(), 1, "self.term.resize( must flag");
    assert_eq!(hz[0].method, "resize");
    let hz = wasm_term_hops(&body(
        "        let term = &mut self.term;\n        term.resize(rows, cols);",
    ));
    assert_eq!(hz.len(), 1, "a `term` rebind must still flag");
}

#[test]
fn wasm_term_hop_ignores_other_receivers_and_methods() {
    assert!(wasm_term_hops(&body("        self.rgba.resize(n, 0);")).is_empty());
    assert!(wasm_term_hops(&body("        t.resize(0, 0);")).is_empty());
    assert!(wasm_term_hops(&body("        self.term.process(bytes);")).is_empty());
    assert!(
        wasm_term_hops(&body(
            "        gpu.renderer.resize_surface(&mut gpu.surface, w, h);"
        ))
        .is_empty()
    );
    assert!(wasm_term_hops(&body("        subterm.resize(r, c);")).is_empty());
    assert!(wasm_term_hops(&body("        log(\"term.resize(1,2)\");")).is_empty());
}

#[test]
fn mask_gates_blank_native_only_blocks_and_test_fns() {
    let src = "fn ensure(&mut self) {\n    \
               #[cfg(not(target_arch = \"wasm32\"))]\n    \
               {\n        \
               let h = std::thread::spawn(move || work());\n    \
               }\n    \
               sync_fallback();\n\
               }\n";
    let masked = mask_unshipped(src);
    assert!(
        !masked.contains("thread::spawn"),
        "the native-only block must be blanked:\n{masked}"
    );
    assert!(
        masked.contains("sync_fallback"),
        "the shipped fallback must survive:\n{masked}"
    );
    let src = "#[test]\n#[timeout(10_000)]\nfn t() {\n    \
               let h = std::thread::spawn(move || {});\n}\nfn shipped() {}\n";
    let masked = mask_unshipped(src);
    assert!(!masked.contains("thread::spawn"), "masked:\n{masked}");
    assert!(masked.contains("fn shipped"), "masked:\n{masked}");
    let src = "#[cfg(windows)]\nfn native() {\n    \
               let h = std::thread::Builder::new();\n}\n\
               #[cfg(debug_assertions)]\nfn dev() {\n    \
               let h = std::thread::spawn(move || {});\n}\n\
               #[cfg(not(windows))]\nfn web() {\n    \
               let w = Worker::new(url);\n}\n";
    let masked = mask_unshipped(src);
    assert!(!masked.contains("thread::Builder"), "masked:\n{masked}");
    assert!(masked.contains("thread::spawn"), "masked:\n{masked}");
    assert!(masked.contains("Worker::new"), "masked:\n{masked}");
}

/// A minimal derivable workspace whose wasm closure carries the marked
/// sinks: the shared manifest fixture plus the wasm root crates, each
/// depending on aterm-grid.
fn synth_wasm_base() -> Vec<(String, String)> {
    let m = "// COST: UNBOUNDED"; // assembled so this line is never a marker
    let mut files = crate::scan_set::test_fixtures::workspace_manifests(&[
        ("aterm-core", ""),
        ("aterm-grid", ""),
    ]);
    for stub in [
        "crates/aterm-types/src/lib.rs",
        "crates/aterm-core/src/lib.rs",
        "crates/aterm-gui/src/main.rs",
    ] {
        files.push((stub.to_string(), "// stub\n".to_string()));
    }
    files.push((
        "crates/aterm-grid/src/grid/reflow.rs".to_string(),
        format!("{m}(scrollback-width-reflow)\npub fn resize_with_reflow_mode() {{}}\n"),
    ));
    files.push((
        "crates/aterm-grid/src/grid/scrollback_reflow.rs".to_string(),
        format!(
            "{m}(ring+tiered-history-lines)\npub fn take_scrollback_lines() {{}}\n\
             {m}(session-history-cells)\npub fn reflow_scrollback_lines() {{}}\n"
        ),
    ));
    files.push((
        "crates/aterm-grid/src/lib.rs".to_string(),
        "// stub\n".to_string(),
    ));
    for rc in ["aterm-wasm", "aterm-gpu-web"] {
        files.push((
            format!("crates/{rc}/Cargo.toml"),
            format!(
                "[package]\nname = \"{rc}\"\n[dependencies]\n\
                 aterm-grid = {{ workspace = true }}\n"
            ),
        ));
    }
    files
}

/// The resize shapes of the two terminal-bearing modules: `resize` detaches
/// via `resize_offloading_scrollback` and `pump_reflow` drives the rewrap.
fn synth_fixed_roots() -> Vec<(String, String)> {
    vec![
        (
            "crates/aterm-wasm/src/lib.rs".to_string(),
            "pub fn resize(&mut self, rows: u16, cols: u16) {\n    \
             if let Some(pending) = self.term.resize_offloading_scrollback(rows, cols) {\n        \
             self.pending_reflow = Some(pending);\n    }\n}\n\
             pub fn pump_reflow(&mut self) -> bool {\n    \
             let Some(pending) = self.pending_reflow.take() else { return false; };\n    \
             self.term.finish_resize_offload(pending.reflow());\n    true\n}\n\
             pub fn process(&mut self, bytes: &[u8]) {\n    self.term.process(bytes);\n}\n"
                .to_string(),
        ),
        (
            "crates/aterm-gpu-web/src/lib.rs".to_string(),
            "pub fn resize(&mut self, rows: u16, cols: u16) {\n    \
             if let Some(pending) = self.term.resize_offloading_scrollback(rows, cols) {\n        \
             self.pending_reflow = Some(pending);\n    }\n}\n\
             pub fn pump_reflow(&mut self) -> bool {\n    \
             let Some(pending) = self.pending_reflow.take() else { return false; };\n    \
             self.term.finish_resize_offload(pending.reflow());\n    true\n}\n"
                .to_string(),
        ),
    ]
}

#[test]
fn synthetic_wasm_tree_with_offloaded_resize_is_green_with_zero_findings() {
    let mut files = synth_wasm_base();
    files.extend(synth_fixed_roots());
    let root = synth_tree("aterm-wasm-census", "green", &files);
    let out = run_wasm_census(&root);
    assert!(out.ok, "expected GREEN, got:\n{}", out.log);
    assert!(
        out.log.contains("VACUOUS") && out.log.contains("wasm32-unknown-unknown"),
        "the threading posture must be printed on GREEN; log:\n{}",
        out.log
    );
}

/// Putting a synchronous `self.term.resize(..)` back into a resize export
/// is an immediate OB-10 red.
#[test]
fn synthetic_reintroduced_sync_resize_is_red_ob10() {
    let mut files = synth_wasm_base();
    files.extend(synth_fixed_roots().into_iter().skip(1)); // gpu-web stays fixed
    files.push((
        "crates/aterm-wasm/src/lib.rs".to_string(),
        "pub fn resize(&mut self, rows: u16, cols: u16) {\n    \
         self.term.resize(rows, cols);\n}\n"
            .to_string(),
    ));
    let root = synth_tree("aterm-wasm-census", "red-reintroduced", &files);
    let out = run_wasm_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(
        out.log.contains("[OB-10]") && out.log.contains("crates/aterm-wasm/src/lib.rs:1"),
        "a reintroduced sync resize must be RED at its site; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("HOW TO REPAIR") && out.log.contains("COOPERATIVE OFFLOAD"),
        "the wasm repair menu must be printed; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_new_unregistered_reach_is_red_ob10_with_path() {
    let mut files = synth_wasm_base();
    files.extend(synth_fixed_roots());
    files.push((
        "crates/aterm-gpu-web/src/extra.rs".to_string(),
        "pub fn set_columns(&mut self, cols: u16) {\n    apply_cols(self, cols);\n}\n\
         fn apply_cols(s: &mut S, cols: u16) {\n    s.term.resize(s.rows, cols);\n}\n"
            .to_string(),
    ));
    let root = synth_tree("aterm-wasm-census", "red-ob10", &files);
    let out = run_wasm_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(out.log.contains("[OB-10]"), "log:\n{}", out.log);
    assert!(
        out.log.contains("set_columns -> apply_cols"),
        "the diagnostic must print the entry-point -> hazard path; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("HOW TO REPAIR") && out.log.contains("COOPERATIVE OFFLOAD"),
        "the wasm repair menu must be printed; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_direct_sink_call_is_red_ob10() {
    let mut files = synth_wasm_base();
    files.extend(synth_fixed_roots());
    files.push((
        "crates/aterm-wasm/src/direct.rs".to_string(),
        "pub fn rewrap_now(&mut self) {\n    \
         let lines = reflow_scrollback_lines(&lines, cols);\n}\n"
            .to_string(),
    ));
    let root = synth_tree("aterm-wasm-census", "red-direct", &files);
    let out = run_wasm_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(
        out.log.contains("[OB-10]") && out.log.contains("DIRECT call"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_unmasked_spawn_in_closure_is_red_ob12_and_gated_spawn_is_not() {
    let mut files = synth_wasm_base();
    files.extend(synth_fixed_roots());
    files.push((
        "crates/aterm-grid/src/worker.rs".to_string(),
        "pub fn kick() {\n    std::thread::spawn(|| {});\n}\n".to_string(),
    ));
    let root = synth_tree("aterm-wasm-census", "red-ob12", &files);
    let out = run_wasm_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(
        out.log.contains("[OB-12]") && out.log.contains("crates/aterm-grid/src/worker.rs"),
        "log:\n{}",
        out.log
    );
    let mut files = synth_wasm_base();
    files.extend(synth_fixed_roots());
    files.push((
        "crates/aterm-grid/src/worker.rs".to_string(),
        "#[cfg(not(target_arch = \"wasm32\"))]\n\
         pub fn kick() {\n    std::thread::spawn(|| {});\n}\n"
            .to_string(),
    ));
    let root = synth_tree("aterm-wasm-census", "green-gated-spawn", &files);
    let out = run_wasm_census(&root);
    assert!(
        out.ok,
        "a cfg(not(wasm32))-gated spawn must not trip OB-12; log:\n{}",
        out.log
    );
}

/// A crate the closure reaches only through a target table false on
/// wasm32 (aterm-uds's place: aterm-shell-integration's `cfg(any(unix,
/// windows))` edge) is no part of the wasm closure: its spawn trips
/// nothing, and the log names the edge left out.
#[test]
fn synthetic_spawn_behind_a_native_target_table_is_not_in_the_closure() {
    let tree = |grid_table: &str| {
        let mut files = synth_wasm_base();
        files.extend(synth_fixed_roots());
        for (rel, text) in &mut files {
            if rel == "Cargo.toml" {
                *text = text.replace(
                    "[workspace.dependencies]\n",
                    "[workspace.dependencies]\n\
                     aterm-native = { path = \"crates/aterm-native\" }\n",
                );
            } else if rel == "crates/aterm-grid/Cargo.toml" {
                text.push_str(&format!(
                    "{grid_table}\naterm-native = {{ workspace = true }}\n"
                ));
            }
        }
        files.push((
            "crates/aterm-native/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-native\"\n".to_string(),
        ));
        files.push((
            "crates/aterm-native/src/lib.rs".to_string(),
            "pub fn watch() {\n    std::thread::spawn(|| {});\n}\n".to_string(),
        ));
        files
    };
    let gated = tree("[target.'cfg(any(unix, windows))'.dependencies]");
    let out = run_wasm_census(&synth_tree("aterm-wasm-census", "native-table", &gated));
    assert!(
        out.ok,
        "a crate only a native target links is out of the closure; log:\n{}",
        out.log
    );
    assert!(
        out.log
            .contains("crates/aterm-grid -> aterm-native [target.cfg(any(unix, windows))]"),
        "the edge left out is named; log:\n{}",
        out.log
    );
    for (name, table) in [
        ("plain-table", "[dependencies]"),
        (
            "wasm-table",
            "[target.'cfg(target_arch = \"wasm32\")'.dependencies]",
        ),
        (
            "feature-table",
            "[target.'cfg(feature = \"x\")'.dependencies]",
        ),
    ] {
        let out = run_wasm_census(&synth_tree("aterm-wasm-census", name, &tree(table)));
        assert!(!out.ok, "CONTROL {name}: expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("[OB-12]") && out.log.contains("crates/aterm-native/src/lib.rs"),
            "CONTROL {name}: log:\n{}",
            out.log
        );
    }
}

/// A file only a `mod` false on wasm32 loads (aterm-gpu's place: its
/// `#[cfg(target_os = "macos")] mod metal;`) is no part of the swept
/// closure: its spawn trips nothing, without a gate of its own, and the
/// log names the cut.
#[test]
fn synthetic_spawn_in_a_file_only_a_false_mod_loads_is_not_swept() {
    let tree = |lib: &str| {
        let mut files = synth_wasm_base();
        files.extend(synth_fixed_roots());
        for (rel, text) in &mut files {
            if rel == "crates/aterm-grid/src/lib.rs" {
                *text = lib.to_string();
            }
        }
        files.push((
            "crates/aterm-grid/src/metal.rs".to_string(),
            "pub fn acquire() {\n    std::thread::Builder::new();\n}\n".to_string(),
        ));
        files
    };
    let gated = tree("// The Metal arm.\n#[cfg(target_os = \"macos\")]\nmod metal;\n");
    let out = run_wasm_census(&synth_tree("aterm-wasm-census", "false-mod", &gated));
    assert!(
        out.ok,
        "a file only a macOS `mod` loads is out of the sweep; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains(
            "crates/aterm-grid/src/lib.rs:3 `mod metal;` under #[cfg(target_os = \"macos\")]"
        ),
        "the cut is named; log:\n{}",
        out.log
    );
    for (name, lib) in [
        ("feature-mod", "#[cfg(feature = \"metal\")]\nmod metal;\n"),
        (
            "unplaced-tree",
            "#[cfg(target_os = \"macos\")]\nmod metal;\ninclude!(concat!(\"gen\", \".rs\"));\n",
        ),
    ] {
        let out = run_wasm_census(&synth_tree("aterm-wasm-census", name, &tree(lib)));
        assert!(!out.ok, "CONTROL {name}: expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("[OB-12]") && out.log.contains("crates/aterm-grid/src/metal.rs:2"),
            "CONTROL {name}: log:\n{}",
            out.log
        );
    }
}

#[test]
fn synthetic_missing_root_crate_fails_closed_ob8() {
    let files: Vec<(String, String)> = synth_wasm_base()
        .into_iter()
        .filter(|(rel, _)| !rel.starts_with("crates/aterm-gpu-web/"))
        .chain(synth_fixed_roots().into_iter().take(1))
        .collect();
    let root = synth_tree("aterm-wasm-census", "red-ob8", &files);
    let out = run_wasm_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(
        out.log.contains("[OB-8] SCAN-SET DERIVATION FAILED"),
        "log:\n{}",
        out.log
    );
}
