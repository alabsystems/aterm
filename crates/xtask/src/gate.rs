// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The local enforcement gate — aterm's replacement for CI (there is NO CI).
//!
//! Run via `cargo run -p xtask -- gate <check>` (wrapped by `tools/verify.sh`
//! and surfaced as `aterm-dev gate`). Invoked manually or via the opt-in
//! `cargo ship cut --gate` — never a hook, never CI (owner decision).
//!
//! The structured checks here are the ones plain shell cannot express:
//!
//! - `drift`: ADVERTISE-vs-IMPLEMENT. Every capability `TerminalCapabilities`
//!   advertises (`field: true` in `aterm_capabilities()`) must have a real
//!   implementation witness in the tree. Fail-closed on unknown capabilities, so
//!   adding a flag without registering a witness is caught. This catches the
//!   `kitty_graphics`/`soft_fonts` "advertised but the payload is discarded" class.
//! - `dormant`: COMPUTED-BUT-UNCONSUMED. Every feature value the engine computes
//!   must have at least one live (non-test) consumer in its required crate.
//!   Catches the `bidi_visual_order_cells`-with-no-renderer class. Entries are
//!   `enforced` once the feature is wired; until then they are reported as
//!   `pending` (the roadmap, in the gate).
//! - `mainloop`: MAIN-LOOP COMPLETENESS CENSUS (L0 whole-Mac-freeze CLASS). A
//!   width change used to rewrap the ENTIRE scrollback synchronously on the
//!   event-loop thread under the per-session `term` mutex (42s freeze). That site
//!   is fixed by an offload; this is the standing CLASS guard. Implemented in the
//!   shared `crates/aterm-census` library (obligations OB-1..OB-6: see its docs),
//!   which walks `crates/aterm-gui/src` from each main-thread ROOT + one
//!   `term_lock` hop and FAILS (printing root -> path -> sink + repair options)
//!   on any unjustified synchronous reach to an UNBOUNDED O(history) sink. The
//!   SAME library is fused into `tools/freeze-safety-gate/build.rs`, so the
//!   census is ALSO an automatic, build-blocking obligation — this verb is the
//!   manual entry point, not the only teeth.
//! - `lockorder`: LOCK-ORDER CENSUS (L0-DEADLOCK, lock-graph sense; OB-7 in the
//!   same shared `crates/aterm-census` library, fused into the same
//!   freeze-safety-gate build). Statically enumerates every lock-acquisition
//!   site and every acquired-while-holding pair across the GUI-process crates,
//!   requires the global lock graph ACYCLIC, and FAILS naming both sites of
//!   every edge of any cycle (plus the repair guidance). NO waiver channel
//!   exists, by design — an allowlisted cycle would be a standing deadlock.
//!   Design authority: docs/RFC-trust-temporal-extraction.md §2.1c.
//! - `wasmloop`: WASM-PROCESS CENSUS (L0-FREEZE, browser-tab analog; OB-8..OB-12
//!   in the same shared `crates/aterm-census` library, fused into the same
//!   freeze-safety-gate build). The wasm renderer modules are their own
//!   single-threaded process (lock-order is a documented VACUOUS posture there,
//!   tripwired by an OB-12 thread-spawn sweep); the census walks the modules'
//!   public JS-callable surface and FAILS on any unregistered synchronous reach
//!   to an UNBOUNDED sink, while any registered standing finding is reported as
//!   a candidate L0 hazard every run (the survey's two — the synchronous wasm
//!   `resize` reflow — were fixed 2026-07-14 by the cooperative offload; the
//!   registry is empty today).
//! - `fault`: INJECTED-BUT-UNEXERCISED. Every fault point injected into production
//!   code (`fault::triggered("name")`, M7 FAULT-INJECT) must be armed by some test,
//!   and every armed name must have a real injection site. Keeps the deterministic
//!   fault-injection harness honest — an untested fail-closed path rots silently.
//! - `lint`: TRUST's linter and formatter — `targo-tippy -D warnings` + `cargo fmt`
//!   through the stage2 cargo — plus grep_guard + license headers. Stock
//!   `cargo clippy` is wrong here and fails closed: the stage2 tree ships no
//!   `cargo-clippy`, so it would resolve one off PATH and drive a stable rustc
//!   that rejects this workspace's `-Ztrust-verify=off`.
//! - `counts`: COMPUTED-ONLY PROOF INVENTORY. Counts ordinary `#[kani::proof]`
//!   attributes under workspace crates, fails closed on scan/read errors or an
//!   empty inventory, and rejects a hand-maintained README total. The semantic
//!   harness-name closure remains the generated-manifest/spec-link L1 gate.
//! - `miri`: UB-FLOOR (skip-if-unavailable). Runs `cargo +nightly miri test` over
//!   the allocator/buffer/grid crates when a nightly miri is installed; otherwise
//!   prints a clear SKIP and passes (never a hard fail on a box without miri).
//! - `perf`: MEM-BUDGET retained-heap ceiling (M2); wall-clock baseline deferred.
//! - `linux` (opt-in, NOT in `all`): the codebase must keep compiling for
//!   `x86_64-unknown-linux-gnu` (no macOS-only API sneaks in un-cfg-gated). With
//!   `cargo-zigbuild` on PATH it checks the WHOLE WORKSPACE (zig cc cross-compiles
//!   the zstd C-dep); else the pure-Rust engine. Skips gracefully if that rustup
//!   target is absent. Matches M5's "uname-gated state probe".
//! - `all`: every check above EXCEPT `linux` (needs the Linux target) and `miri`
//!   (needs a nightly miri toolchain); what the pre-push hook runs.
//!
//! See docs/EXCEED_GHOSTTY_PLAN.md.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::{collect_rs_files, workspace_root};

pub(crate) fn run(check: Option<&str>) -> ExitCode {
    let ok = match check {
        Some("drift") => gate_drift(),
        Some("dormant") => gate_dormant(),
        Some("mainloop") => gate_mainloop(),
        Some("lockorder") => gate_lockorder(),
        Some("wasmloop") => gate_wasmloop(),
        Some("scope") => gate_scope(),
        Some("fault") => gate_fault(),
        Some("linux") => gate_linux(),
        Some("web") => gate_web(),
        Some("certified") => gate_certified(),
        Some("lint") => gate_lint(),
        Some("counts") => gate_counts(),
        Some("miri") => gate_miri(),
        Some("perf") => gate_perf(),
        Some("all") => {
            // Run all; report every failure (don't short-circuit) so one run
            // surfaces the full picture, then fail if any failed.
            let results = [
                ("drift", gate_drift()),
                ("dormant", gate_dormant()),
                ("mainloop", gate_mainloop()),
                ("lockorder", gate_lockorder()),
                ("wasmloop", gate_wasmloop()),
                ("scope", gate_scope()),
                ("fault", gate_fault()),
                ("counts", gate_counts()),
                ("perf", gate_perf()),
                ("lint", gate_lint()),
            ];
            let failed: Vec<&str> = results
                .iter()
                .filter(|(_, ok)| !ok)
                .map(|(n, _)| *n)
                .collect();
            if failed.is_empty() {
                eprintln!(
                    "\ngate all: GREEN — drift, dormant, mainloop, lockorder, wasmloop, scope, fault, counts, perf, lint all passed."
                );
                true
            } else {
                eprintln!("\ngate all: FAILED — {}", failed.join(", "));
                false
            }
        }
        other => {
            eprintln!(
                "usage: xtask gate <all|drift|dormant|mainloop|lockorder|wasmloop|scope|fault|linux|web|certified|lint|counts|miri|perf>\n\
                 (unknown check {other:?})"
            );
            false
        }
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------
// Source scanning helpers
// ---------------------------------------------------------------------------

/// Is this file a test-only source file (excluded from "implementation" scans)?
/// Shared with the census crate (one definition — the gates and the
/// build-blocking census must agree on what "implementation source" means).
use aterm_census::is_test_file;

/// All non-test `*.rs` files under `crates/`, optionally excluding one file by
/// suffix (e.g. the advertise site itself).
fn impl_source_files(exclude_suffix: Option<&str>) -> Vec<PathBuf> {
    let root = workspace_root();
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates"), &mut files);
    files
        .into_iter()
        .filter(|p| !is_test_file(p))
        .filter(|p| match exclude_suffix {
            Some(suf) => !p.to_string_lossy().ends_with(suf),
            None => true,
        })
        .collect()
}

/// Does any non-test source line under `crates/` contain `needle` (excluding the
/// advertise site `terminal_core.rs`)?
fn needle_present(needle: &str) -> bool {
    for file in impl_source_files(Some("terminal_core.rs")) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        // Ignore pure-comment lines so a TODO mention isn't a witness.
        if text
            .lines()
            .any(|l| !l.trim_start().starts_with("//") && l.contains(needle))
        {
            return true;
        }
    }
    false
}

/// Count non-test source lines under `consumer_path` (a file OR a dir) that
/// reference `symbol` as a USE, not its definition. The `fn <symbol>` definition
/// line is excluded so pointing the check at the crate that also DEFINES the
/// symbol still measures real consumers.
fn consumer_count(symbol: &str, consumer_path: &str) -> usize {
    let root = workspace_root();
    let target = root.join(consumer_path);
    let mut files = Vec::new();
    if target.is_file() {
        files.push(target);
    } else {
        let _ = collect_rs_files(&target, &mut files);
    }
    let def_marker = format!("fn {symbol}");
    let mut count = 0;
    for file in files.into_iter().filter(|p| !is_test_file(p)) {
        if let Ok(text) = std::fs::read_to_string(&file) {
            for l in text.lines() {
                let t = l.trim_start();
                if !t.starts_with("//") && l.contains(symbol) && !l.contains(&def_marker) {
                    count += 1;
                }
            }
        }
    }
    count
}

// ---------------------------------------------------------------------------
// G-DRIFT: advertise-vs-implement
// ---------------------------------------------------------------------------

/// The implementation evidence required for an advertised capability.
enum Proof {
    /// A substring that must appear in non-test source (outside the advertise file).
    Needle(&'static str),
    /// A path (relative to the workspace root) that must exist.
    Path(&'static str),
}

struct Witness {
    cap: &'static str,
    proof: Proof,
    /// What implements it (for the failure message when a `true` flag lacks it).
    desc: &'static str,
}

/// One entry per field of `TerminalCapabilities`. Fail-closed: if
/// `aterm_capabilities()` advertises a `true` capability with NO entry here, the
/// gate fails (a new flag must register its witness). Capabilities advertised
/// `false` are not required to have a live witness (that is the honest state).
const WITNESS_REGISTRY: &[Witness] = &[
    Witness {
        cap: "true_color",
        proof: Proof::Needle("fn parse_extended_color"),
        desc: "SGR 38;2/48;2 truecolor (handler_sgr.rs)",
    },
    Witness {
        cap: "color_256",
        proof: Proof::Path("crates/aterm-core/src/terminal/color_resolve.rs"),
        desc: "256-color palette resolution",
    },
    Witness {
        cap: "hyperlinks",
        proof: Proof::Needle("fn handle_osc_8"),
        desc: "OSC 8 hyperlinks",
    },
    Witness {
        cap: "sixel_graphics",
        proof: Proof::Path("crates/aterm-sixel"),
        desc: "Sixel DCS decoder crate",
    },
    Witness {
        cap: "iterm_images",
        proof: Proof::Needle("fn handle_osc_1337"),
        desc: "iTerm2 OSC 1337 inline images",
    },
    Witness {
        cap: "kitty_graphics",
        proof: Proof::Needle("fn handle_kitty_command"),
        desc: "Kitty graphics APC 'G' decode + display (KITTY-CORE)",
    },
    Witness {
        cap: "clipboard",
        proof: Proof::Needle("fn handle_osc_52"),
        desc: "OSC 52 clipboard",
    },
    Witness {
        cap: "shell_integration",
        proof: Proof::Path("crates/aterm-shell-integration"),
        desc: "OSC 133/633 shell integration",
    },
    Witness {
        cap: "synchronized_output",
        proof: Proof::Needle("synchronized_output"),
        desc: "DEC mode 2026 synchronized output",
    },
    Witness {
        cap: "kitty_keyboard",
        proof: Proof::Path("crates/aterm-core/src/terminal/keyboard_mode.rs"),
        desc: "Kitty keyboard protocol",
    },
    Witness {
        cap: "soft_fonts",
        proof: Proof::Needle("fn handle_decdld"),
        desc: "DRCS/DECDLD soft fonts",
    },
    Witness {
        cap: "unicode",
        proof: Proof::Path("crates/aterm-grapheme"),
        desc: "Unicode grapheme segmentation",
    },
    Witness {
        cap: "bracketed_paste",
        proof: Proof::Needle("bracketed_paste"),
        desc: "DEC mode 2004 bracketed paste",
    },
    Witness {
        cap: "focus_reporting",
        proof: Proof::Needle("focus_reporting"),
        desc: "DEC mode 1004 focus reporting",
    },
    Witness {
        cap: "mouse_tracking",
        proof: Proof::Needle("mouse_mode"),
        desc: "DEC mode 1000 mouse tracking",
    },
    Witness {
        cap: "alternate_screen",
        proof: Proof::Needle("alternate_screen"),
        desc: "DEC mode 1049 alternate screen",
    },
];

/// Parse `aterm_capabilities()` from `terminal_core.rs`, returning each
/// `field -> advertised(bool)` pair.
fn parse_advertised_caps() -> Result<Vec<(String, bool)>, String> {
    let path = workspace_root().join("crates/aterm-types/src/terminal_core.rs");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {path:?}: {e}"))?;
    let start = text
        .find("fn aterm_capabilities()")
        .ok_or("aterm_capabilities() not found")?;
    let body = &text[start..];
    let end = body.find('}').unwrap_or(body.len());
    let body = &body[..end];
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("//") {
            continue;
        }
        // Match `name: true,` / `name: false,`
        if let Some((name, rest)) = t.split_once(':') {
            let name = name.trim();
            let val = rest.trim().trim_end_matches(',').trim();
            if val == "true" {
                out.push((name.to_string(), true));
            } else if val == "false" {
                out.push((name.to_string(), false));
            }
        }
    }
    Ok(out)
}

fn gate_drift() -> bool {
    eprintln!("=== gate drift (advertise-vs-implement) ===");
    let caps = match parse_advertised_caps() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gate drift: FAILED to parse capabilities: {e}");
            return false;
        }
    };
    if caps.is_empty() {
        eprintln!("gate drift: FAILED — parsed zero capabilities (parser broke?)");
        return false;
    }
    let mut failures = Vec::new();
    for (cap, advertised) in &caps {
        let entry = WITNESS_REGISTRY.iter().find(|w| w.cap == cap);
        match entry {
            None => {
                // Fail-closed only when an UNKNOWN cap is advertised true.
                if *advertised {
                    failures.push(format!(
                        "  '{cap}' is advertised true but has NO witness registered in gate.rs \
                         (add a Witness entry mapping it to its implementation)"
                    ));
                }
            }
            Some(w) if *advertised => {
                let present = match &w.proof {
                    Proof::Needle(n) => needle_present(n),
                    Proof::Path(p) => workspace_root().join(p).exists(),
                };
                if !present {
                    failures.push(format!(
                        "  '{cap}' advertised true but witness MISSING: {} (expected {})",
                        w.desc,
                        match &w.proof {
                            Proof::Needle(n) => format!("source containing `{n}`"),
                            Proof::Path(p) => format!("path {p}"),
                        }
                    ));
                }
            }
            Some(_) => { /* advertised false: no witness required */ }
        }
    }
    let advertised_true = caps.iter().filter(|(_, a)| *a).count();
    if failures.is_empty() {
        eprintln!(
            "gate drift: GREEN — {advertised_true} advertised capabilities all have implementation witnesses; \
             {} honestly advertised false.",
            caps.len() - advertised_true
        );
        true
    } else {
        eprintln!("gate drift: FAILED — advertise-vs-implement drift:");
        for f in &failures {
            eprintln!("{f}");
        }
        eprintln!(
            "  Fix: implement the capability, or set its `aterm_capabilities()` flag false \
             (honest non-advertisement)."
        );
        false
    }
}

// ---------------------------------------------------------------------------
// G-DORMANT: computed-but-unconsumed
// ---------------------------------------------------------------------------

struct DormantWatch {
    feature: &'static str,
    /// The symbol the engine computes (the producer).
    producer: &'static str,
    /// The crate dir whose non-test code MUST reference the producer.
    consumer_path: &'static str,
    /// `true` once the feature is wired: the gate then FAILS if the consumer
    /// disappears. `false` while the wiring is still pending (reported, not failed).
    enforced: bool,
}

/// Features that must not be computed-and-dropped. Flip `enforced` to true as
/// each is wired (the milestone that wires it owns the flip).
const DORMANCY_REGISTRY: &[DormantWatch] = &[
    // M1 WIRE-BIDI: the render snapshot (cell_frame_into) must invoke the
    // visual-reorder pass, so BOTH renderers + the image capture get visual
    // order. Enforced: the gate fails if render_cells.rs stops calling it.
    DormantWatch {
        feature: "bidi visual reorder",
        producer: "apply_bidi_reorder",
        consumer_path: "crates/aterm-core/src/terminal/render_cells.rs",
        enforced: true,
    },
    // M1 WIRE-MODIFIERS: Caps/Num Lock must be folded into the key modifier byte
    // (winit omits lock state). Enforced: the key path must consume lock_modifiers.
    DormantWatch {
        feature: "caps/num lock modifiers",
        producer: "lock_modifiers",
        consumer_path: "crates/aterm-gui/src/app_input.rs",
        enforced: true,
    },
    // WIRE-COLORSCHEME: the engine reports/pushes the OS color scheme (DEC 2031 +
    // DSR ?996n). Feeding it the REAL OS appearance is the GUI's job — now WIRED:
    // `app_window::attach_os_window` seeds it from winit `Window::theme()` and
    // `WindowEvent::ThemeChanged` forwards live OS toggles, both via
    // `app_colorscheme::apply_os_color_scheme` → `Terminal::set_color_scheme`.
    DormantWatch {
        feature: "OS color-scheme source",
        producer: "set_color_scheme",
        consumer_path: "crates/aterm-gui/src",
        enforced: true,
    },
    // WIRE-INBAND-SIZE: DEC mode 2048 must emit a report on enable AND on resize.
    // Enforced: the report builder must be called (handler_dec enable + resize).
    DormantWatch {
        feature: "in-band size report (DEC 2048)",
        producer: "push_in_band_size_report",
        consumer_path: "crates/aterm-core/src/terminal",
        enforced: true,
    },
    // OSC 9;4 taskbar progress: the OSC 9 handler must parse it into state.
    // Enforced: handle_osc_9 must consume the ConEmu parser.
    DormantWatch {
        feature: "OSC 9;4 taskbar progress",
        producer: "parse_conemu_taskbar_progress",
        consumer_path: "crates/aterm-core/src/terminal/handler_osc_notify.rs",
        enforced: true,
    },
];

fn gate_dormant() -> bool {
    eprintln!("=== gate dormant (computed-but-unconsumed) ===");
    let mut failures = Vec::new();
    let mut pending = 0;
    for w in DORMANCY_REGISTRY {
        let count = consumer_count(w.producer, w.consumer_path);
        if w.enforced && count == 0 {
            failures.push(format!(
                "  '{}' is DORMANT: `{}` has zero live consumers in {} (computed but never used)",
                w.feature, w.producer, w.consumer_path
            ));
        } else if !w.enforced {
            pending += 1;
            eprintln!(
                "  pending: '{}' (`{}` -> {}): {} consumer(s); not yet enforced",
                w.feature, w.producer, w.consumer_path, count
            );
        }
    }
    if failures.is_empty() {
        eprintln!(
            "gate dormant: GREEN — {} enforced feature(s) consumed, {pending} pending wiring.",
            DORMANCY_REGISTRY.iter().filter(|w| w.enforced).count()
        );
        true
    } else {
        eprintln!("gate dormant: FAILED — features computed but never consumed:");
        for f in &failures {
            eprintln!("{f}");
        }
        false
    }
}

// ---------------------------------------------------------------------------
// G-MAINLOOP: MAIN-LOOP COMPLETENESS CENSUS (L0 whole-Mac-freeze CLASS)
// ---------------------------------------------------------------------------
//
// The census implementation lives in `crates/aterm-census` — ONE shared library
// with TWO consumers, so the manual verb and the build-blocking gate can never
// diverge:
//
//   * THIS verb (`cargo xtask gate mainloop`, part of `gate all`), and
//   * `tools/freeze-safety-gate/build.rs`, which fuses the census into the SAME
//     `cargo build` as the temporal proof gate — the AUTOMATIC, fail-closed
//     obligation (no annotation/opt-in needed from the code under scan).
//
// See the crate docs for the obligation list (OB-1..OB-6: marker↔registry —
// the marker sweep scoped to the DERIVED GUI-process closure, the same scan
// set OB-7 derives, out-of-closure workspace markers reported rather than
// registry-checked — root resolution, justified+defined offload boundaries,
// boundary presence, no guarded synchronous reach, no direct sink call) and
// the honest precision limits (lexical walk of crates/aterm-gui/src + one
// term_lock hop).

fn gate_mainloop() -> bool {
    let outcome = aterm_census::run_mainloop_census(&workspace_root());
    eprint!("{}", outcome.log);
    outcome.ok
}

// ---------------------------------------------------------------------------
// G-LOCKORDER: LOCK-ORDER CENSUS (L0-DEADLOCK, lock-graph sense)
// ---------------------------------------------------------------------------
//
// The second engine of the RFC §2.1c L0-DEADLOCK entry (the first — the
// model sense — is ty's CHECK_DEADLOCK on every derived temporal model).
// Shared `crates/aterm-census` implementation (obligation OB-7), fused into
// the same tools/freeze-safety-gate build; this verb is the manual entry
// point. There is NO waiver channel: a detected lock-order cycle can only be
// fixed, never allowlisted.

fn gate_lockorder() -> bool {
    let outcome = aterm_census::run_lock_order_census(&workspace_root());
    eprint!("{}", outcome.log);
    outcome.ok
}

// ---------------------------------------------------------------------------
// G-WASMLOOP: WASM-PROCESS CENSUS (L0-FREEZE, browser-tab analog)
// ---------------------------------------------------------------------------
//
// The third census of the shared `crates/aterm-census` library (obligations
// OB-8..OB-12), fused into the same freeze-safety-gate build; this verb is
// the manual entry point. The wasm renderer modules (aterm-wasm /
// aterm-gpu-web / aterm-effects-web) are their OWN process: single-threaded
// by target (wasm32-unknown-unknown, no atomics), so the lock-order
// obligation is VACUOUS there (documented posture + the OB-12 spawn
// tripwire, not dead graph machinery) — but the L0-FREEZE obligation
// transfers: the hosting JS event loop is the liveness-critical context, and
// the census fails on any UNregistered synchronous entry-point reach to the
// shared UNBOUNDED sinks, while any REGISTERED standing finding is
// re-detected and reported as a candidate L0 hazard every run (the survey's
// two — both modules' synchronous `resize` — were fixed 2026-07-14 by the
// cooperative offload; the registry is empty today).

fn gate_wasmloop() -> bool {
    let outcome = aterm_census::run_wasm_census(&workspace_root());
    eprint!("{}", outcome.log);
    outcome.ok
}

// ---------------------------------------------------------------------------
// G-SCOPE: SCOPE-CARDINALITY CENSUS (the "one enforcer, N instances" class)
// ---------------------------------------------------------------------------
//
// The fourth census of the shared `crates/aterm-census` library (obligations
// OB-13..OB-18), fused into the same freeze-safety-gate build; this verb is
// the manual entry point. A model that verifies a LOCAL property of ONE
// instance of an enforcing structure says nothing about a refactor that
// MULTIPLIES the instances — the flash limiter proves 2 ignitions/second for
// one limiter, and stays green if every split pane gets its own while the
// retina sees 2N. The census pins each safety budget's ownership chain from
// its scope root down to the enforcing state, closes the set of other places
// that state may live, and re-derives both from the tree every build. Only
// the vocabulary lock (OB-17) has a waiver channel; the cardinality
// obligations have none.

fn gate_scope() -> bool {
    let outcome = aterm_census::run_scope_census(&workspace_root());
    eprint!("{}", outcome.log);
    outcome.ok
}

// ---------------------------------------------------------------------------
// G-LINT
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// G-CERTIFIED (kernel-certified verification standard, locally enforced)
// ---------------------------------------------------------------------------

/// `gate certified` — enforce the KERNEL-CERTIFIED standard locally.
///
/// The Trust toolchain now defaults `-Z trust-verify-certified` ON under
/// `-Z trust-verify-full`: a proved obligation must be re-checked by the clean
/// zero-trust CIC kernel (de-Bruijn criterion), not merely solver-trusted. This
/// gate compiles the curated `crates/xtask/certified-corpus/*.rs` (functions
/// whose Level-0 safety obligations the kernel reconstructs) through the `trust`
/// toolchain's trustc under plain `-Z trust-verify-full` and requires exit 0 —
/// i.e. EVERY obligation kernel-CERTIFIES. A regression to solver-trusted (the
/// gap the default tier rejects) or a real refutation fails the gate. Skips
/// cleanly when the `trust` rustup toolchain is absent (non-trust machines), so
/// it never blocks a normal build (consistent with the no-CI/local-gate model).
fn gate_certified() -> bool {
    eprintln!("=== gate certified (corpus must KERNEL-CERTIFY under -Z trust-verify-full) ===");
    let have_trust = Command::new("rustup")
        .args(["run", "trust", "trustc", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_trust {
        eprintln!("gate certified: SKIP — `trust` rustup toolchain not available.");
        return true;
    }
    let dir = workspace_root().join("crates/xtask/certified-corpus");
    let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "rs").unwrap_or(false))
            .collect(),
        Err(e) => {
            eprintln!("gate certified: FAILED — cannot read {dir:?}: {e}");
            return false;
        }
    };
    entries.sort();
    if entries.is_empty() {
        eprintln!("gate certified: FAILED — certified-corpus is empty");
        return false;
    }
    let out = std::env::temp_dir().join("aterm_certified_gate");
    let _ = std::fs::create_dir_all(&out);
    let mut all_ok = true;
    for f in &entries {
        let name = f
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let rlib = out.join(format!("{name}.rlib"));
        let status = Command::new("rustup")
            .args([
                "run",
                "trust",
                "trustc",
                "--edition",
                "2021",
                "--crate-type",
                "lib",
            ])
            .arg(f)
            .args(["-Z", "trust-verify-full"])
            .arg("-o")
            .arg(&rlib)
            .current_dir(workspace_root())
            .status();
        match status {
            Ok(s) if s.success() => eprintln!("  CERTIFIED      {name}"),
            Ok(s) => {
                eprintln!(
                    "  NOT-CERTIFIED  {name} (exit {:?}) — obligation is solver-trusted, not kernel-certified",
                    s.code()
                );
                all_ok = false;
            }
            Err(e) => {
                eprintln!("  ERROR          {name}: {e}");
                all_ok = false;
            }
        }
    }
    if all_ok {
        eprintln!("gate certified: GREEN — every corpus obligation kernel-certifies.");
    } else {
        eprintln!(
            "gate certified: FAILED — an obligation is not kernel-certified under the default tier."
        );
    }
    all_ok
}

/// The Trust stage2 tool directory — THE toolchain, resolved the same way
/// tools/verify.sh and .githooks/pre-push resolve it: `$TRUST_STAGE2_BIN` when
/// set, else `$HOME/trust/build/host/stage2/bin`, with `build/host`'s target-triple
/// symlink resolved to a physical path (Trust's drivers reject a symlinked
/// toolchain path).
fn trust_stage2_bin() -> PathBuf {
    let raw = std::env::var_os("TRUST_STAGE2_BIN").map_or_else(
        || {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join("trust/build/host/stage2/bin")
        },
        PathBuf::from,
    );
    raw.canonicalize().unwrap_or(raw)
}

/// [`run_shell`] with extra environment and an optional directory prepended to
/// PATH — needed by the Trust tools, which resolve sibling drivers (`tippy` finds
/// `tippy-driver`, `cargo fmt` finds `trustfmt`) by looking along PATH.
fn run_shell_env(
    desc: &str,
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    path_prefix: Option<&Path>,
) -> bool {
    eprintln!("  $ {program} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args).current_dir(workspace_root());
    for (key, value) in envs {
        command.env(key, value);
    }
    if let Some(prefix) = path_prefix {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![prefix.to_path_buf()];
        entries.extend(std::env::split_paths(&existing));
        match std::env::join_paths(entries) {
            Ok(joined) => {
                command.env("PATH", joined);
            }
            Err(e) => eprintln!("  {desc}: could not extend PATH ({e}); using inherited PATH"),
        }
    }
    match command.status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("  {desc}: FAILED (exit {:?})", s.code());
            false
        }
        Err(e) => {
            eprintln!("  {desc}: could not run ({e})");
            false
        }
    }
}

fn run_shell(desc: &str, program: &str, args: &[&str]) -> bool {
    eprintln!("  $ {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .current_dir(workspace_root())
        .status();
    match status {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("  {desc}: FAILED (exit {:?})", s.code());
            false
        }
        Err(e) => {
            eprintln!("  {desc}: could not run ({e})");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// G-LINUX (M5: the headless engine must stay cross-platform — Linux-clean)
// ---------------------------------------------------------------------------

/// The codebase must keep compiling for Linux, so a macOS-only API never sneaks in
/// un-cfg-gated. Verified by a type-check against the Linux target. When
/// `cargo-zigbuild` is on PATH, it checks the WHOLE WORKSPACE (its `zig cc` shim
/// cross-compiles the zstd C-dep); otherwise it falls back to the pure-Rust engine
/// (`aterm-core --no-default-features`, no C-dep). Gracefully SKIPS (not a failure)
/// when the `x86_64-unknown-linux-gnu` rustup target's std is absent. Opt-in (NOT in
/// `gate all`) — matches the plan's M5 "uname-gated state probe".
/// Is `bin` resolvable on `PATH`?
fn on_path(bin: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `gate web` — the web renderers (`aterm-wasm` CPU, `aterm-gpu-web` GPU/WebGL2)
/// exist ONLY to run in the Electron renderer on `wasm32`. `gate all`/clippy check
/// the HOST target, so every `#[cfg(target_arch = "wasm32")]` block — the
/// `wasm_bindgen` exports, the async WebGL surface init — is otherwise NEVER
/// compiled. This verb is the only thing that proves the web crates still build for
/// their real target. Kept OUT of `gate all` (like `gate linux`): it's an optional
/// cross-compile; run it on demand (or before pushing web changes). Skips cleanly
/// when the `wasm32` target isn't installed, so it never blocks a non-web machine.
fn gate_web() -> bool {
    const TARGET: &str = "wasm32-unknown-unknown";
    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root())
        .arg("build")
        .arg("--target")
        .arg(TARGET)
        .args(["-p", "aterm-wasm", "-p", "aterm-gpu-web"]);
    eprintln!("=== gate web (aterm-wasm + aterm-gpu-web build for {TARGET}) ===");
    match cmd.output() {
        Ok(o) if o.status.success() => {
            eprintln!("gate web: GREEN — the wasm web renderers build for {TARGET}.");
            true
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            // The wasm32 target's std isn't installed here — skip, don't fail.
            if stderr.contains("may not be installed")
                || stderr.contains("can't find crate for `std`")
                || stderr.contains(&format!("note: the `{TARGET}` target"))
            {
                eprintln!(
                    "gate web: SKIPPED — rustup target {TARGET} not installed \
                     (`rustup target add {TARGET}`). Not a failure."
                );
                true
            } else {
                eprintln!("gate web: FAILED — the web renderers no longer build for wasm32:");
                eprintln!("{stderr}");
                false
            }
        }
        Err(e) => {
            eprintln!("gate web: could not run cargo ({e}); skipping.");
            true
        }
    }
}

fn gate_linux() -> bool {
    const TARGET: &str = "x86_64-unknown-linux-gnu";
    let have_zig = on_path("cargo-zigbuild") && on_path("zig");

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root())
        .arg("check")
        .arg("--target")
        .arg(TARGET);
    if have_zig {
        // zig cc translates the rustc triple cc-rs passes, so the zstd C-dep builds.
        cmd.arg("--workspace");
        cmd.env(format!("CC_{TARGET}"), "cargo-zigbuild zig cc --");
        cmd.env(format!("CXX_{TARGET}"), "cargo-zigbuild zig c++ --");
        eprintln!("=== gate linux (WHOLE WORKSPACE cross-compiles for {TARGET}, via zig cc) ===");
    } else {
        // No C cross-compiler: check the pure-Rust engine (drops the zstd C-dep).
        cmd.args(["-p", "aterm-core", "--no-default-features"]);
        eprintln!(
            "=== gate linux (engine cross-compiles for {TARGET}; install cargo-zigbuild for the full workspace) ==="
        );
    }

    match cmd.output() {
        Ok(o) if o.status.success() => {
            let scope = if have_zig {
                "the whole workspace is"
            } else {
                "the headless engine is"
            };
            eprintln!("gate linux: GREEN — {scope} Linux-clean.");
            true
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            // The Linux target's std is not installed here — skip, don't fail.
            if stderr.contains("may not be installed")
                || stderr.contains("can't find crate for `std`")
                || stderr.contains("note: the `x86_64-unknown-linux-gnu` target")
            {
                eprintln!(
                    "gate linux: SKIPPED — rustup target {TARGET} not installed \
                     (`rustup target add {TARGET}`). Not a failure."
                );
                true
            } else {
                eprintln!("gate linux: FAILED — no longer compiles for Linux:");
                eprintln!("{stderr}");
                false
            }
        }
        Err(e) => {
            eprintln!("gate linux: could not run cargo ({e}); skipping.");
            true
        }
    }
}

fn gate_lint() -> bool {
    eprintln!("=== gate lint (tippy -D warnings + trustfmt + guards) ===");
    let mut ok = true;
    // THE linter and formatter here are Trust's, not stock Rust's. The stage2 tree
    // ships `targo-tippy` and `trustfmt` and ships NO `cargo-clippy`, so plain
    // `cargo clippy` resolves whatever `cargo-clippy` happens to be on PATH —
    // Homebrew's, typically — which drives a stable rustc and dies on this
    // workspace's `-Ztrust-verify=off` before linting a single line. The gate then
    // reports an environment break in the shape of a lint finding.
    //
    // Resolve and invoke exactly as tools/verify.sh does, so verb and gate cannot
    // disagree about what "lint" means: the same candidate order, the same
    // separate CARGO_TARGET_DIR (tippy's flags differ from the main build's, and
    // sharing one dir makes the two thrash each other's cache), and tippy's own
    // directory first on PATH so it finds its `tippy-driver`.
    let tools = trust_stage2_bin();
    let target_tippy = workspace_root().join("target-tippy");
    let tippy = ["targo-tippy", "targo-clippy"]
        .iter()
        .map(|name| tools.join(name))
        .find(|path| path.is_file());
    match &tippy {
        Some(bin) => {
            ok &= run_shell_env(
                "tippy",
                &bin.to_string_lossy(),
                &["--workspace", "--all-targets", "--", "-D", "warnings"],
                &[
                    ("CARGO_TARGET_DIR", target_tippy.to_string_lossy().as_ref()),
                    ("TRUST_NO_MIGRATE_WARN", "1"),
                ],
                Some(&tools),
            );
        }
        None => {
            eprintln!(
                "  tippy: NOT RUN — no targo-tippy/targo-clippy in {}. Nothing was \
                 linted; this is not a clean lint. Build the Trust stage2 \
                 (`python3 x.py build --stage 2` in $HOME/trust) and re-run.",
                tools.display()
            );
            ok = false;
        }
    }
    // `fmt` goes through the stage2 cargo so it picks up Trust's `targo-fmt` /
    // `trustfmt` from the same directory, rather than a stock rustfmt that would
    // reformat to a different style than the one the tree is written in.
    let stage2_cargo = tools.join("cargo");
    if stage2_cargo.is_file() {
        ok &= run_shell_env(
            "trustfmt",
            &stage2_cargo.to_string_lossy(),
            &["fmt", "--all", "--", "--check"],
            &[],
            Some(&tools),
        );
    } else {
        eprintln!(
            "  trustfmt: NOT RUN — no cargo in {}. Formatting was not checked.",
            tools.display()
        );
        ok = false;
    }
    // Both guards take the repo root as their argument (as verify.sh passes it).
    let root = workspace_root();
    let root_str = root.to_string_lossy().into_owned();
    // Execute the guards directly so their `#!/usr/bin/env bash` shebang is
    // honored — they use bash-only process substitution and break under `sh`.
    let guard = root.join("tools/grep_guard.sh");
    if guard.exists() {
        ok &= run_shell("grep_guard", &guard.to_string_lossy(), &[&root_str]);
    }
    let license = root.join("tools/license_check.sh");
    if license.exists() {
        ok &= run_shell("license_check", &license.to_string_lossy(), &[&root_str]);
    }
    if ok {
        eprintln!("gate lint: GREEN");
    } else {
        eprintln!("gate lint: FAILED");
    }
    ok
}

// ---------------------------------------------------------------------------
// G-COUNTS (computed-only proof inventory; no hand-maintained prose total)
// ---------------------------------------------------------------------------

/// Recompute `(harnesses, files)` over the workspace's shipping/test crates:
/// `harnesses` is the number of matching lines, `files` is the number of files with
/// at least one match. The walk reuses [`collect_rs_files`] (skips `target/`) and
/// propagates every collection/read error instead of silently undercounting.
///
/// Exact trimmed-line matching counts an ordinary proof attribute only where it
/// is actually applied. Comments, strings, and `proof_for_contract` are distinct
/// categories and do not inflate this inventory.
fn kani_proof_counts() -> std::io::Result<(usize, usize)> {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files)?;
    let (mut harnesses, mut hit_files) = (0usize, 0usize);
    for file in &files {
        let text = std::fs::read_to_string(file)?;
        let n = text
            .lines()
            .filter(|line| is_ordinary_kani_proof_attr(line))
            .count();
        if n > 0 {
            harnesses += n;
            hit_files += 1;
        }
    }
    Ok((harnesses, hit_files))
}

fn is_ordinary_kani_proof_attr(line: &str) -> bool {
    line.trim() == "#[kani::proof]"
}

fn proof_inventory_is_valid(harnesses: usize, files: usize) -> bool {
    harnesses > 0 && files > 0 && files <= harnesses
}

/// Numeric proof totals in README prose rot immediately as the tree evolves.
/// Reject the old claim shape if it reappears; the live gate output is the sole
/// count authority.
fn readme_asserts_proof_inventory(readme: &str) -> bool {
    const MARKER: &str = "`#[kani::proof]` harnesses";
    readme.lines().any(|line| {
        line.find(MARKER)
            .is_some_and(|idx| line[..idx].chars().any(|c| c.is_ascii_digit()))
    })
}

fn gate_counts() -> bool {
    eprintln!("=== gate counts (computed-only crate proof inventory) ===");
    let (harnesses, files) = match kani_proof_counts() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gate counts: FAILED — could not scan workspace ({e})");
            return false;
        }
    };

    let readme_path = workspace_root().join("README.md");
    let readme = match std::fs::read_to_string(&readme_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("gate counts: FAILED — could not read {readme_path:?} ({e})");
            return false;
        }
    };

    if !proof_inventory_is_valid(harnesses, files) {
        eprintln!(
            "gate counts: FAILED — invalid/empty crate proof inventory \
             ({harnesses} harnesses across {files} files)"
        );
        return false;
    }
    if readme_asserts_proof_inventory(&readme) {
        eprintln!(
            "gate counts: FAILED — README.md contains a hand-maintained numeric \
             `#[kani::proof]` total; use this computed inventory instead"
        );
        return false;
    }

    eprintln!(
        "gate counts: GREEN — live inventory: {harnesses} ordinary `#[kani::proof]` \
         harnesses across {files} crate files; no hand-maintained README total"
    );
    true
}

// ---------------------------------------------------------------------------
// G-MIRI (UB-floor; skip-if-unavailable — never a hard fail without a nightly miri)
// ---------------------------------------------------------------------------

/// Run `cargo +nightly miri test` over the unsafe-bearing leaf crates IF a nightly
/// miri is installed; otherwise print a clear SKIP and pass. Mirrors `gate_linux`'s
/// skip-don't-fail discipline: a box without miri is not a merge-contract failure,
/// but where miri IS present it is a real UB floor. Opt-in (NOT in `gate all`).
fn gate_miri() -> bool {
    // Probe for a nightly miri without committing to a heavy run: `+nightly miri --version`.
    let probe = Command::new("cargo")
        .args(["+nightly", "miri", "--version"])
        .current_dir(workspace_root())
        .output();
    let have_miri = matches!(probe, Ok(ref o) if o.status.success());
    if !have_miri {
        eprintln!(
            "gate miri: SKIPPED — no nightly miri found \
             (`rustup +nightly component add miri`). Not a failure."
        );
        return true;
    }

    eprintln!("=== gate miri (UB floor: cargo +nightly miri test over alloc/buffer/grid) ===");
    let ok = run_shell(
        "miri",
        "cargo",
        &[
            "+nightly",
            "miri",
            "test",
            "-p",
            "aterm-alloc",
            "-p",
            "aterm-buffer",
            "-p",
            "aterm-grid",
        ],
    );
    if ok {
        eprintln!("gate miri: GREEN — no UB detected.");
    } else {
        eprintln!("gate miri: FAILED — miri reported undefined behavior.");
    }
    ok
}

// ---------------------------------------------------------------------------
// G-FAULT (M7: every injected fault point must be exercised by a test)
// ---------------------------------------------------------------------------

/// Extract the string-literal first argument of every `marker("…")` call in
/// `text`. For marker `triggered`, returns the names in `fault::triggered("x")`;
/// note `arm` also matches `disarm("x")` (substring) — intentional, both mean a
/// test touches that fault point.
fn extract_call_string_args(text: &str, marker: &str) -> Vec<String> {
    let pat = format!("{marker}(\"");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(&pat) {
        let after = &rest[i + pat.len()..];
        match after.find('"') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// FAULT discipline (M7 FAULT-INJECT): a fault point injected into production code
/// (`fault::triggered("name")`) that no test arms is an untested fail-closed path —
/// dead weight that rots. Conversely a test that arms a name with no injection site
/// is a stale/typo'd fault. Enforce both directions so the harness stays honest.
/// The registry's own self-tests (`fault.rs`) are excluded — they arm synthetic
/// names to test the registry itself, not real injection sites.
fn gate_fault() -> bool {
    eprintln!("=== gate fault (injected-but-unexercised) ===");
    let root = workspace_root();
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates"), &mut files);

    let mut injected: std::collections::BTreeMap<String, String> = Default::default();
    let mut armed: std::collections::BTreeSet<String> = Default::default();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        if rel.ends_with("aterm-core/src/fault.rs") || rel.ends_with("xtask/src/gate.rs") {
            // The harness's own definition + self-tests, and THIS scanner (whose doc
            // comments + pattern strings mention `triggered("…")` literally).
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        if !is_test_file(file) {
            for name in extract_call_string_args(&text, "triggered") {
                injected.entry(name).or_insert_with(|| rel.clone());
            }
        }
        // `arm("x")` also catches `disarm("x")`; collect `with_armed("x")` too.
        for name in extract_call_string_args(&text, "arm") {
            armed.insert(name);
        }
        for name in extract_call_string_args(&text, "with_armed") {
            armed.insert(name);
        }
    }

    let mut failures = Vec::new();
    for (name, site) in &injected {
        if !armed.contains(name) {
            failures.push(format!(
                "  fault '{name}' injected at {site} but NO test arms it (untested fail-closed path)"
            ));
        }
    }
    for name in &armed {
        if !injected.contains_key(name) {
            failures.push(format!(
                "  fault '{name}' is armed by a test but has NO injection site (stale/typo'd fault)"
            ));
        }
    }

    if failures.is_empty() {
        eprintln!(
            "gate fault: GREEN — {} fault point(s) injected, all exercised by a test.",
            injected.len()
        );
        true
    } else {
        eprintln!("gate fault: FAILED — fault-injection registry is inconsistent:");
        for f in &failures {
            eprintln!("{f}");
        }
        false
    }
}

// ---------------------------------------------------------------------------
// G-PERF (M2): the DETERMINISTIC memory budget is enforced now; the wall-clock
// throughput baseline (tools/golden/perf-baseline.json) is the remaining piece.
// ---------------------------------------------------------------------------

fn gate_perf() -> bool {
    eprintln!("=== gate perf ===");
    // Both gates are DETERMINISTIC (allocation-based, no wall-clock) so they never
    // flake. They are self-contained in aterm-core (no heavy comparison deps).
    // MEM-BUDGET: retained-heap ceiling. PERF-BASELINE: steady-state processing is
    // allocation-free (catches per-line/per-cell O(n)-allocation regressions).
    let mut ok = run_shell(
        "mem-budget",
        "cargo",
        &["test", "-p", "aterm-core", "--test", "mem_budget"],
    );
    ok &= run_shell(
        "perf-scaling",
        "cargo",
        &["test", "-p", "aterm-core", "--test", "perf_scaling"],
    );
    // Every wall-clock lane below also feeds the same-box TREND ledger (E0).
    let mut trend: Vec<crate::perf::TrendSample> = Vec::new();
    // WALL-CLOCK THROUGHPUT (PERF-WALLCLOCK-BASELINE lane): median-of-N MB/s of the
    // engine's parse/process hot path against a committed, generously-thresholded
    // baseline. Designed for a NO-CI multi-machine repo: it catches a CATASTROPHIC
    // regression (debug-build slip, algorithmic blow-up, lock contention) but NEVER
    // flakes on a normal/slower box — see `perf.rs`. Report-only (PASS) when no
    // baseline is present, so a fresh checkout is never blocked.
    ok &= crate::perf::gate_throughput(&mut trend);
    // PATHOLOGICAL-BENCH: per-corpus hostile-input floors (yes-flood /
    // escape-storm / style-churn / long-escapes / wide-unicode), each compared
    // against its OWN recorded baseline so a class-specific regression (e.g. an
    // SGR style-interning blow-up) cannot hide behind a healthy mixed number.
    // Same non-flake contract: report-only PASS with no baseline, generous
    // catastrophic-only ratio otherwise. See `perf.rs` + the harness header.
    ok &= crate::perf::gate_pathological(&mut trend);
    // ARENA-SCROLL (SCROLL-1): scrollback-scrub read-path floors — wheel-scrub,
    // page-sweep, and worst-case jump-to-top rates over a 100k+-line tiered fill.
    // This is the dimension our compressed tiers are structurally most at risk of
    // LOSING to ghostty's all-RAM PageList, and the floor THRU-5's async
    // compression must not regress. Engine-level + headless (frame pacing is
    // windowed-only — that half lives in tools/perf-arena/scroll.sh). Same
    // non-flake contract: report-only PASS with no baseline, per-phase floor.
    ok &= crate::perf::gate_scroll_scrub(&mut trend);
    // E0 KEYED-FLOOR lanes: search (build/query/memory on both corpus shapes),
    // restore (serialize->replay), resize/rewrap (floors + the 42s-freeze-class
    // ABSOLUTE fences, which hold even with no baseline), and the shipped wasm
    // modules (skip-with-notice on a box without node/wasm toolchain).
    ok &= crate::perf::gate_search(&mut trend);
    ok &= crate::perf::gate_restore(&mut trend);
    ok &= crate::perf::gate_resize(&mut trend);
    ok &= crate::perf::gate_wasm(&mut trend);
    // SAME-BOX TREND LEDGER (audit §5.6): the multi-machine floors are
    // deliberately generous (0.45) — this holds every metric to 0.70x of THIS
    // box's recent best, so a genuine same-box 2x regression can no longer
    // ship silently; green runs append to the committed ledger.
    let lanes_ok = ok;
    ok &= crate::perf::gate_trend(&trend, lanes_ok);
    if ok {
        eprintln!(
            "gate perf: GREEN — MEM-BUDGET + PERF-BASELINE (allocation) + wall-clock throughput + pathological + scroll-scrub + search + restore + resize (incl. absolute fences) + wasm floors + same-box trend within bounds."
        );
    } else {
        eprintln!(
            "gate perf: FAILED — perf regression (memory, allocation scaling, a lane floor: throughput / pathological / scroll-scrub / search / restore / resize / wasm, a resize absolute fence, or the same-box trend ledger)."
        );
    }
    ok
}

#[cfg(test)]
mod tests {
    // The census walker's unit tests (parse_fn_def / guard_vars / term_hop_calls
    // / synthetic RED+GREEN trees) moved WITH the implementation to
    // `crates/aterm-census` — run `cargo test -p aterm-census`.
    use super::{
        extract_call_string_args, is_ordinary_kani_proof_attr, proof_inventory_is_valid,
        readme_asserts_proof_inventory,
    };

    #[test]
    fn extracts_triggered_names() {
        let src = r#"if crate::fault::triggered("kitty.chunk_alloc") || x { }"#;
        assert_eq!(
            extract_call_string_args(src, "triggered"),
            vec!["kitty.chunk_alloc".to_string()]
        );
    }

    #[test]
    fn arm_pattern_also_catches_disarm_but_not_with_armed() {
        let src = r#"arm("a"); disarm("b"); with_armed("c", || {});"#;
        // `arm("` is a substring of `disarm("` (intended) but NOT of `with_armed("`.
        let mut got = extract_call_string_args(src, "arm");
        got.sort();
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            extract_call_string_args(src, "with_armed"),
            vec!["c".to_string()]
        );
    }

    #[test]
    fn no_match_returns_empty() {
        assert!(extract_call_string_args("let x = 1;", "triggered").is_empty());
    }

    #[test]
    fn ordinary_kani_proof_attribute_match_is_exact() {
        assert!(is_ordinary_kani_proof_attr("    #[kani::proof]"));
        assert!(!is_ordinary_kani_proof_attr(
            "// #[kani::proof] is documentation"
        ));
        assert!(!is_ordinary_kani_proof_attr(
            "#[kani::proof_for_contract(Parser::step)]"
        ));
    }

    #[test]
    fn proof_inventory_requires_real_harnesses_and_files() {
        assert!(proof_inventory_is_valid(2, 1));
        assert!(!proof_inventory_is_valid(0, 0));
        assert!(!proof_inventory_is_valid(1, 2));
    }

    #[test]
    fn readme_may_reference_live_inventory_but_not_assert_a_numeric_total() {
        assert!(readme_asserts_proof_inventory(
            "There are 9 `#[kani::proof]` harnesses in this snapshot."
        ));
        assert!(!readme_asserts_proof_inventory(
            "Run the computed proof-inventory gate for live totals."
        ));
    }
}
