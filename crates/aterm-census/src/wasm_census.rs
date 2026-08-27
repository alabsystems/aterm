// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! WASM-PROCESS CENSUS (OB-8..OB-12) — the L0-FREEZE obligation carried to the
//! OTHER process this source tree ships: the wasm renderer modules
//! (`crates/aterm-wasm` CPU, `crates/aterm-gpu-web` GPU/WebGL2,
//! `crates/aterm-effects-web` effects overlay) that load into the Electron
//! renderer page. Until 2026-07-14 this process was the census's named
//! coverage gap ("the wasm surface is a different process, outside the
//! one-process deadlock domain" — the REVIEWED_VENDORED_CRATES winit audit).
//!
//! WHAT THE SURVEY FOUND (2026-07-14), and what this census therefore is:
//!
//! * THE PROCESS IS SINGLE-THREADED — BY TARGET, NOT BY CONVENTION. The web
//!   renderers build ONLY for `wasm32-unknown-unknown` (`xtask gate web`),
//!   with no `+atomics` target feature and no SharedArrayBuffer shared
//!   memory anywhere in the build configuration. On that target std threads
//!   do not exist at runtime, so a `std::sync::Mutex` is uncontended by
//!   construction: ABBA deadlock (OB-7's class) requires two threads and is
//!   IMPOSSIBLE; a re-entrant self-lock panics loudly instead of blocking.
//!   The LOCK-ORDER obligation is therefore VACUOUS for this process — it is
//!   documented as a posture (printed every run) and kept honest by the
//!   OB-12 tripwire, NOT implemented as dead graph machinery.
//! * THE OBLIGATION THAT DOES TRANSFER IS L0-FREEZE. The JS event loop
//!   hosting the module instance (the page main thread, or the one Web
//!   Worker the host constructs the engine in) is the liveness-critical
//!   context — the browser-tab analog of the winit main thread. A long
//!   synchronous wasm export blocks input, rendering, and every other task
//!   on that loop. The derived wasm closure SHARES `aterm-grid`, so the
//!   exact same three `// COST: UNBOUNDED(<dim>)` sinks apply — but the
//!   native mitigation does NOT: there is no worker thread to offload to.
//! * THE STANDING REAL FINDING OF THE SURVEY — CLOSED 2026-07-14: both
//!   terminal-bearing modules' `resize` exports called `Terminal::resize`
//!   SYNCHRONOUSLY (the pre-a69a6bb3 shape the native app fixed), so the
//!   wasm tab could freeze on a width reflow where the native app cannot.
//!   FIXED by the COOPERATIVE offload (repair menu option 1): `resize` now
//!   detaches via `resize_offloading_scrollback` (O(1)) and the one-shot
//!   rewrap runs in a later host task (`pump_reflow`, with render-grace and
//!   output-backlog safety nets for hosts that never pump) — no thread
//!   needed. The registry entries were removed when the fix landed (OB-11
//!   went stale-RED, the celebration path); [`WASM_STANDING_HAZARDS`] is
//!   EMPTY today, and any reintroduced synchronous reach is RED via OB-10
//!   immediately.
//!
//! The obligations (each fail-closed; `[OB-n]` tags in the diagnostics):
//!
//! * OB-8  scan-set + marker coherence: the wasm-process closure is DERIVED
//!   from the root crates' manifests (`scan_set::derive_process_scan_set`,
//!   the same machinery and rules as OB-7's GUI closure — an underivable
//!   workspace stops the census, never a guessed scope). Every
//!   `// COST: UNBOUNDED(<dim>)` marker in the derived wasm closure must map
//!   to a registered sink, and every registered sink whose def_file lies
//!   INSIDE the wasm closure must still be marked (both directions).
//! * OB-9  entry-point roots: every declared wasm ROOT crate must yield at
//!   least one PUBLIC fn — the JS-callable surface (every `wasm_bindgen`
//!   export is a `pub fn`, so the public-fn set over-approximates the export
//!   set, the sound direction). A parse regression cannot silently shrink
//!   the walked surface to nothing.
//! * OB-10 no wasm entry point may SYNCHRONOUSLY reach an UNBOUNDED sink —
//!   via the engine `term` field (`self.term.resize(..)`, the modeled
//!   Terminal hop) or by calling a registered sink directly — unless the
//!   site is a registered standing finding (OB-11), which is reported as a
//!   candidate L0 hazard every run, never silently passed.
//! * OB-11 standing-finding registry coherence, fail-closed BOTH ways: every
//!   entry must carry a written finding AND still be detected at its
//!   registered fn/file this run (fixed hazard ⇒ stale entry ⇒ RED until
//!   removed — the celebration path); every detected hazard must be
//!   registered or OB-10 fails. Because entries are matched by re-DETECTION,
//!   renaming the `term` field out of the hop vocabulary also goes RED here
//!   (the lexical model cannot rot silently).
//! * OB-12 single-threaded posture tripwire: the shipped wasm closure must
//!   contain ZERO thread-spawn vocabulary (`thread::spawn(`,
//!   `thread::Builder`, `Worker::new(`) outside `#[cfg(test)]`/`#[cfg(kani)]`/
//!   `#[test]`/`#[cfg(not(target_arch = "wasm32"))]`-gated items. This is
//!   the evidence that keeps the OB-7-is-vacuous posture honest per run: if
//!   aterm ever ships wasm threads (+atomics), this fails the build until
//!   the lock-order obligation is EXTENDED to this process — extension, not
//!   waiver, is the only repair.
//!
//! Consumers (same fan-out as the sibling censuses, so the verb and the gate
//! cannot diverge): `cargo xtask gate wasmloop`, the fused
//! `tools/freeze-safety-gate/build.rs` build (obligation 4), and the
//! `aterm-census` bin (`--wasm`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::lock_order::{mask_gated_items, mask_literals};
use crate::{
    CensusOutcome, GuiFn, UNBOUNDED_REGISTRY, UNBOUNDED_TERM_METHODS, callee_names,
    collect_closure_markers, collect_rs_files, fn_defined_in, ident_ending_at, is_test_file,
    parse_source_fns, scan_set, sink_dim, sink_file, strip_line_comment,
};

/// The wasm renderer modules — the cdylib crates that load into the Electron
/// renderer page. One page = one census run; the closure is the UNION of the
/// three modules' manifest closures (they share every engine crate, and a
/// hazard in any module freezes the same page/worker event loop).
pub const WASM_ROOT_CRATES: &[&str] = &[
    "crates/aterm-effects-web",
    "crates/aterm-gpu-web",
    "crates/aterm-wasm",
];

/// Item gates whose code never compiles into the SHIPPED wasm32 module:
/// test/kani items (never shipped anywhere) plus native-only items. Masked
/// before the walk and before the OB-12 spawn sweep. Exact-match (fail-closed:
/// an unrecognized cfg spelling is NOT masked — a spawn token under it fails
/// OB-12 and forces a re-audit rather than being silently excused).
const WASM_UNSHIPPED_GATES: &[&str] = &[
    "#[cfg(test)]",
    "#[cfg(kani)]",
    "#[test]",
    "#[cfg(not(target_arch = \"wasm32\"))]",
];

/// Thread-spawn vocabulary: any of these in the shipped wasm closure breaks
/// the single-threaded posture (OB-12). `thread::spawn(` also matches
/// `std::thread::spawn(` and `wasm_thread::spawn(`; `thread::Builder` catches
/// the named-builder form; `Worker::new(` catches in-module web_sys Worker
/// construction (a worker spawned from JS is a separate instance with its own
/// linear memory — a separate process in census terms — but a worker spawned
/// from INSIDE the module would be new machinery to audit).
const SPAWN_TOKENS: &[&str] = &["thread::spawn(", "thread::Builder", "Worker::new("];

/// A REGISTERED standing finding: a known, real, deliberately-unfixed
/// synchronous unbounded reach in the wasm process. NOT a waiver — the
/// difference is enforced: each entry is re-DETECTED every run (stale = RED),
/// reprinted every run as a candidate L0 hazard with its full finding text,
/// and never subtracts from the walked surface.
struct StandingWasmHazard {
    /// The fn the hazard lives in (as defined at `def_file`).
    fn_symbol: &'static str,
    /// Repo-relative file holding the hazardous fn.
    def_file: &'static str,
    /// The written finding: what the hazard is, why it is a candidate
    /// L0-analog, and its honest severity bounds.
    finding: &'static str,
}

/// EMPTY since 2026-07-14: the survey's two findings (both terminal-bearing
/// modules' `resize` exports calling `Terminal::resize` synchronously) were
/// FIXED by the cooperative offload — `AtermTerminal::resize` /
/// `AtermGpuTerminal::resize` now detach via `resize_offloading_scrollback`
/// and defer the one-shot rewrap to a later host task (`pump_reflow` + the
/// render-grace / output-backlog safety nets), so no export reaches the width
/// reflow synchronously. The entries went stale-RED under OB-11 the moment
/// the fix landed and were removed (the celebration path). The registry and
/// its machinery stay: any future deliberately-unfixed reach must be
/// registered here WITH its written candidate-L0 finding, or OB-10 is RED.
const WASM_STANDING_HAZARDS: &[StandingWasmHazard] = &[];

/// The honest limits of this census, printed verbatim in every RED diagnostic
/// (and quoted in docs/temporal-safety-gate.md so the docs cannot drift).
pub const WASM_PRECISION_NOTE: &str = "    PRECISION / SCOPE (the honest limits of this census):
      - LEXICAL, name-based: the same fn segmenter and `<ident>(` call matching
        as the main-loop census; no type information. ENTRY POINTS are the root
        crates' `pub fn`s — a sound over-approximation of the wasm_bindgen
        export set (every export is a pub fn). Same-named fns merge.
      - THE TERMINAL HOP is the `term` naming discipline: a call
        `<...>.term.<m>(` / `term.<m>(` with `<m>` in the unbounded-method
        registry is the modeled engine hop (the wasm modules hold the engine as
        a direct `term` field — there is no lock here to key on). Renaming the
        field out of this vocabulary goes RED via OB-11 (the registered
        standing findings would no longer be re-detected), forcing a re-audit
        rather than rotting silently.
      - SCOPE: the call graph covers the root crates' non-test src ONLY (masked
        to the shipped wasm32 surface: #[cfg(test)]/#[cfg(kani)]/#[test]/
        #[cfg(not(target_arch = \"wasm32\"))] items blanked); deeper reaches are
        pinned by the shared `// COST: UNBOUNDED(<dim>)` markers swept over the
        DERIVED wasm-process closure. The OB-12 spawn sweep covers the whole
        derived closure the same masked way.
";

/// The thread/lock posture of the wasm process — printed EVERY run (GREEN or
/// RED), because the honest conclusion of the survey IS the deliverable: the
/// lock-order obligation is vacuous here, and this paragraph plus the OB-12
/// tripwire are its evidence, re-stated and re-verified per run.
pub const WASM_THREADING_POSTURE: &str =
    "    THREAD/LOCK POSTURE of the wasm process (why the LOCK-ORDER obligation is
    VACUOUS here — the evidence, re-verified every run by OB-12):
      - TARGET: wasm32-unknown-unknown (the only target these crates ship for;
        `xtask gate web` builds exactly that), with NO `+atomics` target
        feature and no SharedArrayBuffer shared memory in any build config.
        std threads DO NOT EXIST at runtime on this target: there is no second
        thread of execution inside a module instance.
      - Each wasm-bindgen module instance is bound to exactly ONE JS agent
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

/// Detect synchronous calls to an UNBOUNDED Terminal method through the wasm
/// modules' engine-field naming discipline: the receiver identifier
/// immediately before `.{method}(` must be exactly `term` (matches
/// `self.term.resize(..)` and a `term.resize(..)` local rebind; does NOT match
/// `t.resize(`, `self.rgba.resize(`, `.resize_surface(`).
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
        let Ok(text) = std::fs::read_to_string(&file) else {
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

/// Append the WHY + repair block for an OB-10 violation (the wasm repair menu
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
         \x20        (e.g. `resize_no_reflow`), with the why written at the call site.\n\
         \x20     3. STANDING FINDING (last resort, and NOT a waiver): register the site in\n\
         \x20        WASM_STANDING_HAZARDS (crates/aterm-census/src/wasm_census.rs) WITH the\n\
         \x20        written candidate-L0 finding — it is then reprinted as a hazard every\n\
         \x20        run and goes stale-RED the day the code is actually fixed."
    );
}

/// Run the wasm-process census over the aterm checkout at `root`. Pure
/// function of the source tree (no cargo, no network, no build artifacts) —
/// safe inside a build script, same as the sibling censuses.
pub fn run_wasm_census(root: &Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== gate wasmloop (wasm-process census: L0 freeze CLASS, browser-tab analog) ===\n\
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
                 \x20       {e}\n\
                 gate wasmloop: FAILED — 1 obligation violation(s)."
            );
            return CensusOutcome { ok: false, log };
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

    // [OB-8] Marker <-> registry coherence over the DERIVED wasm closure. The
    // registry side is conditional on the sink's crate being IN this closure
    // (a sink in a GUI-only crate is the mainloop census's obligation, not
    // ours) — today all three sinks live in aterm-grid, which the wasm
    // closure shares.
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

    // Build the ROOT-crate call graph over the SHIPPED wasm32 surface.
    let mut fns: Vec<GuiFn> = Vec::new();
    let mut pub_count: std::collections::BTreeMap<&str, usize> = Default::default();
    for rc in WASM_ROOT_CRATES {
        pub_count.insert(rc, 0);
        for file in crate_source_files(root, rc) {
            let Ok(text) = std::fs::read_to_string(&file) else {
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
    // Entry points: fully-public fns (every wasm_bindgen export is `pub fn`,
    // so this over-approximates the JS-callable surface — sound). `pub(crate)`
    // and friends are not exports.
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
             the census walked nothing.\n\
             gate wasmloop: FAILED — {} obligation violation(s).",
            failures + 1
        );
        return CensusOutcome { ok: false, log };
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

    // Detect every reachable synchronous unbounded reach; classify each as a
    // registered STANDING FINDING (reported) or an OB-10 violation (RED).
    let mut matched_hazards: std::collections::BTreeSet<usize> = Default::default();
    let mut standing_reported = 0usize;
    let mut hazard_hits = 0usize;
    for idx in 0..fns.len() {
        if !parent.contains_key(&idx) {
            continue;
        }
        let file = fns[idx].span.rsplit_once(':').map(|(f, _)| f).unwrap_or("");
        let registered = WASM_STANDING_HAZARDS
            .iter()
            .position(|h| h.fn_symbol == fns[idx].name && h.def_file == file);
        // (a) The modeled engine hop: `term.<unbounded>(`.
        for hz in wasm_term_hops(&fns[idx].body) {
            if let Some(hi) = registered {
                matched_hazards.insert(hi);
                standing_reported += 1;
                let h = &WASM_STANDING_HAZARDS[hi];
                let _ = writeln!(
                    log,
                    "  • STANDING FINDING [OB-10/OB-11] CANDIDATE L0-FREEZE (browser-tab \
                     analog) — registered, reported every run, NOT a waiver:\n\
                     \x20   PATH:  {}\n\
                     \x20   SITE:  {} (fn `{}`)\n\
                     \x20   CALL:  `.{}(..)` on the engine `term` field — SYNCHRONOUS on the \
                     hosting JS event loop\n\
                     \x20   LINE:  {}\n\
                     \x20   SINK:  Terminal::{} -> Grid::resize -> resize_with_reflow_mode\n\
                     \x20          [COST: UNBOUNDED({}) @ {}] -> take_scrollback_lines + \
                     reflow_scrollback_lines\n\
                     \x20   FINDING: {}",
                    reconstruct(idx),
                    fns[idx].span,
                    fns[idx].name,
                    hz.method,
                    hz.line,
                    hz.method,
                    sink_dim("resize_with_reflow_mode"),
                    sink_file("resize_with_reflow_mode"),
                    h.finding,
                );
            } else {
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

    // [OB-11] Standing-finding registry coherence, both ways. (Every detected-
    // but-unregistered hazard already failed OB-10 above.)
    for (hi, h) in WASM_STANDING_HAZARDS.iter().enumerate() {
        if h.finding.trim().is_empty() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-11] standing finding `{}` ({}) has an EMPTY finding text — \
                 every entry must carry the written candidate-L0 analysis.",
                h.fn_symbol, h.def_file
            );
            failures += 1;
        }
        if !fn_defined_in(root, h.def_file, h.fn_symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-11] standing finding `{}` is registered at {} but `fn {}` is \
                 no longer defined there — STALE entry; re-audit (update def_file if it \
                 moved, or remove the entry).",
                h.fn_symbol, h.def_file, h.fn_symbol
            );
            failures += 1;
        } else if !matched_hazards.contains(&hi) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-11] standing finding `{}` ({}) was NOT re-detected this run — \
                 either the hazard was FIXED (remove the entry: the finding is closed) or \
                 the hop vocabulary rotted (e.g. the engine field is no longer named \
                 `term`) — re-audit; a standing finding may not silently stop describing \
                 the code.",
                h.fn_symbol, h.def_file
            );
            failures += 1;
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

    // The posture is printed EVERY run — it is the documented conclusion, not
    // a failure artifact.
    let _ = write!(log, "{WASM_THREADING_POSTURE}");

    if failures > 0 {
        if hazard_hits > 0 {
            let _ = write!(log, "{WASM_PRECISION_NOTE}");
        }
        let _ = writeln!(
            log,
            "gate wasmloop: FAILED — {failures} obligation violation(s) ({hazard_hits} \
             unregistered synchronous unbounded reach(es)). This census blocks BOTH \
             `cargo xtask gate wasmloop` and the `cargo build` of tools/freeze-safety-gate."
        );
        return CensusOutcome { ok: false, log };
    }
    let _ = writeln!(
        log,
        "gate wasmloop: GREEN — {} fn(s) walked from {} public entry point(s) across {} \
         wasm root crate(s); {} STANDING candidate-L0 finding(s) reported above \
         (registered findings, re-detected and reprinted every run — not waivers); no \
         UNregistered synchronous reach to an UNBOUNDED sink; {}/{} registered sink(s) \
         inside this closure marked; 0 thread-spawn token(s) in the shipped closure — \
         single-threaded posture holds, so the lock-order obligation is VACUOUS for \
         this process (documented above, no dead machinery).",
        parent.len(),
        roots.len(),
        WASM_ROOT_CRATES.len(),
        standing_reported,
        sinks_in_closure,
        UNBOUNDED_REGISTRY.len(),
    );
    let _ = writeln!(
        log,
        "    scope: lexical name-based walk of the wasm root crates' shipped src + the \
         `term`-field engine hop; markers swept over the derived {}-crate wasm-process \
         closure (precision limits: docs/temporal-safety-gate.md).",
        scan.scan_dirs.len()
    );
    CensusOutcome { ok: true, log }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(src: &str) -> Vec<String> {
        src.lines().map(|l| l.to_string()).collect()
    }

    // ------------------------------------------------------------------
    // Unit tests: the wasm engine-hop detector and the shipped-surface mask.
    // ------------------------------------------------------------------

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
        // A DIFFERENT method on the engine field.
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

    // ------------------------------------------------------------------
    // Synthetic-tree tests: end-to-end runs against a minimal fabricated
    // wasm workspace, GREEN (standing findings reported) and RED per
    // obligation. Marker lines are assembled (never literal) so this file
    // can never satisfy — or mask — a real marker.
    // ------------------------------------------------------------------

    fn synth_tree(name: &str, files: &[(String, String)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("aterm-wasm-census-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, contents) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
            std::fs::write(&path, contents).expect("write synth file");
        }
        root
    }

    /// A minimal derivable workspace whose wasm closure carries the marked
    /// sinks: the shared manifest fixture plus the three wasm root crates,
    /// each depending on aterm-grid (so the derived closure includes the
    /// marker files) — aterm-gui exists too (the fixture's GUI root), which
    /// mirrors the real tree.
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
        // The three wasm root crates, each with a workspace dep on aterm-grid.
        for rc in ["aterm-wasm", "aterm-gpu-web", "aterm-effects-web"] {
            files.push((
                format!("crates/{rc}/Cargo.toml"),
                format!(
                    "[package]\nname = \"{rc}\"\n[dependencies]\n\
                     aterm-grid = {{ workspace = true }}\n"
                ),
            ));
        }
        // The root manifest needs aterm-grid in [workspace.dependencies]; the
        // fixture already added it via the extras list above. The wasm roots
        // are NOT deps of aterm-gui — they are their own process roots.
        files.push((
            "crates/aterm-effects-web/src/lib.rs".to_string(),
            "pub fn rain_overlay() {}\n".to_string(),
        ));
        files
    }

    /// The FIXED (2026-07-14 cooperative-offload) resize shapes of the two
    /// terminal-bearing modules: `resize` detaches via the sanctioned
    /// `resize_offloading_scrollback` boundary and a `pump_reflow` export
    /// drives the deferred rewrap — no synchronous `term.resize(` hop left.
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
        let root = synth_tree("green", &files);
        let out = run_wasm_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.ok, "expected GREEN, got:\n{}", out.log);
        assert!(
            out.log
                .contains("0 STANDING candidate-L0 finding(s) reported"),
            "the offloaded shape must report ZERO standing findings; log:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("STANDING FINDING"),
            "no standing-finding block may print on the fixed shape; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("VACUOUS") && out.log.contains("wasm32-unknown-unknown"),
            "the threading posture must be printed on GREEN; log:\n{}",
            out.log
        );
    }

    /// The regression tripwire that matters most after the fix: putting the
    /// synchronous `self.term.resize(..)` hop BACK into a resize export is an
    /// immediate OB-10 RED (the registry is empty — nothing sanctions it).
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
        let root = synth_tree("red-reintroduced", &files);
        let out = run_wasm_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        // A NEW public export whose helper synchronously resizes: not in the
        // standing registry -> must be RED with the path.
        files.push((
            "crates/aterm-effects-web/src/extra.rs".to_string(),
            "pub fn set_columns(&mut self, cols: u16) {\n    apply_cols(self, cols);\n}\n\
             fn apply_cols(s: &mut S, cols: u16) {\n    s.term.resize(s.rows, cols);\n}\n"
                .to_string(),
        ));
        let root = synth_tree("red-ob10", &files);
        let out = run_wasm_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        let root = synth_tree("red-direct", &files);
        let out = run_wasm_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("[OB-10]") && out.log.contains("DIRECT call"),
            "log:\n{}",
            out.log
        );
    }

    // NOTE: the OB-11 stale-entry path ("a registered hazard was fixed →
    // RED until the entry is removed") was exercised end-to-end FOR REAL on
    // 2026-07-14: the cooperative-offload fix landed, both registered
    // entries stopped re-detecting, the census went RED with the exact
    // "NOT re-detected … remove the entry" diagnostic, and the entries were
    // removed (the celebration path). With WASM_STANDING_HAZARDS now a
    // compile-time-empty const, that arm cannot be driven from a synthetic
    // tree (entries cannot be injected per-test); the reintroduction
    // tripwire above covers the live regression class, and the OB-11 loops
    // re-engage the day a new entry is registered.

    #[test]
    fn synthetic_unmasked_spawn_in_closure_is_red_ob12_and_gated_spawn_is_not() {
        // (a) An UNGATED spawn in a closure crate: posture broken -> RED.
        let mut files = synth_wasm_base();
        files.extend(synth_fixed_roots());
        files.push((
            "crates/aterm-grid/src/worker.rs".to_string(),
            "pub fn kick() {\n    std::thread::spawn(|| {});\n}\n".to_string(),
        ));
        let root = synth_tree("red-ob12", &files);
        let out = run_wasm_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        let root = synth_tree("green-gated-spawn", &files);
        let out = run_wasm_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        let root = synth_tree("red-ob8", &files);
        let out = run_wasm_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("[OB-8] SCAN-SET DERIVATION FAILED"),
            "log:\n{}",
            out.log
        );
    }

    // ------------------------------------------------------------------
    // Real-tree obligations: `cargo test -p aterm-census` is itself teeth.
    // ------------------------------------------------------------------

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-census lives two levels under the repo root")
            .to_path_buf()
    }

    #[test]
    fn wasm_census_is_green_on_this_tree_with_zero_standing_findings() {
        // Since the 2026-07-14 cooperative-offload fix, NO export reaches the
        // width reflow synchronously and the standing registry is empty: a
        // reappearing "STANDING FINDING" block or a nonzero count here means
        // the sync hop came back (or an entry was re-registered) — re-audit.
        let out = run_wasm_census(&repo_root());
        assert!(out.ok, "wasm census RED on the current tree:\n{}", out.log);
        assert!(
            out.log
                .contains("0 STANDING candidate-L0 finding(s) reported"),
            "the fixed tree must report zero standing findings; log:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("STANDING FINDING"),
            "no standing-finding block may print on the fixed tree; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("0 thread-spawn token(s)"),
            "the single-threaded posture must hold on HEAD; log:\n{}",
            out.log
        );
    }

    #[test]
    fn standing_hazards_resolve_on_this_tree() {
        // Trivially empty since the 2026-07-14 fix; kept as teeth for any
        // future entry (a registered finding must carry its analysis and
        // still resolve at its def site).
        let root = repo_root();
        for h in WASM_STANDING_HAZARDS {
            assert!(
                !h.finding.trim().is_empty(),
                "standing finding `{}` must carry the written analysis",
                h.fn_symbol
            );
            assert!(
                fn_defined_in(&root, h.def_file, h.fn_symbol),
                "standing finding `{}` is not defined in {} — stale entry",
                h.fn_symbol,
                h.def_file
            );
        }
    }

    /// The derived WASM-process closure, pinned (the GUI canary's twin). A
    /// LEGITIMATE dependency change updates the pin — the review diff is the
    /// audit trail; an unexpected delta means the graph or the derivation
    /// drifted. PROVENANCE: derived 2026-07-14 from the three web modules'
    /// manifests (engine edges default-features = false — the disk tier and
    /// its libc/zstd closure are OUT, unlike the GUI closure).
    ///
    /// # `aterm-uds` is an OVER-APPROXIMATION, not browser code
    ///
    /// `aterm-uds` is in this pin because `aterm-shell-integration` reaches it
    /// from a `[target.'cfg(any(unix, windows))'.dependencies]` section, and
    /// [`classify_section`](super::scan_set) counts cfg-target deps IN on
    /// every platform. That rule is deliberately fail-closed: the derivation
    /// does not evaluate cfg predicates, so it cannot prove an edge absent,
    /// and scanning source a target does not link is safe while missing source
    /// it does link is not.
    ///
    /// It is NOT a claim about the browser bundle. `wasm32-unknown-unknown` is
    /// neither `unix` nor `windows`, so cargo never resolves that edge for the
    /// web build: no Unix-domain socket, no `/dev/urandom` fallback and no
    /// blocking `std::fs` reaches the bundle. The wasm nonce mint takes the
    /// `cfg(all(target_arch = "wasm32", target_os = "unknown"))` arm, which
    /// calls `crypto.getRandomValues` through `getrandom` and never mentions
    /// `aterm-uds`. If the census is ever taught to evaluate cfg predicates,
    /// this entry is the first one that should disappear.
    #[test]
    fn derived_wasm_closure_matches_the_pinned_canary() {
        const PINNED: &[&str] = &[
            "crates/aterm-alloc/src",
            "crates/aterm-bits/src",
            "crates/aterm-codec/src",
            "crates/aterm-containment/src",
            "crates/aterm-core/src",
            "crates/aterm-effects-web/src",
            "crates/aterm-effects/src",
            "crates/aterm-error/src",
            "crates/aterm-ffi-types/src",
            "crates/aterm-gpu-web/src",
            "crates/aterm-gpu/src",
            "crates/aterm-grapheme/src",
            "crates/aterm-grid/src",
            "crates/aterm-hash/src",
            "crates/aterm-lexicon/src",
            "crates/aterm-log/src",
            "crates/aterm-lz4/src",
            "crates/aterm-parser/src",
            "crates/aterm-policy/src",
            "crates/aterm-predict/src",
            "crates/aterm-provenance/src",
            // Entered the closure when the first-party regular-expression
            // engine replaced `regex` (+ regex-automata, regex-syntax,
            // aho-corasick): aterm-selection and aterm-search compile patterns
            // through it, and both are in the web modules' graph. Dependency-
            // free and lock-free, so it adds no thread or lock vocabulary here.
            "crates/aterm-regex/src",
            "crates/aterm-render-api/src",
            "crates/aterm-render/src",
            "crates/aterm-rle/src",
            "crates/aterm-scene/src",
            "crates/aterm-scrollback/src",
            "crates/aterm-search/src",
            "crates/aterm-selection/src",
            "crates/aterm-shell-integration/src",
            "crates/aterm-sixel/src",
            "crates/aterm-tempfile/src",
            // Entered the closure when the first-party clock replaced
            // `web-time`: aterm-core, -types, -effects, -gpu and -predict all
            // sample time through it, and on wasm it IS the shim that keeps
            // `Instant::now()` off the panicking std path.
            "crates/aterm-time/src",
            "crates/aterm-types/src",
            // NOT browser code — see the over-approximation note on this test.
            // Reached only through aterm-shell-integration's
            // `cfg(any(unix, windows))` section, which wasm32-unknown-unknown
            // never satisfies; the derivation counts cfg-target deps IN on
            // every platform because it does not evaluate cfg predicates.
            "crates/aterm-uds/src",
            "crates/aterm-vi/src",
            "crates/aterm-wasm/src",
        ];
        let set = scan_set::derive_process_scan_set(&repo_root(), WASM_ROOT_CRATES)
            .expect("derivation must succeed on HEAD");
        let mut pinned: Vec<String> = PINNED.iter().map(|s| s.to_string()).collect();
        pinned.sort();
        assert_eq!(
            set.scan_dirs, pinned,
            "\nthe DERIVED wasm-process closure changed.\n\
             If you just added/removed a dependency of the web modules' graphs, update \
             the pin (the review diff is the audit trail). Otherwise investigate: the \
             derivation or the workspace drifted unexpectedly.\n"
        );
    }
}
