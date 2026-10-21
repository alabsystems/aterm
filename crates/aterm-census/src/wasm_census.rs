// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Wasm-process census (OB-8..OB-12): the L0 freeze obligation for the wasm
//! renderer modules (`crates/aterm-wasm` CPU, `crates/aterm-gpu-web`
//! GPU/WebGL2) that load into the Electron renderer page.
//!
//! * The process is single-threaded by target: the modules build only for
//!   `wasm32-unknown-unknown` with no `+atomics` and no shared memory, so a
//!   `std::sync::Mutex` is uncontended and ABBA deadlock is impossible. The
//!   lock-order obligation is vacuous here; that posture is printed every run
//!   and kept honest by the OB-12 tripwire.
//! * L0 freeze does transfer: the JS event loop hosting the module (page main
//!   thread or the engine's Web Worker) is liveness-critical, and a long
//!   synchronous export blocks it. The closure shares `aterm-grid`, so the same
//!   `// COST: UNBOUNDED(<dim>)` sinks apply, with no worker thread to offload
//!   to. `resize` exports detach via `resize_offloading_scrollback` and the
//!   rewrap runs in a later host task (`pump_reflow`).
//!
//! Obligations (fail-closed; `[OB-n]` tags in the diagnostics):
//!
//! * OB-8 scan-set + marker coherence: the closure is derived from the root
//!   crates' manifests (`scan_set::derive_process_scan_set`; an underivable
//!   workspace stops the census). Every `COST: UNBOUNDED` marker in it maps to
//!   a registered sink, and every registered sink defined inside it is marked.
//! * OB-9 entry-point roots: each wasm root crate yields at least one public
//!   fn (every export, aterm-wasm-abi's `#[export]` in both modules, is a
//!   `pub fn`, a sound over-approximation), so a
//!   parse regression cannot empty the surface.
//! * OB-10 no entry point synchronously reaches an unbounded sink, via the
//!   engine `term` field (`self.term.resize(..)`) or a direct sink call.
//! * OB-12 single-threaded tripwire: the shipped closure contains no
//!   thread-spawn vocabulary (`thread::spawn(`, `thread::Builder`,
//!   `Worker::new(`) outside test and native-only items. Shipping wasm
//!   threads fails the census until lock order is extended to this process.
//!
//! The closure over-approximates the browser bundle: the derivation does not
//! evaluate cfg predicates, so a `[target.'cfg(..)'.dependencies]` edge counts
//! on every platform (missing linked source is unsafe, scanning unlinked
//! source is not). `aterm-uds` (via aterm-shell-integration's `cfg(any(unix,
//! windows))` edge) and `aterm-objc` (via aterm-gpu's `cfg(target_os =
//! "macos")` edge) are scanned though wasm32 never links them, which is why
//! aterm-gpu's Metal `AcquireWorker` spawn carries a
//! `#[cfg(not(target_arch = "wasm32"))]` gate that only OB-12 needs.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::lock_order::{mask_gated_items, mask_literals};
use crate::{
    CensusOutcome, Floor, GuiFn, UNBOUNDED_REGISTRY, UNBOUNDED_TERM_METHODS, callee_names,
    collect_closure_markers, collect_rs_files, ident_ending_at, is_test_file, parse_source_fns,
    scan_set, sink_dim, sink_file, strip_line_comment,
};

/// The wasm renderer modules (cdylib crates loaded into the Electron renderer
/// page). The closure is the union of their manifest closures, since a hazard
/// in any module freezes the same event loop.
pub const WASM_ROOT_CRATES: &[&str] = &["crates/aterm-gpu-web", "crates/aterm-wasm"];

/// Item gates whose code never compiles into the shipped wasm32 module
/// (test and native-only items), masked before the walk and the OB-12
/// sweep. Exact-match: an unrecognized cfg spelling is not masked.
const WASM_UNSHIPPED_GATES: &[&str] = &[
    "#[cfg(test)]",
    "#[test]",
    "#[cfg(not(target_arch = \"wasm32\"))]",
];

/// Thread-spawn vocabulary that breaks the single-threaded posture (OB-12).
/// `thread::spawn(` also matches `std::`/`wasm_thread::` forms;
/// `Worker::new(` catches a worker spawned from inside the module (one spawned
/// from JS is a separate instance).
const SPAWN_TOKENS: &[&str] = &["thread::spawn(", "thread::Builder", "Worker::new("];

/// The honest limits of this census, printed verbatim in every RED diagnostic.
pub const WASM_PRECISION_NOTE: &str = "    PRECISION / SCOPE (the honest limits of this census):
      - LEXICAL, name-based: the same fn segmenter and `<ident>(` call matching
        as the main-loop census; no type information. ENTRY POINTS are the root
        crates' `pub fn`s — a sound over-approximation of the export set
        (aterm-wasm-abi's `#[export]`: every export is a pub fn).
        Same-named fns merge.
      - THE TERMINAL HOP is the `term` naming discipline: a call
        `<...>.term.<m>(` / `term.<m>(` with `<m>` in the unbounded-method
        registry is the modeled engine hop (the wasm modules hold the engine as
        a direct `term` field — there is no lock here to key on). A field
        renamed out of this vocabulary hides the hop from the walk.
      - SCOPE: the call graph covers the root crates' non-test src ONLY (masked
        to the shipped wasm32 surface: #[cfg(test)]/#[test]/
        #[cfg(not(target_arch = \"wasm32\"))] items blanked); deeper reaches are
        pinned by the shared `// COST: UNBOUNDED(<dim>)` markers swept over the
        DERIVED wasm-process closure. The OB-12 spawn sweep covers the whole
        derived closure the same masked way.
";

/// The thread/lock posture of the wasm process, printed every run: lock order
/// is vacuous here, with the OB-12 tripwire as evidence.
pub const WASM_THREADING_POSTURE: &str =
    "    THREAD/LOCK POSTURE of the wasm process (why the LOCK-ORDER obligation is
    VACUOUS here — the evidence, re-verified every run by OB-12):
      - TARGET: wasm32-unknown-unknown (the only target these crates ship for;
        the `wasm` perf lane and the wasm cells build exactly that), with NO
        `+atomics` target feature and no SharedArrayBuffer shared memory in any
        build config.
        std threads DO NOT EXIST at runtime on this target: there is no second
        thread of execution inside a module instance.
      - Each module instance is bound to exactly ONE JS agent
        (the page main thread, or the one Web Worker the host constructs the
        engine in — the crates' own docs describe that split at the JS
        message-passing level, never shared memory). A second instance in
        another worker is a separate linear memory — a separate process in
        census terms.
      - Locks in this closure (e.g. the notifications Arc<Mutex>, present only
        to satisfy Send bounds on engine callbacks) are therefore uncontended
        BY CONSTRUCTION: ABBA deadlock needs two threads holding in opposite
        orders — impossible with one; a re-entrant self-lock on this target
        panics loudly rather than blocking. Running the lock-order graph here
        would be dead machinery producing a vacuous GREEN; this posture line
        plus the OB-12 tripwire is the honest replacement.
      - The obligation that DOES transfer is L0-FREEZE: the hosting JS event
        loop is the liveness-critical context (the browser-tab analog of the
        winit main thread) — OB-10's jurisdiction.
";

/// A synchronous engine-hop call found in a (masked) fn body.
struct WasmTermHop {
    method: String,
    line: String,
}

/// Detect synchronous calls to an unbounded Terminal method: the receiver
/// immediately before `.{method}(` must be exactly `term` (matches
/// `self.term.resize(..)` and a `term` local; not `t.resize(`,
/// `self.rgba.resize(`, `.resize_surface(`).
fn wasm_term_hops(body: &[String]) -> Vec<WasmTermHop> {
    let mut out = Vec::new();
    for raw in body {
        let line = mask_literals(raw);
        let bytes = line.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b != b'.' {
                continue;
            }
            if ident_ending_at(&line, i) != Some("term") {
                continue;
            }
            let after = &line[i + 1..];
            let method: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !method.is_empty()
                && after[method.len()..].starts_with('(')
                && UNBOUNDED_TERM_METHODS.contains(&method.as_str())
            {
                out.push(WasmTermHop {
                    method,
                    line: raw.trim().to_string(),
                });
            }
        }
    }
    out
}

/// Non-test `*.rs` files under `<root>/<crate_dir>/src`, sorted.
fn crate_source_files(root: &Path, crate_dir: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join(crate_dir).join("src"), &mut files);
    files.retain(|p| !is_test_file(p));
    files.sort();
    files
}

/// One OB-12 spawn-vocabulary hit in the shipped wasm closure.
struct SpawnSite {
    span: String,
    token: &'static str,
    line: String,
}

/// Sweep the derived wasm closure for thread-spawn vocabulary surviving the
/// shipped-surface masking (comments stripped, string/char literals masked).
fn spawn_sites(root: &Path, scan_dirs: &[String]) -> Vec<SpawnSite> {
    let mut hits = Vec::new();
    let mut files = Vec::new();
    for dir in scan_dirs {
        let _ = collect_rs_files(&root.join(dir), &mut files);
    }
    files.retain(|p| !is_test_file(p));
    files.sort();
    for file in files {
        let Ok(text) = crate::tree::read_to_string(&file) else {
            continue;
        };
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        let masked = mask_gated_items(&text, WASM_UNSHIPPED_GATES);
        for (i, raw) in masked.lines().enumerate() {
            let line = mask_literals(strip_line_comment(raw));
            for token in SPAWN_TOKENS {
                if line.contains(token) {
                    hits.push(SpawnSite {
                        span: format!("{rel}:{}", i + 1),
                        token,
                        line: raw.trim().to_string(),
                    });
                }
            }
        }
    }
    hits
}

/// Append the why + repair block for an OB-10 violation (the wasm repair menu
/// differs from the native one: there is no worker thread to offload to).
fn append_wasm_why_and_repair(log: &mut String) {
    let _ = writeln!(
        log,
        "    WHY THIS IS THE L0-FREEZE ANALOG: the wasm module runs entirely ON one JS\n\
         \x20        event loop (page main thread or the engine's Web Worker). A synchronous\n\
         \x20        O(history) export body blocks input, rendering, and every other task on\n\
         \x20        that loop until it finishes — the browser-tab analog of the native\n\
         \x20        42-second whole-Mac freeze, and here there is NO worker thread for the\n\
         \x20        native a69a6bb3 offload to hand the work to.\n\
         \x20   HOW TO REPAIR (pick one):\n\
         \x20     1. COOPERATIVE OFFLOAD (no thread needed): detach with\n\
         \x20        `resize_offloading_scrollback(rows, cols)`, return to JS, drive\n\
         \x20        `PendingScrollbackReflow::reflow()` from a later host task (the event\n\
         \x20        loop breathes between tasks), then `finish_resize_offload(..)` /\n\
         \x20        `abort_resize_offload()` — the same audited boundary vocabulary the\n\
         \x20        native census sanctions.\n\
         \x20     2. BOUND the work: a path that provably cannot reach the width reflow\n\
         \x20        (e.g. `resize_no_reflow`), with the why written at the call site."
    );
}

/// The wasm census's verdict title.
const WASM: &str = "wasm-process census";

/// Run the wasm-process census over the checkout at `root`. A pure function
/// of the source tree.
pub fn run_wasm_census(root: &Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== wasm-process census (L0 freeze class, browser-tab analog) ===\n\
         \x20   root: {}",
        root.display()
    );

    // [OB-8] Derive the wasm-process closure (fail-closed, same machinery and
    // rules as the GUI closure of OB-1/OB-7).
    let scan = match scan_set::derive_process_scan_set(root, WASM_ROOT_CRATES) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-8] SCAN-SET DERIVATION FAILED — the census cannot soundly \
                 determine the wasm-process closure from the workspace manifests, so it \
                 refuses to sweep a guessed scope (fail-closed).\n\
                 \x20       {e}"
            );
            return CensusOutcome::red(WASM, log, "1 obligation violation(s).");
        }
    };
    let _ = writeln!(
        log,
        "    scan set: DERIVED from the workspace manifests — the union Cargo.toml \
         path-dependency closure of {} ({} workspace crates, src/ only; the same \
         derivation rules as the GUI-process censuses: normal deps only, cfg-target \
         deps in, optional deps feature-resolved — here with the web modules' \
         default-features = false engine edges, which drop the disk tier).",
        WASM_ROOT_CRATES.join(" + "),
        scan.scan_dirs.len()
    );
    if !scan.proc_macros.is_empty() {
        let listed: Vec<String> = scan
            .proc_macros
            .iter()
            .map(|(n, d)| format!("{n} ({d})"))
            .collect();
        let _ = writeln!(
            log,
            "      excluded proc-macro crate(s) (compiler-host code, never loaded into \
             the wasm module): {}",
            listed.join(", ")
        );
    }

    // [OB-8] Marker <-> registry coherence over the derived closure. The
    // registry side applies only to sinks whose crate is in this closure.
    let markers = collect_closure_markers(root, &scan.scan_dirs);
    for m in &markers {
        if !UNBOUNDED_REGISTRY.iter().any(|u| u.symbol == m.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-8] `{}` carries a `// COST: UNBOUNDED` marker (at {}) inside \
                 the derived wasm-process closure but is NOT in UNBOUNDED_REGISTRY \
                 (register the sink in crates/aterm-census/src/lib.rs).",
                m.symbol, m.span
            );
            failures += 1;
        }
    }
    let mut sinks_in_closure = 0usize;
    for sink in UNBOUNDED_REGISTRY {
        let in_closure = scan
            .scan_dirs
            .iter()
            .any(|d| sink.def_file.starts_with(&format!("{d}/")));
        if !in_closure {
            continue;
        }
        sinks_in_closure += 1;
        if !markers.iter().any(|m| m.symbol == sink.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-8] registered sink `{}` ({}) lies inside the wasm closure but \
                 has LOST its `// COST: UNBOUNDED` marker at {} (restore the marker, or \
                 re-audit the registry entry).",
                sink.symbol, sink.dim, sink.def_file
            );
            failures += 1;
        }
    }

    // Build the ROOT-crate call graph over the shipped wasm32 surface.
    let mut fns: Vec<GuiFn> = Vec::new();
    let mut pub_count: std::collections::BTreeMap<&str, usize> = Default::default();
    for rc in WASM_ROOT_CRATES {
        pub_count.insert(rc, 0);
        for file in crate_source_files(root, rc) {
            let Ok(text) = crate::tree::read_to_string(&file) else {
                continue;
            };
            let rel = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .to_string_lossy()
                .into_owned();
            let masked = mask_gated_items(&text, WASM_UNSHIPPED_GATES);
            parse_source_fns(&masked, &rel, &mut fns);
        }
    }
    // Entry points: fully-public fns (a sound over-approximation of the
    // `#[export]` exports).
    let mut roots: Vec<usize> = Vec::new();
    for (idx, f) in fns.iter().enumerate() {
        let def = f.body.first().map(String::as_str).unwrap_or("");
        if def.trim_start().starts_with("pub ") {
            roots.push(idx);
            if let Some(rc) = WASM_ROOT_CRATES
                .iter()
                .find(|rc| f.span.starts_with(&format!("{}/", rc)))
            {
                *pub_count.entry(rc).or_insert(0) += 1;
            }
        }
    }

    // [OB-9] Every root crate must contribute at least one public entry point.
    for rc in WASM_ROOT_CRATES {
        if pub_count.get(rc).copied().unwrap_or(0) == 0 {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-9] wasm root crate `{rc}` yields ZERO public entry-point fns — \
                 either the crate's public surface vanished or the parser broke; the walked \
                 surface may not silently shrink (update WASM_ROOT_CRATES only with a \
                 re-audit)."
            );
            failures += 1;
        }
    }
    if roots.is_empty() {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-9] no public entry points resolved across any wasm root crate — \
             the census walked nothing."
        );
        return CensusOutcome::red(
            WASM,
            log,
            format!("{} obligation violation(s).", failures + 1),
        );
    }

    // BFS from every public entry point (same merged-name posture as the
    // main-loop census; parents recorded for path reconstruction).
    let mut by_name: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (idx, f) in fns.iter().enumerate() {
        by_name.entry(f.name.clone()).or_default().push(idx);
    }
    let edges: Vec<std::collections::BTreeSet<String>> =
        fns.iter().map(|f| callee_names(&f.body)).collect();
    let mut parent: std::collections::BTreeMap<usize, Option<usize>> = Default::default();
    let mut queue: std::collections::VecDeque<usize> = Default::default();
    for &r in &roots {
        if parent.insert(r, None).is_none() {
            queue.push_back(r);
        }
    }
    while let Some(cur) = queue.pop_front() {
        for callee in &edges[cur] {
            for &cidx in by_name.get(callee).map(Vec::as_slice).unwrap_or(&[]) {
                if let std::collections::btree_map::Entry::Vacant(slot) = parent.entry(cidx) {
                    slot.insert(Some(cur));
                    queue.push_back(cidx);
                }
            }
        }
    }
    let reconstruct = |mut idx: usize| -> String {
        let mut path = vec![fns[idx].name.clone()];
        while let Some(Some(p)) = parent.get(&idx) {
            path.push(fns[*p].name.clone());
            idx = *p;
        }
        path.reverse();
        path.join(" -> ")
    };

    // [OB-10] Every synchronous unbounded reach from an entry point is red.
    let mut hazard_hits = 0usize;
    for idx in 0..fns.len() {
        if !parent.contains_key(&idx) {
            continue;
        }
        // (a) The modeled engine hop: `term.<unbounded>(`.
        for hz in wasm_term_hops(&fns[idx].body) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-10] L0-FREEZE OBLIGATION VIOLATED — a wasm entry point \
                 synchronously reaches an\n\
                 \x20        UNBOUNDED sink on the hosting JS event loop (browser-tab \
                 freeze class).\n\
                 \x20   PATH:  {}\n\
                 \x20   SITE:  {} (fn `{}`)\n\
                 \x20   CALL:  `.{}(..)` on the engine `term` field\n\
                 \x20   LINE:  {}\n\
                 \x20   SINK:  Terminal::{} -> Grid::resize -> resize_with_reflow_mode\n\
                 \x20          [COST: UNBOUNDED({}) @ {}] -> take_scrollback_lines + \
                 reflow_scrollback_lines",
                reconstruct(idx),
                fns[idx].span,
                fns[idx].name,
                hz.method,
                hz.line,
                hz.method,
                sink_dim("resize_with_reflow_mode"),
                sink_file("resize_with_reflow_mode"),
            );
            append_wasm_why_and_repair(&mut log);
            failures += 1;
            hazard_hits += 1;
        }
        // (b) A direct call to a registered sink (no Terminal hop at all).
        for sink in UNBOUNDED_REGISTRY {
            if edges[idx].contains(sink.symbol) {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-10] L0-FREEZE OBLIGATION VIOLATED — a wasm entry point \
                     reaches a DIRECT call to\n\
                     \x20        registered unbounded sink `{}` [COST: UNBOUNDED({}) @ {}].\n\
                     \x20   PATH:  {}\n\
                     \x20   SITE:  {} (fn `{}`)",
                    sink.symbol,
                    sink.dim,
                    sink.def_file,
                    reconstruct(idx),
                    fns[idx].span,
                    fns[idx].name,
                );
                append_wasm_why_and_repair(&mut log);
                failures += 1;
                hazard_hits += 1;
            }
        }
    }

    // [OB-12] The single-threaded posture tripwire over the whole closure.
    let spawns = spawn_sites(root, &scan.scan_dirs);
    for s in &spawns {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-12] THREAD-SPAWN VOCABULARY IN THE SHIPPED WASM CLOSURE — the \
             single-threaded posture (which makes the lock-order obligation vacuous \
             here) is broken or unproven.\n\
             \x20   SITE:  {}\n\
             \x20   TOKEN: `{}`\n\
             \x20   LINE:  {}\n\
             \x20   REPAIR: if this code never ships to wasm32, gate it with one of the \
             recognized markers ({}); if aterm is really adding wasm threads, the \
             lock-order census must be EXTENDED to this process (there is no waiver).",
            s.span,
            s.token,
            s.line,
            WASM_UNSHIPPED_GATES.join(" / ")
        );
        failures += 1;
    }

    // The posture is printed every run — it is the documented conclusion, not
    // a failure artifact.
    let _ = write!(log, "{WASM_THREADING_POSTURE}");

    if failures > 0 {
        if hazard_hits > 0 {
            let _ = write!(log, "{WASM_PRECISION_NOTE}");
        }
        return CensusOutcome::red(
            WASM,
            log,
            format!(
                "{failures} obligation violation(s) ({hazard_hits} synchronous unbounded \
                 reach(es))."
            ),
        );
    }
    let _ = writeln!(
        log,
        "    scope: lexical name-based walk of the wasm root crates' shipped src + the \
         `term`-field engine hop; markers swept over the derived {}-crate wasm-process \
         closure (precision limits: docs/temporal-safety-gate.md).",
        scan.scan_dirs.len()
    );
    let summary = format!(
        "{} fn(s) walked from {} public entry point(s) across {} wasm root crate(s); no \
         synchronous reach to an UNBOUNDED sink; {}/{} registered sink(s) inside this \
         closure marked; 0 thread-spawn token(s) in the shipped closure, so it stays \
         single-threaded and the lock-order obligation is VACUOUS here.",
        parent.len(),
        roots.len(),
        WASM_ROOT_CRATES.len(),
        sinks_in_closure,
        UNBOUNDED_REGISTRY.len(),
    );
    let engines = WASM_ENGINE_CRATES
        .iter()
        .filter(|dir| scan.scan_dirs.iter().any(|d| d == *dir))
        .count();
    CensusOutcome::green(WASM, log, summary).with_floors(vec![Floor::at_least(
        "engine crates (aterm-core, aterm-grid) in the derived wasm closure",
        engines,
        WASM_ENGINE_CRATES.len(),
        "the closure no longer reaches the engine the modules embed, so its sinks' \
         markers go unswept; the derivation lost a dependency edge",
    )])
}

/// The engine crates every wasm module embeds: the derived closure must keep
/// reaching them, or the engine's sink markers fall out of the sweep.
const WASM_ENGINE_CRATES: &[&str] = &["crates/aterm-core/src", "crates/aterm-grid/src"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_set::test_fixtures::synth_tree;

    fn body(src: &str) -> Vec<String> {
        src.lines().map(|l| l.to_string()).collect()
    }

    // Unit tests: the wasm engine-hop detector and the shipped-surface mask.

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
        // Vec::resize on a non-`term` receiver.
        assert!(wasm_term_hops(&body("        self.rgba.resize(n, 0);")).is_empty());
        // The test-local `t` receiver.
        assert!(wasm_term_hops(&body("        t.resize(0, 0);")).is_empty());
        // A different method on the engine field.
        assert!(wasm_term_hops(&body("        self.term.process(bytes);")).is_empty());
        // `resize_surface` must not match the `resize` method token.
        assert!(
            wasm_term_hops(&body(
                "        gpu.renderer.resize_surface(&mut gpu.surface, w, h);"
            ))
            .is_empty()
        );
        // `subterm.` has an identifier boundary before `term`? No — the ident
        // ending at the dot is `subterm`, not `term`.
        assert!(wasm_term_hops(&body("        subterm.resize(r, c);")).is_empty());
        // Inside a string literal: masked.
        assert!(wasm_term_hops(&body("        log(\"term.resize(1,2)\");")).is_empty());
    }

    #[test]
    fn mask_gates_blank_native_only_blocks_and_test_fns() {
        // The aterm-render shape: a cfg(not(wasm32)) BLOCK inside a shipped fn.
        let src = "fn ensure(&mut self) {\n    \
                   #[cfg(not(target_arch = \"wasm32\"))]\n    \
                   {\n        \
                   let h = std::thread::spawn(move || work());\n    \
                   }\n    \
                   sync_fallback();\n\
                   }\n";
        let masked = mask_gated_items(src, WASM_UNSHIPPED_GATES);
        assert!(
            !masked.contains("thread::spawn"),
            "the native-only block must be blanked:\n{masked}"
        );
        assert!(
            masked.contains("sync_fallback"),
            "the shipped fallback must survive:\n{masked}"
        );
        // A #[test] fn (the page_tests shape, attrs after the gate honored).
        let src = "#[test]\n#[timeout(10_000)]\nfn t() {\n    \
                   let h = std::thread::spawn(move || {});\n}\nfn shipped() {}\n";
        let masked = mask_gated_items(src, WASM_UNSHIPPED_GATES);
        assert!(!masked.contains("thread::spawn"), "masked:\n{masked}");
        assert!(masked.contains("fn shipped"), "masked:\n{masked}");
    }

    // Synthetic-tree tests: end-to-end runs against a fabricated wasm
    // workspace, green and red per obligation. Marker lines are assembled so
    // this file can never satisfy or mask a real marker.

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
        // The marked sinks (the same shape the mainloop synthetic trees use).
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
        // The two wasm root crates, each with a workspace dep on aterm-grid.
        for rc in ["aterm-wasm", "aterm-gpu-web"] {
            files.push((
                format!("crates/{rc}/Cargo.toml"),
                format!(
                    "[package]\nname = \"{rc}\"\n[dependencies]\n\
                     aterm-grid = {{ workspace = true }}\n"
                ),
            ));
        }
        // aterm-grid is already in [workspace.dependencies] via the extras;
        // the wasm roots are their own process roots, not deps of aterm-gui.
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
        // A NEW public export whose helper synchronously resizes: RED with
        // the path.
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
        // (a) An ungated spawn in a closure crate: posture broken -> RED.
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
        // (b) The same spawn behind the native-only gate: masked, GREEN.
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
}
