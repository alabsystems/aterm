// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The AI-facing toolchain manual — the content behind `aterm help [topic]`.
//!
//! It answers two questions an AI has when it lands in this environment: *what is
//! this?* and *how do I use it?* — for aterm's own introspection AND for the whole
//! verification toolchain (trust, clean, ty, ay, ny, nn) that ships alongside it.
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
//! [`aterm_types::control_verbs::catalog_lines_full`], the same table the control
//! server answers `help` from — so it never drifts from the real protocol.

use std::fmt::Write as _;

/// The environment blurb printed at the top of the front page (and the agent
/// brief). What this toolchain IS, in three sentences.
const OVERVIEW: &str = "\
aterm is the front door to a self-owned, AI-native verification toolchain: a stack
where the compiler PROVES your Rust, the terminal is a programmable surface an AI
can read and drive, and the whole TOOLCHAIN is installed and cryptographically
attested by one package manager (aterm itself updates through its own signed appcast). Every tool is Rust, offline-capable, and built to be driven
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
  behaves exactly like your shell. It does NOT model the screen: the host terminal draws
  the bytes and the session keeps no grid and no scrollback. (The in-process VT model is
  demand-driven and OFF by default; ATERM_SESSION_MODEL=1 arms it for an in-process
  consumer of the engine, and 0/off/empty do not. It is not readable from outside either
  way.) The shell runs through a protected spawn seam (capability-gated,
  setrlimit-bounded, fail-closed, OS-sandbox-wrapped on demand), not raw forkpty/execvp.
  This passthrough CLI serves NO control socket of its own; the live, introspectable
  surface an AI reads and drives (via `aterm ctl`) is exposed by the WINDOW mode of the
  same binary — `aterm --window`, or `aterm --headless` (ATERM_HEADLESS=1).
  See `aterm help introspection`.

KEY USAGE
  aterm                      start an interactive $SHELL (the default; no args)
  aterm <tool> [args]        run a pinned, store-resolved toolchain tool, e.g.
                             `aterm ay`, `aterm ty` (never $PATH — the managed build)
  aterm pkg <args>           the toolchain package manager (see `aterm help pkg`)
  aterm doctor               pre-flight health check; exit 0 = ready, and scriptable.
                             The `tty` row is a `note`, not a verdict: it measures
                             doctor's OWN stdout, so a piped, CI or agent run no longer
                             exits 1 on a healthy machine. Only `containment` and
                             `shell` move the code; `tty` and `privacy` never do.
  aterm show-config | validate-config | explain-config | list-fonts | list-themes
                             read-only diagnostics; print and exit, no shell spawned
  aterm --sandbox            run the shell under the macOS sandbox (deny net + secrets)

WHEN TO REACH FOR IT
  Use `aterm` for a daily-driver shell in the current terminal, or as the single
  launcher for the chain — `aterm <tool>` gives the pinned, attested build. Use
  `aterm --window` for a real window (tabs, splits, HiDPI). Use `aterm ctl` to introspect
  or drive a RUNNING instance from the outside.

GOTCHAS
  * `aterm <tool>` resolves through the managed STORE, never $PATH — a tool that is not
    installed falls through to a usage error. (`aterm pkg` is atpkg linked INTO this one
    binary, not a sibling executable: there is nothing to co-locate and nothing to be
    missing.) Historic note: pre-one-binary builds exited 127 when the sibling
    binary, never $PATH). Missing ⇒ `aterm pkg` exits 127; an unknown tool is a usage error.
  * Containment precedence: explicit flag > $ATERM_CONTAINMENT_MODE > default `user`;
    a malformed mode fails CLOSED to `containment`. The OS sandbox is actuated on macOS
    only; elsewhere it is rlimits + capability gate, and aterm says so on stderr.
  * macOS: `Operation not permitted` on a file macOS treats as private is privacy consent
    (TCC), not a broken tool — and it can arrive with NO dialog at all. `aterm doctor` has a
    `privacy:` row, `aterm ctl privacy` has the whole posture, and `aterm help permissions`
    says what to do about it. Only a human can grant it; aterm cannot.
  * `-h`/`--help` prints the terse CLI usage; `aterm help` (this manual) is the full guide."#,
        ),
    },
    Topic {
        name: "introspection",
        tagline: "read & drive any terminal via the control protocol (aterm ctl)",
        body: None, // generated — see `introspection_page()`
    },
    Topic {
        name: "conn",
        tagline: "session connections — wire sessions to pull/push each other",
        body: Some(
            r#"conn — session connections: standing pull/push wiring between terminal sessions
(`aterm conn`, the CLI face of the Session Connections fabric).

WHAT IT IS
  A CONNECTION lets one session drive another as a human could, at minimum: PUSH lands
  as keystrokes on the peer's PTY (plus the ^C signal a human's ctrl-C raises), PULL
  reads what a human would see (the rendered screen, blocks, cursor). Kinds: pull,
  push, or both (the default). Connections are per-SESSION authority rows — op-scoped,
  revocable, audited on the session_edge log — never a tool API, which is why a
  supervisor session can drive ANY interactive tool unmodified. `aterm conn` manages
  the standing wiring; the pull/push verbs themselves live on `aterm ctl`
  (turn / text / await — see `aterm help introspection`).

KEY USAGE
  aterm conn                 THIS session's connections: ⇥ outgoing, ⇤ incoming, ⇆ both
  aterm conn ls [--json]     every session connection in the instance
  aterm conn add @<sid>      take control of a session (@self -> peer, both);
                             --to-me inverts (invite a controller); --from <sel> wires
                             any third-party pair; --kind pull|push|both narrows it
  aterm conn set @<sid> --kind ...   declaratively reconfigure (exact set semantics)
  aterm conn rm @<sid> [--kind ...]  disconnect (kind-filtered ok)
  aterm conn spawn controlled|controller [--tab|--window] [--of <sel>]
                             spawn a new session pre-wired `both` (controller: the
                             newborn supervises --of, and its shell receives
                             ATERM_OBSERVE_SESSION_ID naming its charge)
  aterm conn show @<sid>     raise the peer's window + tab;   aterm conn map   the GUI map

WHEN TO REACH FOR IT
  The unattended-operator story: start a worker session (any coding agent), then
  `aterm conn spawn controller` from inside it — the newborn supervisor holds
  pull+push over the worker and drives it with `aterm ctl @<worker> turn '...'`
  whenever it needs an answer or a prod. Reach for `conn` whenever "which session is
  wired to which" is the question, or to add/dissolve that wiring from a shell.

GOTCHAS
  * Peers are SELECTORS (@self / @<sid> / @<local-id>) — never titles (ambiguous).
    Outside an aterm session the @self forms refuse and name $ATERM_PARENT_SESSION_ID;
    everything else targets the latest instance, exactly like `aterm ctl`.
  * Owner-authority only: the verbs ride the instance token beside the control socket
    (same user). A connection is standing wiring — dissolve it with `conn rm`, close
    either endpoint, or the GUI Disconnect; a cold restart dissolves by design.
  * `conn` is presentation over the wire verbs (connect / disconnect / flows / raise) —
    `aterm ctl connect dst=... src=...` is the same act, byte-for-byte."#,
        ),
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
  the agent's own skills directory (today, for Claude Code:
  `~/.claude/skills/drive-aterm/SKILL.md` — how to drive/observe ONE other aterm
  session over the control socket — and `~/.claude/skills/supervise-agent/SKILL.md` —
  the SUPERVISION loop on top: run a worker agent, review each turn against ground
  truth, escalate, resume). The skill content is compiled into the binary, so it
  updates with aterm and there is no second copy to drift.

KEY USAGE
  aterm agents               status: each agent, its context file + skills,
                             installed/stale/absent/foreign
  aterm agents install       install/update the block AND the bundled skills for every
                             DETECTED agent (its config dir exists); others are skipped
  aterm agents install codex force one agent by name (creates the file if needed)
  aterm agents remove        remove exactly the managed block, and aterm-owned skills
  aterm agents primer        print the block — paste into any project AGENTS.md/CLAUDE.md

WHEN TO REACH FOR IT
  Usually never — in a WINDOW. aterm runs this installer itself, in the background, at
  most once a minute, each time the window (or a --headless instance) opens
  a session — every DETECTED agent gets the current primer and skills, and nothing is
  written for an agent whose config dir does not exist (`agents_auto_prime = false` in
  aterm.toml turns the pass off; `aterm agents status` names the knob). Run
  `aterm agents install` to do the same on demand, `aterm agents` to check. A screen
  banner cannot do this job — an agent's context never sees the terminal's output, which
  is exactly why the primer rides in the agent's own files.

GOTCHAS
  * IDEMPOTENT and surgical: the block lives between `<!-- aterm primer ... -->` markers;
    re-running install updates it in place, `remove` deletes exactly the block, and user
    content outside the markers is never touched (an unterminated marker fails closed).
  * A bare `install` skips undetected agents (no config dir = not in use) — name an
    agent explicitly to force it.
  * The primer is intentionally short — four points in about a dozen lines: detection,
    `aterm help`, first moves (`aterm ctl windows` / `ls`, and read a peer's `status`
    before typing into it), and env hygiene. Depth
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
            r#"atpkg — the toolchain package manager. You type `aterm pkg`; its own messages
speak as "atpkg:".

WHAT IT IS
  The batteries behind aterm. atpkg installs and keeps current the toolchain
  programs published by one configurable account (trust, clean, ay, ny, ...) —
  and if you launched the aterm app, it has already run: first launch records
  adoption and installs the ALab toolset over the signed network index, unattended
  (adoption IS the consent; the one thing disclosed up front is size, summed from
  the signed cost rows), and the windowed app updates it from then on (CLI-only
  use never auto-updates; see GOTCHAS). Think "rustup married to a silent
  updater". Nothing installs except under the one trust root aterm itself updates
  under: a PAPER MASTER (public key compiled in; secret half on paper, on no
  computer) signs the roster of MACHINE keys, and a machine on that roster signs
  the freshness-stamped index and every package manifest — `aterm pkg doctor`
  prints the anchor as `paper master pinned (fingerprint …)`. Verification happens
  BEFORE any parse, enforced by construction: the only way to get the bytes the
  parser consumes is to pass a verify function — handing it unverified bytes does
  not type-check. `atpkg run` is the engine behind the `aterm <tool>` launcher,
  and atpkg also OWNS the seams the Trust toolchain reaches you through — rustup's
  `trust` link, PATH, and a checkout's toolchain pins — which is why `aterm pkg
  doctor` is the first thing to run when a build says a toolchain is missing.

KEY USAGE (spelled as you type them — daily verbs first)
  aterm pkg install --default-set
                             the whole ALab toolset in one step — the documented
                             consent act, and the usual first command on a CLI-only
                             box (the app's first launch runs it unattended)
  aterm pkg list             what you have: each program's live build, plus builds
                             kept for rollback — local, no network. Human table on
                             a terminal; pipe it (or pass --porcelain) for the
                             stable tab-separated form scripts parse
  aterm pkg doctor           is it healthy: every check prints its verdict, warns
                             included (`status` answers as the same report, signed
                             with the name you typed)
  aterm pkg update [program] upgrade all (or one) to the channel pin; coherence
                             groups apply all-or-nothing (the rustc-locked tuple
                             moves together)
  aterm pkg install <program> [--elevate=sudo|osascript|never]
                             (NOTE: no OS-installed or vendor-fetched member is published
                             yet — every program in today's index is a signed prebuilt, so
                             --elevate has nothing to apply to)
                             one program: verify the signed index, then install the
                             pinned build. THE EXPLICIT DOOR for a member the OS
                             installs with an administrator (Homebrew's pkg, Apple's
                             Command Line Tools, an apt/dnf package): in a terminal
                             sudo asks there; --elevate=osascript uses the system
                             dialog; without a terminal it records `needs admin` and
                             says so. The unattended pass never elevates. A member
                             the platform's own manager installs without elevation
                             (brew, winget, scoop, cargo, pipx) installs through
                             that manager; a machine without the manager reads it
                             as `unavailable on <target>` — atpkg never installs a
                             package manager
  aterm pkg verify [program] re-attest installed bytes against the signed root (no
                             network) — doctor reads health, verify re-proves bytes
  aterm pkg which <tool>     ONE line: which copy of a tool runs and why — managed
                             (shim → store path, pinned by index N), system copy
                             (not managed by aterm), SHADOWED by a copy ahead on
                             PATH, installed via another protocol (pkg,
                             softwareupdate, or a platform manager: apt, brew,
                             winget, …) at its proof path, or an extra awaiting
                             opt-in
  aterm pkg run <tool> [-- args]
                             exec the store binary — what `aterm <tool>` dispatches to
  aterm pkg seed             the first-launch bootstrap, runnable by hand. On a
                             lean app (every release since v0.63.0) it records
                             adoption, lays a pending stub for each default-set
                             name so the tools answer on PATH before their bytes
                             land, and consults the signed index — so it is NOT
                             offline; the update pass right behind it does the
                             installing. Only a pre-v0.63 seeded bundle fills the
                             store from its own bundled registry. The GUI
                             runs it once per launch; [packages].seed_install=false
                             turns it from adopt-and-install into announce-only

OCCASIONAL (recovery and preference)
  aterm pkg uninstall <program> | --all
                             remove one program, or the WHOLE managed toolset and its
                             disk (the signed on-disk sum — about 4 GiB today) in one
                             step — the way out is as single-step
                             as the way in. Either form stops atpkg auto-completing the
                             set; [packages].exclude drops one program while keeping it
  aterm pkg rollback <program>
                             reactivate the kept previous build — the undo for a bad
                             update (the `superseded (kept for rollback)` rows in list)
  aterm pkg pin | unpin <program>
                             hold a program at its current build through update passes /
                             release the hold (pins gate the coherence-group move)
  aterm pkg gc               reclaim superseded builds and interrupted downloads; it
                             says what it swept and why

PLUMBING (producer / operator / dev — a first hour never needs these)
  aterm pkg link <prog> <dir> | unlink | refresh
                             dev-link a sibling checkout's bins over a program; update
                             HARD-SKIPS a linked program until unlink; refresh re-asserts
                             links after a rebuild
  aterm pkg tree-root <dir>  print the SHA-256 tree_root the publish pipeline signs
  aterm pkg verify-index | verify-pkg <args…>
                             run the client's full trust chain over index/roster or
                             pkg-manifest files on disk (operator / mirror self-check)
  aterm pkg relocate <stage> producer pack-time: vendor machine-local dylibs into the
                             staged sysroot so the signed tarball is self-contained

WHEN TO REACH FOR IT
  To manage the published CLI toolchain — install / update / pin / verify — or to see
  what's installed. The managed tools do their own work; atpkg is what lays them down.
  Distinct from `targo --unverified ship`/aterm-release (which CUTS aterm.app itself).
  When a toolchain looks MISSING: rustup's exact words `error: toolchain 'trust' is not
  installed` mean the `trust` link under ~/.rustup no longer reaches the managed store.
  Run `aterm pkg doctor` — it names the seam that broke — then `aterm pkg repair`, which
  re-lays the shims and shell integration through the same code the install pass runs.
  Never rebuild a toolchain from source to answer that message.
  (`doctor` takes no flags: it reports, `repair` acts.)
  When a foreign shell needs the tools: prefix the command — `aterm <tool>` — or read the
  export line out of `aterm pkg doctor`, which prints it. Nothing writes into an rc file,
  by design.
  When you want the seams spelled out — which rustup link, which PATH hook, which
  checkout pins, and what each currently points at — `aterm pkg doctor` names them, and
  `aterm pkg status` / `aterm pkg which` answer the narrower questions.

GOTCHAS (in the order they bite)
  * PATH: the managed tools are on PATH only in shells started inside aterm (shell.d
    APPENDS the managed bin/, never shadowing system sudo/ssh/git). In a plain
    Terminal.app, over ssh, or in CI, use `aterm <tool>`, or copy the export line
    `aterm pkg doctor` prints. Nothing writes into ~/.zshrc, by design. And bin/ NEVER carries a `cargo`, `rustc`, or `rustup` shim
    (those names are on the sensitive-shim deny-list): cargo reaches the compiler
    through rustup's `trust` toolchain link, which atpkg points at `store/trust/current`
    and re-asserts on every install and update pass — a store update moves the
    compiler without touching rustup.
  * AUTOMATIC updates ride the windowed app: `aterm --window` runs the update pass on a
    6h loop (ATPKG_UPDATE_INTERVAL_SECS), and the app's own self-update check is likewise
    window-only. Headless or CLI-only usage updates NOTHING automatically — run
    `aterm pkg update` explicitly, or drive it from a scheduler (launchd/cron). Every
    update pass, automatic or by hand, also re-asserts the seams and the shell.d hooks,
    so a pass that moved no bytes still repairs a link or hook that drifted.
  * The root anchor is COMPILED IN — the paper master's public key, a committed constant
    (aterm-update-core::pins::PAPER_MASTER_PUBKEYS), not a build env var — so a plain
    `targo --unverified build` is fully armed. There is no atpkg-specific root and no
    rotatable release key any more: the machine keys the paper master's roster names
    sign everything, and a revoked machine stops being trusted at the next index fetch,
    with no aterm rebuild. The kill switch is ATPKG_DISABLE: set it and the network
    verbs (install/update/rollback) refuse with exit 1; local read/maintenance verbs
    (list/which/run/doctor/verify/...) still work.
  * `aterm pkg doctor` is the truth about THIS machine, not this page: every check
    prints ok / warn / FAIL, and its exit code carries the worst of them. It takes NO
    flags — it is a report. To act on what it finds, use `aterm pkg repair`, which
    re-lays the rustup link, the shims and the PATH hook through the same owner code the
    install pass runs, never a second implementation. (This page described
    `--fix`, `--strict` and `--porcelain` on `doctor` until 2026-09-01; `doctor`
    parses no arguments, so all three were silently ignored — and `--fix` was named
    as THE cure for a missing toolchain.)
  * Channel is hard-wired to "stable" today, and the roster `aterm pkg --help` prints is
    test-pinned to the dispatch table — it never advertises a verb that does not run."#,
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
  trustc                         a drop-in rustc that VERIFIES by default
                                 (`-Ztrust-verify=off` compiles as vanilla Rust)
  targo <cmd>                    REFUSED. targo has exactly two lanes and neither
                                 is silent — pick one:
  targo trust <cmd>                VERIFIED: fail-closed, with a per-unit proof
                                   report and a dependency-TCB ledger
  targo --unverified <cmd>         UNVERIFIED: the proof pipeline is off, and the
                                   artifact carries no proof claim
  aterm pkg install trust        install/upgrade the prebuilt toolchain (signed network index)

WHEN TO REACH FOR IT
  Use `targo trust check` whenever the goal is to compile AND prove real Rust — it is the
  only tool that reaches MIR-level invariants. `targo --unverified` is the ordinary
  vanilla-Rust build; a bare `targo build` is refused rather than quietly unverified,
  which is why every command in this manual names a lane. (`trustc` on its own DOES
  verify — that is why this repo's .cargo/config.toml passes `-Ztrust-verify=off`.) Reach for a leaf prover (ay/ty/clean/...) directly only to debug that backend.

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

KEY USAGE  (`aterm pkg install clean` — a signed prebuilt SHIPS; an aterm shell puts the
            managed bin/ on PATH, or use `aterm pkg run clean -- <SUB>`)
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
  * clean pins ay as an immutable GIT revision, deliberately NOT a `../ay` path
    dependency, so a dev tree needs no sibling checkout to build.
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
replacement and more), shipped as the `ty` CLI (with a `tla` companion shim).

WHAT IT IS
  The core is an explicit-state model checker built to match TLC's semantics (TLC is the
  behavioral oracle). Around it the one binary adds JSON output, TLA+ -> Rust codegen,
  deductive theorem proving, a Petri-net / Model Checking Contest frontend, and gated
  symbolic (BMC/IC3/PDR via `ay`) and hardware (AIGER/BTOR2) backends. Soundness-first:
  when uncertain it abstains rather than emit a wrong verdict.

KEY USAGE  (`aterm pkg install ty` — a signed prebuilt SHIPS; an aterm shell puts the
            managed bin/ on PATH, or use `aterm pkg run ty -- <SUB>`)
  ty check Spec.tla --config Spec.cfg [--workers N] [--output json]
                                 explicit-state model checking (the TLC replacement)
  ty prove Spec.tla [-c Spec.cfg] [-o cert.json]
                                 unbounded inductive-safety proof; emits a `ty.cert/v1`
                                 certificate and re-checks it (`ty recheck` replays one)
  ty induct                      check an inductive invariant
  ty corpus fetch                download the sha256-verified benchmark corpus (not in repo)
  ty supremacy compare | reproduce  TLC-vs-ty parity evidence (needs a TLC jar + JDK);
                                 `ty corpus doctor` is the preflight
  ty aiger circuit.aig           hardware model checking (AIGER/BTOR2)

WHEN TO REACH FOR IT
  `ty check` for finite/bounded TLA+ model checking; `ty prove`/`ty induct` for an
  unbounded/deductive result; `ty mcc`/`ty petri` for Petri nets; `ty aiger`/`ty btor2`
  for hardware. Drop to `ay` only for raw SAT/SMT/CHC that ty already wraps.

GOTCHAS
  * INSTALL: a signed prebuilt SHIPS — `aterm pkg install ty`, or the whole toolset with
    `aterm pkg install --default-set`. Only if you build from source instead: on macOS
    install GNU m4 first (`brew install m4`) — a transitive build dep needs it.
  * The symbolic (BMC/k-induction/IC3-PDR) and hardware (AIGER/BTOR2) surfaces are ON by
    default — `ay` is a default feature of the ty CLI, so the installed prebuilt has them.
    Only a deliberate `--no-default-features` source build drops them.
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

KEY USAGE  (`aterm pkg install ay` — a signed prebuilt SHIPS in the default set; an aterm
            shell has it on PATH, or `aterm pkg run ay -- <file>`)
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
  * Inside an aterm shell the `ay` on PATH is the SIGNED PREBUILT from the index — the
    trust bundle vendors an ay internally but does not expose it. In a source checkout,
    `targo --unverified run -p ay --features cli -- <file>` runs ~/ay's own code instead.
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
  rounding) — what `nn` calls "gamma-crown". On eligible nets it ships an exact-rational,
  machine-checkable `<model>.cert.json` proof sidecar.

KEY USAGE  (`aterm pkg install ny` — a signed prebuilt SHIPS; an aterm shell puts the
            managed bin/ on PATH, or use `aterm pkg run ny -- <SUB>`)
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
  dedicated verifier; `nn` is the framework that consumes it; ay is the solver it delegates to.

GOTCHAS
  * Default workspace check must exclude the Python crate: `targo --unverified check --workspace
    --exclude ny-python`. MIP needs `--features mip`.
  * Soundness is opt-OUT: `--allow-unsound-gpu-crown` trades correctness for speed and can
    flip a violated instance to Verified — never use it when the verdict must be trusted.
    Use `ny verify --require-sound` to reject heuristic paths."#,
        ),
    },
    Topic {
        name: "nn",
        tagline: "verified ML framework — torch.export → Metal (the shipping line)",
        body: Some(
            r#"nn — "NN, Verified ML Framework": a Rust ML framework whose `nn` CLI compiles exported
PyTorch models into verifiable Metal inference. It is a default-set program, pinned in
the signed index and installed on first launch, so an aterm shell has `nn` on PATH
already.

WHAT IT IS
  Model code, GPU kernels, and proof tooling in one workspace (nn-core, nn-metal,
  nn-import, nn-verify, ...). Production story: Kani-backed GPU-kernel verification, Metal
  as the real backend, a torch.export + safetensors import bridge emitting a ConvertReport.
  It links `ny` for IBP/CROWN bound propagation and `ay` for SMT, layering formal checks
  (types -> ny bounds -> ay SMT) on top of compiled Metal inference. Same convert/compile/
  run/optimize CLI shape.

KEY USAGE  (`aterm pkg install nn` — a signed prebuilt SHIPS; an aterm shell puts the
            managed bin/ on PATH, or `aterm pkg run nn -- <SUB>`; macOS + Metal required)
  nn convert graph.json weights.safetensors [--optimize ...] [--verify bounds|full]
                               compile pre-exported artifacts -> Metal model + ConvertReport
  nn compile ... --output model.nnc    persist a .nnc plan (+ report)
  nn run ... --input inputs.safetensors   execute on the Metal GPU
  nn optimize model.nnc ...    time-budgeted peephole search vs the baseline plan

WHEN TO REACH FOR IT
  To compile/run/optimize an exported PyTorch model into a Metal pipeline on the nn tree.
  For framework work reach for `nn`; use `ny` to verify a network's
  bounds and `ay` for raw solving.

GOTCHAS
  * NEVER run a bare workspace `targo --unverified test` here — it has kernel-panicked the machine (OOM);
    several test binaries are enormous. Use `targo --unverified nextest run` (honors the single-threaded
    `heavy` group) or `scripts/test-capped.sh` in the nn tree; the biggest GPU tests
    are #[ignore]'d.
  * Intake rule: pre-exported torch.export graph.json + safetensors, not raw
    ONNX/.pt. `--verify` is a report request, feature-gated."#,
        ),
    },
];

/// The dispatchable pages that are NOT [`TOPICS`] entries — generated, or named
/// after a verb rather than a tool. One list, because it was two: the unknown-topic
/// listing and its test each carried their own copy, so a page could be added to
/// one and stay invisible in the other. `agent` also answers to `instructions`,
/// which is an alias rather than a page.
const EXTRA_PAGES: &[&str] = &[
    "config",
    "ship",
    "update",
    "windowing",
    "drive",
    "fleet",
    "trust-backends",
    "permissions",
    "agent",
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
    // KEYED ON THE ROSTER, not hand-listed. `crate::Verb` calls itself "THE
    // front-door verb roster — the ONE place a verb exists", and this page had
    // drifted out of it: `ship` and `update` were front-door verbs with usage
    // lines and blurbs that this manual never mentioned, so the only way to
    // learn `aterm ship` existed was to read the source. `ship` is the verb
    // that motivated the roster in the first place, which is the whole joke.
    //
    // The SIGNATURE column comes from `Verb::usage()` so it can never disagree
    // with the parser. The description stays here because this table wants one
    // tuned line, while `blurb()` is a multi-line `--help` paragraph. A verb
    // with no line here is a compile error (the match is exhaustive) and an
    // omitted verb is a test failure (`overview_lists_every_front_door_verb`).
    let line = |v: crate::Verb| -> &'static str {
        match v {
            crate::Verb::Ctl => {
                "introspect & drive any terminal (read / keys / turn / subscribe / image)"
            }
            crate::Verb::Conn => {
                "session connections — see & wire which sessions pull/push each other"
            }
            crate::Verb::Pkg => "install / update / verify the toolchain",
            crate::Verb::Fleet => {
                "opt-in durable attention queue + guarded turns; legacy federation"
            }
            crate::Verb::Drive => "the agent drive CLI (prompt / read / await / shot)",
            crate::Verb::Ship => {
                "publish aterm: provision a signing machine, cut a release (source checkout only)"
            }
            crate::Verb::Update => "check or report auto-update state headlessly (status | check)",
            crate::Verb::Agents => {
                "make coding agents aterm-aware (the primer; aterm also installs it itself)"
            }
            crate::Verb::NewTab => "open a terminal tab (where it opens is `windowing_behavior`)",
            crate::Verb::NewWindow => "open a NEW window, always",
            crate::Verb::SplitPane => "split the current pane",
        }
    };
    const HELP_USAGE: &str = "aterm help [topic]";
    const HELP_LINE: &str = "this manual — start here (a deep dive on any verb or tool)";
    // Column width across the verb signatures and the topic names.
    let width = TOPICS
        .iter()
        .map(|t| t.name.len())
        .chain(std::iter::once(HELP_USAGE.len()))
        .chain(crate::Verb::ALL.iter().map(|v| v.usage().len()))
        .max()
        .unwrap_or(14);
    let _ = writeln!(s, "  {HELP_USAGE:<width$}  {HELP_LINE}");
    for v in crate::Verb::ALL {
        let _ = writeln!(s, "  {:<width$}  {}", v.usage(), line(*v));
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

/// `aterm help config` — the page whose absence the 2026-08-30 audit called a
/// blocker: the manual twice told the reader to set config keys
/// (`windowing_behavior`, `agents_auto_prime`) and never once said where the
/// file lives, while `explain-config` — whose blurb is "Explain how aterm
/// resolves its configuration" — explains only containment modes and three
/// environment variables. The path and precedence here are
/// `aterm_gui::app_config::config_path` and the window help's CONFIG block.
const CONFIG_PAGE: &str = r#"config — where aterm's settings live

THE FILE
  $XDG_CONFIG_HOME/aterm/aterm.toml   when XDG_CONFIG_HOME is set
  ~/.config/aterm/aterm.toml          otherwise (macOS and Linux)
  %APPDATA%\aterm\aterm.toml          on Windows
  It does not have to exist: every key has a default.

PRECEDENCE
  command-line flag  >  environment  >  config file  >  built-in default
  (Exception: `fallback_fonts`, `symbol_font` and `emoji_font` take the CONFIG value
  over the environment — the reverse of the line above.)

START ONE
  aterm --window --write-config    writes a documented starter aterm.toml — 158
                                   keys, each with its default and a comment (not
                                   quite every key: see THE KEY ROSTER below).
  Settings are reloaded live: save the file and the running app picks it up.
  (Launch- and session-scoped keys say so in their comments; they apply to the
  next window or the next session rather than instantly.)

THE KEY ROSTER
  aterm --window --help            the largest reference: Appearance, Window/Tabs,
                                   Cursor, Sound, Text, Behaviour, Security and Keys.
  Not every key is in that block. `windowing_behavior` (where `aterm new-tab` opens)
  and `agents_auto_prime` (the coding-agent primer) are documented HERE and in
  `aterm help windowing` / `aterm help agents` — they appear in neither the window
  help's CONFIG block nor the starter file. `[packages].seed_install` and
  `cursor_trail` / `cursor_trail_style` are in the starter file.

WHAT THE DIAGNOSTIC SUBCOMMANDS COVER
  aterm show-config | validate-config | explain-config
  These report the RUNTIME resolution — containment mode, the environment
  variables, shell and terminal size — not the contents of the file above.
"#;

/// `aterm help ship`. Advertised as a front-door verb since the roster existed;
/// it answered "unknown topic" until 2026-08-30.
const SHIP_PAGE: &str = r#"ship — publish aterm: provision a signing machine, cut a release

  aterm ship <args>                  (in a source checkout: `targo --unverified ship <args>`)

This verb is the release cutter, `crates/aterm-release`. It is NOT carried by an
ordinary install — it needs a source checkout of the aterm repo, because a cut
builds the app it publishes.

PROVISION — make this machine able to publish
  aterm ship provision --id <machine-id> --check
      A NO-WRITES audit: the roster, the Trust toolchain and verifiers, the
      rustup front door, the x86 slice, Apple's packaging tools, the Developer
      ID identity, a live-tested notary credential, `gh` auth and the channel
      token. Run this first; it names every gap and the exact fix.
  aterm ship provision --id <machine-id>
      The same audit, then — only on a clean pass — the key-mint ceremony, which
      asks for the paper master phrase at the terminal. Keys are never copied
      between machines. The mint is LAST on purpose: a roster id is irreversible,
      so it is never spent on a machine the audit just failed.

CUT — publish a release
  aterm ship cut [--dry-run] [--resume] [--arm64-only] [--rehearse OWNER/REPO]
      gates -> ledger claim -> universal build -> bundle/sign/DMG -> draft-first
      publish -> late tag -> flip -> verify -> mirror.
      --dry-run builds everything locally and uploads nothing.

THE ORDER IS ENFORCED
  Publish the SOURCE first (`pub stage aterm && pub promote aterm`), then cut the
  BINARY. A cut whose version the public channel does not already carry is
  refused, and so is a cutter binary older than the tree it is cutting.

  aterm ship --help          every flag, including recovery (--resume, --abandon)
  docs/RELEASING.md          the full runbook, including what to do when a cut
                             stops half-way
"#;

/// `aterm help update`. `aterm update --help` is a usage error (the verb takes
/// only `status` / `check`), so the manual is the only place this is explained.
const UPDATE_PAGE: &str = r#"update — check or report aterm's own auto-update state, headlessly

  aterm update status      what this copy knows: the running build, whether a
                           newer one is staged, and why the updater is idle
  aterm update check       ask the channel now, instead of waiting for the timer

  (`aterm update --help` is a usage error — those two are the whole verb.)

WHY IT EXISTS
  The windowed app updates itself silently. This is the lane a terminal-only
  machine uses to learn it is stale: it needs no window and no control socket.

  macOS ONLY. Auto-update is compiled for macOS; elsewhere `aterm update status`
  answers "auto-update is macOS-only; nothing to report on this platform", and
  everything below applies to macOS.

WHEN IT REPORTS THAT IT CANNOT UPDATE
  Three different reasons, and only two of them are yours to fix:
    * running from a MOUNTED DISK IMAGE, or from an App-TRANSLOCATED download
      (unzipped and opened without being moved first) — move aterm into
      Applications and open it from there. Until then it also cannot put `aterm`
      on a new shell's PATH.
    * a DEV BUILD (a `cargo run` or a `target/` binary) — nothing is wrong and
      there is nothing to move; the updater simply never owns such a copy.

  aterm help atpkg         the TOOLCHAIN's updates, which are a separate thing
"#;

/// `aterm help windowing` — and the landing page for `new-tab` / `new-window` /
/// `split-pane`, three rostered verbs that had no documentation.
const WINDOWING_PAGE: &str = r#"windowing — open tabs, windows and panes from the command line

  aterm new-tab    [-d <dir>]    open a terminal tab
  aterm new-window [-d <dir>]    open a NEW window, always
  aterm split-pane [-H|-V] [-d <dir>]   split the current pane

  -d <dir>   start in that directory
  -H         split stacked (horizontal divider)
  -V         split side-by-side (the default)

The grammar is deliberately Windows Terminal's: the whole value of a familiar
grammar is that the words are the same words.

WHERE new-tab ACTUALLY OPENS
  That is the `windowing_behavior` config key:
    windowing_behavior = "new_window"   a new window (the DEFAULT)
    windowing_behavior = "attach"       a tab in the already-running aterm
  Windows Terminal's spellings work as aliases (`useNew` / `useExisting`), and
  $ATERM_WINDOWING_BEHAVIOR overrides the file. `new-window` ignores the key and
  always opens a window. See `aterm help config` for where to set it.

  aterm --window --help          the full window-mode flag reference
"#;

/// `aterm help drive`. Previously aliased onto the introspection page, which
/// never mentions `aterm drive` at all.
const DRIVE_PAGE: &str = r#"drive — drive an interactive agent running inside aterm

  aterm drive [--socket PATH] [--idle MS] [--timeout MS] [--ready REGEX] <command>

A host aterm runs your target program (say a coding agent) as its child and
exposes a control socket. This reads the live screen and sends keystrokes over
the same verbs `aterm ctl` uses. The primitive that matters is `await`: block
until the surface reaches a condition, so you never sleep-and-hope.

COMMANDS
  prompt <text...>   type it, press Enter, block until the turn SETTLES (no
                     screen change for --idle ms), then print the settled
                     screen. This is the one you want in a loop.
  read               print the live screen, one row per line
  await <cond>       block until a condition, then print the verdict:
                       idle <ms>      surface unchanged for <ms> (turn done)
                       match <regex>  a visible row matches
                       seq            the next content change lands
                       block          a shell command completes (OSC-133)
  shot [path]        save a pixel-true PNG of the terminal content

  aterm drive --help       every flag
  aterm help introspection the control protocol underneath
"#;

/// The folders macOS asks about, named from the consent module's own table
/// instead of typed here.
///
/// Two reasons, and the second is the load-bearing one. The path literals live
/// in exactly ONE module (`aterm_containment::consent`) and reach the CLI as
/// data — design §3.3 guardrail 4, fenced by `tools/grep_guard.sh` B13 — and
/// this is also what stops the manual from drifting away from the list the
/// warm-up and `aterm ctl privacy` actually use. These are folder NAMES in
/// prose, never a path anything opens.
fn protected_folder_names() -> String {
    aterm_containment::Folder::ALL
        .iter()
        .map(|f| f.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `aterm help permissions` — design §5.5, the page an agent lands on after an
/// EPERM it did not cause.
///
/// Generated rather than a `const` so the folder list comes from
/// [`protected_folder_names`]. Every claim here is one an agent otherwise gets
/// wrong in a way that costs a session: that a missing dialog means it is not a
/// permissions wall, that retrying might work, that `sudo` is the escalation, or
/// that aterm could grant this if it wanted to.
///
/// What it deliberately does NOT say is that the grant ends macOS consent
/// dialogs. Which services a grant covers is not measured (design §7 S4), so the
/// page describes what the grant is and stops there.
fn permissions_page() -> String {
    let folders = protected_folder_names();
    format!(
        "\
permissions — macOS privacy (TCC), and the EPERM that arrives with no dialog

WHAT YOU ARE SEEING
  `Operation not permitted` (EPERM) is macOS PRIVACY when the path is under one of the
  folders macOS protects ({folders}), an external or network volume, a
  folder another app syncs for you, or another app's private data. Not a broken tool, not
  a bad path, and not a bug in aterm. It can arrive with NO DIALOG AT ALL, so \"nothing
  popped up\" does not mean this is not a permissions wall — and when a dialog IS raised,
  it is a system modal only a human can answer, the syscall that raised it is parked until
  they do, and it never times out.

WHAT TO DO, IN ORDER
  1. `aterm ctl privacy`               the whole posture, before you retry anything.
  2. Read `full_disk_access=`, `prompt_possible=`, and your own session's `fs_consent=`
     and `attribution=` (`aterm ctl @<sid> status` carries the last two per session).
  3. Tell your operator what you found. If `full_disk_access=denied`, one grant in
     System Settings ▸ Privacy & Security ▸ Full Disk Access removes this class of
     interruption for the folders that grant covers. YOU cannot grant it, and neither
     can aterm — only a human, in Settings.
  4. `aterm ctl @<sid> await consent timeout=<ms>` parks until the posture CHANGES.
     A latch means aterm's own posture moved; aterm cannot see what a human clicked.

WHAT NOT TO DO
  * Do not retry in a loop. macOS asks a human once and remembers the answer; a retry
    either hits the same wall or parks another syscall behind the same unanswered dialog.
  * Do not `sudo`. Privacy consent is per-app, not per-user: root does not carry it, and
    the same denial applies.
  * Do not rewrite the path, copy the file somewhere else, or work around it silently.
    The operator ends up with a result they cannot reproduce and never learns why.
  * Do not report the tool as broken. Name the wall: which path, and what `privacy` said.

WHY IT NAMES ATERM, NOT YOUR TOOL
  macOS keys file access to the RESPONSIBLE process, and every program started inside a
  session inherits aterm as that process. So the alert names aterm however deep the
  process tree goes, and a grant made for aterm is what the program you ran gets. Run from
  a shell under another terminal, the verdict belongs to THAT terminal instead — which is
  why `aterm doctor`'s `privacy:` row names the responsible app rather than assuming
  itself.

GOTCHAS
  * `attribution=adopted` means this session outlived the aterm process that started it
    (an update was applied in place; your shell kept running). Its file access may differ
    from a fresh tab's — opening a new tab is a valid recovery, and cheap.
  * Per-folder state is `unknown` BY CONSTRUCTION: the only way to learn whether a folder
    is readable is to read it, which is the very act that raises the prompt. `unknown` is
    not `denied`, and neither is inferred from the Full Disk Access state.
  * A dev build's grants do not persist: its code identity changes on every build, and
    macOS keys the grant to that identity. `aterm doctor` says so when it applies.
  * This whole page is macOS. Elsewhere an EPERM is an ordinary permission error.
"
    )
}

/// `aterm help fleet`. `fleet` was a front-door verb aliased onto the
/// introspection page, whose entire fleet content was four lines — while the
/// verb carries a whole claim lifecycle nothing documented.
const FLEET_PAGE: &str = r#"fleet — federate many aterm sessions into one fabric

  aterm fleet <command>

The embedded operator is EXPERIMENTAL and OFF by default. Launch an instance with
$ATERM_OPERATOR=1 to opt in; it then starts with an empty allowlist, so nothing is
observed until you `manage` a session. $ATERM_NO_OPERATOR overrides the opt-in.

STREAMS (no operator needed)
  aterm fleet events         merge every live instance's `subscribe events` to stdout as
                             NDJSON, addressed /fleet/<pid>/events/<sid>
  aterm fleet exec           read `@<sid> <verb> [args…]` lines on stdin, dispatch each to
                             the fleet, emit one NDJSON result per line

THE ATTENTION QUEUE (the operator's own lifecycle)
  aterm fleet status         the operator and the managed allowlist — start here
  aterm fleet manage <sid>   put a session under observation | unmanage <sid> to stop
  aterm fleet next [timeout=<ms>]
                             claim the next event needing attention; returns a CLAIM TOKEN
  aterm fleet extend <event> <claim-token> [ms=<n>]
                             keep a claim alive while you work
  aterm fleet ack <event> <claim-token> <no-action|pause|escalate>
  aterm fleet reconcile <event> <claim-token> <acted|no-action|pause|escalate> confirm=human
                             close the loop after acting. `confirm=human` is deliberate:
                             the fabric will not let an agent silently self-certify
  aterm fleet inspect <event>
  aterm fleet clear-fault confirm=human
  aterm fleet propose        read one JSON proposal for a guarded interactive turn on
                             stdin — the lane by which an agent asks to type into a
                             session it does not own

  aterm fleet --help         every command and its arguments
  aterm help introspection   the control protocol the fabric moves
"#;

/// `aterm help trust-backends` — the four default-set programs the manual never
/// named. They install with everything else, so a reader meets them in
/// `aterm pkg list` and had nowhere to look them up.
const TRUST_BACKENDS_PAGE: &str = r#"trust-backends — the verifier programs that install alongside trust

These four are default-set programs like any other: pinned in the signed index and
installed on first launch. You rarely invoke them — `targo trust check` drives them —
but they appear in `aterm pkg list`, so here is what each is.

THE rustc COHERENCE GROUP  (trust-ir, trust-cg, trust-vc, trust)
  All four are compiled by the SAME self-hosted Trust stage2 and move
  all-or-nothing: Rust has no stable ABI, so the members interoperate only when
  they come from one build. If one member cannot stage, the whole group is held
  back on every client — deliberately.

  trust-ir   the Trust IR: exposes `trust-ir`, `trust-ir-diff`, `trust-ir-fmt` —
             inspect and diff the intermediate representation a verification run
             produced. Reach for it when a proof fails and you want to see the IR.
  trust-cg   the certificate generator (`trust-cg`).
  trust-vc   verification-condition checking: `trust-vc` and `cargo-trust-vc`, so
             it also resolves as `cargo trust-vc`. `check <crate>` parses with syn
             and discharges VCs on ay in-process.
  trust      the compiler itself — see `aterm help trust`.

NOT IN THE GROUP
  trust-mc   the Trust model checker, its own sysroot bundle since 2026-08-19. It
             does NOT ship inside the trust bundle; the engine that most users
             actually reach is statically linked into `targo-trust`.

  aterm pkg list             what is installed, and at which build
  aterm help trust           the compiler these serve
"#;

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
  `@<sid>` routes a verb to that session, relayed transparently to any sibling instance
  of the same user ON THIS MACHINE. Another machine is the opt-in network path
  (`aterm ctl dial <name>` / `dial-list`), not this one.

THE MOVES (an AI's loop is see -> decide -> drive -> observe)
  SEE     aterm ctl @sid text | screen | image f.png | cast frames count=8
  DRIVE   aterm ctl @sid turn 'message'   (verified type -> submit -> settle -> reply)
          aterm ctl @sid send '...' | key enter | paste | resize <r> <c>
  OBSERVE aterm ctl @sid await idle <ms> | await match <re> | ready | wait
  WATCH   aterm ctl subscribe @a,@b,@c events     (the whole fleet on ONE fd, low-rate)
  FLEET   aterm ctl ls        (every session of every instance: pid sid state)
          aterm fleet status | manage <sid> | next   (durable operator; empty allowlist)
          aterm fleet propose < proposal.json        (Owner-only guarded interactive turn)
          aterm fleet events | exec                  (legacy NDJSON federation/dispatch)
          EXPERIMENTAL and OFF by default; ATERM_OPERATOR=1 opts in (docs/OPERATOR-EMBEDDED.md)
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
    // The manual is the one place the FULL entries are always in reach without a live
    // instance; the live server's bare `help` is the short form (this line says so).
    s.push_str(
        "`aterm ctl help` prints the short catalog; `aterm ctl help <verb>` the full entry for one verb.\n",
    );
    for line in aterm_types::control_verbs::catalog_lines_full() {
        s.push_str("  ");
        s.push_str(&line);
        s.push('\n');
    }
    s.push_str(
        "\nThis catalog is generated from the one typed verb table the control server answers\n\
         `help` from — so it is always exactly the protocol this build speaks. Run\n\
         `aterm ctl help --full` against a live session for the same list from the server itself.\n",
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
             See yourself:   aterm ctl @{id} text trim   (trim drops the trailing blank rows)\n  \
             Drive yourself: aterm ctl @{id} turn 'message'   (rarely needed — you ARE the shell)\n  \
             Find peers:     aterm ctl windows  AND  aterm ctl ls\n  \
             \x20               windows: one row per window, which sids sit on its active tab;\n  \
             \x20               ls: every session, with its window= and detail= (the program it is\n  \
             \x20               running; * marks you). Then drive any peer with @<its-sid>.",
        );
    }
    s.push_str(
        "\n\nHOW TO SEE, DRIVE, AND COORDINATE (the introspection control protocol)\n  \
         The loop is see -> decide -> drive -> observe. Read a peer with `@sid text` or a real\n  \
         frame with `@sid image`; drive it with `@sid turn 'msg'` (verified submit + settle +\n  \
         reply); wait without polling via `@sid await idle <ms>` / `await match <re>`; watch a\n  \
         whole fleet on one descriptor with `subscribe @a,@b events`. Humans can interject at\n  \
         any time — the input path is the human's, and a per-session turn lease arbitrates so\n  \
         two drivers never clobber each other. Full detail: `aterm help introspection`.\n  \
         Cheaper reads: `text trim` / `turn trim=1` drop the trailing blank rows (`OK <n>\n  \
         trimmed=<k>`). Place work: `spawn window=<id>` opens a tab in that window WITHOUT\n  \
         raising it (ids from `windows`); `@<sid> spawn` means the window hosting <sid>.\n  \
         A vanished session: `exits [since=<id>]` says when it went, why, and by whom.\n  \
         If `ls` finds nothing it says WHY (a sandbox refusing the socket, a stale socket, an\n  \
         unreadable token) — act on the reason; it never means \"empty\" unless it says so.\n",
    );
    // §5.5. This sits BEFORE the tool list because it is the failure an agent
    // meets first and misreads worst: an EPERM that no dialog announced reads as
    // a broken tool, and the recovery an agent reaches for by reflex — retry,
    // then `sudo`, then a different path — is wrong three times over. The folder
    // names come from the consent module (grep_guard B13); the page behind
    // `aterm help permissions` carries the rest.
    let _ = write!(
        s,
        "\nMACOS FILE PERMISSIONS (an EPERM that may arrive with no dialog)\n  \
         `Operation not permitted` on a path under one of the folders macOS protects\n  \
         ({}), an external or network volume, a folder another app syncs\n  \
         for you, or another app's private data is macOS privacy — not a broken tool — and\n  \
         it can arrive with NO DIALOG AT ALL, so \"nothing popped up\" does not mean this is\n  \
         not a permissions wall. Run `aterm ctl privacy` BEFORE retrying. Do not retry in a\n  \
         loop, do not `sudo`, do not rewrite the path: macOS asks a human once and remembers\n  \
         the answer, and an unanswered dialog never times out. If `full_disk_access=denied`,\n  \
         tell your operator that one grant in System Settings ▸ Privacy & Security ▸ Full\n  \
         Disk Access removes this class of interruption for the folders that grant covers —\n  \
         you cannot grant it and neither can aterm. `aterm ctl @<sid> await consent\n  \
         timeout=<ms>` parks until the posture changes. If `attribution=adopted`, this\n  \
         session outlived the aterm process that started it and its file access may differ\n  \
         from a fresh tab's — opening a new tab is a valid recovery.\n  \
         Full page: `aterm help permissions`.\n",
        protected_folder_names(),
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
         \x20 ny     verify a neural network (CROWN/beta-CROWN)       trust-mc  the Trust model checker\n\
         \x20 nn     ML framework (torch.export -> Metal)            atpkg  install/update/verify the chain (run as `aterm pkg`)\n",
    );
    s.push_str(
        "\nHOUSE RULES (this toolchain is honesty-first)\n  \
         * Peers may be agents. Before you `turn` or `send` into a peer, read its `status`\n    \
         (detail= names the running program: claude, codex, ...) and its `meta role=`; never\n    \
         type into another agent's prompt unless the human named the session AND the message.\n  \
         * Never claim a prover/compiler ran or 'proved' something that didn't — an empty or\n    \
         zero-obligation report is not a proof. Say what actually executed.\n  \
         * No git hooks and no CI anywhere in this toolchain, by owner mandate — gating is\n    \
         inline/optional in the tools (e.g. `targo trust check`, `clean audit`, `ty ... gate`).\n  \
         * Each tool's own AGENTS.md/CLAUDE.md rules win in its repo (e.g. never a bare\n    \
         `targo --unverified test` in nn; always `--locked` in clean).\n",
    );
    s.push_str(
        "\nGO DEEPER\n  aterm help                 the command map for the whole toolchain\n",
    );
    s.push_str("  aterm help <topic>         a deep dive on any tool\n");
    s.push_str(
        "  aterm ctl help             the short verb catalog from a running session;\n\
         \x20                            `help <verb>` one full entry, `help --full` everything\n",
    );
    s.push_str(
        "  aterm agents               the coding-agent primer — how an agent like you learns\n\
         \x20                            aterm exists; aterm installs it itself (see `aterm help agents`)\n",
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
    // EVERY front-door verb resolves — the front page promises `aterm help
    // [topic]` is "a deep dive on any verb or tool", and five of the eleven
    // verbs answered "unknown topic" until 2026-08-30 (`ship` among them, which
    // is how you were supposed to learn this machine can publish at all).
    // `drive` used to land on the introspection page, which never mentions it.
    // Pinned by `every_front_door_verb_resolves`.
    let topic = topic.map(|t| match t {
        "ctl" => "introspection",
        "trust-mc" | "trust-ir" | "trust-cg" | "trust-vc" => "trust-backends",
        "pkg" => "atpkg",
        "new-tab" | "new-window" | "split-pane" => "windowing",
        "settings" => "config",
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
        Some("config") => (CONFIG_PAGE.to_string(), 0),
        Some("ship") => (SHIP_PAGE.to_string(), 0),
        Some("update") => (UPDATE_PAGE.to_string(), 0),
        Some("windowing") => (WINDOWING_PAGE.to_string(), 0),
        Some("drive") => (DRIVE_PAGE.to_string(), 0),
        Some("permissions") => (permissions_page(), 0),
        Some("fleet") => (FLEET_PAGE.to_string(), 0),
        Some("trust-backends") => (TRUST_BACKENDS_PAGE.to_string(), 0),
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
                // The pages that are not TOPICS entries: generated or
                // verb-shaped, but every bit as real to someone guessing.
                for extra in EXTRA_PAGES {
                    let _ = writeln!(msg, "  {extra}");
                }
                (msg, 2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overview page must name EVERY front-door verb. It did not: `ship`
    /// and `update` carried usage lines and blurbs in `crate::Verb` — the roster
    /// that documents itself as "the ONE place a verb exists" — while this
    /// manual, the thing a person actually reads, listed neither, so the only
    /// way to discover `aterm ship` was to read the source. The windowing three
    /// were missing too. Keyed on the roster now; this pins it.
    #[test]
    fn overview_lists_every_front_door_verb() {
        let page = overview_page();
        for v in crate::Verb::ALL {
            assert!(
                page.contains(v.usage()),
                "`aterm help` never mentions the {} verb (looked for {:?})",
                v.name(),
                v.usage()
            );
        }
        // ...and the verb that motivated the roster, named outright, so a future
        // refactor that "simplifies" the loop cannot quietly drop it again.
        assert!(page.contains("aterm ship <args>"), "{page}");
        assert!(page.contains("aterm help [topic]"), "{page}");
    }

    /// The front page promises `aterm help [topic]` is "a deep dive on any verb
    /// or tool". Five of the eleven verbs answered "unknown topic" until
    /// 2026-08-30 — `ship` among them, so the only way to learn this machine can
    /// publish was to read the source. Every rostered verb must RESOLVE.
    #[test]
    fn every_front_door_verb_resolves() {
        for v in crate::Verb::ALL {
            let (page, code) = render(Some(v.name()), None);
            assert_eq!(code, 0, "`aterm help {}` exits {code}", v.name());
            assert!(
                !page.contains("unknown topic"),
                "`aterm help {}` 404s",
                v.name()
            );
            assert!(page.len() > 200, "`aterm help {}` is a stub", v.name());
        }
    }

    /// `drive` used to alias onto the introspection page, which never mentions
    /// `aterm drive` — a redirect that looks like documentation and is not.
    #[test]
    fn a_verbs_page_actually_mentions_that_verb() {
        for name in ["drive", "ship", "update"] {
            let (page, _) = render(Some(name), None);
            assert!(
                page.contains(name),
                "`aterm help {name}` never says {name:?}"
            );
        }
    }

    /// The manual told readers to set config keys and never said where the file
    /// is — the 2026-08-30 audit's one blocker.
    #[test]
    fn the_manual_says_where_the_config_file_lives() {
        let (page, code) = render(Some("config"), None);
        assert_eq!(code, 0);
        assert!(page.contains(".config/aterm/aterm.toml"), "{page}");
        assert!(page.contains("XDG_CONFIG_HOME"), "{page}");
        assert!(page.contains("--write-config"), "{page}");
        // ...and the keys the rest of the manual names must be findable from it.
        assert!(page.contains("windowing_behavior"), "{page}");
        assert!(page.contains("agents_auto_prime"), "{page}");
    }

    /// A guessed topic must list the pages that exist — including the ones that
    /// are not TOPICS entries, which were invisible.
    #[test]
    fn the_unknown_topic_listing_names_the_extra_pages() {
        let (msg, code) = render(Some("no-such-topic"), None);
        assert_eq!(code, 2);
        for extra in EXTRA_PAGES {
            assert!(msg.contains(extra), "the listing omits {extra}: {msg}");
        }
    }

    /// The retired lane must not be promised anywhere: the sealed offline seed
    /// went on 2026-08-26, and the manual still described it as how first launch
    /// fills the store.
    #[test]
    fn no_page_still_promises_the_sealed_offline_seed() {
        for t in TOPICS {
            let (page, _) = render(Some(t.name), None);
            assert!(
                !page.contains("sealed inside the app"),
                "{} still promises the retired in-app seal",
                t.name
            );
        }
    }

    /// Every listed verb carries a description — an aligned table of bare verb
    /// names would be a listing, not a manual.
    #[test]
    fn every_listed_verb_carries_a_description() {
        let page = overview_page();
        for line in page.lines() {
            let Some(rest) = line.strip_prefix("  aterm ") else {
                continue;
            };
            let Some((sig, desc)) = rest.split_once("  ") else {
                continue;
            };
            assert!(
                !desc.trim().is_empty(),
                "`aterm {}` is listed with no description",
                sig.trim()
            );
        }
    }

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
        // EVERY full catalog row must appear (it is the manual: the FULL entries, not
        // the short form the live server's bare `help` answers), proving the page is
        // wired to `catalog_lines_full()` and not a hand-copied or summary list.
        for line in aterm_types::control_verbs::catalog_lines_full() {
            assert!(
                page.contains(&line),
                "manual is missing the full row {line:?}"
            );
        }
        assert!(
            page.contains("`aterm ctl help` prints the short catalog; `aterm ctl help <verb>` the full entry for one verb."),
            "the manual must say where the short and per-verb forms live"
        );
    }

    /// Every page in [`EXTRA_PAGES`] dispatches. The listing is what a guessing
    /// reader is told exists, so a name in it that answers "unknown topic" is
    /// worse than no listing at all.
    #[test]
    fn every_extra_page_dispatches() {
        for name in EXTRA_PAGES {
            let (page, code) = render(Some(name), None);
            assert_eq!(code, 0, "extra page {name} should render 0");
            assert!(!page.trim().is_empty(), "extra page {name} rendered empty");
        }
    }

    /// Every repo-rooted path the MANUAL names must exist.
    ///
    /// Ported from clean's help-truth C3, which caught two shipped defects
    /// there. A reader who follows a path out of `aterm help <topic>` and finds
    /// nothing cannot tell whether they typed it wrong or the tool is lying —
    /// and the manual is where aterm's paths actually live (the `ctl` verb
    /// catalog names none, which its own copy of this check says out loud
    /// rather than reporting a vacuous pass).
    ///
    /// Scans every page a reader can reach: `TOPICS`, `EXTRA_PAGES`, the agent
    /// brief and the front page. The extractor is PROVED on a synthetic string
    /// first, because a check that finds nothing passes.
    #[test]
    fn every_repo_path_named_in_the_manual_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/aterm-cli has a workspace root two levels up");
        const ROOTS: &[&str] = &["crates/", "scripts/", "docs/", "tests/", "data/"];
        // A path in ANOTHER tree is not this repo's to have. The manual documents
        // the sibling tools, so `scripts/x.sh` on an `nn` page means nn's — and a
        // reader reading it from inside aterm cannot tell unless the line says
        // so. Naming the tree is therefore REQUIRED, exactly as clean's C3
        // requires the sentence to say which run writes a file: satisfying the
        // check and improving the sentence are one edit.
        //
        // But naming a tree is not by itself a pass. When that sibling is
        // checked out beside this one, the path is RESOLVED THERE — so a
        // cross-repo reference is verified rather than excused whenever it can
        // be. Only an absent sibling yields, and the reader has at least been
        // told where to look.
        const SIBLINGS: &[&str] = &[
            "nn",
            "ny",
            "ay",
            "clean",
            "trust",
            "gamma-crown",
            "ty",
            "trust-cg",
            "trust-ir",
            "trust-vc",
            "zani",
        ];
        let siblings_root = root.parent().map(std::path::Path::to_path_buf);
        let scan = |page: &str, text: &str, missing: &mut Vec<String>| {
            for line in text.lines() {
                let named: Vec<&str> = SIBLINGS
                    .iter()
                    .filter(|r| {
                        line.contains(&format!("{r} tree")) || line.contains(&format!("{r} repo"))
                    })
                    .copied()
                    .collect();
                for raw in line.split(|c: char| c.is_whitespace() || c == '`' || c == '"') {
                    let tok = raw.trim_matches(|c: char| {
                        !c.is_alphanumeric() && c != '/' && c != '.' && c != '-' && c != '_'
                    });
                    if !ROOTS.iter().any(|r| tok.starts_with(r)) || root.join(tok).exists() {
                        continue;
                    }
                    if named.is_empty() {
                        missing.push(format!("{page}: `{tok}`"));
                        continue;
                    }
                    let mut checked_out = false;
                    let mut found = false;
                    for sib in &named {
                        let Some(dir) = siblings_root.as_ref().map(|p| p.join(sib)) else {
                            continue;
                        };
                        if dir.is_dir() {
                            checked_out = true;
                            found |= dir.join(tok).exists();
                        }
                    }
                    if checked_out && !found {
                        missing.push(format!(
                            "{page}: `{tok}` (names the {} tree, which IS checked out beside \
                             this one, and the path is not there)",
                            named.join("/")
                        ));
                    }
                }
            }
        };

        let mut probe = Vec::new();
        scan(
            "synthetic",
            "see `docs/NO-SUCH-FILE-9f3a.md` and crates/aterm-cli/src/manual.rs",
            &mut probe,
        );
        assert_eq!(
            probe,
            vec!["synthetic: `docs/NO-SUCH-FILE-9f3a.md`".to_string()],
            "the extractor must find a missing path and pass a real one; if this \
             fails, the scan below proves nothing about the manual"
        );
        // ...and the sibling-tree exemption must be EARNED, not automatic.
        let mut probe = Vec::new();
        scan(
            "synthetic",
            "run `scripts/none.sh` in the nn tree",
            &mut probe,
        );
        // `nn` is not cloned beside this repo on every machine. Where it IS, the
        // path is checked there and this probe is a finding; where it is not,
        // naming the tree is the most a source check can ask for. Both are
        // correct, so the probe asserts the DISJUNCTION rather than pinning
        // whichever machine happens to run it.
        let nn_present = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
            .is_some_and(|siblings| siblings.join("nn").is_dir());
        assert_eq!(
            probe.is_empty(),
            !nn_present,
            "a named sibling tree is resolved when checked out and yielded when not; \
             nn_present={nn_present}, probe={probe:?}"
        );
        // The RESOLVING branch, proved against whichever sibling is actually
        // checked out beside this repo. Without this, the cross-repo rule could
        // decay into a blanket exemption on every machine and still look green.
        let siblings_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        if let Some(present) = siblings_dir.as_ref().and_then(|d| {
            ["clean", "trust", "nn", "ny", "ay"]
                .into_iter()
                .find(|s| d.join(s).is_dir())
        }) {
            let mut probe = Vec::new();
            scan(
                "synthetic",
                &format!("see `docs/NO-SUCH-FILE-9f3a.md` in the {present} tree"),
                &mut probe,
            );
            assert_eq!(
                probe.len(),
                1,
                "`{present}` is checked out beside this repo, so a path named as \
                 its own must be RESOLVED there, not excused: {probe:?}"
            );
        }

        let mut probe = Vec::new();
        scan("synthetic", "run `scripts/none.sh` somewhere", &mut probe);
        assert_eq!(
            probe.len(),
            1,
            "an UNqualified missing path is still a finding: {probe:?}"
        );

        let mut names: Vec<&str> = TOPICS.iter().map(|t| t.name).collect();
        names.extend(EXTRA_PAGES.iter().copied());
        names.push("agent");
        let mut missing = Vec::new();
        let mut scanned = 0usize;
        for name in names {
            let (page, _) = render(Some(name), None);
            scanned += 1;
            scan(name, &page, &mut missing);
        }
        let (front, _) = render(None, None);
        scan("(front page)", &front, &mut missing);
        assert!(
            scanned > 10,
            "only {scanned} manual page(s) scanned — the page registry moved and \
             this check has stopped covering the manual"
        );
        assert!(
            missing.is_empty(),
            "{} repo-rooted path(s) named in the manual do not exist. A reader who \
             follows one gets nothing and cannot tell whether they typed it wrong \
             or the tool is lying:\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// `aterm help permissions` (design §5.5) — every load-bearing point, each
    /// one a thing an agent otherwise gets wrong at real cost.
    #[test]
    fn the_permissions_page_teaches_the_whole_eperm_recovery() {
        let (page, code) = render(Some("permissions"), None);
        assert_eq!(code, 0);
        for needle in [
            // it is privacy, and the absence of a dialog proves nothing
            "Operation not permitted",
            "NO DIALOG AT ALL",
            "never times out",
            // the one first move
            "`aterm ctl privacy`",
            "before you retry",
            // the three reflexes that are wrong
            "Do not retry in a loop",
            "Do not `sudo`",
            "Do not rewrite the path",
            // who can fix it, and who cannot
            "Full Disk Access",
            "can aterm — only a human",
            // the handoff case, and the cheap recovery
            "attribution=adopted",
            "opening a new tab is a valid recovery",
            // the await verb
            "await consent",
        ] {
            assert!(
                page.contains(needle),
                "permissions page missing {needle:?}\n{page}"
            );
        }
        // The folder names come from the consent module's own table, not from a
        // literal typed into this file (grep_guard B13).
        for folder in aterm_containment::Folder::ALL {
            assert!(
                page.contains(folder.as_str()),
                "permissions page does not name the {} folder\n{page}",
                folder.as_str()
            );
        }
    }

    /// The honesty posture the owner ruled on: aterm may say the grant removes
    /// this CLASS of interruption for the folders it covers, and may not say it
    /// ends prompts. No page may promise elimination, and none may ask for a
    /// fresh launch (grep_guard B12).
    #[test]
    fn no_permissions_text_overclaims_or_asks_for_a_relaunch() {
        let pages = [
            render(Some("permissions"), None).0,
            render(None, Some("s-abc123")).0,
            render(Some("aterm"), None).0,
        ];
        for page in &pages {
            let lower = page.to_ascii_lowercase();
            for banned in [
                "no more prompts",
                "removes all prompts",
                "eliminates",
                "restart",
                "relaunch",
                "reopen",
                "next launch",
            ] {
                assert!(!lower.contains(banned), "a page says {banned:?}\n{page}");
            }
        }
    }

    /// The agent brief is where an agent lands by default inside a session, so
    /// the EPERM paragraph has to be THERE, not only behind a topic it would
    /// have to know to ask for.
    #[test]
    fn the_agent_brief_carries_the_macos_permission_gotcha() {
        let (page, _) = render(None, Some("s-abc123"));
        for needle in [
            "MACOS FILE PERMISSIONS",
            "NO DIALOG",
            "`aterm ctl privacy` BEFORE retrying",
            "do not `sudo`",
            "attribution=adopted",
            "aterm help permissions",
        ] {
            assert!(
                page.contains(needle),
                "agent brief missing {needle:?}\n{page}"
            );
        }
    }

    /// The front-door page's GOTCHAS name it too, and point at the deep dive —
    /// this is the list a human reads before they ever see a dialog.
    #[test]
    fn the_aterm_page_gotchas_name_macos_privacy() {
        let (page, _) = render(Some("aterm"), None);
        assert!(page.contains("Operation not permitted"), "{page}");
        assert!(page.contains("aterm help permissions"), "{page}");
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
        // (aterm-update-core::pins) — the stale "a plain targo --unverified build bakes no root key"
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

    /// THE PAGE NAMES EVERY VERB. The old KEY USAGE trailed off in "..." — so the
    /// only complete roster lived in `aterm pkg --help`, and a reader of the manual
    /// could not discover `rollback` or `relocate` existed at all. The roster itself
    /// is guarded on the atpkg side (its tier partition test against the dispatch
    /// table); this pins the manual to that same roster — read live, not copied,
    /// so the count is whatever the table says rather than a number in a comment.
    /// (It said "the same 21 names" while the table held 22.)
    #[test]
    fn pkg_manual_names_every_verb() {
        let (page, code) = render(Some("pkg"), None);
        assert_eq!(code, 0);
        // Reads `atpkg::cli::dispatch_roster()` — the DISPATCH TABLE — rather
        // than a list typed out here. This test held its own copy of the
        // verbs until 2026-09-01, and that copy was missing `repair`, so it
        // passed while the page it guards omitted the same verb. A
        // completeness test that duplicates the data it checks against
        // cannot see a divergence in that data: it had exactly the manual's
        // blind spot, which is why the omission survived a test named
        // "names every verb".
        for verb in atpkg::cli::dispatch_roster() {
            assert!(
                page.contains(verb),
                "the pkg manual page must name the `{verb}` verb"
            );
        }
        // Every invitation to type is spelled the runnable way.
        assert!(
            page.contains("aterm pkg install --default-set"),
            "the usual first command is spelled as typed"
        );
    }

    /// The manual may not prescribe an atpkg verb that does not run.
    ///
    /// The mirror of `pkg_manual_names_every_verb`, and the direction nothing
    /// checked. The atpkg page asserted, two bullets below three false claims,
    /// that "the roster `aterm pkg --help` prints is test-pinned to the
    /// dispatch table — it never advertises a verb that does not run". True of
    /// the ROSTER; false of the page saying it. On 2026-09-01 these pages
    /// prescribed `aterm pkg shellenv`, `aterm pkg seam` and
    /// `aterm pkg doctor --fix`. None exists, and an unknown verb exits 2 with
    /// EMPTY stdout — so the page's own `eval "$(aterm pkg shellenv)"`
    /// evaluated nothing and SUCCEEDED. `doctor --fix` was named as THE cure
    /// for `error: toolchain 'trust' is not installed`, directly above "Never
    /// rebuild a toolchain from source to answer that message"; `doctor` parses
    /// no arguments at all.
    ///
    /// Only SOUNDNESS lives here. Completeness is the test above. The two are
    /// separate because the directions are not symmetric: both ROSTERS are
    /// closed, so completeness is decidable — but the front-door NAMESPACE is
    /// open (`aterm <tool>` routes any store program, which is why
    /// `aterm trustc` and `aterm targo` are correct prose), so the same check
    /// cannot be run there.
    #[test]
    fn manual_never_prescribes_a_pkg_verb_that_does_not_run() {
        let roster = atpkg::cli::dispatch_roster();
        let mut ghosts: Vec<String> = Vec::new();
        for topic in TOPICS {
            let Some(body) = topic.body else { continue };
            let mut rest = body;
            while let Some(at) = rest.find("aterm pkg ") {
                let after = &rest[at + "aterm pkg ".len()..];
                let verb: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                let step = verb.len().max(1).min(after.len());
                rest = &after[step..];
                // `aterm pkg --help` and `aterm pkg <tool>` are not verbs.
                if verb.is_empty() || verb.starts_with('-') || roster.contains(&verb.as_str()) {
                    continue;
                }
                ghosts.push(format!("{}: `aterm pkg {verb}`", topic.name));
            }
        }
        assert!(
            ghosts.is_empty(),
            "the manual prescribes {} atpkg verb(s) that are not in the dispatch \
             roster. An unknown verb exits 2 with EMPTY stdout, so a reader who \
             follows one gets silence, not an error:\n  {}\n  Roster: {roster:?}",
            ghosts.len(),
            ghosts.join("\n  ")
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
            // `conn` is its own topic (the session-connections front door).
            ("conn", "session connections"),
        ] {
            let (page, code) = render(Some(verb), None);
            assert_eq!(code, 0, "`aterm help {verb}` must resolve, not 404");
            assert!(
                page.to_lowercase().contains(marker),
                "`aterm help {verb}` should route to the page covering it"
            );
        }
    }

    #[test]
    fn fleet_help_exposes_the_opt_in_operator_and_empty_authority_boundary() {
        let (page, code) = render(Some("fleet"), None);
        assert_eq!(code, 0);
        assert!(page.contains("aterm fleet status"), "{page}");
        assert!(page.contains("empty allowlist"), "{page}");
        assert!(page.contains("guarded interactive turn"), "{page}");
        // An experimental resident subsystem is opt-in, and the page has to say
        // both that it is experimental and how to turn it on — otherwise the
        // feature is undiscoverable and its status is a source-only claim.
        assert!(page.contains("EXPERIMENTAL"), "{page}");
        assert!(page.contains("ATERM_OPERATOR=1"), "{page}");
    }

    /// The brief is the first thing an agent reads inside aterm, so it must name
    /// the moves the agent-experience report found missing (docs/AGENT-EXPERIENCE-
    /// 2026-08-26.md §7): the window listing beside `ls`, the trimmed reads, the
    /// aimed spawn, the exit ledger, the per-verb help — and the one rule that keeps
    /// an agent out of another agent's prompt. It must ALSO stay a brief: the whole
    /// point of `help <verb>` was that the first read is cheap.
    #[test]
    fn agent_brief_teaches_windows_trim_exits_help_and_the_peer_rule() {
        let (agent, code) = render(None, Some("s-abc123"));
        assert_eq!(code, 0);
        for needle in [
            "aterm ctl windows",
            "aterm ctl ls",
            "text trim",
            "turn trim=1",
            "spawn window=<id>",
            "exits [since=<id>]",
            "`help <verb>`",
            "`help --full`",
            "detail=",
            "meta role=",
            "never\n    type into another agent's prompt",
            "it says WHY",
        ] {
            assert!(
                agent.contains(needle),
                "brief must say {needle:?}:\n{agent}"
            );
        }
        // Length discipline: a few added lines, not a manual. `help introspection`
        // and `help <verb>` are where the depth lives.
        assert!(
            agent.lines().count() <= 90 && agent.len() <= 7_000,
            "the brief grew into a manual: {} lines, {} bytes",
            agent.lines().count(),
            agent.len()
        );
        // The sid-less brief carries the same rule and the same help pointers.
        let (generic, _) = render(Some("agent"), None);
        assert!(generic.contains("meta role=") && generic.contains("`help <verb>`"));
    }

    /// The `agents` topic no longer tells a human to run the installer once per
    /// machine: aterm runs it itself, and the topic names the knob that stops it.
    #[test]
    fn agents_topic_says_aterm_primes_agents_itself_and_names_the_knob() {
        let (page, code) = render(Some("agents"), None);
        assert_eq!(code, 0);
        assert!(page.contains("aterm runs this installer itself"), "{page}");
        assert!(page.contains("agents_auto_prime = false"), "{page}");
        assert!(!page.contains("Once per machine"), "{page}");
    }
}
