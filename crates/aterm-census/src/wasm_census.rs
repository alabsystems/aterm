// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Wasm-process L0 freeze and single-threaded posture census ([`WASM_THREADING_POSTURE`]).
//!
//! The modules are single-threaded by target ([`WASM_THREADING_POSTURE`],
//! held by the OB-12 tripwire), so lock order is vacuous here. L0 freeze does
//! transfer: a long synchronous export blocks the hosting JS event loop, and the
//! closure shares `aterm-grid`'s `// COST: UNBOUNDED(<dim>)` sinks with no
//! worker thread to offload to (`resize` exports detach via
//! `resize_offloading_scrollback`; `pump_reflow` rewraps in a later host task).
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::lock_order::{mask_items_where, mask_literals};
use crate::{
    CensusOutcome, Floor, GuiFn, UNBOUNDED_REGISTRY, UNBOUNDED_TERM_METHODS, callee_names,
    collect_closure_markers, collect_rs_files, ident_ending_at, is_test_file, parse_source_fns,
    scan_set, sink_dim, sink_file, strip_line_comment,
};

/// The wasm renderer modules (cdylib crates loaded into the Electron renderer page).
pub const WASM_ROOT_CRATES: &[&str] = &["crates/aterm-gpu-web", "crates/aterm-wasm"];

/// Blank items that are never compiled on `wasm32` ([`crate::wasm_cfg::attr_never_on_wasm32`]).
fn mask_unshipped(text: &str) -> String {
    mask_items_where(text, &crate::wasm_cfg::attr_never_on_wasm32)
}

/// Thread-spawn vocabulary that breaks the single-threaded posture (OB-12).
const SPAWN_TOKENS: &[&str] = &["thread::spawn(", "thread::Builder", "Worker::new("];

/// The honest limits of this census, printed with every OB-10 violation.
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
        to the shipped wasm32 surface: #[test] items and items whose one-line
        #[cfg(..)] is false on wasm32-unknown-unknown blanked; an unknown
        predicate, a feature or debug_assertions, keeps its item; a file every
        `mod` or `include!` that could load it is false there, or sits in such
        a file, is left out); deeper reaches are pinned by the shared
        `// COST: UNBOUNDED(<dim>)` markers swept over the DERIVED wasm-process
        closure. The OB-12 spawn sweep covers the whole derived closure the
        same masked way.
";

/// The thread/lock posture of the wasm process, printed every run.
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

/// Detect synchronous calls to an unbounded Terminal method.
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
            let len = after
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            let method = &after[..len];
            if !method.is_empty()
                && after[method.len()..].starts_with('(')
                && UNBOUNDED_TERM_METHODS.contains(&method)
            {
                out.push(WasmTermHop {
                    method: method.to_string(),
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

/// Source files excluded on `wasm32` ([`crate::wasm_cfg::unloaded_on_wasm32`]).
fn unloaded_files(root: &Path, scan_dirs: &[String]) -> (BTreeSet<PathBuf>, Vec<String>) {
    let mut out = BTreeSet::new();
    let mut notes = Vec::new();
    let (mut crates, mut test_cuts, mut named) = (0usize, 0usize, Vec::new());
    let rel = |p: &Path| crate::rel_path(root, p);
    for dir in scan_dirs {
        let src = root.join(dir);
        let mut paths = Vec::new();
        let _ = collect_rs_files(&src, &mut paths);
        let files: BTreeMap<PathBuf, String> = paths
            .into_iter()
            .filter_map(|p| crate::tree::read_to_string(&p).ok().map(|t| (p, t)))
            .collect();
        let mut roots: BTreeSet<PathBuf> = [src.join("lib.rs"), src.join("main.rs")].into();
        if let Some(crate_dir) = src.parent()
            && let Ok(manifest) = crate::tree::read_to_string(&crate_dir.join("Cargo.toml"))
        {
            for line in manifest.lines() {
                let named_root = line
                    .trim()
                    .strip_prefix("path")
                    .map(str::trim_start)
                    .and_then(|r| r.strip_prefix('='))
                    .map(|r| r.trim().trim_matches('"'))
                    .filter(|p| p.ends_with(".rs"));
                if let Some(p) = named_root {
                    roots.insert(crate_dir.join(p));
                }
            }
        }
        let unloaded = crate::wasm_cfg::unloaded_on_wasm32(&files, &roots);
        if let Some(why) = unloaded.unplaced {
            notes.push(format!(
                "module tree of {dir} not placed ({}): every file in it is swept",
                // `why` begins with its file's path: the rest is text after it.
                crate::rel_path(root, Path::new(&why))
            ));
        }
        if unloaded.files.is_empty() {
            continue;
        }
        crates += 1;
        for cut in &unloaded.cuts {
            if cut.gate == "#[test]" || crate::cfg_attr_never_ships(&cut.gate) {
                test_cuts += 1;
            } else {
                named.push(format!(
                    "{}:{} `{}` under {}",
                    rel(&cut.file),
                    cut.line,
                    cut.loader,
                    cut.gate
                ));
            }
        }
        out.extend(unloaded.files);
    }
    if !out.is_empty() {
        let by = if named.is_empty() {
            String::new()
        } else {
            format!(" and by: {}", named.join(", "))
        };
        notes.insert(
            0,
            format!(
                "{} file(s) in {crates} crate(s) no wasm32-unknown-unknown build loads (every \
                 `mod` or `include!` that loads them is false there), not swept: cut off by \
                 {test_cuts} test-only loader(s){by}",
                out.len()
            ),
        );
    }
    (out, notes)
}

/// Sweep the masked wasm closure for thread-spawn vocabulary, skipping `unloaded` files.
fn spawn_sites(root: &Path, scan_dirs: &[String], unloaded: &BTreeSet<PathBuf>) -> Vec<SpawnSite> {
    let mut hits = Vec::new();
    let mut files = Vec::new();
    for dir in scan_dirs {
        let _ = collect_rs_files(&root.join(dir), &mut files);
    }
    files.retain(|p| !is_test_file(p) && !unloaded.contains(p));
    files.sort();
    for file in files {
        let Ok(text) = crate::tree::read_to_string(&file) else {
            continue;
        };
        // Masking only blanks or cuts text: a file naming no token has no hit to mask.
        if !SPAWN_TOKENS.iter().any(|token| text.contains(token)) {
            continue;
        }
        let rel = crate::rel_path(root, &file);
        let masked = mask_unshipped(&text);
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

/// Append the why + repair block for an OB-10 violation.
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

/// Run the wasm-process census over the checkout at `root`.
pub fn run_wasm_census(root: &Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== wasm-process census (L0 freeze class, browser-tab analog) ===\n\
         \x20   root: {}",
        root.display()
    );

    let scan = match scan_set::derive_linked_scan_set(
        root,
        WASM_ROOT_CRATES,
        &crate::wasm_cfg::target_links_on_wasm32,
    ) {
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
         derivation rules as the GUI-process censuses — normal deps only, optional \
         deps feature-resolved, here with the web modules' default-features = false \
         engine edges, which drop the disk tier — except that a target table false \
         on wasm32-unknown-unknown links nothing).",
        WASM_ROOT_CRATES.join(" + "),
        scan.scan_dirs.len()
    );
    if !scan.unlinked.is_empty() {
        let listed: Vec<String> = scan
            .unlinked
            .iter()
            .map(|(dir, dep, target)| format!("{dir} -> {dep} [target.{target}]"))
            .collect();
        let _ = writeln!(
            log,
            "      target-gated edge(s) no wasm32-unknown-unknown build links, not \
             followed: {}",
            listed.join(", ")
        );
    }
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
    let (unloaded, unloaded_notes) = unloaded_files(root, &scan.scan_dirs);
    for note in &unloaded_notes {
        let _ = writeln!(log, "      {note}");
    }

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
        let in_closure = scan.scan_dirs.iter().any(|d| {
            sink.def_file
                .strip_prefix(d.as_str())
                .is_some_and(|s| s.starts_with('/'))
        });
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

    let mut fns: Vec<GuiFn> = Vec::new();
    let mut pub_count: std::collections::BTreeMap<&str, usize> = Default::default();
    for rc in WASM_ROOT_CRATES {
        pub_count.insert(rc, 0);
        for file in crate_source_files(root, rc) {
            if unloaded.contains(&file) {
                continue;
            }
            let Ok(text) = crate::tree::read_to_string(&file) else {
                continue;
            };
            let rel = crate::rel_path(root, &file);
            let masked = mask_unshipped(&text);
            parse_source_fns(&masked, &rel, &mut fns);
        }
    }
    let mut roots: Vec<usize> = Vec::new();
    for (idx, f) in fns.iter().enumerate() {
        let def = f.body.first().map(String::as_str).unwrap_or("");
        if def.trim_start().starts_with("pub ") {
            roots.push(idx);
            if let Some(rc) = WASM_ROOT_CRATES.iter().find(|rc| {
                f.span
                    .strip_prefix(**rc)
                    .is_some_and(|s| s.starts_with('/'))
            }) {
                *pub_count.entry(rc).or_insert(0) += 1;
            }
        }
    }

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

    let mut hazard_hits = 0usize;
    for idx in 0..fns.len() {
        if !parent.contains_key(&idx) {
            continue;
        }
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

    let spawns = spawn_sites(root, &scan.scan_dirs, &unloaded);
    for s in &spawns {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-12] THREAD-SPAWN VOCABULARY IN THE SHIPPED WASM CLOSURE — the \
             single-threaded posture (which makes the lock-order obligation vacuous \
             here) is broken or unproven.\n\
             \x20   SITE:  {}\n\
             \x20   TOKEN: `{}`\n\
             \x20   LINE:  {}\n\
             \x20   REPAIR: if this code never ships to wasm32, gate it, or the `mod` \
             that loads its file, with a one-line `#[cfg(..)]` false on \
             wasm32-unknown-unknown (e.g. `#[cfg(not(target_arch = \"wasm32\"))]`), or \
             reach its crate only through a target table false there; if aterm is \
             really adding wasm threads, the lock-order census must be EXTENDED to \
             this process (there is no waiver).",
            s.span, s.token, s.line,
        );
        failures += 1;
    }

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
         closure; a lexical walk, not a proof.",
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

/// The engine crates every wasm module embeds.
const WASM_ENGINE_CRATES: &[&str] = &["crates/aterm-core/src", "crates/aterm-grid/src"];

#[cfg(test)]
#[path = "wasm_census_tests.rs"]
mod tests;
