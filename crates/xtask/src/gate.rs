// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The local enforcement gate — aterm's replacement for CI (there is NO CI).
//!
//! Run via `cargo run -p xtask -- gate <check>`. Four verbs are wrapped by
//! `tools/verify.sh` (`drift`, `dormant`, `mainloop`, `counts`) and so also run
//! under the opt-in `cargo ship cut --gate`, which shells out to
//! `tools/verify.sh --full`; the rest are manual. Never a hook, never CI (owner
//! decision). This header used to also claim an `aterm-dev gate` surface; there
//! is none — that crate's `SUBS` registry lists visual-judge / audit /
//! verify-proofs / setup-trust and nothing else (checked 2026-07-31).
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
//!
//!   THE IDENTITY-GUARD ABORT IS NOT A LINT RESULT. Branded Tippy authenticates
//!   its own toolchain (see [`TIPPY_IDENTITY_ABORTS`]), and any create/unlink/
//!   rename in ANY ancestor of the stage2 sysroot mid-run aborts it — a run can
//!   lint clean, print `Finished dev profile`, and still exit non-zero. This
//!   lane retries that signature and only that signature; a real `-D warnings`
//!   failure is never retried into a pass.
//!
//!   RUNNING IT BY HAND — two traps. The path must be CANONICAL:
//!   `$HOME/trust/build/host/stage2/bin` traverses a symlink and is refused
//!   outright ("traverses a symlink or non-canonical path"); use
//!   `$HOME/trust/build/<host-triple>/stage2/bin`. (This verb is safe either way —
//!   `trust_stage2_bin` canonicalizes — but a human copying the `build/host`
//!   path is not.) And give each concurrent invoker its OWN `CARGO_TARGET_DIR`:
//!   the guarded window spans cargo's build-lock WAIT, so a shared
//!   `target-tippy` leaves a fast crate exposed for as long as it is queued.
//!   The working single-crate form of this lane is:
//!   `PATH="$HOME/trust/build/<triple>/stage2/bin:$PATH"
//!   CARGO_TARGET_DIR=<root>/target-tippy TRUST_NO_MIGRATE_WARN=1 targo-tippy
//!   -p <crate> --all-targets -- -D warnings` (add `--no-deps` to see one
//!   crate's own findings when a workspace peer is red).
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
//! - `all`: the [`ALL_ROSTER`] gates — drift, dormant, mainloop, lockorder,
//!   wasmloop, scope, fault, counts, perf, lint — i.e. every check above except
//!   `linux` (needs the Linux target), `miri` (needs a nightly miri toolchain),
//!   `web` and `certified`.
//!   MANUAL ONLY — nothing invokes it. This line used to read "what the pre-push
//!   hook runs"; MEASURED 2026-07-31, that is false. `.githooks/pre-push` runs
//!   exactly one command (the `tools/freeze-safety-gate` build, which fuses the
//!   mainloop / lockorder / wasmloop / scope censuses), and tools/verify.sh
//!   invokes only `drift`, `dormant`, `mainloop` and `counts`. So `fault` and
//!   `perf` have NO automated caller today: run them by hand, or wire them into
//!   verify.sh (`fault` is cheap and toolchain-free; `perf` belongs behind
//!   `--full`).
//!
//! THE NON-VACUITY OBLIGATION ([`NON_VACUITY_REGISTRY`]). Six times on
//! 2026-07-31 a gate in this repo was found ASSERTING MORE THAN IT VERIFIED —
//! `gate drift` had been vacuous since it was written (its witness scan walked
//! gate.rs itself, so every `Proof::Needle` literal was its own witness).
//! Careful reading demonstrably does not catch that class; only a mechanical
//! obligation does. So every entry of [`ALL_ROSTER`] must be paired here with
//! EITHER a named red-fixture test that plants a violation and asserts the gate
//! reports FAILURE, OR an explicit KNOWN GAP carrying its reason — and
//! `every_all_roster_gate_has_a_red_fixture_or_a_registered_known_gap` fails
//! `cargo test -p xtask` (which tools/verify.sh runs at workspace scope, line
//! 331) when a roster entry has neither. The registry's `drives` field states
//! exactly what each fixture calls, so a COMPONENT-level demonstration can
//! never be read as a VERB-level one. The same check runs as the verb
//! `gate nonvacuity`, and at the END of `gate all` — so the honest score
//! ("9/10 roster gates have a red fixture; KNOWN GAP: perf") is printed at the
//! moment a human is about to read the word GREEN, and a violated obligation
//! fails `gate all` itself.
//!
//! See docs/EXCEED_GHOSTTY_PLAN.md.

use std::fmt::Write as _;
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
        // The meta-obligation on its own: cheap (a few file reads), so it can
        // be run without paying for the roster it audits. `all` runs it too.
        Some("nonvacuity") => report_non_vacuity(),
        Some("all") => {
            // Run all; report every failure (don't short-circuit) so one run
            // surfaces the full picture, then fail if any failed.
            let results: Vec<(&str, bool)> = ALL_ROSTER
                .iter()
                .map(|(name, check)| (*name, check()))
                .collect();
            let mut failed: Vec<&str> = results
                .iter()
                .filter(|(_, ok)| !ok)
                .map(|(n, _)| *n)
                .collect();
            // THE META-OBLIGATION, run here too: a roster of ten green gates
            // means nothing if one of them cannot go red. This is the same
            // check `cargo test -p xtask` enforces — run at the exact moment a
            // human is about to read the word GREEN.
            if !report_non_vacuity() {
                failed.push("non-vacuity");
            }
            if failed.is_empty() {
                eprintln!(
                    "\ngate all: GREEN — {} all passed.",
                    roster_names().join(", ")
                );
                true
            } else {
                eprintln!("\ngate all: FAILED — {}", failed.join(", "));
                false
            }
        }
        other => {
            eprintln!(
                "usage: xtask gate <all|drift|dormant|mainloop|lockorder|wasmloop|scope|fault|linux|web|certified|lint|counts|miri|perf|nonvacuity>\n\
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
// THE `all` ROSTER + the NON-VACUITY OBLIGATION over it
// ---------------------------------------------------------------------------

/// A roster entry: the verb's name and the check it runs.
type RosterEntry = (&'static str, fn() -> bool);

/// The gates `gate all` runs, in order. ONE definition with TWO readers — the
/// `all` arm above and [`NON_VACUITY_REGISTRY`]'s meta-test — so a gate cannot
/// join the roster without acquiring a red fixture (or an explicit gap), and
/// cannot leave the roster while a stale registry entry still claims it.
const ALL_ROSTER: &[RosterEntry] = &[
    ("drift", gate_drift),
    ("dormant", gate_dormant),
    ("mainloop", gate_mainloop),
    ("lockorder", gate_lockorder),
    ("wasmloop", gate_wasmloop),
    ("scope", gate_scope),
    ("fault", gate_fault),
    ("counts", gate_counts),
    ("perf", gate_perf),
    ("lint", gate_lint),
];

fn roster_names() -> Vec<&'static str> {
    ALL_ROSTER.iter().map(|(name, _)| *name).collect()
}

/// How a roster gate's ABILITY TO GO RED is established.
enum RedProof {
    /// `test` — a `#[test] fn` in `file` (workspace-relative) — plants a
    /// violation and asserts a RED verdict.
    Fixture {
        test: &'static str,
        file: &'static str,
        /// EXACTLY what the fixture calls. A component-level demonstration
        /// (`run_repo_guards`) must say so here; only a fixture that calls the
        /// verb's own reporting function may claim the verb. Prose, for the
        /// reader — the machine-checked half is `calls`.
        drives: &'static str,
        /// The symbol the fixture MUST mention, checked as a substring of its
        /// body. Without this the obligation is satisfiable by a fixture that
        /// never touches the gate — `assert!(!false);` in a correctly-named
        /// `#[test]` scored as proof, which is the very defect this registry
        /// exists to stop, one level up. It cannot prove the call is REACHED
        /// (that needs coverage, not a substring), but it does bind the fixture
        /// to the gate it claims, and the registry already knew this symbol.
        calls: &'static str,
        /// Does the fixture drive the VERB, or only a component of it? The
        /// printed score separates the two rather than counting them together —
        /// `lint`'s fixture drives `run_repo_guards`, not `gate_lint`, and a
        /// score that calls both "verb-level" over-claims exactly like the
        /// verdict line this repo fixed this morning.
        verb_level: bool,
    },
    /// NOBODY HAS EVER SHOWN THIS GATE FAIL. `reason` states why a fixture is
    /// not feasible today and what would close the gap. An honest gap is the
    /// point of this obligation — a fabricated fixture is the thing it exists
    /// to prevent.
    ///
    /// Currently constructed only by this module's own tests: as of the perf and
    /// lint fixtures landing, the live registry has NO known gaps, which is the
    /// point. The variant STAYS — deleting it would leave a future hard-to-prove
    /// gate with no honest way to say so, and the pressure would go somewhere
    /// worse (a fixture that drives a component and calls it the verb). The
    /// allow is scoped to `not(test)` so it silences exactly the build where the
    /// variant is genuinely unconstructed, and nothing else.
    #[cfg_attr(not(test), allow(dead_code))]
    KnownGap { reason: &'static str },
}

struct RedFixture {
    gate: &'static str,
    proof: RedProof,
}

/// A KNOWN GAP reason shorter than this is not a reason. (An arbitrary but
/// enforced floor: "TODO" and "hard" cannot be registered as engineering
/// judgement.)
const MIN_GAP_REASON: usize = 120;

/// One entry per [`ALL_ROSTER`] gate — fail-closed in both directions.
const NON_VACUITY_REGISTRY: &[RedFixture] = &[
    RedFixture {
        gate: "drift",
        proof: RedProof::Fixture {
            test: "an_unwitnessed_capability_advertised_true_fails_the_drift_verb",
            file: "crates/xtask/src/gate.rs",
            drives: "the VERB: drift_report() with the REAL WITNESS_REGISTRY over a \
                     fixture root whose advertise file is mutated to advertise a \
                     capability with no implementation witness (GREEN before, RED \
                     after, GREEN again once the witness lands)",
            calls: "drift_report",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "dormant",
        proof: RedProof::Fixture {
            test: "deleting_the_only_consumer_fails_the_dormant_verb",
            file: "crates/xtask/src/gate.rs",
            drives: "the VERB: dormant_report() with the REAL registry entry for \
                     `apply_bidi_reorder` over a copy of the real render_cells.rs \
                     with its consumer lines deleted (GREEN before, RED after)",
            calls: "dormant_report",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "mainloop",
        proof: RedProof::Fixture {
            test: "synthetic_prefix_resize_shape_is_red_with_path",
            file: "crates/aterm-census/src/lib.rs",
            drives: "the VERB's implementation: run_mainloop_census() over a \
                     synthetic tree that reintroduces the synchronous \
                     term_lock(..).resize(..) shape (OB-5 RED with the path)",
            calls: "run_mainloop_census",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "lockorder",
        proof: RedProof::Fixture {
            test: "synthetic_cross_boundary_abba_is_red_across_the_namespace",
            file: "crates/aterm-census/src/lock_order.rs",
            drives: "the VERB's implementation: run_lock_order_census() over a \
                     synthetic tree carrying an A-B/B-A cycle across the \
                     vendored-namespace boundary (OB-7 RED naming both sites)",
            calls: "run_synth_files",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "wasmloop",
        proof: RedProof::Fixture {
            test: "synthetic_reintroduced_sync_resize_is_red_ob10",
            file: "crates/aterm-census/src/wasm_census.rs",
            drives: "the VERB's implementation: run_wasm_census() over a synthetic \
                     tree that puts the synchronous self.term.resize(..) back into \
                     a wasm resize export (OB-10 RED at its site)",
            calls: "run_wasm_census",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "scope",
        proof: RedProof::Fixture {
            test: "a_per_pane_word_decorations_map_fails_the_flash_limiter_chain",
            file: "crates/aterm-census/src/scope_census.rs",
            drives: "the VERB's implementation: run_scope_census_over() with the \
                     REAL flash-limiter claim over a copy of the real \
                     aterm-gui/src/lib.rs made per-pane (OB-13 RED)",
            calls: "run_one",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "fault",
        proof: RedProof::Fixture {
            test: "an_unarmed_injection_site_fails_the_fault_verb",
            file: "crates/xtask/src/gate.rs",
            drives: "the VERB: fault_report() over a synthetic tree whose injected \
                     fault point no test arms (and the mirror direction: an armed \
                     name with no injection site)",
            calls: "fault_report",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "counts",
        proof: RedProof::Fixture {
            test: "an_empty_inventory_and_a_hand_maintained_total_fail_the_counts_verb",
            file: "crates/xtask/src/gate.rs",
            drives: "the VERB: counts_report() over synthetic roots — an empty \
                     proof inventory, a README asserting a numeric harness total, \
                     and an unreadable README (each RED; the clean root GREEN)",
            calls: "counts_report",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "perf",
        proof: RedProof::Fixture {
            test: "every_perf_lane_can_turn_the_verb_red",
            file: "crates/xtask/src/gate.rs",
            drives: "the VERB: gate_perf_with() over a lane provider that fails ONE \
                     lane at a time, all ten in turn, each required to turn the \
                     verdict red on its own — plus a clean sweep proving the verb is \
                     not stuck red and asks for every lane once in order, and that \
                     the trend lane receives the real lanes_ok rather than a \
                     constant. What this catches is the vacuity a component test \
                     cannot: a lane computed and then DROPPED (`ok &= f()` slipping \
                     to `f();`), which makes the gate green by not listening. The \
                     lane VALUES are proven separately by perf.rs's own decision \
                     tests (compare_fails_below_floor, keyed_compare_fails_only_the_\
                     collapsed_metric, trend_same_box_regression_trips_and_other_\
                     boxes_do_not); this pins the wiring between them. NOT COVERED: \
                     that the live measurement bindings inside LivePerfLanes select \
                     the right corpora — that still needs a fixture workspace and a \
                     toolchain compile.",
            calls: "gate_perf_with",
            verb_level: true,
        },
    },
    RedFixture {
        gate: "lint",
        proof: RedProof::Fixture {
            test: "every_lint_lane_can_turn_the_verb_red",
            file: "crates/xtask/src/gate.rs",
            drives: "the VERB: gate_lint_with() over a lane provider that fails ONE \
                     lane at a time (tippy / trustfmt / guards), each required to \
                     turn the verdict red on its own, plus a clean sweep pinning \
                     order and arity. The fail-closed branches are driven for real \
                     by an_absent_toolchain_fails_each_lint_lane_closed_on_its_own, \
                     which runs LiveLintLanes against a stage2 dir holding neither \
                     targo-tippy nor cargo and requires each lane to answer false \
                     SEPARATELY — an earlier fixture asserted only their \
                     conjunction, which left any single arm free to stop failing \
                     closed unnoticed.",
            calls: "gate_lint_with",
            verb_level: true,
        },
    },
];

/// Run the non-vacuity obligation over the live tree and print its verdict —
/// including, on success, the HONEST SCORE (how many roster gates are actually
/// proven red-capable, and which are registered gaps), so the word GREEN is
/// never read without it. Returns `false` if the obligation is violated.
fn report_non_vacuity() -> bool {
    let root = workspace_root();
    let violations = non_vacuity_violations(&roster_names(), NON_VACUITY_REGISTRY, &|rel| {
        std::fs::read_to_string(root.join(rel)).ok()
    });
    let gaps: Vec<&str> = NON_VACUITY_REGISTRY
        .iter()
        .filter(|e| matches!(e.proof, RedProof::KnownGap { .. }))
        .map(|e| e.gate)
        .collect();
    if violations.is_empty() {
        // Report VERB-level and COMPONENT-level separately. Counting them together
        // said "N gates have a red fixture that plants a violation and asserts
        // FAILURE" while one of the N only demonstrated a component — the same
        // over-claim, in the same sentence position, as the verdict line this repo
        // corrected this morning. The score sits next to GREEN; it has to be exact.
        let component: Vec<&str> = NON_VACUITY_REGISTRY
            .iter()
            .filter(|e| {
                matches!(
                    e.proof,
                    RedProof::Fixture {
                        verb_level: false,
                        ..
                    }
                )
            })
            .map(|e| e.gate)
            .collect();
        let verb = ALL_ROSTER.len() - gaps.len() - component.len();
        eprintln!(
            "\n=== non-vacuity: {}/{} roster gate(s) proven red at the VERB; \
             {} at a COMPONENT only; {} never shown to fail ===",
            verb,
            ALL_ROSTER.len(),
            component.len(),
            gaps.len()
        );
        for c in &component {
            eprintln!(
                "  COMPONENT ONLY: `{c}`'s fixture drives part of the verb, not the verb — \
                 the verb itself has not been shown to go red."
            );
        }
        for gap in &gaps {
            eprintln!(
                "  KNOWN GAP: `{gap}` has NEVER been shown to fail — read its GREEN as unproven \
                 (reason in NON_VACUITY_REGISTRY)."
            );
        }
        true
    } else {
        eprintln!(
            "\n=== non-vacuity: FAILED — a `gate all` entry asserts more than anyone has shown it verifies ==="
        );
        for v in &violations {
            eprintln!("{v}");
        }
        false
    }
}

/// THE MECHANICAL OBLIGATION: every [`ALL_ROSTER`] gate is paired with a red
/// fixture or an explicit gap, and every named fixture EXISTS, is a `#[test]`,
/// and asserts a NEGATIVE outcome. Returns one line per violation (empty ⇒
/// discharged).
///
/// Pure in its inputs — `read(rel)` supplies file text — so the real meta-test
/// drives the real tree while this checker's OWN red fixtures drive planted
/// registries. WHAT IT CANNOT CHECK, stated plainly: no static check can tell
/// whether a test's assertions actually exercise the gate. It proves a named,
/// `#[test]`-annotated, NEGATIVE-asserting fixture exists; the `drives` field
/// records the scope claim for a human, and is not verified.
fn non_vacuity_violations(
    roster: &[&str],
    registry: &[RedFixture],
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (i, e) in registry.iter().enumerate() {
        if registry.iter().take(i).any(|p| p.gate == e.gate) {
            out.push(format!(
                "  '{}' has more than one NON_VACUITY_REGISTRY entry (which one is the proof?)",
                e.gate
            ));
        }
        if !roster.contains(&e.gate) {
            out.push(format!(
                "  '{}' is registered as a red fixture but is NOT in the `all` roster \
                 (stale entry: delete it, or restore the gate)",
                e.gate
            ));
        }
    }
    for gate in roster {
        let Some(entry) = registry.iter().find(|e| e.gate == *gate) else {
            out.push(format!(
                "  '{gate}' is in the `all` roster with NO NON_VACUITY_REGISTRY entry — \
                 nobody has shown it can fail. Add a red-fixture test that plants a \
                 violation and asserts FAILURE, or register an explicit KnownGap."
            ));
            continue;
        };
        match &entry.proof {
            RedProof::Fixture {
                test,
                file,
                drives,
                calls,
                ..
            } => {
                if drives.trim().is_empty() {
                    out.push(format!(
                        "  '{gate}': the fixture `{test}` records no `drives` scope — say \
                         whether it drives the verb or a component"
                    ));
                }
                let Some(text) = read(file) else {
                    out.push(format!(
                        "  '{gate}': the fixture file {file} could not be read — the \
                         registered proof does not exist"
                    ));
                    continue;
                };
                match test_fn_body(&text, test) {
                    Err(why) => out.push(format!("  '{gate}': fixture `{test}` in {file}: {why}")),
                    Ok(body) => {
                        // Comments are NOT source. Densifying the raw body let
                        // `// we used to assert!(!ok) here` satisfy the negative-
                        // assertion check — a fixture proved by its own commentary.
                        let code: String = body
                            .lines()
                            .map(|l| l.split_once("//").map_or(l, |(before, _)| before))
                            .collect::<Vec<_>>()
                            .join("\n");
                        // Whitespace-insensitive: the formatter, not the author,
                        // decides whether `assert!(` and `!ok` share a line.
                        let dense: String = code.chars().filter(|c| !c.is_whitespace()).collect();
                        if !dense.contains("assert!(!") {
                            out.push(format!(
                                "  '{gate}': fixture `{test}` in {file} contains no NEGATIVE \
                                 assertion (`assert!(!…)`) — a red fixture must assert the \
                                 gate FAILS, not that it passes"
                            ));
                        }
                        // BIND THE FIXTURE TO THE GATE. Without this the obligation
                        // is satisfied by any correctly-named `#[test]` containing a
                        // negative assertion — `assert!(!false);` scored as proof.
                        // A substring cannot prove the call is REACHED, but it does
                        // stop a fixture that never mentions the gate from claiming it.
                        if !dense.contains(
                            &calls
                                .chars()
                                .filter(|c| !c.is_whitespace())
                                .collect::<String>(),
                        ) {
                            out.push(format!(
                                "  '{gate}': fixture `{test}` in {file} never mentions `{calls}` \
                                 — it cannot be a demonstration that THIS gate goes red"
                            ));
                        }
                    }
                }
            }
            RedProof::KnownGap { reason } => {
                if reason.trim().len() < MIN_GAP_REASON {
                    out.push(format!(
                        "  '{gate}': the KnownGap reason is {} chars; a gap must carry a real \
                         reason (>= {MIN_GAP_REASON}) saying why a fixture is infeasible and \
                         what would close it",
                        reason.trim().len()
                    ));
                }
            }
        }
    }
    out
}

/// The source text of `#[test] fn <name>() { … }`, or why it could not be
/// located. Segmentation uses rustfmt's closing-brace-at-fn-indent invariant —
/// the same lexical contract the census walker relies on (and the same honest
/// limit: it is a text scan, not a parse).
fn test_fn_body<'a>(text: &'a str, name: &str) -> Result<&'a str, String> {
    let needle = format!("fn {name}(");
    // The FIRST occurrence that starts a definition line: a mention inside a
    // string or a trailing comment must not be mistaken for the fn itself.
    let (line_start, at) = text
        .match_indices(&needle)
        .map(|(at, _)| (text[..at].rfind('\n').map_or(0, |i| i + 1), at))
        .find(|(line_start, at)| text[*line_start..*at].chars().all(char::is_whitespace))
        .ok_or_else(|| {
            format!("no definition line starting `{needle}` in the file — renamed or deleted?")
        })?;
    let indent = &text[line_start..at];
    // Walk back over attributes / comments / blank lines: `#[test]` must be
    // among them, or this is a helper fn rather than a test.
    let mut attributed = false;
    let mut ignored = false;
    for line in text[..line_start].lines().rev() {
        let t = line.trim();
        // `#[ignore]` makes a fixture that EXISTS but never RUNS — proof on paper
        // and nothing at the moment it is needed, which is this obligation's whole
        // subject. The walk-back already reads these lines, so rejecting it is free.
        if t.starts_with("#[ignore") {
            ignored = true;
        }
        if t == "#[test]" {
            attributed = true;
            break;
        }
        if !(t.is_empty() || t.starts_with("//") || t.starts_with("#[")) {
            break;
        }
    }
    if !attributed {
        return Err(format!(
            "`{name}` is not annotated `#[test]` — it cannot fail the build"
        ));
    }
    if ignored {
        return Err(format!(
            "`{name}` is `#[ignore]`d — it exists but never runs, so it proves nothing"
        ));
    }
    let close = format!("\n{indent}}}");
    let end = text[at..]
        .find(&close)
        .map(|i| at + i + close.len())
        .ok_or_else(|| {
            format!("no closing brace at `{name}`'s indent — is the file rustfmt-clean?")
        })?;
    Ok(&text[line_start..end])
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
///
/// THIS FILE is always excluded, and that exclusion is load-bearing rather than
/// tidy. `WITNESS_REGISTRY` spells each `Proof::Needle` out as a string literal
/// on an ordinary (non-comment) line of gate.rs, so while gate.rs was in the
/// scan every needle witnessed ITSELF and `gate drift` could not go red. MEASURED
/// 2026-07-31: `grep -rn handle_decdld crates apps` returned exactly one hit —
/// the registry entry at the `soft_fonts` witness — and flipping `soft_fonts` to
/// `true` in `aterm_capabilities()` with no DRCS code anywhere still printed
/// "gate drift: GREEN — 16 advertised capabilities all have implementation
/// witnesses". `gate_fault` already carves itself out for exactly this reason
/// (see its `xtask/src/gate.rs` skip); the witness scan had been missed.
fn impl_source_files(root: &Path, exclude_suffix: Option<&str>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates"), &mut files);
    files
        .into_iter()
        .filter(|p| !is_test_file(p))
        .filter(|p| !p.to_string_lossy().ends_with("xtask/src/gate.rs"))
        .filter(|p| match exclude_suffix {
            Some(suf) => !p.to_string_lossy().ends_with(suf),
            None => true,
        })
        .collect()
}

/// Does any non-test source line under `root/crates/` contain `needle`
/// (excluding the advertise site `terminal_core.rs`)?
fn needle_present(root: &Path, needle: &str) -> bool {
    for file in impl_source_files(root, Some("terminal_core.rs")) {
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

/// Count non-test source lines under `root/consumer_path` (a file OR a dir)
/// that reference `symbol` as a USE, not its definition. The `fn <symbol>`
/// definition line is excluded so pointing the check at the crate that also
/// DEFINES the symbol still measures real consumers.
fn consumer_count(root: &Path, symbol: &str, consumer_path: &str) -> usize {
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
fn parse_advertised_caps(root: &Path) -> Result<Vec<(String, bool)>, String> {
    let path = root.join("crates/aterm-types/src/terminal_core.rs");
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
    let (ok, log) = drift_report(&workspace_root(), WITNESS_REGISTRY);
    eprint!("{log}");
    ok
}

/// `gate drift` over an arbitrary root and witness registry, returning the
/// verdict plus the transcript the verb prints. Rooted so a red fixture can
/// plant a violation in a copy of the tree — until 2026-08-01 nothing had ever
/// driven this verb to FAILURE (the drift fix that day proved the witness scan
/// no longer witnesses itself, which is a precondition, not the verdict).
fn drift_report(root: &Path, registry: &[Witness]) -> (bool, String) {
    let mut log = String::new();
    let _ = writeln!(log, "=== gate drift (advertise-vs-implement) ===");
    let caps = match parse_advertised_caps(root) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(log, "gate drift: FAILED to parse capabilities: {e}");
            return (false, log);
        }
    };
    if caps.is_empty() {
        let _ = writeln!(
            log,
            "gate drift: FAILED — parsed zero capabilities (parser broke?)"
        );
        return (false, log);
    }
    let mut failures = Vec::new();
    for (cap, advertised) in &caps {
        let entry = registry.iter().find(|w| w.cap == cap);
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
                    Proof::Needle(n) => needle_present(root, n),
                    Proof::Path(p) => root.join(p).exists(),
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
        let _ = writeln!(
            log,
            "gate drift: GREEN — {advertised_true} advertised capabilities all have implementation witnesses; \
             {} honestly advertised false.",
            caps.len() - advertised_true
        );
        (true, log)
    } else {
        let _ = writeln!(log, "gate drift: FAILED — advertise-vs-implement drift:");
        for f in &failures {
            let _ = writeln!(log, "{f}");
        }
        let _ = writeln!(
            log,
            "  Fix: implement the capability, or set its `aterm_capabilities()` flag false \
             (honest non-advertisement)."
        );
        (false, log)
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
    let (ok, log) = dormant_report(&workspace_root(), DORMANCY_REGISTRY);
    eprint!("{log}");
    ok
}

/// `gate dormant` over an arbitrary root and registry, returning the verdict
/// plus the transcript the verb prints. Rooted so a red fixture can delete the
/// only consumer in a COPY of the real file and watch the gate go red.
fn dormant_report(root: &Path, registry: &[DormantWatch]) -> (bool, String) {
    let mut log = String::new();
    let _ = writeln!(log, "=== gate dormant (computed-but-unconsumed) ===");
    let mut failures = Vec::new();
    let mut pending = 0;
    for w in registry {
        let count = consumer_count(root, w.producer, w.consumer_path);
        if w.enforced && count == 0 {
            failures.push(format!(
                "  '{}' is DORMANT: `{}` has zero live consumers in {} (computed but never used)",
                w.feature, w.producer, w.consumer_path
            ));
        } else if !w.enforced {
            pending += 1;
            let _ = writeln!(
                log,
                "  pending: '{}' (`{}` -> {}): {} consumer(s); not yet enforced",
                w.feature, w.producer, w.consumer_path, count
            );
        }
    }
    if failures.is_empty() {
        let _ = writeln!(
            log,
            "gate dormant: GREEN — {} enforced feature(s) consumed, {pending} pending wiring.",
            registry.iter().filter(|w| w.enforced).count()
        );
        (true, log)
    } else {
        let _ = writeln!(
            log,
            "gate dormant: FAILED — features computed but never consumed:"
        );
        for f in &failures {
            let _ = writeln!(log, "{f}");
        }
        (false, log)
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

/// Can a `trust` toolchain be reached through a rustup proxy? Only consulted
/// when there is no stage2 `trustc` to run directly.
fn rustup_has_trust() -> bool {
    Command::new("rustup")
        .args(["run", "trust", "trustc", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Resolve the driver that compiles the certified corpus: the program to spawn
/// plus the argv prefix that reaches `trustc` through it.
///
/// The stage2 tree IS the toolchain here, so it wins; rustup is only ONE way of
/// reaching a trustc, never the ground truth (`rust-toolchain.toml` names
/// `trust`, and the thing that satisfies it is `$TRUST_STAGE2_BIN` — the same
/// resolution tools/verify.sh, .githooks/pre-push and `gate lint` already use).
/// Probing rustup FIRST inverted the intended skip: MEASURED 2026-07-31 on the
/// owner's box, `command -v rustup` is not found while `$HOME/trust/build/host/
/// stage2/bin/trustc` runs, so the gate SKIP-passed having compiled zero corpus
/// files on the only machine that has the toolchain. `None` — neither driver
/// exists — is the one honest skip.
///
/// `have_rustup` is a thunk so the fallback probe never spawns a process when
/// the stage2 driver is present.
fn certified_driver(
    stage2_bin: &Path,
    have_rustup: impl FnOnce() -> bool,
) -> Option<(PathBuf, &'static [&'static str])> {
    let stage2_trustc = stage2_bin.join("trustc");
    if stage2_trustc.is_file() {
        return Some((stage2_trustc, &[]));
    }
    if have_rustup() {
        return Some((PathBuf::from("rustup"), &["run", "trust", "trustc"]));
    }
    None
}

/// The verification flag the corpus compiles under. MEASURED 2026-07-31 against
/// trustc 0.1.0 (rustc 1.99.0-dev, ccc7939e4): `rustc -Z help | grep trust`
/// lists NEITHER `trust-verify-full` NOR `trust-verify-certified` — both spellings
/// this gate was written against are gone, and trustc rejects the old one with
/// "error: unknown unstable option: `trust-verify-full`". `-Ztrust-policy=certify`
/// is the live successor; its own help text calls it "the release gate, `targo
/// trust certify`" and says it "demands FULL static discharge and fails on every
/// unproved obligation". Same class of rename the tree already documents for
/// `-Zno-trust-verify=yes` -> `-Ztrust-verify=off` (.cargo/config.toml, verify.sh).
const CERTIFY_FLAG: &str = "-Ztrust-policy=certify";

/// Did the driver reject the verification flag itself, rather than return a
/// verdict about the corpus? Matched on rustc's exact wording, MEASURED from the
/// stale-flag run: "error: unknown unstable option: `trust-verify-full`".
fn flag_was_rejected(stderr: &str) -> bool {
    stderr.contains("unknown unstable option")
}

/// One `note: Trust verification: …` block from the driver's stderr.
struct CertifyBlock {
    proved: usize,
    failed: usize,
    unknown: usize,
    timed_out: usize,
    runtime_checked: usize,
    obligations: usize,
    /// The `= note: of which N kernel-certified …` follow-up, if it was emitted.
    kernel_certified: Option<usize>,
}

/// The integer immediately preceding `label` on `line` ("… 3 proved, …" -> 3).
fn count_before(line: &str, label: &str) -> Option<usize> {
    let head = line[..line.find(label)?].trim_end();
    let digits: String = head
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

/// THE PARSE CONTRACT over trustc's verification notes. MEASURED 2026-08-01
/// against `trustc 0.1.0` (rustc 1.99.0-dev, ccc7939e4) compiling the live
/// corpus — these two lines verbatim, with the source-snippet lines trustc
/// prints between them elided (they carry no counters):
///
/// ```text
/// note: Trust verification: 1 proved, 0 failed, 0 unknown, 0 timed out, 0 runtime-checked out of 1 obligation(s)
///    = note: of which 1 kernel-certified by the clean CIC kernel (zero-trust re-check; …)
/// ```
///
/// Each `Trust verification:` line opens a block; the following
/// `kernel-certified` note (emitted per block) fills it in. Deliberately
/// PERMISSIVE about ordering and surrounding text, STRICT about the counters it
/// needs: a line it cannot parse yields no block, and zero blocks is a FAILURE
/// in [`judge_kernel_certification`] rather than a silent pass.
fn parse_certify_blocks(stderr: &str) -> Vec<CertifyBlock> {
    let mut out: Vec<CertifyBlock> = Vec::new();
    for line in stderr.lines() {
        if line.contains("Trust verification:") {
            let get = |label| count_before(line, label);
            if let (Some(proved), Some(failed), Some(unknown), Some(timed_out), Some(rt), Some(n)) = (
                get(" proved"),
                get(" failed"),
                get(" unknown"),
                get(" timed out"),
                get(" runtime-checked"),
                get(" obligation(s)"),
            ) {
                out.push(CertifyBlock {
                    proved,
                    failed,
                    unknown,
                    timed_out,
                    runtime_checked: rt,
                    obligations: n,
                    kernel_certified: None,
                });
            }
        } else if line.contains("kernel-certified")
            && let (Some(block), Some(n)) =
                (out.last_mut(), count_before(line, " kernel-certified"))
        {
            block.kernel_certified = Some(n);
        }
    }
    out
}

/// KERNEL CERTIFICATION, asserted rather than merely surfaced: every obligation
/// in every reported block must be proved AND re-checked by the clean CIC
/// kernel. Returns the total kernel-certified obligation count, or the reason
/// the standard was not met.
///
/// FAIL-CLOSED ON ITS OWN CONTRACT: no parsable block, or a block with no
/// `kernel-certified` note, is an ERROR ("the note format changed") — never a
/// pass. That is the whole point: the previous version of this gate printed
/// GREEN off the exit code alone and said so honestly in its header; a parser
/// that silently found nothing would be a regression to exactly that state
/// while claiming more.
fn judge_kernel_certification(stderr: &str) -> Result<usize, String> {
    let blocks = parse_certify_blocks(stderr);
    if blocks.is_empty() {
        return Err(
            "PARSE CONTRACT BROKEN — no `note: Trust verification: N proved, … out of M \
             obligation(s)` line in the driver's output. Nothing was checked. Re-probe the \
             driver's note format and update parse_certify_blocks()."
                .to_string(),
        );
    }
    let mut certified = 0;
    for (i, b) in blocks.iter().enumerate() {
        let Some(kc) = b.kernel_certified else {
            return Err(format!(
                "PARSE CONTRACT BROKEN — verification block {i} reported no `of which N \
                 kernel-certified` note. Either the driver stopped emitting it (re-probe and \
                 update the contract) or those obligations are NOT kernel-certified."
            ));
        };
        if b.failed + b.unknown + b.timed_out > 0 {
            return Err(format!(
                "block {i}: {} failed, {} unknown, {} timed out — not fully discharged",
                b.failed, b.unknown, b.timed_out
            ));
        }
        if b.proved != b.obligations || b.runtime_checked > 0 {
            return Err(format!(
                "block {i}: only {} of {} obligation(s) statically proved ({} runtime-checked) — \
                 a runtime check is not a kernel certification",
                b.proved, b.obligations, b.runtime_checked
            ));
        }
        if kc != b.obligations {
            return Err(format!(
                "block {i}: {kc} of {} obligation(s) kernel-certified — the rest are \
                 solver-trusted. THIS is the regression the exit code cannot see.",
                b.obligations
            ));
        }
        certified += kc;
    }
    Ok(certified)
}

/// `gate certified` — enforce the KERNEL-CERTIFIED standard locally.
///
/// Compiles the curated `crates/xtask/certified-corpus/*.rs` (functions whose
/// Level-0 safety obligations the clean zero-trust CIC kernel can reconstruct)
/// through the Trust driver under [`CERTIFY_FLAG`] and requires exit 0.
///
/// TWO independent conditions, both required:
///   * EXIT 0 under `certify` — FULL STATIC DISCHARGE. MEASURED RED: an
///     obligation the solver cannot discharge (probe: a nonlinear `a * b`
///     bound) returns unknown and trustc aborts non-zero.
///   * [`judge_kernel_certification`] over the driver's notes — every
///     obligation of every block proved AND kernel-certified by the clean CIC
///     kernel. This closes what the header used to list as NOT PROVEN: a
///     regression from kernel-certified to merely solver-trusted keeps exit 0
///     (certify fails only on UNPROVED obligations) and is now caught by the
///     parse contract instead of being left for a human to notice in a note.
///
/// The parse contract's own fragility is handled fail-closed: if the note
/// format changes, the gate goes RED saying "PARSE CONTRACT BROKEN", exactly as
/// a renamed `-Z` flag goes RED as "STALE-FLAG". It never degrades to a pass.
///
/// Skips only when NEITHER a stage2 `trustc` nor a rustup `trust` toolchain
/// exists, so it never blocks a normal build (the no-CI/local-gate model).
fn gate_certified() -> bool {
    eprintln!(
        "=== gate certified (corpus must fully discharge under {CERTIFY_FLAG}, \
         every obligation kernel-certified) ==="
    );
    let tools = trust_stage2_bin();
    let Some((driver, prefix)) = certified_driver(&tools, rustup_has_trust) else {
        eprintln!(
            "gate certified: SKIP — no trustc at {} and no `trust` rustup toolchain either.",
            tools.join("trustc").display()
        );
        return true;
    };
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
        // Captured rather than inherited so a REJECTED FLAG can be told apart
        // from a verification verdict — see `flag_was_rejected`. The notes are
        // forwarded verbatim either way, so nothing a human would have seen is
        // lost.
        let output = Command::new(&driver)
            .args(prefix)
            .args(["--edition", "2021", "--crate-type", "lib"])
            .arg(f)
            .arg(CERTIFY_FLAG)
            .arg("-o")
            .arg(&rlib)
            .current_dir(workspace_root())
            .output();
        match output {
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                eprint!("{stderr}");
                if o.status.success() {
                    // Exit 0 is FULL STATIC DISCHARGE. Kernel certification is a
                    // STRICTLY STRONGER claim that lives only in the notes — so
                    // read them, and fail closed if they are absent or short.
                    match judge_kernel_certification(&stderr) {
                        Ok(n) => eprintln!(
                            "  CERTIFIED      {name} — {n} obligation(s) kernel-certified by the \
                             clean CIC kernel"
                        ),
                        Err(why) => {
                            eprintln!("  NOT-CERTIFIED  {name} — {why}");
                            all_ok = false;
                        }
                    }
                } else if flag_was_rejected(&stderr) {
                    // This is an ENVIRONMENT/STALE-FLAG break, and reporting it
                    // as "the corpus regressed" would send the reader hunting a
                    // proof problem that does not exist. Trust renames unstable
                    // options; the gate must say so in its own words.
                    eprintln!(
                        "  STALE-FLAG     {name} — {} rejected `{CERTIFY_FLAG}` as an unknown \
                         unstable option. The corpus was NOT verified. Re-probe with \
                         `trustc -Z help | grep trust` and update CERTIFY_FLAG.",
                        driver.display()
                    );
                    all_ok = false;
                } else {
                    eprintln!(
                        "  NOT-DISCHARGED {name} (exit {:?}) — an obligation is unproved under \
                         the certify policy",
                        o.status.code()
                    );
                    all_ok = false;
                }
            }
            Err(e) => {
                eprintln!("  ERROR          {name}: {e}");
                all_ok = false;
            }
        }
    }
    // Both conditions are now asserted, so say both — and no more. What remains
    // outside this gate's reach is the CORPUS itself: it proves nothing about
    // functions nobody added to `certified-corpus/`.
    if all_ok {
        eprintln!(
            "gate certified: GREEN — every corpus obligation FULLY DISCHARGED under \
             {CERTIFY_FLAG} and KERNEL-CERTIFIED by the clean CIC kernel (counts per file above)."
        );
    } else {
        eprintln!(
            "gate certified: FAILED — the corpus did not fully discharge, an obligation was \
             solver-trusted rather than kernel-certified, or the flag/note contract is stale; \
             see the per-file line."
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
    cwd: &Path,
) -> bool {
    eprintln!("  $ {program} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
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

/// The two signatures branded Tippy emits when its TOOLCHAIN-IDENTITY guard
/// trips. Neither is a lint result: the run can lint completely clean, print
/// `Finished dev profile`, and still exit non-zero.
///
/// `AuthenticatedDriverExecution::capture` snapshots the stage2 sysroot's whole
/// ANCESTOR-DIRECTORY CHAIN (up to `/`) plus `trustc`/`tippy-driver` — dev, ino,
/// mode, nlink, uid, gid, mtime, ctime for each — and re-validates before AND
/// after the guarded operation. An entry created, removed or renamed in ANY
/// ancestor mid-run aborts it. A `$HOME/trust` rebuild relinking stage2 is the
/// realistic trigger; sampling this box's real chain at 4 Hz for 15 minutes
/// found zero churn, so it is rare but genuinely environmental.
/// Attempts allowed for a tippy run aborted by the identity guard. Three is
/// generous for an abort whose trigger is a one-off filesystem event; a run
/// that trips it three times is telling you the tree is not quiet.
const TIPPY_IDENTITY_RETRIES: u32 = 3;

const TIPPY_IDENTITY_ABORTS: [&str; 2] = [
    "branded Tippy driver identity changed",
    "compiler identity changed while Targo was running",
];

/// [`run_shell_env`] that also TEES stderr, so a caller can tell a transient
/// environment abort from a real finding. Only the tippy lane needs this; every
/// other lane's failure means what it says.
fn run_shell_env_capturing(
    desc: &str,
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    path_prefix: Option<&Path>,
    cwd: &Path,
) -> (bool, String) {
    use std::io::Read as _;
    eprintln!("  $ {program} {}", args.join(" "));
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stderr(std::process::Stdio::piped());
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
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  {desc}: could not run ({e})");
            return (false, String::new());
        }
    };
    let mut captured = String::new();
    if let Some(mut err) = child.stderr.take() {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
        captured = String::from_utf8_lossy(&buf).into_owned();
        // TEE: the operator must still see the findings live, exactly as the
        // uncaptured lanes print them.
        eprint!("{captured}");
    }
    match child.wait() {
        Ok(s) if s.success() => (true, captured),
        Ok(s) => {
            eprintln!("  {desc}: FAILED (exit {:?})", s.code());
            (false, captured)
        }
        Err(e) => {
            eprintln!("  {desc}: could not run ({e})");
            (false, captured)
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

/// Run the two repo guard scripts (`tools/grep_guard.sh`, `tools/license_check.sh`),
/// FAILING CLOSED when one is missing.
///
/// Both take the repo root as their argument, and both are executed directly so
/// their `#!/usr/bin/env bash` shebang is honored — they use bash-only process
/// substitution and break under `sh`.
///
/// A missing guard is a gate FAILURE, not a silent pass: `ok &= …` inside an
/// `if …exists()` with no `else` let `gate lint` print GREEN with two of its four
/// components never run, while tools/verify.sh's `run_guard` fails closed on the
/// identical condition ("$label missing or not executable"). Both scripts exist
/// today, so this branch has never fired — but the tippy and trustfmt components
/// directly above were hardened to fail closed and this pair was not, which is
/// precisely the verb/script disagreement the header comment says cannot happen.
fn run_repo_guards(root: &Path) -> bool {
    let root_str = root.to_string_lossy().into_owned();
    let mut ok = true;
    for (label, rel) in [
        ("grep_guard", "tools/grep_guard.sh"),
        ("license_check", "tools/license_check.sh"),
    ] {
        let script = root.join(rel);
        if script.exists() {
            ok &= run_shell(label, &script.to_string_lossy(), &[&root_str]);
        } else {
            eprintln!(
                "  {label}: NOT RUN — {} is missing. Its checks did not run; this is not a clean lint.",
                script.display()
            );
            ok = false;
        }
    }
    ok
}

/// The three lanes `gate lint` ANDs into one verdict, in run order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LintLane {
    Tippy,
    Trustfmt,
    Guards,
}

const LINT_LANES: [LintLane; 3] = [LintLane::Tippy, LintLane::Trustfmt, LintLane::Guards];

/// Where a `gate lint` lane's verdict comes from — the seam that lets a test
/// fail ONE lane and watch the verb follow. Testing the three together only
/// proves their conjunction; it leaves each individual arm free to stop
/// failing closed without anything noticing.
trait LintLanes {
    fn run(&mut self, lane: LintLane) -> bool;
}

/// The real lanes: Trust's linter and formatter over a real root.
struct LiveLintLanes<'a> {
    root: &'a Path,
    tools: &'a Path,
}

impl LintLanes for LiveLintLanes<'_> {
    fn run(&mut self, lane: LintLane) -> bool {
        match lane {
            // THE linter here is Trust's, not stock Rust's. The stage2 tree ships
            // `targo-tippy` and ships NO `cargo-clippy`, so plain `cargo clippy`
            // resolves whatever `cargo-clippy` is on PATH — Homebrew's, typically
            // — which drives a stable rustc and dies on this workspace's
            // `-Ztrust-verify=off` before linting a line, then reports an
            // environment break in the shape of a lint finding.
            //
            // Resolve and invoke exactly as tools/verify.sh does, so verb and
            // gate cannot disagree about what "lint" means: the same candidate
            // order, the same separate CARGO_TARGET_DIR (tippy's flags differ
            // from the main build's; one shared dir makes them thrash each
            // other's cache), and tippy's own directory first on PATH so it
            // finds its `tippy-driver`.
            LintLane::Tippy => match resolve_tippy(self.tools) {
                // RETRY THE IDENTITY ABORT, AND ONLY THAT. See
                // [`TIPPY_IDENTITY_ABORTS`]: the guard can trip on a clean run,
                // so reporting it as red is a lie in one direction — and
                // retrying anything else would be a lie in the far worse
                // direction, turning a genuine `-D warnings` failure into a
                // pass. The match is on the abort signature alone.
                //
                // The window is wider than the compile: it spans cargo's
                // BUILD-LOCK WAIT, so with one shared `target-tippy` a
                // two-second leaf crate can sit exposed for minutes queued
                // behind another build. That is why the crates observed failing
                // were the FAST ones, and why concurrent invokers should each
                // use their own `CARGO_TARGET_DIR`.
                Some(bin) => {
                    let mut outcome = false;
                    for attempt in 1..=TIPPY_IDENTITY_RETRIES {
                        let (ok, stderr) = run_shell_env_capturing(
                            "tippy",
                            &bin.to_string_lossy(),
                            &["--workspace", "--all-targets", "--", "-D", "warnings"],
                            &[
                                (
                                    "CARGO_TARGET_DIR",
                                    self.root.join("target-tippy").to_string_lossy().as_ref(),
                                ),
                                ("TRUST_NO_MIGRATE_WARN", "1"),
                            ],
                            Some(self.tools),
                            self.root,
                        );
                        if ok {
                            outcome = true;
                            break;
                        }
                        let aborted = TIPPY_IDENTITY_ABORTS
                            .iter()
                            .any(|sig| stderr.contains(sig));
                        if !aborted {
                            // A real finding (or any other failure): report it.
                            break;
                        }
                        if attempt == TIPPY_IDENTITY_RETRIES {
                            eprintln!(
                                "  tippy: toolchain-identity abort on all {TIPPY_IDENTITY_RETRIES} \
                                 attempts — NOTHING WAS LINTED. This is an environment abort, not \
                                 a clean lint: something changed an ancestor of the stage2 sysroot \
                                 mid-run (a $HOME/trust rebuild will do it). Re-run once the tree is \
                                 quiet."
                            );
                            break;
                        }
                        eprintln!(
                            "  tippy: toolchain-identity abort (attempt \
                             {attempt}/{TIPPY_IDENTITY_RETRIES}) — transient, retrying"
                        );
                    }
                    outcome
                }
                None => {
                    eprintln!(
                        "  tippy: NOT RUN — no targo-tippy/targo-clippy in {}. Nothing was \
                         linted; this is not a clean lint. Build the Trust stage2 \
                         (`python3 x.py build --stage 2` in $HOME/trust) and re-run.",
                        self.tools.display()
                    );
                    false
                }
            },
            // `fmt` goes through the stage2 cargo so it picks up Trust's
            // `trustfmt` from the same directory, rather than a stock rustfmt
            // that would reformat to a different style than the tree is in.
            LintLane::Trustfmt => {
                let stage2_cargo = self.tools.join("cargo");
                if stage2_cargo.is_file() {
                    run_shell_env(
                        "trustfmt",
                        &stage2_cargo.to_string_lossy(),
                        &["fmt", "--all", "--", "--check"],
                        &[],
                        Some(self.tools),
                        self.root,
                    )
                } else {
                    eprintln!(
                        "  trustfmt: NOT RUN — no cargo in {}. Formatting was not checked.",
                        self.tools.display()
                    );
                    false
                }
            }
            LintLane::Guards => run_repo_guards(self.root),
        }
    }
}

/// The tippy binary in `tools`, by the same candidate order `tools/verify.sh`
/// uses. `None` means NOT RUN — never "clean".
fn resolve_tippy(tools: &Path) -> Option<PathBuf> {
    ["targo-tippy", "targo-clippy"]
        .iter()
        .map(|name| tools.join(name))
        .find(|path| path.is_file())
}

fn gate_lint() -> bool {
    let root = workspace_root();
    let tools = trust_stage2_bin();
    gate_lint_with(&mut LiveLintLanes {
        root: &root,
        tools: &tools,
    })
}

/// The `gate lint` VERB: run every lane, AND every result, report. Every lane
/// runs even after one fails — a lint report that stops at the first finding
/// tells you less than one that ran everything.
fn gate_lint_with(lanes: &mut dyn LintLanes) -> bool {
    eprintln!("=== gate lint (tippy -D warnings + trustfmt + guards) ===");
    let mut ok = true;
    for lane in LINT_LANES {
        ok &= lanes.run(lane);
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
fn kani_proof_counts(root: &Path) -> std::io::Result<(usize, usize)> {
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
    let (ok, log) = counts_report(&workspace_root());
    eprint!("{log}");
    ok
}

/// `gate counts` over an arbitrary root, returning the verdict plus the
/// transcript the verb prints. Rooted so a red fixture can plant each of the
/// three failure conditions (empty inventory, hand-maintained README total,
/// unreadable README) and watch the gate go red on every one.
fn counts_report(root: &Path) -> (bool, String) {
    let mut log = String::new();
    let _ = writeln!(
        log,
        "=== gate counts (computed-only crate proof inventory) ==="
    );
    let (harnesses, files) = match kani_proof_counts(root) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(log, "gate counts: FAILED — could not scan workspace ({e})");
            return (false, log);
        }
    };

    let readme_path = root.join("README.md");
    let readme = match std::fs::read_to_string(&readme_path) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(
                log,
                "gate counts: FAILED — could not read {readme_path:?} ({e})"
            );
            return (false, log);
        }
    };

    if !proof_inventory_is_valid(harnesses, files) {
        let _ = writeln!(
            log,
            "gate counts: FAILED — invalid/empty crate proof inventory \
             ({harnesses} harnesses across {files} files)"
        );
        return (false, log);
    }
    if readme_asserts_proof_inventory(&readme) {
        let _ = writeln!(
            log,
            "gate counts: FAILED — README.md contains a hand-maintained numeric \
             `#[kani::proof]` total; use this computed inventory instead"
        );
        return (false, log);
    }

    let _ = writeln!(
        log,
        "gate counts: GREEN — live inventory: {harnesses} ordinary `#[kani::proof]` \
         harnesses across {files} crate files; no hand-maintained README total"
    );
    (true, log)
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
    let (ok, log) = fault_report(&workspace_root());
    eprint!("{log}");
    ok
}

/// `gate fault` over an arbitrary root, returning the verdict plus the
/// transcript the verb prints. Rooted so a red fixture can plant an unarmed
/// injection site (and its mirror, an armed name with no site) in a synthetic
/// tree and watch both directions go red.
fn fault_report(root: &Path) -> (bool, String) {
    let mut log = String::new();
    let _ = writeln!(log, "=== gate fault (injected-but-unexercised) ===");
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates"), &mut files);

    let mut injected: std::collections::BTreeMap<String, String> = Default::default();
    let mut armed: std::collections::BTreeSet<String> = Default::default();
    for file in &files {
        let rel = file
            .strip_prefix(root)
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
        let _ = writeln!(
            log,
            "gate fault: GREEN — {} fault point(s) injected, all exercised by a test.",
            injected.len()
        );
        (true, log)
    } else {
        let _ = writeln!(
            log,
            "gate fault: FAILED — fault-injection registry is inconsistent:"
        );
        for f in &failures {
            let _ = writeln!(log, "{f}");
        }
        (false, log)
    }
}

// ---------------------------------------------------------------------------
// G-PERF (M2): the DETERMINISTIC memory budget is enforced now; the wall-clock
// throughput baseline (tools/golden/perf-baseline.json) is the remaining piece.
// ---------------------------------------------------------------------------

/// The ten lanes `gate perf` ANDs into one verdict, in run order.
///
/// Named so the verb's aggregation is testable. The failure this guards against
/// is not a lane returning the wrong answer — `perf.rs` has its own decision
/// tests for that — it is a lane's answer being COMPUTED AND THEN DROPPED, the
/// one-character `ok &= f()` -> `f();` slip that makes a gate green by not
/// listening. That slip is invisible to review and to every component test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PerfLane {
    MemBudget,
    PerfScaling,
    Throughput,
    Pathological,
    ScrollScrub,
    Search,
    Restore,
    Resize,
    Wasm,
    Trend,
}

/// The nine MEASURING lanes, in the order the verb runs them. `Trend` is not
/// here: it is the tenth, and it needs the AND of these nine as an input.
const PERF_MEASURING_LANES: [PerfLane; 9] = [
    PerfLane::MemBudget,
    PerfLane::PerfScaling,
    PerfLane::Throughput,
    PerfLane::Pathological,
    PerfLane::ScrollScrub,
    PerfLane::Search,
    PerfLane::Restore,
    PerfLane::Resize,
    PerfLane::Wasm,
];

/// Where a `gate perf` lane's verdict comes from. The real implementation
/// measures the live tree; a test substitutes one that fails a chosen lane.
trait PerfLanes {
    /// `lanes_ok` is the AND of the nine measuring lanes, and is meaningful
    /// only for [`PerfLane::Trend`].
    fn run(
        &mut self,
        lane: PerfLane,
        trend: &mut Vec<crate::perf::TrendSample>,
        lanes_ok: bool,
    ) -> bool;
}

/// The live-tree lanes: two deterministic allocation gates spawned here, plus
/// the eight in `perf.rs` that measure and compare against committed baselines.
struct LivePerfLanes;

impl PerfLanes for LivePerfLanes {
    fn run(
        &mut self,
        lane: PerfLane,
        trend: &mut Vec<crate::perf::TrendSample>,
        lanes_ok: bool,
    ) -> bool {
        match lane {
            // Both are DETERMINISTIC (allocation-based, no wall-clock) so they
            // never flake, and self-contained in aterm-core. MEM-BUDGET is a
            // retained-heap ceiling; PERF-BASELINE catches per-line/per-cell
            // O(n)-allocation regressions in steady-state processing.
            PerfLane::MemBudget => run_shell(
                "mem-budget",
                "cargo",
                &["test", "-p", "aterm-core", "--test", "mem_budget"],
            ),
            PerfLane::PerfScaling => run_shell(
                "perf-scaling",
                "cargo",
                &["test", "-p", "aterm-core", "--test", "perf_scaling"],
            ),
            // Median-of-N MB/s of the parse/process hot path against a committed,
            // generously-thresholded baseline: catches a CATASTROPHIC regression
            // (debug-build slip, algorithmic blow-up, lock contention) but never
            // flakes on a slower box. Report-only PASS with no baseline.
            PerfLane::Throughput => crate::perf::gate_throughput(trend),
            // Per-corpus hostile-input floors, each against its OWN baseline, so a
            // class-specific regression cannot hide behind a healthy mixed number.
            PerfLane::Pathological => crate::perf::gate_pathological(trend),
            // Scrollback-scrub read-path floors over a 100k+-line tiered fill —
            // the dimension the compressed tiers are structurally most at risk of
            // losing to an all-RAM page list.
            PerfLane::ScrollScrub => crate::perf::gate_scroll_scrub(trend),
            // E0 keyed-floor lanes. `resize` carries the 42s-freeze-class ABSOLUTE
            // fences, which hold even with no baseline; `wasm` skips with notice on
            // a box without the node/wasm toolchain.
            PerfLane::Search => crate::perf::gate_search(trend),
            PerfLane::Restore => crate::perf::gate_restore(trend),
            PerfLane::Resize => crate::perf::gate_resize(trend),
            PerfLane::Wasm => crate::perf::gate_wasm(trend),
            // Same-box trend ledger (audit §5.6): the multi-machine floors are
            // deliberately generous, so this holds every metric to 0.70x of THIS
            // box's recent best. Green runs append to the committed ledger.
            PerfLane::Trend => crate::perf::gate_trend(trend, lanes_ok),
        }
    }
}

fn gate_perf() -> bool {
    gate_perf_with(&mut LivePerfLanes)
}

/// The `gate perf` VERB: run every lane, AND every result, report.
///
/// Every lane runs even after one fails — a perf report that stops at the first
/// regression tells you less than one that measures all ten.
fn gate_perf_with(lanes: &mut dyn PerfLanes) -> bool {
    eprintln!("=== gate perf ===");
    let mut trend: Vec<crate::perf::TrendSample> = Vec::new();
    let mut ok = true;
    for lane in PERF_MEASURING_LANES {
        ok &= lanes.run(lane, &mut trend, true);
    }
    ok &= lanes.run(PerfLane::Trend, &mut trend, ok);
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
mod tippy_identity_retry_tests {
    use super::*;

    /// THE IDENTITY ABORT IS RETRIED; A REAL FINDING IS NOT.
    ///
    /// Both directions matter and only one of them is obvious. Retrying the
    /// abort is what stops a clean tree reading as red. NOT retrying anything
    /// else is what stops a genuine `-D warnings` failure being retried into a
    /// pass — the far worse error, and the reason the match is on the abort
    /// signature alone rather than on "did it fail".
    #[test]
    fn only_the_identity_abort_is_retried() {
        // Non-vacuity: the signatures the lane matches on must be the ones
        // branded Tippy actually prints, so a reworded upstream message shows
        // up here rather than as a silently un-retried abort.
        assert!(TIPPY_IDENTITY_ABORTS
            .iter()
            .any(|s| s.contains("driver identity changed")));
        assert!(TIPPY_IDENTITY_ABORTS
            .iter()
            .any(|s| s.contains("while Targo was running")));

        let abort = "error: branded Tippy driver identity changed: selected Trust \
                     toolchain directory ancestor /Users changed identity or contents";
        let finding = "error: unused variable: `now`\nerror: could not compile";
        let is_abort =
            |text: &str| TIPPY_IDENTITY_ABORTS.iter().any(|sig| text.contains(sig));

        assert!(is_abort(abort), "the guard trip must be recognised");
        assert!(
            !is_abort(finding),
            "a real -D warnings finding must NEVER be retried into a pass"
        );
        // The budget is finite: an abort that never clears has to fail, or a
        // permanently churning tree would spin here forever. Checked at COMPILE
        // time — it is a claim about a constant, and a runtime assert over
        // constants is exactly the vacuous shape this session spent four rounds
        // removing.
        const { assert!(TIPPY_IDENTITY_RETRIES >= 2 && TIPPY_IDENTITY_RETRIES <= 5) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // VERB-LEVEL red proofs for `gate lint` and `gate perf`.
    //
    // Both gates were registered non-vacuity gaps: `perf` had never been shown
    // to fail at all, and `lint`'s only red fixture drove `run_repo_guards`, a
    // component. A gate nobody has watched go red is a gate nobody has shown
    // is listening.
    // ------------------------------------------------------------------

    /// Fails exactly one lane; records the order lanes were asked for.
    struct StubLanes {
        fail: Option<PerfLane>,
        seen: Vec<(PerfLane, bool)>,
    }

    impl PerfLanes for StubLanes {
        fn run(
            &mut self,
            lane: PerfLane,
            _trend: &mut Vec<crate::perf::TrendSample>,
            lanes_ok: bool,
        ) -> bool {
            self.seen.push((lane, lanes_ok));
            Some(lane) != self.fail
        }
    }

    #[test]
    fn every_perf_lane_can_turn_the_verb_red() {
        // THE VACUITY THIS KILLS: a lane whose result is computed and then
        // dropped (`ok &= f()` slipping to `f();`) makes `gate perf` green by
        // not listening, and no component test can see it. So: fail each lane
        // in turn and require the VERB's verdict to follow, one lane at a time.
        let all: Vec<PerfLane> = PERF_MEASURING_LANES
            .iter()
            .copied()
            .chain(std::iter::once(PerfLane::Trend))
            .collect();
        for lane in all {
            let mut stub = StubLanes {
                fail: Some(lane),
                seen: Vec::new(),
            };
            assert!(
                !gate_perf_with(&mut stub),
                "gate perf stayed GREEN with lane {lane:?} failing — its result \
                 is not reaching the verdict"
            );
        }
    }

    #[test]
    fn a_clean_sweep_is_green_and_runs_every_lane_once() {
        // The other half: the verb is not stuck red either, and it really does
        // ask for all ten lanes, in order, exactly once.
        let mut stub = StubLanes {
            fail: None,
            seen: Vec::new(),
        };
        assert!(gate_perf_with(&mut stub));
        let order: Vec<PerfLane> = stub.seen.iter().map(|(l, _)| *l).collect();
        let expected: Vec<PerfLane> = PERF_MEASURING_LANES
            .iter()
            .copied()
            .chain(std::iter::once(PerfLane::Trend))
            .collect();
        assert_eq!(order, expected, "every lane must run, once, in order");
    }

    #[test]
    fn the_trend_lane_is_told_whether_the_measuring_lanes_held() {
        // `gate_trend`'s contract takes `lanes_ok` because a trend reading over
        // a run whose floors already failed means something different. Pass it
        // a constant and the ledger silently records the wrong thing.
        let mut clean = StubLanes {
            fail: None,
            seen: Vec::new(),
        };
        let _ = gate_perf_with(&mut clean);
        assert_eq!(clean.seen.last().map(|(_, ok)| *ok), Some(true));

        let mut broken = StubLanes {
            fail: Some(PerfLane::Search),
            seen: Vec::new(),
        };
        let _ = gate_perf_with(&mut broken);
        assert_eq!(
            broken.seen.last().map(|(_, ok)| *ok),
            Some(false),
            "a failed measuring lane must reach the trend lane as lanes_ok=false"
        );
    }

    /// Fails exactly one lint lane; records which lanes were asked for.
    struct StubLintLanes {
        fail: Option<LintLane>,
        seen: Vec<LintLane>,
    }

    impl LintLanes for StubLintLanes {
        fn run(&mut self, lane: LintLane) -> bool {
            self.seen.push(lane);
            Some(lane) != self.fail
        }
    }

    #[test]
    fn every_lint_lane_can_turn_the_verb_red() {
        // ONE LANE AT A TIME. Failing all three together only proves their
        // conjunction — it leaves each arm free to stop failing closed with
        // nothing noticing, which is exactly what the old fixture allowed.
        for lane in LINT_LANES {
            let mut stub = StubLintLanes {
                fail: Some(lane),
                seen: Vec::new(),
            };
            assert!(
                !gate_lint_with(&mut stub),
                "gate lint stayed GREEN with lane {lane:?} failing — its result \
                 is not reaching the verdict"
            );
        }
    }

    #[test]
    fn a_clean_lint_is_green_and_runs_every_lane_once() {
        let mut stub = StubLintLanes {
            fail: None,
            seen: Vec::new(),
        };
        assert!(gate_lint_with(&mut stub));
        assert_eq!(stub.seen, LINT_LANES, "every lane must run, once, in order");
    }

    #[test]
    fn an_absent_toolchain_fails_each_lint_lane_closed_on_its_own() {
        // The REAL lanes, each isolated: with a stage2 dir holding neither
        // targo-tippy nor cargo, nothing was linted and nothing was
        // format-checked. "Nothing ran" must never read as "clean".
        let tmp = std::env::temp_dir().join(format!("aterm-gate-lint-red-{}", std::process::id()));
        let root = tmp.join("root");
        let tools = tmp.join("empty-stage2");
        let _ = std::fs::create_dir_all(&root);
        let _ = std::fs::create_dir_all(&tools);
        let mut live = LiveLintLanes {
            root: &root,
            tools: &tools,
        };
        let tippy = live.run(LintLane::Tippy);
        let fmt = live.run(LintLane::Trustfmt);
        let guards = live.run(LintLane::Guards);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(!tippy, "a missing linter must not report a clean lint");
        assert!(!fmt, "a missing formatter must not report clean formatting");
        assert!(
            !guards,
            "missing guard scripts must not report clean guards"
        );
        assert_eq!(resolve_tippy(&tools), None);
    }

    // The census walker's unit tests (parse_fn_def / guard_vars / term_hop_calls
    // / synthetic RED+GREEN trees) moved WITH the implementation to
    // `crates/aterm-census` — run `cargo test -p aterm-census`.
    use super::{
        ALL_ROSTER, DORMANCY_REGISTRY, DormantWatch, NON_VACUITY_REGISTRY, RedFixture, RedProof,
        WITNESS_REGISTRY, certified_driver, counts_report, dormant_report, drift_report,
        extract_call_string_args, fault_report, flag_was_rejected, impl_source_files,
        is_ordinary_kani_proof_attr, judge_kernel_certification, needle_present,
        non_vacuity_violations, proof_inventory_is_valid, readme_asserts_proof_inventory,
        roster_names, run_repo_guards, test_fn_body,
    };
    use crate::workspace_root;
    use std::path::{Path, PathBuf};

    // -----------------------------------------------------------------------
    // Fixture-tree helpers (the same discipline crates/aterm-census uses: work
    // on REAL text where possible, and assert every mutation actually applied
    // — a stale `from` would make the whole demonstration vacuous).
    // -----------------------------------------------------------------------

    fn fixture_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("aterm-gate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create fixture root");
        root
    }

    fn write_file(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write fixture file");
    }

    /// Copy one repo-relative file out of the live checkout into `root`.
    fn copy_real_file(root: &Path, rel: &str) {
        let from = workspace_root().join(rel);
        let text = std::fs::read_to_string(&from)
            .unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
        write_file(root, rel, &text);
    }

    /// Delete every line of a fixture file containing `symbol`, asserting the
    /// deletion applied (else the RED below would prove nothing).
    fn delete_lines_containing(root: &Path, rel: &str, symbol: &str) {
        let path = root.join(rel);
        let before = std::fs::read_to_string(&path).expect("read for mutation");
        let after: String = before
            .lines()
            .filter(|l| !l.contains(symbol))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_ne!(
            before.lines().count(),
            after.lines().count(),
            "no line of {rel} mentions `{symbol}` — the mutation is stale, so this \
             demonstration proves nothing"
        );
        std::fs::write(&path, after).expect("write mutation");
    }

    fn mutate(root: &Path, rel: &str, from: &str, to: &str) {
        let path = root.join(rel);
        let before = std::fs::read_to_string(&path).expect("read for mutation");
        let after = before.replace(from, to);
        assert_ne!(
            before, after,
            "the mutation no longer applies to {rel} (looking for `{from}`) — the pinned \
             text is stale, so this demonstration proves nothing"
        );
        std::fs::write(&path, after).expect("write mutation");
    }

    /// A renamed `-Z` flag must not be reported as a corpus regression. The
    /// first string is trustc's verbatim output when `gate certified` was
    /// finally able to run (2026-07-31); the second is a genuine verdict.
    #[test]
    fn a_rejected_flag_is_told_apart_from_a_verification_verdict() {
        assert!(flag_was_rejected(
            "error: unknown unstable option: `trust-verify-full`\n"
        ));
        assert!(!flag_was_rejected(
            "note: unknown or timed-out obligations are unproved coverage gaps\n\
             error: aborting due to 2 previous errors\n"
        ));
    }

    /// A scanner that scans its own registry proves nothing. Before the
    /// `xtask/src/gate.rs` exclusion this assertion FAILED: gate.rs was in the
    /// walk, so every `Proof::Needle` string literal in `WITNESS_REGISTRY` was
    /// its own witness and `gate drift` could not go red.
    #[test]
    fn witness_scan_excludes_the_registry_that_declares_the_needles() {
        let scanned = impl_source_files(&workspace_root(), Some("terminal_core.rs"));
        assert!(
            !scanned
                .iter()
                .any(|p| p.to_string_lossy().ends_with("xtask/src/gate.rs")),
            "gate.rs is in its own witness scan; every Needle would witness itself"
        );
        // And the consequence, stated directly: the registry's own text is not
        // evidence. `Proof::Needle(` appears on ordinary lines of gate.rs and
        // (by construction — it is this gate's private vocabulary) nowhere else
        // in the tree's non-test source.
        assert!(
            !needle_present(&workspace_root(), "Proof::Needle("),
            "the witness registry is witnessing itself"
        );
    }

    /// The live loaded gun this gate exists to catch. `soft_fonts` is advertised
    /// FALSE today with the in-source note "Advertise false until a real DRCS
    /// implementation lands"; MEASURED 2026-07-31, `grep -rn handle_decdld
    /// crates apps` hits only the registry entry below — there is no DRCS code.
    /// Flipping the flag to `true` used to print "gate drift: GREEN — 16
    /// advertised capabilities all have implementation witnesses" (verified by
    /// hand before the fix). If DRCS ever lands, THIS assertion flipping is the
    /// correct signal to retarget the fixture at another unimplemented needle —
    /// not a spurious failure.
    #[test]
    fn a_needle_with_no_implementation_is_not_witnessed() {
        assert!(
            !needle_present(&workspace_root(), "fn handle_decdld"),
            "soft_fonts' witness is satisfied with no DRCS implementation in the tree"
        );
        // Guard the fixture itself: it is only meaningful while that IS the
        // registered proof for soft_fonts.
        let w = WITNESS_REGISTRY
            .iter()
            .find(|w| w.cap == "soft_fonts")
            .expect("soft_fonts must stay registered");
        assert!(
            matches!(&w.proof, super::Proof::Needle(n) if *n == "fn handle_decdld"),
            "retarget this fixture: soft_fonts' registered proof changed"
        );
    }

    /// The stage2 tree is THE toolchain; rustup is at most a way of reaching it.
    /// Probing rustup first made `gate certified` SKIP-pass on the owner's box —
    /// the only machine that HAS a trustc (MEASURED: no `rustup` on PATH,
    /// working `$HOME/trust/build/host/stage2/bin/trustc`).
    #[test]
    fn certified_driver_prefers_stage2_trustc_and_never_probes_rustup_then() {
        let dir = std::env::temp_dir().join("aterm_gate_certified_driver_test");
        let _ = std::fs::create_dir_all(&dir);
        let trustc = dir.join("trustc");
        std::fs::write(&trustc, b"#!/bin/sh\n").expect("write fake trustc");
        let probed = std::cell::Cell::new(false);
        let got = certified_driver(&dir, || {
            probed.set(true);
            true
        });
        assert_eq!(got, Some((trustc, &[] as &'static [&'static str])));
        assert!(
            !probed.get(),
            "rustup was probed even though a stage2 trustc exists"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn certified_driver_falls_back_to_rustup_and_skips_only_when_neither_exists() {
        let empty = std::env::temp_dir().join("aterm_gate_certified_driver_empty");
        let _ = std::fs::create_dir_all(&empty);
        assert_eq!(
            certified_driver(&empty, || true),
            Some((PathBuf::from("rustup"), &["run", "trust", "trustc"][..]))
        );
        assert_eq!(certified_driver(&empty, || false), None);
        let _ = std::fs::remove_dir_all(&empty);
    }

    /// A missing guard script must FAIL the lint, matching verify.sh's
    /// `run_guard` ("$label missing or not executable"). The old `if
    /// script.exists()` had no `else`, so this returned true — `gate lint`
    /// could print GREEN with grep_guard and license_check never run.
    #[test]
    fn missing_guard_scripts_fail_the_lint_closed() {
        let dir = std::env::temp_dir().join("aterm_gate_guards_absent_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create empty root");
        assert!(
            !run_repo_guards(&dir),
            "a root with no tools/ passed the guard stage"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    // -----------------------------------------------------------------------
    // RED FIXTURES: each plants a violation and asserts the VERB reports
    // FAILURE. Registered in NON_VACUITY_REGISTRY; the meta-test below fails
    // the build if any roster gate loses its fixture.
    // -----------------------------------------------------------------------

    /// G-DRIFT, verb level. The REAL [`WITNESS_REGISTRY`] over a fixture
    /// advertise file: GREEN while the capability is advertised false, RED the
    /// moment it is advertised true with no implementation witness, GREEN again
    /// once the witness lands. The third leg matters — without it the RED could
    /// be structural (a fixture root where nothing can ever be witnessed).
    #[test]
    fn an_unwitnessed_capability_advertised_true_fails_the_drift_verb() {
        let root = fixture_root("drift-red");
        // `unicode`'s registered proof is Proof::Path("crates/aterm-grapheme"),
        // so the fixture provides it; `soft_fonts`' is Proof::Needle("fn
        // handle_decdld"), which nothing in the fixture implements yet.
        write_file(&root, "crates/aterm-grapheme/src/lib.rs", "// grapheme\n");
        write_file(
            &root,
            "crates/aterm-types/src/terminal_core.rs",
            "pub fn aterm_capabilities() -> TerminalCapabilities {\n\
             \x20   TerminalCapabilities {\n\
             \x20       unicode: true,\n\
             \x20       soft_fonts: false,\n\
             \x20   }\n\
             }\n",
        );

        let (ok, log) = drift_report(&root, WITNESS_REGISTRY);
        assert!(ok, "the honest fixture must be GREEN first:\n{log}");

        mutate(
            &root,
            "crates/aterm-types/src/terminal_core.rs",
            "soft_fonts: false",
            "soft_fonts: true",
        );
        let (ok, log) = drift_report(&root, WITNESS_REGISTRY);
        assert!(
            !ok,
            "advertising soft_fonts with no DRCS implementation MUST fail drift:\n{log}"
        );
        assert!(
            log.contains("'soft_fonts' advertised true but witness MISSING"),
            "the diagnostic must name the capability and its missing witness:\n{log}"
        );

        // And the mirror: land the witness, and the same tree goes GREEN — so
        // the RED above is the missing implementation, not the fixture shape.
        write_file(
            &root,
            "crates/aterm-core/src/terminal/handler_decdld.rs",
            "fn handle_decdld(&mut self) {}\n",
        );
        let (ok, log) = drift_report(&root, WITNESS_REGISTRY);
        assert!(ok, "a real witness must satisfy the gate:\n{log}");

        // Fail-closed on an UNKNOWN capability advertised true.
        mutate(
            &root,
            "crates/aterm-types/src/terminal_core.rs",
            "unicode: true,",
            "unicode: true,\n        teleportation: true,",
        );
        let (ok, log) = drift_report(&root, WITNESS_REGISTRY);
        assert!(
            !ok,
            "an unregistered advertised capability must fail:\n{log}"
        );
        assert!(
            log.contains("'teleportation' is advertised true but has NO witness registered"),
            "the fail-closed branch must name the unregistered capability:\n{log}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// G-DORMANT, verb level, driven by the REAL registry entry over a COPY of
    /// the real consumer file: `render_cells.rs` calls `apply_bidi_reorder`
    /// exactly once (line 886 on 2026-08-01, plus one comment mention the
    /// counter already ignores). Delete those lines — the WIRE-BIDI regression
    /// this watch exists for — and the gate must go red.
    #[test]
    fn deleting_the_only_consumer_fails_the_dormant_verb() {
        let watch = DORMANCY_REGISTRY
            .iter()
            .find(|w| w.producer == "apply_bidi_reorder")
            .expect("the bidi watch must stay registered (retarget this fixture if it moves)");
        assert!(
            watch.enforced,
            "this fixture only demonstrates the ENFORCED arm"
        );
        let root = fixture_root("dormant-red");
        copy_real_file(&root, watch.consumer_path);

        let (ok, log) = dormant_report(&root, std::slice::from_ref(watch));
        assert!(
            ok,
            "the unmutated real consumer file must be GREEN, or the RED below proves \
             nothing:\n{log}"
        );

        delete_lines_containing(&root, watch.consumer_path, watch.producer);
        let (ok, log) = dormant_report(&root, std::slice::from_ref(watch));
        assert!(
            !ok,
            "a producer with zero live consumers MUST fail the dormant gate:\n{log}"
        );
        assert!(
            log.contains("is DORMANT") && log.contains("apply_bidi_reorder"),
            "the diagnostic must name the dormant producer:\n{log}"
        );

        // The PENDING arm is reported, never failed — assert that distinction
        // directly, since it is the reason an entry can sit at zero consumers.
        let pending = [DormantWatch {
            feature: watch.feature,
            producer: watch.producer,
            consumer_path: watch.consumer_path,
            enforced: false,
        }];
        let (ok, log) = dormant_report(&root, &pending);
        assert!(ok, "a pending watch must not fail the gate:\n{log}");
        assert!(log.contains("pending:"), "log:\n{log}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// G-FAULT, verb level, BOTH directions: an injected fault point no test
    /// arms (the untested fail-closed path M7 exists to prevent), and its
    /// mirror, a test arming a name with no injection site.
    #[test]
    fn an_unarmed_injection_site_fails_the_fault_verb() {
        let root = fixture_root("fault-red");
        write_file(
            &root,
            "crates/demo/src/alloc.rs",
            "pub fn chunk() -> Option<u8> {\n\
             \x20   if crate::fault::triggered(\"demo.chunk_alloc\") {\n\
             \x20       return None;\n\
             \x20   }\n\
             \x20   Some(0)\n\
             }\n",
        );

        let (ok, log) = fault_report(&root);
        assert!(!ok, "an unarmed injection site MUST fail the gate:\n{log}");
        assert!(
            log.contains("'demo.chunk_alloc' injected at") && log.contains("NO test arms it"),
            "the diagnostic must name the site and the direction:\n{log}"
        );

        // Arm it from a test file: GREEN. (So the RED above is the missing
        // test, not the fixture tree.)
        write_file(
            &root,
            "crates/demo/tests/fault_demo.rs",
            "#[test]\nfn t() {\n    with_armed(\"demo.chunk_alloc\", || {});\n}\n",
        );
        let (ok, log) = fault_report(&root);
        assert!(ok, "an armed injection site must pass:\n{log}");

        // The mirror direction: a stale/typo'd arm with no injection site.
        write_file(
            &root,
            "crates/demo/tests/stale.rs",
            "#[test]\nfn t2() {\n    arm(\"demo.ghost\");\n}\n",
        );
        let (ok, log) = fault_report(&root);
        assert!(!ok, "an armed name with no site MUST fail the gate:\n{log}");
        assert!(
            log.contains("'demo.ghost'") && log.contains("NO injection site"),
            "the diagnostic must name the stale fault:\n{log}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// G-COUNTS, verb level, all three failure conditions: an empty inventory
    /// (the scan broke, or every harness was deleted), a README that reasserts
    /// a hand-maintained numeric total, and an unreadable README (fail-closed).
    #[test]
    fn an_empty_inventory_and_a_hand_maintained_total_fail_the_counts_verb() {
        let root = fixture_root("counts-red");
        write_file(
            &root,
            "crates/demo/src/lib.rs",
            "#[cfg(kani)]\nmod proofs {\n    #[kani::proof]\n    fn p() {}\n}\n",
        );
        write_file(
            &root,
            "README.md",
            "Run the computed proof-inventory gate for live totals.\n",
        );
        let (ok, log) = counts_report(&root);
        assert!(ok, "the honest fixture must be GREEN first:\n{log}");
        assert!(log.contains("1 ordinary `#[kani::proof]`"), "log:\n{log}");

        // (a) EMPTY INVENTORY.
        delete_lines_containing(&root, "crates/demo/src/lib.rs", "#[kani::proof]");
        let (ok, log) = counts_report(&root);
        assert!(!ok, "an empty proof inventory MUST fail the gate:\n{log}");
        assert!(
            log.contains("invalid/empty crate proof inventory"),
            "log:\n{log}"
        );

        // (b) HAND-MAINTAINED README TOTAL.
        write_file(
            &root,
            "crates/demo/src/lib.rs",
            "#[cfg(kani)]\nmod proofs {\n    #[kani::proof]\n    fn p() {}\n}\n",
        );
        write_file(
            &root,
            "README.md",
            "There are 9 `#[kani::proof]` harnesses in this snapshot.\n",
        );
        let (ok, log) = counts_report(&root);
        assert!(
            !ok,
            "a hand-maintained README total MUST fail the gate:\n{log}"
        );
        assert!(log.contains("hand-maintained numeric"), "log:\n{log}");

        // (c) UNREADABLE README — fail closed, never a silent pass.
        std::fs::remove_file(root.join("README.md")).expect("remove README");
        let (ok, log) = counts_report(&root);
        assert!(!ok, "a missing README MUST fail the gate closed:\n{log}");
        assert!(log.contains("could not read"), "log:\n{log}");
        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // THE NON-VACUITY OBLIGATION ITSELF — and, because a gate that cannot go
    // red is the very defect this exists to catch, four fixtures proving THIS
    // check goes red too.
    // -----------------------------------------------------------------------

    #[test]
    fn every_all_roster_gate_has_a_red_fixture_or_a_registered_known_gap() {
        let root = workspace_root();
        let violations = non_vacuity_violations(&roster_names(), NON_VACUITY_REGISTRY, &|rel| {
            std::fs::read_to_string(root.join(rel)).ok()
        });
        assert!(
            violations.is_empty(),
            "NON-VACUITY OBLIGATION VIOLATED — a `gate all` entry asserts more than anyone \
             has shown it verifies:\n{}\n  Fix: add a red-fixture test that plants a violation \
             and asserts the gate FAILS, and register it in NON_VACUITY_REGISTRY — or register \
             an explicit KnownGap with its reason.",
            violations.join("\n")
        );
        // Say the honest score out loud, so a reader of the test output learns
        // how much of the roster is actually proven rather than assuming all.
        let gaps: Vec<&str> = NON_VACUITY_REGISTRY
            .iter()
            .filter(|e| matches!(e.proof, RedProof::KnownGap { .. }))
            .map(|e| e.gate)
            .collect();
        eprintln!(
            "non-vacuity: {}/{} roster gates have a red fixture; KNOWN GAPS: {}",
            ALL_ROSTER.len() - gaps.len(),
            ALL_ROSTER.len(),
            if gaps.is_empty() {
                "none".to_string()
            } else {
                gaps.join(", ")
            }
        );
    }

    /// A registry file the fixtures can be tested against without touching the
    /// real tree.
    fn fake_read(text: &'static str) -> impl Fn(&str) -> Option<String> {
        move |_rel: &str| Some(text.to_string())
    }

    const GOOD_FIXTURE_FILE: &str = "mod tests {\n    #[test]\n    fn f() {\n        \
                                     assert!(!thing());\n    }\n}\n";

    #[test]
    fn the_obligation_goes_red_when_a_roster_gate_has_no_entry() {
        let registry = [RedFixture {
            gate: "drift",
            proof: RedProof::Fixture {
                test: "f",
                file: "x.rs",
                drives: "the verb",
                calls: "thing",
                verb_level: true,
            },
        }];
        let v = non_vacuity_violations(
            &["drift", "newgate"],
            &registry,
            &fake_read(GOOD_FIXTURE_FILE),
        );
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(
            v[0].contains("'newgate' is in the `all` roster with NO"),
            "{v:?}"
        );
        // …and the registered one is accepted, so the check is not simply
        // rejecting everything.
        assert!(
            non_vacuity_violations(&["drift"], &registry, &fake_read(GOOD_FIXTURE_FILE)).is_empty()
        );
    }

    #[test]
    fn the_obligation_goes_red_on_a_fixture_that_does_not_exist_or_is_not_a_test() {
        let registry = [RedFixture {
            gate: "drift",
            proof: RedProof::Fixture {
                test: "f",
                file: "x.rs",
                drives: "the verb",
                calls: "thing",
                verb_level: true,
            },
        }];
        // Missing file.
        let v = non_vacuity_violations(&["drift"], &registry, &|_| None);
        assert!(v.len() == 1 && v[0].contains("could not be read"), "{v:?}");
        // Renamed away.
        let v = non_vacuity_violations(
            &["drift"],
            &registry,
            &fake_read(
                "mod tests {\n    #[test]\n    fn g() {\n        assert!(!x());\n    }\n}\n",
            ),
        );
        assert!(v.len() == 1 && v[0].contains("renamed or deleted"), "{v:?}");
        // Present, but a plain helper — it can never fail the build.
        let v = non_vacuity_violations(
            &["drift"],
            &registry,
            &fake_read("mod tests {\n    fn f() {\n        assert!(!x());\n    }\n}\n"),
        );
        assert!(
            v.len() == 1 && v[0].contains("not annotated `#[test]`"),
            "{v:?}"
        );
    }

    /// THE HEART OF IT: a registered fixture that only ever asserts SUCCESS is
    /// exactly the vacuous gate this obligation exists to catch.
    #[test]
    fn the_obligation_goes_red_on_a_fixture_with_no_negative_assertion() {
        let registry = [RedFixture {
            gate: "drift",
            proof: RedProof::Fixture {
                test: "f",
                file: "x.rs",
                drives: "the verb",
                calls: "thing",
                verb_level: true,
            },
        }];
        let v = non_vacuity_violations(
            &["drift"],
            &registry,
            &fake_read(
                "mod tests {\n    #[test]\n    fn f() {\n        assert!(thing());\n    }\n}\n",
            ),
        );
        assert!(
            v.len() == 1 && v[0].contains("no NEGATIVE assertion"),
            "{v:?}"
        );
    }

    #[test]
    fn the_obligation_goes_red_on_a_hand_waved_gap_and_on_a_stale_entry() {
        let thin = [RedFixture {
            gate: "perf",
            proof: RedProof::KnownGap { reason: "hard" },
        }];
        let v = non_vacuity_violations(&["perf"], &thin, &fake_read(GOOD_FIXTURE_FILE));
        assert!(
            v.len() == 1 && v[0].contains("must carry a real reason"),
            "{v:?}"
        );

        // A stale entry for a gate that left the roster.
        let stale = [RedFixture {
            gate: "removed",
            proof: RedProof::KnownGap { reason: "x" },
        }];
        let v = non_vacuity_violations(&[], &stale, &fake_read(GOOD_FIXTURE_FILE));
        assert!(
            v.len() == 1 && v[0].contains("NOT in the `all` roster"),
            "{v:?}"
        );
    }

    /// The roster is the single source of truth for BOTH readers: if the `all`
    /// verb ever stops running a roster entry (or grows one the registry never
    /// sees), the obligation above is measuring the wrong set.
    #[test]
    fn the_roster_is_the_only_definition_of_what_gate_all_runs() {
        let names = roster_names();
        assert_eq!(names.len(), ALL_ROSTER.len());
        assert!(
            names.contains(&"perf") && names.contains(&"lint") && names.contains(&"drift"),
            "{names:?}"
        );
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "duplicate roster entry: {names:?}"
        );
    }

    #[test]
    fn test_fn_body_is_bounded_by_the_closing_brace_at_fn_indent() {
        let src = "mod tests {\n    #[test]\n    fn a() {\n        assert!(!x);\n    }\n\
                   \n    #[test]\n    fn b() {\n        assert!(y);\n    }\n}\n";
        let a = test_fn_body(src, "a").expect("a");
        assert!(
            a.contains("assert!(!x)") && !a.contains("assert!(y)"),
            "{a}"
        );
        let b = test_fn_body(src, "b").expect("b");
        assert!(
            b.contains("assert!(y)") && !b.contains("assert!(!x)"),
            "{b}"
        );
        // A mention inside a string literal is not a definition.
        let s = "mod tests {\n    const N: &str = \"fn a(\";\n    #[test]\n    \
                 fn a() {\n        assert!(!x);\n    }\n}\n";
        assert!(test_fn_body(s, "a").expect("a").contains("assert!(!x)"));
    }

    // -----------------------------------------------------------------------
    // G-CERTIFIED: the kernel-certification parse contract
    // -----------------------------------------------------------------------

    /// trustc 0.1.0's output, MEASURED 2026-08-01 compiling
    /// `crates/xtask/certified-corpus/guarded_cursor_advance.rs` with
    /// `-Ztrust-policy=certify` (two functions, one obligation each). The note
    /// lines are verbatim; the source-snippet lines trustc interleaves are
    /// elided, and one `-->` line per block is kept so the parser is exercised
    /// against interleaved non-note text rather than a clean pair.
    const MEASURED_CERTIFY_STDERR: &str = "\
note: Trust verification: 1 proved, 0 failed, 0 unknown, 0 timed out, 0 runtime-checked out of 1 obligation(s)
  --> crates/xtask/certified-corpus/guarded_cursor_advance.rs:13:1
   = note: of which 1 kernel-certified by the clean CIC kernel (zero-trust re-check; runtime-check elision requires exact MIR Assert identity)

note: Trust verification: 1 proved, 0 failed, 0 unknown, 0 timed out, 0 runtime-checked out of 1 obligation(s)
  --> crates/xtask/certified-corpus/guarded_cursor_advance.rs:18:1
   = note: of which 1 kernel-certified by the clean CIC kernel (zero-trust re-check; runtime-check elision requires exact MIR Assert identity)
";

    #[test]
    fn kernel_certification_is_asserted_not_merely_surfaced() {
        assert_eq!(judge_kernel_certification(MEASURED_CERTIFY_STDERR), Ok(2));

        // THE REGRESSION THE EXIT CODE CANNOT SEE: still fully discharged
        // (certify passes, exit 0) but the kernel no longer re-checks it.
        let solver_trusted =
            MEASURED_CERTIFY_STDERR.replace("of which 1 kernel", "of which 0 kernel");
        let err = judge_kernel_certification(&solver_trusted).expect_err("must be RED");
        assert!(err.contains("solver-trusted"), "{err}");

        // The parse contract fails CLOSED, never open.
        let no_note = MEASURED_CERTIFY_STDERR
            .lines()
            .filter(|l| !l.contains("kernel-certified"))
            .collect::<Vec<_>>()
            .join("\n");
        let err = judge_kernel_certification(&no_note).expect_err("must be RED");
        assert!(err.contains("PARSE CONTRACT BROKEN"), "{err}");
        let err = judge_kernel_certification("").expect_err("must be RED");
        assert!(err.contains("PARSE CONTRACT BROKEN"), "{err}");

        // An unproved / runtime-checked obligation is not a certification.
        let unknown = "note: Trust verification: 0 proved, 0 failed, 1 unknown, 0 timed out, \
                       0 runtime-checked out of 1 obligation(s)\n   = note: of which 0 \
                       kernel-certified by the clean CIC kernel\n";
        assert!(
            judge_kernel_certification(unknown)
                .expect_err("must be RED")
                .contains("not fully discharged")
        );
        let runtime = "note: Trust verification: 1 proved, 0 failed, 0 unknown, 0 timed out, \
                       1 runtime-checked out of 2 obligation(s)\n   = note: of which 1 \
                       kernel-certified by the clean CIC kernel\n";
        assert!(
            judge_kernel_certification(runtime)
                .expect_err("must be RED")
                .contains("runtime-checked")
        );
    }
}
