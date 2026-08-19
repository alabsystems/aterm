// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The AI-facing toolchain manual — the content behind `aterm help [topic]`.
//!
//! It answers two questions an AI has when it lands in this environment: *what is
//! this?* and *how do I use it?* — for aterm's own introspection AND for the whole
//! verification toolchain (trust, clean, ty, ay, ny, my, nn) that ships alongside it.
//!
//! ## One command, two faces (context-aware)
//!
//! * **Outside an aterm session** (another system, CI, a foreign shell) — `aterm help`
//!   prints the ecosystem OVERVIEW + the command map; `aterm help <topic>` prints a
//!   per-tool deep dive. This is reference documentation.
//! * **Inside an aterm session** (`$ATERM_PARENT_SESSION_ID` is set — a live
//!   introspectable session with a control socket) — `aterm help` (no topic) prints FULL
//!   AGENT INSTRUCTIONS: the operating brief for an AI agent driving this environment,
//!   with the session's own sid wired in. `aterm help <topic>` still prints the deep dive.
//!
//! ## Single source of truth
//!
//! [`TOPICS`] is the one table of deep dives; the front-page command map and the
//! `every_topic_renders_and_is_listed_on_the_front_page` binding test are both derived
//! from it, so a topic can never ship listed-but-empty or unlisted. The `introspection`
//! topic is special: its verb list is GENERATED from
//! [`aterm_types::control_verbs::catalog_lines`], the same table the control server
//! answers `help` from — so it never drifts from the real protocol.

use std::fmt::Write as _;

/// The environment blurb printed at the top of the front page (and the agent
/// brief). What this toolchain IS, in three sentences.
const OVERVIEW: &str = "\
aterm is the front door to a self-owned, AI-native verification toolchain: a stack
where the compiler PROVES your Rust, the terminal is a programmable surface an AI
can read and drive, and the entire chain is installed and cryptographically attested
by one package manager. Every tool is Rust, offline-capable, and built to be driven
by an agent — not just a human at a keyboard.";

/// A deep-dive manual entry for one tool/topic. `name` is what you type after
/// `help`; `tagline` is the one-liner on the command map; `body` is the page.
struct Topic {
    /// The `help <name>` key and command-map label.
    name: &'static str,
    /// One-line identity for the command map.
    tagline: &'static str,
    /// The full page (plain text, printed as-is). `None` for `introspection`,
    /// whose body is generated at render time from the live verb catalog.
    body: Option<&'static str>,
}

/// THE table of manual topics — the command map and the completeness gate both
/// derive from this. Order is the display order on the front page.
const TOPICS: &[Topic] = &[
    Topic {
        name: "aterm",
        tagline: "transparent introspecting terminal + toolchain launcher",
        body: Some(
            r#"aterm — a transparent, introspecting terminal, and the launcher for this toolchain.

WHAT IT IS
  `aterm` spawns your $SHELL in a PTY and passes I/O through UNCHANGED — it looks and
  behaves exactly like your shell — while feeding every byte into the aterm VT engine
  (an in-process model of the screen). The shell runs through a protected spawn seam
  (capability-gated, setrlimit-bounded, fail-closed, OS-sandbox-wrapped on demand), not
  raw forkpty/execvp. This passthrough CLI serves NO control socket of its own; the live,
  introspectable surface an AI reads and drives (via `aterm ctl`) is exposed by the
  WINDOW mode of the same binary — `aterm --window`, or `aterm --headless`
  (ATERM_HEADLESS=1). See `aterm help introspection`.

KEY USAGE
  aterm                      start an interactive $SHELL (the default; no args)
  aterm <tool> [args]        run a pinned, store-resolved toolchain tool, e.g.
                             `aterm ay`, `aterm ty` (never $PATH — the managed build)
  aterm pkg <args>           the toolchain package manager (see `aterm help atpkg`)
  aterm doctor               pre-flight health check; exit 0 = ready
  aterm show-config | validate-config | explain-config | list-fonts | list-themes
                             read-only diagnostics; print and exit, no shell spawned
  aterm --sandbox            run the shell under the macOS sandbox (deny net + secrets)

WHEN TO REACH FOR IT
  Use `aterm` for a daily-driver shell in the current terminal, or as the single
  launcher for the chain — `aterm <tool>` gives the pinned, attested build. Use
  `aterm --window` for a real window (tabs, splits, HiDPI). Use `aterm ctl` to introspect
  or drive a RUNNING instance from the outside.

GOTCHAS
  * `aterm <tool>` / `aterm pkg` need a co-located `atpkg` (resolved next to the aterm
    binary, never $PATH). Missing ⇒ `aterm pkg` exits 127; an unknown tool is a usage error.
  * Containment precedence: explicit flag > $ATERM_CONTAINMENT_MODE > default `user`;
    a malformed mode fails CLOSED to `containment`. The OS sandbox is actuated on macOS
    only; elsewhere it is rlimits + capability gate, and aterm says so on stderr.
  * `-h`/`--help` prints the terse CLI usage; `aterm help` (this manual) is the full guide."#,
        ),
    },
    Topic {
        name: "introspection",
        tagline: "read & drive any terminal via the control protocol (aterm ctl)",
        body: None, // generated — see `introspection_page()`
    },
    Topic {
        name: "agents",
        tagline: "make coding agents aterm-aware — the 3-line primer installer",
        body: Some(
            r#"agents — make coding agents aterm-aware (`aterm agents`, the primer installer).

WHAT IT IS
  A coding agent (Claude Code, Codex CLI, Gemini CLI, OpenCode, ...) never reads the
  terminal's scrollback — the only channel that reliably reaches its context in EVERY
  project is its global context file (~/.claude/CLAUDE.md, ~/.codex/AGENTS.md,
  ~/.gemini/GEMINI.md, ~/.config/opencode/AGENTS.md). `aterm agents` manages a short,
  marked, SELF-GATING primer block in those files: how to DETECT aterm
  ($TERM_PROGRAM=aterm / $ATERM_CHILD=1), that `aterm help` prints the agent operating
  brief, and why the agent's CLAUDE*/CODEX_*/... env vars were stripped. Outside aterm
  the block tells the agent to ignore itself, so installing it is harmless everywhere.

  It ALSO installs the bundled SKILLS: whole files aterm ships and owns, written into
  the agent's own skills directory (today `~/.claude/skills/drive-aterm/SKILL.md` for
  Claude Code — how to drive ANOTHER aterm session over the control socket). The skill
  content is compiled into the binary, so it updates with aterm and there is no second
  copy to drift.

KEY USAGE
  aterm agents               status: each agent, its context file + skills,
                             installed/stale/absent/foreign
  aterm agents install       install/update the block AND the bundled skills for every
                             DETECTED agent (its config dir exists); others are skipped
  aterm agents install codex force one agent by name (creates the file if needed)
  aterm agents remove        remove exactly the managed block, and aterm-owned skills
  aterm agents primer        print the block — paste into any project AGENTS.md/CLAUDE.md

WHEN TO REACH FOR IT
  Once per machine, and again after installing a new coding agent: run
  `aterm agents install` so any agent launched inside aterm knows what aterm is and how
  to go deeper. A screen banner cannot do this job — an agent's context never sees the
  terminal's output, which is exactly why the primer rides in the agent's own files.

GOTCHAS
  * IDEMPOTENT and surgical: the block lives between `<!-- aterm primer ... -->` markers;
    re-running install updates it in place, `remove` deletes exactly the block, and user
    content outside the markers is never touched (an unterminated marker fails closed).
  * A bare `install` skips undetected agents (no config dir = not in use) — name an
    agent explicitly to force it.
  * The primer is intentionally 3 lines: detection, `aterm help`, env hygiene. Depth
    lives HERE, behind `aterm help`, not in the agent's context file.
  * SKILLS are whole managed FILES, not blocks, so they carry an `<!-- aterm skill ... -->`
    marker instead. A file at that path WITHOUT the marker is yours: aterm reports it
    `foreign` and never writes or deletes it. Deleting the marker line is therefore the
    supported way to fork a shipped skill and keep your version.
  * Only Claude Code defines a skills convention today, so only `claude` gets one; the
    other agents receive the primer alone (aterm never fabricates a skills dir)."#,
        ),
    },
    Topic {
        name: "atpkg",
        tagline: "toolchain package manager — install / update / verify",
        body: Some(
            r#"atpkg — the toolchain package manager (also reachable as `aterm pkg`).

WHAT IT IS
  atpkg installs and silently self-updates the toolchain programs published by one
  configurable account (trust, clean, ay, ny, ...), gated by a compile-time-pinned
  Ed25519 offline root key over a signed, freshness-stamped index. Think "rustup
  married to a silent updater". Verification happens BEFORE any parse, enforced by
  construction: the only way to get the bytes the parser consumes is to pass a verify
  function — handing it unverified bytes does not type-check. `atpkg run` is the engine
  behind the `aterm <tool>` launcher.

KEY USAGE
  atpkg list                 installed (program, build) pairs — local, no network
  atpkg install <program>    verify the signed index, then install/upgrade the pinned build
  atpkg seed                 first-run batteries bootstrap: fill an EMPTY store from the
                             signed registry sealed inside the app bundle. Zero network by
                             construction (a local DirFetcher, never the network chain), so
                             it installs only what the seal carries; anything newer is the
                             consented `update` pass's job. The GUI runs it once per launch;
                             [packages].seed_install (default true) turns it from install
                             into announce-only when false
  atpkg update [program]     upgrade all (or one) to the channel pin;
                             coherence groups apply all-or-nothing (the rustc-locked
                             tuple moves together)
  atpkg uninstall <program> | --all
                             remove one program, or the WHOLE managed toolset and its
                             disk (~3.2 GB) in one step — the way out is as single-step
                             as the way in. Either form stops atpkg auto-completing the
                             set; [packages].exclude drops one program while keeping it
  atpkg which <tool>         print the store path a tool resolves to (never $PATH)
  atpkg run <tool> [-- args] exec the store binary — what `aterm <tool>` dispatches to
  atpkg doctor | verify      health surface / re-attest the store against the signed root

WHEN TO REACH FOR IT
  To manage the published CLI toolchain — install / update / pin / verify — or to see
  what's installed. The managed tools do their own work; atpkg is what lays them down.
  Distinct from `cargo ship`/aterm-release (which CUTS aterm.app itself).

GOTCHAS
  * The root anchor is COMPILED IN — a committed constant (aterm-update-core::pins), not a
    build env var — so a plain `cargo build` is fully armed. The kill switch is
    ATPKG_DISABLE: set it and the network verbs (install/update/rollback) refuse with
    exit 1; local read/maintenance verbs (list/which/run/doctor/verify/...) still work.
  * AUTOMATIC updates ride the windowed app: `aterm --window` runs the update pass on a
    6h loop (ATPKG_UPDATE_INTERVAL_SECS), and the app's own self-update check is likewise
    window-only. Headless or CLI-only usage updates NOTHING automatically — run
    `aterm pkg update` explicitly, or drive it from a scheduler (launchd/cron).
  * Channel is hard-wired to "stable" today; the real verbs are exactly atpkg's match
    arms (doctor/which/list/run/uninstall/install/update/rollback/pin/gc/verify/...)."#,
        ),
    },
    Topic {
        name: "trust",
        tagline: "self-proving Rust compiler (trustc / targo) — verification by default",
        body: Some(
            r#"trust — a self-proving Rust compiler: a fork of rust-lang/rust with a verification
pass built INTO the compiler, not bolted on beside it.

WHAT IT IS
  Building Trust builds the whole Rust compiler and emits `trustc` (compiles all valid
  Rust identically to rustc, and PROVES it) and `targo` (the drop-in cargo). A pass runs
  after MIR optimization, extracts proof obligations (overflow, bounds, div-by-zero,
  casts, panic-safety, ownership, contracts) and dispatches them to sibling backends via
  a router: `ay` (SMT), trust-mc (BMC), trust-vc (ownership), trust-wp (deductive), `ty`
  (temporal/TLA+), `clean` (higher-order). It is the umbrella compiler that ORCHESTRATES
  the leaf provers — you rarely call those directly.

KEY USAGE
  targo trust check              verify the current crate; human-readable report
                                 (`--format json` for one row per function/obligation)
  targo trust report-query --report r.json --require proved
                                 gate: exit 0 only if the selector matches >=1 obligation
                                 and ALL selected are proved (an empty report is NOT a proof)
  targo trust doctor | solvers   backend health; expect `ready: true`, `available: 6/6`
  targo / trustc                 ordinary cargo / rustc — drop-in, do NOT verify by default
  aterm pkg install trust        install/upgrade the prebuilt toolchain (signed seed/registry)

WHEN TO REACH FOR IT
  Use `targo trust check` whenever the goal is to compile AND prove real Rust — it is the
  only tool that reaches MIR-level invariants. Use plain `targo`/`trustc` for ordinary
  builds. Reach for a leaf prover (ay/ty/clean/...) directly only to debug that backend.

GOTCHAS
  * INSTALL: a prebuilt, self-contained sysroot SHIPS — `aterm pkg install trust`, or the
    whole toolset with `aterm pkg install --default-set` (the same act as Settings ▸
    Packages ▸ Install ALab toolset). trustc/targo then resolve via `aterm trustc` /
    `aterm targo` (store-pinned, never $PATH) and land on PATH inside aterm-integrated
    shells. Building from source is NOT a supported install path: trust is
    coherence-grouped, so atpkg permanently refuses to source-build it (prebuilt-only).
  * An empty/zero-obligation report is not a proof — always gate with `--require proved`.
  * Toolchain is Trust-branded only (trustc/targo/targo-trust/trustfmt); a genesis
    stage0 is dev-only and is rejected as proof evidence."#,
        ),
    },
    Topic {
        name: "clean",
        tagline: "Lean-shaped theorem prover — trusted kernel + tactics + .olean",
        body: Some(
            r#"clean — a from-scratch, pure-Rust implementation of Lean 4-shaped theorem-proving
infrastructure, built for AI agents to call directly (no Lean REPL/subprocess).

WHAT IT IS
  A workspace whose trusted core is `clean-kernel`, a `#![forbid(unsafe_code)]`
  Lean-compatible type checker. Around it: a parser, elaborator, tactic engine, .olean
  import, a Mathverse math library, C/Rust verification surfaces, and a JSON-RPC server
  so non-Rust clients get the same API. It is the theorem-proving / kernel-checking /
  proof-certificate layer of the toolchain. It deliberately does NOT do its siblings'
  jobs: SMT -> `ay`, bounded model checking -> trust-mc, NN-verification runtime -> ny
  (clean only hosts proofs ABOUT those algorithms).

KEY USAGE  (not on PATH here — run `cargo run --locked -p clean --bin clean -- <SUB>`)
  clean features [--search X]    discover the real CLI (registered feature descriptors)
  clean check <file.lean> [--json]   parse -> elaborate -> trusted kernel; accept/reject
  clean export-cert / kernel cert verify   emit / re-check a .cleancert proof bundle
  clean audit soundness          the kernel soundness certificate (C1-C5) + TCB
  clean server --port 8080       JSON-RPC 2.0 server (check/getType/prove/proveTLA/...)
  clean mathverse find|search    query the cross-system math corpus

WHEN TO REACH FOR IT
  Lean-shaped work: kernel type-checking, elaboration, .olean import, proof certificates,
  C (ACSL) / Rust (VIR) verification surfaces, TLA+/TLAPS obligations, Mathverse. Not for
  raw SMT (ay), BMC (trust-mc), or NN runtime (ny).

GOTCHAS
  * Path-depends on ../ay — if that sibling checkout is missing, no cargo command runs.
  * Always pass `--locked`. NO CI/hooks — enforcement is local (`just ci`, `clean audit`).
  * HONESTY: only say "proved" when the theorem's axiom closure ⊆ the foundational
    axioms; a Theorem wrapping an Axiom is a restatement, not a proof."#,
        ),
    },
    Topic {
        name: "ty",
        tagline: "TLA+ toolchain — model checker (TLC replacement) + prover",
        body: Some(
            r#"ty — a ground-up Rust reimplementation of the TLA+ verification toolchain (a TLC
replacement and more), shipped as one CLI binary named `ty`.

WHAT IT IS
  The core is an explicit-state model checker built to match TLC's semantics (TLC is the
  behavioral oracle). Around it the one binary adds JSON output, TLA+ -> Rust codegen,
  deductive theorem proving, a Petri-net / Model Checking Contest frontend, and gated
  symbolic (BMC/IC3/PDR via `ay`) and hardware (AIGER/BTOR2) backends. Soundness-first:
  when uncertain it abstains rather than emit a wrong verdict.

KEY USAGE  (not on PATH — `cargo build --release -p tla-cli` -> ./target/release/ty)
  ty check Spec.tla --config Spec.cfg [--workers N] [--output json]
                                 explicit-state model checking (the TLC replacement)
  ty prove Spec.tla [--theorem NAME]   deductive proving (discharge THEOREM obligations)
  ty induct                      check an inductive invariant
  ty corpus fetch                download the sha256-verified benchmark corpus (not in repo)
  ty supremacy compare | diagnose   TLC-vs-ty parity evidence (needs a TLC jar + JDK)
  ty aiger circuit.aig           hardware model checking (only with `--features ay`)

WHEN TO REACH FOR IT
  `ty check` for finite/bounded TLA+ model checking; `ty prove`/`ty induct` for an
  unbounded/deductive result; `ty mcc`/`ty petri` for Petri nets; `ty aiger`/`ty btor2`
  for hardware. Drop to `ay` only for raw SAT/SMT/CHC that ty already wraps.

GOTCHAS
  * No published binaries — build from source. On macOS install GNU m4 first
    (`brew install m4`) — a transitive build dep needs it.
  * The symbolic/hardware surfaces exist only when compiled `--features ay`.
  * Do not assume ty is faster than TLC — use `ty supremacy compare` for real evidence."#,
        ),
    },
    Topic {
        name: "ay",
        tagline: "SAT / SMT / CHC solver — proof-carrying, a Z3 replacement",
        body: Some(
            r#"ay — a Rust SAT, SMT, and CHC solver: the proof-carrying decision-procedure engine at
the bottom of the toolchain (positioned as a Z3 replacement).

WHAT IT IS
  The solver the higher-level tools call. Working SAT, SMT, and CHC paths; incomplete
  paths return `unknown` rather than an unchecked verdict. Proof-carrying by default:
  every `unsat` is emitted as a machine-checkable certificate (Alethe for SMT, DRAT/LRAT
  for SAT, ay-chc-cert for CHC) so a false `unsat` cannot hide. The `trust` pipeline
  vendors ay and re-checks its Alethe in a kernel.

KEY USAGE  (needs the `cli` feature: `cargo run -p ay --features cli -- <file>`)
  ay FILE                      solve, auto-detecting format (.cnf DIMACS / HORN CHC / SMT-LIB2);
                               on unsat, writes a proof cert next to the input
  ay --z3-mode -in             read SMT-LIB2 from stdin as a Z3-style drop-in (incremental)
  ay solve --proof out.alethe FILE   explicit proof emission (fails loud if uncheckable)
  ay check drat FORMULA PROOF  re-check an emitted DRAT/LRAT proof
  ay z3-audit | verifier-audit honest readiness gates (Z3 / Creusot-Why3-Verus backend)

WHEN TO REACH FOR IT
  When you need to DECIDE a formula (SAT of SMT-LIB2 / DIMACS / CHC-Horn), get a model,
  or produce a checkable unsat certificate. Verification tools call ay as their backend,
  not the reverse. Use `ay check` when you only need to verify an existing proof.

GOTCHAS
  * The PATH `ay` is the trust monorepo's vendored copy, NOT ~/ay — build ~/ay explicitly
    to exercise its code. The binary needs `--features cli`.
  * Exit codes differ by input: SMT-LIB returns 0 regardless of sat/unsat (Z3 convention);
    DIMACS uses 10=SAT / 20=UNSAT. Don't treat nonzero as failure for SAT input.
  * It is a SCOPED Z3-compatible CLI, not a universal drop-in — unsupported options are
    rejected explicitly, never emulated."#,
        ),
    },
    Topic {
        name: "ny",
        tagline: "neural-network verifier — CROWN / β-CROWN, VNN-LIB, proof certs",
        body: Some(
            r#"ny — a Rust neural-network verifier: given a network and a property it returns a SOUND
verdict (verified / falsified-with-counterexample / unknown).

WHAT IT IS
  Loads ONNX (primary) plus SafeTensors/PyTorch/GGUF/NNet, and properties as VNN-LIB
  `.vnnlib` files or an L-inf epsilon ball. Methods: ibp, crown, alpha (alpha-CROWN),
  beta (beta-CROWN) and an SDP path; complete verification is beta-CROWN branch-and-bound
  with PGD falsification and a MIP fallback. Its soundness core is error-carrying CROWN
  (f64 matmul with a certified per-coefficient error folded outward with directed
  rounding) — what my/nn call "gamma-crown". On eligible nets it ships an exact-rational,
  machine-checkable `<model>.cert.json` proof sidecar.

KEY USAGE  (not on PATH — `cargo run --release -p ny-cli -- <SUB>`)
  ny verify model.onnx -p prop.vnnlib --method alpha [--require-sound] [--json]
                               fast sound over-approximation (may say unknown)
  ny beta-crown model.onnx -p prop.vnnlib [--timeout N]
                               complete branch-and-bound; writes a proof cert when eligible
  ny vnncomp v1 CATEGORY model.onnx prop.vnnlib RESULTS TIMEOUT
                               competition protocol (auto-selects preset/strategy)
  ny inspect model.onnx        structure + optional FLOP/memory cost
  ny lipschitz model.onnx      certified global Lipschitz upper bound (exact rational)

WHEN TO REACH FOR IT
  When you have an ONNX network + a VNN-LIB property (or an epsilon-robustness question)
  and want a trustworthy verdict. `ny verify` for a quick sound answer; `ny beta-crown`
  for a decided sat/unsat with a certificate; `ny vnncomp` for scored runs. ny is the
  dedicated verifier; my/nn are frameworks that consume it; ay is the solver it delegates to.

GOTCHAS
  * Default workspace check must exclude the Python crate: `cargo check --workspace
    --exclude ny-python`. MIP needs `--features mip`.
  * Soundness is opt-OUT: `--allow-unsound-gpu-crown` trades correctness for speed and can
    flip a violated instance to Verified — never use it when the verdict must be trusted.
    Use `ny verify --require-sound` to reject heuristic paths."#,
        ),
    },
    Topic {
        name: "my",
        tagline: "verified ML framework + compiler — exported PyTorch → Metal",
        body: Some(
            r#"my — "Verified ML Framework": a Rust-native ML stack whose `my` CLI compiles exported
PyTorch artifacts into optimized, Metal-ready inference. (The newer of the my/nn pair.)

WHAT IT IS
  A framework + compiler, not a standalone verifier. The CLI's working job is an
  exported-artifact bridge: take a `torch.export` graph.json + `safetensors` weights and
  lower them (trace -> fuse/peephole -> Metal MSL) into a runnable CompiledModel, emitting
  a machine-readable ConvertReport. Verification (Kani kernel proofs, partial gamma-crown
  bounds via ny, z4 SMT) is surfaced as optional report hooks. Metal (Apple GPU) is the
  only production backend.

KEY USAGE  (not on PATH — `cargo run -p my-cli -- <SUB>`; macOS + Metal required)
  my convert graph.json weights.safetensors [--optimize normal|aggressive] [--verify bounds|full]
                               compile exported artifacts -> Metal model + ConvertReport
  my convert model.pt --from-pytorch --model-spec module:Class --input-shape 1 3 224 224
                               shell out to my_export.py first, then convert (needs python+torch)
  my compile ... -o model.myc  compile to a serialized .myc plan (+ report)
  my run ... --input inputs.safetensors   execute on the Metal GPU
  my optimize plan.myc ...     search the peephole space to minimize dispatch count/cost

WHEN TO REACH FOR IT
  Framework/compiler work on Apple GPUs: exported PyTorch -> optimized Metal inference,
  run/optimize a compiled model, build nn models in Rust. For standalone formal
  VERIFICATION of a network reach for `ny`; for raw solving, `ay`. my and nn are sibling
  framework iterations (my is newer, v0.2.0-dev) — prefer my for framework work.

GOTCHAS
  * Metal-only: every subcommand initializes the Metal backend (macOS + Metal GPU).
  * Input is NOT raw PyTorch/ONNX — it expects pre-exported torch.export graph.json +
    safetensors; `--from-pytorch` is the only .pt on-ramp and subprocesses a Python script.
  * `--verify full` is a REPORT request, not inline Kani kernel-safety proofs."#,
        ),
    },
    Topic {
        name: "nn",
        tagline: "verified ML framework (earlier line) — torch.export → Metal",
        body: Some(
            r#"nn — "NN, Verified ML Framework" (v0.1.0): the earlier sibling of `my`. A Rust ML
framework whose `nn` CLI compiles exported PyTorch models into verifiable Metal inference.

WHAT IT IS
  Model code, GPU kernels, and proof tooling in one workspace (nn-core, nn-metal,
  nn-import, nn-verify, ...). Production story: Kani-backed GPU-kernel verification, Metal
  as the real backend, a torch.export + safetensors import bridge emitting a ConvertReport.
  It links `ny` for IBP/CROWN bound propagation and `ay` for SMT, layering formal checks
  (types -> ny bounds -> ay SMT) on top of compiled Metal inference. Same convert/compile/
  run/optimize CLI shape as `my`, one version behind.

KEY USAGE  (not on PATH — `cargo run -p nn-cli -- <SUB>`; macOS + Metal required)
  nn convert graph.json weights.safetensors [--optimize ...] [--verify bounds|full]
                               compile pre-exported artifacts -> Metal model + ConvertReport
  nn compile ... --output model.nnc    persist a .nnc plan (+ report)
  nn run ... --input inputs.safetensors   execute on the Metal GPU
  nn optimize model.nnc ...    time-budgeted peephole search vs the baseline plan

WHEN TO REACH FOR IT
  To compile/run/optimize an exported PyTorch model into a Metal pipeline on the nn tree.
  For framework work in general prefer `my` (newer); reach for `ny` to verify a network's
  bounds and `ay` for raw solving. nn and my are sibling framework iterations.

GOTCHAS
  * NEVER run a bare workspace `cargo test` here — it has kernel-panicked the machine (OOM);
    several test binaries are enormous. Use `cargo nextest run` (honors the single-threaded
    `heavy` group) or scripts/test-capped.sh; the biggest GPU tests are #[ignore]'d.
  * Same intake rule as `my`: pre-exported torch.export graph.json + safetensors, not raw
    ONNX/.pt. `--verify` is a report request, feature-gated."#,
        ),
    },
];

/// Every `help <topic>` key, in display order — used by the completeness gate to
/// prove each dispatchable topic is also listed on the front page.
#[cfg(test)]
fn topic_names() -> Vec<&'static str> {
    TOPICS.iter().map(|t| t.name).collect()
}

/// The front page for `help` outside a session: the environment blurb + the
/// command map + how to go deeper.
fn overview_page() -> String {
    let mut s = String::new();
    s.push_str("aterm — the toolchain manual\n\n");
    s.push_str(OVERVIEW);
    s.push_str("\n\nONE COMMAND, MANY VERBS — `aterm <verb>`\n");
    let verbs = [
        (
            "help [topic]",
            "this manual — start here (a deep dive on any verb or tool)",
        ),
        (
            "ctl <args>",
            "introspect & drive any terminal (read / keys / turn / subscribe / image)",
        ),
        ("pkg <args>", "install / update / verify the toolchain"),
        (
            "fleet <args>",
            "federate many sessions' events; dispatch commands back",
        ),
        (
            "drive <args>",
            "the agent drive CLI (await / send / turn helpers)",
        ),
        (
            "agents [<cmd>]",
            "make coding agents aterm-aware (install the 3-line primer; run once)",
        ),
    ];
    // Column width across the verb signatures and the topic names.
    let width = TOPICS
        .iter()
        .map(|t| t.name.len())
        .chain(verbs.iter().map(|(v, _)| v.len() + "aterm ".len()))
        .max()
        .unwrap_or(14);
    for (v, desc) in verbs {
        let _ = writeln!(s, "  {:<width$}  {desc}", format!("aterm {v}"));
    }
    s.push_str("\nAND EVERY TOOL — `aterm help <name>` for how to use each\n");
    for t in TOPICS {
        let _ = writeln!(s, "  {:<width$}  {}", t.name, t.tagline);
    }
    s.push_str(
        "\nInside an aterm session, `aterm help` prints the agent operating brief automatically.\n",
    );
    s
}

/// The `introspection` topic — GENERATED from the live control-verb catalog so it
/// never drifts from the real protocol, plus the how-to an AI needs to use it.
fn introspection_page() -> String {
    let mut s = String::new();
    s.push_str(
        "\
introspection — read and drive any terminal via the control protocol.

WHAT IT IS
  Every window-mode session — a window, or `aterm --headless` (ATERM_HEADLESS=1) for an
  engine + socket with no window — exposes a control socket speaking a small newline
  protocol. (The plain `aterm` passthrough CLI serves NONE: it is a transparent shell
  wrapper, not an introspection host.) The `aterm ctl` client talks that socket: read the
  terminal state (as text/styled cells) or application-rendered client pixels, send
  keystrokes, wait on events, and drive a whole fleet through the same application input
  path. OS compositor and display output are outside this interface. A session is addressed by its sid;
  `@<sid>` routes a verb to that session, relayed transparently even across instances and
  machines.

THE MOVES (an AI's loop is see -> decide -> drive -> observe)
  SEE     aterm ctl @sid text | screen | image f.png | cast frames count=8
  DRIVE   aterm ctl @sid turn 'message'   (verified type -> submit -> settle -> reply)
          aterm ctl @sid send '...' | key enter | paste | resize <r> <c>
  OBSERVE aterm ctl @sid await idle <ms> | await match <re> | ready | wait
  WATCH   aterm ctl subscribe @a,@b,@c events     (the whole fleet on ONE fd, low-rate)
  FLEET   aterm ctl ls        (every session of every instance: pid sid state)
          aterm fleet events | exec   (federate N sessions -> one NDJSON stream; dispatch back)
  RECALL  aterm ctl @sid history [<n>]      (per-turn record + deterministic screen hash)

HOW TO USE IT
  `turn` is the AI-to-anything verb: it types a message, submits it, waits for the target
  to settle, and returns the settled screen — closing the paste/Enter race so one CLI can
  drive another as if a human were at the keyboard. `subscribe ... events` is event-driven:
  you pull a full screen/image only when an event says something changed, so watching five
  or fifty sessions costs almost nothing until it matters. Headless works too (pass
  --headless, or the exactly equivalent ATERM_HEADLESS=1, for an engine + control socket
  with no window; either way the launch names the mode on stderr). Discoverability is
  OPT-IN: launch the window with ATERM_AI_HINT=1 to inject a single dim line above the
  first prompt announcing the terminal is AI-introspectable and drivable with aterm-ctl
  (off by default — a transparent terminal injects nothing into your screen).

THE FULL, ALWAYS-CURRENT VERB CATALOG (generated from the protocol table):",
    );
    s.push('\n');
    for line in aterm_types::control_verbs::catalog_lines() {
        s.push_str("  ");
        s.push_str(&line);
        s.push('\n');
    }
    s.push_str(
        "\nThis catalog is generated from the one typed verb table the control server answers\n\
         `help` from — so it is always exactly the protocol this build speaks. Run\n\
         `aterm ctl help` against a live session for the same list from the server itself.\n",
    );
    s
}

/// The full agent operating brief — what `aterm help` prints INSIDE an aterm session.
/// `sid` is the caller's own session id when known (wired in so the agent can drive
/// itself), else a generic placeholder for an explicit `help agent` outside a session.
fn agent_page(sid: Option<&str>) -> String {
    let mut s = String::new();
    let you = match sid {
        Some(id) => format!(
            "You are an AI agent operating inside aterm session {id}. That terminal is\n  \
             introspectable and driveable — by you, by peer agents, and by the human, all at\n  \
             once. You can watch and drive your OWN session and any peer with `aterm ctl @<sid>`.",
        ),
        None => {
            "You are an AI agent in this verification toolchain (no aterm session id in the\n  \
             environment, so run inside an aterm session to introspect a live terminal)."
                .to_string()
        }
    };
    s.push_str("aterm — agent operating brief\n\n");
    s.push_str(OVERVIEW);
    s.push_str("\n\nWHERE YOU ARE\n  ");
    s.push_str(&you);
    if let Some(id) = sid {
        let _ = write!(
            s,
            "\n\n  Your session: {id}\n  \
             See yourself:   aterm ctl @{id} text\n  \
             Drive yourself: aterm ctl @{id} turn 'message'   (rarely needed — you ARE the shell)\n  \
             Find peers:     aterm ctl ls        (then drive any peer with @<its-sid>)",
        );
    }
    s.push_str(
        "\n\nHOW TO SEE, DRIVE, AND COORDINATE (the introspection control protocol)\n  \
         The loop is see -> decide -> drive -> observe. Read a peer with `@sid text` or a real\n  \
         frame with `@sid image`; drive it with `@sid turn 'msg'` (verified submit + settle +\n  \
         reply); wait without polling via `@sid await idle <ms>` / `await match <re>`; watch a\n  \
         whole fleet on one descriptor with `subscribe @a,@b events`. Humans can interject at\n  \
         any time — the input path is the human's, and a per-session turn lease arbitrates so\n  \
         two drivers never clobber each other. Full detail: `aterm help introspection`.\n",
    );
    s.push_str(
        "\nENV HYGIENE (why your agent context vars may be missing)\n  \
         aterm STRIPS AI-agent context variables from the shell it spawns — every CLAUDE*,\n  \
         ANTHROPIC_*, COPILOT_*, CODEX_*, CURSOR_*, AI_*, and _DEVTOOL_* var is removed before\n  \
         exec, so they never leak into your session. If an inner tool reports its context\n  \
         vars went missing, aterm sanitized them here by design (aterm_types::env_sanitize) —\n  \
         re-export what it needs, or run it outside aterm to keep the originals.\n",
    );
    s.push_str(
        "\nTHE TOOLS AT HAND (run `aterm help <name>` for how to use each)\n\
         \x20 trust  compile AND prove Rust (targo trust check)      ay   decide a formula (SAT/SMT/CHC)\n\
         \x20 clean  Lean-shaped theorem proving                     ty   TLA+ model checking + proving\n\
         \x20 ny     verify a neural network (CROWN/beta-CROWN)       my   exported PyTorch -> Metal (framework)\n\
         \x20 nn     ML framework (earlier line)                     atpkg install/update/verify the chain\n",
    );
    s.push_str(
        "\nHOUSE RULES (this toolchain is honesty-first)\n  \
         * Never claim a prover/compiler ran or 'proved' something that didn't — an empty or\n    \
         zero-obligation report is not a proof. Say what actually executed.\n  \
         * No git hooks and no CI anywhere in this toolchain, by owner mandate — gating is\n    \
         inline/optional in the tools (e.g. `targo trust check`, `clean audit`, `ty ... gate`).\n  \
         * Each tool's own AGENTS.md/CLAUDE.md rules win in its repo (e.g. never a bare\n    \
         `cargo test` in nn; always `--locked` in clean).\n",
    );
    s.push_str(
        "\nGO DEEPER\n  aterm help                 the command map for the whole toolchain\n",
    );
    s.push_str("  aterm help <topic>         a deep dive on any tool\n");
    s.push_str(
        "  aterm ctl help             the live control-verb catalog from a running session\n",
    );
    s.push_str(
        "  aterm agents               keep the coding-agent primer installed — how an agent\n\
         \x20                            like you learns aterm exists (see `aterm help agents`)\n",
    );
    s
}

/// The caller's own aterm session id, if it is running inside one — the signal that
/// `aterm help` should print the agent brief rather than the reference front page.
/// `$ATERM_PARENT_SESSION_ID` is set by an aterm session that exposes a control
/// socket, so its presence means a live, introspectable session exists.
pub fn in_session() -> Option<String> {
    std::env::var("ATERM_PARENT_SESSION_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Render the manual. `topic` is the optional `help <topic>` argument; `session` is
/// the caller's own sid when it is inside an aterm session (from [`in_session`]).
///
/// * `None` topic, inside a session -> the agent brief (with the sid wired in).
/// * `None` topic, outside -> the reference front page (overview + command map).
/// * `Some("agent")` -> the agent brief explicitly (any context).
/// * `Some("introspection")` -> the generated protocol page.
/// * `Some(tool)` -> that tool's deep dive.
/// * an unknown topic -> a usage error listing the topics, exit code 2.
pub fn render(topic: Option<&str>, session: Option<&str>) -> (String, i32) {
    // A front-door VERB typed as a help topic resolves to the page that documents it, so
    // `aterm help ctl`/`pkg`/`fleet`/`drive` (advertised on the front page) never 404 — the
    // control protocol, package manager, and orchestration all live under existing topics.
    let topic = topic.map(|t| match t {
        "ctl" | "fleet" | "drive" => "introspection",
        "pkg" => "atpkg",
        other => other,
    });
    match topic {
        None => {
            if let Some(sid) = session {
                (agent_page(Some(sid)), 0)
            } else {
                (overview_page(), 0)
            }
        }
        Some("agent") | Some("instructions") => (agent_page(session), 0),
        Some("introspection") => (introspection_page(), 0),
        Some(name) => match TOPICS.iter().find(|t| t.name == name) {
            Some(t) => {
                // Only `introspection` has a generated body; it is handled above, so
                // every remaining TOPICS entry carries an authored body.
                let body = t
                    .body
                    .unwrap_or_else(|| unreachable!("only introspection is generated"));
                (format!("{body}\n"), 0)
            }
            None => {
                let mut msg = format!(
                    "aterm help: unknown topic '{name}'\n\n\
                     available topics (aterm help <topic>):\n"
                );
                for t in TOPICS {
                    let _ = writeln!(msg, "  {}", t.name);
                }
                msg.push_str("  agent\n");
                (msg, 2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_page_and_agent_brief_render_nonempty() {
        let (front, code) = render(None, None);
        assert_eq!(code, 0);
        assert!(
            front.contains("aterm <verb>")
                && front.contains("aterm ctl")
                && front.contains("trust")
        );
        let (agent, code) = render(None, Some("s-abc123"));
        assert_eq!(code, 0);
        assert!(agent.contains("session s-abc123") && agent.contains("aterm ctl @s-abc123"));
    }

    #[test]
    fn every_topic_renders_and_is_listed_on_the_front_page() {
        let front = overview_page();
        for name in topic_names() {
            let (page, code) = render(Some(name), None);
            assert_eq!(code, 0, "topic {name} should render 0");
            assert!(!page.trim().is_empty(), "topic {name} rendered empty");
            assert!(
                front.contains(name),
                "topic {name} is dispatchable but missing from the command map"
            );
        }
    }

    #[test]
    fn introspection_topic_is_generated_from_the_live_verb_catalog() {
        let (page, code) = render(Some("introspection"), None);
        assert_eq!(code, 0);
        // A representative verb from the real catalog must appear, proving the page is
        // wired to `catalog_lines()` and not a hand-copied list.
        let catalog: String = aterm_types::control_verbs::catalog_lines().collect();
        let sample = catalog.split_whitespace().next().unwrap_or("subscribe");
        assert!(page.contains(sample) || page.contains("subscribe"));
    }

    #[test]
    fn unknown_topic_is_a_usage_error_listing_topics() {
        let (msg, code) = render(Some("nonesuch"), None);
        assert_eq!(code, 2);
        assert!(msg.contains("unknown topic") && msg.contains("trust"));
    }

    #[test]
    fn agent_brief_is_reachable_by_name_in_any_context() {
        let (page, code) = render(Some("agent"), None);
        assert_eq!(code, 0);
        assert!(page.contains("agent operating brief") && page.contains("HOUSE RULES"));
    }

    #[test]
    fn introspection_page_scopes_the_socket_to_gui_or_headless() {
        // FINDING #5: the control socket is a GUI/headless affordance; the plain
        // passthrough CLI serves none. The page must say so, not overclaim that
        // "every aterm session" exposes a socket.
        let (page, code) = render(Some("introspection"), None);
        assert_eq!(code, 0);
        assert!(
            page.contains("window"),
            "must scope the socket to the window mode"
        );
        assert!(
            page.contains("--headless") || page.contains("ATERM_HEADLESS"),
            "must name the headless socket host"
        );
        assert!(
            page.contains("passthrough CLI serves NONE"),
            "must disclaim the plain CLI passthrough socket"
        );
        // The `aterm` topic body makes the same scoping honest.
        let (aterm_page, _) = render(Some("aterm"), None);
        assert!(
            aterm_page.contains("serves NO control socket"),
            "the aterm topic must not overclaim the passthrough is introspectable"
        );
    }

    #[test]
    fn agent_brief_documents_env_hygiene_and_ai_hint() {
        // FINDING #7: an inner agent that lost its context vars can learn why, and the
        // opt-in AI hint is discoverable from the manual (not only the README).
        let (agent, code) = render(None, Some("s-abc123"));
        assert_eq!(code, 0);
        assert!(
            agent.contains("ENV HYGIENE"),
            "agent brief must document env stripping"
        );
        for token in [
            "CLAUDE",
            "ANTHROPIC_",
            "COPILOT_",
            "CODEX_",
            "CURSOR_",
            "AI_",
        ] {
            assert!(
                agent.contains(token),
                "env-hygiene note must name the {token} deny prefix"
            );
        }
        let (page, _) = render(Some("introspection"), None);
        assert!(
            page.contains("ATERM_AI_HINT"),
            "the opt-in AI hint must be documented in the manual"
        );
    }

    #[test]
    fn trust_topic_steers_to_the_prebuilt_install_never_a_source_build() {
        // FINDING: a prebuilt self-contained sysroot SHIPS (seed + signed registry) and
        // source-building trust is permanently refused (coherence-grouped ⇒ prebuilt-only,
        // atpkg's sourcebuild choke point). The primary "how do I install trust" surface
        // must name the real path and never steer users into an x.py build.
        let (page, code) = render(Some("trust"), None);
        assert_eq!(code, 0);
        assert!(
            page.contains("aterm pkg install trust"),
            "must name the shipped install path"
        );
        assert!(
            !page.contains("x.py"),
            "must not steer users to the refused source-build path"
        );
        assert!(
            page.contains("prebuilt"),
            "must say the sysroot ships prebuilt"
        );
    }

    #[test]
    fn atpkg_topic_states_the_compiled_anchor_and_the_window_scoped_update_loop() {
        // FINDING (root anchor): the root key is a committed constant
        // (aterm-update-core::pins) — the stale "a plain cargo build bakes no root key"
        // claim must be gone, and the ATPKG_DISABLE kill switch documented.
        let (page, code) = render(Some("atpkg"), None);
        assert_eq!(code, 0);
        assert!(
            page.contains("COMPILED IN") && page.contains("ATPKG_DISABLE"),
            "must document the committed anchor and the kill switch"
        );
        assert!(
            !page.contains("bakes no root key"),
            "the inert-by-default claim is stale"
        );
        // FINDING (update scope): both update loops live in the windowed app only, so
        // the manual must tell headless/CLI users to run `aterm pkg update` themselves.
        assert!(
            page.contains("aterm pkg update") && page.contains("scheduler"),
            "must state the headless/CLI update obligation"
        );
    }

    #[test]
    fn front_door_verbs_resolve_as_help_topics() {
        // The front page advertises `aterm help ctl/pkg/fleet/drive`, so each MUST resolve to
        // a real page (never the exit-2 unknown-topic error), aliased to the topic that owns it.
        for (verb, marker) in [
            ("ctl", "control protocol"),
            ("fleet", "control protocol"),
            ("drive", "control protocol"),
            ("pkg", "package manager"),
        ] {
            let (page, code) = render(Some(verb), None);
            assert_eq!(code, 0, "`aterm help {verb}` must resolve, not 404");
            assert!(
                page.to_lowercase().contains(marker),
                "`aterm help {verb}` should route to the page covering it"
            );
        }
    }
}
